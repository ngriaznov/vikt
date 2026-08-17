//! Shared in-process textual mutation engine for `vikt calibrate`'s
//! source-splicing frontends.
//!
//! Some languages have no cheap AST to splice mutants through (Rust's
//! frontend lowers MIR, not a syntax tree it could re-render) or gain
//! nothing from one (Java/Kotlin have `vikt-ts` for *scoring*, but that
//! walker throws away everything but the lowering vocabulary — it has no
//! "write this node back out" direction). For all of them, a byte-level
//! splice on the original source is cheaper than standing up a
//! reprint-capable parser, at the cost that a splice can propose an edit the
//! language rejects: flipping a `<` that was really a generic bracket,
//! dividing through a dereference. That is priced in — the calibrate
//! pipeline runs a build step before the suite for every mutant from one of
//! these languages, and a mutant that does not compile is *invalid*, never a
//! kill (see [`vikt_core::mutant::MutantSet::invalid_discarded`] and each
//! language's `Language::needs_build_step`). This engine only has to keep
//! the obvious non-code out — string literals, comments, char literals —
//! which the byte mask below does without parsing.
//!
//! What varies between languages is purely lexical: comment markers, string
//! quoting, whether block comments nest, whether `'` opens a char literal
//! unconditionally or needs Rust's lifetime-vs-literal disambiguation. That
//! lexicon is [`LanguageSyntax`]; the mutation logic itself — which
//! operators swap for which, how integer literals perturb — is identical
//! across every language this engine serves, because every one of them
//! shares the same C-family operator vocabulary (`+ - * / < > <= >= == != &&
//! ||`). A language whose grammar reuses one of these tokens for something
//! else entirely (a generic bracket, an arrow) is not specially excluded;
//! the mandatory build step prices the resulting invalid mutants out rather
//! than this engine trying to parse enough of the language to know better.
//!
//! Splicing keeps every other line's number intact, so a kill is keyed to
//! exactly the line the panel scored.

use std::path::Path;

use crate::mutant::{Mutant, MutantSet};

/// How a language spells comments, strings and char literals — the only
/// per-language knob this engine has. The mutation logic that reads it
/// (operator/literal scanning) never varies.
#[derive(Debug, Clone, Copy)]
pub struct LanguageSyntax {
    /// Line-comment marker, e.g. `//`. `None` for a language with none.
    pub line_comment: Option<&'static str>,
    /// Block-comment open/close markers, e.g. `("/*", "*/")`.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// Whether a nested opener inside a block comment starts a fresh level
    /// (Rust, Kotlin) rather than being inert text the first closer ends
    /// the whole comment regardless of (Java, Go).
    pub block_comment_nests: bool,
    /// Non-escaping, delimiter-matched string kinds checked before the
    /// standard quote below, longest/most specific first so a delimiter
    /// that is a prefix of another (`"""` starts with `"`) is never
    /// mistaken for it: Kotlin's triple-quoted raw string, Go's backtick
    /// string. Empty for a language with neither.
    pub raw_quote_strings: &'static [&'static str],
    /// The standard escaping string's quote character, e.g. `"` for every
    /// language this engine serves.
    pub quote: char,
    /// Rust's `r"..."`/`r#"..."#`/`b"..."`/`br"..."` raw and byte strings —
    /// a prefix-triggered shape no other language modeled here has.
    pub rust_raw_strings: bool,
    /// How `'` is read.
    pub char_literal: CharLiteralRule,
}

/// How a language's `'` token disambiguates from other uses of the same
/// byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharLiteralRule {
    /// No char-literal syntax at all.
    None,
    /// `'` always opens a char literal, closed by the next unescaped `'`
    /// (Java, Kotlin, Go).
    Always,
    /// `'` opens a char literal only when it looks like one — followed by
    /// an escape (`'\n'`) or exactly one byte then a closing `'` (`'x'`) —
    /// and is read as code (a lifetime) otherwise (Rust's `'a`).
    LifetimeAmbiguous,
}

/// Rust: line and nesting-block comments, no raw quote strings of its own
/// kind (its raw strings are the prefix-triggered `rust_raw_strings` shape
/// instead), lifetime-ambiguous `'`.
pub const RUST: LanguageSyntax = LanguageSyntax {
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    block_comment_nests: true,
    raw_quote_strings: &[],
    quote: '"',
    rust_raw_strings: true,
    char_literal: CharLiteralRule::LifetimeAmbiguous,
};

/// Java: block comments do not nest (the language spec is explicit about
/// this — `/* /* */ */` ends at the first `*/`), no raw/triple-quoted
/// strings, `'` unconditionally opens a char literal.
pub const JAVA: LanguageSyntax = LanguageSyntax {
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    block_comment_nests: false,
    raw_quote_strings: &[],
    quote: '"',
    rust_raw_strings: false,
    char_literal: CharLiteralRule::Always,
};

/// Kotlin: block comments nest (unlike Java), triple-quoted raw strings
/// (`"""..."""`, no escapes) checked ahead of the standard quote, `'`
/// unconditionally opens a char literal (Kotlin has no lifetime syntax).
pub const KOTLIN: LanguageSyntax = LanguageSyntax {
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    block_comment_nests: true,
    raw_quote_strings: &["\"\"\""],
    quote: '"',
    rust_raw_strings: false,
    char_literal: CharLiteralRule::Always,
};

/// Go: block comments do not nest (same rule as Java), backtick raw
/// strings (no escapes, may span lines) checked ahead of the standard
/// quote, `'` unconditionally opens a rune literal.
pub const GO: LanguageSyntax = LanguageSyntax {
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    block_comment_nests: false,
    raw_quote_strings: &["`"],
    quote: '"',
    rust_raw_strings: false,
    char_literal: CharLiteralRule::Always,
};

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
#[allow(clippy::too_many_lines)] // one operator/literal family per match arm; splitting hides the shape
pub fn mutants_for(
    path: &Path,
    spans: &[(u32, u32)],
    limit: usize,
    syntax: &LanguageSyntax,
) -> std::io::Result<MutantSet> {
    let src = std::fs::read_to_string(path)?;
    let mask = code_mask(&src, syntax);
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
            // Rust turbofish `::<`. Generic brackets still slip through;
            // the build step prices those as invalid, not killed.
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
        // (`10u32`, `10L`) survive because only the digit run is replaced.
        // A `prev` byte >= 0x80 is a UTF-8 continuation byte: within masked
        // code that only occurs inside a Unicode identifier (`café2`),
        // never on its own, so it blocks a digit run exactly like an ASCII
        // alphanumeric would.
        let prev_is_ident = prev.is_ascii_alphanumeric() || prev == b'_' || prev >= 0x80;
        if b.is_ascii_digit() && !prev_is_ident && prev != b'.' {
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
/// byte-string literal, per `syntax`'s lexicon.
fn code_mask(src: &str, syntax: &LanguageSyntax) -> Vec<bool> {
    let b = src.as_bytes();
    let mut mask = vec![true; b.len()];
    let mut i = 0;
    while i < b.len() {
        if let Some(lc) = syntax.line_comment
            && b[i..].starts_with(lc.as_bytes())
        {
            while i < b.len() && b[i] != b'\n' {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if let Some((open, close)) = syntax.block_comment
            && b[i..].starts_with(open.as_bytes())
        {
            i = mask_block_comment(b, &mut mask, i, open, close, syntax.block_comment_nests);
            continue;
        }
        if let Some(delim) = syntax
            .raw_quote_strings
            .iter()
            .find(|d| b[i..].starts_with(d.as_bytes()))
        {
            i = mask_delimited(b, &mut mask, i, delim.as_bytes(), false);
            continue;
        }
        if syntax.quote.is_ascii() && b[i] == syntax.quote as u8 {
            i = mask_delimited(b, &mut mask, i, &[b[i]], true);
            continue;
        }
        if syntax.rust_raw_strings && matches!(b[i], b'r' | b'b') && is_raw_or_byte_string(b, i) {
            i = mask_rust_prefixed_string(b, &mut mask, i);
            continue;
        }
        if b[i] == b'\'' {
            let is_char_literal = match syntax.char_literal {
                CharLiteralRule::None => false,
                CharLiteralRule::Always => true,
                CharLiteralRule::LifetimeAmbiguous => {
                    b.get(i + 1) == Some(&b'\\') || b.get(i + 2) == Some(&b'\'')
                }
            };
            if is_char_literal {
                i = mask_delimited(b, &mut mask, i, b"'", true);
                continue;
            }
        }
        i += 1;
    }
    mask
}

/// A delimiter-matched region starting at `open` (the opening delimiter
/// itself, `delim` bytes long): masks through to the next occurrence of
/// `delim`, honoring backslash escapes when `escapes` is set. Covers every
/// string/char-literal shape this engine masks — a plain `"`-quoted string
/// (`delim = "\""`, escapes), a char literal (`delim = "'"`, escapes), a
/// Kotlin triple-quoted string (`delim = "\"\"\""`, no escapes) and a Go
/// backtick string (`delim = "\`"`, no escapes) all reduce to the same
/// scan. Returns the index after the closing delimiter, or the end of the
/// file for an unterminated region.
fn mask_delimited(b: &[u8], mask: &mut [bool], open: usize, delim: &[u8], escapes: bool) -> usize {
    let mut i = open;
    for k in 0..delim.len() {
        if let Some(m) = mask.get_mut(i + k) {
            *m = false;
        }
    }
    i += delim.len();
    while i < b.len() {
        if escapes && b[i] == b'\\' {
            mask[i] = false;
            i += 1;
            if i < b.len() {
                mask[i] = false;
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(delim) {
            for k in 0..delim.len() {
                mask[i + k] = false;
            }
            return i + delim.len();
        }
        mask[i] = false;
        i += 1;
    }
    i
}

/// A block comment starting at `open_at` (the position of `open`'s first
/// byte): masks the opening delimiter, then scans for `close`, tracking
/// nesting depth when `nests` is set (an inner `open` starts a fresh level,
/// as Rust and Kotlin both allow) or ignoring inner occurrences entirely
/// when it is not (Java and Go: the first `close` ends the comment no
/// matter what looks like a nested opener appears inside it). Returns the
/// index after the closing delimiter, or the end of the file for an
/// unterminated comment.
fn mask_block_comment(
    b: &[u8],
    mask: &mut [bool],
    open_at: usize,
    open: &str,
    close: &str,
    nests: bool,
) -> usize {
    let (ob, cb) = (open.as_bytes(), close.as_bytes());
    let mut i = open_at;
    for k in 0..ob.len() {
        if let Some(m) = mask.get_mut(i + k) {
            *m = false;
        }
    }
    i += ob.len();
    let mut depth = 1usize;
    while i < b.len() {
        if nests && b[i..].starts_with(ob) {
            depth += 1;
            for k in 0..ob.len() {
                mask[i + k] = false;
            }
            i += ob.len();
            continue;
        }
        if b[i..].starts_with(cb) {
            for k in 0..cb.len() {
                mask[i + k] = false;
            }
            i += cb.len();
            depth -= 1;
            if depth == 0 {
                return i;
            }
            continue;
        }
        mask[i] = false;
        i += 1;
    }
    i
}

/// Rust's `r"..."`/`r#"..."#`/`b"..."`/`br"..."` string, the caller having
/// already confirmed via [`is_raw_or_byte_string`] that `i` opens one:
/// masks the `r`/`b`/`#`-prefix through the opening quote, then dispatches
/// to a raw scan (no escapes, closed by `"` plus the same hash count) for
/// an `r`-prefixed string, or the ordinary escaping scan for a plain byte
/// string. Mirrors the shape every other delimited region here takes, but
/// kept as its own function because the closing delimiter's hash count is
/// only known after scanning the prefix, not fixed up front the way every
/// other shape's is.
fn mask_rust_prefixed_string(b: &[u8], mask: &mut [bool], i: usize) -> usize {
    let mut j = i + 1;
    if b.get(j) == Some(&b'r') {
        j += 1;
    }
    let mut hashes = 0;
    while b.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    let Some(&b'"') = b.get(j) else {
        return i + 1;
    };
    for m in &mut mask[i..=j] {
        *m = false;
    }
    if b[i] == b'r' || b.get(i + 1) == Some(&b'r') {
        mask_raw_string(b, mask, j, hashes)
    } else {
        mask_delimited(b, mask, j, b"\"", true)
    }
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
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn mutants_of(
        src: &str,
        spans: &[(u32, u32)],
        syntax: &LanguageSyntax,
        ext: &str,
    ) -> MutantSet {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vikt-core-textmut-{}-{seq}.{ext}",
            std::process::id()
        ));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(src.as_bytes())
            .unwrap();
        let set = mutants_for(&path, spans, usize::MAX, syntax).unwrap();
        let _ = std::fs::remove_file(&path);
        set
    }

    /// Java's block comments do not nest: `/* /* */ x */` ends at the
    /// first `*/`, leaving ` x */` as live code — so the stray `*/` there
    /// is code the mask must not swallow, and a `+` inside it is a real
    /// mutation site.
    #[test]
    fn java_block_comments_do_not_nest() {
        let set = mutants_of(
            "class C {\n  int f() {\n    /* /* nested */ return 1 + 1; */\n  }\n}\n",
            &[(1, 5)],
            &JAVA,
            "java",
        );
        // Nothing after the real closing `*/` on line 3 is code in this
        // fixture (`*/` at the end closes nothing further), so no `+`
        // mutant should ever appear: the whole line reads as one comment
        // from Java's non-nesting rule, `/* /* nested */` opens and the
        // *first* `*/` ends it, then ` return 1 + 1; */` is live code, and
        // that trailing `*/` is a syntax error a Java parser would reject
        // (out of scope for this masking test — the mask only needs to
        // agree with the language on where the comment itself ends).
        assert!(
            set.mutants.iter().any(|m| m.kind == "bin"),
            "the `+` after Java's non-nesting close must be a live mutation site: {:?}",
            set.mutants.iter().map(|m| &m.detail).collect::<Vec<_>>()
        );
    }

    /// Kotlin's block comments nest: the same source that ends a Java
    /// comment early keeps masking through to the *matching* close, so the
    /// `+` inside the nested comment is never a mutation site.
    #[test]
    fn kotlin_block_comments_nest() {
        let set = mutants_of(
            "fun f(): Int {\n  /* outer /* inner 1 + 1 */ still comment */\n  return 2 + 2\n}\n",
            &[(1, 4)],
            &KOTLIN,
            "kt",
        );
        assert!(
            set.mutants.iter().all(|m| m.line == 3),
            "nothing inside the nested comment may mutate, got lines {:?}",
            set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
        );
        assert!(set.mutants.iter().any(|m| m.kind == "bin" && m.line == 3));
    }

    /// Kotlin's triple-quoted strings are raw: a `+` or `<` inside one is
    /// never a mutation site, even though it looks exactly like the
    /// operator this engine otherwise mutates.
    #[test]
    fn kotlin_triple_quoted_strings_are_masked() {
        let set = mutants_of(
            "fun f(): Int {\n  val s = \"\"\"a + b < c\"\"\"\n  return 1 + 1\n}\n",
            &[(1, 4)],
            &KOTLIN,
            "kt",
        );
        assert!(
            set.mutants.iter().all(|m| m.line == 3),
            "only the code on line 3 may mutate; the triple-quoted string on line 2 must stay untouched, got lines {:?}",
            set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
        );
    }

    /// Go's backtick strings are raw, same treatment as Kotlin's triple
    /// quotes: content inside is never a mutation site.
    #[test]
    fn go_backtick_strings_are_masked() {
        let set = mutants_of(
            "func f() int {\n\tvar s = `a + b < c`\n\treturn 1 + 1\n}\n",
            &[(1, 4)],
            &GO,
            "go",
        );
        assert!(
            set.mutants.iter().all(|m| m.line == 3),
            "only line 3's code may mutate; the backtick string on line 2 must stay untouched, got lines {:?}",
            set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
        );
    }

    /// Java and Kotlin both read `'` as an unconditional char literal — no
    /// lifetime ambiguity to resolve — so its contents are always masked.
    #[test]
    fn always_char_literal_rule_masks_unconditionally() {
        for (syntax, ext) in [(&JAVA, "java"), (&KOTLIN, "kt")] {
            let set = mutants_of(
                "x() {\n  char c = '+';\n  return 1 + 1;\n}\n",
                &[(1, 4)],
                syntax,
                ext,
            );
            assert!(
                set.mutants.iter().all(|m| m.line == 3),
                "the `+` inside the char literal on line 2 must not mutate ({ext}), got lines {:?}",
                set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
            );
        }
    }

    /// Every language's line comments are masked identically to Rust's.
    #[test]
    fn line_comments_are_masked_for_every_language() {
        for (syntax, ext) in [(&JAVA, "java"), (&KOTLIN, "kt"), (&GO, "go")] {
            let set = mutants_of(
                "x() {\n  // a + b\n  return 1 + 1;\n}\n",
                &[(1, 4)],
                syntax,
                ext,
            );
            assert!(
                set.mutants.iter().all(|m| m.line == 3),
                "the `+` inside the line comment on line 2 must not mutate ({ext}), got lines {:?}",
                set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
            );
        }
    }
}
