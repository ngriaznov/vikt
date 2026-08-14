//! `--scope file` through the built binary, over a small JS fixture with a
//! hub function and thin wrappers. JS is used because `vikt_js::lower_file`
//! runs in-process through oxc — no external toolchain, so unlike the
//! Python/JS calibrate tests this one never skips.

use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const FIXTURE: &str = "tests/fixtures/scope/hub.js";

fn run(args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(args)
        .output()
        .expect("running the vikt binary");
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout parses as JSON")
}

/// The peak (highest within-function `score`) span of a named function, and
/// its `file_score`.
fn peak_file_score(v: &serde_json::Value, name: &str) -> f64 {
    let f = v["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .find(|f| f["name"] == name)
        .unwrap_or_else(|| panic!("no function named {name} in sidecar"));
    let spans = f["spans"].as_array().expect("spans array");
    let peak = spans
        .iter()
        .max_by(|a, b| {
            a["function_score"]
                .as_f64()
                .unwrap()
                .partial_cmp(&b["function_score"].as_f64().unwrap())
                .unwrap()
        })
        .unwrap_or_else(|| panic!("{name} has no spans"));
    peak["file_score"]
        .as_f64()
        .unwrap_or_else(|| panic!("{name}'s peak span has no file_score: {peak}"))
}

/// Under `--scope function`, no span anywhere carries a `file_score` key at
/// all - the field is skipped, not merely `null`, so readers opting out of
/// file scope see exactly the artifact this tool emitted before the feature
/// existed.
#[test]
fn function_scope_omits_file_score_entirely() {
    let v = run(&["--scope", "function"]);
    for f in v["functions"].as_array().unwrap() {
        for s in f["spans"].as_array().unwrap() {
            assert!(
                s.get("file_score").is_none(),
                "function scope must omit file_score, found it on {s}"
            );
        }
    }
}

/// `--scope file` is the default spelled out; its JSON output must be
/// byte-identical to a bare invocation, and a bare invocation carries
/// `file_score` spans.
#[test]
fn explicit_file_scope_is_byte_identical_to_default() {
    let bare = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .output()
        .expect("running the vikt binary");
    let explicit = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--scope", "file"])
        .output()
        .expect("running the vikt binary");
    assert!(bare.status.success() && explicit.status.success());
    assert_eq!(
        bare.stdout, explicit.stdout,
        "--scope file must not change a single byte of the default output"
    );
    let v: serde_json::Value = serde_json::from_slice(&bare.stdout).unwrap();
    // Panics if the default output lacks file_score spans.
    let _ = peak_file_score(&v, "hub");
}

/// Under `--scope file`, the hub's own peak line outranks every wrapper's
/// peak line: the hub is called by three wrappers and carries most of the
/// file's real work, so its call-graph weight dominates even though each
/// wrapper's single call is *locally* its most important line.
#[test]
fn file_scope_ranks_the_hub_above_its_wrappers() {
    let v = run(&["--scope", "file"]);
    let hub = peak_file_score(&v, "hub");
    for wrapper in ["wrapperA", "wrapperB", "wrapperC"] {
        let w = peak_file_score(&v, wrapper);
        assert!(
            hub > w,
            "hub's file_score peak {hub} should outrank {wrapper}'s peak {w}"
        );
    }
}

/// `--scope file` never touches tiers: the tier a line carries is identical
/// to the default run, only `file_score` is added.
#[test]
fn file_scope_leaves_tiers_untouched() {
    let function_scope = run(&["--scope", "function"]);
    let file_scope = run(&["--scope", "file"]);
    let tiers = |v: &serde_json::Value| -> Vec<(String, String, String)> {
        v["functions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| {
                let name = f["name"].as_str().unwrap().to_owned();
                f["spans"].as_array().unwrap().iter().map(move |s| {
                    (
                        name.clone(),
                        s["start"].to_string(),
                        s["tier"].as_str().unwrap().to_owned(),
                    )
                })
            })
            .collect()
    };
    assert_eq!(tiers(&function_scope), tiers(&file_scope));
}

/// `--annotate` carries the file-score column by default; `--scope
/// function` drops it.
#[test]
fn annotate_column_present_by_default_absent_under_function_scope() {
    let default_out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--annotate", FIXTURE])
        .output()
        .expect("running the vikt binary");
    let function_out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--scope", "function", "--annotate", FIXTURE])
        .output()
        .expect("running the vikt binary");
    assert!(default_out.status.success() && function_out.status.success());

    let default_text = String::from_utf8_lossy(&default_out.stdout);
    let function_text = String::from_utf8_lossy(&function_out.stdout);
    let default_line = default_text
        .lines()
        .find(|l| l.contains("function hub"))
        .expect("annotated hub declaration line, default scope");
    let function_line = function_text
        .lines()
        .find(|l| l.contains("function hub"))
        .expect("annotated hub declaration line, function scope");
    assert!(
        default_line.len() > function_line.len(),
        "default (file) scope should carry the score column: {function_line:?} vs {default_line:?}"
    );
}

/// `--format sarif` is untouched by `--scope`: results are one-per-line,
/// ordered by the sidecar's own function order, never ranked against each
/// other, so file scope has nothing to change there.
#[test]
fn sarif_output_is_unaffected_by_scope() {
    let function_scope = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--format", "sarif"])
        .output()
        .expect("running the vikt binary");
    let file_scope = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--format", "sarif", "--scope", "file"])
        .output()
        .expect("running the vikt binary");
    assert!(function_scope.status.success() && file_scope.status.success());
    assert_eq!(
        function_scope.stdout, file_scope.stdout,
        "--scope must not change SARIF output"
    );
}

/// `--format text` names the file-score column by default; `--scope
/// function` drops it.
#[test]
fn text_format_carries_file_score_by_default_not_under_function_scope() {
    let default_out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--format", "text"])
        .output()
        .expect("running the vikt binary");
    let function_out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(FIXTURE)
        .args(["--format", "text", "--scope", "function"])
        .output()
        .expect("running the vikt binary");
    assert!(default_out.status.success() && function_out.status.success());
    let default_text = String::from_utf8_lossy(&default_out.stdout);
    let function_text = String::from_utf8_lossy(&function_out.stdout);
    assert!(default_text.contains("file "));
    assert!(!function_text.contains("file "));
}
