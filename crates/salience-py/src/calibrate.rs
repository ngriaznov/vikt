//! Mutant generation for `salience calibrate`: `.py` source in, mutated
//! sources out.
//!
//! Same shape as the lowering in the crate root, for the same reason: the
//! mutation operators work on the `ast` module's view of the source, and that
//! view belongs to the interpreter — see `src/calibrate.py`, embedded at
//! compile time and run as a subprocess of whatever interpreter the caller
//! points at.

use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Deserialize;

use crate::PyError;

/// The mutation script, embedded so the crate is self-contained.
const CALIBRATE_PY: &str = include_str!("calibrate.py");

/// Per-process sequence for the temp script's name. The pid alone is not
/// unique here: callers run concurrently on threads of one process (the test
/// harness does), and under a shared name one call's `remove_file` can land
/// between another's write and the interpreter opening the script.
static SCRIPT_SEQ: AtomicUsize = AtomicUsize::new(0);

/// One line-targeted mutant: the whole file with a single AST edit applied.
///
/// Derives `Deserialize` directly rather than through a mirror struct: the
/// mirror pattern in the crate root exists to keep salience-core free of a
/// serialization contract, and no core type is involved here.
#[derive(Debug, Clone, Deserialize)]
pub struct Mutant {
    /// Line of the edit in the *original* source — the line the panel scored.
    /// The mutated text below is re-serialized and its own line numbers drift.
    pub line: u32,
    /// Operator family: `cmp`, `bin`, `aug`, `bool`, `const` or `delete`.
    pub kind: String,
    /// Human-readable edit, e.g. `GtE -> Gt`.
    pub detail: String,
    /// Full mutated file content, ready to write over the copy.
    pub source: String,
}

/// What the generator found and what it emitted.
#[derive(Debug, Clone, Deserialize)]
pub struct MutantSet {
    /// Sites found inside the requested spans, before the cap. When this
    /// exceeds `mutants.len()`, the caller truncated and must say so.
    pub total_sites: usize,
    /// At most the requested number of mutants, sorted by line.
    pub mutants: Vec<Mutant>,
}

/// Generates up to `limit` mutants whose sites fall inside `spans`
/// (inclusive line ranges). `limit == 0` counts sites without emitting.
///
/// # Errors
///
/// See [`PyError`]. A file the interpreter cannot parse surfaces as
/// [`PyError::Failed`] carrying the `SyntaxError`.
pub fn mutants_for(
    path: &Path,
    spans: &[(u32, u32)],
    limit: usize,
    interpreter: &str,
) -> Result<MutantSet, PyError> {
    if spans.is_empty() {
        // Nothing to target; the script's span grammar has no empty form.
        return Ok(MutantSet {
            total_sites: 0,
            mutants: Vec::new(),
        });
    }
    // Same temp-file-not-`-c` choice as the lowering, so tracebacks carry
    // real line numbers when the script is being worked on; unlike the
    // lowering, the name carries [`SCRIPT_SEQ`] as well as the pid.
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut script = std::env::temp_dir();
    script.push(format!(
        "salience-calibrate-{}-{seq}.py",
        std::process::id()
    ));
    {
        let mut fh = std::fs::File::create(&script)?;
        fh.write_all(CALIBRATE_PY.as_bytes())?;
    }

    let spans_arg = spans
        .iter()
        .map(|(lo, hi)| format!("{lo}-{hi}"))
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new(interpreter)
        .arg(&script)
        .arg(path)
        .arg(&spans_arg)
        .arg(limit.to_string())
        .output();
    let _ = std::fs::remove_file(&script);
    let output = output?;

    if !output.status.success() {
        return Err(PyError::Failed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(serde_json::from_slice(&output.stdout)?)
}
