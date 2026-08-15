//! Generic tree-sitter fallback frontend for `vikt-core`.
//!
//! Used whenever a primary lowering is unavailable - Rust without the MIR
//! helper, Python without `python3` - and as the only path for source-level
//! `.java`/`.kt` analysis (today only compiled `.class` is analyzable).
//!
//! One [`walk`] engine drives every language, table-selected by
//! [`Language`]: a per-language [`grammar::GrammarTable`] maps tree-sitter
//! node kinds and field names onto the lowering vocabulary (function defs,
//! bindings, assignments, calls, branches, loops, returns/throws, state
//! writes). Every lowering here is statement-granular - see [`walk`]'s
//! module docs for why and for the known v1 simplifications.
//!
//! All four languages have complete tables. `Language::Java` and
//! `Language::Kotlin` are also the *only* path for source-level `.java`/
//! `.kt` analysis - today's other frontends only read compiled `.class`
//! bytecode, so tree-sitter is not a fallback for these two, it's new
//! capability.

mod grammar;
mod walk;

use std::path::Path;

use thiserror::Error;
use vikt_core::ir::FunctionIr;

/// A source language this frontend can be asked to lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    Java,
    Kotlin,
}

impl Language {
    /// Infers a language from a file extension (no leading dot).
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "java" => Some(Self::Java),
            "kt" | "kts" => Some(Self::Kotlin),
            _ => None,
        }
    }

    fn ts_language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        }
    }

    fn table(self) -> &'static grammar::GrammarTable {
        match self {
            Self::Rust => &grammar::RUST,
            Self::Python => &grammar::PYTHON,
            Self::Java => &grammar::JAVA,
            Self::Kotlin => &grammar::KOTLIN,
        }
    }
}

/// Failure to lower a source file.
#[derive(Debug, Error)]
pub enum TsError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The extension names no known language.
    #[error("{path}: no tree-sitter grammar for extension {ext:?}")]
    UnsupportedExtension {
        /// Offending path.
        path: String,
        /// The extension that didn't match anything.
        ext: String,
    },
    /// The grammar's ABI didn't match the linked tree-sitter runtime.
    #[error("{path}: parser could not load the grammar")]
    Grammar {
        /// Offending path.
        path: String,
    },
    /// tree-sitter's `parse` returned nothing at all.
    #[error("{path}: parse failed outright")]
    ParseFailed {
        /// Offending path.
        path: String,
    },
    /// Parse errors dominate the tree: lowering would produce more noise
    /// than signal, so this refuses rather than guess.
    #[error("{path}: {ratio:.0}% of nodes are parse errors; refusing to lower")]
    Syntax {
        /// Offending path.
        path: String,
        /// Percentage (0-100) of nodes that are `ERROR`/`MISSING`.
        ratio: f64,
    },
}

/// One function whose body degraded to a stub because of a parse error.
///
/// Reported, never silent: a parse error inside a function body only
/// degrades that function, and this is how a caller finds out which one.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// The affected function's name.
    pub function: String,
    /// What went wrong.
    pub message: String,
}

/// A lowered module: one [`FunctionIr`] per function plus a `<module>`
/// wrapper for top-level code, mirroring the other frontends' shape.
#[derive(Debug)]
pub struct LoweredModule {
    /// Source path as given.
    pub file: String,
    /// Every lowered function, `<module>` first.
    pub functions: Vec<FunctionIr>,
    /// Functions degraded by a parse error - see [`Diagnostic`].
    pub diagnostics: Vec<Diagnostic>,
    /// Which grammar produced this module, e.g. `"vikt-ts/tree-sitter-rust"`
    /// - the sidecar's generator provenance.
    pub generator: &'static str,
}

/// Lowers a file, inferring its language from the extension.
///
/// # Errors
///
/// See [`TsError`].
pub fn lower_file(path: &Path) -> Result<LoweredModule, TsError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language = Language::from_extension(ext).ok_or_else(|| TsError::UnsupportedExtension {
        path: path.display().to_string(),
        ext: ext.to_owned(),
    })?;
    let source = std::fs::read_to_string(path).map_err(|e| TsError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    lower_source(language, &source, &path.display().to_string())
}

/// Lowers source text directly for a given [`Language`].
///
/// # Errors
///
/// See [`TsError`].
pub fn lower_source(
    language: Language,
    source: &str,
    file: &str,
) -> Result<LoweredModule, TsError> {
    let table = language.table();
    let ts_language = language.ts_language();

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|_| TsError::Grammar {
            path: file.to_owned(),
        })?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| TsError::ParseFailed {
            path: file.to_owned(),
        })?;
    let root = tree.root_node();

    // A parse error inside one function degrades only that function (see
    // `walk`'s per-function handling), but a file that is mostly noise
    // isn't worth lowering at all - the threshold is deliberately generous
    // so a handful of local errors never trips it.
    let (total, bad) = walk::error_stats(root);
    let ratio = if total == 0 {
        0.0
    } else {
        f64::from(u32::try_from(bad).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
    };
    if ratio > 0.25 {
        return Err(TsError::Syntax {
            path: file.to_owned(),
            ratio: ratio * 100.0,
        });
    }

    let (functions, diagnostics) = walk::lower_module(table, root, source, file);
    Ok(LoweredModule {
        file: file.to_owned(),
        functions,
        diagnostics,
        generator: table.generator,
    })
}
