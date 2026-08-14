//! The mutant wire shape `vikt calibrate`'s language engines emit.
//!
//! Every frontend that wants to be calibrated needs exactly one thing: a way
//! to turn a scored line range into candidate source rewrites. Python's
//! engine runs out-of-process and crosses this as JSON; JavaScript's runs
//! in-process and builds it directly. Neither engine, nor the shape itself,
//! knows anything about the other — this lives in the core crate so the CLI's
//! execute/tally/judge pipeline can stay written against one type regardless
//! of which engine produced it.

use serde::Deserialize;

/// One line-targeted mutant: the whole file with a single edit applied.
#[derive(Debug, Clone, Deserialize)]
pub struct Mutant {
    /// Line of the edit in the *original* source — the line the panel scored.
    /// An engine that reserializes the whole file (Python's AST round-trip)
    /// drifts every other line's number; one that splices by byte span
    /// (JavaScript's) does not, but callers key on this field either way.
    pub line: u32,
    /// Operator family, e.g. `cmp`, `bin`, `bool`, `const`, `delete`.
    pub kind: String,
    /// Human-readable edit, e.g. `GtE -> Gt`.
    pub detail: String,
    /// Full mutated file content, ready to write over the copy.
    pub source: String,
}

/// What a mutation engine found and what it emitted for one file.
#[derive(Debug, Clone, Deserialize)]
pub struct MutantSet {
    /// Sites found inside the requested spans, before the cap. When this
    /// exceeds `mutants.len() + invalid_discarded`, the caller ran out of
    /// budget before reaching every site and must say so.
    pub total_sites: usize,
    /// At most the requested number of mutants, sorted by line.
    pub mutants: Vec<Mutant>,
    /// Sites whose edit produced text the frontend's own parser rejected —
    /// a splice-based engine can propose a rewrite that is syntactically
    /// invalid (moving a byte span across a token boundary it didn't parse),
    /// and such a mutant is neither a survivor nor a kill; counting it as
    /// either would corrupt the kill rate. Always zero for an engine (like
    /// Python's AST round-trip) that only ever emits mutants derived from an
    /// already-valid tree. `#[serde(default)]` because Python's wire format
    /// predates this field and never sends it.
    #[serde(default)]
    pub invalid_discarded: usize,
}
