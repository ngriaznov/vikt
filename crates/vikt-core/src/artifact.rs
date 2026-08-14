//! The sidecar artifact: line ranges to tier, with a reason for each.
//!
//! This is the whole deliverable. Everything upstream exists to produce it, and
//! every consumer (an agent reading a file, a harness hook gating an edit, a
//! profiler picking a starting point, a dependency graph weighting its
//! nodes) reads this and nothing else.
//!
//! Two design commitments show up in the schema:
//!
//! - **Nothing is removed.** The artifact is metadata *about* source, never a
//!   rewritten copy of it. A consumer that ignores the sidecar sees exactly the
//!   file it would have seen anyway.
//! - **Every span carries its reason.** A tier a consumer cannot interrogate is
//!   a number to be trusted on faith, which is precisely what an inference-free
//!   analysis exists to avoid.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::importance::{FunctionImportance, LineSpan, Tier, project_to_lines};
use crate::ir::FunctionIr;

/// Schema identifier, bumped on any breaking change to the shape below.
///
/// `SpanRecord::file_score` (added for `--scope file`) is deliberately *not*
/// such a change: it is optional, defaults to absent, and every struct here
/// derives plain `Serialize`/`Deserialize` with no `deny_unknown_fields`, so
/// an old reader ignores the new key and a new reader sees `None` on output
/// that never set it. Single-file, default-scope output is unaffected byte
/// for byte. Reserve the next schema bump for a change that removes,
/// renames, or repurposes an existing field's meaning.
pub const SCHEMA: &str = "vikt-sidecar/v1";

/// One tiered run of source lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanRecord {
    /// First line, inclusive, 1-based.
    pub start: u32,
    /// Last line, inclusive, 1-based.
    pub end: u32,
    /// `core` / `boundary` / `plumbing` / `inert`.
    pub tier: String,
    /// Continuous importance against a fixed scale, `0.0..=1.0`. Use for
    /// thresholds and policy.
    pub score: f64,
    /// Percentile of that score within this function, `0.0..=1.0`. Use for
    /// heatmaps and for "show me the top of this body".
    pub rank: f64,
    /// File-scope score: [`score`](Self::score) reweighted by the owning
    /// function's call-graph standing among its file's other functions, then
    /// rank-normalised across every scored line of the file (see
    /// [`crate::filescope`]). Present only when the run asked for file
    /// scope (`vikt --scope file`); absent, and omitted from JSON entirely,
    /// under the default within-function scope. Not comparable across
    /// files — each file is re-ranked against itself alone.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file_score: Option<f64>,
    /// Why this run got its tier.
    pub reasons: Vec<String>,
}

impl SpanRecord {
    fn from_span(s: &LineSpan) -> Self {
        Self {
            start: s.start,
            end: s.end,
            tier: s.tier.name().to_owned(),
            // Two decimals: the score is a ranking signal, and pretending to
            // more precision than the weights justify invites false diffs
            // between runs on trivially different inputs.
            score: (s.score * 100.0).round() / 100.0,
            rank: (s.rank * 100.0).round() / 100.0,
            file_score: None,
            reasons: s.reasons.clone(),
        }
    }
}

/// How much of the body carried source positions at all.
///
/// This is the honesty field. On substrates where line mapping is partial —
/// synthetic members, compiler-generated bridges, optimized-away constructs —
/// a consumer needs to know it is looking at an incomplete picture rather than
/// at a body that genuinely has no core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// Instructions lowered from this function.
    pub instructions: usize,
    /// How many carried a source line.
    pub with_line: usize,
}

/// Counts per tier, for quick filtering without walking every span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierCounts {
    /// Lines tiered `core`.
    pub core: usize,
    /// Lines tiered `boundary`.
    pub boundary: usize,
    /// Lines tiered `plumbing`.
    pub plumbing: usize,
    /// Lines tiered `inert`.
    pub inert: usize,
}

/// The importance map for one function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionRecord {
    /// Source file this function's lines refer to. Omitted when it matches
    /// the sidecar-level `file`, which keeps single-file output (every
    /// frontend except cargo crate mode) byte-identical to before this field
    /// existed. Crate mode spans many files and needs per-function
    /// attribution.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub file: String,
    /// Fully-qualified name.
    pub name: String,
    /// Descriptor or signature, when the substrate has one.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub signature: String,
    /// First line of the body, when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub decl_line: Option<u32>,
    /// Position-mapping coverage.
    pub coverage: Coverage,
    /// Line counts per tier.
    pub summary: TierCounts,
    /// The map itself, ordered by start line.
    pub spans: Vec<SpanRecord>,
}

/// The sidecar for one source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    /// Schema identifier.
    pub schema: String,
    /// What produced this, including the frontend name.
    pub generator: String,
    /// Source file the spans refer to.
    pub file: String,
    /// One entry per analyzed function, ordered by declaration line then name.
    pub functions: Vec<FunctionRecord>,
}

impl Sidecar {
    /// Creates an empty sidecar for `file`.
    #[must_use]
    pub fn new(file: impl Into<String>, generator: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            generator: generator.into(),
            file: file.into(),
            functions: Vec::new(),
        }
    }

    /// Appends the analysis of one function.
    pub fn push(&mut self, ir: &FunctionIr, sal: &FunctionImportance) {
        let spans = project_to_lines(ir, sal);
        let mut summary = TierCounts::default();
        for s in &spans {
            let lines = (s.end - s.start + 1) as usize;
            match s.tier {
                Tier::Core => summary.core += lines,
                Tier::Boundary => summary.boundary += lines,
                Tier::Plumbing => summary.plumbing += lines,
                Tier::Inert => summary.inert += lines,
            }
        }
        self.functions.push(FunctionRecord {
            file: if ir.id.file == self.file {
                String::new()
            } else {
                ir.id.file.clone()
            },
            name: ir.id.name.clone(),
            signature: ir.id.signature.clone(),
            decl_line: ir.id.decl_line,
            coverage: Coverage {
                instructions: ir.nodes.len(),
                with_line: ir.nodes.iter().filter(|n| n.line.is_some()).count(),
            },
            summary,
            spans: spans.iter().map(SpanRecord::from_span).collect(),
        });
    }

    /// Sorts functions into a stable order, so that two runs over the same
    /// input produce byte-identical JSON regardless of the order a frontend
    /// happened to walk the class or module.
    pub fn finish(&mut self) {
        self.functions.sort_by(|a, b| {
            a.decl_line
                .cmp(&b.decl_line)
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    /// Attaches a file-scope score to every span already pushed, from a
    /// per-file map of line to file-scope score (see
    /// [`crate::filescope::file_scores`]).
    ///
    /// `by_file` is keyed the way [`FunctionRecord::file`] resolves: a
    /// function's own `file` when set, otherwise this sidecar's `file`. A
    /// span whose file has no entry, or whose lines are absent from that
    /// file's map — a sibling function skipped for being empty, invalid, or
    /// over `--max-instructions` never contributes lines — keeps
    /// `file_score: None` rather than guessing. A span spanning more than
    /// one scored line takes the highest of its lines' scores, the same
    /// `max`-over-the-run convention [`SpanRecord::score`] itself already
    /// uses.
    pub fn apply_file_scope(&mut self, by_file: &BTreeMap<String, BTreeMap<u32, f64>>) {
        let sidecar_file = self.file.clone();
        for f in &mut self.functions {
            let file = if f.file.is_empty() {
                &sidecar_file
            } else {
                &f.file
            };
            let Some(lines) = by_file.get(file) else {
                continue;
            };
            for s in &mut f.spans {
                let peak = (s.start..=s.end)
                    .filter_map(|line| lines.get(&line).copied())
                    .fold(None, |acc: Option<f64>, v| {
                        Some(acc.map_or(v, |a| a.max(v)))
                    });
                s.file_score = peak.map(|v| (v * 100.0).round() / 100.0);
            }
        }
    }

    /// The file-scope score covering `line`, if any function's map claims it
    /// and file scope was computed for this sidecar ([`Self::apply_file_scope`]).
    ///
    /// Mirrors [`Self::tier_at`]'s overlap handling: the highest score among
    /// spans that claim the line, since nested functions can overlap it.
    #[must_use]
    pub fn file_score_at(&self, line: u32) -> Option<f64> {
        self.functions
            .iter()
            .flat_map(|f| &f.spans)
            .filter(|s| line >= s.start && line <= s.end)
            .filter_map(|s| s.file_score)
            .fold(None, |acc, v| Some(acc.map_or(v, |a: f64| a.max(v))))
    }

    /// The tier covering `line`, if any function's map claims it.
    ///
    /// This is the query an edit-policy hook makes: given a line the agent
    /// wants to touch, how sensitive is it? Returns the most salient tier when
    /// nested functions overlap.
    #[must_use]
    pub fn tier_at(&self, line: u32) -> Option<&str> {
        let mut best: Option<(Tier, &str)> = None;
        for f in &self.functions {
            for s in &f.spans {
                if line >= s.start && line <= s.end {
                    let t = match s.tier.as_str() {
                        "core" => Tier::Core,
                        "boundary" => Tier::Boundary,
                        "plumbing" => Tier::Plumbing,
                        _ => Tier::Inert,
                    };
                    if best.is_none_or(|(bt, _)| t > bt) {
                        best = Some((t, s.tier.as_str()));
                    }
                }
            }
        }
        best.map(|(_, name)| name)
    }
}
