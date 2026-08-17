//! Lowering-path selection shared between `analyze` and `calibrate`:
//! `auto`/`primary`/`ast`.
//!
//! Only Rust keeps a primary-with-fallback split under `auto`: MIR measured
//! well ahead of the AST lowering (rho 0.868 vs 0.601 on identical
//! mutants), so the helper is used whenever present, and `auto` falls back
//! only on the specific error shape that means "prerequisite missing"
//! (`RsError::HelperMissing`) - a real compile error is reported as that
//! failure, never silently swapped for a different lowering. Python scores
//! via the AST unconditionally under `auto`: measured head-to-head it did
//! at least as well as bytecode (0.651 vs 0.635) with no interpreter
//! needed; `--lowering primary` keeps the bytecode path reproducible.
//! See `eval/calibration/ast-fallback-comparison.md` for both numbers.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use vikt_core::PanelProfile;
use vikt_core::ir::FunctionIr;

use crate::language;

/// Which lowering strategy to use for a language that has both a primary
/// (bytecode/MIR) frontend and this crate's tree-sitter fallback. Global to
/// `analyze` and `calibrate` both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum Lowering {
    /// Rust: the MIR helper when present, tree-sitter otherwise (noted on
    /// stderr each time, never silent). Python: tree-sitter always. The
    /// per-language rationale is in the module docs.
    #[default]
    Auto,
    /// Require the primary; error exactly as it always has if it's
    /// unavailable. The unaffected choice for `.class` (bytecode is the
    /// only lowering there) and `.java`/`.kt`/`.go` source (tree-sitter is
    /// the only lowering there - see `lower_ts_source`).
    Primary,
    /// Force the tree-sitter fallback even where the primary would work.
    /// An error, not a no-op, for `.js`/`.ts`: `vikt-js`'s oxc frontend
    /// already *is* the AST lowering, so there is nothing to force it away
    /// from.
    Ast,
}

/// One resolved lowering: the sidecar's generator provenance, the lowered
/// functions, and the panel profile fitted to the graph granularity that
/// produced them (`Instruction` for bytecode/MIR, `Statement` for every
/// tree-sitter or oxc lowering) - computed once, here, so `analyze` and
/// `calibrate` never re-derive a profile from a bare extension the way a
/// `profile_for_ext`/`panel_profile` duplicate would, which is exactly what
/// goes wrong the moment one extension can be lowered two different ways.
pub struct Lowered {
    pub generator: String,
    pub functions: Vec<FunctionIr>,
    pub profile: PanelProfile,
}

fn ast_single_file(input: &Path) -> Result<Lowered, Box<dyn std::error::Error>> {
    let lowered = vikt_ts::lower_file(input)?;
    Ok(Lowered {
        generator: lowered.generator.to_owned(),
        functions: lowered.functions,
        profile: PanelProfile::Statement,
    })
}

/// Lowers a `.py` file, `auto`/`primary`/`ast` as documented on [`Lowering`].
///
/// `auto` IS the AST path for Python: measured head-to-head on identical
/// mutants, the tree-sitter lowering scored at least as well as the
/// bytecode one (`eval/calibration/ast-fallback-comparison.md`), needs no
/// interpreter, and runs in-process. The bytecode path stays reachable via
/// `--lowering primary` so that comparison remains reproducible.
///
/// # Errors
///
/// Parse or I/O failures; under `primary`, additionally a missing
/// interpreter.
pub fn lower_python(
    input: &Path,
    python: &str,
    lowering: Lowering,
) -> Result<Lowered, Box<dyn std::error::Error>> {
    match lowering {
        Lowering::Primary => primary_python(input, python),
        Lowering::Auto | Lowering::Ast => ast_single_file(input),
    }
}

fn primary_python(input: &Path, python: &str) -> Result<Lowered, Box<dyn std::error::Error>> {
    let lowered = vikt_py::lower_file_with(input, python)?;
    Ok(Lowered {
        generator: "vikt-py/dis".to_owned(),
        functions: lowered.functions,
        profile: PanelProfile::Instruction,
    })
}

/// Lowers a `.js`/`.ts` file. `oxc` is the only lowering this frontend has
/// ever had - it is not a fallback from anything - so `ast` is rejected
/// rather than silently treated as a no-op, and `primary`/`auto` behave
/// identically.
///
/// # Errors
///
/// A lowering failure from `vikt_js`, or - only for `Lowering::Ast` - an
/// explanatory error that `ast` doesn't apply here.
pub fn lower_js(input: &Path, lowering: Lowering) -> Result<Lowered, Box<dyn std::error::Error>> {
    if lowering == Lowering::Ast {
        return Err(
            "--lowering ast doesn't apply to .js/.ts: vikt-js's oxc frontend already IS the AST \
lowering - there is no separate bytecode/MIR primary to skip"
                .into(),
        );
    }
    let lowered = vikt_js::lower_file(input)?;
    Ok(Lowered {
        generator: "vikt-js/oxc".to_owned(),
        functions: lowered.functions,
        profile: PanelProfile::Statement,
    })
}

/// Lowers a `.java`/`.kt`/`.go` source file. There is no bytecode/MIR
/// primary for any of them - Java/Kotlin's only compiled route is `.class`;
/// Go has no compiled-artifact route in this crate at all - so every value
/// of `lowering` resolves to the same tree-sitter lowering; this is new
/// capability, not a fallback with a choice to make.
///
/// # Errors
///
/// Any `vikt_ts` lowering failure.
pub fn lower_ts_source(input: &Path) -> Result<Lowered, Box<dyn std::error::Error>> {
    ast_single_file(input)
}

/// Lowers a single `.rs` file (not a cargo package - see
/// [`lower_rust_crate`] for that), `auto`/`primary`/`ast` as documented on
/// [`Lowering`].
///
/// # Errors
///
/// Any lowering failure that isn't specifically "the MIR helper is
/// missing" under `auto`.
pub fn lower_rust_file(
    input: &Path,
    lowering: Lowering,
) -> Result<Lowered, Box<dyn std::error::Error>> {
    match lowering {
        Lowering::Primary => primary_rust_file(input),
        Lowering::Ast => ast_single_file(input),
        Lowering::Auto => match vikt_rs::lower_file(input) {
            Ok(lowered) => Ok(Lowered {
                generator: "vikt-rs/rustc_public".to_owned(),
                functions: lowered.functions,
                profile: PanelProfile::Instruction,
            }),
            Err(vikt_rs::RsError::HelperMissing) => {
                eprintln!(
                    "vikt: note: vikt-rust-lower not found; falling back to the tree-sitter lowering (pass --lowering primary to require it)"
                );
                ast_single_file(input)
            }
            Err(e) => Err(e.into()),
        },
    }
}

fn primary_rust_file(input: &Path) -> Result<Lowered, Box<dyn std::error::Error>> {
    let lowered = vikt_rs::lower_file(input)?;
    Ok(Lowered {
        generator: "vikt-rs/rustc_public".to_owned(),
        functions: lowered.functions,
        profile: PanelProfile::Instruction,
    })
}

/// Lowers a whole cargo package (a directory or a `Cargo.toml` path),
/// `auto`/`primary`/`ast` as documented on [`Lowering`]. `ast` has no cargo
/// invocation at all here - `vikt_rs::lower_crate` is the only thing that
/// knows how to resolve a package's own dependency graph, so the fallback
/// is a plain per-file tree-sitter scan of the package's own sources (see
/// [`walk_rust_sources`]), which cannot see anything outside the package.
///
/// # Errors
///
/// Any lowering failure that isn't specifically "the MIR helper is
/// missing" under `auto`.
pub fn lower_rust_crate(
    input: &Path,
    package: Option<&str>,
    lowering: Lowering,
) -> Result<Lowered, Box<dyn std::error::Error>> {
    match lowering {
        Lowering::Primary => primary_rust_crate(input, package),
        Lowering::Ast => ast_rust_package(input),
        Lowering::Auto => match vikt_rs::lower_crate(input, package) {
            Ok(modules) => Ok(Lowered {
                generator: "vikt-rs/rustc_public".to_owned(),
                functions: modules.into_iter().flat_map(|m| m.functions).collect(),
                profile: PanelProfile::Instruction,
            }),
            Err(vikt_rs::RsError::HelperMissing) => {
                eprintln!(
                    "vikt: note: vikt-rust-lower not found; falling back to a per-file tree-sitter scan of the package (pass --lowering primary to require it)"
                );
                ast_rust_package(input)
            }
            Err(e) => Err(e.into()),
        },
    }
}

fn primary_rust_crate(
    input: &Path,
    package: Option<&str>,
) -> Result<Lowered, Box<dyn std::error::Error>> {
    let modules = vikt_rs::lower_crate(input, package)?;
    Ok(Lowered {
        generator: "vikt-rs/rustc_public".to_owned(),
        functions: modules.into_iter().flat_map(|m| m.functions).collect(),
        profile: PanelProfile::Instruction,
    })
}

fn ast_rust_package(input: &Path) -> Result<Lowered, Box<dyn std::error::Error>> {
    let root = rust_package_root(input);
    let mut functions = Vec::new();
    for rel in walk_rust_sources(&root)? {
        let abs = root.join(&rel);
        match vikt_ts::lower_file(&abs) {
            Ok(lowered) => functions.extend(lowered.functions),
            Err(e) => eprintln!("vikt: skipping {}: {e}", rel.display()),
        }
    }
    Ok(Lowered {
        generator: "vikt-ts/tree-sitter-rust".to_owned(),
        functions,
        profile: PanelProfile::Statement,
    })
}

/// The package root a `.rs` input resolves to: itself if it's already a
/// directory, else the parent of the `Cargo.toml` path.
#[must_use]
pub fn rust_package_root(input: &Path) -> PathBuf {
    if input.is_dir() {
        input.to_path_buf()
    } else {
        input
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }
}

/// Every `.rs` file under `root`, relative to it, skipping `target/` and
/// dot-directories - the tree-sitter fallback's substitute for a cargo scan
/// when there is no cargo invocation at all. Sorted, so a scan is
/// order-deterministic like every other frontend's file discovery here.
///
/// # Errors
///
/// Any I/O failure reading a directory under `root`.
pub fn walk_rust_sources(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_rust_sources_into(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_rust_sources_into(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if name.starts_with('.') || name == "target" {
                continue;
            }
            walk_rust_sources_into(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.push(rel);
        }
    }
    Ok(())
}

/// The stderr note describing how a `.class` file's line numbers were
/// resolved, shared between a single `.class` input (threaded through the
/// caller and printed once scoring finishes) and a `.class` file found
/// inside a [`lower_folder`] walk (printed immediately, since a folder can
/// contain more than one).
#[must_use]
pub fn class_note(lowered: &vikt_jvm::LoweredClass) -> Option<String> {
    lowered.smap_stratum.as_ref().map(|stratum| {
        format!(
            "{}: line numbers resolved through the SourceDebugExtension \
(JSR-45/SMAP, `{stratum}` stratum); inlined bodies collapsed onto their call sites\
{}",
            lowered.binary_name,
            if lowered.foreign_lines_dropped > 0 {
                format!(
                    ", {} instruction(s) dropped as belonging to another source file",
                    lowered.foreign_lines_dropped
                )
            } else {
                String::new()
            }
        )
    })
}

/// Every registry-known source file under `root`, relative to it and
/// classified — the walk behind a plain-directory `vikt` input and a
/// `Cargo.toml` directory's non-Rust sources. Skips dot-directories and the
/// conventional non-source directories ([`language::SKIP_DIRS`]), the same
/// skip conventions `vikt calibrate`'s tree scan applies to its own copy.
/// Sorted by relative path, so a walk is order-deterministic like every
/// other file discovery in this crate.
///
/// # Errors
///
/// Any I/O failure reading a directory under `root`.
pub fn walk_registry_sources(root: &Path) -> std::io::Result<Vec<(PathBuf, language::InputKind)>> {
    let mut out = Vec::new();
    walk_registry_sources_into(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk_registry_sources_into(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, language::InputKind)>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if name.starts_with('.') || language::SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_registry_sources_into(root, &path, out)?;
        } else if let Some(kind) = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(language::classify)
        {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.push((rel, kind));
        }
    }
    Ok(())
}

/// Lowers every registry-known source file under a plain directory input,
/// skipping any [`language::InputKind`] in `skip` — a `Cargo.toml`
/// directory's extra scan passes `&[InputKind::Rust]` since cargo mode
/// already lowered every `.rs` file through [`lower_rust_crate`]; a bare
/// directory input passes nothing. Each file lowers through whichever
/// frontend its extension selects, exactly as a single-file `vikt`
/// invocation would — `lowering` governs every Python, Rust or `.class` file
/// found, same as it does for a single-file input. A file whose frontend
/// fails to lower it is reported on stderr and skipped: one broken file in a
/// folder should not block scoring the rest of it, the policy `calibrate`'s
/// tree scan already uses.
///
/// # Errors
///
/// Any I/O failure walking `root` itself; per-file lowering failures are
/// reported on stderr and skipped, not propagated.
pub fn lower_folder(
    root: &Path,
    python: &str,
    lowering: Lowering,
    skip: &[language::InputKind],
) -> std::io::Result<Vec<Lowered>> {
    let mut out = Vec::new();
    for (rel, kind) in walk_registry_sources(root)? {
        if skip.contains(&kind) {
            continue;
        }
        let abs = root.join(&rel);
        match lower_one(&abs, kind, python, lowering) {
            Ok(lowered) => out.push(lowered),
            Err(e) => eprintln!("vikt: skipping {}: {e}", rel.display()),
        }
    }
    Ok(out)
}

/// Lowers one file through whichever frontend `kind` selects — [`lower_folder`]'s
/// per-extension dispatch, mirroring `main`'s single-file dispatch exactly so
/// a file scores the same whether it is handed to `vikt` alone or found
/// inside a folder.
fn lower_one(
    path: &Path,
    kind: language::InputKind,
    python: &str,
    lowering: Lowering,
) -> Result<Lowered, Box<dyn std::error::Error>> {
    match kind {
        language::InputKind::ClassFile => {
            let bytes = std::fs::read(path)?;
            let lowered = vikt_jvm::lower_class(&bytes)?;
            if let Some(note) = class_note(&lowered) {
                eprintln!("vikt: note: {note}");
            }
            Ok(Lowered {
                generator: "vikt-jvm/mokapot".to_owned(),
                functions: lowered.functions,
                profile: PanelProfile::Instruction,
            })
        }
        language::InputKind::Python => lower_python(path, python, lowering),
        language::InputKind::JsTs => lower_js(path, lowering),
        language::InputKind::JvmSource | language::InputKind::GoSource => lower_ts_source(path),
        language::InputKind::Rust => lower_rust_file(path, lowering),
    }
}
