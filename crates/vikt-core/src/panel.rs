//! The instrument panel: five scorers and two structural facts, combined by
//! weights fitted against blind expert judgement.
//!
//! # Why a panel
//!
//! Two rounds of cross-discipline search (nineteen algorithms from as many
//! fields — see `eval/RESULTS-leading-algorithms.md`) ended in the same
//! finding from both directions: no single structural measure of a dependence
//! graph orders a function's lines the way a careful reader does. The best
//! solo instrument reaches Spearman ρ ≈ 0.36 against blind per-line labels,
//! which is no better than the null heuristic "later lines matter more".
//! Every instrument also has functions it ranks *backwards*.
//!
//! But the instruments fail in different places — deletion-sensitivity is
//! blind where confluence-order sees, and vice versa — so a linear
//! combination held out at ρ ≈ 0.52 on functions the fit never saw, beating
//! every single signal on 12 of 16. Importance is not one quantity; it has
//! facets, and each panel member carries one:
//!
//! | member    | field                    | facet                                   |
//! |-----------|--------------------------|-----------------------------------------|
//! | `current` | this crate's incumbent   | dependence cones, control, loop carriage |
//! | `schur`   | linear algebra           | what is lost if the statement is deleted |
//! | `pivot`   | reliability engineering  | is it a single point of failure          |
//! | `trophic` | food-web ecology         | how many derivation layers lie beneath   |
//! | `strahler`| geomorphology            | do independent derivations converge here |
//! | position  | (structural fact)        | judgement's demonstrated positional lean |
//! | boundary  | (structural fact)        | the tier layer's I/O call                |
//!
//! # The combination
//!
//! Everything is done at LINE level, mirroring how the fit was measured:
//!
//! 1. each instrument's per-node scores are projected to lines by `max` over
//!    the line's non-structural nodes — the same rule the artifact layer uses;
//! 2. each instrument's per-line values are rank-normalised to `[0, 1]`
//!    within the function (a scorer's *scale* is arbitrary across functions;
//!    its *ordering* is the signal — this is also what makes the combination
//!    immune to any monotone rescaling of a member);
//! 3. position is the line's rank in source order; boundary is `1.0` when the
//!    line's strongest tier is [`Tier::Boundary`];
//! 4. the line's panel score is the dot product with the selected
//!    [`PanelProfile`]'s weight vector, and every node on the line inherits
//!    it.
//!
//! # Provenance of the weights
//!
//! Two vectors, one per dependence-graph granularity — see
//! [`INSTRUCTION_WEIGHTS`], [`STATEMENT_WEIGHTS`] and [`PanelProfile`] for
//! the fit details, the held-out numbers, and why a single vector does not
//! serve both substrates.
//!
//! The weights are baked as constants rather than loaded from a file: the
//! panel is a *shipped strategy*, not a tunable: retuning belongs in eval/,
//! with fresh blind labels, not in production flags.

use std::collections::BTreeMap;

use crate::graph::Graph;
use crate::importance::{Denylist, FunctionImportance, Tier};
use crate::ir::FunctionIr;

/// Fitted weight of each panel member, in the order
/// `[current, schur, pivot, trophic, strahler, position, boundary]`.
///
/// Ridge regression (λ = 0.03, chosen by inner cross-validation), fitted on
/// `eval/ground-truth-v3.json`: 16 stdlib functions, 425 lines, labelled
/// 0–10 by the expert rater from source, blind, before any scorer ran.
/// Leave-one-function-out evaluation of this exact feature set: mean held-out
/// Spearman **0.517** (position-only null: 0.359; best single instrument:
/// 0.357). The fitting script is `eval/judgement.py`; the numbers are
/// reproducible from the repo.
///
/// The graph these weights were fitted against is bytecode-granular: one
/// node per JVM/CPython instruction, several to a source line. Use this
/// vector for bytecode frontends. See [`STATEMENT_WEIGHTS`] for the
/// statement-granular AST refit, and [`PanelProfile`] for how the two are
/// selected.
///
/// Do not hand-edit: these are the ridge coefficients from the oracle fit,
/// and the held-out number quoted with them is only true of exactly
/// these values.
pub const INSTRUCTION_WEIGHTS: [f64; 7] = [0.1643, 0.1654, 0.1888, 0.1092, 0.1097, 0.2505, -0.0521];

/// Fitted weight of each panel member for statement-granular AST graphs, in
/// the same order as [`INSTRUCTION_WEIGHTS`]:
/// `[current, schur, pivot, trophic, strahler, position, boundary]`.
///
/// Ridge on `eval/ground-truth-js-v1.json`: 9 functions, 100 lines, drawn
/// from lodash/express/zod, labelled blind and committed before measurement.
/// λ = 0.3, chosen by inner leave-one-out. Leave-one-function-out held-out
/// Spearman **0.676**, against 0.603 for the instruction weights applied
/// zero-shot to this substrate, and 0.695 for the position null.
///
/// The instruction weights transfer (0.603 beats their own 0.517 on Python),
/// but not evenly: on statement-granular graphs strahler's per-node
/// resolution roughly doubles its discriminating power, while schur and
/// pivot - deletion-sensitivity and single-point-of-failure, both leaning on
/// bytecode's denser dependence structure - collapse toward noise. This
/// vector is the corrective: a per-substrate refit rather than one weight
/// vector asked to serve graphs of two different granularities.
///
/// Do not hand-edit; see [`INSTRUCTION_WEIGHTS`] for the sibling vector's
/// provenance and the same caveat.
pub const STATEMENT_WEIGHTS: [f64; 7] = [0.1386, 0.1109, 0.0917, 0.1559, 0.1587, 0.1800, 0.0522];

/// Which fitted weight vector the panel should use.
///
/// The panel's weights were ridge-fitted against a specific dependence-graph
/// granularity, and that granularity is a property of the *frontend*, not
/// the language: bytecode frontends (JVM, CPython) emit several nodes per
/// source line, so schur's deletion-sensitivity and pivot's single-point-of-
/// failure signal have room to discriminate between them; the oxc-based JS/TS
/// frontend lowers to one node per statement, which starves those two
/// instruments and roughly doubles strahler's discriminating power (see the
/// transfer measurement in `eval/RESULTS-real-code.md`). The granularity of
/// the dependence graph changes which instruments carry signal, so the
/// weight vector has to change with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelProfile {
    /// Bytecode frontends (JVM, `CPython`): [`INSTRUCTION_WEIGHTS`].
    #[default]
    Instruction,
    /// AST frontends (JS/TS): [`STATEMENT_WEIGHTS`].
    Statement,
}

impl PanelProfile {
    /// The fitted weight vector for this profile.
    #[must_use]
    pub fn weights(self) -> [f64; 7] {
        match self {
            PanelProfile::Instruction => INSTRUCTION_WEIGHTS,
            PanelProfile::Statement => STATEMENT_WEIGHTS,
        }
    }
}

/// Within-function rank of each value in `[0, 1]`, ties sharing their mean
/// rank — the same normalisation the fit used (`eval/judgement.py::rank01`).
///
/// The float equality in the tie loop is intended: identical scores must
/// share a rank, and the values come from deterministic arithmetic, not
/// accumulated error.
#[allow(clippy::float_cmp)]
pub(crate) fn rank01(vals: &[f64]) -> Vec<f64> {
    let n = vals.len();
    if n <= 1 {
        return vec![0.5; n];
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        vals[a]
            .partial_cmp(&vals[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && vals[order[j + 1]] == vals[order[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0;
        for k in i..=j {
            out[order[k]] = avg;
        }
        i = j + 1;
    }
    let d = (n - 1) as f64;
    out.iter().map(|x| x / d).collect()
}

/// Per-line `max` projection of a per-node score vector, over the given
/// line → nodes map.
fn to_lines(per_node: &[f64], lines: &BTreeMap<u32, Vec<usize>>) -> Vec<f64> {
    lines
        .values()
        .map(|nodes| {
            nodes
                .iter()
                .map(|&i| per_node[i])
                .fold(f64::NEG_INFINITY, f64::max)
        })
        .collect()
}

/// The per-line feature vectors the panel combines, in weight order:
/// `[current, schur, pivot, trophic, strahler, position, boundary]` — the
/// five instruments rank-normalised within the function, the two structural
/// features on their natural `[0, 1]` scales.
///
/// Measurement/audit surface: exists so calibration can pair each line's raw
/// instrument readings with observed behaviour (mutant kill rates) and refit
/// the weights offline. [`score`] combines exactly these values, so the two
/// can never disagree.
#[must_use]
pub fn line_features(
    ir: &FunctionIr,
    sal: &FunctionImportance,
    denylist: &Denylist,
) -> BTreeMap<u32, [f64; 7]> {
    let graph: &Graph = &sal.graph;

    // Line → participating nodes, excluding structural nodes, exactly as the
    // artifact layer's projection does.
    let mut lines: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, node) in ir.nodes.iter().enumerate() {
        if node.is_structural() {
            continue;
        }
        if let Some(l) = node.line {
            lines.entry(l).or_default().push(i);
        }
    }
    if lines.is_empty() {
        return BTreeMap::new();
    }

    // The five instruments, per node.
    let current: Vec<f64> = sal.nodes.iter().map(|ns| ns.score).collect();
    let schur = crate::schur::score(ir, graph, denylist);
    let pivot = crate::pivot::rescale_for_display(&crate::pivot::birnbaum_scores(ir, graph));
    let trophic = crate::trophic::score(graph);
    let strahler = crate::strahler::score(graph);

    // Projected to lines and rank-normalised.
    let feats: Vec<Vec<f64>> = [&current, &schur, &pivot, &trophic, &strahler]
        .iter()
        .map(|v| rank01(&to_lines(v, &lines)))
        .collect();

    let m = lines.len();
    let position: Vec<f64> = if m == 1 {
        vec![0.5]
    } else {
        (0..m).map(|i| i as f64 / (m - 1) as f64).collect()
    };
    let boundary: Vec<f64> = lines
        .values()
        .map(|nodes| {
            let strongest = nodes
                .iter()
                .map(|&i| sal.nodes[i].tier)
                .max()
                .unwrap_or(Tier::Inert);
            if strongest == Tier::Boundary {
                1.0
            } else {
                0.0
            }
        })
        .collect();

    lines
        .keys()
        .enumerate()
        .map(|(i, &line)| {
            (
                line,
                [
                    feats[0][i],
                    feats[1][i],
                    feats[2][i],
                    feats[3][i],
                    feats[4][i],
                    position[i],
                    boundary[i],
                ],
            )
        })
        .collect()
}

/// Computes the panel score for every node of `ir`, using `profile`'s
/// fitted weight vector.
///
/// `sal` must be the result of [`crate::importance::analyze`] on the same `ir`:
/// its per-node scores are the `current` member, its tiers drive the boundary
/// feature and the inert floor. Structural nodes and nodes without a line
/// score `0.0` — they are invisible to the artifact projection anyway.
#[must_use]
pub fn score(
    ir: &FunctionIr,
    sal: &FunctionImportance,
    denylist: &Denylist,
    profile: PanelProfile,
) -> Vec<f64> {
    let weights = profile.weights();
    let n = ir.nodes.len();
    let features = line_features(ir, sal, denylist);
    if features.is_empty() {
        return vec![0.0; n];
    }

    let per_line: BTreeMap<u32, f64> = features
        .iter()
        .map(|(&line, f)| {
            let s: f64 = f.iter().zip(&weights).map(|(x, w)| x * w).sum();
            (line, s.clamp(0.0, 1.0))
        })
        .collect();

    (0..n)
        .map(|i| {
            let node = &ir.nodes[i];
            if node.is_structural() {
                return 0.0;
            }
            node.line
                .and_then(|l| per_line.get(&l))
                .copied()
                .unwrap_or(0.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importance::{ScoreWeights, analyze};
    use crate::ir::{FunctionId, Node, NodeKind};

    fn fixture() -> FunctionIr {
        // line 1: x = param        (defs 0)
        // line 2: y = x + 1        (defs 1, uses 0)
        // line 3: z = y * y        (defs 2, uses 1)
        // line 4: sink[k] = z      (state write, uses 2)
        let nodes = vec![
            Node {
                line: Some(1),
                kind: NodeKind::Pure,
                defs: vec![0],
                uses: vec![],
                succs: vec![1],
                label: "load".into(),
            },
            Node {
                line: Some(2),
                kind: NodeKind::Pure,
                defs: vec![1],
                uses: vec![0],
                succs: vec![2],
                label: "add".into(),
            },
            Node {
                line: Some(3),
                kind: NodeKind::Pure,
                defs: vec![2],
                uses: vec![1],
                succs: vec![3],
                label: "mul".into(),
            },
            Node {
                line: Some(4),
                kind: NodeKind::StateWrite {
                    target: "sink".into(),
                },
                defs: vec![],
                uses: vec![2],
                succs: vec![],
                label: "store".into(),
            },
        ];
        FunctionIr {
            id: FunctionId {
                file: "panel-test".into(),
                name: "chain".into(),
                signature: String::new(),
                decl_line: Some(1),
            },
            nodes,
            entry: 0,
        }
    }

    #[test]
    fn every_node_on_a_line_shares_the_line_score() {
        let ir = fixture();
        let sal = analyze(&ir, &Denylist::new(), &ScoreWeights::default());
        let s = score(&ir, &sal, &Denylist::new(), PanelProfile::Instruction);
        assert_eq!(s.len(), ir.nodes.len());
        // One node per line here, so simply: all finite, all in [0,1].
        for v in &s {
            assert!(v.is_finite() && (0.0..=1.0).contains(v), "{v}");
        }
    }

    #[test]
    fn later_derivation_outranks_setup_on_a_pure_chain() {
        // On a straight chain every instrument agrees the tail is where the
        // work lands, and position reinforces it; the panel must too, under
        // either profile.
        let ir = fixture();
        let sal = analyze(&ir, &Denylist::new(), &ScoreWeights::default());
        for profile in [PanelProfile::Instruction, PanelProfile::Statement] {
            let s = score(&ir, &sal, &Denylist::new(), profile);
            assert!(
                s[3] > s[0],
                "{profile:?}: state write {} should outrank initial load {}",
                s[3],
                s[0]
            );
        }
    }

    #[test]
    fn deterministic() {
        // Repeat calls must agree, and must agree under both profiles - the
        // profile only picks which constant vector feeds the same
        // deterministic arithmetic.
        let ir = fixture();
        let sal = analyze(&ir, &Denylist::new(), &ScoreWeights::default());
        for profile in [PanelProfile::Instruction, PanelProfile::Statement] {
            let a = score(&ir, &sal, &Denylist::new(), profile);
            let b = score(&ir, &sal, &Denylist::new(), profile);
            assert_eq!(a, b, "{profile:?} was not deterministic");
        }
    }

    #[test]
    fn profile_selects_the_weight_vector() {
        // The two vectors were fitted independently on different substrates;
        // on the same fixture they must actually produce different scores,
        // or `profile` would be decorative. Any node whose feature vector
        // isn't degenerate will do - `state write` (line 4) is at the
        // convergence of all five instruments, so it moves the most.
        let ir = fixture();
        let sal = analyze(&ir, &Denylist::new(), &ScoreWeights::default());
        let instruction = score(&ir, &sal, &Denylist::new(), PanelProfile::Instruction);
        let statement = score(&ir, &sal, &Denylist::new(), PanelProfile::Statement);
        assert_ne!(
            instruction, statement,
            "Instruction and Statement profiles scored this fixture identically"
        );
    }

    #[test]
    fn weights_match_documented_fit() {
        // The doc comments quote held-out 0.517 (instruction) and 0.676
        // (statement) for exactly these values; a drive-by edit to one
        // constant would silently falsify the docs.
        let instruction_sum: f64 = INSTRUCTION_WEIGHTS.iter().sum();
        assert!(
            (instruction_sum - 0.9358).abs() < 1e-9,
            "instruction weights changed: sum={instruction_sum}"
        );
        let statement_sum: f64 = STATEMENT_WEIGHTS.iter().sum();
        assert!(
            (statement_sum - 0.888).abs() < 1e-9,
            "statement weights changed: sum={statement_sum}"
        );
    }
}
