//! The calibrate subcommand's JavaScript/TypeScript engine, through the
//! built binary, over the checked-in JS fixture project plus a handful of
//! synthetic trees for the mixed-language, `node_modules` and TypeScript
//! behaviors. Skips rather than fails when the relevant toolchain (`node`,
//! `python3`) is missing.
//!
//! The JS fixture mirrors `tests/fixtures/calibrate`'s design exactly: every
//! function keeps its dead bookkeeping early and its behaviour late, so the
//! positional null is reliably wrong while the panel is reliably right. If
//! this test ever flakes, the fixture has drifted — see
//! `tests/calibrate.rs`'s module docs for the same note on the Python side.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Integration tests run with the crate root as the working directory.
const JS_FIXTURE: &str = "tests/fixtures/calibrate-js";

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

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

/// The full pipeline over the JS fixture: the run completes, reports both
/// correlations and a verdict, the panel beats the positional null, and the
/// input tree comes out byte-identical.
#[test]
fn calibrates_a_javascript_tree_and_leaves_it_untouched() {
    if !node_available() {
        eprintln!("skipping: node not on PATH");
        return;
    }
    let before = snapshot(Path::new(JS_FIXTURE));
    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args([
            "calibrate",
            JS_FIXTURE,
            "--test-cmd",
            "node --test",
            "--gate",
        ])
        .output()
        .expect("running the salience binary");
    let after = snapshot(Path::new(JS_FIXTURE));
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
    assert!(stdout.contains("verdict: calibrated"), "stdout:\n{stdout}");

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

/// A directory with both Python and JavaScript/TypeScript sources is
/// calibrated in whichever scored more lines, and the run says which and by
/// how much — never a silent choice.
#[test]
fn mixed_language_trees_pick_the_larger_and_say_so() {
    if !python_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("salience-calibrate-mixed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    // A real multi-branch function, several scored lines.
    std::fs::write(
        dir.join("big.py"),
        "def clamp(values):\n    out = []\n    for v in values:\n        if v < 0:\n            v = 0\n        if v > 100:\n            v = 100\n        out.append(v)\n    return out\n",
    )
    .expect("temp file");
    // A single-line arrow function: far fewer scored lines than big.py.
    std::fs::write(dir.join("tiny.js"), "const id = (x) => x;\n").expect("temp file");

    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "true"])
        .output()
        .expect("running the salience binary");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("both Python and JavaScript/TypeScript sources found"),
        "stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("calibrating Python only"),
        "stdout:\n{stdout}"
    );
}

/// `node_modules` is staged into the copy (a `require`/`import` inside it
/// must resolve) but never scored or mutated — no mutant ever names a path
/// inside it, and the run reports the directory as linked in unscored.
#[test]
fn node_modules_is_present_but_never_scored_or_mutated() {
    if !node_available() {
        eprintln!("skipping: node not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "salience-calibrate-nodemods-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("node_modules/leftpad")).expect("temp dir is writable");
    std::fs::write(
        dir.join("node_modules/leftpad/index.js"),
        "module.exports = function bump(x) {\n  const unused = 1 + 1;\n  return x + 1;\n};\n",
    )
    .expect("temp file");
    std::fs::write(
        dir.join("app.js"),
        "const bump = require(\"./node_modules/leftpad\");\nfunction twice(x) {\n  const scale = 2 * 1;\n  if (x < 0) {\n    return 0;\n  }\n  return bump(bump(x));\n}\nmodule.exports = { twice };\n",
    )
    .expect("temp file");
    std::fs::create_dir_all(dir.join("test")).expect("temp dir is writable");
    std::fs::write(
        dir.join("test/app.test.js"),
        "const test = require(\"node:test\");\nconst assert = require(\"node:assert/strict\");\nconst { twice } = require(\"../app.js\");\ntest(\"twice adds two\", () => {\n  assert.equal(twice(3), 5);\n  assert.equal(twice(-1), 0);\n});\n",
    )
    .expect("temp file");

    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "node --test", "--budget", "20"])
        .output()
        .expect("running the salience binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("node_modules directory linked in unscored"),
        "stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("node_modules"),
        "no mutant may target a file inside node_modules\nstderr:\n{stderr}"
    );
}

/// A TypeScript source in the scored set prints the type-check caveat: a
/// mutant that fails to type-check reads as killed by the repository's own
/// toolchain, the same as one an actual test caught.
#[test]
fn typescript_sources_print_the_type_check_caveat() {
    if !node_available() {
        eprintln!("skipping: node not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("salience-calibrate-ts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    std::fs::write(
        dir.join("clamp.ts"),
        "export function clamp(value: number): number {\n  const scale: number = 1 * 1;\n  if (value < 0) {\n    return 0;\n  }\n  if (value > 100) {\n    return 100;\n  }\n  return value;\n}\n",
    )
    .expect("temp file");

    let out = Command::new(env!("CARGO_BIN_EXE_salience"))
        .args(["calibrate"])
        .arg(&dir)
        .args(["--test-cmd", "true", "--budget", "5"])
        .output()
        .expect("running the salience binary");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("TypeScript sources among the scored files"),
        "stdout:\n{stdout}"
    );
}
