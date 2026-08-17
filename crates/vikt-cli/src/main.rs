//! `vikt` — produce a per-line importance sidecar for a compiled class, or a
//! Rust, Python, JS/TS, Java, Kotlin or Go source file.
//!
//! ```text
//! vikt Foo.class                 # JSON sidecar on stdout
//! vikt foo.py --annotate Foo.py  # tiered source view
//! vikt Foo.class --stats         # tier histogram and timing
//! vikt foo.py --format sarif     # SARIF 2.1.0 for code-scanning uploads
//! vikt foo.py --scope file       # blend in each function's call-graph standing
//! vikt src/ --scope repo         # same blend, cross-file, over a whole folder or repo
//! vikt foo.rs --lowering ast     # force the tree-sitter fallback
//! vikt Foo.kt                    # Kotlin source - tree-sitter, always
//! vikt foo.go                    # Go source - tree-sitter, always
//! vikt calibrate src/ --test-cmd "python3 -m unittest"  # self-calibration
//! vikt calibrate src/ --test-cmd "node --test"          # ...or JS/TS
//! vikt calibrate pkg/ --test-cmd "cargo test"            # ...or a cargo package
//! ```

#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
// `CPython`, `MokaIR`, `JSR`, `SMAP` and friends are proper nouns in prose,
// not identifiers.
// Line counts are small; the f64 cast in the histogram cannot lose anything.
#![allow(clippy::cast_precision_loss, clippy::doc_markdown)]

mod calibrate;
mod language;
mod lowering;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand, ValueEnum};
use lowering::Lowering;
use vikt_core::ir::FunctionIr;
use vikt_core::{
    Denylist, FunctionImportance, PanelProfile, SarifLog, ScopedFunction, ScoreWeights, Scorer,
    Sidecar, Tier, analyze_with_scorer, file_scores, project_to_lines, repo_scores,
};

/// Output shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// The sidecar artifact as JSON.
    Json,
    /// One line per span, for reading in a terminal.
    Text,
    /// SARIF 2.1.0, one `note` result per reported line, for code-scanning
    /// uploads. `--sarif-tiers` selects which tiers are reported.
    Sarif,
}

/// CLI surface of the SARIF tier filter. Only the two tiers a code-scanning
/// consumer can act on are offered: plumbing and inert annotations would
/// outnumber the signal on any real file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SarifTier {
    /// Behavior-carrying lines.
    Core,
    /// Frontier lines: returns, throws, state writes, opaque calls.
    Boundary,
}

/// How wide a lens `--format`, `--annotate` and the sidecar's spans use for
/// ranking. Tiers are unaffected either way — this only chooses what the
/// continuous score and rank are measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
enum Scope {
    /// Every score and rank is local to its own function body, unaffected
    /// by any sibling function in the same file. Two different functions'
    /// scores are not comparable under this scope.
    Function,
    /// Additionally computes a function-importance layer over the file's
    /// intra-file call graph (see `vikt_core::filescope`) — fan-in,
    /// call-graph depth, size and boundary density among the file's other
    /// functions — and blends it into each line's within-function score,
    /// then re-ranks every scored line of the file against that blend. Adds
    /// a `file_score` field to every sidecar span; `score`, `rank` and tiers
    /// are untouched. A directory or cargo-package input still scopes each
    /// file to itself — file scope never compares lines across two
    /// different source files.
    #[default]
    File,
    /// The same call-graph-weighted blend, one rung up: every scored
    /// function of the whole run contributes to one call graph, cross-file
    /// edges included, and the re-rank runs over every scored line of the
    /// run at once rather than per file. Adds a `repo_score` field to every
    /// sidecar span; `score`, `rank`, `file_score` and tiers are untouched.
    /// A single-file input still computes it — trivially the same call
    /// graph `file` scope would build for that one file — but it earns its
    /// keep on a directory, cargo package, or any input with more than one
    /// file.
    Repo,
}

#[derive(Debug, Parser)]
#[command(
    name = "vikt",
    about = "Deterministic per-line importance tiering over function bodies",
    long_about = "Classifies every statement in every function body as core, boundary, \
plumbing or inert, and projects the result onto source lines.\n\n\
Accepts a JVM .class file, a Rust file or cargo package, or a Python, JS/TS, Java, \
Kotlin or Go source file. No model runs: every tier is the output of a dominance, \
loop or reachability query, and every span carries the reason that produced it.",
    // The analyze surface stays flag-style; `calibrate` is the one verb-shaped
    // operation. These two settings are what let a positional input and a
    // subcommand share the top level without either becoming spuriously
    // optional.
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// The `.class`, `.rs`, `.py`, `.js`/`.ts`, `.java`, `.kt` or `.go` file
    /// to analyze; a Rust cargo package (a directory with `Cargo.toml`, or a
    /// `Cargo.toml` path — its other-language sources, if any, are also
    /// lowered); or any other directory, walked for every registry-known
    /// source extension it contains, of any language, into one sidecar.
    #[arg(required = true)]
    input: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Json)]
    format: Format,

    /// Tiers reported as SARIF results, comma-separated. Only consulted with
    /// `--format sarif`.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "core",
        value_name = "TIERS"
    )]
    sarif_tiers: Vec<SarifTier>,

    /// Print the source with each line marked by tier. Requires the source file
    /// when analyzing bytecode, since a class file does not carry it.
    #[arg(long, value_name = "SOURCE")]
    annotate: Option<PathBuf>,

    /// Print a tier histogram and the analysis time.
    #[arg(long)]
    stats: bool,

    /// Do not treat any call as inert.
    #[arg(long)]
    no_denylist: bool,

    /// Additional inert call patterns, matched by substring. Repeatable.
    #[arg(long = "inert", value_name = "PATTERN")]
    inert: Vec<String>,

    /// Interpreter to use when lowering Python.
    #[arg(long, default_value = "python3")]
    python: String,

    /// Which algorithm produces the continuous per-line score. Tier
    /// assignment is unaffected by this choice.
    #[arg(long, value_enum, default_value_t = ScorerArg::Panel)]
    scorer: ScorerArg,

    /// Blend each function's standing in its file's call graph into every
    /// line's score (`file`, the default); rank lines within their own
    /// function only (`function`); or blend each function's standing in the
    /// *whole run's* call graph, cross-file edges included (`repo`). See
    /// [`Scope`].
    #[arg(long, value_enum, default_value_t = Scope::File)]
    scope: Scope,

    /// With a cargo package as input (a directory or Cargo.toml), select one
    /// package of the workspace, as `cargo -p` would.
    #[arg(long, value_name = "NAME")]
    package: Option<String>,

    /// Which lowering to use: `auto` (default) uses the primary
    /// bytecode/MIR frontend where available and falls back to the
    /// tree-sitter engine (noted on stderr) otherwise; `primary` requires
    /// the primary and errors exactly as before if it's missing; `ast`
    /// forces the tree-sitter engine even where the primary would work.
    /// `.class` and `.java`/`.kt`/`.go` source each have only one lowering,
    /// so this is a no-op for them; `.js`/`.ts` rejects `ast` explicitly
    /// (see `lowering::lower_js`).
    #[arg(long, value_enum, default_value_t = Lowering::Auto)]
    lowering: Lowering,

    /// Skip function bodies larger than this many instructions (0 = no limit).
    ///
    /// The analysis is quadratic in body size, and every body ever observed
    /// over this default was a machine-generated module-level data table -
    /// emoji code maps, HTML entity lists - where per-line importance is
    /// meaningless: an 18,086-instruction dict literal costs ~40 seconds to
    /// score and every line of it would score the same. Skipped bodies are
    /// reported on stderr, never silently dropped.
    #[arg(long, value_name = "N", default_value_t = 4096)]
    max_instructions: usize,
}

/// The one verb the CLI grows beyond analysis.
#[derive(Debug, Subcommand)]
enum Cmd {
    /// Self-calibrate on a repository: mutate lines the panel scored, let the
    /// repository's own test suite kill mutants, and report whether the panel
    /// ordering agrees with the kill rates. Python, JavaScript/TypeScript,
    /// Rust cargo packages (a directory with Cargo.toml), Java, Kotlin and
    /// Go; a Python/JS tree with both is calibrated in whichever scored more
    /// lines, while Rust/Java/Kotlin/Go each require the tree to hold no
    /// other calibratable language. Rust, Java, Kotlin and Go all build
    /// every mutant first — one that does not compile is invalid, not
    /// killed; Java and Kotlin have no default build command and require
    /// `--build-cmd` explicitly. TypeScript caveat: a type-invalid mutant is
    /// read as killed by the repository's own toolchain, not distinguished
    /// from a test catch.
    Calibrate(calibrate::CalibrateArgs),
}

/// CLI surface of [`Scorer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ScorerArg {
    /// The fitted five-instrument combination - the shipped default, and the
    /// only variant intended for consumers. Which of the two fitted weight
    /// vectors is used is substrate-selected automatically from the input's
    /// extension, not a flag: bytecode inputs (.class, .py) get the
    /// instruction-granular fit, JS/TS inputs get the statement-granular
    /// fit. See vikt-core's `panel` module for provenance of both.
    ///
    /// The single-instrument variants below are for measurement, audit and
    /// regression pinning ONLY. Every one has been measured at or below a
    /// positional null against expert judgement, and every one has functions
    /// it ranks backwards; the panel beats them all because their failures
    /// do not overlap. Do not ship a single instrument's score.
    Panel,
    /// The incumbent hand-tuned blend, alone. Measurement/audit only.
    Current,
    /// Schur-complement deletion sensitivity, alone. Measurement/audit only.
    Schur,
    /// Birnbaum structural importance, alone. Measurement/audit only.
    Pivot,
    /// Trophic-level derivation depth, alone. Measurement/audit only.
    Trophic,
    /// Horton-Strahler confluence order, alone. Measurement/audit only.
    Strahler,
}

/// Resolves the CLI's flat `--scorer` choice to a [`Scorer`], consulting
/// `profile` only for [`ScorerArg::Panel`]: that's the one variant whose
/// weight vector depends on the dependence-graph granularity the *lowering
/// that actually ran* produced (see [`PanelProfile`]) - `profile` comes
/// straight from [`lowering::Lowered`], not re-derived from the input's
/// extension, since the same extension can lower two different ways
/// (`.rs`/`.py` with or without their primary frontend) and only one of
/// those is instruction-granular. Every other `ScorerArg` variant is a
/// single instrument with no substrate-specific fit, so `profile` is unused
/// for them.
fn resolve_scorer(arg: ScorerArg, profile: PanelProfile) -> Scorer {
    match arg {
        ScorerArg::Panel => Scorer::Panel(profile),
        ScorerArg::Current => Scorer::Current,
        ScorerArg::Schur => Scorer::Schur,
        ScorerArg::Pivot => Scorer::Pivot,
        ScorerArg::Trophic => Scorer::Trophic,
        ScorerArg::Strahler => Scorer::Strahler,
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = match &args.command {
        Some(Cmd::Calibrate(c)) => calibrate::run(c),
        None => run(&args).map(|()| ExitCode::SUCCESS),
    };
    result.unwrap_or_else(|e| {
        eprintln!("vikt: {e}");
        ExitCode::FAILURE
    })
}

/// One lowered function paired with the panel profile fitted to whichever
/// lowering produced it. A single-substrate input (one file, or a cargo
/// package's own `.rs` files) has exactly one profile for every function in
/// it, same as before this type existed; a folder or a `Cargo.toml`
/// directory's extra non-Rust files can mix bytecode/MIR-lowered files
/// (`Instruction`) with tree-sitter/oxc-lowered ones (`Statement`) in one
/// run, and scoring needs to know which is which per function.
struct FnInput {
    ir: FunctionIr,
    profile: PanelProfile,
}

#[allow(clippy::too_many_lines)] // one stage per pipeline step; splitting hides the shape
fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let input = args
        .input
        .as_deref()
        .expect("invariant: clap requires the input whenever no subcommand is given");
    let mut denylist = if args.no_denylist {
        Denylist::empty()
    } else {
        Denylist::new()
    };
    for p in &args.inert {
        denylist = denylist.with(p.clone());
    }
    let weights = ScoreWeights::default();

    let is_crate_input = (input.is_dir() && input.join("Cargo.toml").is_file())
        || input.file_name().and_then(|f| f.to_str()) == Some("Cargo.toml");
    let is_folder_input = input.is_dir() && !is_crate_input;

    let lower_started = Instant::now();
    let (generator, functions, note) = if is_folder_input {
        let (generator, functions) = lower_folder_input(input, args)?;
        (generator, functions, None)
    } else if is_crate_input {
        let (generator, functions) = lower_crate_input(input, args)?;
        (generator, functions, None)
    } else {
        lower_single_file_input(input, args)?
    };

    let file_label = functions
        .first()
        .map_or_else(|| input.display().to_string(), |fi| fi.ir.id.file.clone());
    let mut sidecar = Sidecar::new(file_label, generator);

    // Best-effort source, for embedding each span's `text`: a source-file
    // frontend's `ir.id.file` is the exact path it was lowered from, so
    // reading it back always succeeds; a bytecode input's is a name
    // reconstructed from debug info (SMAP), not a path on disk, so the read
    // fails and `--annotate`'s source - the one place vikt is ever told
    // where such a file's source actually lives - is tried instead. Neither
    // succeeding leaves spans without `text`, never a fabricated one.
    // Cached per file path since one file's functions share one read.
    let annotate_source = args.annotate.as_deref().and_then(read_source);
    let mut source_cache: BTreeMap<String, Option<String>> = BTreeMap::new();

    let lowering = lower_started.elapsed();
    let analysis_started = Instant::now();
    let mut analyzed = 0usize;
    // Retained under `--scope file`/`--scope repo` only: both need every
    // sibling function's tiered analysis at once, unlike the push-as-you-go
    // default.
    let mut scoped: Vec<(&FunctionIr, FunctionImportance)> = Vec::new();
    for fi in &functions {
        let ir = &fi.ir;
        if let Err(e) = ir.validate() {
            eprintln!("vikt: skipping {}: {e}", ir.id.name);
            continue;
        }
        if ir.is_empty() {
            continue;
        }
        if args.max_instructions > 0 && ir.len() > args.max_instructions {
            eprintln!(
                "vikt: skipping {} ({} instructions > --max-instructions {})",
                ir.id.name,
                ir.len(),
                args.max_instructions
            );
            continue;
        }
        let sal = analyze_with_scorer(
            ir,
            &denylist,
            &weights,
            resolve_scorer(args.scorer, fi.profile),
        );
        let source = source_cache.entry(ir.id.file.clone()).or_insert_with(|| {
            read_source(Path::new(&ir.id.file)).or_else(|| annotate_source.clone())
        });
        sidecar.push_with_source(ir, &sal, source.as_deref());
        analyzed += 1;
        if matches!(args.scope, Scope::File | Scope::Repo) {
            scoped.push((ir, sal));
        }
    }
    sidecar.finish();
    match args.scope {
        Scope::Function => {}
        Scope::File => sidecar.apply_file_scope(&file_scope_by_file(&scoped)),
        Scope::Repo => sidecar.apply_repo_scope(&repo_scope_all(&scoped)),
    }
    let analysis = analysis_started.elapsed();

    if let Some(note) = note {
        eprintln!("vikt: note: {note}");
    }

    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&sidecar)?),
        Format::Text => print_text(&sidecar),
        Format::Sarif => {
            let tiers: Vec<Tier> = args
                .sarif_tiers
                .iter()
                .map(|t| match t {
                    SarifTier::Core => Tier::Core,
                    SarifTier::Boundary => Tier::Boundary,
                })
                .collect();
            // Crate mode and folder mode both have a root to relativize
            // file paths against; a single-file input's path passes through
            // as the artifact recorded it.
            let root = (is_crate_input || is_folder_input).then(|| {
                if input.is_dir() {
                    input.to_path_buf()
                } else {
                    input.parent().unwrap_or(Path::new("")).to_path_buf()
                }
            });
            let log = SarifLog::from_sidecar(&sidecar, &tiers, root.as_deref());
            println!("{}", serde_json::to_string_pretty(&log)?);
        }
    }

    if let Some(source) = &args.annotate {
        annotate(source, &sidecar, args.scope)?;
    }

    if args.stats {
        print_stats(
            &sidecar,
            analyzed,
            functions.len(),
            lowering,
            analysis,
            &functions,
        );
    }
    Ok(())
}

/// A generator string, the functions lowered under it, and — single-file
/// input only — the deferred `.class` SMAP note. See
/// [`lower_single_file_input`].
type SingleFileLowering =
    Result<(String, Vec<FnInput>, Option<String>), Box<dyn std::error::Error>>;

/// Single-file dispatch: the `.class`, `.rs`, `.py`, `.js`/`.ts`, `.java`,
/// `.kt` or `.go` input `main`'s doc comment advertises, one frontend chosen
/// by extension. The `.class` path's SMAP note is threaded through rather than
/// printed immediately, preserving the message order a single-file run has
/// always had (after scoring finishes, before the artifact is printed).
fn lower_single_file_input(input: &Path, args: &Args) -> SingleFileLowering {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let kind = language::classify(ext).ok_or_else(|| {
        format!(
            "unsupported input type {ext:?}: expected one of {}",
            language::supported_extensions()
        )
    })?;
    let (generator, raw_functions, note, profile) = match kind {
        language::InputKind::ClassFile => {
            let bytes = std::fs::read(input)?;
            let lowered = vikt_jvm::lower_class(&bytes)?;
            let note = lowering::class_note(&lowered);
            (
                "vikt-jvm/mokapot".to_owned(),
                lowered.functions,
                note,
                PanelProfile::Instruction,
            )
        }
        language::InputKind::Python => {
            let lowered = lowering::lower_python(input, &args.python, args.lowering)?;
            (lowered.generator, lowered.functions, None, lowered.profile)
        }
        language::InputKind::JsTs => {
            let lowered = lowering::lower_js(input, args.lowering)?;
            (lowered.generator, lowered.functions, None, lowered.profile)
        }
        language::InputKind::JvmSource | language::InputKind::GoSource => {
            let lowered = lowering::lower_ts_source(input)?;
            (lowered.generator, lowered.functions, None, lowered.profile)
        }
        language::InputKind::Rust => {
            let lowered = lowering::lower_rust_file(input, args.lowering)?;
            (lowered.generator, lowered.functions, None, lowered.profile)
        }
    };
    let functions = raw_functions
        .into_iter()
        .map(|ir| FnInput { ir, profile })
        .collect();
    Ok((generator, functions, note))
}

/// A cargo package (a directory with `Cargo.toml`, or a `Cargo.toml` path):
/// every `.rs` file of the primary package through cargo, exactly as
/// before, plus every other registry-known source file the package
/// directory contains — a Java helper script, a Python code-gen tool, a
/// `.js` build step — lowered through its own frontend and folded into the
/// same run, said on stderr since this is new behavior a reader of older
/// output would not expect.
fn lower_crate_input(
    input: &Path,
    args: &Args,
) -> Result<(String, Vec<FnInput>), Box<dyn std::error::Error>> {
    let root = lowering::rust_package_root(input);
    // Whole package through cargo: every source file of the primary
    // package, dependencies compiled but not analyzed.
    let lowered = lowering::lower_rust_crate(input, args.package.as_deref(), args.lowering)?;
    let rust_profile = lowered.profile;
    let mut generators: BTreeSet<String> = [lowered.generator].into_iter().collect();
    let mut functions: Vec<FnInput> = lowered
        .functions
        .into_iter()
        .map(|ir| FnInput {
            ir,
            profile: rust_profile,
        })
        .collect();

    let extra = lowering::lower_folder(
        &root,
        &args.python,
        args.lowering,
        &[language::InputKind::Rust],
    )?;
    if !extra.is_empty() {
        eprintln!(
            "vikt: note: {} non-Rust source file(s) alongside the cargo package also lowered",
            extra.len()
        );
    }
    for lowered in extra {
        generators.insert(lowered.generator);
        let profile = lowered.profile;
        functions.extend(
            lowered
                .functions
                .into_iter()
                .map(|ir| FnInput { ir, profile }),
        );
    }

    Ok((
        generators.into_iter().collect::<Vec<_>>().join(", "),
        functions,
    ))
}

/// A plain directory with no `Cargo.toml`: every registry-known source file
/// it contains, of any language the registry knows, lowered through
/// whichever frontend its extension selects and folded into one run — the
/// folder becomes a first-class multi-language input rather than the
/// cargo-only error a directory used to produce.
fn lower_folder_input(
    input: &Path,
    args: &Args,
) -> Result<(String, Vec<FnInput>), Box<dyn std::error::Error>> {
    let files = lowering::lower_folder(input, &args.python, args.lowering, &[])?;
    if files.is_empty() {
        return Err(format!(
            "no supported source files found under {} (expected one of {})",
            input.display(),
            language::supported_extensions()
        )
        .into());
    }
    let generators: BTreeSet<String> = files.iter().map(|f| f.generator.clone()).collect();
    let functions: Vec<FnInput> = files
        .into_iter()
        .flat_map(|f| {
            let profile = f.profile;
            f.functions
                .into_iter()
                .map(move |ir| FnInput { ir, profile })
        })
        .collect();
    Ok((
        generators.into_iter().collect::<Vec<_>>().join(", "),
        functions,
    ))
}

/// Runs [`file_scores`] once per source file among `scoped`, never mixing
/// lines across files — a directory or cargo-package input can retain
/// functions from several files at once (crate mode), and file scope must
/// stay scoped to each one individually.
fn file_scope_by_file(
    scoped: &[(&FunctionIr, FunctionImportance)],
) -> BTreeMap<String, BTreeMap<u32, f64>> {
    let line_scores: Vec<BTreeMap<u32, f64>> = scoped
        .iter()
        .map(|(ir, sal)| per_line_scores(ir, sal))
        .collect();

    let mut by_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, (ir, _)) in scoped.iter().enumerate() {
        by_file.entry(ir.id.file.as_str()).or_default().push(i);
    }

    by_file
        .into_iter()
        .map(|(file, idxs)| {
            let functions: Vec<ScopedFunction<'_>> = idxs
                .iter()
                .map(|&i| ScopedFunction {
                    ir: scoped[i].0,
                    importance: &scoped[i].1,
                    line_scores: &line_scores[i],
                })
                .collect();
            (file.to_owned(), file_scores(&functions))
        })
        .collect()
}

/// Runs [`repo_scores`] once across every function retained under `--scope
/// repo`, letting the call graph and the re-rank both cross file
/// boundaries, then regroups its `(file, line)` keys by file — the shape
/// [`Sidecar::apply_repo_scope`] expects, identical in kind to what
/// [`file_scope_by_file`] builds per file for [`Sidecar::apply_file_scope`].
/// Unlike `file_scope_by_file`, this makes exactly one call: repo scope's
/// whole point is that a sibling in another file can still shape a
/// function's weight and still share the re-rank.
fn repo_scope_all(
    scoped: &[(&FunctionIr, FunctionImportance)],
) -> BTreeMap<String, BTreeMap<u32, f64>> {
    let line_scores: Vec<BTreeMap<u32, f64>> = scoped
        .iter()
        .map(|(ir, sal)| per_line_scores(ir, sal))
        .collect();
    let functions: Vec<ScopedFunction<'_>> = scoped
        .iter()
        .zip(&line_scores)
        .map(|((ir, sal), line_scores)| ScopedFunction {
            ir,
            importance: sal,
            line_scores,
        })
        .collect();
    let mut by_file: BTreeMap<String, BTreeMap<u32, f64>> = BTreeMap::new();
    for ((file, line), score) in repo_scores(&functions) {
        by_file.entry(file).or_default().insert(line, score);
    }
    by_file
}

/// A function's within-function score, per source line: [`project_to_lines`]'s
/// span-level scores expanded to one entry per line, since
/// [`ScopedFunction::line_scores`] wants a flat per-line map rather than
/// runs.
fn per_line_scores(ir: &FunctionIr, sal: &FunctionImportance) -> BTreeMap<u32, f64> {
    project_to_lines(ir, sal)
        .into_iter()
        .flat_map(|s| (s.start..=s.end).map(move |line| (line, s.score)))
        .collect()
}

/// One line per span.
fn print_text(sidecar: &vikt_core::Sidecar) {
    for f in &sidecar.functions {
        println!(
            "\n{} {}  [{}/{} instructions carry a line]",
            f.name, f.signature, f.coverage.with_line, f.coverage.instructions
        );
        for s in &f.spans {
            let range = if s.start == s.end {
                format!("{}", s.start)
            } else {
                format!("{}-{}", s.start, s.end)
            };
            // Empty when the corresponding scope wasn't requested, so
            // default output stays byte-identical: each of `--scope
            // file`/`--scope repo`'s only text-format footprint is its one
            // extra column, present exactly where its score is `Some` —
            // and the two are mutually exclusive, since only one scope
            // ever runs per invocation.
            let file_score = s
                .file_score
                .map(|v| format!(" file {v:.2}"))
                .unwrap_or_default();
            let repo_score = s
                .repo_score
                .map(|v| format!(" repo {v:.2}"))
                .unwrap_or_default();
            println!(
                "  {range:>9}  {:<9} {:.2}{file_score}{repo_score}  {}",
                s.tier,
                s.function_score,
                s.reasons.first().map_or("", String::as_str)
            );
        }
    }
}

/// Best-effort source read for embedding [`vikt_core::SpanRecord::text`]:
/// `None` on any I/O or UTF-8 failure rather than propagating it - unlike
/// `--annotate`'s own read below, this is speculative (tried against every
/// function's `ir.id.file`, most of which are never `--annotate`'s target)
/// and its absence is itself meaningful output (`text` simply stays unset),
/// not an error.
fn read_source(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The source with a tier marker per line, plus a file- or repo-scope score
/// column matching `scope`.
fn annotate(source: &Path, sidecar: &vikt_core::Sidecar, scope: Scope) -> std::io::Result<()> {
    let text = std::fs::read_to_string(source)?;
    println!("\n--- {} ---", source.display());
    for (i, line) in text.lines().enumerate() {
        let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let marker = match sidecar.tier_at(n) {
            Some("core") => "CORE ",
            Some("boundary") => "BOUND",
            Some("plumbing") => "plumb",
            Some("inert") => "inert",
            _ => "     ",
        };
        match scope {
            Scope::Function => println!("{marker} {n:>4} | {line}"),
            Scope::File => {
                let file_score = sidecar
                    .file_score_at(n)
                    .map_or_else(|| "     ".to_owned(), |v| format!("{v:>5.2}"));
                println!("{marker} {file_score} {n:>4} | {line}");
            }
            Scope::Repo => {
                let repo_score = sidecar
                    .repo_score_at(n)
                    .map_or_else(|| "     ".to_owned(), |v| format!("{v:>5.2}"));
                println!("{marker} {repo_score} {n:>4} | {line}");
            }
        }
    }
    Ok(())
}

/// Histogram and timing.
fn print_stats(
    sidecar: &vikt_core::Sidecar,
    analyzed: usize,
    total: usize,
    lowering: std::time::Duration,
    analysis: std::time::Duration,
    functions: &[FnInput],
) {
    let mut core = 0;
    let mut boundary = 0;
    let mut plumbing = 0;
    let mut inert = 0;
    for f in &sidecar.functions {
        core += f.summary.core;
        boundary += f.summary.boundary;
        plumbing += f.summary.plumbing;
        inert += f.summary.inert;
    }
    let lines = core + boundary + plumbing + inert;
    let instructions: usize = functions.iter().map(|fi| fi.ir.len()).sum();
    let with_line: usize = sidecar.functions.iter().map(|f| f.coverage.with_line).sum();

    eprintln!("\n--- stats ---");
    eprintln!("functions   {analyzed} analyzed / {total} lowered");
    eprintln!("instructions {instructions}, {with_line} with a source line");
    eprintln!("lines       {lines} tiered");
    if lines > 0 {
        let pct = |x: usize| (x as f64) * 100.0 / (lines as f64);
        eprintln!("  core      {core:>5}  {:.1}%", pct(core));
        eprintln!("  boundary  {boundary:>5}  {:.1}%", pct(boundary));
        eprintln!("  plumbing  {plumbing:>5}  {:.1}%", pct(plumbing));
        eprintln!("  inert     {inert:>5}  {:.1}%", pct(inert));
    }
    // Reported apart because they are paid at different times and by
    // different people. Lowering is I/O plus a parse — for Python it is a whole
    // interpreter subprocess — and happens once per file. Analysis is the part
    // that would run inside an editor hook, and the part the caching story is
    // about.
    eprintln!("lowering    {lowering:?}");
    eprintln!("analysis    {analysis:?}");
    if analyzed > 0 {
        eprintln!(
            "            {:?} per function",
            analysis / u32::try_from(analyzed).unwrap_or(1)
        );
    }
}
