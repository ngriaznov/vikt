//! The calibrate subcommand's Go engine, through the built binary, over the
//! checked-in Go module fixture. Skipped rather than failed when `go` is
//! not on PATH.
//!
//! The fixture mirrors `tests/fixtures/calibrate-java`'s design exactly:
//! every function keeps its dead bookkeeping early and its behaviour late,
//! so the positional null is reliably wrong while the panel is reliably
//! right. If this test ever flakes, the fixture has drifted — see
//! `tests/calibrate.rs`'s module docs for the same note on the Python side.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const GO_FIXTURE: &str = "tests/fixtures/calibrate-go";

fn go_available() -> bool {
    Command::new("go")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every file under `root`, by relative path, byte for byte.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("fixture directory is readable") {
            let path = entry.expect("fixture entry is readable").path();
            if path.is_dir() {
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

/// The full pipeline over the Go fixture: no `--build-cmd` needed (Go's
/// default is `go vet ./...`), `--test-cmd` runs `go test ./...`, the run
/// reports both correlations and a verdict, the panel beats the positional
/// null, and the input tree comes out byte-identical.
#[test]
fn calibrates_a_go_tree_and_leaves_it_untouched() {
    if !go_available() {
        eprintln!("skipping: go not on PATH");
        return;
    }
    let before = snapshot(Path::new(GO_FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            GO_FIXTURE,
            "--test-cmd",
            "go test ./...",
            "--sample",
            "4",
            "--budget",
            "40",
            "--timeout-secs",
            "120",
            "--scope",
            "function",
        ])
        .output()
        .expect("running the vikt binary");
    let after = snapshot(Path::new(GO_FIXTURE));
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
        stdout.contains("Go mutants compile before they run"),
        "the per-mutant build cost must be announced up front:\n{stdout}"
    );
    assert!(
        stdout.contains("scored via vikt-ts/tree-sitter-go"),
        "the run must say which lowering scored it:\n{stdout}"
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
