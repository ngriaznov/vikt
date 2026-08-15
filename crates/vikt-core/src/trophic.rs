//! Derivation-depth scorer: **how many layers of transformation lie beneath
//! this statement?** —
//! [trophic levels](https://en.wikipedia.org/wiki/Trophic_level) (Levine,
//! *Several Measures of Trophic Structure Applicable to Complex Food Webs*,
//! 1980) over the dependence graph read as a food web: a statement
//! consuming nothing is basal at `1.0`; otherwise
//! `level(v) = 1 + weighted mean of the levels it reads` (data edges `1.0`,
//! control edges `0.6` — the same graph [`crate::schur`] builds). Backward
//! only: depth of derivation, never downstream reach.
//!
//! Self-edges are dropped; each nontrivial SCC is solved exactly as one
//! linear system (Gaussian elimination, fixed pivoting, uncapped by spec —
//! `O(E + Σ_C |C|³)`). A cycle receiving no outside weight has no defined
//! level — ecologically, a closed loop with no producer — and is assigned
//! basal `1.0` (`tests::isolated_cycle_with_no_input_is_basal`). Levels
//! min-max normalise to `0.0..=1.0` per function (all equal ⇒ `0.5`
//! everywhere). Deterministic throughout.
//!
//! Known weaknesses: fan-in *dilutes* — one deeply derived operand pulls an
//! expression up by roughly `1/n` of its depth, the opposite direction from
//! [`crate::schur`]'s multiplicative blow-up; a level says nothing about
//! whether anything depends on the value now (that is `schur`/influence
//! territory); the `0.6` control weight is a convention shared with
//! `schur`, not derived from the analogy.

use std::collections::BTreeMap;

use crate::graph::{Graph, strongly_connected};
use crate::ir::NodeId;

/// Weight contributed by a data (def-use) dependence edge.
const DATA_WEIGHT: f64 = 1.0;
/// Weight contributed by a control-dependence edge. Matches
/// [`crate::schur::CTRL_WEIGHT`] so the edge construction — and therefore
/// what differs between the two scorers — is limited to what each does with
/// the weights, not the weights themselves.
const CTRL_WEIGHT: f64 = 0.6;

/// Pivot magnitude below which a nontrivial component's Gaussian elimination
/// is treated as singular. Guards a division that should, by construction
/// (see the module docs' "no outside input" case), only ever be attempted
/// when the component *does* receive outside weight — this is a defensive
/// floor against floating-point near-cancellation, not an expected path.
const PIVOT_EPS: f64 = 1e-12;

/// Per-node trophic-level score, min-max normalised into `0.0..=1.0` across
/// the function. `n == 0` returns an empty vector; a function with every
/// level equal (including the single-node case) returns `0.5` for every
/// node.
///
/// Tier assignment is untouched by this function — it is called only to
/// overwrite [`crate::importance::NodeImportance::score`] after tiering has
/// already run. See [`crate::importance::analyze_with_scorer`].
#[must_use]
pub fn score(graph: &Graph) -> Vec<f64> {
    let n = graph.n;
    if n == 0 {
        return Vec::new();
    }
    let levels = trophic_levels(graph);
    normalize(&levels)
}

/// Raw (unnormalised) trophic level per node.
fn trophic_levels(graph: &Graph) -> Vec<f64> {
    let n = graph.n;
    let in_edges = weighted_in_edges(graph);

    // Consumer-direction adjacency for SCC discovery: an edge u -> v here
    // means v depends on u, the same orientation cone_sizes and schur build
    // their succ graphs in, so strongly_connected's "reverse topological"
    // contract (sinks first) applies identically.
    let mut succ: Vec<Vec<NodeId>> = vec![Vec::new(); n];
    for (v, edges) in in_edges.iter().enumerate() {
        for &(u, _) in edges {
            succ[u].push(v);
        }
    }
    let (comp_of, comps) = strongly_connected(n, &succ);

    let mut level = vec![0.0f64; n];
    // Reversed: strongly_connected emits sinks (nothing depends on them
    // further downstream — irrelevant here) first and sources last, but a
    // node's level needs its *predecessors* solved first, so walk from the
    // last component to the first. The same reversal crate::schur::compute_l
    // applies to the identically-oriented crate::schur::compute_r ordering.
    for c in (0..comps.len()).rev() {
        let members = &comps[c];
        if members.len() == 1 {
            let v = members[0];
            level[v] = solve_single(v, &in_edges, &level);
        } else {
            solve_component(members, &comp_of, &in_edges, &mut level);
        }
    }
    level
}

/// Weighted in-edge list per node: `(source, weight)` pairs from
/// [`Graph::uses_defs`] (weight [`DATA_WEIGHT`]) and [`Graph::ctrl_deps`]
/// (weight [`CTRL_WEIGHT`]), aggregated by source and with self-edges
/// dropped.
fn weighted_in_edges(graph: &Graph) -> Vec<Vec<(NodeId, f64)>> {
    let n = graph.n;
    let mut acc: Vec<BTreeMap<NodeId, f64>> = vec![BTreeMap::new(); n];
    for (node, row) in acc.iter_mut().enumerate() {
        for &d in &graph.uses_defs[node] {
            let src = graph.defs[d].node;
            if src != node {
                *row.entry(src).or_insert(0.0) += DATA_WEIGHT;
            }
        }
        for &c in &graph.ctrl_deps[node] {
            if c != node {
                *row.entry(c).or_insert(0.0) += CTRL_WEIGHT;
            }
        }
    }
    acc.into_iter()
        .map(BTreeMap::into_iter)
        .map(Iterator::collect)
        .collect()
}

/// Trivial (single-node, no self-edge — self-edges never appear, see
/// [`weighted_in_edges`]) component: every in-edge originates outside the
/// component and is therefore already solved by the time this runs.
fn solve_single(v: NodeId, in_edges: &[Vec<(NodeId, f64)>], level: &[f64]) -> f64 {
    let edges = &in_edges[v];
    let total: f64 = edges.iter().map(|&(_, w)| w).sum();
    if total <= 0.0 {
        return 1.0; // basal
    }
    let weighted: f64 = edges.iter().map(|&(u, w)| w * level[u]).sum();
    1.0 + weighted / total
}

/// Nontrivial strongly connected component: solves the linear system
/// described in the module docs exactly, or — if no member receives any
/// weight from outside the component — assigns every member `1.0`.
fn solve_component(
    members: &[NodeId],
    comp_of: &[usize],
    in_edges: &[Vec<(NodeId, f64)>],
    level: &mut [f64],
) {
    let k = members.len();
    let component = comp_of[members[0]];
    let mut local: BTreeMap<NodeId, usize> = BTreeMap::new();
    for (i, &v) in members.iter().enumerate() {
        local.insert(v, i);
    }

    // S(v) on the diagonal, -w(u,v) off-diagonal for internal edges, and the
    // known contribution from outside the component collected separately.
    let mut mat = vec![0.0f64; k * k];
    let mut total_s = vec![0.0f64; k];
    let mut outside_weight = 0.0f64;
    let mut outside_contribution = vec![0.0f64; k];
    for (i, &v) in members.iter().enumerate() {
        debug_assert_eq!(comp_of[v], component, "members must share one component");
        for &(u, w) in &in_edges[v] {
            total_s[i] += w;
            if let Some(&j) = local.get(&u) {
                mat[i * k + j] -= w;
            } else {
                outside_weight += w;
                outside_contribution[i] += w * level[u];
            }
        }
        mat[i * k + i] += total_s[i];
    }

    if outside_weight <= 0.0 {
        // No primary producer feeds this component at all: the system is
        // singular by construction (see the module docs). Assign the same
        // floor a basal node gets.
        for &v in members {
            level[v] = 1.0;
        }
        return;
    }

    let rhs: Vec<f64> = (0..k)
        .map(|i| total_s[i] + outside_contribution[i])
        .collect();
    match gaussian_solve(&mat, &rhs, k) {
        Some(solved) => {
            for (i, &v) in members.iter().enumerate() {
                level[v] = solved[i];
            }
        }
        None => {
            // Should not happen: outside_weight > 0 makes the component
            // irreducibly diagonally dominant, hence nonsingular. Kept as a
            // defensive floor rather than a panic, matching the "no outside
            // input" convention above.
            for &v in members {
                level[v] = 1.0;
            }
        }
    }
}

/// Solves the dense `k x k` system `mat * x = rhs` by Gaussian elimination
/// with partial pivoting (largest magnitude in the remaining column, ties
/// broken by lowest row index — deterministic). Returns `None` the moment a
/// pivot's magnitude falls below [`PIVOT_EPS`], rather than divide by a
/// near-zero value.
fn gaussian_solve(mat: &[f64], rhs: &[f64], k: usize) -> Option<Vec<f64>> {
    let mut a = mat.to_vec();
    let mut y = rhs.to_vec();
    for col in 0..k {
        let mut pivot_row = col;
        let mut pivot_val = a[col * k + col].abs();
        for row in (col + 1)..k {
            let candidate = a[row * k + col].abs();
            if candidate > pivot_val {
                pivot_val = candidate;
                pivot_row = row;
            }
        }
        if pivot_val < PIVOT_EPS {
            return None;
        }
        if pivot_row != col {
            for c in 0..k {
                a.swap(col * k + c, pivot_row * k + c);
            }
            y.swap(col, pivot_row);
        }
        let pivot = a[col * k + col];
        for row in (col + 1)..k {
            let factor = a[row * k + col] / pivot;
            if factor != 0.0 {
                for c in col..k {
                    a[row * k + c] -= factor * a[col * k + c];
                }
                y[row] -= factor * y[col];
            }
        }
    }
    let mut x = vec![0.0f64; k];
    for i in (0..k).rev() {
        let mut sum = y[i];
        for (j, &xj) in x.iter().enumerate().skip(i + 1) {
            sum -= a[i * k + j] * xj;
        }
        x[i] = sum / a[i * k + i];
    }
    Some(x)
}

/// Min-max normalisation into `0.0..=1.0`. All-equal levels (including the
/// single-node case) normalise to `0.5` for every node, rather than an
/// arbitrary constant or a division by zero.
fn normalize(levels: &[f64]) -> Vec<f64> {
    let min = levels.iter().copied().fold(f64::INFINITY, f64::min);
    let max = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if (max - min).abs() < f64::EPSILON {
        return vec![0.5; levels.len()];
    }
    levels
        .iter()
        .map(|&l| ((l - min) / (max - min)).clamp(0.0, 1.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CallOpacity, FunctionId, FunctionIr, Node, NodeKind};

    fn fid(name: &str) -> FunctionId {
        FunctionId {
            file: "Test.py".into(),
            name: name.into(),
            signature: String::new(),
            decl_line: Some(1),
        }
    }

    /// `a = input(); b = a; c = a; d = b + c` — fan-out then fan-in, worked
    /// out by hand from the module's own definition:
    ///
    /// - `level(a) = 1.0` (basal: `a` reads nothing already in the function)
    /// - `level(b) = level(c) = 1.0 + level(a)/1.0 = 2.0`
    /// - `level(d) = 1.0 + (level(b) + level(c))/2.0 = 1.0 + 2.0 = 3.0`
    ///
    /// Min-max normalised over `{1.0, 2.0, 2.0, 3.0}`: `a` -> `0.0`, `b`/`c`
    /// -> `0.5`, `d` -> `1.0`.
    #[test]
    fn hand_computable_diamond() {
        const A: u32 = 1;
        const B: u32 = 2;
        const C: u32 = 3;
        const D: u32 = 4;
        let ir = FunctionIr {
            id: fid("diamond"),
            nodes: vec![
                Node::pure(1).with_dataflow([A], []).with_succs([1]), // a = input()
                Node::pure(2).with_dataflow([B], [A]).with_succs([2]), // b = a
                Node::pure(3).with_dataflow([C], [A]).with_succs([3]), // c = a
                Node::pure(4).with_dataflow([D], [B, C]),             // d = b + c
            ],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        let scores = score(&graph);
        assert_eq!(scores.len(), 4);
        assert!((scores[0] - 0.0).abs() < 1e-9, "got {scores:?}");
        assert!((scores[1] - 0.5).abs() < 1e-9, "got {scores:?}");
        assert!((scores[2] - 0.5).abs() < 1e-9, "got {scores:?}");
        assert!((scores[3] - 1.0).abs() < 1e-9, "got {scores:?}");
    }

    /// `x = 0; while True: y = f(x, z); z = g(y)` — `y` and `z` form a
    /// genuine two-node cycle (each reads the other's previous definition),
    /// fed by the single external input `x` into `y`.
    ///
    /// Solving the module's linear system by hand:
    ///
    /// ```text
    /// level(y) = 1 + (level(x) + level(z)) / 2      (S(y) = 2: one edge from x, one from z)
    /// level(z) = 1 + level(y)                        (S(z) = 1: one edge from y)
    /// ```
    ///
    /// with `level(x) = 1.0` (basal), substituting gives `level(y) = 2 +
    /// level(y)/2`, so `level(y) = 4.0` and `level(z) = 5.0`.
    #[test]
    fn hand_computable_cycle() {
        const X: u32 = 1;
        const Y: u32 = 2;
        const Z: u32 = 3;
        let ir = FunctionIr {
            id: fid("cycle"),
            nodes: vec![
                Node::pure(1).with_dataflow([X], []).with_succs([1]), // x = 0
                Node::pure(2).with_dataflow([Y], [X, Z]).with_succs([2]), // y = f(x, z)
                Node::pure(3).with_dataflow([Z], [Y]).with_succs([1]), // z = g(y); loop
            ],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        assert!(
            graph
                .loops
                .iter()
                .any(|l| l.body.contains(&1) && l.body.contains(&2)),
            "y and z must form a natural loop"
        );
        let levels = trophic_levels(&graph);
        assert!((levels[0] - 1.0).abs() < 1e-9, "x: got {levels:?}");
        assert!((levels[1] - 4.0).abs() < 1e-9, "y: got {levels:?}");
        assert!((levels[2] - 5.0).abs() < 1e-9, "z: got {levels:?}");

        let scores = score(&graph);
        assert!((scores[0] - 0.0).abs() < 1e-9, "got {scores:?}");
        assert!((scores[1] - 0.75).abs() < 1e-9, "got {scores:?}");
        assert!((scores[2] - 1.0).abs() < 1e-9, "got {scores:?}");
    }

    /// `a <-> b`, a bare two-cycle with no other statement feeding it at
    /// all. Neither member is basal, and neither has any outside input, so
    /// the linear system is singular by construction (see the module docs)
    /// and both fall back to the basal floor of `1.0` — which, since it is
    /// the whole function, normalises to `0.5` for both.
    #[test]
    fn isolated_cycle_with_no_input_is_basal() {
        const A: u32 = 1;
        const B: u32 = 2;
        let ir = FunctionIr {
            id: fid("isolated_cycle"),
            nodes: vec![
                Node::pure(1).with_dataflow([A], [B]).with_succs([1]), // a = f(b)
                Node::pure(2).with_dataflow([B], [A]).with_succs([0]), // b = g(a); loop
            ],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        assert_eq!(graph.n, 2);
        let levels = trophic_levels(&graph);
        assert!((levels[0] - 1.0).abs() < 1e-9, "got {levels:?}");
        assert!((levels[1] - 1.0).abs() < 1e-9, "got {levels:?}");
        let scores = score(&graph);
        assert!((scores[0] - 0.5).abs() < 1e-9, "got {scores:?}");
        assert!((scores[1] - 0.5).abs() < 1e-9, "got {scores:?}");
    }

    /// A function with a diamond, a branch and a loop, structurally similar
    /// to `crate::schur`'s own bake-off fixture. Basal nodes must score the
    /// function's minimum (or tie for it), and every level and score must be
    /// finite and in range regardless of the cycle in the middle.
    #[test]
    fn basal_nodes_score_minimum_and_levels_stay_finite() {
        let mut nodes = vec![
            Node::pure(0).with_dataflow([1], []).with_succs([1]), // 0: a = input() -- basal
            Node::pure(1).with_dataflow([2], [1]).with_succs([2]), // 1: b = a
            Node::pure(2).with_dataflow([3], [1]).with_succs([3]), // 2: c = a
            Node::pure(3).with_dataflow([4], [2, 3]).with_succs([4]), // 3: d = b + c
            Node::pure(4)
                .with_dataflow([], [4])
                .with_kind(NodeKind::Branch)
                .with_succs([5, 8]), // 4: branch on d
            Node::pure(5).with_dataflow([5], [4]).with_succs([6]), // 5: e = d
            Node::pure(6)
                .with_dataflow([5], [5])
                .with_kind(NodeKind::Branch)
                .with_succs([6, 7]), // 6: e = e + 1 (loop)
            Node::pure(7)
                .with_dataflow([], [5])
                .with_kind(NodeKind::Return), // 7: return e
            Node {
                line: Some(9),
                kind: NodeKind::Call {
                    callee: "logging.info".into(),
                    opacity: CallOpacity::Opaque,
                },
                defs: Vec::new(),
                uses: vec![4],
                succs: Vec::new(),
                label: String::new(),
            }, // 8: log(d)
        ];
        for (i, n) in nodes.iter_mut().enumerate() {
            n.line = Some(u32::try_from(i).unwrap_or(u32::MAX) + 1);
        }
        let ir = FunctionIr {
            id: fid("diamond_branch_loop"),
            nodes,
            entry: 0,
        };
        ir.validate().expect("well-formed fixture");
        let graph = Graph::build(&ir);
        let scores = score(&graph);

        assert_eq!(scores.len(), graph.n);
        for &s in &scores {
            assert!(s.is_finite(), "non-finite score in {scores:?}");
            assert!((0.0..=1.0).contains(&s), "out of range in {scores:?}");
        }
        // Node 0 is the only basal node (nothing else has an empty in-edge
        // list): every other node's score must be >= its score.
        let basal_score = scores[0];
        for (i, &s) in scores.iter().enumerate() {
            assert!(
                s >= basal_score - 1e-9,
                "node {i} scored {s} below basal node 0's {basal_score}"
            );
        }
    }

    /// The whole point of a deterministic scorer: running it twice over the
    /// same graph must produce bit-identical output, not just numerically
    /// close output.
    #[test]
    fn deterministic_across_runs() {
        let mut nodes = vec![
            Node::pure(0).with_dataflow([1], []).with_succs([1]),
            Node::pure(1).with_dataflow([2], [1]).with_succs([2]),
            Node::pure(2).with_dataflow([3], [1]).with_succs([3]),
            Node::pure(3).with_dataflow([4], [2, 3]).with_succs([4]),
            Node::pure(4)
                .with_dataflow([], [4])
                .with_kind(NodeKind::Branch)
                .with_succs([5, 6]),
            Node::pure(5).with_dataflow([5], [4]).with_succs([6]),
            Node::pure(6)
                .with_dataflow([], [5])
                .with_kind(NodeKind::Return),
        ];
        for (i, n) in nodes.iter_mut().enumerate() {
            n.line = Some(u32::try_from(i).unwrap_or(u32::MAX) + 1);
        }
        let ir = FunctionIr {
            id: fid("determinism"),
            nodes,
            entry: 0,
        };
        let graph = Graph::build(&ir);
        let first = score(&graph);
        let second = score(&graph);
        assert_eq!(first, second, "identical graph must score identically");
    }

    /// The empty function must not panic and must report no scores.
    #[test]
    fn empty_function_scores_nothing() {
        let ir = FunctionIr {
            id: fid("empty"),
            nodes: Vec::new(),
            entry: 0,
        };
        let graph = Graph::build(&ir);
        assert_eq!(score(&graph), Vec::<f64>::new());
    }

    /// A single instruction with nothing to depend on: basal, and alone, so
    /// it normalises to the all-equal `0.5`.
    #[test]
    fn single_node_is_basal_and_normalizes_to_half() {
        let ir = FunctionIr {
            id: fid("single"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        assert_eq!(score(&graph), vec![0.5]);
    }
}
