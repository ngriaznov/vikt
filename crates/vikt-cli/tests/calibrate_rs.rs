//! The calibrate subcommand over the Rust fixture package. Skips rather than
//! fails when the nightly MIR helper is not built — the same policy as
//! `crates/vikt-rs/tests` — since lowering a cargo package is impossible
//! without it.
//!
//! The fixture mirrors the Python and JavaScript ones: dead bookkeeping
//! early, behaviour late. Budget and sample are kept small because every
//! Rust mutant costs a build; the assertions are therefore structural (the
//! pipeline ran, reported its counts, judged, and left the tree untouched)
//! plus the one measurement this fixture is designed to make stable: the
//! panel beats the positional null.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const FIXTURE: &str = "tests/fixtures/calibrate-rs";

/// The helper as `vikt-rs` discovers it, made absolute so the child process
/// finds it regardless of its own working directory.
fn helper_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VIKT_RUST_LOWER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for rel in [
        "../../tools/rust-lower/target/release/vikt-rust-lower",
        "../../tools/rust-lower/target/debug/vikt-rust-lower",
    ] {
        if let Ok(p) = Path::new(rel).canonicalize() {
            return Some(p);
        }
    }
    None
}

/// Every file under `root`, by relative path, byte for byte. `target/` is
/// excluded: a developer who ran the fixture's own tests in place leaves
/// build output that calibrate neither copies nor touches.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("fixture directory is readable") {
            let path = entry.expect("fixture entry is readable").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .expect("path is under root")
                        .to_path_buf(),
                    std::fs::read(&path).expect("fixture file is readable"),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The full Rust pipeline: build-validated mutants, kill-rate correlations, a
/// verdict, the panel beating the null on the designed fixture, and the input
/// tree byte-identical afterward.
#[test]
fn calibrates_the_rust_fixture_and_leaves_it_untouched() {
    let Some(helper) = helper_path() else {
        eprintln!("skipping: vikt-rust-lower not built (tools/rust-lower)");
        return;
    };
    let before = snapshot(Path::new(FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "cargo test --quiet",
            "--sample",
            "4",
            "--budget",
            "24",
            "--timeout-secs",
            "240",
            "--scope",
            "function",
        ])
        .env("VIKT_RUST_LOWER", &helper)
        .output()
        .expect("running the vikt binary");
    let after = snapshot(Path::new(FIXTURE));
    assert_eq!(
        before, after,
        "calibrate must leave the input tree byte-identical"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "vikt failed: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("Rust mutants compile before they run"),
        "the per-mutant build cost must be announced up front:\n{stdout}"
    );
    assert!(
        stdout.contains("pooled Spearman rho"),
        "correlations must be reported:\n{stdout}"
    );
    assert!(
        stdout.contains("verdict:"),
        "a verdict must be rendered:\n{stdout}"
    );
    // The designed fixture: inert bookkeeping first, behaviour last. The
    // panel must order that better than "earlier is more important".
    let panel = extract(&stdout, "  panel");
    let null = extract(&stdout, "  positional null");
    assert!(
        panel > null,
        "the panel ({panel}) must beat the positional null ({null}) on the designed fixture:\n{stdout}"
    );
}

/// Pulls the trailing float from the report line starting with `prefix`.
fn extract(stdout: &str, prefix: &str) -> f64 {
    stdout
        .lines()
        .find(|l| l.starts_with(prefix))
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("missing `{prefix}` line in:\n{stdout}"))
}

/// `--lowering ast` calibrates the same fixture through the tree-sitter
/// fallback instead of the MIR helper — no `VIKT_RUST_LOWER`, no nightly
/// toolchain for scoring at all (the build/test commands still run on
/// whatever stable `cargo` is on PATH, same as every other calibrate run).
/// Mutant *generation* is unaffected: `vikt_rs::calibrate` still splices
/// text and still needs a build to validate each mutant, so this exercises
/// the whole pipeline, not just scoring in isolation.
#[test]
fn calibrates_the_rust_fixture_via_tree_sitter_without_the_helper() {
    let before = snapshot(Path::new(FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "cargo test --quiet",
            "--sample",
            "4",
            "--budget",
            "16",
            "--timeout-secs",
            "240",
            "--scope",
            "function",
            "--lowering",
            "ast",
        ])
        .env_remove("VIKT_RUST_LOWER")
        .output()
        .expect("running the vikt binary");
    let after = snapshot(Path::new(FIXTURE));
    assert_eq!(
        before, after,
        "calibrate must leave the input tree byte-identical"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "vikt failed: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("scored via vikt-ts/tree-sitter-rust (statement profile)"),
        "the run must say which lowering scored it:\n{stdout}"
    );
    assert!(
        stdout.contains("verdict:"),
        "a verdict must still be rendered:\n{stdout}"
    );
}
