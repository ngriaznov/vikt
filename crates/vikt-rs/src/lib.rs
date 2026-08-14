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
//! In order: the `VIKT_RUST_LOWER` environment variable, then
//! `vikt-rust-lower` on `PATH`, then the debug and release build
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

use serde::Deserialize;
use vikt_core::ir::{CallOpacity, FunctionId, FunctionIr, Node, NodeKind, VarId};

pub mod calibrate;

/// Something went wrong invoking or parsing the MIR lowering.
#[derive(Debug, thiserror::Error)]
pub enum RsError {
    /// The helper binary could not be found anywhere.
    #[error(
        "vikt-rust-lower not found. Build it once with:\n    \
         cd tools/rust-lower && cargo build --release\n\
         (rustup fetches the pinned nightly automatically), then either put \
         it on PATH or set VIKT_RUST_LOWER to the binary."
    )]
    HelperMissing,
    /// The helper could not be spawned.
    #[error("could not run the MIR lowering: {0}")]
    Spawn(#[from] std::io::Error),
    /// The helper exited non-zero, usually a compile error in the target.
    #[error("vikt-rust-lower exited with {status}: {stderr}")]
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
    #[serde(default)]
    file: String,
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
    if let Ok(p) = std::env::var("VIKT_RUST_LOWER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join("vikt-rust-lower");
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    for rel in [
        "tools/rust-lower/target/release/vikt-rust-lower",
        "tools/rust-lower/target/debug/vikt-rust-lower",
    ] {
        let cand = PathBuf::from(rel);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(RsError::HelperMissing)
}

/// The toolchain the helper is pinned to; `lower_crate` runs cargo under it
/// so dependency rlibs and the analyzed crate share one compiler version.
pub const PINNED_TOOLCHAIN: &str = "nightly-2026-08-13";

/// Lowers a whole cargo package: dependencies included, exactly as cargo
/// builds it.
///
/// Runs `cargo check` with the helper installed as `RUSTC_WRAPPER` and the
/// pinned toolchain selected. Dependencies, build scripts and proc-macros
/// compile untouched; the primary package is compiled through `rustc_public`
/// and its lowering lands in a temp directory this function reads back. A
/// separate `CARGO_TARGET_DIR` (`target/vikt` under the package) keeps
/// dependency artifacts cached across runs without disturbing the user's own
/// build; when cargo considers the primary package fresh and skips it, its
/// fingerprints are removed and the check reruns, so a lowering is always
/// produced.
///
/// `manifest_dir` may be the package directory or its `Cargo.toml`.
/// `package` selects one package of a workspace (`cargo -p`).
///
/// # Errors
///
/// See [`RsError`]. Compile errors in the package surface as
/// [`RsError::Failed`] with cargo's own diagnostics.
pub fn lower_crate(
    manifest_dir: &Path,
    package: Option<&str>,
) -> Result<Vec<LoweredModule>, RsError> {
    let helper = find_helper()?;
    let helper = helper.canonicalize().map_err(RsError::Spawn)?;
    let manifest = if manifest_dir.ends_with("Cargo.toml") {
        manifest_dir.to_path_buf()
    } else {
        manifest_dir.join("Cargo.toml")
    };
    let pkg_root = manifest.parent().unwrap_or(manifest_dir).to_path_buf();
    let out_dir = std::env::temp_dir().join(format!("vikt-lower-{}-{:x}", std::process::id(), {
        // Cheap path hash so parallel invocations do not collide.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in pkg_root.to_string_lossy().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }));
    std::fs::create_dir_all(&out_dir)?;
    // Dependency artifacts cache here across runs. VIKT_TARGET_DIR
    // overrides it for callers that must not write inside the analyzed
    // package at all - analysis of a read-only checkout, say.
    let target_dir = std::env::var_os("VIKT_TARGET_DIR")
        .map_or_else(|| pkg_root.join("target").join("vikt"), PathBuf::from);

    let run = |out_dir: &Path| -> Result<std::process::Output, RsError> {
        let mut cmd = Command::new("cargo");
        cmd.arg("check")
            .arg("--manifest-path")
            .arg(&manifest)
            .env("RUSTUP_TOOLCHAIN", PINNED_TOOLCHAIN)
            .env("RUSTC_WRAPPER", &helper)
            .env("VIKT_LOWER_OUT", out_dir)
            .env("CARGO_TARGET_DIR", &target_dir);
        if let Some(p) = package {
            cmd.arg("-p").arg(p);
        }
        Ok(cmd.output()?)
    };

    let mut output = run(&out_dir)?;
    if output.status.success() && std::fs::read_dir(&out_dir)?.next().is_none() {
        // Primary package was fresh, so the wrapper never saw it: drop its
        // fingerprints and force one recompile. Two layouts, because cargo
        // moved them: classic `debug/.fingerprint/<unit>-<hash>/`, and the
        // newer per-unit `debug/build/<package>/<hash>/fingerprint`.
        let debug = target_dir.join("debug");
        let matches_pkg = |name: &str| {
            package.is_none_or(|p| name.starts_with(&p.replace('-', "_")) || name.starts_with(p))
        };
        for base in [debug.join(".fingerprint"), debug.join("build")] {
            if let Ok(entries) = std::fs::read_dir(&base) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if matches_pkg(&name) {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }
        output = run(&out_dir)?;
    }

    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&out_dir);
        return Err(RsError::Failed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let mut modules = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&out_dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let bytes = std::fs::read(&path)?;
        let wire: WireModule = serde_json::from_slice(&bytes)?;
        modules.push(wire_to_module(wire));
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    if modules.is_empty() {
        return Err(RsError::Failed {
            status: "0".into(),
            stderr: "cargo check succeeded but produced no lowering; is the                      package a proc-macro or build-script-only crate?"
                .into(),
        });
    }
    Ok(modules)
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
    Ok(wire_to_module(module))
}

fn wire_to_module(module: WireModule) -> LoweredModule {
    let file = module.file;
    let functions = module
        .functions
        .into_iter()
        .map(|f| FunctionIr {
            id: FunctionId {
                // Per-function file: crate mode spans many source files, and
                // the helper records each body's own.
                file: if f.file.is_empty() {
                    file.clone()
                } else {
                    f.file
                },
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
    LoweredModule { file, functions }
}
