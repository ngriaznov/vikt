//! End-to-end tests over real oxc lowering, mirroring the Python and JVM
//! frontends' fixture so all three substrates can be compared line for line.

use salience_core::{Denylist, Reason, ScoreWeights, Sidecar, Tier, analyze};

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
    assert_eq!(
        side.tier_at(12),
        Some("core"),
        "subtotal reaches TOTALS[key]"
    );
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
    assert!(
        looped,
        "no loop-carried reason found; for-of back edge missing"
    );
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
            assert!(
                n.line.is_some(),
                "{}: node {:?} has no line",
                f.id.name,
                n.label
            );
        }
    }
}

/// A syntactically broken file reports instead of panicking.
#[test]
fn reports_syntax_errors() {
    let r = salience_js::lower_source("function {{{", "bad.js");
    assert!(r.is_err());
}

/// lodash's `memoize`, shape-for-shape: a factory whose returned closure
/// reads two of the factory's own parameters. This is the fixture that
/// scored 0.41 against expert labels in the JS/TS transfer test - the
/// closure-capture gap named in salience-js's own module docs (see
/// eval/RESULTS-real-code.md, eval/ground-truth-js-v1.json).
const MEMOIZE_SOURCE: &str = r#"function memoize(func, resolver) {
  var memoized = function() {
    var key = resolver ? resolver(arguments) : arguments[0];
    if (cacheHas(key)) { return cacheGet(key); }
    var result = func.apply(null, arguments);
    cacheSet(key, result);
    return result;
  };
  return memoized;
}
"#;

/// The closure-bearing statement's uses must include the parameters it
/// captures. Before this fix `memoized = function() {...}` used nothing at
/// all - `func` and `resolver` were read only inside the nested function,
/// which is exactly the invisibility the transfer test measured.
#[test]
fn closure_uses_include_its_captured_params() {
    let lowered = salience_js::lower_source(MEMOIZE_SOURCE, "memoize.js").unwrap();
    let memoize = lowered
        .functions
        .iter()
        .find(|f| f.id.name == "memoize")
        .expect("memoize lowered");
    let params = memoize
        .nodes
        .iter()
        .find(|n| n.label == "<params>")
        .expect("params node");
    let closure = memoize
        .nodes
        .iter()
        .find(|n| n.label.contains("memoized"))
        .expect("closure-bearing statement");
    // `func` and `resolver` are among the params node's defs; nothing else
    // in that def set is ever read by the closure, so the intersection is
    // exactly the captured pair.
    let captured: Vec<_> = params
        .defs
        .iter()
        .filter(|d| closure.uses.contains(d))
        .collect();
    assert_eq!(
        captured.len(),
        2,
        "expected func and resolver captured; params defs {:?}, closure uses {:?}",
        params.defs,
        closure.uses
    );
}

/// End-to-end tier check: with the capture edge in place, `memoize`'s
/// parameters are no longer plumbing-dead - they reach the `return
/// memoized;` through the closure that closes over them.
#[test]
fn memoize_params_are_not_plumbing_dead() {
    let lowered = salience_js::lower_source(MEMOIZE_SOURCE, "memoize.js").unwrap();
    let memoize = lowered
        .functions
        .iter()
        .find(|f| f.id.name == "memoize")
        .expect("memoize lowered");
    let sal = analyze(memoize, &Denylist::new(), &ScoreWeights::default());
    let params_id = memoize
        .nodes
        .iter()
        .position(|n| n.label == "<params>")
        .expect("params node");
    assert_ne!(
        sal.nodes[params_id].tier,
        Tier::Plumbing,
        "params dangled instead of reaching the return through the closure; reasons: {:?}",
        sal.nodes[params_id]
            .reasons
            .iter()
            .map(Reason::describe)
            .collect::<Vec<_>>()
    );
}

/// A labelled `continue` reaching out of a nested loop to an outer labelled
/// one: `total` is only ever incremented after the `continue`, so it stays
/// loop-carried only if the labelled continue is a real back edge and not
/// the terminal node v1 lowered it as.
const LABELLED_CONTINUE_SOURCE: &str = r#"function sumSkippingNegativeRows(rows) {
  let total = 0;
outer:
  for (const row of rows) {
    for (const x of row) {
      if (x < 0) {
        continue outer;
      }
      total += x;
    }
  }
  return total;
}
"#;

#[test]
fn labelled_continue_is_a_real_back_edge() {
    let lowered = salience_js::lower_source(LABELLED_CONTINUE_SOURCE, "labelled.js").unwrap();
    let f = lowered
        .functions
        .iter()
        .find(|f| f.id.name == "sumSkippingNegativeRows")
        .expect("function lowered");
    let continue_node = f
        .nodes
        .iter()
        .find(|n| n.label == "continue")
        .expect("continue node lowered");
    assert!(
        !continue_node.succs.is_empty(),
        "labelled continue is terminal - no back edge to the outer loop"
    );

    let sal = analyze(f, &Denylist::new(), &ScoreWeights::default());
    let looped = f.nodes.iter().zip(&sal.nodes).any(|(n, ns)| {
        n.label.contains("total")
            && ns
                .reasons
                .iter()
                .map(Reason::describe)
                .any(|r| r.contains("loop-carried"))
    });
    assert!(
        looped,
        "accumulator after the labelled continue lost its loop-carried reason"
    );
}

/// Same input, same bytes out - twice, for both fixtures this change
/// touches.
#[test]
fn closures_and_labelled_continue_are_deterministic() {
    let a = format!(
        "{:?}",
        salience_js::lower_source(MEMOIZE_SOURCE, "d.js").unwrap()
    );
    let b = format!(
        "{:?}",
        salience_js::lower_source(MEMOIZE_SOURCE, "d.js").unwrap()
    );
    assert_eq!(a, b);

    let c = format!(
        "{:?}",
        salience_js::lower_source(LABELLED_CONTINUE_SOURCE, "d2.js").unwrap()
    );
    let d = format!(
        "{:?}",
        salience_js::lower_source(LABELLED_CONTINUE_SOURCE, "d2.js").unwrap()
    );
    assert_eq!(c, d);
}
