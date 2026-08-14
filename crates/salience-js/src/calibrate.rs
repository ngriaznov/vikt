//! In-process mutation engine for `salience calibrate`'s JavaScript /
//! TypeScript frontend.
//!
//! Python's engine (`salience_py::calibrate`) reserializes the whole file
//! through `ast.unparse` after every edit: sound, because the tree it
//! reserializes from is always valid, but it drops every comment and
//! renumbers every line, so only the *original* line of the edit stays
//! meaningful. This engine never reserializes anything. Each mutant is a
//! byte-span splice directly on the original source text — the operator
//! token, the numeric literal, or (for statement deletion) the statement's
//! own span — so every line the edit doesn't touch keeps its exact bytes,
//! comments included. Multi-line spans are padded back out with trailing
//! newlines on deletion, so even lines *after* a deleted statement keep
//! their original numbers.
//!
//! A splice is not guaranteed syntactically safe by construction the way an
//! AST edit is: moving a byte span cannot straddle a token the parser
//! resolved differently, but the surrounding grammar might still reject the
//! result (a swapped operator whose new precedence changes what the rest of
//! the line parses as, say). So every mutant is re-parsed before it is
//! offered to the caller; one that fails is discarded and counted rather
//! than executed — see [`MutantSet::invalid_discarded`].

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{BinaryExpression, ExpressionStatement, LogicalExpression, NumericLiteral};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::operator::{BinaryOperator, LogicalOperator};
use salience_core::mutant::{Mutant, MutantSet};

use crate::JsError;

/// Generates up to `limit` mutants whose sites fall inside `spans`
/// (inclusive line ranges). `limit == 0` counts sites without emitting.
///
/// # Errors
///
/// [`JsError::Io`] if the file cannot be read, [`JsError::Syntax`] if the
/// *original* file fails to parse. A mutant that itself fails to reparse is
/// not an error here — see the module docs — it is discarded and counted in
/// [`MutantSet::invalid_discarded`].
pub fn mutants_for(path: &Path, spans: &[(u32, u32)], limit: usize) -> Result<MutantSet, JsError> {
    if spans.is_empty() {
        // Nothing to target; every kind below needs a line inside a span.
        return Ok(MutantSet {
            total_sites: 0,
            mutants: Vec::new(),
            invalid_discarded: 0,
        });
    }
    let display = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| JsError::Io {
        path: display.clone(),
        source: e,
    })?;
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::mjs());

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, source_type).parse();
    if parsed.panicked {
        return Err(JsError::Syntax {
            path: display,
            first: parsed
                .diagnostics
                .first()
                .map_or_else(|| "parser panicked".into(), ToString::to_string),
        });
    }

    let line_starts = line_starts(&source);
    let mut collector = Collector {
        source: &source,
        spans,
        line_starts: &line_starts,
        sites: Vec::new(),
    };
    collector.visit_program(&parsed.program);
    let mut sites = collector.sites;
    // Same reasoning as the Python engine: sorting by (line, kind, detail)
    // makes truncation under a budget cut the tail of the file, not an
    // arbitrary traversal order.
    sites.sort_by(|a, b| (a.line, a.kind, &a.detail).cmp(&(b.line, b.kind, &b.detail)));
    let total_sites = sites.len();

    let mut mutants = Vec::with_capacity(limit.min(total_sites));
    let mut invalid_discarded = 0usize;
    for site in sites.into_iter().take(limit) {
        let mutated = splice_preserving_lines(&source, site.span, &site.replacement);
        if !reparses(&mutated, source_type) {
            invalid_discarded += 1;
            continue;
        }
        mutants.push(Mutant {
            line: site.line,
            kind: site.kind.to_owned(),
            detail: site.detail,
            source: mutated,
        });
    }
    Ok(MutantSet {
        total_sites,
        mutants,
        invalid_discarded,
    })
}

/// Whether `source` parses cleanly: no panic, and no recoverable diagnostic
/// either. A recoverable parse error still produces a `Program`, so checking
/// only `panicked` would wave a mutant through that oxc itself flagged.
fn reparses(source: &str, source_type: SourceType) -> bool {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    !parsed.panicked && parsed.diagnostics.is_empty()
}

/// One candidate edit, tagged with the line the panel scored and the exact
/// byte span it replaces.
struct Candidate {
    line: u32,
    kind: &'static str,
    detail: String,
    span: (u32, u32),
    replacement: String,
}

/// One pass over the parsed program collecting every mutation site whose
/// line falls inside a requested span. A `Visit` implementation rather than
/// hand-rolled recursion: the default `walk_*` functions already descend
/// into every expression and statement position, so overriding only the
/// four kinds this engine mutates is enough — everything else falls through
/// to the default walk unchanged.
struct Collector<'s> {
    source: &'s str,
    spans: &'s [(u32, u32)],
    line_starts: &'s [u32],
    sites: Vec<Candidate>,
}

impl Collector<'_> {
    fn line_of(&self, offset: u32) -> u32 {
        u32::try_from(self.line_starts.partition_point(|&s| s <= offset)).unwrap_or(u32::MAX)
    }

    fn in_span(&self, line: u32) -> bool {
        self.spans.iter().any(|&(lo, hi)| lo <= line && line <= hi)
    }

    fn push(&mut self, line: u32, kind: &'static str, old: &str, new: &str, span: (u32, u32)) {
        self.sites.push(Candidate {
            line,
            kind,
            detail: format!("{old} -> {new}"),
            span,
            replacement: new.to_owned(),
        });
    }
}

impl<'a> Visit<'a> for Collector<'_> {
    fn visit_binary_expression(&mut self, it: &BinaryExpression<'a>) {
        let line = self.line_of(it.span.start);
        if self.in_span(line)
            && let Some(span) = operator_span(
                self.source,
                it.left.span().end,
                it.right.span().start,
                it.operator.as_str(),
            )
        {
            for new_op in cmp_swaps(it.operator) {
                self.push(line, "cmp", it.operator.as_str(), new_op.as_str(), span);
            }
            if let Some(new_op) = bin_swap(it.operator) {
                self.push(line, "bin", it.operator.as_str(), new_op.as_str(), span);
            }
        }
        walk::walk_binary_expression(self, it);
    }

    fn visit_logical_expression(&mut self, it: &LogicalExpression<'a>) {
        let line = self.line_of(it.span.start);
        if self.in_span(line)
            && let Some(new_op) = bool_swap(it.operator)
            && let Some(span) = operator_span(
                self.source,
                it.left.span().end,
                it.right.span().start,
                it.operator.as_str(),
            )
        {
            self.push(line, "bool", it.operator.as_str(), new_op.as_str(), span);
        }
        walk::walk_logical_expression(self, it);
    }

    fn visit_numeric_literal(&mut self, it: &NumericLiteral<'a>) {
        let line = self.line_of(it.span.start);
        if self.in_span(line) {
            let span = (it.span.start, it.span.end);
            let old = fmt_num(it.value);
            let plus_one = fmt_num(it.value + 1.0);
            self.push(line, "const", &old, &plus_one, span);
            // `n -> n+1` already covers `0 -> 1`; `1` additionally gets the
            // reverse edge, the off-by-one-down mutant `n+1` cannot reach.
            #[allow(clippy::float_cmp)] // 1.0 is an exact literal value, not a computed one
            if it.value == 1.0 {
                self.push(line, "const", &old, "0", span);
            }
        }
        walk::walk_numeric_literal(self, it);
    }

    fn visit_expression_statement(&mut self, it: &ExpressionStatement<'a>) {
        let line = self.line_of(it.span.start);
        if self.in_span(line) {
            self.push(
                line,
                "delete",
                "statement",
                ";",
                (it.span.start, it.span.end),
            );
        }
        walk::walk_expression_statement(self, it);
    }
}

/// Comparison swaps that stay inside their operator's own family: relational
/// operators swap their boundary (`<` <-> `<=`) and reverse their order
/// (`<` <-> `>`); `==`/`!=` and `===`/`!==` only ever swap within their own
/// family. `===` never becomes `==` — there is no table entry that crosses
/// them, so no code path here can produce that edit.
fn cmp_swaps(op: BinaryOperator) -> Vec<BinaryOperator> {
    let mut out = Vec::new();
    match op {
        BinaryOperator::LessThan => out.push(BinaryOperator::LessEqualThan),
        BinaryOperator::LessEqualThan => out.push(BinaryOperator::LessThan),
        BinaryOperator::GreaterThan => out.push(BinaryOperator::GreaterEqualThan),
        BinaryOperator::GreaterEqualThan => out.push(BinaryOperator::GreaterThan),
        _ => {}
    }
    if let Some(inverse) = op.equality_inverse_operator() {
        out.push(inverse);
    }
    if let Some(inverse) = op.compare_inverse_operator() {
        out.push(inverse);
    }
    out
}

/// Arithmetic swaps, restricted to the requested operator set: `+ - * / %`.
fn bin_swap(op: BinaryOperator) -> Option<BinaryOperator> {
    match op {
        BinaryOperator::Addition => Some(BinaryOperator::Subtraction),
        BinaryOperator::Subtraction => Some(BinaryOperator::Addition),
        BinaryOperator::Multiplication => Some(BinaryOperator::Division),
        BinaryOperator::Division => Some(BinaryOperator::Multiplication),
        BinaryOperator::Remainder => Some(BinaryOperator::Multiplication),
        _ => None,
    }
}

/// `&&` <-> `||`. `??` is left alone: it is not a boolean connective, and
/// swapping it for `&&`/`||` would change what values short-circuit it, not
/// just which branch runs.
fn bool_swap(op: LogicalOperator) -> Option<LogicalOperator> {
    match op {
        LogicalOperator::And => Some(LogicalOperator::Or),
        LogicalOperator::Or => Some(LogicalOperator::And),
        LogicalOperator::Coalesce => None,
    }
}

/// Formats a numeric value the way it needs to appear in a splice: plain
/// decimal, no exponent, integral values without a trailing `.0`. The
/// *original* literal may have been hex, scientific or underscore-separated
/// — irrelevant here, since the splice replaces the whole token and any
/// plain-decimal spelling of the same value is equally valid JavaScript.
fn fmt_num(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Locates an operator token's own byte span between two operand spans:
/// scans the (whitespace/comments) gap between them for the operator's text,
/// skipping over `//` and `/* */` comments so a decoy occurrence inside one
/// (`a /*<*/ < b`) cannot steal the match. Parens around either operand are
/// not skipped specially — they are not the operator's own text, so the scan
/// simply passes over them like any other non-matching character.
fn operator_span(source: &str, left_end: u32, right_start: u32, op: &str) -> Option<(u32, u32)> {
    let start = left_end as usize;
    let end = (right_start as usize).min(source.len());
    let gap = source.get(start..end)?;
    let mut idx = 0usize; // byte offset into `gap`, always a char boundary
    while idx < gap.len() {
        if gap[idx..].starts_with("//") {
            idx += gap[idx..].find('\n').unwrap_or(gap.len() - idx);
            continue;
        }
        if gap[idx..].starts_with("/*") {
            idx += gap[idx..]
                .find("*/")
                .map_or(gap.len() - idx, |p| p + "*/".len());
            continue;
        }
        if gap[idx..].starts_with(op) {
            let abs_start = u32::try_from(start + idx).ok()?;
            let abs_end = abs_start + u32::try_from(op.len()).ok()?;
            return Some((abs_start, abs_end));
        }
        // One *character*, not one byte, so the next slice starts on a
        // boundary — the gap can hold arbitrary Unicode inside a comment.
        idx += gap[idx..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Splices `replacement` over `span` in `source`, padding it with trailing
/// newlines when the replacement has fewer than the span it replaces — the
/// mechanism that keeps every line *after* a deleted multi-line statement at
/// its original number.
fn splice_preserving_lines(source: &str, span: (u32, u32), replacement: &str) -> String {
    let (start, end) = (span.0 as usize, span.1 as usize);
    let removed_lines = source[start..end].bytes().filter(|&b| b == b'\n').count();
    let added_lines = replacement.bytes().filter(|&b| b == b'\n').count();
    let mut out = String::with_capacity(source.len() + replacement.len());
    out.push_str(&source[..start]);
    out.push_str(replacement);
    if added_lines < removed_lines {
        out.push_str(&"\n".repeat(removed_lines - added_lines));
    }
    out.push_str(&source[end..]);
    out
}

/// Byte offset of the start of every line, index 0 first — mirrors the
/// lowering frontend's own construction so line numbers agree between
/// scoring and mutation.
fn line_starts(source: &str) -> Vec<u32> {
    std::iter::once(0u32)
        .chain(
            source
                .bytes()
                .enumerate()
                .filter(|&(_, b)| b == b'\n')
                .map(|(i, _)| u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Runs the collector over `source` through a real temp file (the public
    /// entry point takes a path, not a string) and returns every mutant's
    /// `(line, kind, detail)`, asserting none of them were discarded — these
    /// fixtures are all meant to reparse cleanly, so a discard here would
    /// itself be the bug under test.
    fn sites_in(source: &str, spans: &[(u32, u32)]) -> Vec<(u32, String, String)> {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "salience-js-calibrate-test-{}-{seq}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("site.js");
        std::fs::write(&path, source).expect("temp file");
        let set = mutants_for(&path, spans, 1000).expect("mutants_for");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            set.invalid_discarded, 0,
            "every mutant in this fixture is expected to reparse cleanly"
        );
        set.mutants
            .into_iter()
            .map(|m| (m.line, m.kind, m.detail))
            .collect()
    }

    #[test]
    fn comparison_families_never_cross() {
        let src = "function f(a, b) {\n  return a === b && a < b;\n}\n";
        let sites = sites_in(src, &[(1, 3)]);
        let cmp: Vec<&String> = sites
            .iter()
            .filter(|(_, k, _)| k == "cmp")
            .map(|(_, _, d)| d)
            .collect();
        assert!(cmp.iter().any(|d| d.as_str() == "=== -> !=="), "{cmp:?}");
        assert!(cmp.iter().any(|d| d.as_str() == "< -> <="), "{cmp:?}");
        assert!(cmp.iter().any(|d| d.as_str() == "< -> >"), "{cmp:?}");
        // `===` must never weaken into the loose-equality family: no table
        // entry produces it, so this holds by construction, but the test
        // pins it so a future edit to `cmp_swaps` cannot reintroduce it.
        assert!(!cmp.iter().any(|d| d.as_str() == "=== -> =="), "{cmp:?}");
        assert!(!cmp.iter().any(|d| d.as_str() == "=== -> !="), "{cmp:?}");
    }

    #[test]
    fn arithmetic_and_logical_swaps() {
        let src = "function f(a, b) {\n  return a + b * (a % b) && a - b;\n}\n";
        let sites = sites_in(src, &[(1, 3)]);
        let bin: Vec<&String> = sites
            .iter()
            .filter(|(_, k, _)| k == "bin")
            .map(|(_, _, d)| d)
            .collect();
        assert!(bin.iter().any(|d| d.as_str() == "+ -> -"), "{bin:?}");
        assert!(bin.iter().any(|d| d.as_str() == "* -> /"), "{bin:?}");
        assert!(bin.iter().any(|d| d.as_str() == "% -> *"), "{bin:?}");
        assert!(bin.iter().any(|d| d.as_str() == "- -> +"), "{bin:?}");
        let bool_sites: Vec<&String> = sites
            .iter()
            .filter(|(_, k, _)| k == "bool")
            .map(|(_, _, d)| d)
            .collect();
        assert!(
            bool_sites.iter().any(|d| d.as_str() == "&& -> ||"),
            "{bool_sites:?}"
        );
    }

    #[test]
    fn numeric_perturbation_and_the_one_to_zero_edge() {
        let src = "function f() {\n  return 0 + 1 + 5;\n}\n";
        let sites = sites_in(src, &[(1, 3)]);
        let consts: Vec<&String> = sites
            .iter()
            .filter(|(_, k, _)| k == "const")
            .map(|(_, _, d)| d)
            .collect();
        assert!(consts.iter().any(|d| d.as_str() == "0 -> 1"), "{consts:?}");
        assert!(consts.iter().any(|d| d.as_str() == "1 -> 2"), "{consts:?}");
        assert!(consts.iter().any(|d| d.as_str() == "1 -> 0"), "{consts:?}");
        assert!(consts.iter().any(|d| d.as_str() == "5 -> 6"), "{consts:?}");
    }

    #[test]
    fn only_expression_statements_are_deletable() {
        let src = "function f(a) {\n  let x = a;\n  f(a);\n  return x;\n}\n";
        let sites = sites_in(src, &[(1, 5)]);
        let deletes: Vec<(u32, &String)> = sites
            .iter()
            .filter(|(_, k, _)| k == "delete")
            .map(|(l, _, d)| (*l, d))
            .collect();
        // `f(a);` is the only expression statement; `let x = a;` is a
        // declaration and `return x;` is a return, neither deletable here.
        assert_eq!(deletes.len(), 1, "{deletes:?}");
        assert_eq!(deletes[0].0, 3);
        assert_eq!(deletes[0].1.as_str(), "statement -> ;");
    }

    #[test]
    fn sites_outside_the_span_are_not_collected() {
        let src = "function f(a, b) {\n  return a < b;\n}\n";
        let sites = sites_in(src, &[(1, 1)]);
        assert!(sites.is_empty(), "{sites:?}");
    }

    #[test]
    fn a_decoy_operator_inside_a_comment_does_not_steal_the_splice() {
        // The gap between `a` and `b` contains a block comment that itself
        // contains `<`; `operator_span` must skip over it and land on the
        // real operator, not the one hiding in the comment.
        let src = "function f(a, b) {\n  return a /*<*/ < b;\n}\n";
        let sites = sites_in(src, &[(1, 3)]);
        let cmp: Vec<&String> = sites
            .iter()
            .filter(|(_, k, _)| k == "cmp")
            .map(|(_, _, d)| d)
            .collect();
        assert!(cmp.iter().any(|d| d.as_str() == "< -> <="), "{cmp:?}");
    }

    #[test]
    fn reparse_guard_rejects_broken_syntax_and_accepts_valid_source() {
        let js = SourceType::mjs();
        assert!(reparses("function f() { return 1 + 1; }\n", js));
        assert!(!reparses("function f() { return 1 + ; }\n", js));
    }
}
