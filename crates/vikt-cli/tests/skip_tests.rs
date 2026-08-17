//! Default test-skip for directory inputs, through the built binary, over a
//! fixture directory mixing one production Go file with its `_test.go`
//! suite: `widget.go`'s `BuildWidget`/`itoa` against `widget_test.go`'s
//! `TestBuildWidget`. Mirrors the case that motivated the behavior — a
//! table-driven Go test file dominating a repo-scope ranking of production
//! code — at integration-test scale rather than gorilla/mux scale.

use std::process::Command;
use std::process::Output;

const FIXTURE: &str = "tests/fixtures/skip-tests";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(args)
        .output()
        .expect("running the vikt binary")
}

fn function_names(stdout: &[u8]) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_slice(stdout).expect("stdout parses as JSON");
    v["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_owned())
        .collect()
}

/// By default, a directory input's walk never lowers `widget_test.go`:
/// `TestBuildWidget` is absent from the sidecar, the production functions
/// are present, and the skip is noted on stderr — never inside the JSON.
#[test]
fn folder_input_skips_test_files_by_default() {
    let out = run(&[]);
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names = function_names(&out.stdout);
    assert!(
        names.iter().any(|n| n == "BuildWidget"),
        "expected BuildWidget among the sidecar's functions: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "itoa"),
        "expected itoa among the sidecar's functions: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "TestBuildWidget"),
        "TestBuildWidget should have been skipped by default: {names:?}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("skipped 1 test file") && stderr.contains("--include-tests"),
        "expected a skip note on stderr, got: {stderr}"
    );
    let stdout_text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout_text.contains("skipped") && !stdout_text.contains("--include-tests"),
        "the skip note must never leak into the JSON: {stdout_text}"
    );
}

/// `--include-tests` restores the old walk-everything behavior:
/// `TestBuildWidget` is lowered and scored alongside the production
/// functions, and no skip note is printed.
#[test]
fn include_tests_flag_restores_the_test_file() {
    let out = run(&["--include-tests"]);
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names = function_names(&out.stdout);
    assert!(
        names.iter().any(|n| n == "TestBuildWidget"),
        "expected TestBuildWidget among the sidecar's functions under --include-tests: {names:?}"
    );
    assert!(names.iter().any(|n| n == "BuildWidget"));

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("skipped") || !stderr.contains("test file"),
        "no skip note is expected once nothing was skipped: {stderr}"
    );
}

/// An explicitly named single-file input always scores, test file or not —
/// the flag governs directory discovery, never an explicit file's fate.
#[test]
fn explicit_single_test_file_always_scores() {
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(format!("{FIXTURE}/widget_test.go"))
        .output()
        .expect("running the vikt binary");
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names = function_names(&out.stdout);
    assert!(
        names.iter().any(|n| n == "TestBuildWidget"),
        "an explicitly named test file must always score: {names:?}"
    );
}
