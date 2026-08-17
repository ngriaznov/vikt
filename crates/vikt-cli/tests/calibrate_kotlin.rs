//! The calibrate subcommand's Kotlin engine, through the built binary, over
//! the checked-in `kotlinc`-only fixture (no gradle). Skipped rather than
//! failed when `kotlinc` is not on PATH — this toolchain is not installed
//! by the test run itself, only used when already present.
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
const KOTLIN_FIXTURE: &str = "tests/fixtures/calibrate-kotlin";

fn kotlinc_available() -> bool {
    Command::new("kotlinc")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every file under `root`, by relative path, byte for byte. `test.jar` is
/// excluded: a developer who ran the fixture's own build in place leaves a
/// compiled jar calibrate neither produces nor touches.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("fixture directory is readable") {
            let path = entry.expect("fixture entry is readable").path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().and_then(|e| e.to_str()) != Some("jar") {
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

/// The full pipeline over the Kotlin fixture: `--build-cmd` compiles every
/// `.kt` file with a bare `kotlinc -include-runtime` (no gradle),
/// `--test-cmd` runs the already-built jar, the run reports both
/// correlations and a verdict, the panel beats the positional null, and the
/// input tree comes out byte-identical.
#[test]
fn calibrates_a_kotlin_tree_and_leaves_it_untouched() {
    if !kotlinc_available() {
        eprintln!("skipping: kotlinc not on PATH");
        return;
    }
    let before = snapshot(Path::new(KOTLIN_FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            KOTLIN_FIXTURE,
            "--build-cmd",
            "kotlinc -include-runtime -d test.jar $(find . -name '*.kt')",
            "--test-cmd",
            "java -jar test.jar",
            "--sample",
            "4",
            "--budget",
            "24",
            "--timeout-secs",
            "240",
            "--scope",
            "function",
        ])
        .output()
        .expect("running the vikt binary");
    let after = snapshot(Path::new(KOTLIN_FIXTURE));
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
        stdout.contains("Kotlin mutants compile before they run"),
        "the per-mutant build cost must be announced up front:\n{stdout}"
    );
    assert!(
        stdout.contains("scored via vikt-ts/tree-sitter-kotlin"),
        "the run must say which lowering scored it:\n{stdout}"
    );
    assert!(
        stdout.contains("verdict:"),
        "a verdict must be rendered:\n{stdout}"
    );
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
