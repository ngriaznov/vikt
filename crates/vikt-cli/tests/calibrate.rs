//! The calibrate subcommand through the built binary, over the checked-in
//! fixture project. Skips rather than fails when python3 is missing.
//!
//! The fixture is designed to make the verdict stable, not merely possible:
//! every function keeps its dead bookkeeping early and its behaviour late, so
//! the positional null is reliably wrong while the panel is reliably right,
//! and the whole pipeline — mutants, test runs, sampling — is deterministic
//! by construction. If this test ever flakes, the fixture has drifted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const FIXTURE: &str = "tests/fixtures/calibrate";

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every file under `root`, by relative path, byte for byte.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("fixture directory is readable") {
            let path = entry.expect("fixture entry is readable").path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .expect("path is under root")
                        .to_path_buf(),
                    std::fs::read(&path).expect("fixture file is readable"),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The full pipeline: the run completes, reports both correlations and a
/// verdict, the panel beats the positional null on the designed fixture, and
/// the input tree comes out byte-identical — calibration mutates only its
/// temporary copy, never the tree it was pointed at.
#[test]
fn calibrates_the_fixture_and_leaves_it_untouched() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let before = snapshot(Path::new(FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -m unittest discover",
            "--gate",
            "--scope",
            "function",
        ])
        .output()
        .expect("running the vikt binary");
    let after = snapshot(Path::new(FIXTURE));
    assert_eq!(
        before, after,
        "calibrate must leave the input tree byte-identical"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected the gate to pass on the designed fixture\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("pooled Spearman rho"), "stdout:\n{stdout}");
    assert!(stdout.contains("positional null"), "stdout:\n{stdout}");
    // The fixture separates dead-early from live-late hard enough that the
    // full verdict — not just "beats the null" — is stable.
    assert!(stdout.contains("verdict: calibrated"), "stdout:\n{stdout}");

    // The two pooled rho values, read back from the report: the panel must
    // beat the positional null on this fixture by construction.
    let rho = |label: &str| -> f64 {
        stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix(label))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("no `{label}` rho in stdout:\n{stdout}"))
    };
    let panel = rho("panel");
    let null = rho("positional null");
    assert!(
        panel > null,
        "panel rho {panel} must beat the positional null {null}"
    );
}

/// `--scope file`: the run completes, announces the file-scope notice, and
/// still reaches a verdict. `mathops.py`'s `total_with_tax` wraps `checkout`
/// for exactly this — a hub called by a wrapper in the same file — so the
/// file-scope call graph has an edge to weight functions by, not just two
/// functions with nothing between them.
#[test]
fn scope_file_runs_and_announces_itself() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -m unittest discover",
            "--scope",
            "file",
        ])
        .output()
        .expect("running the vikt binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("scope file"),
        "the file-scope notice must be printed:\n{stdout}"
    );
    assert!(stdout.contains("pooled Spearman rho"), "stdout:\n{stdout}");
    assert!(stdout.contains("verdict:"), "stdout:\n{stdout}");
}

/// `--scope repo`: the run completes, announces the repo-scope notice — the
/// cross-file generalisation of the `--scope file` one above — and still
/// reaches a verdict.
#[test]
fn scope_repo_runs_and_announces_itself() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -m unittest discover",
            "--scope",
            "repo",
        ])
        .output()
        .expect("running the vikt binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("scope repo"),
        "the repo-scope notice must be printed:\n{stdout}"
    );
    assert!(stdout.contains("pooled Spearman rho"), "stdout:\n{stdout}");
    assert!(stdout.contains("verdict:"), "stdout:\n{stdout}");
}

/// Starving the run of budget lands under the mutant floor, and with `--gate`
/// that is exit code 2: "not enough data" is a different answer from
/// "uncalibrated" (3), and a CI job must be able to tell them apart.
#[test]
fn gate_reports_insufficient_data_as_exit_2() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -m unittest discover",
            "--budget",
            "5",
            "--gate",
        ])
        .output()
        .expect("running the vikt binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "stdout:\n{stdout}");
    assert!(
        stdout.contains("verdict: insufficient data"),
        "stdout:\n{stdout}"
    );
    // Five mutants against far more candidate sites: the truncation must be
    // announced, never silent.
    assert!(
        stdout.contains("budget of 5 mutants reached"),
        "stdout:\n{stdout}"
    );
}

/// A test command that fails on the unmutated tree aborts before any mutant
/// runs: kill rates measured against a broken baseline would be noise.
#[test]
fn failing_baseline_aborts() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -c \"import sys; sys.exit(1)\"",
        ])
        .output()
        .expect("running the vikt binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unmutated"), "stderr:\n{stderr}");
}

/// A baseline that hangs is killed as a whole process group, not just its
/// `sh` wrapper: mutation makes infinite loops routinely, so a forking test
/// command must not leak live grandchildren that keep running against the
/// shared copy after the timeout. The command backgrounds a sleeper, records
/// its pid outside the doomed copy, and hangs; after the run aborts, the
/// sleeper must be gone.
#[test]
fn timeout_kills_the_whole_process_group() {
    let pidfile =
        std::env::temp_dir().join(format!("vikt-calibrate-pgkill-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args(["calibrate", FIXTURE, "--timeout-secs", "1", "--test-cmd"])
        .arg(format!("sleep 300 & echo $! > {}; wait", pidfile.display()))
        .output()
        .expect("running the vikt binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("exceeded"), "stderr:\n{stderr}");

    let pid = std::fs::read_to_string(&pidfile)
        .expect("the test command recorded the grandchild pid")
        .trim()
        .to_owned();
    let _ = std::fs::remove_file(&pidfile);
    // SIGKILL delivery is not synchronous with the binary's exit; poll
    // briefly before declaring the grandchild leaked.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let alive = Command::new("sh")
            .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status()
            .expect("probing the grandchild")
            .success();
        if !alive {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sleep grandchild (pid {pid}) survived the timeout kill"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The temporary copy carries dot-prefixed files and materialises symlinked
/// sources: a suite is entitled to its `.coveragerc`-style configuration,
/// and a linked module must lower and run rather than silently vanish. The
/// test command itself asserts both are present in the copy.
#[test]
fn copy_keeps_dot_files_and_follows_symlinks() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vikt-calibrate-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    std::fs::write(
        dir.join("real_impl.py"),
        "def scale(value):\n    total = value + 1\n    return total * 2\n",
    )
    .expect("temp file");
    std::fs::write(dir.join(".coveragerc"), "[run]\n").expect("temp file");
    std::os::unix::fs::symlink(dir.join("real_impl.py"), dir.join("linked.py"))
        .expect("symlink in temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "test -f .coveragerc && test -f linked.py"])
        .output()
        .expect("running the vikt binary");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("baseline test command passed"),
        "stdout:\n{stdout}"
    );
}

/// A tree with sources for a frontend calibrate does not support at all gets
/// the honest scope error, not a generic "nothing found". No interpreter or
/// oxc parse involved: the check runs on file extensions alone, before
/// any engine touches the tree. JVM sources are the one frontend family
/// calibrate has never covered.
#[test]
fn unsupported_frontend_trees_are_rejected() {
    let dir = std::env::temp_dir().join(format!("vikt-calibrate-scope-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    std::fs::write(dir.join("App.java"), "class App {}\n").expect("temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "true"])
        .output()
        .expect("running the vikt binary");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("contains neither"), "stderr:\n{stderr}");
}

/// Loose `.rs` files without a `Cargo.toml` cannot run a suite, and the
/// error must say what the fix is — point at the package root — rather than
/// pretending the tree is empty.
#[test]
fn rust_sources_without_a_manifest_get_directed_to_the_package_root() {
    let dir = std::env::temp_dir().join(format!("vikt-calibrate-norust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    std::fs::write(dir.join("app.rs"), "fn main() {}\n").expect("temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "true"])
        .output()
        .expect("running the vikt binary");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no Cargo.toml"), "stderr:\n{stderr}");
}

/// `--emit-dataset`: one JSON object per scored, mutated line, carrying
/// exactly the seven panel features, sorted by (file, function, line), row
/// count matching what the run itself reported. The fixture run is
/// deterministic, so this pins the dataset against the verdict pipeline —
/// both must always measure the identical set of lines.
#[test]
fn emit_dataset_writes_consistent_jsonl() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dataset =
        std::env::temp_dir().join(format!("vikt-calibrate-ds-{}.jsonl", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_vikt"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -m unittest discover",
        ])
        .arg("--emit-dataset")
        .arg(&dataset)
        .output()
        .expect("running the vikt binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "vikt failed:\n{stdout}");
    let text = std::fs::read_to_string(&dataset).expect("dataset file exists");
    let _ = std::fs::remove_file(&dataset);

    let rows: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("every dataset line parses as JSON"))
        .collect();
    assert!(!rows.is_empty());
    // Field order is a property of the emitted text (serde struct order),
    // which a parsed map may not preserve — assert it on the raw line.
    let first = text.lines().next().unwrap();
    let positions: Vec<usize> = [
        "current", "schur", "pivot", "trophic", "strahler", "position", "boundary",
    ]
    .iter()
    .map(|k| {
        first
            .find(&format!("\"{k}\""))
            .expect("feature key present")
    })
    .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "instrument keys must be emitted in weight order: {first}"
    );
    let reported: usize = stdout
        .lines()
        .find_map(|l| {
            l.strip_prefix("dataset: wrote ")
                .and_then(|r| r.split(' ').next())
                .and_then(|n| n.parse().ok())
        })
        .expect("the run must report the dataset row count");
    assert_eq!(rows.len(), reported);

    let mut keys_prev: Option<(String, String, u64)> = None;
    for row in &rows {
        let inst = row["instruments"]
            .as_object()
            .expect("instruments is an object");
        let mut keys: Vec<_> = inst.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "boundary", "current", "pivot", "position", "schur", "strahler", "trophic"
            ],
            "exactly the seven panel features"
        );
        let panel = row["panel"].as_f64().expect("panel is a number");
        assert!((0.0..=1.0).contains(&panel));
        assert_eq!(row["language"], "python");
        assert_eq!(row["profile"], "statement");

        // Additive fields: present regardless of `--scope`, which this run
        // never passed.
        let features = row["function_features"]
            .as_object()
            .expect("function_features is an object");
        let mut feature_keys: Vec<_> = features.keys().map(String::as_str).collect();
        feature_keys.sort_unstable();
        assert_eq!(
            feature_keys,
            ["boundary_density", "fan_in", "size_share", "trophic"],
            "exactly the four filescope function features"
        );
        for key in ["boundary_density", "fan_in", "size_share", "trophic"] {
            let v = features[key]
                .as_f64()
                .unwrap_or_else(|| panic!("function_features.{key} is a number"));
            assert!(
                (0.0..=1.0).contains(&v),
                "function_features.{key} = {v} out of [0, 1]"
            );
        }
        let file_score = row["file_score"].as_f64().expect("file_score is a number");
        assert!(
            (0.0..=1.0).contains(&file_score),
            "file_score {file_score} out of [0, 1]"
        );
        let key = (
            row["file"].as_str().unwrap().to_owned(),
            row["function"].as_str().unwrap().to_owned(),
            row["line"].as_u64().unwrap(),
        );
        if let Some(prev) = &keys_prev {
            assert!(
                *prev <= key,
                "rows must be sorted by (file, function, line)"
            );
        }
        keys_prev = Some(key);
    }
}
