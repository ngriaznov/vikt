//! Batch evaluation over a real corpus.
//!
//! Not part of the shipped tool: this exists to answer three questions the
//! unit tests cannot: does it survive real code, how fast is it at scale, and
//! what does the tier distribution actually look like when nobody constructed
//! the input.
//!
//! ```text
//! cargo run --release --example evaluate -- jvm  <dir-of-classes>
//! cargo run --release --example evaluate -- py   <dir-of-py-files>
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use salience_core::ir::FunctionIr;
use salience_core::{Denylist, ScoreWeights, Scorer, Sidecar, analyze_with_scorer};

#[derive(Default)]
struct Stats {
    files_seen: usize,
    files_failed: usize,
    files_panicked: usize,
    functions: usize,
    functions_empty: usize,
    functions_invalid: usize,
    /// Bodies over the size guard - generated data tables, not logic.
    functions_skipped_huge: usize,
    instructions: usize,
    instructions_with_line: usize,
    /// Functions whose instructions carried no line at all.
    functions_no_lines: usize,
    lines_core: usize,
    lines_boundary: usize,
    lines_plumbing: usize,
    lines_inert: usize,
    analysis_time: Duration,
    /// Per-function instruction counts, for percentiles.
    sizes: Vec<usize>,
    /// Per-function analysis nanoseconds, for percentiles.
    times_ns: Vec<u128>,
    failures: BTreeMap<String, usize>,
    /// Tier counts bucketed by function size, to see whether discrimination
    /// depends on how much body there is to discriminate.
    buckets: BTreeMap<&'static str, [usize; 4]>,
    /// (nanos, instructions, name) for the slowest functions.
    slowest: Vec<(u128, usize, String)>,
}

fn bucket_of(n: usize) -> &'static str {
    match n {
        0..=9 => "1  tiny   (<10)",
        10..=29 => "2  small  (10-29)",
        30..=99 => "3  medium (30-99)",
        100..=299 => "4  large  (100-299)",
        _ => "5  huge   (300+)",
    }
}

impl Stats {
    fn record(&mut self, ir: &FunctionIr) {
        self.functions += 1;
        if ir.is_empty() {
            self.functions_empty += 1;
            return;
        }
        if ir.validate().is_err() {
            self.functions_invalid += 1;
            return;
        }
        // Mirrors the CLI's --max-instructions default: every body ever seen
        // over this size was a generated module-level data table.
        if ir.len() > 4096 {
            self.functions_skipped_huge += 1;
            return;
        }
        self.instructions += ir.len();
        let with_line = ir.nodes.iter().filter(|n| n.line.is_some()).count();
        self.instructions_with_line += with_line;
        if with_line == 0 {
            self.functions_no_lines += 1;
        }
        self.sizes.push(ir.len());

        let started = Instant::now();
        // SALIENCE_SCORER=current measures the incumbent alone; default is the
        // shipped panel, so the numbers reported are the numbers users get.
        // This harness only ever runs over jvm/py corpora (see the module
        // doc), both bytecode-granular, so the instruction profile is
        // correct unconditionally here - no extension dispatch needed.
        let scorer = match std::env::var("SALIENCE_SCORER").as_deref() {
            Ok("current") => Scorer::Current,
            _ => Scorer::Panel(salience_core::PanelProfile::Instruction),
        };
        let sal = analyze_with_scorer(ir, &Denylist::new(), &ScoreWeights::default(), scorer);
        let elapsed = started.elapsed();
        self.analysis_time += elapsed;
        self.times_ns.push(elapsed.as_nanos());

        let mut side = Sidecar::new(ir.id.file.clone(), "evaluate");
        side.push(ir, &sal);
        let s = &side.functions[0].summary;
        self.lines_core += s.core;
        self.lines_boundary += s.boundary;
        self.lines_plumbing += s.plumbing;
        self.lines_inert += s.inert;

        let b = self.buckets.entry(bucket_of(ir.len())).or_insert([0; 4]);
        b[0] += s.core;
        b[1] += s.boundary;
        b[2] += s.plumbing;
        b[3] += s.inert;

        self.slowest
            .push((elapsed.as_nanos(), ir.len(), ir.id.name.clone()));
        if self.slowest.len() > 512 {
            self.slowest.sort_by_key(|t| std::cmp::Reverse(t.0));
            self.slowest.truncate(16);
        }
    }

    fn report(&self, label: &str) {
        let pct = |x: usize, of: usize| {
            if of == 0 {
                0.0
            } else {
                x as f64 * 100.0 / of as f64
            }
        };
        let lines = self.lines_core + self.lines_boundary + self.lines_plumbing + self.lines_inert;
        println!("\n=================== {label} ===================");
        println!(
            "files        {} seen, {} failed to parse, {} panicked",
            self.files_seen, self.files_failed, self.files_panicked
        );
        println!(
            "functions    {} total, {} empty, {} malformed, {} skipped as oversized data tables, {} with no line info",
            self.functions,
            self.functions_empty,
            self.functions_invalid,
            self.functions_skipped_huge,
            self.functions_no_lines
        );
        println!(
            "instructions {} total, {} with a line ({:.1}%)",
            self.instructions,
            self.instructions_with_line,
            pct(self.instructions_with_line, self.instructions)
        );
        println!("lines tiered {lines}");
        println!(
            "  core       {:>7}  {:>5.1}%",
            self.lines_core,
            pct(self.lines_core, lines)
        );
        println!(
            "  boundary   {:>7}  {:>5.1}%",
            self.lines_boundary,
            pct(self.lines_boundary, lines)
        );
        println!(
            "  plumbing   {:>7}  {:>5.1}%",
            self.lines_plumbing,
            pct(self.lines_plumbing, lines)
        );
        println!(
            "  inert      {:>7}  {:>5.1}%",
            self.lines_inert,
            pct(self.lines_inert, lines)
        );

        let mut sizes = self.sizes.clone();
        sizes.sort_unstable();
        let mut times = self.times_ns.clone();
        times.sort_unstable();
        let p = |v: &[u128], q: f64| -> u128 {
            if v.is_empty() {
                return 0;
            }
            v[((v.len() - 1) as f64 * q) as usize]
        };
        let ps = |v: &[usize], q: f64| -> usize {
            if v.is_empty() {
                return 0;
            }
            v[((v.len() - 1) as f64 * q) as usize]
        };
        println!(
            "size         p50={} p90={} p99={} max={} instructions",
            ps(&sizes, 0.50),
            ps(&sizes, 0.90),
            ps(&sizes, 0.99),
            sizes.last().copied().unwrap_or(0)
        );
        println!(
            "analysis     total {:?}, mean {:.1}us, p50={:.1}us p90={:.1}us p99={:.1}us max={:.1}us",
            self.analysis_time,
            self.analysis_time.as_secs_f64() * 1e6 / self.times_ns.len().max(1) as f64,
            p(&times, 0.50) as f64 / 1000.0,
            p(&times, 0.90) as f64 / 1000.0,
            p(&times, 0.99) as f64 / 1000.0,
            times.last().copied().unwrap_or(0) as f64 / 1000.0,
        );
        println!("tier mix by function size (core / boundary / plumbing / inert):");
        for (name, b) in &self.buckets {
            let t: usize = b.iter().sum();
            println!(
                "  {name:<18} {:>6} lines   {:>5.1}% {:>5.1}% {:>5.1}% {:>5.1}%",
                t,
                pct(b[0], t),
                pct(b[1], t),
                pct(b[2], t),
                pct(b[3], t)
            );
        }
        let mut slow = self.slowest.clone();
        slow.sort_by_key(|t| std::cmp::Reverse(t.0));
        println!("slowest functions:");
        for (ns, size, name) in slow.into_iter().take(6) {
            println!(
                "  {:>10.1}ms  {:>6} instructions  {name}",
                ns as f64 / 1e6,
                size
            );
        }
        if !self.failures.is_empty() {
            println!("failure modes:");
            let mut f: Vec<_> = self.failures.iter().collect();
            f.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            for (msg, n) in f.into_iter().take(8) {
                println!("  {n:>5}x {msg}");
            }
        }
    }
}

fn walk(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("usage: evaluate <jvm|py|js> <dir>");
    let root = PathBuf::from(args.next().expect("usage: evaluate <jvm|py> <dir>"));
    let limit: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    let mut stats = Stats::default();
    let wall = Instant::now();

    match mode.as_str() {
        "jvm" => {
            for path in walk(&root, "class").into_iter().take(limit) {
                stats.files_seen += 1;
                let Ok(bytes) = std::fs::read(&path) else {
                    stats.files_failed += 1;
                    continue;
                };
                // A panic in one class must not end the run — finding out *that*
                // it panics is part of what this is for.
                let lowered = std::panic::catch_unwind(|| salience_jvm::lower_class(&bytes));
                match lowered {
                    Err(_) => {
                        stats.files_panicked += 1;
                        *stats
                            .failures
                            .entry(format!("panic in {}", path.display()))
                            .or_default() += 1;
                    }
                    Ok(Err(e)) => {
                        stats.files_failed += 1;
                        *stats.failures.entry(e.to_string()).or_default() += 1;
                    }
                    Ok(Ok(c)) => {
                        for ir in &c.functions {
                            stats.record(ir);
                        }
                    }
                }
            }
        }
        "js" => {
            for path in walk(&root, "js")
                .into_iter()
                .chain(walk(&root, "ts"))
                .take(limit)
            {
                stats.files_seen += 1;
                match salience_js::lower_file(&path) {
                    Err(e) => {
                        stats.files_failed += 1;
                        let msg = e.to_string();
                        let short = msg.lines().next().unwrap_or("?").to_owned();
                        *stats.failures.entry(short).or_default() += 1;
                    }
                    Ok(m) => {
                        for ir in &m.functions {
                            stats.record(ir);
                        }
                    }
                }
            }
        }
        "py" => {
            for path in walk(&root, "py").into_iter().take(limit) {
                stats.files_seen += 1;
                match salience_py::lower_file(&path) {
                    Err(e) => {
                        stats.files_failed += 1;
                        let msg = e.to_string();
                        let short = msg.lines().next().unwrap_or("?").to_owned();
                        *stats.failures.entry(short).or_default() += 1;
                    }
                    Ok(m) => {
                        for ir in &m.functions {
                            stats.record(ir);
                        }
                    }
                }
            }
        }
        other => panic!("unknown mode {other}"),
    }

    stats.report(&format!("{mode} :: {}", root.display()));
    println!("wall         {:?}", wall.elapsed());
}
