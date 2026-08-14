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
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -m unittest discover",
            "--gate",
        ])
        .output()
        .expect("running the salience binary");
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

/// Starving the run of budget lands under the mutant floor, and with `--gate`
/// that is exit code 2: "not enough data" is a different answer from
/// "uncalibrated" (3), and a CI job must be able to tell them apart.
#[test]
fn gate_reports_insufficient_data_as_exit_2() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
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
        .expect("running the salience binary");
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
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args([
            "calibrate",
            FIXTURE,
            "--test-cmd",
            "python3 -c \"import sys; sys.exit(1)\"",
        ])
        .output()
        .expect("running the salience binary");
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
    let pidfile = std::env::temp_dir().join(format!(
        "salience-calibrate-pgkill-{}.pid",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&pidfile);
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args(["calibrate", FIXTURE, "--timeout-secs", "1", "--test-cmd"])
        .arg(format!("sleep 300 & echo $! > {}; wait", pidfile.display()))
        .output()
        .expect("running the salience binary");
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
    let dir = std::env::temp_dir().join(format!("salience-calibrate-links-{}", std::process::id()));
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
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "test -f .coveragerc && test -f linked.py"])
        .output()
        .expect("running the salience binary");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("baseline test command passed"),
        "stdout:\n{stdout}"
    );
}

/// A tree with sources for another frontend gets the honest scope error, not
/// a generic "nothing found". No interpreter involved: the check runs before
/// any Python is spawned.
#[test]
fn non_python_trees_are_rejected() {
    let dir = std::env::temp_dir().join(format!("salience-calibrate-scope-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    std::fs::write(dir.join("app.js"), "export const answer = 42;\n").expect("temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "true"])
        .output()
        .expect("running the salience binary");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Python sources only"), "stderr:\n{stderr}");
}
