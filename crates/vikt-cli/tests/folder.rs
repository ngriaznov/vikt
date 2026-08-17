//! Folder input and `--scope repo`, through the built binary, over a
//! fixture directory mixing Python and JavaScript sources. Each language has
//! its own cross-file hub — a function with real work, called from two
//! *different* files — so `--scope repo` has something `--scope file`
//! structurally cannot see: `hub.py`'s `py_hub` is called from
//! `wrapper_a.py` and `wrapper_b.py`, `hub.js`'s `jsHub` from `wrapperA.js`
//! and `wrapperB.js`.

use std::process::Command;

const FIXTURE: &str = "tests/fixtures/folder";

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

/// Every function name across every file in the sidecar, `<module>`
/// included.
fn function_names(v: &serde_json::Value) -> Vec<String> {
    v["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The peak (highest `function_score`) span's `repo_score` for a named
/// function.
fn peak_repo_score(v: &serde_json::Value, name: &str) -> f64 {
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
    peak["repo_score"]
        .as_f64()
        .unwrap_or_else(|| panic!("{name}'s peak span has no repo_score: {peak}"))
}

/// A directory without a `Cargo.toml` walks every registry-known extension
/// it contains and lowers each through its own frontend, all into one
/// sidecar — the folder becomes a first-class multi-language input rather
/// than the cargo-only error a bare directory used to be.
#[test]
fn folder_input_scores_both_languages_in_one_sidecar() {
    let v = run(&[]);
    let names = function_names(&v);
    for expected in [
        "py_hub",
        "py_helper",
        "wrapper_a",
        "wrapper_b",
        "jsHub",
        "jsHelper",
        "wrapperA",
        "wrapperB",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "expected {expected} among the sidecar's functions: {names:?}"
        );
    }
    let generator = v["generator"].as_str().unwrap();
    assert!(
        generator.contains("vikt-ts/tree-sitter-python"),
        "generator should credit the Python frontend: {generator}"
    );
    assert!(
        generator.contains("vikt-js/oxc"),
        "generator should credit the JavaScript frontend: {generator}"
    );
}

/// The default scope is still `file`, unaffected by folder input: every
/// span in the sidecar carries `file_score`, exactly as a single-file input
/// already does.
#[test]
fn folder_input_default_scope_carries_file_score() {
    let v = run(&[]);
    let has_file_score = v["functions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["spans"].as_array().unwrap())
        .any(|s| {
            s.get("file_score")
                .and_then(serde_json::Value::as_f64)
                .is_some()
        });
    assert!(has_file_score, "default scope must carry file_score spans");
}

/// `--scope repo` ranks each language's cross-file hub above both of its
/// callers' own peaks — a call graph edge `--scope file` cannot see at all,
/// since the hub and its callers never share one file.
#[test]
fn repo_scope_ranks_the_cross_file_hub_above_its_callers() {
    let v = run(&["--scope", "repo"]);

    let py_hub = peak_repo_score(&v, "py_hub");
    for caller in ["wrapper_a", "wrapper_b"] {
        let w = peak_repo_score(&v, caller);
        assert!(
            py_hub > w,
            "py_hub's repo_score peak {py_hub} should outrank {caller}'s peak {w}"
        );
    }

    let js_hub = peak_repo_score(&v, "jsHub");
    for caller in ["wrapperA", "wrapperB"] {
        let w = peak_repo_score(&v, caller);
        assert!(
            js_hub > w,
            "jsHub's repo_score peak {js_hub} should outrank {caller}'s peak {w}"
        );
    }
}

/// `--scope repo` never touches tiers: the tier a line carries is identical
/// to the default (function-scope-equivalent-for-tiers) run, only
/// `repo_score` is added.
#[test]
fn repo_scope_leaves_tiers_untouched() {
    let function_scope = run(&["--scope", "function"]);
    let repo_scope = run(&["--scope", "repo"]);
    let tiers = |v: &serde_json::Value| -> Vec<(String, String, String)> {
        let mut out: Vec<_> = v["functions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| {
                let file = f["file"].as_str().unwrap_or_default().to_owned();
                let name = f["name"].as_str().unwrap().to_owned();
                f["spans"].as_array().unwrap().iter().map(move |s| {
                    (
                        format!("{file}:{name}"),
                        s["start"].to_string(),
                        s["tier"].as_str().unwrap().to_owned(),
                    )
                })
            })
            .collect();
        out.sort();
        out
    };
    assert_eq!(tiers(&function_scope), tiers(&repo_scope));
}

/// Under `--scope function` or `--scope file`, no span anywhere carries a
/// `repo_score` key at all — only `--scope repo` populates it.
#[test]
fn repo_score_absent_outside_repo_scope() {
    for scope in ["function", "file"] {
        let v = run(&["--scope", scope]);
        for f in v["functions"].as_array().unwrap() {
            for s in f["spans"].as_array().unwrap() {
                assert!(
                    s.get("repo_score").is_none(),
                    "--scope {scope} must omit repo_score, found it on {s}"
                );
            }
        }
    }
}
