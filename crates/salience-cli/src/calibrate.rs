//! `salience calibrate` — per-repo self-calibration by mutation testing.
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
//! Python only for now: mutation needs an AST round-trip and a test-command
//! convention, and the Python frontend is the one that ships both.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use salience_core::calibration::{
    MIN_EXECUTED_MUTANTS, MIN_SCORED_LINES, NULL_MARGIN, RHO_FLOOR, Verdict, spearman, verdict,
};
use salience_core::{
    Denylist, PanelProfile, ScoreWeights, Scorer, analyze_with_scorer, project_to_lines,
};
use salience_py::calibrate::Mutant;

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
/// code against this repository's tests.
const SKIP_DIRS: &[&str] = &["__pycache__", "node_modules", "target", "venv"];

/// Extensions that mark a tree as belonging to another frontend, for the
/// "Python only" error to fire instead of a puzzling "no sources found".
const OTHER_FRONTENDS: &[&str] = &[
    "class", "java", "kt", "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "rs",
];

#[derive(Debug, clap::Args)]
pub struct CalibrateArgs {
    /// Directory of Python sources. Calibration currently supports Python
    /// sources only; other frontends are rejected with an error.
    pub path: PathBuf,

    /// Command that runs the project's tests, executed with `sh -c` from the
    /// root of a temporary copy of the tree. Must pass on the unmutated tree.
    #[arg(long, value_name = "COMMAND")]
    pub test_cmd: String,

    /// Functions to sample, largest first (line count, then name — no
    /// randomness, so two runs sample identically).
    #[arg(long, value_name = "N", default_value_t = 12)]
    pub sample: usize,

    /// Total mutant budget across all sampled functions. Hitting it is
    /// reported, never silent.
    #[arg(long, value_name = "M", default_value_t = 150)]
    pub budget: usize,

    /// Timeout per test run, in seconds. A run that exceeds it is killed and
    /// the mutant counts as killed: a hung suite noticed the mutation.
    #[arg(long, value_name = "S", default_value_t = 60)]
    pub timeout_secs: u64,

    /// Gate on the verdict: exit 0 when calibrated or marginal, 2 on
    /// insufficient data, 3 when uncalibrated. Without this flag the exit
    /// code only reports whether the measurement itself ran.
    #[arg(long)]
    pub gate: bool,

    /// Interpreter used to lower sources, generate mutants — and, typically,
    /// referenced by the test command itself.
    #[arg(long, default_value = "python3")]
    pub python: String,
}

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
}

/// Highest panel score per line, per file — the same "most salient span
/// wins" reading the sidecar's `tier_at` uses.
type FileScores = BTreeMap<PathBuf, BTreeMap<u32, f64>>;

/// A mutant tied to the (relative) file it rewrites.
type FileMutant = (PathBuf, Mutant);

/// One test-suite run over a mutant.
#[derive(Clone, Copy)]
enum TestOutcome {
    Pass,
    Fail,
    Timeout,
}

pub fn run(args: &CalibrateArgs) -> Result<ExitCode, Box<dyn Error>> {
    if !args.path.is_dir() {
        return Err(format!(
            "calibrate takes a directory of Python sources, and {} is not a directory",
            args.path.display()
        )
        .into());
    }

    let scan = scan_tree(&args.path)?;
    if scan.py.is_empty() {
        return Err(if scan.other_frontend {
            format!(
                "calibration currently supports Python sources only, and {} contains none",
                args.path.display()
            )
        } else {
            format!("no Python sources found under {}", args.path.display())
        }
        .into());
    }

    // Everything from here on happens in the copy. The input tree is never
    // opened for writing; the integration tests hold this to byte-identity.
    let copy = TempTree::create(&args.path, &scan.files)?;
    println!(
        "calibrate: copied {} files to a temporary tree{}",
        scan.files.len(),
        if scan.skipped_large > 0 {
            format!(" ({} files over 8 MiB skipped)", scan.skipped_large)
        } else {
            String::new()
        }
    );

    let timeout = Duration::from_secs(args.timeout_secs);
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

    let (mut scored, file_scores) = score_tree(&copy.root, &scan.py, &args.python);

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

    let (mutants, candidates) = generate_mutants(&copy.root, sampled, args)?;
    if candidates > mutants.len() {
        println!(
            "calibrate: budget of {} mutants reached: {} of {candidates} candidate sites will run; raise --budget for full coverage",
            args.budget,
            mutants.len(),
        );
    } else {
        println!(
            "calibrate: {} mutants across the sampled functions (budget {})",
            mutants.len(),
            args.budget
        );
    }

    let tally = execute_mutants(&copy.root, &mutants, args, timeout)?;
    println!(
        "calibrate: {} mutants executed: {} killed, {} survived, {} timed out (timeouts count as killed)",
        tally.executed(),
        tally.killed,
        tally.survived,
        tally.timeouts
    );

    let v = judge(sampled, &file_scores, &tally);
    Ok(if args.gate {
        ExitCode::from(gate_code(v))
    } else {
        ExitCode::SUCCESS
    })
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

/// Lowers and panel-scores every non-test source in the copy. Test files are
/// excluded from scoring and mutation both: mutating the suite measures the
/// suite's self-checks, not the code the panel scored. A file the frontend
/// cannot lower is reported and skipped rather than aborting the run — one
/// broken scratch file should not block calibrating the rest of a tree.
fn score_tree(root: &Path, py: &[PathBuf], python: &str) -> (Vec<ScoredFn>, FileScores) {
    let mut scored = Vec::new();
    let mut file_scores = FileScores::new();
    for rel in py.iter().filter(|p| !is_test_path(p)) {
        let lowered = match salience_py::lower_file_with(&root.join(rel), python) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("calibrate: skipping {}: {e}", rel.display());
                continue;
            }
        };
        for ir in &lowered.functions {
            if ir.validate().is_err() || ir.is_empty() || ir.len() > MAX_INSTRUCTIONS {
                continue;
            }
            // `<module>`, `<lambda>` and comprehension code objects overlap
            // the extent of a real def; sampling them would double-count the
            // same lines and degrade the positional null into noise.
            if ir.id.name.contains('<') {
                continue;
            }
            let sal = analyze_with_scorer(
                ir,
                &Denylist::new(),
                &ScoreWeights::default(),
                Scorer::Panel(PanelProfile::Instruction),
            );
            let spans = project_to_lines(ir, &sal);
            let Some(lo) = spans.iter().map(|s| s.start).min() else {
                continue;
            };
            let hi = spans.iter().map(|s| s.end).max().unwrap_or(lo);
            let per_file = file_scores.entry(rel.clone()).or_default();
            let mut lines = 0;
            for s in &spans {
                for ln in s.start..=s.end {
                    lines += 1;
                    let e = per_file.entry(ln).or_insert(f64::MIN);
                    if s.score > *e {
                        *e = s.score;
                    }
                }
            }
            scored.push(ScoredFn {
                file: rel.clone(),
                name: ir.id.name.clone(),
                lo,
                hi,
                lines,
            });
        }
    }
    (scored, file_scores)
}

/// Generates line-targeted mutants for the sampled functions, file by file
/// in path order, capped by the budget. Returns the mutants and the uncapped
/// candidate-site count, so the caller can say when it truncated.
fn generate_mutants(
    root: &Path,
    sampled: &[ScoredFn],
    args: &CalibrateArgs,
) -> Result<(Vec<FileMutant>, usize), Box<dyn Error>> {
    let mut files: Vec<&PathBuf> = sampled.iter().map(|f| &f.file).collect();
    files.sort();
    files.dedup();
    let mut mutants: Vec<FileMutant> = Vec::new();
    let mut candidates = 0usize;
    for file in files {
        let mut spans: Vec<(u32, u32)> = sampled
            .iter()
            .filter(|f| &f.file == file)
            .map(|f| (f.lo, f.hi))
            .collect();
        spans.sort_unstable();
        let remaining = args.budget - mutants.len();
        let set =
            salience_py::calibrate::mutants_for(&root.join(file), &spans, remaining, &args.python)?;
        candidates += set.total_sites;
        mutants.extend(set.mutants.into_iter().map(|m| (file.clone(), m)));
    }
    Ok((mutants, candidates))
}

/// Kill/survive counts, overall and per mutated line of the original source.
#[derive(Default)]
struct Tally {
    killed: usize,
    survived: usize,
    timeouts: usize,
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

/// Pairs each mutated line with its panel score, its positional-null score
/// and its kill rate, prints the correlations, and renders the verdict.
fn judge(sampled: &[ScoredFn], file_scores: &FileScores, tally: &Tally) -> Verdict {
    // A line is attributed to the innermost sampled function containing it,
    // so nested defs do not inherit their parent's extent for the null.
    let mut pairs: Vec<(usize, f64, f64, f64)> = Vec::new(); // (fn, panel, null, kill rate)
    let mut unscored = 0usize;
    for ((file, line), (kills, total)) in &tally.per_line {
        let owner = sampled
            .iter()
            .enumerate()
            .filter(|(_, f)| &f.file == file && f.lo <= *line && *line <= f.hi)
            .min_by_key(|(_, f)| f.hi - f.lo)
            .map(|(i, _)| i);
        let score = file_scores.get(file).and_then(|m| m.get(line));
        let (Some(owner), Some(&score)) = (owner, score) else {
            unscored += 1;
            continue;
        };
        let f = &sampled[owner];
        let null = if f.hi > f.lo {
            1.0 - f64::from(line - f.lo) / f64::from(f.hi - f.lo)
        } else {
            1.0
        };
        let rate = *kills as f64 / *total as f64;
        pairs.push((owner, score, null, rate));
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

    let mut by_fn: BTreeMap<usize, Vec<(f64, f64)>> = BTreeMap::new();
    for &(owner, score, _, rate) in &pairs {
        by_fn.entry(owner).or_default().push((score, rate));
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

    let panel: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    let null: Vec<f64> = pairs.iter().map(|p| p.2).collect();
    let rates: Vec<f64> = pairs.iter().map(|p| p.3).collect();
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

/// What a walk of the input tree found. Paths are relative to the root and
/// sorted, so every downstream stage is order-deterministic.
struct TreeScan {
    files: Vec<PathBuf>,
    py: Vec<PathBuf>,
    other_frontend: bool,
    skipped_large: usize,
}

fn scan_tree(root: &Path) -> std::io::Result<TreeScan> {
    let mut scan = TreeScan {
        files: Vec::new(),
        py: Vec::new(),
        other_frontend: false,
        skipped_large: 0,
    };
    let mut visited = BTreeSet::new();
    walk(root, root, &mut scan, &mut visited)?;
    scan.files.sort();
    scan.py.sort();
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
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
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
            if ext == "py" {
                scan.py.push(rel.clone());
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
    fn create(src: &Path, files: &[PathBuf]) -> std::io::Result<Self> {
        let mut root = std::env::temp_dir();
        root.push(format!("salience-calibrate-{}", std::process::id()));
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
        Ok(tree)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// True for files that are part of the test suite rather than the code under
/// test: `test`/`tests` directories, `test_*.py`, `*_test.py`.
fn is_test_path(rel: &Path) -> bool {
    let in_test_dir = rel.parent().is_some_and(|p| {
        p.components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("test" | "tests")))
    });
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    in_test_dir || name.starts_with("test_") || name.ends_with("_test.py")
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
    let mut child = Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
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

/// SIGKILLs the process group led by `pid`. Signalling a group takes a
/// negative pid, which `Child::kill` cannot express and the workspace's
/// `unsafe_code = "deny"` keeps libc from providing, so the signal goes
/// through the shell's own `kill` — the run already depends on `sh`. The
/// caller still kills and reaps the direct child, which covers the window
/// where the shell exited before it could be signalled.
fn kill_group(pid: u32) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -s KILL -- -{pid}"))
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
}
