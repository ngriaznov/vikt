//! End-to-end tests over real bytecode, compiled by `javac` at test time.
//!
//! Skipped rather than failed when no JDK is present, so the suite stays green
//! on machines that only build Rust.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use vikt_core::{Denylist, ScoreWeights, Sidecar, analyze};

/// Compiles `source` with debug info and returns the directory holding the
/// resulting classes, or `None` when `javac` is unavailable.
fn compile(name: &str, source: &str) -> Option<PathBuf> {
    // Tests run in parallel in one process, so the pid alone does not make the
    // directory unique. A counter is what keeps one test from deleting the
    // classes another is still reading.
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("vikt-jvm-test-{name}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let java = dir.join(format!("{name}.java"));
    std::fs::write(&java, source).ok()?;

    // `-g` is what emits the LineNumberTable; without it the analysis still
    // runs but projects onto nothing, which is precisely what one test checks.
    let out = Command::new("javac").arg("-g").arg(&java).output().ok()?;
    if !out.status.success() {
        eprintln!("javac failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(dir)
}

fn sidecar_for(dir: &Path, class: &str) -> Sidecar {
    let bytes = std::fs::read(dir.join(format!("{class}.class"))).expect("class file exists");
    let lowered = vikt_jvm::lower_class(&bytes).expect("class parses");
    let mut side = Sidecar::new(
        lowered.source_file.clone().unwrap_or_default(),
        "vikt-jvm/test",
    );
    for ir in &lowered.functions {
        ir.validate().expect("frontend emits a well-formed graph");
        let sal = analyze(ir, &Denylist::new(), &ScoreWeights::default());
        side.push(ir, &sal);
    }
    side.finish();
    side
}

const SOURCE: &str = r#"
import java.util.List;
import java.util.logging.Logger;

public class Sample {
    private static final Logger LOG = Logger.getLogger("sample");
    private double stored;

    public double process(List<Double> prices, double rate, boolean apply) {
        LOG.info("starting with " + prices.size() + " prices");
        String unused = "goes nowhere";
        int counted = 0;
        double subtotal = 0.0;
        for (Double p : prices) {
            if (p == null) {
                continue;
            }
            subtotal += p;
            counted++;
        }
        double total = subtotal;
        if (apply) {
            total = subtotal * (1.0 + rate);
        }
        LOG.fine("counted " + counted);
        this.stored = total;
        return total;
    }
}
"#;

/// The headline behavior, on real bytecode: an accumulator that reaches a state
/// write is core, while a counter that only ever reaches a log call is inert —
/// even though the two are syntactically identical and both loop-carried.
#[test]
fn separates_a_real_accumulator_from_a_log_only_counter() {
    let Some(dir) = compile("Sample", SOURCE) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let side = sidecar_for(&dir, "Sample");

    // The two loop bodies: `subtotal += p` and `counted++`, adjacent lines,
    // syntactically identical, both loop-carried — and correctly separated.
    assert_eq!(
        side.tier_at(18),
        Some("core"),
        "subtotal reaches the field write"
    );
    assert_eq!(
        side.tier_at(19),
        Some("inert"),
        "counted only ever reaches a log call"
    );
    // Both log lines, including the `prices.size()` call built as an argument
    // for one of them.
    assert_eq!(side.tier_at(10), Some("inert"));
    assert_eq!(side.tier_at(25), Some("inert"));
    // The dead local.
    assert_eq!(side.tier_at(11), Some("plumbing"));
    // The `if (apply)` predicate.
    assert_eq!(side.tier_at(22), Some("core"));
    // The field write and the return: the frontier.
    assert_eq!(side.tier_at(26), Some("boundary"));
    assert_eq!(side.tier_at(27), Some("boundary"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every instruction in a `-g` compiled body must carry a source line. If this
/// regresses, the line table is being read wrongly and every span is suspect.
#[test]
fn debug_compiled_bytecode_has_full_line_coverage() {
    let Some(dir) = compile("Sample", SOURCE) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let side = sidecar_for(&dir, "Sample");
    for f in &side.functions {
        assert_eq!(
            f.coverage.instructions, f.coverage.with_line,
            "{} lost line attribution",
            f.name
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two runs over the same bytes must produce identical JSON, or the artifact
/// cannot be cached or diffed.
#[test]
fn lowering_is_deterministic() {
    let Some(dir) = compile("Sample", SOURCE) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let a = serde_json::to_string(&sidecar_for(&dir, "Sample")).unwrap();
    let b = serde_json::to_string(&sidecar_for(&dir, "Sample")).unwrap();
    assert_eq!(a, b);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Garbage in must not panic.
#[test]
fn rejects_non_class_bytes() {
    assert!(vikt_jvm::lower_class(b"not a class file at all").is_err());
    assert!(vikt_jvm::lower_class(&[]).is_err());
}

// --- Kotlin -------------------------------------------------------------
// These run only where a `kotlinc` is on PATH. The SMAP resolution they cover
// is also pinned hermetically by the unit tests in `src/smap.rs`, which use the
// exact attribute text a real `kotlinc 2.1.20` emitted for this fixture — so
// losing the compiler loses coverage of the *plumbing*, not of the mapping.

/// Exercises the constructs whose bytecode line attribution is in question:
/// an inline function with a lambda, a stdlib inline call (`map`), a suspend
/// function, `when` over an enum, default arguments, and a data class.
const KOTLIN_SOURCE: &str = r#"package demo

val LOG: java.util.logging.Logger = java.util.logging.Logger.getLogger("t")

inline fun <T> timed(label: String, block: () -> T): T {
    val start = System.nanoTime()
    val result = block()
    LOG.fine("$label took ${System.nanoTime() - start}ns")
    return result
}

data class Order(val id: String, val amount: Double)

enum class Kind { RETAIL, WHOLESALE }

class Processor {
    private var runningTotal: Double = 0.0

    fun rateFor(kind: Kind): Double = when (kind) {
        Kind.RETAIL -> 0.20
        Kind.WHOLESALE -> 0.10
    }

    fun withDefaults(base: Double, markup: Double = 1.5): Double = base * markup

    fun usesInline(prices: List<Double>): Double = timed("sum") {
        var acc = 0.0
        for (p in prices) {
            acc += p
        }
        acc
    }

    fun mapped(prices: List<Double>): List<Double> = prices.map { it * 2.0 }

    suspend fun suspending(xs: List<Double>): Double {
        var acc = 0.0
        for (x in xs) {
            kotlinx.coroutines.yield()
            acc += x
        }
        return acc
    }

    fun plain(prices: List<Double>): Double {
        var subtotal = 0.0
        for (p in prices) {
            if (p < 0) continue
            subtotal += p
        }
        runningTotal = subtotal
        return subtotal
    }
}
"#;

/// Compiles Kotlin, returning the output directory, or `None` when no compiler
/// is available.
fn compile_kotlin(source: &str) -> Option<PathBuf> {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vikt-kt-test-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("Fixture.kt");
    std::fs::write(&src, source).ok()?;

    let out = Command::new("kotlinc")
        .arg(&src)
        .arg("-d")
        .arg(dir.join("out"))
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("kotlinc failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(dir)
}

/// The regression this whole `smap` module exists for.
///
/// A Kotlin class containing inline call sites has `LineNumberTable` entries
/// well past the end of its source file — they index a synthetic composite file
/// that only the SMAP describes. Measured on `kotlinc 2.1.20`, an 80-line
/// fixture produced entries for lines 82 through 89, four of which belonged to
/// `kotlin/collections/_Collections.kt` rather than to the fixture at all.
///
/// After resolution every span must fall inside the real file.
#[test]
fn kotlin_inline_call_sites_do_not_produce_lines_past_end_of_file() {
    let Some(dir) = compile_kotlin(KOTLIN_SOURCE) else {
        eprintln!("skipping: kotlinc unavailable");
        return;
    };
    let real_lines = u32::try_from(KOTLIN_SOURCE.lines().count()).unwrap();

    let classes = dir.join("out").join("demo");
    let mut checked = 0;
    for entry in std::fs::read_dir(&classes).expect("kotlinc produced classes") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("class") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("class file readable");
        let lowered = vikt_jvm::lower_class(&bytes).expect("class parses");
        let mut side = Sidecar::new("Fixture.kt", "vikt-jvm/test");
        for ir in &lowered.functions {
            ir.validate().expect("well-formed graph");
            let sal = analyze(ir, &Denylist::new(), &ScoreWeights::default());
            side.push(ir, &sal);
        }
        side.finish();

        for f in &side.functions {
            for s in &f.spans {
                assert!(
                    s.end <= real_lines,
                    "{}: {} span {}-{} exceeds the {}-line source file — SMAP \
resolution failed",
                    path.display(),
                    f.name,
                    s.start,
                    s.end,
                    real_lines
                );
            }
        }
        checked += 1;
    }
    assert!(checked > 0, "expected at least one class");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The class holding the inline call sites must actually carry an SMAP, or the
/// test above would pass vacuously.
#[test]
fn kotlin_inlining_class_carries_a_source_map() {
    let Some(dir) = compile_kotlin(KOTLIN_SOURCE) else {
        eprintln!("skipping: kotlinc unavailable");
        return;
    };
    let bytes = std::fs::read(dir.join("out").join("demo").join("Processor.class"))
        .expect("Processor.class exists");
    let lowered = vikt_jvm::lower_class(&bytes).expect("class parses");
    assert_eq!(
        lowered.smap_stratum.as_deref(),
        Some("KotlinDebug"),
        "expected the call-site stratum to be the one used"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `suspend fun` is compiled into a state machine whose dispatch jumps
/// straight into the middle of the body. That makes the graph irreducible: the
/// loop header stops dominating the loop tail, and a textbook natural-loop
/// detector finds nothing.
///
/// Measured before the fix, on `kotlinc 2.1.20`: `fetchAndTotal`, a `suspend
/// fun` whose `for` loop contains a `delay(1)`, reported `natural_loops=0`
/// against `retreating_edges=1` — the back edge was there, but it was not a
/// dominator back edge. Every non-suspend method in the same class found its
/// loop correctly.
#[test]
fn a_loop_in_a_suspend_function_is_still_found() {
    let Some(dir) = compile_kotlin(KOTLIN_SOURCE) else {
        eprintln!("skipping: kotlinc unavailable");
        return;
    };
    let bytes = std::fs::read(dir.join("out").join("demo").join("Processor.class"))
        .expect("Processor.class exists");
    let lowered = vikt_jvm::lower_class(&bytes).expect("class parses");

    assert!(
        lowered.state_machines_excised >= 1,
        "expected at least one coroutine state machine to be recognised"
    );

    let suspending = lowered
        .functions
        .iter()
        .find(|f| f.id.name.ends_with("::suspending"))
        .expect("the suspend function was lowered");
    let graph = vikt_core::Graph::build(suspending);
    assert_eq!(
        graph.loops.len(),
        1,
        "the source-level `for` loop must survive the state machine; \
without excision this is 0"
    );

    // The control case: the same loop shape without `suspend`.
    let plain = lowered
        .functions
        .iter()
        .find(|f| f.id.name.ends_with("::plain"))
        .expect("the plain function was lowered");
    assert_eq!(vikt_core::Graph::build(plain).loops.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Excision must not cost the map any source line. The resume-restore blocks it
/// makes unreachable re-execute lines the normal path also covers, so removing
/// them should change tiers, never coverage of the file.
#[test]
fn excision_does_not_drop_any_source_line() {
    let Some(dir) = compile_kotlin(KOTLIN_SOURCE) else {
        eprintln!("skipping: kotlinc unavailable");
        return;
    };
    let bytes = std::fs::read(dir.join("out").join("demo").join("Processor.class"))
        .expect("Processor.class exists");
    let lowered = vikt_jvm::lower_class(&bytes).expect("class parses");

    let suspending = lowered
        .functions
        .iter()
        .find(|f| f.id.name.ends_with("::suspending"))
        .expect("the suspend function was lowered");

    // The body spans the `for` and the `return`; every one of those lines must
    // still appear somewhere in the map.
    let sal = analyze(suspending, &Denylist::new(), &ScoreWeights::default());
    let mut side = Sidecar::new("Fixture.kt", "vikt-jvm/test");
    side.push(suspending, &sal);
    side.finish();
    let covered: Vec<u32> = side.functions[0]
        .spans
        .iter()
        .flat_map(|s| s.start..=s.end)
        .collect();
    assert!(
        covered.len() >= 4,
        "expected the suspend body to still be mapped, got {covered:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
