//! The SARIF projection, pinned against a hand-built sidecar so the assertions
//! are about the wire shape and nothing else: no frontend, no analysis run.

use std::path::Path;

use salience_core::Tier;
use salience_core::artifact::{Coverage, FunctionRecord, Sidecar, SpanRecord, TierCounts};
use salience_core::sarif::{SARIF_SCHEMA, SARIF_VERSION, SarifLog};

/// A two-function sidecar exercising every path the projection branches on:
/// a multi-line core span, a boundary span, an inert span that must never
/// become a result, and a second function carrying its own file (the crate
/// mode shape).
fn sample() -> Sidecar {
    let mut side = Sidecar::new("/repo/src/lib.rs", "salience-core/test");
    side.functions.push(FunctionRecord {
        file: String::new(),
        name: "process".to_owned(),
        signature: String::new(),
        decl_line: Some(1),
        coverage: Coverage {
            instructions: 4,
            with_line: 4,
        },
        summary: TierCounts {
            core: 2,
            boundary: 1,
            inert: 1,
            ..TierCounts::default()
        },
        spans: vec![
            SpanRecord {
                start: 2,
                end: 3,
                tier: "core".to_owned(),
                score: 0.87,
                rank: 1.0,
                reasons: vec!["branch predicate".to_owned()],
            },
            SpanRecord {
                start: 4,
                end: 4,
                tier: "boundary".to_owned(),
                score: 0.41,
                rank: 0.5,
                reasons: vec!["return".to_owned()],
            },
            SpanRecord {
                start: 5,
                end: 5,
                tier: "inert".to_owned(),
                score: 0.02,
                rank: 0.0,
                reasons: vec!["denylisted call".to_owned()],
            },
        ],
    });
    side.functions.push(FunctionRecord {
        file: "/repo/src/other.rs".to_owned(),
        name: "helper".to_owned(),
        signature: String::new(),
        decl_line: Some(1),
        coverage: Coverage {
            instructions: 1,
            with_line: 1,
        },
        summary: TierCounts {
            core: 1,
            ..TierCounts::default()
        },
        spans: vec![SpanRecord {
            start: 7,
            end: 7,
            tier: "core".to_owned(),
            score: 0.90,
            rank: 1.0,
            reasons: vec!["loop-carried".to_owned()],
        }],
    });
    side
}

/// The envelope is fixed: schema key, version, one run, driver identity, and
/// one rule per requested tier under the `salience/<tier>` id scheme.
#[test]
fn envelope_and_rules() {
    let log = SarifLog::from_sidecar(&sample(), &[Tier::Core, Tier::Boundary], None);
    let v = serde_json::to_value(&log).unwrap();

    assert_eq!(v["$schema"], SARIF_SCHEMA);
    assert_eq!(v["version"], SARIF_VERSION);
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(v["runs"].as_array().unwrap().len(), 1);

    let driver = &v["runs"][0]["tool"]["driver"];
    assert_eq!(driver["name"], "salience");
    assert_eq!(driver["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        driver["informationUri"]
            .as_str()
            .unwrap()
            .starts_with("https://")
    );
    let ids: Vec<&str> = driver["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["salience/core", "salience/boundary"]);
}

/// A span becomes one result per line it covers, each carrying the tier's
/// rule id, `note` level, the score in the message, and the raw numbers in
/// the property bag. Inert spans never surface.
#[test]
fn one_result_per_reported_line() {
    let log = SarifLog::from_sidecar(
        &sample(),
        &[Tier::Core, Tier::Boundary],
        Some(Path::new("/repo")),
    );
    let v = serde_json::to_value(&log).unwrap();
    let results = v["runs"][0]["results"].as_array().unwrap();

    // Lines 2, 3 (core span), 4 (boundary), 7 (second function). The inert
    // line 5 is filtered out.
    assert_eq!(results.len(), 4);

    let first = &results[0];
    assert_eq!(first["ruleId"], "salience/core");
    assert_eq!(first["level"], "note");
    assert_eq!(first["message"]["text"], "core line (panel 0.87)");
    let loc = &first["locations"][0]["physicalLocation"];
    assert_eq!(loc["artifactLocation"]["uri"], "src/lib.rs");
    assert_eq!(loc["region"]["startLine"], 2);
    assert_eq!(first["properties"]["score"], 0.87);
    assert_eq!(first["properties"]["tier"], "core");

    assert_eq!(
        results[1]["locations"][0]["physicalLocation"]["region"]["startLine"],
        3
    );
    assert_eq!(results[2]["ruleId"], "salience/boundary");

    // The second function's own file wins over the sidecar-level file, and is
    // relativized against the same root.
    let last = &results[3];
    assert_eq!(
        last["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "src/other.rs"
    );
    assert_eq!(
        last["locations"][0]["physicalLocation"]["region"]["startLine"],
        7
    );
}

/// The default request — core alone — yields one rule and no boundary
/// results, and a path outside the root passes through untouched.
#[test]
fn core_only_and_foreign_root() {
    let log = SarifLog::from_sidecar(&sample(), &[Tier::Core], Some(Path::new("/elsewhere")));
    let v = serde_json::to_value(&log).unwrap();

    let rules = v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"], "salience/core");

    let results = v["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r["ruleId"] == "salience/core"));
    assert_eq!(
        results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "/repo/src/lib.rs"
    );
}

/// Rule order and result order do not depend on the order the caller listed
/// tiers in, so two invocations differing only in argument order emit
/// byte-identical SARIF.
#[test]
fn tier_order_is_canonical() {
    let side = sample();
    let a = SarifLog::from_sidecar(&side, &[Tier::Core, Tier::Boundary], None);
    let b = SarifLog::from_sidecar(&side, &[Tier::Boundary, Tier::Core], None);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

/// Paths that are not URI-safe verbatim — spaces, non-ASCII — come out
/// percent-encoded with `/` untouched: SARIF requires a valid RFC 3986 URI
/// reference, and code-scanning uploads reject a raw space in the uri.
#[test]
fn uris_are_percent_encoded() {
    let mut side = sample();
    side.file = "/repo/space dir/naïve file.rs".to_owned();
    side.functions.truncate(1);
    let log = SarifLog::from_sidecar(&side, &[Tier::Core], Some(Path::new("/repo")));
    let v = serde_json::to_value(&log).unwrap();
    assert_eq!(
        v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "space%20dir/na%C3%AFve%20file.rs"
    );
}
