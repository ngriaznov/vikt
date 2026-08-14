//! Rust frontend: real MIR, through a quarantined nightly.
//!
//! The faithful substrate for Rust is MIR - it is where the compiler has
//! already desugared `?`, loops, pattern matches and drops into honest
//! control flow, with spans intact. The only sanctioned road to MIR from
//! outside the compiler is `rustc_public`, and that road is nightly-only
//! even to read the data structures. This crate is how the tool stays on
//! stable anyway: the nightly requirement is quarantined inside one helper
//! binary, `tools/rust-lower`, pinned by its own rust-toolchain.toml, which
//! this crate spawns as a subprocess speaking the same neutral JSON contract
//! the Python frontend proved out. The main workspace never links a compiler
//! internal.
//!
//! # Finding the helper
//!
//! In order: the `SALIENCE_RUST_LOWER` environment variable, then
//! `salience-rust-lower` on `PATH`, then the debug and release build
//! locations under `tools/rust-lower/target` relative to the current
//! directory (the developer case). If none exists the error says how to
//! build it - one `cargo build` inside `tools/rust-lower`, where rustup
//! auto-fetches the pinned toolchain.
//!
//! # What the lowering looks like
//!
//! MIR statements and terminators become IR nodes near bytecode granularity:
//! assignments define locals, `SwitchInt` is a branch, `Call` carries the
//! callee's full path (so the denylist recognises `std::io::_print`, the
//! call every `println!` expands to), an assignment through a `Deref`
//! projection or into a static is a state write, and `Drop` is an opaque
//! call because user `Drop` impls run arbitrary code. Unwind and cleanup
//! edges are dropped, mirroring the measured exception-edge decision shared
//! by every other frontend. Monomorphic MIR means generic functions appear
//! once, not per instantiation.

use std::path::{Path, PathBuf};
use std::process::Command;

use salience_core::ir::{CallOpacity, FunctionId, FunctionIr, Node, NodeKind, VarId};
use serde::Deserialize;

/// Something went wrong invoking or parsing the MIR lowering.
#[derive(Debug, thiserror::Error)]
pub enum RsError {
    /// The helper binary could not be found anywhere.
    #[error(
        "salience-rust-lower not found. Build it once with:\n    \
         cd tools/rust-lower && cargo build --release\n\
         (rustup fetches the pinned nightly automatically), then either put \
         it on PATH or set SALIENCE_RUST_LOWER to the binary."
    )]
    HelperMissing,
    /// The helper could not be spawned.
    #[error("could not run the MIR lowering: {0}")]
    Spawn(#[from] std::io::Error),
    /// The helper exited non-zero, usually a compile error in the target.
    #[error("salience-rust-lower exited with {status}: {stderr}")]
    Failed {
        /// Exit status, rendered.
        status: String,
        /// The helper's stderr, which carries rustc's own diagnostics.
        stderr: String,
    },
    /// The helper produced output this crate could not read.
    #[error("could not parse the lowering output: {0}")]
    Decode(#[from] serde_json::Error),
}

// Wire format: identical to the Python frontend's, by construction - the
// helper emits the same shape lower.py does.
#[derive(Debug, Deserialize)]
struct WireModule {
    file: String,
    functions: Vec<WireFunction>,
}

#[derive(Debug, Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    signature: String,
    decl_line: Option<u32>,
    entry: usize,
    nodes: Vec<WireNode>,
}

#[derive(Debug, Deserialize)]
struct WireNode {
    line: Option<u32>,
    kind: WireKind,
    defs: Vec<VarId>,
    uses: Vec<VarId>,
    succs: Vec<usize>,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum WireKind {
    Pure,
    Branch,
    Return,
    Throw,
    Call {
        callee: String,
        #[serde(default)]
        opacity: String,
    },
    StateWrite {
        target: String,
    },
}

impl From<WireKind> for NodeKind {
    fn from(w: WireKind) -> Self {
        match w {
            WireKind::Pure => Self::Pure,
            WireKind::Branch => Self::Branch,
            WireKind::Return => Self::Return,
            WireKind::Throw => Self::Throw,
            WireKind::Call { callee, opacity } => Self::Call {
                callee,
                opacity: if opacity == "inert" {
                    CallOpacity::Inert
                } else {
                    CallOpacity::Opaque
                },
            },
            WireKind::StateWrite { target } => Self::StateWrite { target },
        }
    }
}

/// A lowered Rust file.
#[derive(Debug, Clone)]
pub struct LoweredModule {
    /// The source file analyzed.
    pub file: String,
    /// One entry per MIR body: functions, methods, closures, const
    /// initializers.
    pub functions: Vec<FunctionIr>,
}

fn find_helper() -> Result<PathBuf, RsError> {
    if let Ok(p) = std::env::var("SALIENCE_RUST_LOWER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join("salience-rust-lower");
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    for rel in [
        "tools/rust-lower/target/release/salience-rust-lower",
        "tools/rust-lower/target/debug/salience-rust-lower",
    ] {
        let cand = PathBuf::from(rel);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(RsError::HelperMissing)
}

/// Lowers a Rust source file by driving the nightly-pinned helper.
///
/// # Errors
///
/// See [`RsError`]. A compile error in the target surfaces as
/// [`RsError::Failed`] carrying rustc's own diagnostics.
pub fn lower_file(path: &Path) -> Result<LoweredModule, RsError> {
    let helper = find_helper()?;
    lower_file_with(path, &helper)
}

/// Lowers a Rust source file with an explicitly named helper binary,
/// bypassing discovery. What tests and embedders use.
///
/// # Errors
///
/// See [`RsError`].
pub fn lower_file_with(path: &Path, helper: &Path) -> Result<LoweredModule, RsError> {
    let output = Command::new(helper).arg(path).output()?;
    if !output.status.success() {
        return Err(RsError::Failed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let module: WireModule = serde_json::from_slice(&output.stdout)?;
    let file = module.file;
    let functions = module
        .functions
        .into_iter()
        .map(|f| FunctionIr {
            id: FunctionId {
                file: file.clone(),
                name: f.name,
                signature: f.signature,
                decl_line: f.decl_line,
            },
            nodes: f
                .nodes
                .into_iter()
                .map(|n| Node {
                    line: n.line,
                    kind: n.kind.into(),
                    defs: n.defs,
                    uses: n.uses,
                    succs: n.succs,
                    label: n.label,
                })
                .collect(),
            entry: f.entry,
        })
        .collect();
    Ok(LoweredModule { file, functions })
}
