//! The mutant generator through a real interpreter.
//!
//! Skipped rather than failed when no interpreter is present.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use salience_py::calibrate::{MutantSet, mutants_for};

/// The same explicit probe the CLI tests use: skipping is only ever for a
/// missing interpreter, so that with python3 present a broken generator
/// fails the suite instead of passing it vacuously.
fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Writes `source` to a uniquely named temp file.
fn write_source(source: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "salience-py-mutants-{}-{seq}.py",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("temp file is writable");
    path
}

/// `None` only when no interpreter is present; any other error is a real
/// generator failure and panics.
fn mutants(source: &str, spans: &[(u32, u32)], limit: usize) -> Option<MutantSet> {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return None;
    }
    let path = write_source(source);
    let set = mutants_for(&path, spans, limit, "python3");
    let _ = std::fs::remove_file(&path);
    Some(set.expect("mutant generation succeeds with python3 present"))
}

const SOURCE: &str = r#""""Module docstring: exempt from mutation."""


def total(items):
    """Function docstring: also exempt."""
    acc = 0
    for x in items:
        if x >= 3:
            acc = acc + x
    return acc
"#;

/// Sites land only inside the requested span, on the operator and constant
/// lines, and never on a docstring — a docstring lowers to no instruction,
/// so there would be no panel score for such a mutant to check against.
#[test]
fn sites_respect_spans_and_docstrings() {
    let Some(set) = mutants(SOURCE, &[(4, 10)], 1000) else {
        return;
    };
    assert_eq!(
        set.total_sites,
        set.mutants.len(),
        "limit above total emits all"
    );
    assert!(
        set.total_sites >= 6,
        "cmp, arith, const and deletion sites all land"
    );
    for m in &set.mutants {
        assert!(
            (4..=10).contains(&m.line),
            "site at line {} escapes the span",
            m.line
        );
        assert_ne!(m.line, 5, "the docstring line must not be mutated");
        assert!(!m.source.is_empty());
        assert_ne!(m.source, SOURCE, "a mutant differs from the original");
    }
    // The kill decision is keyed on the original line, so mutants must be
    // usable in any order; the contract sorts them by line for stable
    // truncation under a budget.
    assert!(set.mutants.is_sorted_by_key(|m| m.line));
}

/// The limit caps what is emitted, never what is counted: truncation must be
/// reportable, so `total_sites` stays at the uncapped count. Limit zero is
/// the count-only probe.
#[test]
fn limit_caps_emission_not_the_count() {
    let Some(all) = mutants(SOURCE, &[(4, 10)], 1000) else {
        return;
    };
    let Some(capped) = mutants(SOURCE, &[(4, 10)], 3) else {
        return;
    };
    assert_eq!(capped.mutants.len(), 3);
    assert_eq!(capped.total_sites, all.total_sites);
    let Some(counted) = mutants(SOURCE, &[(4, 10)], 0) else {
        return;
    };
    assert!(counted.mutants.is_empty());
    assert_eq!(counted.total_sites, all.total_sites);
}

/// Two runs over the same source produce the same mutants in the same order:
/// calibration re-runs must sample and execute identically.
#[test]
fn generation_is_deterministic() {
    let Some(a) = mutants(SOURCE, &[(4, 10)], 1000) else {
        return;
    };
    let Some(b) = mutants(SOURCE, &[(4, 10)], 1000) else {
        return;
    };
    let key = |s: &MutantSet| {
        s.mutants
            .iter()
            .map(|m| (m.line, m.kind.clone(), m.detail.clone(), m.source.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&a), key(&b));
}

/// A file the interpreter cannot parse surfaces as `Failed` carrying the
/// interpreter's own message, exactly like the lowering.
#[test]
fn syntax_errors_surface() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let path = write_source("def broken(:\n");
    let result = mutants_for(&path, &[(1, 2)], 10, "python3");
    let _ = std::fs::remove_file(&path);
    match result {
        Err(salience_py::PyError::Failed { stderr, .. }) => {
            assert!(stderr.contains("SyntaxError"), "stderr was: {stderr}");
        }
        other => panic!("a syntax error must surface as Failed, got {other:?}"),
    }
}
