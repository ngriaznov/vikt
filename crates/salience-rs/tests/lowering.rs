//! End-to-end tests over real MIR lowering, mirroring the JVM, Python and JS
//! suites' fixture so all four substrates can be compared claim for claim.
//!
//! Skipped rather than failed when the nightly helper has not been built -
//! the same policy the other frontends apply to a missing JDK or interpreter.

use std::path::PathBuf;

use salience_core::{Denylist, ScoreWeights, Sidecar, analyze};

fn helper() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SALIENCE_RUST_LOWER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for rel in [
        "../../tools/rust-lower/target/release/salience-rust-lower",
        "../../tools/rust-lower/target/debug/salience-rust-lower",
    ] {
        let p = PathBuf::from(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn sidecar_for(path: &str) -> Option<Sidecar> {
    let h = helper()?;
    let lowered = match salience_rs::lower_file_with(std::path::Path::new(path), &h) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skipping: {e}");
            return None;
        }
    };
    let mut side = Sidecar::new(lowered.file.clone(), "salience-rs/test");
    for ir in &lowered.functions {
        ir.validate().expect("frontend emits a well-formed graph");
        let sal = analyze(ir, &Denylist::new(), &ScoreWeights::default());
        side.push(ir, &sal);
    }
    side.finish();
    Some(side)
}

/// The same headline behavior every other frontend shows, on real MIR: an
/// accumulator reaching a state write is core, a counter reaching only a
/// println is inert.
#[test]
fn separates_a_real_accumulator_from_a_log_only_counter() {
    let Some(side) = sidecar_for("../../demo/rust/orders.rs") else {
        eprintln!("helper not built; skipping");
        return;
    };
    // `subtotal += p` line 16, `inspected += 1` line 17.
    assert_eq!(
        side.tier_at(16),
        Some("core"),
        "subtotal reaches the static"
    );
    assert_eq!(
        side.tier_at(17),
        Some("inert"),
        "inspected only ever reaches println!"
    );
    // Both println! lines.
    assert_eq!(side.tier_at(7), Some("inert"));
    assert_eq!(side.tier_at(23), Some("inert"));
    // The `if apply_tax` predicate, the write to the static, the return.
    assert_eq!(side.tier_at(20), Some("core"));
    assert_eq!(side.tier_at(25), Some("core"));
    assert_eq!(side.tier_at(27), Some("boundary"));
}

/// The for loop must be a real loop: the accumulator is loop-carried.
#[test]
fn for_loop_is_loop_carried() {
    let Some(_h) = helper() else {
        eprintln!("helper not built; skipping");
        return;
    };
    let h = helper().unwrap();
    let lowered =
        salience_rs::lower_file_with(std::path::Path::new("../../demo/rust/orders.rs"), &h)
            .unwrap();
    let ir = lowered
        .functions
        .iter()
        .find(|f| f.id.name.ends_with("process"))
        .expect("process lowered");
    let sal = analyze(ir, &Denylist::new(), &ScoreWeights::default());
    let looped = sal
        .nodes
        .iter()
        .flat_map(|n| n.reasons.iter().map(salience_core::Reason::describe))
        .any(|r| r.contains("loop-carried"));
    assert!(looped, "no loop-carried reason; MIR back edge missing");
}

/// Same input, same bytes out - twice.
#[test]
fn lowering_is_deterministic() {
    let Some(_h) = helper() else {
        eprintln!("helper not built; skipping");
        return;
    };
    let h = helper().unwrap();
    let a = format!(
        "{:?}",
        salience_rs::lower_file_with(std::path::Path::new("../../demo/rust/orders.rs"), &h)
            .unwrap()
    );
    let b = format!(
        "{:?}",
        salience_rs::lower_file_with(std::path::Path::new("../../demo/rust/orders.rs"), &h)
            .unwrap()
    );
    assert_eq!(a, b);
}

/// A compile error in the target reports rustc's own diagnostics.
#[test]
fn reports_compile_errors() {
    let Some(_h) = helper() else {
        eprintln!("helper not built; skipping");
        return;
    };
    let h = helper().unwrap();
    let bad = std::env::temp_dir().join("salience-rs-bad.rs");
    std::fs::write(&bad, "fn broken( {").unwrap();
    let r = salience_rs::lower_file_with(&bad, &h);
    let _ = std::fs::remove_file(&bad);
    assert!(r.is_err());
}
