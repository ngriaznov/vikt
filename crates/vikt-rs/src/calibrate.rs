//! In-process textual mutation engine for `vikt calibrate`'s Rust frontend.
//!
//! Rust mutants are textual splices, not AST rewrites: the frontend lowers
//! MIR, and re-lowering per candidate site just to reserialize source would
//! cost a compile each. A splice can therefore propose an edit the language
//! rejects — flipping a `<` that was really a generic bracket, dividing
//! through a dereference. That is priced in: the calibrate pipeline runs a
//! build step before the suite for every Rust mutant, and a mutant that does
//! not compile is *invalid*, never a kill. The engine only has to keep the
//! obvious non-code out — string literals, comments, char literals — which
//! the byte mask below does without parsing.
//!
//! Splicing also keeps every other line's number intact, so the kill is keyed
//! to exactly the line the panel scored — same property as the JavaScript
//! engine, and the reason `Mutant::line` needs no adjustment here.

use std::path::Path;

use vikt_core::mutant::{Mutant, MutantSet};

/// One candidate edit at a byte offset.
struct Site {
    line: u32,
    start: usize,
    end: usize,
    replacement: String,
    kind: &'static str,
    detail: String,
}

/// Line-targeted mutants for one file, restricted to the scored `spans`
/// (inclusive line ranges), at most `limit` of them, sorted by line.
/// `total_sites` counts every candidate found so the caller can announce
/// budget truncation; `invalid_discarded` is always zero here — validity is
/// decided later by the build step, which the pipeline reports separately.
///
/// # Errors
///
/// Only I/O: the file is read once, up front.
pub fn mutants_for(path: &Path, spans: &[(u32, u32)], limit: usize) -> std::io::Result<MutantSet> {
    let src = std::fs::read_to_string(path)?;
    let mask = code_mask(&src);
    let in_span = |line: u32| spans.iter().any(|&(lo, hi)| lo <= line && line <= hi);

    let mut sites = Vec::new();
    let bytes = src.as_bytes();
    let mut line: u32 = 1;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if !mask[i] || !in_span(line) {
            i += 1;
            continue;
        }
        let prev = if i > 0 { bytes[i - 1] } else { 0 };
        let next = *bytes.get(i + 1).unwrap_or(&0);
        // Two-byte operators first so their halves are never seen alone.
        let two: Option<(&str, &str, &str)> = match (b, next) {
            (b'&', b'&') => Some(("&&", "||", "bool")),
            (b'|', b'|') => Some(("||", "&&", "bool")),
            (b'<', b'=') => Some(("<=", ">", "cmp")),
            (b'>', b'=') if prev != b'<' && prev != b'>' && prev != b'=' && prev != b'!' => {
                Some((">=", "<", "cmp"))
            }
            (b'=', b'=') if prev != b'=' && prev != b'!' && prev != b'<' && prev != b'>' => {
                Some(("==", "!=", "cmp"))
            }
            (b'!', b'=') if next == b'=' => Some(("!=", "==", "cmp")),
            _ => None,
        };
        if let Some((from, to, kind)) = two {
            sites.push(Site {
                line,
                start: i,
                end: i + 2,
                replacement: to.to_owned(),
                kind,
                detail: format!("{from} -> {to}"),
            });
            i += 2;
            continue;
        }
        let one: Option<(&str, &str, &str)> = match b {
            // `<`/`>` not part of `<<`, `>>`, `->`, `=>`, `<=`, `>=`, or a
            // turbofish `::<`. Generic brackets still slip through; the
            // build step prices those as invalid, not killed.
            b'<' if next != b'<' && next != b'=' && prev != b'<' && prev != b':' => {
                Some(("<", ">=", "cmp"))
            }
            b'>' if next != b'>'
                && next != b'='
                && prev != b'>'
                && prev != b'-'
                && prev != b'=' =>
            {
                Some((">", "<=", "cmp"))
            }
            // `+`/`-` swap, `+=`/`-=` included; `->` excluded above via prev.
            b'+' => Some(("+", "-", "bin")),
            b'-' if next != b'>' => Some(("-", "+", "bin")),
            b'*' if prev != b'/' && next != b'/' => Some(("*", "/", "bin")),
            b'/' if prev != b'*' && next != b'*' && prev != b'/' && next != b'/' => {
                Some(("/", "*", "bin"))
            }
            _ => None,
        };
        if let Some((from, to, kind)) = one {
            sites.push(Site {
                line,
                start: i,
                end: i + 1,
                replacement: to.to_owned(),
                kind,
                detail: format!("{from} -> {to}"),
            });
            i += 1;
            continue;
        }
        // Integer literals: digits not adjacent to an identifier or a float
        // point. `0 -> 1`, `1 -> 0`, otherwise `n -> n+1`; suffixes
        // (`10u32`) survive because only the digit run is replaced.
        if b.is_ascii_digit() && !prev.is_ascii_alphanumeric() && prev != b'_' && prev != b'.' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
            if bytes.get(i).copied() != Some(b'.') {
                let text: String = src[start..i].chars().filter(|c| *c != '_').collect();
                if let Ok(n) = text.parse::<u64>() {
                    let to = match n {
                        0 => 1,
                        1 => 0,
                        n => n + 1,
                    };
                    sites.push(Site {
                        line,
                        start,
                        end: i,
                        replacement: to.to_string(),
                        kind: "const",
                        detail: format!("{n} -> {to}"),
                    });
                }
            }
            continue;
        }
        i += 1;
    }

    let total_sites = sites.len();
    let mutants = sites
        .into_iter()
        .take(limit)
        .map(|s| Mutant {
            line: s.line,
            kind: s.kind.to_owned(),
            detail: s.detail,
            source: format!("{}{}{}", &src[..s.start], s.replacement, &src[s.end..]),
        })
        .collect();
    Ok(MutantSet {
        total_sites,
        mutants,
        invalid_discarded: 0,
    })
}

/// True at each byte that is code rather than a comment, string, char or
/// byte-string literal. Handles nested block comments, escapes, and raw
/// strings up to `r###"`; lifetimes (`'a`) are code, not char literals.
fn code_mask(src: &str) -> Vec<bool> {
    let b = src.as_bytes();
    let mut mask = vec![true; b.len()];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    mask[i] = false;
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        mask[i] = false;
                        mask[i + 1] = false;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        mask[i] = false;
                        mask[i + 1] = false;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        mask[i] = false;
                        i += 1;
                    }
                }
            }
            b'"' => i = mask_string(b, &mut mask, i, 0),
            b'r' | b'b' if is_raw_or_byte_string(b, i) => {
                let mut j = i + 1;
                if b.get(j) == Some(&b'r') {
                    j += 1;
                }
                let mut hashes = 0;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) == Some(&b'"') {
                    for m in &mut mask[i..=j] {
                        *m = false;
                    }
                    if b[i] == b'r' || b.get(i + 1) == Some(&b'r') {
                        i = mask_raw_string(b, &mut mask, j, hashes);
                    } else {
                        i = mask_string(b, &mut mask, j, 0);
                    }
                } else {
                    i += 1;
                }
            }
            // A char literal, as opposed to a lifetime: `'x'` or `'\...'`.
            b'\'' if b.get(i + 1) == Some(&b'\\') || b.get(i + 2) == Some(&b'\'') => {
                mask[i] = false;
                i += 1;
                if b.get(i) == Some(&b'\\') {
                    mask[i] = false;
                    i += 1;
                    if i < b.len() {
                        mask[i] = false;
                        i += 1;
                    }
                } else if i < b.len() {
                    mask[i] = false;
                    i += 1;
                }
                if b.get(i) == Some(&b'\'') {
                    mask[i] = false;
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    mask
}

/// A `"`-delimited (possibly escape-carrying) string starting at `open`;
/// returns the index after the closing quote.
fn mask_string(b: &[u8], mask: &mut [bool], open: usize, _hashes: usize) -> usize {
    let mut i = open;
    mask[i] = false;
    i += 1;
    while i < b.len() {
        mask[i] = false;
        if b[i] == b'\\' {
            i += 1;
            if i < b.len() {
                mask[i] = false;
            }
        } else if b[i] == b'"' {
            return i + 1;
        }
        i += 1;
    }
    i
}

/// A raw string whose opening `"` sits at `open`, closed by `"` plus
/// `hashes` `#`s; no escapes exist inside.
fn mask_raw_string(b: &[u8], mask: &mut [bool], open: usize, hashes: usize) -> usize {
    let mut i = open;
    mask[i] = false;
    i += 1;
    while i < b.len() {
        mask[i] = false;
        if b[i] == b'"' && (1..=hashes).all(|k| b.get(i + k) == Some(&b'#')) {
            for k in 1..=hashes {
                mask[i + k] = false;
            }
            return i + hashes + 1;
        }
        i += 1;
    }
    i
}

/// True when the `r`/`b` at `i` opens a raw or byte string rather than being
/// an identifier character: preceded by a non-identifier byte and followed by
/// the right prefix shape.
fn is_raw_or_byte_string(b: &[u8], i: usize) -> bool {
    let prev = if i > 0 { b[i - 1] } else { 0 };
    if prev.is_ascii_alphanumeric() || prev == b'_' {
        return false;
    }
    let mut j = i + 1;
    if b[i] == b'b' && b.get(j) == Some(&b'r') {
        j += 1;
    }
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    b.get(j) == Some(&b'"') && (b[i] == b'r' || b[i] == b'b')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn mutants_of(src: &str, spans: &[(u32, u32)]) -> MutantSet {
        let mut f = tempfile_path();
        std::fs::File::create(&f.0)
            .unwrap()
            .write_all(src.as_bytes())
            .unwrap();
        let set = mutants_for(&f.0, spans, usize::MAX).unwrap();
        let _ = std::fs::remove_file(&f.0);
        f.1 = false;
        set
    }

    fn tempfile_path() -> (std::path::PathBuf, bool) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        (
            std::env::temp_dir().join(format!("vikt-rs-mutants-{}-{seq}.rs", std::process::id())),
            true,
        )
    }

    /// Operators inside strings and comments are never mutation sites; the
    /// same operator in code on the same line is.
    #[test]
    fn strings_and_comments_are_masked() {
        let set = mutants_of(
            "fn f(a: i64) -> i64 {\n    let s = \"a + b < c\"; // x + y\n    a + 2\n}\n",
            &[(1, 4)],
        );
        assert!(
            set.mutants.iter().all(|m| m.line == 3),
            "only the code `+` and the literal on line 3 may mutate, got lines {:?}",
            set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
        );
        assert!(set.mutants.iter().any(|m| m.kind == "bin"));
        assert!(set.mutants.iter().any(|m| m.kind == "const"));
    }

    /// Two `+` on one line become two distinct single-edit mutants, each
    /// leaving the other occurrence untouched.
    #[test]
    fn one_occurrence_per_mutant() {
        let set = mutants_of(
            "fn f(a: i64, b: i64) -> i64 {\n    a + b + 1\n}\n",
            &[(1, 3)],
        );
        let plus: Vec<_> = set.mutants.iter().filter(|m| m.kind == "bin").collect();
        assert_eq!(plus.len(), 2);
        for m in &plus {
            assert_eq!(m.source.matches(" - ").count(), 1);
            assert_eq!(m.source.matches(" + ").count(), 1);
        }
    }

    /// Lifetimes are code, not char literals: the mask must not swallow the
    /// rest of the file after `'a`, and `->` / `=>` are never comparison
    /// sites.
    #[test]
    fn lifetimes_arrows_and_fat_arrows_survive() {
        let set = mutants_of(
            "fn f<'a>(x: &'a i64) -> i64 {\n    match *x { 0 => 7, n => n - 3 }\n}\n",
            &[(1, 3)],
        );
        assert!(set.mutants.iter().any(|m| m.detail == "- -> +"));
        assert!(
            set.mutants
                .iter()
                .any(|m| m.kind == "const" && m.detail == "0 -> 1")
        );
        // No mutant may have corrupted the arrow tokens.
        for m in &set.mutants {
            assert!(
                m.source.contains("->") && m.source.contains("=>"),
                "arrow tokens must survive every mutant: {}",
                m.detail
            );
        }
    }

    /// Spans restrict sites: a line outside every span contributes nothing.
    #[test]
    fn spans_bound_the_sites() {
        let set = mutants_of(
            "fn f(a: i64) -> i64 {\n    a + 1\n}\nfn g(b: i64) -> i64 {\n    b * 2\n}\n",
            &[(4, 6)],
        );
        assert!(set.mutants.iter().all(|m| m.line >= 4));
        assert!(set.total_sites >= 2);
    }

    /// The budget caps emitted mutants while `total_sites` still reports the
    /// uncapped count, so the caller can announce the truncation.
    #[test]
    fn budget_truncation_is_visible() {
        let mut f = tempfile_path();
        std::fs::File::create(&f.0)
            .unwrap()
            .write_all(b"fn f(a: i64) -> i64 {\n    a + 1 + 2 + 3 + 4\n}\n")
            .unwrap();
        let set = mutants_for(&f.0, &[(1, 3)], 2).unwrap();
        let _ = std::fs::remove_file(&f.0);
        f.1 = false;
        assert_eq!(set.mutants.len(), 2);
        assert!(set.total_sites > 2);
    }
}
