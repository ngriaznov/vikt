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
/// `SpanRecord::file_score` (added for `--scope file`), `SpanRecord::repo_score`
/// (added for `--scope repo`) and `SpanRecord::text` (added to embed a span's
/// source) are individually additive, not breaking changes: each is
/// optional, defaults to absent, and every struct here derives plain
/// `Serialize`/`Deserialize` with no `deny_unknown_fields`, so an old reader
/// ignores a new key and a new reader sees `None` on output that never set
/// it. Reserve schema bumps for changes that remove, rename, or repurpose an
/// existing field's meaning — exactly what v2 was: v1's per-span `score` is
/// `function_score` from v2 on, renamed so its scope is unambiguous next to
/// `file_score`. v3 is [`FunctionRecord::file`] going from
/// sometimes-omitted to unconditional (see its docs) — a genuine
/// presence-contract change existing readers may depend on, unlike `text`
/// riding along in the same bump.
pub const SCHEMA: &str = "vikt-sidecar/v3";

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
    pub function_score: f64,
    /// Percentile of that score within this function, `0.0..=1.0`. Use for
    /// heatmaps and for "show me the top of this body".
    pub rank: f64,
    /// File-scope score: [`function_score`](Self::function_score) reweighted by the owning
    /// function's call-graph standing among its file's other functions, then
    /// rank-normalised across every scored line of the file (see
    /// [`crate::filescope`]). Present only when the run asked for file
    /// scope (`vikt --scope file`); absent, and omitted from JSON entirely,
    /// under the default within-function scope. Not comparable across
    /// files — each file is re-ranked against itself alone.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file_score: Option<f64>,
    /// Repo-scope score: [`function_score`](Self::function_score) reweighted
    /// by the owning function's call-graph standing among *every* scored
    /// function of the run, cross-file edges included, then
    /// rank-normalised across every scored line of the whole run (see
    /// [`crate::filescope::repo_scores`]). Present only under `vikt --scope
    /// repo`; absent, and omitted from JSON entirely, otherwise. Unlike
    /// [`file_score`](Self::file_score), comparable across the files of one
    /// run — that is the point of the wider scope.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repo_score: Option<f64>,
    /// The verbatim source of the lines this span covers — [`start`](Self::start)
    /// through [`end`](Self::end) inclusive, each right-trimmed of trailing
    /// whitespace, newline-joined when the span covers more than one line.
    /// What `--annotate` already shows, embedded here so the JSON is
    /// self-describing without a second read of the source file. Present
    /// only when source text was available when this sidecar was built —
    /// always for a source-file frontend, only when `--annotate`'s source
    /// was supplied for a bytecode input; never fabricated as an empty
    /// string when it wasn't.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub text: Option<String>,
    /// Why this run got its tier.
    pub reasons: Vec<String>,
}

impl SpanRecord {
    /// `source_lines` is the owning function's file, pre-split on `\n` once
    /// by the caller rather than per span — see [`Sidecar::push_with_source`].
    fn from_span(s: &LineSpan, source_lines: Option<&[&str]>) -> Self {
        Self {
            start: s.start,
            end: s.end,
            tier: s.tier.name().to_owned(),
            // Two decimals: the score is a ranking signal, and pretending to
            // more precision than the weights justify invites false diffs
            // between runs on trivially different inputs.
            function_score: (s.score * 100.0).round() / 100.0,
            rank: (s.rank * 100.0).round() / 100.0,
            file_score: None,
            repo_score: None,
            text: source_lines.and_then(|lines| span_text(lines, s.start, s.end)),
            reasons: s.reasons.clone(),
        }
    }
}

/// The verbatim text of `lines[start..=end]` (1-based, inclusive), each line
/// right-trimmed of trailing whitespace, newline-joined for a multi-line
/// span. `None` when `start`/`end` don't both land inside `lines` — a
/// source file that has drifted out of sync with the analyzed one (or is
/// simply the wrong file) must not fabricate or misattribute text.
fn span_text(lines: &[&str], start: u32, end: u32) -> Option<String> {
    let start_idx = usize::try_from(start.checked_sub(1)?).ok()?;
    let end_idx = usize::try_from(end.checked_sub(1)?).ok()?;
    let span = lines.get(start_idx..=end_idx)?;
    Some(
        span.iter()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n"),
    )
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
    /// Source file this function's lines refer to. Unconditional as of
    /// schema v3: every record names its own file, so a consumer reading
    /// one function at a time never has to cross-reference the sidecar-level
    /// `file` to find out which source it belongs to — the gap that let a
    /// multi-file (folder/repo) run emit one function record with no `file`
    /// at all, whenever that function's file happened to match whichever
    /// file [`Sidecar::file`] was set from. Before v3 this was omitted when
    /// it matched the sidecar-level `file`, to keep single-file output
    /// byte-identical to before the field existed. `#[serde(default)]` so a
    /// v2 sidecar predating this change — which may still omit it — still
    /// deserializes, falling back to the empty string exactly as v2 readers
    /// already had to handle.
    #[serde(default)]
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

    /// Appends the analysis of one function. Spans carry no [`SpanRecord::text`] —
    /// equivalent to [`Self::push_with_source`] with `source: None`, for a
    /// caller with no source text on hand (or that doesn't want it embedded).
    pub fn push(&mut self, ir: &FunctionIr, sal: &FunctionImportance) {
        self.push_with_source(ir, sal, None);
    }

    /// [`Self::push`], additionally embedding each span's [`SpanRecord::text`]
    /// from `source` — the full text of the file `ir` was lowered from.
    /// `None` when no source text is available for this function (a
    /// bytecode input with no `--annotate` source supplied): every span's
    /// `text` is then `None` too, never a fabricated empty string.
    pub fn push_with_source(
        &mut self,
        ir: &FunctionIr,
        sal: &FunctionImportance,
        source: Option<&str>,
    ) {
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
        let source_lines: Option<Vec<&str>> = source.map(|s| s.lines().collect());
        self.functions.push(FunctionRecord {
            file: ir.id.file.clone(),
            name: ir.id.name.clone(),
            signature: ir.id.signature.clone(),
            decl_line: ir.id.decl_line,
            coverage: Coverage {
                instructions: ir.nodes.len(),
                with_line: ir.nodes.iter().filter(|n| n.line.is_some()).count(),
            },
            summary,
            spans: spans
                .iter()
                .map(|s| SpanRecord::from_span(s, source_lines.as_deref()))
                .collect(),
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
    /// See [`Self::apply_scope`] for the shared file-lookup and
    /// nothing-guessed contract; this is that helper writing into
    /// [`SpanRecord::file_score`].
    pub fn apply_file_scope(&mut self, by_file: &BTreeMap<String, BTreeMap<u32, f64>>) {
        self.apply_scope(by_file, |s, v| s.file_score = v);
    }

    /// Attaches a repo-scope score to every span already pushed, from a
    /// per-file map of line to repo-scope score — [`crate::filescope::repo_scores`]'s
    /// `(file, line)` keys regrouped by file, the shape [`Self::apply_scope`]
    /// expects.
    ///
    /// See [`Self::apply_scope`] for the shared file-lookup and
    /// nothing-guessed contract; this is that helper writing into
    /// [`SpanRecord::repo_score`].
    pub fn apply_repo_scope(&mut self, by_file: &BTreeMap<String, BTreeMap<u32, f64>>) {
        self.apply_scope(by_file, |s, v| s.repo_score = v);
    }

    /// Shared core behind [`Self::apply_file_scope`] and
    /// [`Self::apply_repo_scope`]: writes `set`'s target from `by_file`,
    /// keyed the way [`FunctionRecord::file`] resolves — a function's own
    /// `file` when set, otherwise this sidecar's `file`. A span whose file
    /// has no entry, or whose lines are absent from that file's map — a
    /// sibling function skipped for being empty, invalid, or over
    /// `--max-instructions` never contributes lines — is left `None` rather
    /// than guessed. A span spanning more than one scored line takes the
    /// highest of its lines' scores, the same `max`-over-the-run convention
    /// [`SpanRecord::function_score`] itself already uses.
    fn apply_scope(
        &mut self,
        by_file: &BTreeMap<String, BTreeMap<u32, f64>>,
        set: impl Fn(&mut SpanRecord, Option<f64>),
    ) {
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
                set(s, peak.map(|v| (v * 100.0).round() / 100.0));
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

    /// The repo-scope score covering `line`, mirroring [`Self::file_score_at`]
    /// for [`SpanRecord::repo_score`].
    #[must_use]
    pub fn repo_score_at(&self, line: u32) -> Option<f64> {
        self.functions
            .iter()
            .flat_map(|f| &f.spans)
            .filter(|s| line >= s.start && line <= s.end)
            .filter_map(|s| s.repo_score)
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

#[cfg(test)]
mod tests {
    use super::span_text;

    #[test]
    fn single_line_span_is_right_trimmed() {
        let lines = ["fn hub(x) {   ", "    x + 1", "}"];
        assert_eq!(
            span_text(&lines, 1, 1).as_deref(),
            Some("fn hub(x) {"),
            "trailing whitespace on the covered line must not survive into `text`"
        );
    }

    #[test]
    fn multi_line_span_is_newline_joined_and_each_line_right_trimmed() {
        let lines = ["fn hub(x) {  ", "    x + 1   ", "}"];
        assert_eq!(
            span_text(&lines, 1, 3).as_deref(),
            Some("fn hub(x) {\n    x + 1\n}"),
            "every covered line must be right-trimmed on its own, then newline-joined"
        );
    }

    #[test]
    fn span_past_the_end_of_the_file_is_none_not_fabricated() {
        let lines = ["only line"];
        assert_eq!(
            span_text(&lines, 1, 2),
            None,
            "a span whose end line doesn't exist must not fabricate partial text"
        );
        assert_eq!(span_text(&lines, 5, 5), None);
    }

    #[test]
    fn zero_line_is_none() {
        // 1-based lines: `start: 0` cannot occur from a real lowering, but
        // must not underflow or panic if it somehow did.
        let lines = ["a"];
        assert_eq!(span_text(&lines, 0, 0), None);
    }
}
