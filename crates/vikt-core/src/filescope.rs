//! File-scope importance: a function-importance layer over the intra-file
//! call graph, blended with the within-function line scores every function
//! already carries.
//!
//! Tiers and within-function scores are untouched — this module only
//! re-ranks lines *across* a file's functions, using facts a single-function
//! analysis cannot see: which functions are called by others, and how deep
//! the call graph runs beneath each one.
//!
//! # The call graph
//!
//! Nodes are the file's functions. An edge `A -> B` exists when a [`Call`]
//! node in `A`'s body names `B`. Matching is conservative and
//! frontend-agnostic — core has no notion of any particular substrate's
//! qualname format, so it applies one rule uniformly: exact equality of the
//! callee string against a sibling's [`FunctionId::name`](crate::ir::FunctionId::name),
//! or, failing that, an unambiguous match on the trailing `::`/`.`-separated
//! segment. A callee matching more than one candidate, under either rule,
//! resolves to nothing rather than guessing. Same-file candidates are tried
//! before the search ever widens past that file — see [`resolve_callee`]'s
//! docs on why, which matters once [`repo_scores`] is in play.
//!
//! # Function features
//!
//! Four per-function signals, each rank-normalised to `[0, 1]` across the
//! file's functions ([`crate::panel`]'s `rank01`, the same normalisation the
//! line panel uses):
//!
//! - **call-graph depth** — a food-web-style trophic level over the call
//!   graph: a function calling nothing is basal (`1.0`); a function calling
//!   only basal functions sits one level up; a mutually recursive cluster
//!   collapses to one shared level, derived from whatever it calls outside
//!   itself (or `1.0` if nothing does).
//! - **fan-in** — the count of distinct other functions that call it.
//! - **size share** — its count of scored lines.
//! - **boundary density** — the fraction of its scored lines whose strongest
//!   tier is [`Tier::Boundary`].
//!
//! A function's weight is the mean of its four ranks. [`file_scores`]
//! multiplies each line's within-function score by its owning function's
//! weight, then rank-normalises the products across every scored line in the
//! file. A line owned by more than one function (nested definitions sharing
//! source lines) is attributed to the innermost — the function whose scored
//! lines span the narrowest range — mirroring the attribution rule
//! `vikt-cli`'s calibration already uses for the same situation.
//!
//! # Repo scope
//!
//! [`repo_scores`] is the same apparatus one rung up, not a second
//! implementation: every function below already treats `functions` as an
//! opaque slice and never assumes its members share a file — `resolve_edges`
//! matches callees by name across the whole slice regardless of which
//! function came from where (preferring a same-file candidate over a
//! same-named one elsewhere, per [`resolve_callee`]'s docs), and
//! [`function_features`]/[`function_weights`] are already correct for a
//! cross-file call graph. The one thing that *does* assume a single file is
//! bare line numbers as a key: two different files can each have a line 10.
//! [`repo_scores`] and [`line_owners_by_file`]
//! key on `(file, line)` instead — the file coming from each function's own
//! [`FunctionIr::id`]'s `file` — so the identical weighting and
//! rank-normalisation can run once across every scored function in a repo or
//! folder input, letting a call cross a file boundary and letting the
//! re-rank see every file's lines at once. [`file_scores`] is recovered by
//! calling this with one file's functions and dropping the (redundant) file
//! component of the key, which is exactly what its callers already did
//! before this module supported more than one file at a time.
//!
//! [`Call`]: crate::ir::NodeKind::Call

use std::collections::{BTreeMap, BTreeSet};

use crate::graph::strongly_connected;
use crate::importance::{FunctionImportance, Tier};
use crate::ir::{FunctionIr, NodeKind};
use crate::panel::rank01;

/// One function's contribution to a file-scope pass.
///
/// `line_scores` is whatever within-function per-line score the caller
/// already has — typically [`crate::panel::score`]'s output projected to
/// lines, but any `0.0..=1.0` scoring works, since this layer only reweights
/// and re-ranks it. `importance` must be the result of analyzing `ir`, used
/// here only for its tiers (boundary density).
#[derive(Debug, Clone, Copy)]
pub struct ScopedFunction<'a> {
    /// The function's lowered body, source of the call graph.
    pub ir: &'a FunctionIr,
    /// Its tiered analysis, source of boundary density.
    pub importance: &'a FunctionImportance,
    /// Its within-function per-line score, source of what gets reweighted.
    pub line_scores: &'a BTreeMap<u32, f64>,
}

/// File-scope line score: each function's within-function [`ScopedFunction::line_scores`]
/// multiplied by its call-graph weight ([`function_weights`]), then
/// rank-normalised to `[0, 1]` across every scored line of the file.
///
/// A line scored by more than one function is attributed to the innermost —
/// the function with the narrowest span of scored lines, ties broken by
/// earlier position in `functions`.
///
/// Assumes every function in `functions` shares one file — every caller
/// groups by file before calling, which is what makes stripping the (here,
/// constant) file component of [`blended_scores`]'s key lossless. Passing
/// functions from more than one file collapses same-numbered lines from
/// different files onto one key; that generalisation is [`repo_scores`].
#[must_use]
pub fn file_scores(functions: &[ScopedFunction<'_>]) -> BTreeMap<u32, f64> {
    blended_scores(functions)
        .into_iter()
        .map(|((_, line), v)| (line, v))
        .collect()
}

/// [`file_scores`]'s cross-file generalisation: the identical call-graph
/// weighting and rank-normalisation, but computed once over every scored
/// function passed in — which may span the whole repository or folder input
/// — and keyed by `(file, line)` since two different files can reuse the
/// same line number. Where a [`file_scores`] caller restricts `functions` to
/// one file (blocking the call graph at that file's boundary), a
/// [`repo_scores`] caller passes every scored function at once, so
/// `resolve_callee` can match a call across a file boundary and the
/// re-ranking runs over every file's lines together.
#[must_use]
pub fn repo_scores(functions: &[ScopedFunction<'_>]) -> BTreeMap<(String, u32), f64> {
    blended_scores(functions)
}

/// The shared core behind [`file_scores`] and [`repo_scores`]: each
/// function's within-function line score multiplied by its call-graph
/// weight, then rank-normalised across every scored line among `functions`,
/// keyed by `(file, line)`.
fn blended_scores(functions: &[ScopedFunction<'_>]) -> BTreeMap<(String, u32), f64> {
    let weights = function_weights(functions);
    let owners = line_owners_by_file(functions);
    let keys: Vec<(String, u32)> = owners.keys().cloned().collect();
    let raw: Vec<f64> = keys
        .iter()
        .map(|key @ (_, line)| {
            let i = owners[key];
            weights[i] * functions[i].line_scores.get(line).copied().unwrap_or(0.0)
        })
        .collect();
    keys.into_iter().zip(rank01(&raw)).collect()
}

/// Per-line owning function index into `functions`, by the attribution rule
/// [`file_scores`] blends with: narrowest scored-line extent wins, the
/// first-encountered function breaking ties. Public so consumers reporting
/// per-line data (calibration's dataset rows) attribute it identically.
///
/// Assumes every function in `functions` shares one file — see
/// [`file_scores`]'s note; [`line_owners_by_file`] is the generalisation
/// that keeps the file component.
#[must_use]
pub fn line_owners(functions: &[ScopedFunction<'_>]) -> BTreeMap<u32, usize> {
    line_owners_by_file(functions)
        .into_iter()
        .map(|((_, line), i)| (line, i))
        .collect()
}

/// [`line_owners`]'s cross-file generalisation, keyed by `(file, line)` —
/// the attribution [`repo_scores`] blends with.
#[must_use]
pub fn line_owners_by_file(functions: &[ScopedFunction<'_>]) -> BTreeMap<(String, u32), usize> {
    let mut owner: BTreeMap<(String, u32), (usize, u32)> = BTreeMap::new();
    for (i, f) in functions.iter().enumerate() {
        let Some(&lo) = f.line_scores.keys().next() else {
            continue;
        };
        let hi = *f.line_scores.keys().next_back().unwrap_or(&lo);
        let width = hi - lo;
        for &line in f.line_scores.keys() {
            let key = (f.ir.id.file.clone(), line);
            owner
                .entry(key)
                .and_modify(|cur| {
                    if width < cur.1 {
                        *cur = (i, width);
                    }
                })
                .or_insert((i, width));
        }
    }
    owner.into_iter().map(|(key, (i, _))| (key, i)).collect()
}

/// A function's four rank-normalised call-graph signals, individually —
/// [`function_weights`]'s per-function mean, broken apart so calibration and
/// the dataset writer can report or refit them separately.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FunctionFeatures {
    /// Call-graph depth: the unweighted analogue of [`crate::trophic`]'s
    /// per-statement derivation depth, rank-normalised.
    pub trophic: f64,
    /// Distinct-caller count, rank-normalised.
    pub fan_in: f64,
    /// Scored-line count, rank-normalised.
    pub size_share: f64,
    /// Fraction of scored lines at [`Tier::Boundary`], rank-normalised.
    pub boundary_density: f64,
}

impl FunctionFeatures {
    /// The mean of the four signals — [`function_weights`]'s value for this
    /// function.
    #[must_use]
    pub fn mean(self) -> f64 {
        (self.trophic + self.fan_in + self.size_share + self.boundary_density) / 4.0
    }
}

/// Per-function call-graph features, parallel to `functions`: call-graph
/// depth, fan-in, size share and boundary density, each rank-normalised to
/// `[0, 1]` across `functions`.
#[must_use]
pub fn function_features(functions: &[ScopedFunction<'_>]) -> Vec<FunctionFeatures> {
    let n = functions.len();
    if n == 0 {
        return Vec::new();
    }
    let edges = resolve_edges(functions);

    let trophic = rank01(&trophic_levels(&edges));
    let fan_in = rank01(&fan_in(&edges));
    let size: Vec<f64> = functions
        .iter()
        .map(|f| f.line_scores.len() as f64)
        .collect();
    let size_share = rank01(&size);
    let boundary: Vec<f64> = functions.iter().map(boundary_density).collect();
    let boundary_density = rank01(&boundary);

    (0..n)
        .map(|i| FunctionFeatures {
            trophic: trophic[i],
            fan_in: fan_in[i],
            size_share: size_share[i],
            boundary_density: boundary_density[i],
        })
        .collect()
}

/// Per-function call-graph weight, parallel to `functions`: the mean of four
/// rank-normalised features ([`FunctionFeatures`]). Exposed separately from
/// [`file_scores`] so calibration and the dataset writer can pair a
/// function's own weight with its features.
#[must_use]
pub fn function_weights(functions: &[ScopedFunction<'_>]) -> Vec<f64> {
    function_features(functions)
        .into_iter()
        .map(FunctionFeatures::mean)
        .collect()
}

/// Resolved call edges: `edges[i]` is the set of function indices `i` calls,
/// by the matching rule documented on the module.
fn resolve_edges(functions: &[ScopedFunction<'_>]) -> Vec<BTreeSet<usize>> {
    let names: Vec<&str> = functions.iter().map(|f| f.ir.id.name.as_str()).collect();
    let files: Vec<&str> = functions.iter().map(|f| f.ir.id.file.as_str()).collect();
    functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            f.ir.nodes
                .iter()
                .filter_map(|node| match &node.kind {
                    NodeKind::Call { callee, .. } => {
                        resolve_callee(callee, files[i], &names, &files)
                    }
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// Resolves one callee string to a unique sibling, or declines.
///
/// Same-file candidates are tried first and, when any exist, are the only
/// ones considered — a caller's own file defining the name it calls is a
/// far stronger signal than a same-named function anywhere else in the
/// repo, and letting a coincidental repo-wide namesake compete with it
/// would turn a previously-unambiguous file-scope match into a declined
/// one the moment [`repo_scores`] pulls in a same-named function from an
/// unrelated file. Only when the caller's own file has no candidate at all
/// does the search widen to every file passed in — matching a call across
/// a file boundary being the entire point of that generalisation (see the
/// module docs). Exact name equality is tried first at each tier; if it
/// matches more than one candidate in that tier (only possible with
/// duplicate names), that call site resolves to nothing rather than
/// picking one arbitrarily. Otherwise falls back, within the same tier, to
/// the trailing `::`/`.`-separated segment, again only when exactly one
/// candidate's own trailing segment matches.
fn resolve_callee(
    callee: &str,
    caller_file: &str,
    names: &[&str],
    files: &[&str],
) -> Option<usize> {
    let same_file: Vec<usize> = (0..names.len())
        .filter(|&i| files[i] == caller_file)
        .collect();
    resolve_in(callee, &same_file, names)
        .or_else(|| resolve_in(callee, &(0..names.len()).collect::<Vec<_>>(), names))
}

/// [`resolve_callee`]'s single-tier search: exact match, then trailing
/// segment, over exactly the candidate indices given.
fn resolve_in(callee: &str, candidates: &[usize], names: &[&str]) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    let mut exact = candidates.iter().copied().filter(|&i| names[i] == callee);
    if let Some(only) = exact.next() {
        return if exact.next().is_none() {
            Some(only)
        } else {
            None
        };
    }
    let suffix = last_segment(callee);
    if suffix.is_empty() {
        return None;
    }
    let mut matches = candidates
        .iter()
        .copied()
        .filter(|&i| last_segment(names[i]) == suffix);
    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
    }
}

/// The trailing `::`- or `.`-separated segment of a qualified name.
fn last_segment(name: &str) -> &str {
    name.rsplit(['.', ':']).next().unwrap_or(name)
}

/// Distinct-caller count per function, self-calls excluded.
fn fan_in(edges: &[BTreeSet<usize>]) -> Vec<f64> {
    let mut counts = vec![0u32; edges.len()];
    for (i, callees) in edges.iter().enumerate() {
        for &j in callees {
            if j != i {
                counts[j] += 1;
            }
        }
    }
    counts.into_iter().map(f64::from).collect()
}

/// Raw (unnormalised) call-graph depth per function: `1.0` for a function
/// that calls nothing (or only itself); otherwise `1.0` plus the deepest
/// level among what it calls. Mutually recursive functions form one strongly
/// connected component and share a single level, derived from whatever the
/// component calls outside itself (`1.0` if nothing does) — a longest-path
/// computation over the call graph's DAG condensation, the unweighted
/// analogue of [`crate::trophic`]'s per-statement derivation depth.
fn trophic_levels(edges: &[BTreeSet<usize>]) -> Vec<f64> {
    let n = edges.len();
    // Callees of each function, self-calls dropped: a function does not
    // derive its depth from itself.
    let callees: Vec<Vec<usize>> = edges
        .iter()
        .enumerate()
        .map(|(i, set)| set.iter().copied().filter(|&j| j != i).collect())
        .collect();

    // Producer -> consumer adjacency for SCC discovery, the same orientation
    // crate::trophic builds: an edge u -> v here means v calls u.
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v, called) in callees.iter().enumerate() {
        for &u in called {
            succ[u].push(v);
        }
    }
    let (comp_of, comps) = strongly_connected(n, &succ);

    // strongly_connected emits sinks (nothing calls them) first; a
    // function's depth needs its callees solved first, so walk components in
    // reverse, exactly as crate::trophic does over the identical adjacency
    // convention.
    let mut level = vec![0.0f64; n];
    for members in comps.iter().rev() {
        match members.as_slice() {
            [v] if callees[*v].is_empty() => level[*v] = 1.0,
            [v] => {
                level[*v] = 1.0
                    + callees[*v]
                        .iter()
                        .map(|&u| level[u])
                        .fold(f64::NEG_INFINITY, f64::max);
            }
            members => solve_component(members, &comp_of, &callees, &mut level),
        }
    }
    level
}

/// A mutually recursive cluster: every member shares one level, derived from
/// whichever external callee (outside the component) sits deepest, or `1.0`
/// if the component calls nothing outside itself — the same "no primary
/// producer" convention [`crate::trophic`] uses for an isolated cycle.
fn solve_component(
    members: &[usize],
    comp_of: &[usize],
    callees: &[Vec<usize>],
    level: &mut [f64],
) {
    let component = comp_of[members[0]];
    let mut deepest_outside = f64::NEG_INFINITY;
    for &v in members {
        for &u in &callees[v] {
            if comp_of[u] != component {
                deepest_outside = deepest_outside.max(level[u]);
            }
        }
    }
    let assigned = if deepest_outside.is_finite() {
        1.0 + deepest_outside
    } else {
        1.0
    };
    for &v in members {
        level[v] = assigned;
    }
}

/// Fraction of `f`'s scored lines whose strongest tier is [`Tier::Boundary`],
/// `0.0` if it has none.
fn boundary_density(f: &ScopedFunction<'_>) -> f64 {
    if f.line_scores.is_empty() {
        return 0.0;
    }
    let tiers = line_tiers(f.ir, f.importance);
    let boundary = f
        .line_scores
        .keys()
        .filter(|line| tiers.get(line) == Some(&Tier::Boundary))
        .count();
    boundary as f64 / f.line_scores.len() as f64
}

/// Strongest tier per source line, `max` over the line's non-structural
/// nodes — the same projection [`crate::importance::project_to_lines`] and
/// [`crate::panel::line_features`] use.
fn line_tiers(ir: &FunctionIr, importance: &FunctionImportance) -> BTreeMap<u32, Tier> {
    let mut tiers: BTreeMap<u32, Tier> = BTreeMap::new();
    for ns in &importance.nodes {
        let node = &ir.nodes[ns.node];
        if node.is_structural() {
            continue;
        }
        let Some(line) = node.line else { continue };
        tiers
            .entry(line)
            .and_modify(|t| {
                if ns.tier > *t {
                    *t = ns.tier;
                }
            })
            .or_insert(ns.tier);
    }
    tiers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importance::{Denylist, ScoreWeights, analyze};
    use crate::ir::{CallOpacity, FunctionId, Node};

    fn fid(name: &str) -> FunctionId {
        FunctionId {
            file: "File.py".into(),
            name: name.into(),
            signature: String::new(),
            decl_line: Some(1),
        }
    }

    /// A hub function with real work inside it - including a call to its own
    /// private `helper` - called in turn by three thin wrappers, plus two
    /// unrelated leaf functions (call nothing, called by nothing). Every
    /// function occupies its own disjoint line range, as distinct functions
    /// in one file always do.
    ///
    /// `helper` matters structurally, not just narratively: without it, call-
    /// graph depth places the hub at one extreme and every wrapper at the
    /// other - the exact mirror image of fan-in and size share, which place
    /// the hub at *their* high extreme. Two features favouring a bipartition
    /// one way and two favouring the same bipartition the other way average
    /// to the same `0.5` for both groups, by a `rank01` identity, regardless
    /// of the actual values - not a property of file scope, just what
    /// happens when every feature reduces to the same two-group split. Once
    /// the hub itself calls something, call-graph depth separates hub from
    /// wrapper into three groups instead of two, breaking that coincidence,
    /// the same way it would in any real file where call depth doesn't
    /// perfectly correlate with every other structural signal.
    fn hub_and_wrappers() -> Vec<FunctionIr> {
        let hub = FunctionIr {
            id: fid("hub"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]),
                Node::pure(2).with_dataflow([2], [1]).with_succs([2]),
                Node::pure(3).with_dataflow([3], [2]).with_succs([3]),
                Node {
                    line: Some(4),
                    kind: NodeKind::Call {
                        callee: "helper".into(),
                        opacity: CallOpacity::Opaque,
                    },
                    defs: vec![4],
                    uses: vec![3],
                    succs: vec![4],
                    label: String::new(),
                },
                Node::pure(5)
                    .with_dataflow([], [4])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let helper = FunctionIr {
            id: fid("helper"),
            nodes: vec![
                Node::pure(60).with_dataflow([1], []).with_succs([1]),
                Node::pure(61)
                    .with_dataflow([], [1])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let call_node = |line: u32| Node {
            line: Some(line),
            kind: NodeKind::Call {
                callee: "hub".into(),
                opacity: CallOpacity::Opaque,
            },
            defs: Vec::new(),
            uses: Vec::new(),
            succs: Vec::new(),
            label: String::new(),
        };
        let wrapper_a = FunctionIr {
            id: fid("wrapper_a"),
            nodes: vec![call_node(10)],
            entry: 0,
        };
        let wrapper_b = FunctionIr {
            id: fid("wrapper_b"),
            nodes: vec![
                Node::pure(20).with_dataflow([9], []).with_succs([1]),
                call_node(21),
            ],
            entry: 0,
        };
        let wrapper_c = FunctionIr {
            id: fid("wrapper_c"),
            nodes: vec![
                Node::pure(30).with_dataflow([8], []).with_succs([1]),
                call_node(31),
            ],
            entry: 0,
        };
        let leaf1 = FunctionIr {
            id: fid("leaf1"),
            nodes: vec![
                Node::pure(40).with_dataflow([7], []).with_succs([1]),
                Node::pure(41)
                    .with_dataflow([], [7])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let leaf2 = FunctionIr {
            id: fid("leaf2"),
            nodes: vec![
                Node::pure(50).with_dataflow([6], []).with_succs([1]),
                Node::pure(51)
                    .with_dataflow([], [6])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        vec![hub, helper, wrapper_a, wrapper_b, wrapper_c, leaf1, leaf2]
    }

    /// Hand-crafted within-function line scores, linearly increasing to
    /// `1.0` at the function's last line - decoupled from the plain
    /// `analyze` score on purpose, the same way calibrate.rs's panel score
    /// and plain `analyze` score are two different things over one `ir`.
    fn line_scores_from_lines(ir: &FunctionIr) -> BTreeMap<u32, f64> {
        let lines: Vec<u32> = ir.lines().into_iter().collect();
        let n = lines.len().max(1) as f64;
        lines
            .into_iter()
            .enumerate()
            .map(|(i, l)| (l, (i as f64 + 1.0) / n))
            .collect()
    }

    /// Every function of [`hub_and_wrappers`], already analyzed and given
    /// hand-fed line scores, owned so tests can borrow a [`ScopedFunction`]
    /// slice from it without juggling six separate bindings each.
    struct Fixture {
        irs: Vec<FunctionIr>,
        sals: Vec<FunctionImportance>,
        lines: Vec<BTreeMap<u32, f64>>,
    }

    impl Fixture {
        fn build() -> Self {
            let irs = hub_and_wrappers();
            let denylist = Denylist::new();
            let weights = ScoreWeights::default();
            let sals: Vec<_> = irs
                .iter()
                .map(|ir| analyze(ir, &denylist, &weights))
                .collect();
            let lines: Vec<_> = irs.iter().map(line_scores_from_lines).collect();
            Self { irs, sals, lines }
        }

        fn scoped(&self) -> Vec<ScopedFunction<'_>> {
            (0..self.irs.len())
                .map(|i| ScopedFunction {
                    ir: &self.irs[i],
                    importance: &self.sals[i],
                    line_scores: &self.lines[i],
                })
                .collect()
        }
    }

    #[test]
    fn hub_outranks_its_wrappers_under_file_scope() {
        let fx = Fixture::build();

        // Each function peaks at 1.0 within itself.
        for m in &fx.lines {
            let peak = m.values().copied().fold(f64::MIN, f64::max);
            assert!((peak - 1.0).abs() < 1e-9);
        }

        // Every function's lines occupy its own disjoint range, so the
        // flat file-wide map keys them without collision.
        let functions = fx.scoped();
        let scored = file_scores(&functions);

        let hub_peak = scored[&5]; // hub's last line, where its hand-fed score peaks
        for &wrapper_peak_line in &[10u32, 21, 31] {
            let wrapper_peak = scored[&wrapper_peak_line];
            assert!(
                hub_peak > wrapper_peak,
                "hub's peak line {hub_peak} should outrank wrapper line {wrapper_peak_line}'s peak {wrapper_peak}"
            );
        }
    }

    #[test]
    fn ambiguous_call_targets_add_no_edges() {
        // Two siblings share a trailing segment ("a.foo" and "b.foo"), so a
        // bare "foo" callee cannot resolve to either one.
        let caller = FunctionIr {
            id: fid("caller"),
            nodes: vec![Node {
                line: Some(1),
                kind: NodeKind::Call {
                    callee: "foo".into(),
                    opacity: CallOpacity::Opaque,
                },
                defs: Vec::new(),
                uses: Vec::new(),
                succs: Vec::new(),
                label: String::new(),
            }],
            entry: 0,
        };
        let a = FunctionIr {
            id: fid("a.foo"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let b = FunctionIr {
            id: fid("b.foo"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let functions = [&caller, &a, &b];
        let names: Vec<&str> = functions.iter().map(|f| f.id.name.as_str()).collect();
        let files: Vec<&str> = functions.iter().map(|f| f.id.file.as_str()).collect();
        for node in &caller.nodes {
            if let NodeKind::Call { callee, .. } = &node.kind {
                assert_eq!(
                    resolve_callee(callee, caller.id.file.as_str(), &names, &files),
                    None
                );
            }
        }
    }

    #[test]
    fn single_function_file_degrades_to_within_function_order() {
        let ir = FunctionIr {
            id: fid("solo"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]),
                Node::pure(2)
                    .with_dataflow([], [1])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let sal = analyze(&ir, &Denylist::new(), &ScoreWeights::default());
        let mut lines = BTreeMap::new();
        lines.insert(1u32, 0.2);
        lines.insert(2u32, 0.9);
        let functions = [ScopedFunction {
            ir: &ir,
            importance: &sal,
            line_scores: &lines,
        }];
        let scored = file_scores(&functions);
        assert!(
            scored[&2] > scored[&1],
            "order must be preserved: {scored:?}"
        );
    }

    #[test]
    fn deterministic() {
        let fx = Fixture::build();
        let functions = fx.scoped();
        let a = file_scores(&functions);
        let b = file_scores(&functions);
        assert_eq!(a, b, "file_scores was not deterministic");
        let w1 = function_weights(&functions);
        let w2 = function_weights(&functions);
        assert_eq!(w1, w2, "function_weights was not deterministic");
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(file_scores(&[]).is_empty());
        assert!(function_weights(&[]).is_empty());
    }

    fn fid_in(file: &str, name: &str) -> FunctionId {
        FunctionId {
            file: file.into(),
            name: name.into(),
            signature: String::new(),
            decl_line: Some(1),
        }
    }

    fn call(line: u32, callee: &str) -> Node {
        Node {
            line: Some(line),
            kind: NodeKind::Call {
                callee: callee.into(),
                opacity: CallOpacity::Opaque,
            },
            defs: Vec::new(),
            uses: Vec::new(),
            succs: Vec::new(),
            label: String::new(),
        }
    }

    /// A hub with real work in its body - including a call to its own
    /// private `helper`, in the same file - called by two thin wrappers
    /// each in its *own* file: the shape `--scope repo` exists for.
    /// [`resolve_edges`] matches by name across the whole slice regardless
    /// of file, so this call graph edge would never appear if each
    /// wrapper's file were scored (and its call graph built) independently,
    /// the way `--scope file` does. The hub calling `helper` is not
    /// decoration: it is what [`hub_and_wrappers`]'s own doc comment
    /// explains keeps call-graph depth from collapsing hub and wrappers
    /// into the same two-group `rank01` tie that fan-in and size share
    /// would otherwise produce on their own.
    fn hub_across_files() -> Vec<FunctionIr> {
        let hub = FunctionIr {
            id: fid_in("hub.py", "hub"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]),
                Node::pure(2).with_dataflow([2], [1]).with_succs([2]),
                Node::pure(3).with_dataflow([3], [2]).with_succs([3]),
                Node {
                    line: Some(4),
                    kind: NodeKind::Call {
                        callee: "helper".into(),
                        opacity: CallOpacity::Opaque,
                    },
                    defs: vec![4],
                    uses: vec![3],
                    succs: vec![4],
                    label: String::new(),
                },
                Node::pure(5)
                    .with_dataflow([], [4])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let helper = FunctionIr {
            id: fid_in("hub.py", "helper"),
            nodes: vec![
                Node::pure(6).with_dataflow([1], []).with_succs([1]),
                Node::pure(7)
                    .with_dataflow([], [1])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let wrapper_a = FunctionIr {
            id: fid_in("wrapper_a.py", "wrapper_a"),
            nodes: vec![call(1, "hub")],
            entry: 0,
        };
        let wrapper_b = FunctionIr {
            id: fid_in("wrapper_b.py", "wrapper_b"),
            nodes: vec![call(1, "hub")],
            entry: 0,
        };
        vec![hub, helper, wrapper_a, wrapper_b]
    }

    #[test]
    fn repo_scores_resolves_calls_across_file_boundaries() {
        let irs = hub_across_files();
        let denylist = Denylist::new();
        let weights = ScoreWeights::default();
        let sals: Vec<_> = irs
            .iter()
            .map(|ir| analyze(ir, &denylist, &weights))
            .collect();
        let lines: Vec<_> = irs.iter().map(line_scores_from_lines).collect();
        let functions: Vec<ScopedFunction<'_>> = (0..irs.len())
            .map(|i| ScopedFunction {
                ir: &irs[i],
                importance: &sals[i],
                line_scores: &lines[i],
            })
            .collect();

        let scored = repo_scores(&functions);
        let hub_peak = scored[&("hub.py".to_owned(), 5)];
        let peak_a = scored[&("wrapper_a.py".to_owned(), 1)];
        let peak_b = scored[&("wrapper_b.py".to_owned(), 1)];
        assert!(
            hub_peak > peak_a && hub_peak > peak_b,
            "hub {hub_peak} should outrank its cross-file callers ({peak_a}, {peak_b})"
        );

        // Fan-in only counts once resolve_edges can see both wrappers' call
        // sites, which only happens when every function - regardless of
        // file - is scored in one pass. A per-file pass (what --scope file
        // does with this same fixture) would leave the hub with zero
        // in-file callers, since each wrapper's own file never contains the
        // hub that calls it.
        let features = function_features(&functions);
        assert!(
            features[0].fan_in > features[2].fan_in,
            "hub's fan-in must reflect both cross-file callers: {features:?}"
        );
    }

    #[test]
    fn repo_scores_keys_same_numbered_lines_from_different_files_separately() {
        let a = FunctionIr {
            id: fid_in("a.py", "solo_a"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let b = FunctionIr {
            id: fid_in("b.py", "solo_b"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let denylist = Denylist::new();
        let weights = ScoreWeights::default();
        let sal_a = analyze(&a, &denylist, &weights);
        let sal_b = analyze(&b, &denylist, &weights);
        let mut lines_a = BTreeMap::new();
        lines_a.insert(1u32, 0.3);
        let mut lines_b = BTreeMap::new();
        lines_b.insert(1u32, 0.9);
        let functions = [
            ScopedFunction {
                ir: &a,
                importance: &sal_a,
                line_scores: &lines_a,
            },
            ScopedFunction {
                ir: &b,
                importance: &sal_b,
                line_scores: &lines_b,
            },
        ];
        let scored = repo_scores(&functions);
        assert_eq!(
            scored.len(),
            2,
            "both files' line 1 must survive as distinct keys"
        );
        assert!(scored.contains_key(&("a.py".to_owned(), 1)));
        assert!(scored.contains_key(&("b.py".to_owned(), 1)));
    }

    #[test]
    fn repo_scores_empty_input_is_empty() {
        assert!(repo_scores(&[]).is_empty());
    }

    /// A caller's own-file definition of the name it calls must win over a
    /// same-named function elsewhere in the repo — without file-locality
    /// priority, `repo_scores` pulling in `other.py`'s unrelated `helper`
    /// would make the exact-name match ambiguous (two candidates named
    /// `helper`) and silently drop an edge that was unambiguous at file
    /// scope.
    #[test]
    fn repo_scores_prefers_same_file_callee_over_a_repo_wide_namesake() {
        let caller = FunctionIr {
            id: fid_in("caller.py", "caller"),
            nodes: vec![call(1, "helper")],
            entry: 0,
        };
        let local_helper = FunctionIr {
            id: fid_in("caller.py", "helper"),
            nodes: vec![Node::pure(2).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let unrelated_helper = FunctionIr {
            id: fid_in("other.py", "helper"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let irs = [caller, local_helper, unrelated_helper];
        let denylist = Denylist::new();
        let weights = ScoreWeights::default();
        let sals: Vec<_> = irs
            .iter()
            .map(|ir| analyze(ir, &denylist, &weights))
            .collect();
        let lines: Vec<_> = irs.iter().map(line_scores_from_lines).collect();
        let functions: Vec<ScopedFunction<'_>> = (0..irs.len())
            .map(|i| ScopedFunction {
                ir: &irs[i],
                importance: &sals[i],
                line_scores: &lines[i],
            })
            .collect();

        let features = function_features(&functions);
        assert!(
            features[1].fan_in > features[2].fan_in,
            "the same-file helper.py:helper must get the call's fan-in, not other.py's namesake: {features:?}"
        );
    }
}
