//! The SARIF format through the built binary, over the checked-in Python
//! demo — the one frontend whose toolchain (python3) every test environment
//! is expected to carry. Skips rather than fails when it is missing.

use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const DEMO: &str = "../../demo/python/orders.py";

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `--format sarif` emits a parseable SARIF 2.1.0 log whose default tier
/// filter (core alone) still finds core lines in the demo.
#[test]
fn sarif_over_python_demo() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(DEMO)
        .args(["--format", "sarif"])
        .output()
        .expect("running the vikt binary");
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout parses as JSON");
    assert_eq!(
        v["$schema"],
        "https://json.schemastore.org/sarif-2.1.0.json"
    );
    assert_eq!(v["version"], "2.1.0");

    let results = v["runs"][0]["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|r| r["ruleId"] == "vikt/core"),
        "the demo has an accumulator loop; core lines must surface"
    );
    // The default filter is core-only; boundary results need --sarif-tiers.
    assert!(results.iter().all(|r| r["ruleId"] == "vikt/core"));
    let first = &results[0];
    assert_eq!(first["level"], "note");
    assert_eq!(
        first["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        DEMO
    );
    assert!(first["properties"]["score"].is_f64() || first["properties"]["score"].is_number());
}

/// `--sarif-tiers core,boundary` widens the filter: both rules are declared
/// and boundary lines (the demo writes a module-level dict and returns) join
/// the results.
#[test]
fn sarif_tiers_widen_to_boundary() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(DEMO)
        .args(["--format", "sarif", "--sarif-tiers", "core,boundary"])
        .output()
        .expect("running the vikt binary");
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout parses as JSON");
    let ids: Vec<&str> = v["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .expect("rules array")
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["vikt/core", "vikt/boundary"]);
    let results = v["runs"][0]["results"].as_array().expect("results array");
    assert!(results.iter().any(|r| r["ruleId"] == "vikt/boundary"));
}

/// A bare invocation is not SARIF: the default output is the sidecar
/// artifact, unchanged by the new format's existence.
#[test]
fn default_output_is_still_the_sidecar() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .arg(DEMO)
        .output()
        .expect("running the vikt binary");
    assert!(
        out.status.success(),
        "vikt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout parses as JSON");
    assert_eq!(v["schema"], "vikt-sidecar/v3");
    assert!(v.get("$schema").is_none());
}
