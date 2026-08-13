//! End-to-end tests over real oxc lowering, mirroring the Python and JVM
//! frontends' fixture so all three substrates can be compared line for line.

use salience_core::{Denylist, ScoreWeights, Sidecar, analyze};

const SOURCE: &str = r#"const TOTALS = {};

function totalOrder(prices, rate, applyIt, key) {
  console.info("starting with", prices.length, "prices");
  const unused = "goes nowhere";
  let counted = 0;
  let subtotal = 0.0;
  for (const p of prices) {
    if (p === null) {
      continue;
    }
    subtotal += p;
    counted += 1;
  }
  let total = subtotal;
  if (applyIt) {
    total = subtotal * (1.0 + rate);
  }
  console.debug("counted", counted);
  TOTALS[key] = total;
  return total;
}
"#;

fn sidecar_for(source: &str, file: &str) -> Sidecar {
    let lowered = salience_js::lower_source(source, file).expect("fixture parses");
    let mut side = Sidecar::new(lowered.file.clone(), "salience-js/test");
    for ir in &lowered.functions {
        ir.validate().expect("frontend emits a well-formed graph");
        let sal = analyze(ir, &Denylist::new(), &ScoreWeights::default());
        side.push(ir, &sal);
    }
    side.finish();
    side
}

/// The same headline behavior the JVM and Python frontends show, on a third
/// substrate: an accumulator reaching a state write is core, a counter
/// reaching only a log is inert.
#[test]
fn separates_a_real_accumulator_from_a_log_only_counter() {
    let side = sidecar_for(SOURCE, "fixture.js");
    // `subtotal += p` line 12, `counted += 1` line 13.
    assert_eq!(side.tier_at(12), Some("core"), "subtotal reaches TOTALS[key]");
    assert_eq!(
        side.tier_at(13),
        Some("inert"),
        "counted only ever reaches console.debug"
    );
    // Both console lines.
    assert_eq!(side.tier_at(4), Some("inert"));
    assert_eq!(side.tier_at(19), Some("inert"));
    // The dead local.
    assert_eq!(side.tier_at(5), Some("plumbing"));
    // The `if (applyIt)` predicate.
    assert_eq!(side.tier_at(16), Some("core"));
    // The escaping write and the return.
    assert_eq!(side.tier_at(20), Some("boundary"));
    assert_eq!(side.tier_at(21), Some("boundary"));
}

/// The for-of loop must produce a real back edge: the accumulator's
/// definition is loop-carried and the analysis must say so.
#[test]
fn for_of_is_a_real_loop() {
    let lowered = salience_js::lower_source(SOURCE, "fixture.js").unwrap();
    let ir = lowered
        .functions
        .iter()
        .find(|f| f.id.name == "totalOrder")
        .expect("totalOrder lowered");
    let sal = analyze(ir, &Denylist::new(), &ScoreWeights::default());
    let looped = sal
        .nodes
        .iter()
        .flat_map(|n| n.reasons.iter().map(salience_core::Reason::describe))
        .any(|r| r.contains("loop-carried"));
    assert!(looped, "no loop-carried reason found; for-of back edge missing");
}

/// TypeScript parses through the same path; annotations are invisible.
#[test]
fn typescript_lowers_identically_modulo_types() {
    let ts = SOURCE
        .replace(
            "function totalOrder(prices, rate, applyIt, key) {",
            "function totalOrder(prices: number[], rate: number, applyIt: boolean, key: string): number {",
        )
        .replace("const TOTALS = {};", "const TOTALS: Record<string, number> = {};");
    let side = sidecar_for(&ts, "fixture.ts");
    assert_eq!(side.tier_at(12), Some("core"));
    assert_eq!(side.tier_at(13), Some("inert"));
    assert_eq!(side.tier_at(20), Some("boundary"));
}

/// Same input, same bytes out - twice.
#[test]
fn lowering_is_deterministic() {
    let a = format!("{:?}", salience_js::lower_source(SOURCE, "d.js").unwrap());
    let b = format!("{:?}", salience_js::lower_source(SOURCE, "d.js").unwrap());
    assert_eq!(a, b);
}

/// Every lowered node carries a line - the AST always knows its position.
#[test]
fn every_node_carries_a_line() {
    let lowered = salience_js::lower_source(SOURCE, "fixture.js").unwrap();
    for f in &lowered.functions {
        for n in &f.nodes {
            assert!(n.line.is_some(), "{}: node {:?} has no line", f.id.name, n.label);
        }
    }
}

/// A syntactically broken file reports instead of panicking.
#[test]
fn reports_syntax_errors() {
    let r = salience_js::lower_source("function {{{", "bad.js");
    assert!(r.is_err());
}
