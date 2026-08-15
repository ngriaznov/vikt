//! `vikt calibrate` — per-repo self-calibration by mutation testing.
//!
//! The published bakeoff numbers say how the panel performs on the corpora it
//! was fitted against. They say nothing about *this* repository. Calibration
//! answers that locally and without judgement in the loop: mutate lines the
//! panel scored, let the repository's own test suite decide which mutants die,
//! and check whether the panel's ordering agrees with the kill rates.
//!
//! Everything destructive happens in a temporary copy of the tree; the input
//! tree is never opened for writing. The verdict is judged against the same
//! positional null the scorer bakeoffs use — score a line by how early it
//! sits in its function — computed on the same lines, because a panel that
//! cannot beat "earlier is more important" on this repository has nothing to
//! offer it.
//!
//! Python, JavaScript/TypeScript, and Rust cargo packages: mutation needs a
//! language-specific rewrite engine and a test-command convention — see
//! [`Language`] for how a tree picks one. Rust runs a build step before the
//! suite for every mutant, because its engine splices text and a splice can
//! propose an edit the language rejects: a mutant that does not compile is
//! *invalid*, discarded from the kill rate entirely, and each mutant costs a
//! compile, which the run says up front.
//! TypeScript carries one extra caveat: a mutant that is syntactically valid
//! JavaScript but violates TypeScript's type system is read as *killed* by
//! whatever runs the repository's own type check, indistinguishable from a
//! mutant a test actually caught. `run` prints a one-line notice when any
//! scored file is `.ts`/`.mts`/`.cts`/`.tsx`.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::ValueEnum;
use vikt_core::calibration::{
    MIN_EXECUTED_MUTANTS, MIN_SCORED_LINES, NULL_MARGIN, RHO_FLOOR, Verdict, spearman, verdict,
};
use vikt_core::mutant::MutantSet;
use vikt_core::{
    Denylist, FunctionFeatures, FunctionImportance, FunctionIr, PanelProfile, ScopedFunction,
    ScoreWeights, Scorer, analyze, analyze_with_scorer, project_to_lines,
};

use crate::lowering::{self, Lowering};

/// Body-size ceiling, matching the analyze path's `--max-instructions`
/// default: anything larger has only ever been generated data tables, where
/// per-line importance is meaningless and mutants are wasted.
const MAX_INSTRUCTIONS: usize = 4096;

/// Files larger than this are not copied: at this size a file is a data
/// artifact, not source, and a tree that keeps models or dumps next to its
/// code should not make calibration copy them.
const MAX_COPY_BYTES: u64 = 8 * 1024 * 1024;

/// Directory names never copied, beyond everything dot-prefixed: build
/// output and vendored trees, where mutants would measure someone else's
/// code against this repository's tests. `node_modules` is handled
/// separately (see [`TreeScan::node_modules`]): it must exist in the copy
/// for a JavaScript suite to run, but never as scored or mutated source.
const SKIP_DIRS: &[&str] = &["__pycache__", "target", "venv"];

/// Extensions Python sources carry.
const PY_EXT: &[&str] = &["py"];

/// Extensions JavaScript/TypeScript sources carry.
const JS_EXT: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];

/// The JS_EXT subset that is TypeScript rather than plain JavaScript — the
/// subset the type-check caveat in the module docs applies to.
const TS_EXT: &[&str] = &["ts", "mts", "cts", "tsx"];

/// Extensions Rust sources carry. A tree qualifies as Rust by its
/// `Cargo.toml`, not by these — mutants need a test suite, and a bare `.rs`
/// file has no way to run one — but the scan still collects them so the
/// error for a package-less Rust tree can say what it saw.
const RS_EXT: &[&str] = &["rs"];

/// Extensions that mark a tree as belonging to a frontend calibrate does not
/// support at all, for an honest "not supported" error instead of a puzzling
/// "no sources found".
const OTHER_FRONTENDS: &[&str] = &["class", "java", "kt"];

/// Which lowering-and-mutation engine handles a tree, chosen once in `run`
/// from what [`scan_tree`] found and threaded through every stage that needs
/// to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Python,
    JavaScript,
    Rust,
}

impl Language {
    /// How the language reads in a sentence, for progress and error text.
    fn label(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::JavaScript => "JavaScript/TypeScript",
            Self::Rust => "Rust",
        }
    }

    /// The dataset's `language` value: one lowercase word, stable across
    /// releases because refit tooling keys on it.
    fn slug(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
        }
    }

    /// Which panel weight vector fits the frontend's dependence-graph
    /// granularity — instruction-level for bytecode, statement-level for
    /// the oxc-lowered AST. Mirrors `profile_for_ext` in `main.rs`.
    fn panel_profile(self) -> PanelProfile {
        match self {
            Self::Python | Self::Rust => PanelProfile::Instruction,
            Self::JavaScript => PanelProfile::Statement,
        }
    }

    /// The synthetic function name(s) that overlap the extent of a real
    /// `def`/function and would double-count lines if sampled. `<module>`
    /// is checked first and unconditionally: every frontend that has one
    /// (Python's bytecode compiler, `vikt_js::lower_source`, and this
    /// crate's own tree-sitter fallback for Python/Rust - see
    /// `vikt_ts::LoweredModule`) uses exactly that name for the synthetic
    /// top-level wrapper, regardless of which lowering actually produced
    /// it. Beyond that: Python's bytecode compiler also synthesizes a code
    /// object — `<lambda>`, `<listcomp>` and friends — for constructs that
    /// are not their own top-level definition; excluding anything with `<`
    /// in its name catches the rest at once. Every other JS function —
    /// named or anonymous — is a real function whose lines are its own,
    /// most often an arrow function assigned to a name; excluding it the
    /// way Python excludes lambdas would silently drop most JS code from
    /// calibration.
    fn is_synthetic(self, name: &str) -> bool {
        if name == "<module>" {
            return true;
        }
        match self {
            Self::Python => name.contains('<'),
            Self::JavaScript => false,
            // MIR bodies for closures, generators and inline consts carry
            // brace-qualified names (`f::{closure#0}`) and overlap their
            // parent function's extent the way Python lambdas do.
            Self::Rust => name.contains('{'),
        }
    }
}

/// How wide a lens the panel score and positional null use when paired
/// against kill rates. Mirrors `main.rs`'s `Scope` for the analyze path, but
/// is its own type: this crate has no dependency from `calibrate` back onto
/// `main`, and the two enums' variants happen to coincide rather than share
/// an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
enum Scope {
    /// Panel score and positional null are both local to each sampled
    /// function's own extent — today's behavior.
    #[default]
    Function,
    /// Panel score is [`vikt_core::file_scores`]'s call-graph-weighted blend
    /// across every scored function in the mutated line's file, and the
    /// positional null is measured over that file's whole scored-line
    /// extent instead of the one owning function's. Verdict machinery
    /// (pooled Spearman, thresholds) is unchanged either way.
    File,
}

#[derive(Debug, clap::Args)]
pub struct CalibrateArgs {
    /// Directory of Python and/or JavaScript/TypeScript sources. A tree with
    /// files for another frontend entirely is rejected with an error. A tree
    /// with both Python and JavaScript/TypeScript sources is calibrated in
    /// whichever language scored more lines; the run says which, and why.
    ///
    /// TypeScript caveat: a mutant that is syntactically valid JavaScript
    /// but fails TypeScript's type check is read as *killed* by whatever
    /// runs the repository's own type-checking step — indistinguishable
    /// from a mutant an actual test caught. `--test-cmd` for a TypeScript
    /// tree should therefore isolate test failures from build/type-check
    /// failures if the two matter to distinguish; calibrate itself only
    /// ever sees the command's exit code.
    pub path: PathBuf,

    /// Command that runs the project's tests, executed with `sh -c` (`cmd
    /// /C` on Windows) from the root of a temporary copy of the tree. Must pass on the unmutated
    /// tree. For Python, typically a `python3 -m unittest`/`pytest`
    /// invocation; for JavaScript/TypeScript, typically `node --test` or
    /// `npm test`.
    #[arg(long, value_name = "COMMAND")]
    pub test_cmd: String,

    /// Functions to sample, largest first (line count, then name — no
    /// randomness, so two runs sample identically).
    #[arg(long, value_name = "N", default_value_t = 12)]
    pub sample: usize,

    /// Total mutant budget across all sampled functions. Hitting it is
    /// reported, never silent. Defaults to 150, except 60 for Rust, where
    /// every mutant costs a compile.
    #[arg(long, value_name = "M")]
    pub budget: Option<usize>,

    /// Timeout per test run, in seconds. A run that exceeds it is killed and
    /// the mutant counts as killed: a hung suite noticed the mutation.
    #[arg(long, value_name = "S", default_value_t = 60)]
    pub timeout_secs: u64,

    /// Gate on the verdict: exit 0 when calibrated or marginal, 2 on
    /// insufficient data, 3 when uncalibrated. Without this flag the exit
    /// code only reports whether the measurement itself ran.
    #[arg(long)]
    pub gate: bool,

    /// Build command run before the test command for every Rust mutant,
    /// from the copy root. Non-zero exit marks the mutant invalid — a
    /// textual splice the language rejected — which is discarded, not
    /// killed. Unused outside Rust.
    #[arg(long, value_name = "COMMAND", default_value = "cargo test --no-run")]
    pub build_cmd: String,

    /// Write one JSON line per mutated line that carries a panel score:
    /// the seven per-line panel features (measurement/audit surface of
    /// `vikt-core::panel::line_features`), the panel score, and the
    /// observed kill counts — the raw material for refitting the panel
    /// weights offline against behaviour. Overwritten, sorted by
    /// (file, function, line).
    #[arg(long, value_name = "PATH")]
    pub emit_dataset: Option<PathBuf>,

    /// Interpreter used to lower Python sources and generate Python mutants
    /// — and, typically, referenced by the test command itself. Unused when
    /// the tree calibrates as JavaScript/TypeScript: that engine runs
    /// in-process against the workspace's own oxc parser, no interpreter
    /// involved.
    #[arg(long, default_value = "python3")]
    pub python: String,

    /// Measure the panel against `vikt_core::filescope`'s
    /// call-graph-weighted blend across each line's file (`file`, the
    /// default — matching what a default `vikt` invocation scores) or
    /// against each sampled line's own function alone (`function`).
    /// `--emit-dataset` rows always carry both the file-scope score and its
    /// function features regardless of this flag; only the reported
    /// correlations and verdict change.
    #[arg(long, value_enum, default_value_t = Scope::File)]
    scope: Scope,

    /// Which lowering scores Python or Rust sources: `auto` (default) uses
    /// the primary (CPython bytecode / MIR) frontend where available and
    /// falls back to the tree-sitter engine otherwise, printing which one
    /// ran; `primary` requires it; `ast` forces tree-sitter. Unaffected for
    /// JavaScript/TypeScript, which only ever calibrates through `vikt-js`'s
    /// in-process oxc engine. Scoring is the only thing this changes:
    /// mutant *generation* for a Python tree still needs `python3` on PATH
    /// regardless of this flag, since `vikt_py::calibrate` mutates through
    /// the interpreter's own `ast` module, not through the panel's IR.
    #[arg(long, value_enum, default_value_t = Lowering::Auto)]
    lowering: Lowering,
}

/// A tree's scoring outcome: every scored function, the highest per-line
/// score per file, and which lowering produced them both (generator string
/// plus the panel profile that fits its graph granularity).
type ScoreOutcome = (Vec<ScoredFn>, FileScores, String, PanelProfile);

/// A function the panel scored, keyed to the copied tree.
struct ScoredFn {
    /// Path relative to the tree root.
    file: PathBuf,
    name: String,
    /// Extent in source lines, from the first to the last scored span.
    lo: u32,
    hi: u32,
    /// Scored lines in the extent.
    lines: usize,
    /// Per-line panel feature vectors, for `--emit-dataset` only. Computed
    /// from the plain `analyze` pass, never from the panel-overwritten
    /// scores: `line_features` documents that contract.
    feats: BTreeMap<u32, [f64; 7]>,
    /// Owned lowered body and plain (non-panel) tiered analysis, retained
    /// only so `filescope_layer` can build a [`ScopedFunction`] over every
    /// scored function of a file at once, after scoring has otherwise
    /// finished with them.
    ir: FunctionIr,
    importance: FunctionImportance,
    /// Panel score per scored line — the same values folded into
    /// `file_scores`' per-file max, kept per-function here for
    /// [`ScopedFunction::line_scores`].
    line_scores: BTreeMap<u32, f64>,
}

/// Highest panel score per line, per file — the same "most salient span
/// wins" reading the sidecar's `tier_at` uses.
type FileScores = BTreeMap<PathBuf, BTreeMap<u32, f64>>;

/// A mutant tied to the (relative) file it rewrites.
type FileMutant = (PathBuf, vikt_core::mutant::Mutant);

/// One test-suite run over a mutant.
#[derive(Clone, Copy)]
enum TestOutcome {
    Pass,
    Fail,
    Timeout,
}

#[allow(clippy::too_many_lines)] // one stage per pipeline step; splitting hides the shape
pub fn run(args: &CalibrateArgs) -> Result<ExitCode, Box<dyn Error>> {
    if !args.path.is_dir() {
        return Err(format!(
            "calibrate takes a directory of sources, and {} is not a directory",
            args.path.display()
        )
        .into());
    }

    let scan = scan_tree(&args.path)?;
    let rust = args.path.join("Cargo.toml").is_file();
    if !rust && !scan.rs.is_empty() {
        return Err(format!(
            "Rust calibration targets a cargo package, and {} has .rs sources but no Cargo.toml — point --path at the package root",
            args.path.display()
        )
        .into());
    }
    if !rust && scan.py.is_empty() && scan.js.is_empty() {
        return Err(if scan.other_frontend {
            format!(
                "calibration supports Python and JavaScript/TypeScript sources only, and {} contains neither",
                args.path.display()
            )
        } else {
            format!(
                "no Python or JavaScript/TypeScript sources found under {}",
                args.path.display()
            )
        }
        .into());
    }

    // Everything from here on happens in the copy. The input tree is never
    // opened for writing; the integration tests hold this to byte-identity.
    let copy = TempTree::create(&args.path, &scan.files, &scan.node_modules)?;
    println!(
        "calibrate: copied {} files to a temporary tree{}{}",
        scan.files.len(),
        if scan.skipped_large > 0 {
            format!(" ({} files over 8 MiB skipped)", scan.skipped_large)
        } else {
            String::new()
        },
        if scan.node_modules.is_empty() {
            String::new()
        } else {
            format!(
                ", {} node_modules director{} linked in unscored",
                scan.node_modules.len(),
                if scan.node_modules.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        }
    );

    let budget = args.budget.unwrap_or(if rust { 60 } else { 150 });
    let timeout = Duration::from_secs(args.timeout_secs);
    if rust {
        println!(
            "calibrate: Rust mutants compile before they run (`{}` per mutant) — minutes, not seconds",
            args.build_cmd
        );
        match run_test(&args.build_cmd, &copy.root, timeout)? {
            TestOutcome::Pass => {}
            TestOutcome::Fail => {
                return Err(format!(
                    "the build command fails on the unmutated tree; fix `{}` (run from the tree root) before calibrating",
                    args.build_cmd
                )
                .into());
            }
            TestOutcome::Timeout => {
                return Err(format!(
                    "the build command exceeded {}s on the unmutated tree; raise --timeout-secs",
                    args.timeout_secs
                )
                .into());
            }
        }
    }
    match run_test(&args.test_cmd, &copy.root, timeout)? {
        TestOutcome::Pass => println!("calibrate: baseline test command passed"),
        TestOutcome::Fail => {
            return Err(format!(
                "the test command fails on the unmutated tree; fix `{}` (run from the tree root) before calibrating",
                args.test_cmd
            )
            .into());
        }
        TestOutcome::Timeout => {
            return Err(format!(
                "the test command exceeded {}s on the unmutated tree; raise --timeout-secs",
                args.timeout_secs
            )
            .into());
        }
    }

    let (lang, (mut scored, file_scores, generator, profile)) = if rust {
        (Language::Rust, score_crate(&copy.root, args.lowering)?)
    } else {
        score_by_majority(&copy.root, &scan, &args.python, args.lowering)
    };
    println!(
        "calibrate: scored via {generator} ({} profile)",
        match profile {
            PanelProfile::Instruction => "instruction",
            PanelProfile::Statement => "statement",
        }
    );

    if lang == Language::JavaScript && scored.iter().any(|f| is_typescript(&f.file)) {
        println!(
            "calibrate: TypeScript sources among the scored files — a mutant that fails the \
repository's own type check is read as killed, indistinguishable from one an actual test caught"
        );
    }

    // Largest first: big bodies carry the most rankable lines per test run.
    // The full key is deterministic, so two runs sample the same functions.
    scored.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });
    let sampled = &scored[..scored.len().min(args.sample)];
    println!(
        "calibrate: {} functions scored, {} sampled (largest first)",
        scored.len(),
        sampled.len()
    );

    // Only built when something will read it: `--scope file` needs it for
    // the verdict, `--emit-dataset` needs it for every row regardless of
    // scope. Computed over every scored function, not just the sample — an
    // unsampled sibling still shapes its file's call graph.
    let filescope = if args.scope == Scope::File || args.emit_dataset.is_some() {
        filescope_layer(&scored)
    } else {
        Filescope::default()
    };
    if args.scope == Scope::File {
        println!(
            "calibrate: scope file — panel score and positional null measured against the file's call-graph-weighted blend, not each function alone"
        );
    }

    let (mutants, candidates, invalid_discarded) =
        generate_mutants(&copy.root, sampled, lang, args, budget)?;
    if invalid_discarded > 0 {
        println!(
            "calibrate: {invalid_discarded} invalid mutants discarded (failed to re-parse after mutation)"
        );
    }
    let attempted = mutants.len() + invalid_discarded;
    if candidates > attempted {
        println!(
            "calibrate: budget of {budget} mutants reached: {} of {candidates} candidate sites will run; raise --budget for full coverage",
            mutants.len(),
        );
    } else {
        println!(
            "calibrate: {} mutants across the sampled functions (budget {budget})",
            mutants.len(),
        );
    }

    let tally = execute_mutants(&copy.root, &mutants, args, lang, timeout)?;
    println!(
        "calibrate: {} mutants executed: {} killed, {} survived, {} timed out (timeouts count as killed)",
        tally.executed(),
        tally.killed,
        tally.survived,
        tally.timeouts
    );
    if tally.invalid_compile > 0 {
        println!(
            "calibrate: {} invalid (did not compile) — textual splices the language rejected, excluded from every rate",
            tally.invalid_compile
        );
    }

    let pairs = pair_lines(sampled, &file_scores, &filescope.scores, args.scope, &tally);
    let v = judge(sampled, &pairs, &tally);
    if let Some(path) = &args.emit_dataset {
        let rows = write_dataset(
            path,
            &args.path,
            (lang, profile),
            sampled,
            &pairs,
            &tally,
            &filescope,
        )?;
        println!("dataset: wrote {rows} rows to {}", path.display());
    }
    Ok(if args.gate {
        ExitCode::from(gate_code(v))
    } else {
        ExitCode::SUCCESS
    })
}

/// True for `.ts`/`.mts`/`.cts`/`.tsx` — the extensions the type-check
/// caveat applies to.
fn is_typescript(rel: &Path) -> bool {
    rel.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| TS_EXT.contains(&ext))
}

/// Exit status under `--gate`, as documented on the flag. A number rather
/// than `ExitCode` so the mapping is testable: `ExitCode` carries no
/// equality.
fn gate_code(v: Verdict) -> u8 {
    match v {
        Verdict::Calibrated | Verdict::Marginal => 0,
        Verdict::InsufficientData => 2,
        Verdict::Uncalibrated => 3,
    }
}

/// Scores a mixed (or single-language) Python/JavaScript tree and picks the
/// language with more scored lines, saying so when both are present.
fn score_by_majority(
    root: &Path,
    scan: &TreeScan,
    python: &str,
    lowering: Lowering,
) -> (Language, ScoreOutcome) {
    let py = (!scan.py.is_empty())
        .then(|| score_tree(root, &scan.py, Language::Python, python, lowering));
    let js = (!scan.js.is_empty())
        .then(|| score_tree(root, &scan.js, Language::JavaScript, python, lowering));
    match (py, js) {
        (Some(p), None) => (Language::Python, p),
        (None, Some(j)) => (Language::JavaScript, j),
        (Some(p), Some(j)) => {
            let py_lines: usize = p.1.values().map(BTreeMap::len).sum();
            let js_lines: usize = j.1.values().map(BTreeMap::len).sum();
            let (winner, other, w_lines, o_lines) = if py_lines >= js_lines {
                (Language::Python, Language::JavaScript, py_lines, js_lines)
            } else {
                (Language::JavaScript, Language::Python, js_lines, py_lines)
            };
            println!(
                "calibrate: both Python and JavaScript/TypeScript sources found ({py_lines} Python scored lines, {js_lines} JavaScript/TypeScript); calibrating {} only ({w_lines} scored lines beats {other}'s {o_lines}) — narrow --path to target {other} instead",
                winner.label(),
                other = other.label(),
            );
            if winner == Language::Python {
                (Language::Python, p)
            } else {
                (Language::JavaScript, j)
            }
        }
        (None, None) => {
            unreachable!("checked above: at least one of scan.py, scan.js is non-empty")
        }
    }
}

/// Whether `lang` should score through the tree-sitter lowering rather than
/// a primary frontend. Python's answer is yes unless `--lowering primary`
/// explicitly asks for the bytecode path — the AST measured at least as
/// well on identical mutants (`eval/calibration/ast-fallback-comparison.md`)
/// and needs no interpreter for scoring (mutation still runs through
/// `python3` either way). JavaScript has no primary/fallback split at all -
/// `vikt-js`'s oxc engine is the only lowering it has ever had.
fn use_tree_sitter(lang: Language, lowering: Lowering) -> bool {
    match (lang, lowering) {
        (Language::Python, Lowering::Auto | Lowering::Ast) => true,
        (Language::JavaScript, _) | (Language::Python, Lowering::Primary) => false,
        // Rust lowers whole packages, not files — see `score_crate`, which
        // makes this decision for itself against `vikt_rs::lower_crate`'s
        // own error rather than a separate up-front probe.
        (Language::Rust, _) => unreachable!("use_tree_sitter is never called for Rust"),
    }
}

/// Lowers and panel-scores every non-test source of `lang` in the copy. Test
/// files are excluded from scoring and mutation both: mutating the suite
/// measures the suite's self-checks, not the code the panel scored. A file
/// the frontend cannot lower is reported and skipped rather than aborting
/// the run — one broken scratch file should not block calibrating the rest
/// of a tree.
fn score_tree(
    root: &Path,
    files: &[PathBuf],
    lang: Language,
    python: &str,
    lowering: Lowering,
) -> ScoreOutcome {
    let via_ts = use_tree_sitter(lang, lowering);
    let profile = if via_ts {
        PanelProfile::Statement
    } else {
        lang.panel_profile()
    };
    let generator = match (lang, via_ts) {
        (Language::Python, true) => "vikt-ts/tree-sitter-python",
        (Language::Python, false) => "vikt-py/dis",
        (Language::JavaScript, _) => "vikt-js/oxc",
        (Language::Rust, _) => unreachable!("score_tree is never called for Rust"),
    };
    let mut scored = Vec::new();
    let mut file_scores = FileScores::new();
    for rel in files.iter().filter(|p| !is_test_path(p, lang)) {
        let functions = match (lang, via_ts) {
            (Language::Python, true) => match vikt_ts::lower_file(&root.join(rel)) {
                Ok(l) => l.functions,
                Err(e) => {
                    eprintln!("calibrate: skipping {}: {e}", rel.display());
                    continue;
                }
            },
            (Language::Python, false) => match vikt_py::lower_file_with(&root.join(rel), python) {
                Ok(l) => l.functions,
                Err(e) => {
                    eprintln!("calibrate: skipping {}: {e}", rel.display());
                    continue;
                }
            },
            (Language::JavaScript, _) => match vikt_js::lower_file(&root.join(rel)) {
                Ok(l) => l.functions,
                Err(e) => {
                    eprintln!("calibrate: skipping {}: {e}", rel.display());
                    continue;
                }
            },
            (Language::Rust, _) => unreachable!("score_tree is never called for Rust"),
        };
        score_functions(
            rel,
            &functions,
            lang,
            profile,
            &mut scored,
            &mut file_scores,
        );
    }
    (scored, file_scores, generator.to_owned(), profile)
}

/// Lowers and panel-scores a whole cargo package, `auto`/`primary`/`ast` as
/// documented on [`Lowering`]. The primary path is one lowering covering
/// every file, so unlike [`score_tree`] there is no per-file skip there: a
/// package that does not compile does not calibrate, and says why through
/// cargo's own diagnostics. Functions whose spans resolve outside the copy
/// (macro expansions, generated code) never reach here — the lowering
/// already drops foreign lines. The tree-sitter fallback path (see
/// [`lowering::lower_rust_crate`]) has no such single-shot lowering, so it
/// walks the package's own sources file by file instead, same as
/// [`score_tree`].
fn score_crate(root: &Path, lowering_arg: Lowering) -> Result<ScoreOutcome, Box<dyn Error>> {
    let lowered = lowering::lower_rust_crate(root, None, lowering_arg)?;
    let mut scored = Vec::new();
    let mut file_scores = FileScores::new();
    let mut by_file: BTreeMap<PathBuf, Vec<FunctionIr>> = BTreeMap::new();
    for ir in lowered.functions {
        let file = PathBuf::from(&ir.id.file);
        let rel = file.strip_prefix(root).unwrap_or(&file).to_path_buf();
        by_file.entry(rel).or_default().push(ir);
    }
    for (rel, functions) in by_file {
        // An absolute path that did not strip is outside the copy entirely:
        // a dependency's sources, or the sysroot. Not ours to mutate.
        if rel.is_absolute() || is_test_path(&rel, Language::Rust) {
            continue;
        }
        score_functions(
            &rel,
            &functions,
            Language::Rust,
            lowered.profile,
            &mut scored,
            &mut file_scores,
        );
    }
    Ok((scored, file_scores, lowered.generator, lowered.profile))
}

/// The shared per-function scoring step: panel scores projected to lines
/// (via the same overwrite-and-project path the sidecar uses) plus the raw
/// panel feature vectors for `--emit-dataset`, which come from the plain
/// `analyze` pass — `line_features` takes the incumbent's scores as its
/// `current` member, so it must never see the panel-overwritten ones.
fn score_functions(
    rel: &Path,
    functions: &[vikt_core::FunctionIr],
    lang: Language,
    profile: PanelProfile,
    scored: &mut Vec<ScoredFn>,
    file_scores: &mut FileScores,
) {
    for ir in functions {
        if let Err(e) = ir.validate() {
            eprintln!(
                "calibrate: skipping {} in {}: {e}",
                ir.id.name,
                rel.display()
            );
            continue;
        }
        if ir.is_empty() {
            continue;
        }
        if ir.len() > MAX_INSTRUCTIONS {
            eprintln!(
                "calibrate: skipping {} in {} ({} instructions > {MAX_INSTRUCTIONS})",
                ir.id.name,
                rel.display(),
                ir.len()
            );
            continue;
        }
        // Synthetic wrapper functions overlap the extent of a real def;
        // sampling them would double-count the same lines and degrade
        // the positional null into noise. See `Language::is_synthetic`.
        if lang.is_synthetic(&ir.id.name) {
            continue;
        }
        let sal = analyze_with_scorer(
            ir,
            &Denylist::new(),
            &ScoreWeights::default(),
            Scorer::Panel(profile),
        );
        let spans = project_to_lines(ir, &sal);
        let Some(lo) = spans.iter().map(|s| s.start).min() else {
            continue;
        };
        let hi = spans.iter().map(|s| s.end).max().unwrap_or(lo);
        let base = analyze(ir, &Denylist::new(), &ScoreWeights::default());
        let feats = vikt_core::panel::line_features(ir, &base, &Denylist::new());
        let per_file = file_scores.entry(rel.to_path_buf()).or_default();
        let mut lines = 0;
        let mut line_scores = BTreeMap::new();
        for s in &spans {
            for ln in s.start..=s.end {
                lines += 1;
                line_scores.insert(ln, s.score);
                let e = per_file.entry(ln).or_insert(f64::MIN);
                if s.score > *e {
                    *e = s.score;
                }
            }
        }
        scored.push(ScoredFn {
            file: rel.to_path_buf(),
            name: ir.id.name.clone(),
            lo,
            hi,
            lines,
            feats,
            ir: ir.clone(),
            importance: base,
            line_scores,
        });
    }
}

/// The file-scope function-importance layer, computed once and threaded
/// through pairing and dataset writing together, so both stay in lockstep
/// without pushing the argument count of either past clippy's limit.
#[derive(Default)]
struct Filescope {
    /// Blended per-line score, file by file — parallel in shape to
    /// [`FileScores`].
    scores: FileScores,
    /// Parallel to the `scored` slice `filescope_layer` was built from:
    /// each function's own four call-graph features.
    features: Vec<FunctionFeatures>,
    /// Per file, each scored line's owning function as an index into that
    /// same slice — `vikt_core::line_owners`'s attribution, which is what
    /// `scores` blended, so dataset rows report the features of the
    /// function that actually produced the line's file score.
    owners: BTreeMap<PathBuf, BTreeMap<u32, usize>>,
}

/// Builds the file-scope layer, grouped by file so a line is never blended
/// against a sibling in a different source file. Runs over every scored
/// function, not only the sample: an unsampled sibling still shapes the
/// call graph its sampled neighbours are ranked against.
fn filescope_layer(scored: &[ScoredFn]) -> Filescope {
    let mut by_file: BTreeMap<&Path, Vec<usize>> = BTreeMap::new();
    for (i, f) in scored.iter().enumerate() {
        by_file.entry(&f.file).or_default().push(i);
    }
    let mut layer = Filescope {
        features: vec![FunctionFeatures::default(); scored.len()],
        ..Filescope::default()
    };
    for (file, idxs) in by_file {
        let functions: Vec<ScopedFunction<'_>> = idxs
            .iter()
            .map(|&i| ScopedFunction {
                ir: &scored[i].ir,
                importance: &scored[i].importance,
                line_scores: &scored[i].line_scores,
            })
            .collect();
        for (&i, feat) in idxs.iter().zip(vikt_core::function_features(&functions)) {
            layer.features[i] = feat;
        }
        let owners = vikt_core::line_owners(&functions)
            .into_iter()
            .map(|(line, local)| (line, idxs[local]))
            .collect();
        layer.owners.insert(file.to_path_buf(), owners);
        layer
            .scores
            .insert(file.to_path_buf(), vikt_core::file_scores(&functions));
    }
    layer
}

/// Generates line-targeted mutants for the sampled functions, file by file
/// in path order, capped by the budget, through `lang`'s mutation engine.
/// Returns the mutants, the uncapped candidate-site count (so the caller can
/// say when it truncated) and the count discarded for failing to re-parse
/// after mutation (JavaScript only — see the module docs).
fn generate_mutants(
    root: &Path,
    sampled: &[ScoredFn],
    lang: Language,
    args: &CalibrateArgs,
    budget: usize,
) -> Result<(Vec<FileMutant>, usize, usize), Box<dyn Error>> {
    let mut files: Vec<&PathBuf> = sampled.iter().map(|f| &f.file).collect();
    files.sort();
    files.dedup();
    let mut mutants: Vec<FileMutant> = Vec::new();
    let mut candidates = 0usize;
    let mut invalid_discarded = 0usize;
    for file in files {
        let mut spans: Vec<(u32, u32)> = sampled
            .iter()
            .filter(|f| &f.file == file)
            .map(|f| (f.lo, f.hi))
            .collect();
        spans.sort_unstable();
        let remaining = budget - mutants.len();
        let set: MutantSet = match lang {
            Language::Python => {
                vikt_py::calibrate::mutants_for(&root.join(file), &spans, remaining, &args.python)?
            }
            Language::JavaScript => {
                vikt_js::calibrate::mutants_for(&root.join(file), &spans, remaining)?
            }
            Language::Rust => vikt_rs::calibrate::mutants_for(&root.join(file), &spans, remaining)?,
        };
        candidates += set.total_sites;
        invalid_discarded += set.invalid_discarded;
        mutants.extend(set.mutants.into_iter().map(|m| (file.clone(), m)));
    }
    Ok((mutants, candidates, invalid_discarded))
}

/// Kill/survive counts, overall and per mutated line of the original source.
#[derive(Default)]
struct Tally {
    killed: usize,
    survived: usize,
    timeouts: usize,
    /// Rust only: mutants whose build step failed — textual splices the
    /// language rejected. Neither killed nor survived; absent from
    /// `per_line` so no kill rate ever sees them.
    invalid_compile: usize,
    /// (file, line) -> (kills, mutants).
    per_line: BTreeMap<(PathBuf, u32), (usize, usize)>,
}

impl Tally {
    fn executed(&self) -> usize {
        self.killed + self.survived + self.timeouts
    }
}

/// Runs every mutant: write it over the copy, run the suite, restore the
/// copy. Killed = the suite noticed (non-zero exit or hang). The original
/// bytes are restored before any error propagates, so a failed run never
/// leaves the copy mutated.
fn execute_mutants(
    root: &Path,
    mutants: &[FileMutant],
    args: &CalibrateArgs,
    lang: Language,
    timeout: Duration,
) -> Result<Tally, Box<dyn Error>> {
    let mut originals: BTreeMap<&PathBuf, Vec<u8>> = BTreeMap::new();
    for (file, _) in mutants {
        if !originals.contains_key(file) {
            originals.insert(file, std::fs::read(root.join(file))?);
        }
    }
    let mut tally = Tally::default();
    for (i, (file, mutant)) in mutants.iter().enumerate() {
        let abs = root.join(file);
        std::fs::write(&abs, mutant.source.as_bytes())?;
        // Rust: build first. A splice the compiler rejects is an invalid
        // mutant, not a kill — and a build that hangs is treated the same
        // way, since nothing behavioural was ever measured. The copy's
        // target dir persists across mutants, so each build is incremental.
        if lang == Language::Rust {
            let build = run_test(&args.build_cmd, root, timeout);
            match build {
                Ok(TestOutcome::Pass) => {}
                Ok(TestOutcome::Fail | TestOutcome::Timeout) => {
                    std::fs::write(&abs, &originals[file])?;
                    tally.invalid_compile += 1;
                    eprintln!(
                        "calibrate: mutant {}/{} {}:{} {} ({}) -> invalid (did not compile)",
                        i + 1,
                        mutants.len(),
                        file.display(),
                        mutant.line,
                        mutant.kind,
                        mutant.detail,
                    );
                    continue;
                }
                Err(e) => {
                    std::fs::write(&abs, &originals[file])?;
                    return Err(e.into());
                }
            }
        }
        let outcome = run_test(&args.test_cmd, root, timeout);
        std::fs::write(&abs, &originals[file])?;
        let outcome = outcome?;
        let kill = match outcome {
            TestOutcome::Pass => {
                tally.survived += 1;
                false
            }
            TestOutcome::Fail => {
                tally.killed += 1;
                true
            }
            TestOutcome::Timeout => {
                tally.timeouts += 1;
                true
            }
        };
        let entry = tally
            .per_line
            .entry((file.clone(), mutant.line))
            .or_insert((0, 0));
        entry.1 += 1;
        if kill {
            entry.0 += 1;
        }
        eprintln!(
            "calibrate: mutant {}/{} {}:{} {} ({}) -> {}",
            i + 1,
            mutants.len(),
            file.display(),
            mutant.line,
            mutant.kind,
            mutant.detail,
            match outcome {
                TestOutcome::Pass => "survived",
                TestOutcome::Fail => "killed",
                TestOutcome::Timeout => "timeout (killed)",
            }
        );
    }
    Ok(tally)
}

/// One mutated line joined with everything the verdict and the dataset both
/// need: its innermost sampled function, panel score, positional null and
/// kill counts.
struct PairedLine {
    file: PathBuf,
    line: u32,
    owner: usize,
    panel: f64,
    null: f64,
    kills: usize,
    total: usize,
}

/// Pairs each mutated line with its panel score, its positional-null score
/// and its kill counts. A line is attributed to the innermost sampled
/// function containing it, so nested defs do not inherit their parent's
/// extent for the null; lines without a panel score are excluded here, which
/// keeps the dataset and the verdict measuring the identical set.
///
/// Under [`Scope::Function`] `panel`/`null` are exactly as before: the
/// owning sampled function's own score and positional extent. Under
/// [`Scope::File`] both come from `filescope` instead — the call-graph
/// blended score, and the positional null measured over the whole file's
/// scored-line extent rather than one function's. `owner` (used for
/// per-function reporting and the dataset's function features) is found the
/// same way either way.
fn pair_lines(
    sampled: &[ScoredFn],
    file_scores: &FileScores,
    filescope: &FileScores,
    scope: Scope,
    tally: &Tally,
) -> Vec<PairedLine> {
    let mut pairs = Vec::new();
    let mut unscored = 0usize;
    for ((file, line), (kills, total)) in &tally.per_line {
        let owner = sampled
            .iter()
            .enumerate()
            .filter(|(_, f)| &f.file == file && f.lo <= *line && *line <= f.hi)
            .min_by_key(|(_, f)| f.hi - f.lo)
            .map(|(i, _)| i);
        let panel_score = match scope {
            Scope::Function => file_scores.get(file).and_then(|m| m.get(line)),
            Scope::File => filescope.get(file).and_then(|m| m.get(line)),
        };
        let (Some(owner), Some(&panel_score)) = (owner, panel_score) else {
            unscored += 1;
            continue;
        };
        let null = match scope {
            Scope::Function => {
                let f = &sampled[owner];
                if f.hi > f.lo {
                    1.0 - f64::from(line - f.lo) / f64::from(f.hi - f.lo)
                } else {
                    1.0
                }
            }
            Scope::File => {
                let extent = filescope
                    .get(file)
                    .expect("filescope carries this file: `panel_score` above matched against it");
                let file_lo = *extent
                    .keys()
                    .next()
                    .expect("non-empty: `panel_score` above matched a line in it");
                let file_hi = *extent.keys().next_back().unwrap_or(&file_lo);
                if file_hi > file_lo {
                    1.0 - f64::from(line - file_lo) / f64::from(file_hi - file_lo)
                } else {
                    1.0
                }
            }
        };
        pairs.push(PairedLine {
            file: file.clone(),
            line: *line,
            owner,
            panel: panel_score,
            null,
            kills: *kills,
            total: *total,
        });
    }
    println!(
        "calibrate: {} mutated lines carry a panel score{}",
        pairs.len(),
        if unscored > 0 {
            format!(", {unscored} carry none and are excluded")
        } else {
            String::new()
        }
    );
    pairs
}

/// Prints the correlations and renders the verdict from the paired lines.
fn judge(sampled: &[ScoredFn], pairs: &[PairedLine], tally: &Tally) -> Verdict {
    let mut by_fn: BTreeMap<usize, Vec<(f64, f64)>> = BTreeMap::new();
    for pl in pairs {
        by_fn
            .entry(pl.owner)
            .or_default()
            .push((pl.panel, pl.kills as f64 / pl.total as f64));
    }
    let mut printed_header = false;
    for (owner, obs) in &by_fn {
        if obs.len() < 4 {
            continue;
        }
        if !printed_header {
            println!("\nper-function rho (functions with >= 4 scored lines):");
            printed_header = true;
        }
        let f = &sampled[*owner];
        let rho = spearman(
            &obs.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            &obs.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        );
        println!(
            "  {}:{:<24} {rho:>6.3}  ({} lines)",
            f.file.display(),
            f.name,
            obs.len()
        );
    }

    let panel: Vec<f64> = pairs.iter().map(|p| p.panel).collect();
    let null: Vec<f64> = pairs.iter().map(|p| p.null).collect();
    let rates: Vec<f64> = pairs
        .iter()
        .map(|p| p.kills as f64 / p.total as f64)
        .collect();
    let panel_rho = spearman(&panel, &rates);
    let null_rho = spearman(&null, &rates);
    println!("\npooled Spearman rho over {} lines:", pairs.len());
    println!("  panel            {panel_rho:>6.3}");
    println!("  positional null  {null_rho:>6.3}");

    let v = verdict(panel_rho, null_rho, pairs.len(), tally.executed());
    match v {
        Verdict::Calibrated => println!(
            "\nverdict: calibrated — panel rho {panel_rho:.3} beats the positional null ({null_rho:.3}) by {:.3} (needs margin >= {NULL_MARGIN} and rho >= {RHO_FLOOR})",
            panel_rho - null_rho
        ),
        Verdict::Marginal => println!(
            "\nverdict: marginal — panel rho {panel_rho:.3} beats the positional null ({null_rho:.3}), but not by both the {NULL_MARGIN} margin and the {RHO_FLOOR} floor"
        ),
        Verdict::Uncalibrated => println!(
            "\nverdict: uncalibrated — panel rho {panel_rho:.3} does not beat the positional null ({null_rho:.3}) on this tree"
        ),
        Verdict::InsufficientData => println!(
            "\nverdict: insufficient data — {} scored lines (needs >= {MIN_SCORED_LINES}) over {} executed mutants (needs >= {MIN_EXECUTED_MUTANTS})",
            pairs.len(),
            tally.executed()
        ),
    }
    v
}

/// The `--emit-dataset` row: one mutated, panel-scored line. Serialized as
/// JSONL in struct-field order; the `instruments` keys are the panel's
/// feature order and refit tooling depends on exactly these seven.
/// `function_features` and `file_score` are additive: present regardless of
/// `--scope`, since they measure the file-scope layer independently of
/// which scope the run's own verdict used.
#[derive(serde::Serialize)]
struct DatasetRow<'a> {
    root: String,
    language: &'static str,
    profile: &'static str,
    file: String,
    function: &'a str,
    line: u32,
    instruments: Instruments,
    panel: f64,
    mutants: usize,
    killed: usize,
    kill_rate: f64,
    function_features: FunctionFeaturesRow,
    file_score: f64,
}

/// The seven panel features by name, in weight order.
#[derive(serde::Serialize)]
struct Instruments {
    current: f64,
    schur: f64,
    pivot: f64,
    trophic: f64,
    strahler: f64,
    position: f64,
    boundary: u8,
}

/// `vikt_core::filescope`'s four rank-normalised call-graph signals for the
/// row's own function, under their dataset names.
#[derive(serde::Serialize)]
struct FunctionFeaturesRow {
    trophic: f64,
    fan_in: f64,
    size_share: f64,
    boundary_density: f64,
}

impl From<FunctionFeatures> for FunctionFeaturesRow {
    fn from(f: FunctionFeatures) -> Self {
        Self {
            trophic: f.trophic,
            fan_in: f.fan_in,
            size_share: f.size_share,
            boundary_density: f.boundary_density,
        }
    }
}

/// Writes the dataset: every paired line, sorted by (file, function, line),
/// one JSON object per line, overwriting `path`. Returns the row count.
/// `filescope` is the file-scope layer over every scored function
/// ([`filescope_layer`]), computed independently of `--scope` — a dataset
/// row always carries both its blended score and its function features.
fn write_dataset(
    path: &Path,
    target: &Path,
    scored_via: (Language, PanelProfile),
    sampled: &[ScoredFn],
    pairs: &[PairedLine],
    _tally: &Tally,
    filescope: &Filescope,
) -> Result<usize, Box<dyn Error>> {
    let (lang, scored_profile) = scored_via;
    let profile = match scored_profile {
        PanelProfile::Instruction => "instruction",
        PanelProfile::Statement => "statement",
    };
    let mut rows: Vec<&PairedLine> = pairs.iter().collect();
    rows.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| sampled[a.owner].name.cmp(&sampled[b.owner].name))
            .then_with(|| a.line.cmp(&b.line))
    });
    let mut out = String::new();
    let mut written = 0usize;
    for pl in rows {
        let f = &sampled[pl.owner];
        // A line whose owning function carries no feature vector for it
        // (projection edge cases) is excluded, mirroring the rho exclusion.
        // Same treatment for a missing file-scope score, which should not
        // happen once `filescope.scores` covers every scored function, but
        // a silent skip beats a panicking index either way.
        let Some(feats) = f.feats.get(&pl.line) else {
            continue;
        };
        let Some(&file_score) = filescope.scores.get(&pl.file).and_then(|m| m.get(&pl.line)) else {
            continue;
        };
        let row = DatasetRow {
            root: target.display().to_string(),
            language: lang.slug(),
            profile,
            file: pl.file.display().to_string(),
            function: &f.name,
            line: pl.line,
            instruments: Instruments {
                current: feats[0],
                schur: feats[1],
                pivot: feats[2],
                trophic: feats[3],
                strahler: feats[4],
                position: feats[5],
                boundary: u8::from(feats[6] > 0.5),
            },
            panel: pl.panel,
            mutants: pl.total,
            killed: pl.kills,
            kill_rate: pl.kills as f64 / pl.total as f64,
            function_features: filescope.features[filescope
                .owners
                .get(&pl.file)
                .and_then(|m| m.get(&pl.line))
                .copied()
                .unwrap_or(pl.owner)]
            .into(),
            file_score,
        };
        out.push_str(&serde_json::to_string(&row)?);
        out.push('\n');
        written += 1;
    }
    std::fs::write(path, out)?;
    Ok(written)
}

/// What a walk of the input tree found. Paths are relative to the root and
/// sorted, so every downstream stage is order-deterministic.
struct TreeScan {
    files: Vec<PathBuf>,
    py: Vec<PathBuf>,
    js: Vec<PathBuf>,
    rs: Vec<PathBuf>,
    /// `node_modules` directories found anywhere in the tree, relative to
    /// the root. Never descended into for scoring or mutation, but staged
    /// into the copy (see [`TempTree::create`]) since a `node --test`/`npm
    /// test` command needs its dependencies to run at all.
    node_modules: Vec<PathBuf>,
    other_frontend: bool,
    skipped_large: usize,
}

fn scan_tree(root: &Path) -> std::io::Result<TreeScan> {
    let mut scan = TreeScan {
        files: Vec::new(),
        py: Vec::new(),
        js: Vec::new(),
        rs: Vec::new(),
        node_modules: Vec::new(),
        other_frontend: false,
        skipped_large: 0,
    };
    let mut visited = BTreeSet::new();
    walk(root, root, &mut scan, &mut visited)?;
    scan.files.sort();
    scan.py.sort();
    scan.js.sort();
    scan.rs.sort();
    scan.node_modules.sort();
    Ok(scan)
}

fn walk(
    root: &Path,
    dir: &Path,
    scan: &mut TreeScan,
    visited: &mut BTreeSet<PathBuf>,
) -> std::io::Result<()> {
    // Symlinks are followed below, so a cyclic link could revisit a
    // directory; the canonical-path set makes every real directory walk once.
    if !visited.insert(std::fs::canonicalize(dir)?) {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Metadata through the path, not the entry: the entry's variant
        // reports a symlink as neither file nor directory, and a tree is
        // entitled to keep sources or fixture dirs behind links — the copy
        // materialises what they point at. A dangling link has nothing to
        // copy; skipping it beats aborting the run over one stale reference.
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if meta.is_dir() {
            if name.starts_with('.') {
                continue;
            }
            if name == "node_modules" {
                let rel = path
                    .strip_prefix(root)
                    .expect("invariant: the walk never leaves the root")
                    .to_path_buf();
                scan.node_modules.push(rel);
                continue;
            }
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk(root, &path, scan, visited)?;
        } else if meta.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            // Dot-prefixed files ride along: `.env` and `.coveragerc` are
            // configuration the suite runs under, and a copy without them
            // measures a differently configured suite.
            if ext == "pyc" {
                continue;
            }
            if meta.len() > MAX_COPY_BYTES {
                scan.skipped_large += 1;
                continue;
            }
            if OTHER_FRONTENDS.contains(&ext) {
                scan.other_frontend = true;
            }
            let rel = path
                .strip_prefix(root)
                .expect("invariant: the walk never leaves the root")
                .to_path_buf();
            if PY_EXT.contains(&ext) {
                scan.py.push(rel.clone());
            } else if JS_EXT.contains(&ext) {
                scan.js.push(rel.clone());
            } else if RS_EXT.contains(&ext) {
                scan.rs.push(rel.clone());
            }
            scan.files.push(rel);
        }
    }
    Ok(())
}

/// The temporary copy, removed on drop so no exit path leaks it.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn create(src: &Path, files: &[PathBuf], node_modules: &[PathBuf]) -> std::io::Result<Self> {
        let mut root = std::env::temp_dir();
        root.push(format!("vikt-calibrate-{}", std::process::id()));
        // A leftover from a crashed run under the same pid would mix trees.
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        let tree = Self { root };
        for rel in files {
            let dst = tree.root.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(src.join(rel), dst)?;
        }
        for rel in node_modules {
            let dst = tree.root.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // A symlink to the source tree would be cheaper, but a
            // `require`/`import` resolved into it can run the dependency's
            // own build tooling (postinstall scripts, bundler caches) —
            // writes that would land in the real tree through the link.
            // The invariant that the input tree is never opened for
            // writing holds only by copying node_modules fully, same as
            // every other file in the tree.
            copy_dir_all(&src.join(rel), &dst)?;
        }
        Ok(tree)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Recursively copies `src` into `dst`, preserving any symlink it contains
/// (workspace-linked packages inside `node_modules` are commonly symlinks)
/// rather than resolving and duplicating its target.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if ty.is_symlink() {
            clone_symlink(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// True for files that are part of the test suite rather than the code under
/// test. Shared across languages: `test`/`tests` directories, and (for
/// JavaScript/TypeScript only) `__tests__`. Filename suffixes are
/// per-language convention: `test_*.py`/`*_test.py` for Python,
/// `*.test.<ext>`/`*.spec.<ext>` for JavaScript/TypeScript.
// The `.test`/`.spec` suffix check below is deliberately case-sensitive —
// it is a filename convention, not a file-extension check — so clippy's
// file-extension heuristic does not apply here.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_test_path(rel: &Path, lang: Language) -> bool {
    let in_test_dir = rel.parent().is_some_and(|p| {
        p.components().any(|c| {
            let name = c.as_os_str().to_str();
            matches!(name, Some("test" | "tests"))
                || (lang == Language::JavaScript && name == Some("__tests__"))
                || (lang == Language::Rust && matches!(name, Some("benches" | "examples")))
        })
    });
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let by_name = match lang {
        Language::Python => name.starts_with("test_") || name.ends_with("_test.py"),
        Language::JavaScript => JS_EXT.iter().any(|ext| {
            name.strip_suffix(&format!(".{ext}"))
                .is_some_and(|base| base.ends_with(".test") || base.ends_with(".spec"))
        }),
        // Rust unit tests live inside source files behind #[cfg(test)],
        // which `cargo check` never lowers — path rules only need to cover
        // the conventional integration/bench/example directories above.
        Language::Rust => false,
    };
    in_test_dir || by_name
}

/// Runs the test command with a hard wall-clock limit.
///
/// Output goes to the void: a suite's chatter is per-mutant noise, and an
/// unread pipe would deadlock a chatty suite long before the timeout.
/// `PYTHONDONTWRITEBYTECODE` keeps the copy free of `.pyc` staleness hazards
/// between mutant swaps.
///
/// The shell leads a fresh process group and the timeout kills the group,
/// not just the shell: timeouts are a routine outcome here (a flipped loop
/// condition is a stock mutant), and killing only the wrapper of a compound
/// or forking command would leave the suite's grandchildren running against
/// the shared copy while later mutants are swapped in under them.
fn run_test(cmd: &str, dir: &Path, timeout: Duration) -> std::io::Result<TestOutcome> {
    let mut child = shell(cmd, dir)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(if status.success() {
                TestOutcome::Pass
            } else {
                TestOutcome::Fail
            });
        }
        if started.elapsed() >= timeout {
            kill_group(child.id());
            child.kill()?;
            let _ = child.wait();
            return Ok(TestOutcome::Timeout);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The platform shell, leading its own process group where the platform
/// has one so [`kill_group`] can end the whole suite, not just the wrapper.
#[cfg(unix)]
fn shell(cmd: &str, dir: &Path) -> Command {
    use std::os::unix::process::CommandExt as _;
    let mut c = Command::new("sh");
    c.args(["-c", cmd]).current_dir(dir).process_group(0);
    c
}

#[cfg(windows)]
fn shell(cmd: &str, dir: &Path) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", cmd]).current_dir(dir);
    c
}

/// Recreates a symlink found inside `node_modules`. Windows restricts
/// symlink creation to elevated or developer-mode sessions, so there the
/// link's target is copied instead — correctness over fidelity.
#[cfg(unix)]
fn clone_symlink(link: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(std::fs::read_link(link)?, target)
}

#[cfg(windows)]
fn clone_symlink(link: &Path, target: &Path) -> std::io::Result<()> {
    if std::fs::metadata(link)?.is_dir() {
        copy_dir_all(link, target)
    } else {
        std::fs::copy(link, target).map(|_| ())
    }
}

/// Kills the whole process tree the test command spawned. Signalling a
/// Unix group takes a negative pid, which `Child::kill` cannot express and
/// the workspace's `unsafe_code = "deny"` keeps libc from providing, so
/// the signal goes through the shell's own `kill` — the run already
/// depends on `sh`. Windows has a first-class tree kill in `taskkill`.
/// The caller still kills and reaps the direct child either way, covering
/// the window where the wrapper exited before it could be signalled.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -s KILL -- -{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn kill_group(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented `--gate` contract, one branch per verdict: marginal
    /// gates as success, and 2 (not enough data) never collapses into 3
    /// (measured disagreement) — a CI job tells them apart by exit code
    /// alone.
    #[test]
    fn gate_codes_match_the_documented_contract() {
        assert_eq!(gate_code(Verdict::Calibrated), 0);
        assert_eq!(gate_code(Verdict::Marginal), 0);
        assert_eq!(gate_code(Verdict::InsufficientData), 2);
        assert_eq!(gate_code(Verdict::Uncalibrated), 3);
    }

    /// The Python directory/suffix rules are exactly what they were before
    /// this module gained JavaScript support — `__tests__` and
    /// `.test.py`/`.spec.py` conventions are JavaScript-only additions and
    /// must never start matching Python paths.
    #[test]
    fn python_test_path_rule_is_unchanged() {
        assert!(is_test_path(
            Path::new("pkg/tests/test_foo.py"),
            Language::Python
        ));
        assert!(is_test_path(Path::new("test_foo.py"), Language::Python));
        assert!(is_test_path(Path::new("foo_test.py"), Language::Python));
        assert!(!is_test_path(
            Path::new("pkg/__tests__/foo.py"),
            Language::Python
        ));
        assert!(!is_test_path(Path::new("foo.spec.py"), Language::Python));
        assert!(!is_test_path(Path::new("pkg/foo.py"), Language::Python));
    }

    #[test]
    fn javascript_test_path_conventions() {
        assert!(is_test_path(
            Path::new("pkg/tests/foo.js"),
            Language::JavaScript
        ));
        assert!(is_test_path(
            Path::new("pkg/__tests__/foo.js"),
            Language::JavaScript
        ));
        assert!(is_test_path(Path::new("foo.test.ts"), Language::JavaScript));
        assert!(is_test_path(
            Path::new("foo.spec.tsx"),
            Language::JavaScript
        ));
        assert!(!is_test_path(Path::new("pkg/foo.js"), Language::JavaScript));
    }

    #[test]
    fn synthetic_name_filter_differs_by_language() {
        assert!(Language::Python.is_synthetic("<module>"));
        assert!(Language::Python.is_synthetic("<lambda>"));
        assert!(!Language::Python.is_synthetic("checkout"));

        assert!(Language::JavaScript.is_synthetic("<module>"));
        // JS anonymous functions must stay scorable: excluding them the way
        // Python excludes lambdas would drop most real JS code.
        assert!(!Language::JavaScript.is_synthetic("<fn@12>"));
        assert!(!Language::JavaScript.is_synthetic("checkout"));
    }

    /// `node_modules` lands in the copy as a real directory, never a
    /// symlink back to the source tree — a symlink would let the test
    /// command's own tooling (build caches, postinstall scripts) write
    /// through it into the real project, violating the "input tree is
    /// never opened for writing" invariant documented at the top of this
    /// module.
    #[test]
    fn node_modules_is_copied_not_symlinked() {
        let src = std::env::temp_dir().join(format!(
            "vikt-calibrate-nm-src-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.join("node_modules/leftpad")).unwrap();
        std::fs::write(
            src.join("node_modules/leftpad/index.js"),
            b"module.exports = 1;\n",
        )
        .unwrap();

        let tree = TempTree::create(&src, &[], &[PathBuf::from("node_modules")]).unwrap();
        let copied = tree.root.join("node_modules");
        assert!(
            !copied.symlink_metadata().unwrap().file_type().is_symlink(),
            "node_modules in the copy must be a real directory, not a symlink to the source"
        );
        assert!(copied.join("leftpad/index.js").is_file());

        let _ = std::fs::remove_dir_all(&src);
    }
}
