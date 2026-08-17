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
    /// Java sources.
    pub const JAVA: &[&str] = &["java"];
    /// Kotlin sources.
    pub const KOTLIN: &[&str] = &["kt", "kts"];
    /// JVM-language sources, lowered from source by the tree-sitter
    /// frontend (the `.class` route stays the bytecode path). The union of
    /// [`JAVA`] and [`KOTLIN`] — kept as its own table since `InputKind`'s
    /// single-file dispatch doesn't need to tell them apart the way
    /// `calibrate`'s per-language [`crate::language::Language`] registry
    /// does.
    pub const JVM_SOURCE: &[&str] = &["java", "kt", "kts"];
    /// Go sources, lowered from source by the tree-sitter frontend - there
    /// is no bytecode/MIR primary to fall back from at all, same as
    /// [`JVM_SOURCE`] but for a language with no compiled-artifact route
    /// either.
    pub const GO: &[&str] = &["go"];
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
    GoSource,
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
    } else if ext::GO.contains(e) {
        Some(InputKind::GoSource)
    } else {
        None
    }
}

/// Every extension some frontend takes, for the unsupported-input error —
/// derived from the same tables dispatch reads, so the message cannot drift.
#[must_use]
pub fn supported_extensions() -> String {
    [
        ext::CLASS,
        ext::PYTHON,
        ext::JS,
        ext::JVM_SOURCE,
        ext::RUST,
        ext::GO,
    ]
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

/// A language `calibrate` can mutate and score. `.class` inputs are
/// analyzable but not calibratable: no mutation engine exists for compiled
/// bytecode, and the scope error names it honestly rather than claiming it
/// was not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Python,
    JavaScript,
    Rust,
    Java,
    Kotlin,
    Go,
}

impl Language {
    /// How the language reads in a sentence, for progress and error text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::JavaScript => "JavaScript/TypeScript",
            Self::Rust => "Rust",
            Self::Java => "Java",
            Self::Kotlin => "Kotlin",
            Self::Go => "Go",
        }
    }

    /// The dataset's `language` value: one lowercase word, stable across
    /// releases because refit tooling keys on it.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::Go => "go",
        }
    }

    /// The extensions this language's calibratable sources carry.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Python => ext::PYTHON,
            Self::JavaScript => ext::JS,
            Self::Rust => ext::RUST,
            Self::Java => ext::JAVA,
            Self::Kotlin => ext::KOTLIN,
            Self::Go => ext::GO,
        }
    }

    /// Which panel weight vector fits the *primary* frontend's
    /// dependence-graph granularity — instruction-level for bytecode/MIR,
    /// statement-level for AST lowerings. A tree-sitter-scored run
    /// overrides this to `Statement` at the score site, where the lowering
    /// that actually ran is known. Java, Kotlin and Go have no primary
    /// frontend at all — tree-sitter is their only lowering — so this is
    /// never actually read for them; the arm exists only so the match stays
    /// exhaustive.
    pub fn panel_profile(self) -> PanelProfile {
        match self {
            Self::Python | Self::Rust => PanelProfile::Instruction,
            Self::JavaScript | Self::Java | Self::Kotlin | Self::Go => PanelProfile::Statement,
        }
    }

    /// Default total mutant budget. Smaller for every language whose
    /// mutants cost a build step before the suite runs (Rust, Java,
    /// Kotlin) — Go keeps the same default despite also needing one,
    /// since `go build`/`go vet` are fast enough that the per-mutant cost
    /// the smaller budget exists to price in barely shows up.
    pub fn budget_default(self) -> usize {
        match self {
            Self::Rust | Self::Java | Self::Kotlin => 60,
            Self::Python | Self::JavaScript | Self::Go => 150,
        }
    }

    /// Whether every mutant must pass a build step before the suite runs —
    /// a textual splice the language's compiler (or, for Go, `go vet`)
    /// rejects is invalid, not killed.
    pub fn needs_build_step(self) -> bool {
        matches!(self, Self::Rust | Self::Java | Self::Kotlin | Self::Go)
    }

    /// The default `--build-cmd` for a language that [`needs_build_step`],
    /// or `None` when the flag has no safe default and must be given
    /// explicitly — Java and Kotlin projects have no one obvious build
    /// invocation this crate could assume (gradle, maven and a bare
    /// `javac`/`kotlinc` call all differ per project), so calibrating them
    /// requires `--build-cmd` outright.
    ///
    /// [`needs_build_step`]: Self::needs_build_step
    pub fn default_build_cmd(self) -> Option<&'static str> {
        match self {
            Self::Rust => Some("cargo test --no-run"),
            Self::Go => Some("go vet ./..."),
            Self::Java | Self::Kotlin | Self::Python | Self::JavaScript => None,
        }
    }

    /// The synthetic function name(s) that overlap the extent of a real
    /// definition and would double-count lines if sampled. `<module>` is
    /// checked first and unconditionally: every frontend that has one uses
    /// exactly that name for the synthetic top-level wrapper, regardless of
    /// which lowering produced it. Python's bytecode compiler also
    /// synthesizes `<lambda>`, `<listcomp>` and friends — anything with `<`
    /// catches them all. Every JS, Java, Kotlin or Go function, named or
    /// anonymous, is a real function whose lines are its own — `vikt-ts`
    /// gives every closure its own real extent, not one that overlaps its
    /// enclosing function's.
    ///
    /// Rust is the one language that reaches this from two different
    /// lowerings sharing one brace-qualified closure name format
    /// (`f::{closure#0}`, `grammar.rs`'s `closure_name_format` and
    /// `vikt-rs`'s MIR path both produce it): MIR's closure body really
    /// does overlap the enclosing function's own reported line, so it stays
    /// synthetic there, but `vikt-ts`'s tree-sitter fallback gives the same
    /// name its own non-overlapping extent exactly like Java/Kotlin/Go —
    /// treating it as synthetic there would silently drop every Rust
    /// closure's lines from a `--lowering ast` (or MIR-missing `auto`)
    /// calibrate run. `via_ts` disambiguates the two; every other language
    /// here has only ever had one lowering reach `calibrate` and ignores
    /// it.
    pub fn is_synthetic(self, name: &str, via_ts: bool) -> bool {
        if name == "<module>" {
            return true;
        }
        match self {
            Self::Python => name.contains('<'),
            Self::JavaScript | Self::Java | Self::Kotlin | Self::Go => false,
            Self::Rust => !via_ts && name.contains('{'),
        }
    }

    /// True for files that are part of the test suite rather than the code
    /// under test: `test`/`tests` directories for everyone, plus
    /// per-language conventions — `__tests__` and `.test`/`.spec` suffixes
    /// for JavaScript, `test_*`/`*_test.py` names for Python,
    /// `benches`/`examples` directories for Rust (whose unit tests live
    /// behind `#[cfg(test)]` and are never lowered at all), `*Test.java`/
    /// `*Test.kt`/`*Test.kts` names for Java/Kotlin (the JVM convention;
    /// `src/test/...` trees are already covered by the generic `test`
    /// directory-component check), `*_test.go` for Go.
    // The suffix checks are filename conventions, not file extensions,
    // hence the case-sensitive comparisons.
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
            Self::Java => name.ends_with("Test.java"),
            Self::Kotlin => name.ends_with("Test.kt") || name.ends_with("Test.kts"),
            Self::Go => name.ends_with("_test.go"),
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
        use vikt_core::textmut;
        Ok(match self {
            Self::Python => vikt_py::calibrate::mutants_for(file, spans, limit, python)?,
            Self::JavaScript => vikt_js::calibrate::mutants_for(file, spans, limit)?,
            Self::Rust => vikt_rs::calibrate::mutants_for(file, spans, limit)?,
            Self::Java => textmut::mutants_for(file, spans, limit, &textmut::JAVA)?,
            Self::Kotlin => textmut::mutants_for(file, spans, limit, &textmut::KOTLIN)?,
            Self::Go => textmut::mutants_for(file, spans, limit, &textmut::GO)?,
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
        let all = [
            ext::CLASS,
            ext::PYTHON,
            ext::JS,
            ext::JVM_SOURCE,
            ext::RUST,
            ext::GO,
        ]
        .concat();
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
            (ext::GO, InputKind::GoSource),
        ] {
            for e in exts {
                assert_eq!(classify(e), Some(kind), "{e}");
            }
        }
        assert_eq!(classify("rb"), None);
        assert_eq!(classify(""), None);
    }

    /// `JAVA` and `KOTLIN` are disjoint and together equal `JVM_SOURCE` —
    /// the split table used by `calibrate`'s registry must never drift from
    /// the combined one `InputKind` dispatch reads.
    #[test]
    fn java_and_kotlin_partition_jvm_source() {
        assert!(ext::JAVA.iter().all(|e| ext::JVM_SOURCE.contains(e)));
        assert!(ext::KOTLIN.iter().all(|e| ext::JVM_SOURCE.contains(e)));
        assert!(!ext::JAVA.iter().any(|e| ext::KOTLIN.contains(e)));
        let mut union: Vec<&str> = [ext::JAVA, ext::KOTLIN].concat();
        union.sort_unstable();
        let mut jvm = ext::JVM_SOURCE.to_vec();
        jvm.sort_unstable();
        assert_eq!(union, jvm);
    }

    /// The JVM naming convention (`FooTest.java`/`FooTest.kt`) is
    /// case-sensitive and language-specific; `src/test/...` is already
    /// covered by the generic `test`-directory-component rule shared by
    /// every language.
    #[test]
    fn java_kotlin_go_test_path_conventions() {
        assert!(
            Language::Java.is_test_path(Path::new("src/test/java/com/example/CheckoutTest.java"))
        );
        assert!(Language::Java.is_test_path(Path::new("CheckoutTest.java")));
        assert!(!Language::Java.is_test_path(Path::new("Checkout.java")));
        assert!(!Language::Java.is_test_path(Path::new("checkouttest.java")));

        assert!(
            Language::Kotlin.is_test_path(Path::new("src/test/kotlin/com/example/CheckoutTest.kt"))
        );
        assert!(Language::Kotlin.is_test_path(Path::new("CheckoutTest.kts")));
        assert!(!Language::Kotlin.is_test_path(Path::new("Checkout.kt")));

        assert!(Language::Go.is_test_path(Path::new("checkout_test.go")));
        assert!(!Language::Go.is_test_path(Path::new("checkout.go")));
    }

    /// Java, Kotlin and Go closures keep their own scorable extent, the
    /// same treatment JavaScript's already get — only the `<module>`
    /// top-level wrapper is synthetic for these three. `via_ts` is
    /// meaningless for them (only Rust reads it) so both values must agree.
    #[test]
    fn java_kotlin_go_never_treat_a_real_function_as_synthetic() {
        for lang in [Language::Java, Language::Kotlin, Language::Go] {
            for via_ts in [false, true] {
                assert!(lang.is_synthetic("<module>", via_ts));
                assert!(!lang.is_synthetic("checkout", via_ts));
                assert!(!lang.is_synthetic("lambda$checkout$0", via_ts));
            }
        }
    }

    /// Rust is the one language `is_synthetic` treats differently by
    /// lowering: the MIR primary's `f::{closure#0}` really does overlap the
    /// enclosing function's own reported line, so it stays synthetic
    /// (`via_ts: false`); `vikt-ts`'s tree-sitter fallback gives the exact
    /// same name its own non-overlapping extent, the Java/Kotlin/Go
    /// treatment, so it must not be (`via_ts: true`) — see the doc comment
    /// on `is_synthetic`.
    #[test]
    fn rust_closure_synthetic_only_under_the_mir_primary() {
        assert!(Language::Rust.is_synthetic("outer::{closure#0}", false));
        assert!(!Language::Rust.is_synthetic("outer::{closure#0}", true));
        assert!(Language::Rust.is_synthetic("<module>", true));
    }

    /// Rust and Go keep a sensible `--build-cmd` default; Java and Kotlin
    /// have none, which is what makes the flag required for them in
    /// `calibrate`.
    #[test]
    fn default_build_cmd_is_none_only_for_java_and_kotlin() {
        assert_eq!(
            Language::Rust.default_build_cmd(),
            Some("cargo test --no-run")
        );
        assert_eq!(Language::Go.default_build_cmd(), Some("go vet ./..."));
        assert_eq!(Language::Java.default_build_cmd(), None);
        assert_eq!(Language::Kotlin.default_build_cmd(), None);
    }
}
