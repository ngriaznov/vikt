//! The one place `vikt-cli` knows which languages exist and what facts
//! attach to each: extensions, labels, panel profiles, calibration
//! conventions. The frontends own *how* a language lowers, [`crate::lowering`]
//! owns *which path is taken*; this module owns what a language *is*.
//! Adding a language means one frontend crate (or one `vikt-ts` grammar
//! table) plus rows here — nothing else in this crate should need to learn
//! its name.

use std::path::Path;

use vikt_core::PanelProfile;
use vikt_core::mutant::MutantSet;

/// File-extension facts — the single source of truth `main`'s dispatch and
/// `calibrate`'s tree scan both read.
pub mod ext {
    /// Compiled JVM classes, the bytecode frontend's input.
    pub const CLASS: &[&str] = &["class"];
    /// Python sources.
    pub const PYTHON: &[&str] = &["py"];
    /// JavaScript and TypeScript sources, the oxc frontend's inputs.
    pub const JS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];
    /// The [`JS`] subset that is TypeScript — the type-check caveat in
    /// calibrate's docs applies to exactly these.
    pub const TYPESCRIPT: &[&str] = &["ts", "mts", "cts", "tsx"];
    /// Rust sources.
    pub const RUST: &[&str] = &["rs"];
    /// JVM-language sources, lowered from source by the tree-sitter
    /// frontend (the `.class` route stays the bytecode path).
    pub const JVM_SOURCE: &[&str] = &["java", "kt", "kts"];
}

/// Directory names a source walk never descends into, beyond anything
/// dot-prefixed: build output, dependency caches and virtualenvs holding no
/// source of ours to lower. Shared by a folder or repo-scope input's walk
/// ([`crate::lowering::walk_registry_sources`]) and `vikt calibrate`'s tree
/// scan, which additionally stages `node_modules` into its copy unscored
/// rather than skipping it outright, so a JavaScript suite still has its
/// dependencies to run against.
pub const SKIP_DIRS: &[&str] = &["node_modules", "target", "venv", "__pycache__"];

/// What an input file's extension selects, for `main`'s dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    ClassFile,
    Python,
    JsTs,
    JvmSource,
    Rust,
}

/// Classifies an extension, or `None` for anything no frontend takes.
#[must_use]
pub fn classify(extension: &str) -> Option<InputKind> {
    let e = &extension;
    if ext::CLASS.contains(e) {
        Some(InputKind::ClassFile)
    } else if ext::PYTHON.contains(e) {
        Some(InputKind::Python)
    } else if ext::JS.contains(e) {
        Some(InputKind::JsTs)
    } else if ext::JVM_SOURCE.contains(e) {
        Some(InputKind::JvmSource)
    } else if ext::RUST.contains(e) {
        Some(InputKind::Rust)
    } else {
        None
    }
}

/// Every extension some frontend takes, for the unsupported-input error —
/// derived from the same tables dispatch reads, so the message cannot drift.
#[must_use]
pub fn supported_extensions() -> String {
    [ext::CLASS, ext::PYTHON, ext::JS, ext::JVM_SOURCE, ext::RUST]
        .concat()
        .iter()
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// True for `.ts`/`.mts`/`.cts`/`.tsx` — the extensions the TypeScript
/// type-check caveat applies to.
#[must_use]
pub fn is_typescript(rel: &Path) -> bool {
    rel.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ext::TYPESCRIPT.contains(&e))
}

/// A language `calibrate` can mutate and score. `.class`, `.java` and `.kt`
/// inputs are analyzable but not calibratable: no mutation engine exists
/// for them, and the scope error names them honestly rather than claiming
/// they were not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Python,
    JavaScript,
    Rust,
}

impl Language {
    /// How the language reads in a sentence, for progress and error text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::JavaScript => "JavaScript/TypeScript",
            Self::Rust => "Rust",
        }
    }

    /// The dataset's `language` value: one lowercase word, stable across
    /// releases because refit tooling keys on it.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
        }
    }

    /// The extensions this language's calibratable sources carry.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Python => ext::PYTHON,
            Self::JavaScript => ext::JS,
            Self::Rust => ext::RUST,
        }
    }

    /// Which panel weight vector fits the *primary* frontend's
    /// dependence-graph granularity — instruction-level for bytecode/MIR,
    /// statement-level for AST lowerings. A tree-sitter-scored run
    /// overrides this to `Statement` at the score site, where the lowering
    /// that actually ran is known.
    pub fn panel_profile(self) -> PanelProfile {
        match self {
            Self::Python | Self::Rust => PanelProfile::Instruction,
            Self::JavaScript => PanelProfile::Statement,
        }
    }

    /// Default total mutant budget. Rust's is smaller because every mutant
    /// costs an incremental compile before its test run.
    pub fn budget_default(self) -> usize {
        match self {
            Self::Rust => 60,
            Self::Python | Self::JavaScript => 150,
        }
    }

    /// Whether every mutant must pass a build step before the suite runs —
    /// textual Rust splices the compiler rejects are invalid, not killed.
    pub fn needs_build_step(self) -> bool {
        matches!(self, Self::Rust)
    }

    /// The synthetic function name(s) that overlap the extent of a real
    /// definition and would double-count lines if sampled. `<module>` is
    /// checked first and unconditionally: every frontend that has one uses
    /// exactly that name for the synthetic top-level wrapper, regardless of
    /// which lowering produced it. Python's bytecode compiler also
    /// synthesizes `<lambda>`, `<listcomp>` and friends — anything with `<`
    /// catches them all; MIR closure/generator bodies carry brace-qualified
    /// names (`f::{closure#0}`). Every other JS function, named or
    /// anonymous, is a real function whose lines are its own.
    pub fn is_synthetic(self, name: &str) -> bool {
        if name == "<module>" {
            return true;
        }
        match self {
            Self::Python => name.contains('<'),
            Self::JavaScript => false,
            Self::Rust => name.contains('{'),
        }
    }

    /// True for files that are part of the test suite rather than the code
    /// under test: `test`/`tests` directories for everyone, plus
    /// per-language conventions — `__tests__` and `.test`/`.spec` suffixes
    /// for JavaScript, `test_*`/`*_test.py` names for Python,
    /// `benches`/`examples` directories for Rust (whose unit tests live
    /// behind `#[cfg(test)]` and are never lowered at all).
    // The `.test`/`.spec` suffix check is a filename convention, not a file
    // extension, hence the case-sensitive comparison.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    pub fn is_test_path(self, rel: &Path) -> bool {
        let in_test_dir = rel.parent().is_some_and(|p| {
            p.components().any(|c| {
                let name = c.as_os_str().to_str();
                matches!(name, Some("test" | "tests"))
                    || (self == Self::JavaScript && name == Some("__tests__"))
                    || (self == Self::Rust && matches!(name, Some("benches" | "examples")))
            })
        });
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let by_name = match self {
            Self::Python => name.starts_with("test_") || name.ends_with("_test.py"),
            Self::JavaScript => ext::JS.iter().any(|e| {
                name.strip_suffix(&format!(".{e}"))
                    .is_some_and(|base| base.ends_with(".test") || base.ends_with(".spec"))
            }),
            Self::Rust => false,
        };
        in_test_dir || by_name
    }

    /// The language's mutation engine, one dispatch site for all of them.
    ///
    /// # Errors
    ///
    /// Whatever the engine reports: I/O, a failed `python` spawn, a parse
    /// failure on the file being mutated.
    pub fn mutants_for(
        self,
        file: &Path,
        spans: &[(u32, u32)],
        limit: usize,
        python: &str,
    ) -> Result<MutantSet, Box<dyn std::error::Error>> {
        Ok(match self {
            Self::Python => vikt_py::calibrate::mutants_for(file, spans, limit, python)?,
            Self::JavaScript => vikt_js::calibrate::mutants_for(file, spans, limit)?,
            Self::Rust => vikt_rs::calibrate::mutants_for(file, spans, limit)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every extension maps to exactly one input kind: the registry's
    /// tables are disjoint, so dispatch can never be order-dependent.
    #[test]
    fn extension_tables_are_disjoint() {
        let all = [ext::CLASS, ext::PYTHON, ext::JS, ext::JVM_SOURCE, ext::RUST].concat();
        let mut seen = std::collections::BTreeSet::new();
        for e in &all {
            assert!(seen.insert(*e), "extension {e:?} appears in two tables");
        }
        assert!(
            ext::TYPESCRIPT.iter().all(|e| ext::JS.contains(e)),
            "TYPESCRIPT must be a subset of JS"
        );
    }

    #[test]
    fn classify_covers_every_table_and_rejects_strangers() {
        for (exts, kind) in [
            (ext::CLASS, InputKind::ClassFile),
            (ext::PYTHON, InputKind::Python),
            (ext::JS, InputKind::JsTs),
            (ext::JVM_SOURCE, InputKind::JvmSource),
            (ext::RUST, InputKind::Rust),
        ] {
            for e in exts {
                assert_eq!(classify(e), Some(kind), "{e}");
            }
        }
        assert_eq!(classify("rb"), None);
        assert_eq!(classify(""), None);
    }
}
