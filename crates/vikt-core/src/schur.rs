//! Deletion-sensitivity scorer: **how much less influence would the function
//! deliver to its outputs if this statement were deleted?** Answered in
//! closed form via the
//! [Schur complement](https://en.wikipedia.org/wiki/Schur_complement)
//! identity for principal-submatrix inverses over an
//! [absorbing-chain](https://en.wikipedia.org/wiki/Absorbing_Markov_chain)
//! fundamental matrix — not approximated by a proxy statistic.
//!
//! Model: nodes are instructions; edge `v -> u` when `u` reads `v`'s value
//! (weight `1.0`) or `v` is a branch controlling `u` (`0.6`). Outputs `Ω`
//! are the non-denylisted `NodeKind::is_effect` nodes and absorb; rows
//! normalise by *total* out-weight so dead-ending influence is genuinely
//! lost. With `M = I - Q` over the transient nodes,
//! `score(v) = L_v·R_v/D_v` — upstream visit count times probability of
//! ever reaching an output, corrected by the closed-form cycle-return term —
//! which equals exactly the drop in total delivered influence when `v`'s
//! row and column are deleted (`tests::brute_force_matches_deletion` checks this against literal re-solves). An `Ω` node scores what it
//! received. Undamped; every nontrivial SCC is solved exactly as a dense
//! block up to `EXACT_SCC_CAP`. Cost `O(E + Σ_C |C|³)`. Deterministic:
//! `BTreeMap` order, fixed partial pivoting, fixed-sweep Jacobi fallback.
//!
//! Known weaknesses: `L` is a route-multiplicity count and inflates in
//! dense fan-in regions regardless of semantics; on a loop-free body
//! `D ≡ 1` and the score collapses to upstream-reach × downstream-reach;
//! an `Ω` node with no dependence predecessors (a bare-literal `return`)
//! scores `0.0` despite being the function's visible action.

// Single-letter names throughout (`q`, `b`, `r`, `l`, `d`, `f`, `m`, `n`) are
// deliberate: they are the linear-algebra notation the module doc above
// defines and uses consistently, and spelling them out would make the code
// harder to check against the math, not easier.
#![allow(clippy::many_single_char_names)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::graph::{Graph, strongly_connected};
use crate::importance::Denylist;
use crate::ir::{FunctionIr, NodeId, NodeKind};

/// Weight contributed by a data (def-use) dependence edge.
const DATA_WEIGHT: f64 = 1.0;
/// Weight contributed by a control-dependence edge. Chosen to match the
/// `dirichlet` channel's edge construction exactly, so the two scorers differ
/// only in what they do with the same weighted graph.
const CTRL_WEIGHT: f64 = 0.6;

/// Largest strongly connected block solved by exact dense LU.
///
/// This is the one place this scorer is not exact. Cost per block is
/// `O(|C|³)`; left uncapped, a single pathological function whose dependence
/// graph collapses into one giant cycle would make the whole analysis cubic
/// in function size. Realistic loop-carried cycles — the accumulator
/// variables of a handful of nested loops — top out at a handful to a few
/// dozen mutually-dependent statements, so 48 costs nothing in practice
/// (`Σ|C|³` over the densest packing the cap allows, `⌈5000/48⌉` blocks at
/// the cap, is on the order of 10⁷ flops) while still bounding the adversarial
/// case.
///
/// Blocks larger than this, or a capped block whose elimination hits
/// [`PIVOT_EPS`], fall back to bounded Jacobi iteration (see
/// [`iterative_block_solve`]) for `R` and `L`, and to `D_v = 1.0` — no loop
/// credit — for every member. That second half is a real approximation, not
/// a rounding compromise: it silently degrades those nodes to the acyclic
/// `L·R` product.
///
/// The first guess here was `48`, on the theory that a loop-carried cycle is
/// a handful of accumulator variables. Measuring against the evaluation
/// corpus proved that guess wrong in the direction that matters: two real
/// functions — `argparse.ArgumentParser._parse_known_args.<locals>
/// .consume_optional` and `tarfile.TarInfo._proc_pax` — have single
/// dependence-graph cycles of `80` and `61` nodes respectively (both are
/// `while`-loop state machines with several mutually-updated locals). `128`
/// covers both with headroom while keeping the worst realistic packing —
/// `⌈5000/128⌉ ≈ 40` maximally-sized blocks, each costing on the order of
/// `3·128³ ≈ 6×10⁶` flops for the R, L and D solves together — in the tens
/// of milliseconds, not the hundreds.
const EXACT_SCC_CAP: usize = 128;

/// Hard cap on Jacobi sweeps in the oversized/ill-conditioned fallback.
/// Never exceeded regardless of how slowly a block's spectral radius decays —
/// see [`ITER_TOL`] for what "done" means and the module docs for why this
/// path is not expected to run on real code.
const ITER_MAX: usize = 200;

/// Convergence criterion for the fallback solver: stop once the largest
/// per-node update is below this, in absolute terms (all quantities solved
/// here are probabilities or expected visit counts on the order of `1`, so an
/// absolute tolerance is meaningful without rescaling).
const ITER_TOL: f64 = 1e-10;

/// Damping applied only inside the fallback solver. A block that reaches
/// `EXACT_SCC_CAP` or a failed pivot may have leak mass too small for plain
/// Jacobi to contract quickly (or, pathologically, sum to a within-block row
/// total of exactly `1.0`, i.e. no leak at all, which plain Jacobi would not
/// converge on). Multiplying every within-block step by `0.999` guarantees
/// geometric contraction regardless, at the cost of a small, bounded bias
/// toward under-counting loop persistence in exactly the cases already
/// flagged as approximate.
const FALLBACK_DAMPING: f64 = 0.999;

/// Pivot magnitude below which exact Gaussian elimination is abandoned for a
/// block, in favour of the damped iterative fallback, rather than risk
/// dividing by a near-zero pivot.
const PIVOT_EPS: f64 = 1e-12;

/// Per-node Schur-complement deletion-sensitivity score, normalised into
/// `0.0..=1.0` by dividing through by the function's total delivered
/// influence `F`. `0.0` for every node that cannot reach an output at all.
///
/// Tier assignment is untouched by this function — it is called only to
/// overwrite [`crate::importance::NodeImportance::score`] after tiering has
/// already run. See [`crate::importance::analyze_with_scorer`].
#[must_use]
pub fn score(ir: &FunctionIr, graph: &Graph, denylist: &Denylist) -> Vec<f64> {
    let n = graph.n;
    let mut out = vec![0.0f64; n];
    if n == 0 {
        return out;
    }
    let omega = omega_set(ir, denylist);
    if !omega.iter().any(|&b| b) {
        return out; // nothing is ever delivered; every deletion is free.
    }

    let system = System::build(graph, &omega);
    if system.transient.is_empty() {
        // No interior at all: whatever reaches Ω does so in zero steps,
        // meaning the function's entire observable behaviour *is* its
        // output statement(s). There is no flow to attribute, so fall back
        // to reporting these as maximally salient rather than a wall of
        // zeros. See the "single node" test.
        for v in 0..n {
            if omega[v] {
                out[v] = 1.0;
            }
        }
        return out;
    }

    let solved = system.solve(&BTreeSet::new());
    let f = solved.f;
    if f <= 0.0 || !f.is_finite() {
        return out;
    }

    for (i, &v) in system.transient.iter().enumerate() {
        let raw = solved.l[i] * solved.r[i] / solved.d[i].max(1.0);
        out[v] = (raw / f).clamp(0.0, 1.0);
    }
    for v in 0..n {
        if omega[v] {
            let share = solved.omega_score.get(&v).copied().unwrap_or(0.0);
            out[v] = (share / f).clamp(0.0, 1.0);
        }
    }

    // Belt and braces: nothing above should be able to produce a NaN or
    // infinity (every division is guarded above), but the contract is
    // absolute, so scrub defensively rather than trust the derivation.
    for s in &mut out {
        if !s.is_finite() {
            *s = 0.0;
        }
    }
    out
}

/// `Ω`: effect nodes, minus calls the denylist matches. Identical criterion
/// to [`crate::importance::analyze`]'s own "what is a real effect" question.
fn omega_set(ir: &FunctionIr, denylist: &Denylist) -> Vec<bool> {
    ir.nodes
        .iter()
        .map(|node| {
            node.kind.is_effect()
                && !matches!(&node.kind, NodeKind::Call { callee, .. } if denylist.matches(callee))
        })
        .collect()
}

/// Raw (unnormalised) weighted dependence edges, plus each node's total raw
/// out-weight — the normalisation denominator, taken over *every* successor,
/// including ones that never reach `Ω`. Deliberately not
/// [`crate::graph::dependence_successors`]: that function discards weight and
/// multiplicity, both load-bearing here.
fn raw_edges(graph: &Graph) -> (Vec<Vec<(NodeId, f64)>>, Vec<f64>) {
    let n = graph.n;
    let mut acc: Vec<BTreeMap<NodeId, f64>> = vec![BTreeMap::new(); n];
    for (node, row) in acc.iter_mut().enumerate() {
        for &d in &graph.defs_at[node] {
            for &u in &graph.def_users[d] {
                *row.entry(u).or_insert(0.0) += DATA_WEIGHT;
            }
        }
    }
    for (dependent, branches) in graph.ctrl_deps.iter().enumerate() {
        for &b in branches {
            *acc[b].entry(dependent).or_insert(0.0) += CTRL_WEIGHT;
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut totals = Vec::with_capacity(n);
    for m in acc {
        totals.push(m.values().sum());
        out.push(m.into_iter().collect());
    }
    (out, totals)
}

/// Nodes that can reach some member of `omega`, following `out_adj` forward —
/// computed as a multi-source BFS on the transpose, seeded from `omega`.
fn reachable_to(n: usize, out_adj: &[Vec<(NodeId, f64)>], omega: &[bool]) -> Vec<bool> {
    let mut rev: Vec<Vec<NodeId>> = vec![Vec::new(); n];
    for (v, edges) in out_adj.iter().enumerate() {
        for &(u, _) in edges {
            rev[u].push(v);
        }
    }
    let mut seen = vec![false; n];
    let mut q: VecDeque<NodeId> = VecDeque::new();
    for (v, &is_omega) in omega.iter().enumerate() {
        if is_omega {
            seen[v] = true;
            q.push_back(v);
        }
    }
    while let Some(cur) = q.pop_front() {
        for &p in &rev[cur] {
            if !seen[p] {
                seen[p] = true;
                q.push_back(p);
            }
        }
    }
    seen
}

/// The reduced Markov system: transient (non-`Ω`) nodes that can reach an
/// output, with row-normalised transition weight to other transient nodes
/// (`q`) and to `Ω` (`b`), plus enough detail (`omega_share`) to later
/// attribute delivered mass back to individual outputs.
struct System {
    /// Node ids reaching `Ω`, excluding `Ω` itself, sorted ascending.
    transient: Vec<NodeId>,
    /// `local_of[v]` is this node's index into `transient`, if it has one.
    local_of: Vec<Option<usize>>,
    /// `q[i]`: local index `i`'s edges to other transient local indices,
    /// each `(target, probability)`. [`System::solve`] derives its own
    /// (possibly deletion-filtered) transpose from this on demand rather
    /// than caching one here, since a cached transpose would go stale the
    /// moment a caller asks for a deletion.
    q: Vec<Vec<(usize, f64)>>,
    /// `b[i]`: local index `i`'s one-step probability of landing on *some*
    /// `Ω` node directly.
    b: Vec<f64>,
    /// `omega_share[i]`: which `Ω` nodes local index `i` feeds directly, and
    /// with what probability. `Σ_j omega_share[i][j].1 == b[i]`.
    omega_share: Vec<Vec<(NodeId, f64)>>,
}

/// The three solved vectors plus the derived totals, for one system (whole or
/// with a deletion applied).
struct Solved {
    r: Vec<f64>,
    l: Vec<f64>,
    d: Vec<f64>,
    /// Delivered mass per `Ω` node.
    omega_score: BTreeMap<NodeId, f64>,
    /// `Σ omega_score`, the function's total delivered influence.
    f: f64,
}

impl System {
    fn build(graph: &Graph, omega: &[bool]) -> Self {
        let n = graph.n;
        let (raw_out, total_out) = raw_edges(graph);
        let in_s = reachable_to(n, &raw_out, omega);
        let transient: Vec<NodeId> = (0..n).filter(|&v| in_s[v] && !omega[v]).collect();
        let m = transient.len();

        let mut local_of = vec![None; n];
        for (i, &v) in transient.iter().enumerate() {
            local_of[v] = Some(i);
        }

        let mut q: Vec<Vec<(usize, f64)>> = vec![Vec::new(); m];
        let mut b = vec![0.0f64; m];
        let mut omega_share: Vec<Vec<(NodeId, f64)>> = vec![Vec::new(); m];
        for (i, &v) in transient.iter().enumerate() {
            let tot = total_out[v];
            if tot <= 0.0 {
                // Cannot happen for a node that is genuinely in `transient`
                // (it would have no way to reach Ω), but never divide by it.
                continue;
            }
            for &(u, w) in &raw_out[v] {
                let p = w / tot;
                if omega[u] {
                    b[i] += p;
                    omega_share[i].push((u, p));
                } else if let Some(j) = local_of[u] {
                    q[i].push((j, p));
                }
                // else: u is outside S: leaked mass, part of neither.
            }
        }

        Self {
            transient,
            local_of,
            q,
            b,
            omega_share,
        }
    }

    /// Solves the system, optionally with `exclude` (original [`NodeId`]s)
    /// deleted first: their row and column are dropped entirely — edges
    /// into an excluded node are just leaked, not rerouted, matching what
    /// deleting a statement means. Used both for the real score (with an
    /// empty exclusion set) and by the brute-force verification test.
    fn solve(&self, exclude: &BTreeSet<NodeId>) -> Solved {
        let m = self.transient.len();
        let excluded: BTreeSet<usize> = exclude.iter().filter_map(|&v| self.local_of[v]).collect();

        let q_eff: Vec<Vec<(usize, f64)>> = (0..m)
            .map(|i| {
                if excluded.contains(&i) {
                    Vec::new()
                } else {
                    self.q[i]
                        .iter()
                        .copied()
                        .filter(|&(j, _)| !excluded.contains(&j))
                        .collect()
                }
            })
            .collect();
        let b_eff: Vec<f64> = (0..m)
            .map(|i| {
                if excluded.contains(&i) {
                    0.0
                } else {
                    self.b[i]
                }
            })
            .collect();
        let mut q_pred_eff: Vec<Vec<(usize, f64)>> = vec![Vec::new(); m];
        for (i, edges) in q_eff.iter().enumerate() {
            for &(j, w) in edges {
                q_pred_eff[j].push((i, w));
            }
        }

        let succ_struct: Vec<Vec<NodeId>> = q_eff
            .iter()
            .map(|edges| edges.iter().map(|&(j, _)| j).collect())
            .collect();
        let (comp_of, comps) = strongly_connected(m, &succ_struct);

        let d = compute_d(&q_eff, &comps);
        let r = compute_r(&q_eff, &b_eff, &comp_of, &comps);
        let l = compute_l(&q_pred_eff, &comp_of, &comps);

        let mut omega_score: BTreeMap<NodeId, f64> = BTreeMap::new();
        for (i, shares) in self.omega_share.iter().enumerate() {
            if excluded.contains(&i) {
                continue;
            }
            for &(onode, p) in shares {
                *omega_score.entry(onode).or_insert(0.0) += l[i] * p;
            }
        }
        let f: f64 = omega_score.values().copied().sum();

        Solved {
            r,
            l,
            d,
            omega_score,
            f,
        }
    }
}

/// Whether `edges[v]` contains a self-loop.
fn has_self_loop(edges: &[Vec<(usize, f64)>], v: usize) -> bool {
    edges[v].iter().any(|&(u, _)| u == v)
}

/// `D_v = (M⁻¹)_{vv}` for every node: `1.0`, at no cost, outside a nontrivial
/// component; a local dense block inversion — needing nothing from the rest
/// of the graph — for the rest.
fn compute_d(q: &[Vec<(usize, f64)>], comps: &[Vec<NodeId>]) -> Vec<f64> {
    let m = q.len();
    let mut d = vec![1.0f64; m];
    for comp in comps {
        let trivial = comp.len() == 1 && !has_self_loop(q, comp[0]);
        if trivial {
            continue;
        }
        if comp.len() <= EXACT_SCC_CAP
            && let Some(diag) = exact_block_diagonal(q, comp)
        {
            for (li, &v) in comp.iter().enumerate() {
                d[v] = diag[li].max(1.0);
            }
        }
        // Oversized or ill-conditioned: leave D_v at 1.0: the documented
        // fallback (no loop-persistence credit) rather than an O(|C|^3) risk.
    }
    d
}

/// `R = M⁻¹b`, by one sweep over components in Tarjan's emission order
/// (already reverse-topological: a component's `Q`-successors are always
/// already finished).
fn compute_r(
    q: &[Vec<(usize, f64)>],
    b: &[f64],
    comp_of: &[usize],
    comps: &[Vec<NodeId>],
) -> Vec<f64> {
    let m = q.len();
    let mut r = vec![0.0f64; m];
    for (c, members) in comps.iter().enumerate() {
        let rhs: Vec<f64> = members
            .iter()
            .map(|&v| {
                let mut acc = b[v];
                for &(u, w) in &q[v] {
                    if comp_of[u] != c {
                        acc += w * r[u];
                    }
                }
                acc
            })
            .collect();
        if members.len() == 1 && !has_self_loop(q, members[0]) {
            r[members[0]] = rhs[0];
            continue;
        }
        let x = solve_block(q, members, &rhs);
        for (li, &v) in members.iter().enumerate() {
            r[v] = x[li];
        }
    }
    r
}

/// `L = 1ᵀM⁻¹`, by one sweep in the opposite order over the transposed
/// edges: a component's predecessor components are always already finished.
fn compute_l(q_pred: &[Vec<(usize, f64)>], comp_of: &[usize], comps: &[Vec<NodeId>]) -> Vec<f64> {
    let m = q_pred.len();
    let mut l = vec![0.0f64; m];
    for c in (0..comps.len()).rev() {
        let members = &comps[c];
        let rhs: Vec<f64> = members
            .iter()
            .map(|&v| {
                let mut acc = 1.0; // uniform injection
                for &(w, wt) in &q_pred[v] {
                    if comp_of[w] != c {
                        acc += wt * l[w];
                    }
                }
                acc
            })
            .collect();
        if members.len() == 1 && !has_self_loop(q_pred, members[0]) {
            l[members[0]] = rhs[0];
            continue;
        }
        let x = solve_block(q_pred, members, &rhs);
        for (li, &v) in members.iter().enumerate() {
            l[v] = x[li];
        }
    }
    l
}

/// Solves `(I - Q_C) x = rhs` (or, when `edges` is the transposed adjacency,
/// `(I - Q_Cᵀ) x = rhs`) for one nontrivial component, exactly by dense LU
/// when the block is small enough and well-conditioned, otherwise by the
/// bounded damped-Jacobi fallback.
fn solve_block(edges: &[Vec<(usize, f64)>], members: &[NodeId], rhs: &[f64]) -> Vec<f64> {
    let k = members.len();
    if k <= EXACT_SCC_CAP {
        let mut local = vec![usize::MAX; edges.len()];
        for (li, &v) in members.iter().enumerate() {
            local[v] = li;
        }
        let mut mat = build_matrix(edges, members, &local);
        if let Some(perm) = lu_decompose(&mut mat, k) {
            return lu_solve(&mat, &perm, k, rhs);
        }
    }
    iterative_block_solve(edges, members, rhs)
}

/// Diagonal of `(I - Q_C)⁻¹` for one nontrivial component, or `None` if the
/// block exceeds `EXACT_SCC_CAP` or its elimination hits a near-zero pivot.
fn exact_block_diagonal(q: &[Vec<(usize, f64)>], members: &[NodeId]) -> Option<Vec<f64>> {
    let k = members.len();
    let mut local = vec![usize::MAX; q.len()];
    for (li, &v) in members.iter().enumerate() {
        local[v] = li;
    }
    let mut mat = build_matrix(q, members, &local);
    let perm = lu_decompose(&mut mat, k)?;
    let mut diag = Vec::with_capacity(k);
    let mut unit = vec![0.0f64; k];
    for j in 0..k {
        unit[j] = 1.0;
        let x = lu_solve(&mat, &perm, k, &unit);
        diag.push(x[j]);
        unit[j] = 0.0;
    }
    Some(diag)
}

/// Builds `I - Q_C` (or its transpose, when `edges` is a predecessor list)
/// as a flat row-major `k*k` matrix.
fn build_matrix(edges: &[Vec<(usize, f64)>], members: &[NodeId], local: &[usize]) -> Vec<f64> {
    let k = members.len();
    let mut mat = vec![0.0f64; k * k];
    for i in 0..k {
        mat[i * k + i] = 1.0;
    }
    for (li, &v) in members.iter().enumerate() {
        for &(u, w) in &edges[v] {
            if u < local.len() && local[u] != usize::MAX {
                mat[li * k + local[u]] -= w;
            }
        }
    }
    mat
}

/// In-place LU decomposition with partial pivoting (largest magnitude in the
/// remaining column, ties broken by lowest row index: deterministic).
/// Returns `None`, rather than a near-singular factorisation, the moment any
/// pivot's magnitude falls below [`PIVOT_EPS`].
fn lu_decompose(mat: &mut [f64], k: usize) -> Option<Vec<usize>> {
    let mut perm: Vec<usize> = (0..k).collect();
    for col in 0..k {
        let mut best = col;
        let mut best_val = mat[col * k + col].abs();
        for row in (col + 1)..k {
            let v = mat[row * k + col].abs();
            if v > best_val {
                best_val = v;
                best = row;
            }
        }
        if best_val < PIVOT_EPS {
            return None;
        }
        if best != col {
            for c in 0..k {
                mat.swap(col * k + c, best * k + c);
            }
            perm.swap(col, best);
        }
        let pivot = mat[col * k + col];
        for row in (col + 1)..k {
            let factor = mat[row * k + col] / pivot;
            if factor != 0.0 {
                mat[row * k + col] = factor;
                for c in (col + 1)..k {
                    mat[row * k + c] -= factor * mat[col * k + c];
                }
            }
        }
    }
    Some(perm)
}

/// Forward/back substitution against an LU factorisation from
/// [`lu_decompose`].
fn lu_solve(lu: &[f64], perm: &[usize], k: usize, rhs: &[f64]) -> Vec<f64> {
    let mut y = vec![0.0f64; k];
    for i in 0..k {
        let mut s = rhs[perm[i]];
        for (j, yj) in y.iter().enumerate().take(i) {
            s -= lu[i * k + j] * yj;
        }
        y[i] = s;
    }
    let mut x = vec![0.0f64; k];
    for i in (0..k).rev() {
        let mut s = y[i];
        for j in (i + 1)..k {
            s -= lu[i * k + j] * x[j];
        }
        x[i] = s / lu[i * k + i];
    }
    x
}

/// Bounded, damped Jacobi iteration: the fallback for a block too large for
/// `EXACT_SCC_CAP` or whose elimination hit [`PIVOT_EPS`]. Plain Jacobi
/// (every update reads only the *previous* sweep's values) rather than
/// Gauss-Seidel, so the result depends only on the fixed member order, never
/// on incidental update order. Stops the moment the largest per-node change
/// drops below [`ITER_TOL`], and never runs more than [`ITER_MAX`] sweeps
/// regardless — see [`FALLBACK_DAMPING`] for why that cap is not always
/// enough to reach the tolerance, and why that is an accepted, documented
/// limitation rather than a correctness bug.
fn iterative_block_solve(edges: &[Vec<(usize, f64)>], members: &[NodeId], rhs: &[f64]) -> Vec<f64> {
    let k = members.len();
    let mut local = vec![usize::MAX; edges.len()];
    for (li, &v) in members.iter().enumerate() {
        local[v] = li;
    }
    let mut x = rhs.to_vec();
    for _ in 0..ITER_MAX {
        let mut next = vec![0.0f64; k];
        let mut max_delta = 0.0f64;
        for (li, &v) in members.iter().enumerate() {
            let mut acc = rhs[li];
            for &(u, w) in &edges[v] {
                if u < local.len() && local[u] != usize::MAX {
                    acc += FALLBACK_DAMPING * w * x[local[u]];
                }
            }
            max_delta = max_delta.max((acc - x[li]).abs());
            next[li] = acc;
        }
        x = next;
        if max_delta < ITER_TOL {
            break;
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FunctionId, Node};

    fn fid(name: &str) -> FunctionId {
        FunctionId {
            file: "Test.py".into(),
            name: name.into(),
            signature: String::new(),
            decl_line: Some(1),
        }
    }

    /// `x = 0; y = x; return y` — a straight three-node chain, hand-solvable
    /// (worked out in the module's own design notes): R = [1.0, 1.0],
    /// L = [1.0, 2.0], D = [1.0, 1.0], F = 2.0, so the normalised scores are
    /// `0.5`, `1.0`, and the `return` itself absorbs all of `F` for `1.0`.
    #[test]
    fn hand_computable_straight_chain() {
        let ir = FunctionIr {
            id: fid("chain"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]), // x = 0
                Node::pure(2).with_dataflow([2], [1]).with_succs([2]), // y = x
                Node::pure(3)
                    .with_dataflow([], [2])
                    .with_kind(NodeKind::Return), // return y
            ],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        let scores = score(&ir, &graph, &Denylist::new());
        assert_eq!(scores.len(), 3);
        assert!((scores[0] - 0.5).abs() < 1e-9, "got {scores:?}");
        assert!((scores[1] - 1.0).abs() < 1e-9, "got {scores:?}");
        assert!((scores[2] - 1.0).abs() < 1e-9, "got {scores:?}");
    }

    /// A function with exactly one instruction, which is itself the only
    /// effect. There is no interior to solve a linear system over; the
    /// scorer must still report something rather than a NaN or a silent
    /// zero for the function's entire body.
    #[test]
    fn degenerate_single_node() {
        let ir = FunctionIr {
            id: fid("single"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        let scores = score(&ir, &graph, &Denylist::new());
        assert_eq!(scores, vec![1.0]);
    }

    /// `x = 0; while (...) { x = x + 1 }; return x` — the loop body node
    /// both uses and redefines `x`, so reaching-definitions makes it its own
    /// predecessor: a genuine self-loop in the dependence graph, exercising
    /// the `D_v > 1.0` path with no other machinery involved.
    #[test]
    fn cyclic_self_loop_does_not_panic_or_nan() {
        let ir = FunctionIr {
            id: fid("loop"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]), // x = 0
                Node::pure(2)
                    .with_dataflow([1], [1])
                    .with_kind(NodeKind::Branch)
                    .with_succs([1, 2]), // x = x + 1; loop or exit
                Node::pure(3)
                    .with_dataflow([], [1])
                    .with_kind(NodeKind::Return),
            ],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        assert!(graph.loops.iter().any(|l| l.body.contains(&1)));
        let scores = score(&ir, &graph, &Denylist::new());
        assert_eq!(scores.len(), 3);
        for s in &scores {
            assert!(s.is_finite(), "NaN/Inf reached the output: {scores:?}");
            assert!((0.0..=1.0).contains(s), "out of range: {scores:?}");
        }
        // The loop header carries the accumulator both into the next
        // iteration and out to the return; it should not score zero.
        assert!(scores[1] > 0.0, "got {scores:?}");
    }

    /// Deletes five nodes from a graph with branches, fan-in and a loop, one
    /// at a time, and re-solves the *whole system from scratch* for each —
    /// independent of the closed-form Schur identity `score()` uses. This is
    /// the check the design brief asks for: proof the identity holds here,
    /// not just an assertion that it should.
    #[test]
    fn brute_force_matches_deletion() {
        // A function with a diamond (fan-out then fan-in), a loop, and two
        // effects, so both R (multiple downstream routes) and L (multiple
        // upstream routes) take on nontrivial values, and D > 1.0 somewhere.
        //   0: a = input                 succ 1
        //   1: b = a                     succ 2
        //   2: c = a                     succ 3
        //   3: d = b + c   (fan-in)      succ 4
        //   4: branch on d               succ 5, 8
        //   5: e = d                     succ 6
        //   6: e = e + 1 (self-loop)     succ 6, 7   (loop)
        //   7: return e                  (effect)
        //   8: log(d)                    (denylisted, must not be Ω)
        let mut nodes = vec![
            Node::pure(0).with_dataflow([1], []).with_succs([1]), // 0: a
            Node::pure(1).with_dataflow([2], [1]).with_succs([2]), // 1: b = a
            Node::pure(2).with_dataflow([3], [1]).with_succs([3]), // 2: c = a
            Node::pure(3).with_dataflow([4], [2, 3]).with_succs([4]), // 3: d = b+c
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
                    opacity: crate::ir::CallOpacity::Opaque,
                },
                defs: Vec::new(),
                uses: vec![4],
                succs: Vec::new(),
                label: String::new(),
            }, // 8: log(d)
        ];
        // fix line numbers to be distinct/monotonic for realism; irrelevant
        // to the linear algebra.
        for (i, n) in nodes.iter_mut().enumerate() {
            n.line = Some(u32::try_from(i).unwrap_or(u32::MAX) + 1);
        }
        let ir = FunctionIr {
            id: fid("diamond_loop"),
            nodes,
            entry: 0,
        };
        let graph = Graph::build(&ir);
        let denylist = Denylist::new();
        let omega = omega_set(&ir, &denylist);
        assert!(omega[7], "return must be an output");
        assert!(!omega[8], "the logging call must not be an output");

        let system = System::build(&graph, &omega);
        let baseline = system.solve(&BTreeSet::new());
        assert!(baseline.f > 0.0);

        let candidates: Vec<NodeId> = system.transient.clone();
        assert!(candidates.len() >= 5, "fixture too small: {candidates:?}");
        let picks: Vec<NodeId> = (0..5)
            .map(|i| candidates[i * (candidates.len() - 1) / 4])
            .collect();

        for &v in &picks {
            let mut ex = BTreeSet::new();
            ex.insert(v);
            let deleted = system.solve(&ex);
            let brute_force_delta = baseline.f - deleted.f;

            let i = system.local_of[v].expect("picked from transient");
            let raw = baseline.l[i] * baseline.r[i] / baseline.d[i].max(1.0);

            assert!(
                (brute_force_delta - raw).abs() < 1e-9,
                "node {v}: brute-force delta {brute_force_delta} vs closed-form {raw}"
            );
        }
    }

    /// Denylisted calls must never become `Ω`, mirroring
    /// `crate::importance::analyze`'s own inert rule.
    #[test]
    fn denylisted_call_is_not_an_output() {
        let ir = FunctionIr {
            id: fid("logonly"),
            nodes: vec![Node {
                line: Some(1),
                kind: NodeKind::Call {
                    callee: "print".into(),
                    opacity: crate::ir::CallOpacity::Opaque,
                },
                defs: Vec::new(),
                uses: Vec::new(),
                succs: Vec::new(),
                label: String::new(),
            }],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        let scores = score(&ir, &graph, &Denylist::new());
        assert_eq!(scores, vec![0.0]);
    }
}
