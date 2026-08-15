//! `--lowering <auto|primary|ast>` through the built binary: the tree-sitter
//! fallback for Rust and Python, and the new source-level Java/Kotlin
//! capability. Every test here is helper-free by design — none requires
//! `vikt-rust-lower` to be built, and the Python cases that matter most
//! (`ast`) do not require `python3` either, since forcing the fallback is
//! the whole point.

use std::path::Path;
use std::process::Command;

const RUST_DEMO: &str = "../../demo/rust/orders.rs";
const PYTHON_DEMO: &str = "../../demo/python/orders.py";
const KOTLIN_DEMO: &str = "../../demo/kotlin/Orders.kt";
const JAVA_DEMO: &str = "../../demo/java/OrderProcessor.java";
const JS_FIXTURE: &str = "tests/fixtures/scope/hub.js";

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args(args)
        // The MIR helper is never on PATH in this environment (it's a
        // nightly-only tool, excluded from the workspace build) and no test
        // here sets it - every Rust case is exercised without it.
        .env_remove("VIKT_RUST_LOWER")
        .output()
        .expect("running the vikt binary")
}

fn parse_sidecar(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout parses as JSON")
}

fn function_names(sidecar: &serde_json::Value) -> Vec<&str> {
    sidecar["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .map(|f| f["name"].as_str().expect("name is a string"))
        .collect()
}

/// Whether the MIR helper is discoverable the way `vikt_rs::find_helper`
/// looks for it - only used to decide whether the `auto`-without-helper
/// test is exercising a real fallback or a no-op, mirroring the skip policy
/// `crates/vikt-rs/tests` and `calibrate_rs.rs` already use for the inverse
/// case.
fn helper_available() -> bool {
    if std::env::var_os("VIKT_RUST_LOWER").is_some() {
        return true;
    }
    if Command::new("vikt-rust-lower")
        .arg("--help")
        .output()
        .is_ok()
    {
        return true;
    }
    [
        "../../tools/rust-lower/target/release/vikt-rust-lower",
        "../../tools/rust-lower/target/debug/vikt-rust-lower",
    ]
    .iter()
    .any(|p| Path::new(p).is_file())
}

/// `--lowering ast` on a `.rs` file: forced tree-sitter regardless of
/// whether the helper is available, so this is the one Rust test immune to
/// the environment either way.
#[test]
fn rust_lowering_ast_uses_tree_sitter() {
    let out = run(&[RUST_DEMO, "--lowering", "ast", "--stats"]);
    let sidecar = parse_sidecar(&out);
    assert_eq!(sidecar["generator"], "vikt-ts/tree-sitter-rust");
    let names = function_names(&sidecar);
    assert!(names.contains(&"process"), "found: {names:?}");

    // Sensible tiers: at least one core line, and the file's coverage isn't
    // suspiciously empty - a walker bug that silently produced zero nodes
    // would still exit 0 and pass the generator check above.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("lines"),
        "the --stats histogram must be printed:\n{stderr}"
    );
    let process = sidecar["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "process")
        .expect("process function in the sidecar");
    let tiers: Vec<&str> = process["spans"]
        .as_array()
        .expect("spans array")
        .iter()
        .map(|s| s["tier"].as_str().unwrap())
        .collect();
    assert!(
        tiers.contains(&"core"),
        "the loop/branch-carrying body must produce at least one core line: {tiers:?}"
    );
}

/// `--lowering auto` (the default) without the helper: falls back silently
/// but noted, same generator as the forced case above. Skips if this
/// environment happens to have the helper built - the point of this test is
/// specifically the absence.
#[test]
fn rust_lowering_auto_falls_back_without_the_helper() {
    if helper_available() {
        eprintln!("skipping: vikt-rust-lower is available in this environment");
        return;
    }
    let out = run(&[RUST_DEMO]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("vikt-rust-lower not found"),
        "auto must note the fallback on stderr:\n{stderr}"
    );
    let sidecar = parse_sidecar(&out);
    assert_eq!(sidecar["generator"], "vikt-ts/tree-sitter-rust");
}

/// `--lowering primary` without the helper: the documented hard error,
/// unchanged from before this task - `auto`/`ast` exist precisely so this
/// stays available as an explicit choice.
#[test]
fn rust_lowering_primary_without_the_helper_still_errors() {
    if helper_available() {
        eprintln!("skipping: vikt-rust-lower is available in this environment");
        return;
    }
    let out = run(&[RUST_DEMO, "--lowering", "primary"]);
    assert!(
        !out.status.success(),
        "primary must fail without the helper"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("vikt-rust-lower not found"),
        "the original helper-missing error text must survive: {stderr}"
    );
}

/// `--lowering ast` on a `.py` file: forced tree-sitter, and - the point of
/// this test - never touches `python3` at all, so it is meaningful
/// regardless of whether this environment has an interpreter.
#[test]
fn python_lowering_ast_uses_tree_sitter() {
    let out = run(&[PYTHON_DEMO, "--lowering", "ast", "--stats"]);
    let sidecar = parse_sidecar(&out);
    assert_eq!(sidecar["generator"], "vikt-ts/tree-sitter-python");
    let names = function_names(&sidecar);
    assert!(!names.is_empty(), "at least one function must be lowered");
}

/// `--lowering ast` doesn't apply to JS/TS: a hard error naming oxc as the
/// reason, not a silent no-op.
#[test]
fn lowering_ast_on_javascript_is_a_hard_error() {
    let out = run(&[JS_FIXTURE, "--lowering", "ast"]);
    assert!(!out.status.success(), "ast on .js must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("oxc"),
        "the error must explain oxc IS the AST path: {stderr}"
    );
}

/// `.kt` source, new capability: the demo file's methods come back named
/// `Type::method`, matching the JVM frontend's own naming shape.
#[test]
fn kotlin_source_scores_with_jvm_style_method_names() {
    let out = run(&[KOTLIN_DEMO, "--stats"]);
    let sidecar = parse_sidecar(&out);
    assert_eq!(sidecar["generator"], "vikt-ts/tree-sitter-kotlin");
    let names = function_names(&sidecar);
    assert!(names.contains(&"Processor::plain"), "found: {names:?}");
    assert!(
        names.contains(&"Processor::fetchAndTotal"),
        "found: {names:?}"
    );
    // `--lowering` is meaningless here - there is no primary at source
    // level - so every value must resolve identically.
    for mode in ["auto", "primary", "ast"] {
        let out = run(&[KOTLIN_DEMO, "--lowering", mode]);
        let sidecar = parse_sidecar(&out);
        assert_eq!(sidecar["generator"], "vikt-ts/tree-sitter-kotlin");
    }
}

/// `.java` source, new capability: same naming contract as Kotlin above.
#[test]
fn java_source_scores_with_jvm_style_method_names() {
    let out = run(&[JAVA_DEMO, "--stats"]);
    let sidecar = parse_sidecar(&out);
    assert_eq!(sidecar["generator"], "vikt-ts/tree-sitter-java");
    let names = function_names(&sidecar);
    assert!(
        names.contains(&"OrderProcessor::process"),
        "found: {names:?}"
    );
}

/// The unsupported-extension error text now mentions every extension this
/// binary understands, including the two new source-level capabilities.
#[test]
fn unsupported_extension_error_mentions_java_and_kotlin() {
    let dir = std::env::temp_dir().join(format!(
        "vikt-tree-sitter-test-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let bogus = dir.join("notes.txt");
    std::fs::write(&bogus, "not a source file").unwrap();

    let out = run(&[bogus.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(".java"), "error text: {stderr}");
    assert!(stderr.contains(".kt"), "error text: {stderr}");
    assert!(stderr.contains(".rs"), "error text: {stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A parse-error stub kept out of the assertions above but exercised here:
/// the tree-sitter path degrades gracefully rather than panicking on real
/// demo files, over every language this task added or touched.
#[test]
fn every_demo_lowers_without_diagnostics_reported_on_stderr() {
    for demo in [RUST_DEMO, PYTHON_DEMO, KOTLIN_DEMO, JAVA_DEMO] {
        let mode: &[&str] = if demo == RUST_DEMO || demo == PYTHON_DEMO {
            &["--lowering", "ast"]
        } else {
            &[]
        };
        let mut args: Vec<&str> = vec![demo];
        args.extend_from_slice(mode);
        let out = run(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{demo} must lower cleanly: {stderr}");
        assert!(
            !stderr.contains("parse error"),
            "{demo} must not degrade any function: {stderr}"
        );
    }
}
