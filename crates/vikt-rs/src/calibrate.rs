//! In-process textual mutation engine for `vikt calibrate`'s Rust frontend.
//!
//! Rust mutants are textual splices, not AST rewrites: the frontend lowers
//! MIR, and re-lowering per candidate site just to reserialize source would
//! cost a compile each. A splice can therefore propose an edit the language
//! rejects — flipping a `<` that was really a generic bracket, dividing
//! through a dereference. That is priced in: the calibrate pipeline runs a
//! build step before the suite for every Rust mutant, and a mutant that does
//! not compile is *invalid*, never a kill.
//!
//! The engine itself — masking non-code bytes, scanning for operators and
//! integer literals — is [`vikt_core::textmut`], shared with the Java,
//! Kotlin and Go frontends; this module only supplies Rust's lexicon
//! ([`vikt_core::textmut::RUST`]: nesting block comments, lifetime-vs-char-
//! literal disambiguation, `r"..."`/`b"..."` raw and byte strings) and keeps
//! its own public signature so callers never see the shared crate.

use std::path::Path;

use vikt_core::mutant::MutantSet;
use vikt_core::textmut;

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
    textmut::mutants_for(path, spans, limit, &textmut::RUST)
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

    /// A multi-byte escape body (`\u{...}`) inside a char literal is masked
    /// in full, not just its first byte: the digits and braces inside it
    /// must never surface as (invalid) mutation sites.
    #[test]
    fn unicode_escape_char_literal_is_fully_masked() {
        let set = mutants_of(
            "fn f(a: i64) -> i64 {\n    let c = '\\u{2764}';\n    a + 1\n}\n",
            &[(1, 4)],
        );
        assert!(
            set.mutants.iter().all(|m| m.line == 3),
            "only line 3's code may mutate; the char literal on line 2 must stay untouched, got lines {:?}",
            set.mutants.iter().map(|m| m.line).collect::<Vec<_>>()
        );
    }

    /// A digit run right after a non-ASCII identifier byte (a UTF-8
    /// continuation byte, always >= 0x80) belongs to the identifier, not a
    /// standalone integer literal — `café2` must not be spliced into
    /// `café3`.
    #[test]
    fn digit_after_non_ascii_identifier_is_not_mutated() {
        let set = mutants_of(
            "fn f(caf\u{e9}2: i64) -> i64 {\n    caf\u{e9}2 + 1\n}\n",
            &[(1, 3)],
        );
        let consts: Vec<_> = set.mutants.iter().filter(|m| m.kind == "const").collect();
        assert!(
            consts.iter().all(|m| m.detail == "1 -> 0"),
            "only the standalone `1` literal may mutate, got {:?}",
            consts.iter().map(|m| &m.detail).collect::<Vec<_>>()
        );
        for m in &set.mutants {
            assert!(
                m.source.matches("caf\u{e9}2").count() == 2,
                "both occurrences of the identifier must survive every mutant: {}",
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
