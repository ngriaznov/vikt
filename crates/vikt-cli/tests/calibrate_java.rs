//! The calibrate subcommand's Java engine, through the built binary, over
//! the checked-in `javac`-only fixture (no gradle, no JUnit — a bare `main`
//! test runner, matching how the fixture docs describe it). Skipped rather
//! than failed when `javac` is not on PATH.
//!
//! The fixture mirrors `tests/fixtures/calibrate`'s design exactly: every
//! method keeps its dead bookkeeping early and its behaviour late, so the
//! positional null is reliably wrong while the panel is reliably right. If
//! this test ever flakes, the fixture has drifted — see
//! `tests/calibrate.rs`'s module docs for the same note on the Python side.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const JAVA_FIXTURE: &str = "tests/fixtures/calibrate-java";

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every file under `root`, by relative path, byte for byte. `out/` is
/// excluded: a developer who ran the fixture's own build in place leaves
/// compiled classes calibrate neither copies nor touches.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("fixture directory is readable") {
            let path = entry.expect("fixture entry is readable").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "out") {
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

/// The full pipeline over the Java fixture: `--build-cmd` compiles every
/// `.java` file with a bare `javac` (no gradle), `--test-cmd` runs the
/// already-compiled `MathOpsTest`, the run reports both correlations and a
/// verdict, the panel beats the positional null, and the input tree comes
/// out byte-identical.
#[test]
fn calibrates_a_java_tree_and_leaves_it_untouched() {
    if !javac_available() {
        eprintln!("skipping: javac not on PATH");
        return;
    }
    let before = snapshot(Path::new(JAVA_FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            JAVA_FIXTURE,
            "--build-cmd",
            "javac -d out $(find . -name '*.java')",
            "--test-cmd",
            "java -cp out MathOpsTest",
            "--sample",
            "4",
            "--budget",
            "24",
            "--timeout-secs",
            "120",
            "--scope",
            "function",
        ])
        .output()
        .expect("running the vikt binary");
    let after = snapshot(Path::new(JAVA_FIXTURE));
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
        stdout.contains("Java mutants compile before they run"),
        "the per-mutant build cost must be announced up front:\n{stdout}"
    );
    assert!(
        stdout.contains("scored via vikt-ts/tree-sitter-java"),
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
