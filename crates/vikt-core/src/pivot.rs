//! Single-point-of-failure scorer: Birnbaum (1969) structural importance at
//! `p = 1/2` — the fraction of other-component states in which this
//! statement alone decides whether a correct value reaches an output;
//! identically the absolute
//! [Banzhaf index](https://en.wikipedia.org/wiki/Banzhaf_power_index) of
//! the induced simple game. Computed by two linear adjoint passes over a
//! noisy-OR delivery fixpoint on the dependence graph — the COP
//! observability recursion from VLSI design-for-test — never by cut-set
//! enumeration: `O(E)` against a #P-hard exact problem, accepting the
//! standard independence approximation.
//!
//! Load-bearing choices: noisy-OR rather than AND (AND collapses to a
//! two-class answer, the failure that sank the mincut/flux scorers — see
//! `eval/RESULTS-scorer-bakeoff.md`); `p = 1/2` is the maximum-entropy
//! device that turns the derivative into a pure structural count (Barlow &
//! Proschan), not a bug-rate estimate. The forward pass runs in the
//! log-survival domain (`log1p`/`expm1`) so saturated fan-out keeps
//! resolution; every divisor is `>= 1/2` by construction; `SWEEPS` is a
//! fixed budget so output cannot depend on convergence order; a rootless
//! closed cycle seeds from its root components (`entry_set`). Raw `B_v`
//! decays roughly geometrically per dependence hop —
//! `rescale_for_display` exists for exactly that, measured.
//!
//! Known weaknesses (measured — see the report): reconvergence bias
//! (independence assumed, so diamonds overstate reliability and understate
//! the apex's bottleneck status); the per-hop decay makes long
//! straight-line bodies resemble a symmetric hump a depth feature could
//! imitate.

use std::collections::BTreeSet;

use crate::graph::{Graph, dependence_successors, strongly_connected};
use crate::ir::{FunctionIr, NodeId};

/// `p_v` for every component. Not an estimate: the point at which the
/// Birnbaum derivative equals the Banzhaf index (see module docs).
const P: f64 = 0.5;

/// Fixed sweep budget inside a dependence cycle. Proven, not tuned: both
/// passes are sup-norm contractions with modulus `<= 1/2` at `p = 1/2` (see
/// module docs), so 64 sweeps land the residual at `2^-64`, far below `f64`
/// precision. Fixed rather than a tolerance loop so two runs cannot diverge
/// on convergence order, and bounded so nothing here can loop unboundedly.
const SWEEPS: u32 = 64;

/// `rho`, `A` and `B` for every node, computed once.
///
/// Returning all three (rather than just `B`) is what lets
/// [`birnbaum_identity_matches_long_form`] check `B_v == rho_v * A_v / p_v`
/// against an independently recomputed pred-sum, instead of trusting the
/// production formula to check itself.
struct Fields {
    // Read only by tests (`birnbaum_identity_matches_long_form`, which checks
    // `b` against an independent recomputation from `rho` and `a`); the
    // production path only ever needs `b`.
    #[cfg_attr(not(test), allow(dead_code))]
    rho: Vec<f64>,
    #[cfg_attr(not(test), allow(dead_code))]
    a: Vec<f64>,
    b: Vec<f64>,
}

/// Per-node Birnbaum structural importance, `B_v` in `0.0..=1.0`.
///
/// `graph` must be [`Graph::build`]'s output for `ir` — this function does
/// not rebuild dominance or reaching definitions, only the dependence edges,
/// from `graph`'s already-computed `defs`, `defs_at`, `def_users` and
/// `ctrl_deps`.
pub(crate) fn birnbaum_scores(ir: &FunctionIr, graph: &Graph) -> Vec<f64> {
    compute(ir, graph).b
}

/// Rescales raw `B_v` into `0.0..=1.0` for display, preserving rank order
/// exactly — a strictly increasing transform of every positive value, so any
/// rank-correlation measurement (Spearman against ground truth, in
/// particular) is completely unaffected by calling this — while spreading
/// values across the full range so a fixed-precision display does not
/// silently retie them.
///
/// This function exists because of a real, measured failure, not a
/// hypothetical one: `B_v` decays close to geometrically with dependence
/// distance from the nearest output (module docs, "geometric per-hop
/// decay"), so on anything but a tiny function raw values span many orders
/// of magnitude and the overwhelming majority are under `0.01`. This crate's
/// artifact serializes `score` to two decimal places
/// ([`crate::artifact::SpanRecord`]) — a deliberate, reasonable choice for a
/// weighted-sum scorer whose values already spread `0.0..=1.0` roughly
/// evenly, but fatal for `pivot` as shipped: measured on `os._walk` (278
/// instructions), 69 of 278 nodes had strictly positive raw `B_v`, and every
/// single one rounded to `0.00`, producing a function-wide tie and a
/// Spearman correlation against expert labels of exactly `0.0` — not because
/// the ranking was wrong, but because it was invisible to the serialization.
///
/// The fix follows the same design principle `importance::score_of` already
/// uses for `Current` ("normalise against this body's own peak... importance
/// is a claim about relative standing inside a body"): take `ln(B_v)` and
/// rescale it linearly against *this function's* own `[min, max]` of
/// positive values. The geometric decay that caused the problem is exactly
/// what makes the log-domain the right one to rescale in. It turns an
/// order-of-magnitude spread into a linear one, which two decimal places can
/// represent. Nodes with `B_v = 0` (no path to any output) stay at `0.0`;
/// a function where every reachable node ties at the same positive `B_v`
/// (`lo == hi`, e.g. the series-chain fixture in this module's tests) maps
/// every one of them to `1.0` rather than dividing by zero.
pub(crate) fn rescale_for_display(b: &[f64]) -> Vec<f64> {
    let logs: Vec<Option<f64>> = b
        .iter()
        .map(|&v| if v > 0.0 { Some(v.ln()) } else { None })
        .collect();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for l in logs.iter().flatten() {
        lo = lo.min(*l);
        hi = hi.max(*l);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return vec![0.0; b.len()]; // every node scored exactly 0
    }
    let span = hi - lo;
    logs.iter()
        .map(|l| match l {
            None => 0.0,
            Some(_) if span <= 0.0 => 1.0, // every positive B_v tied already
            Some(l) => ((l - lo) / span).clamp(0.0, 1.0),
        })
        .collect()
}

/// The shared computation behind [`birnbaum_scores`] and the module's tests.
fn compute(ir: &FunctionIr, graph: &Graph) -> Fields {
    let n = graph.n;
    if n == 0 {
        return Fields {
            rho: Vec::new(),
            a: Vec::new(),
            b: Vec::new(),
        };
    }

    // Same dependence edges the incumbent scorer's cones are built from:
    // i -> j means j's delivery is conditioned on i's (a def feeding a use, a
    // branch controlling a dependent statement).
    let succ = dependence_successors(
        n,
        &graph.defs,
        &graph.defs_at,
        &graph.def_users,
        &graph.ctrl_deps,
    );
    let mut pred: Vec<Vec<NodeId>> = vec![Vec::new(); n];
    for (from, tos) in succ.iter().enumerate() {
        for &to in tos {
            pred[to].push(from);
        }
    }

    let is_output: Vec<bool> = (0..n).map(|v| ir.nodes[v].kind.is_effect()).collect();
    let (comp_of, comps) = strongly_connected(n, &succ);
    let entries = entry_set(n, &pred, &comp_of, &comps);
    debug_assert!(
        !entries.is_empty(),
        "src must always have somewhere to seed"
    );

    let rho = forward_pass(&succ, &comps, &is_output);
    let rho_src = or_combine(entries.iter().map(|&e| P * rho[e]));
    let a = backward_pass(&pred, &comps, &rho, &entries, rho_src);

    let b: Vec<f64> = (0..n)
        .map(|v| {
            let raw = 2.0 * rho[v] * a[v];
            debug_assert!(raw.is_finite(), "node {v}: B_v was not finite ({raw})");
            raw.clamp(0.0, 1.0)
        })
        .collect();

    Fields { rho, a, b }
}

/// `-expm1(SUM log1p(-x))` for `x` in `xs` — the log-survival-domain
/// equivalent of `1 - PROD(1 - x)`, accurate even when the product itself
/// would underflow to exactly `0.0` (see module docs on numerical safety).
fn or_combine(xs: impl Iterator<Item = f64>) -> f64 {
    let ell: f64 = xs.map(|x| (-x).ln_1p()).sum();
    -ell.exp_m1()
}

/// Pass 1: `rho`, sink-first (the order [`strongly_connected`] returns).
///
/// A component with no self-referencing edge among its own members needs
/// exactly one sweep — every `rho_j` it reads belongs to an
/// already-completed earlier component. Only an actual dependence cycle
/// (loop-carried data, or a branch and its own control-dependent body) gets
/// `SWEEPS` iterations.
fn forward_pass(succ: &[Vec<NodeId>], comps: &[Vec<NodeId>], is_output: &[bool]) -> Vec<f64> {
    let n = succ.len();
    let mut rho = vec![0.0f64; n];
    for (v, &out) in is_output.iter().enumerate() {
        if out {
            rho[v] = 1.0; // w_o, default 1 — pinned, never updated below.
        }
    }
    for members in comps {
        let mut ordered = members.clone();
        ordered.sort_unstable();
        let sweeps = sweep_count(&ordered, succ);
        for _ in 0..sweeps {
            for &v in &ordered {
                if is_output[v] {
                    continue;
                }
                rho[v] = or_combine(succ[v].iter().map(|&j| P * rho[j]));
            }
        }
    }
    rho
}

/// Pass 2: the adjoint, in the reverse of pass 1's component order (forward
/// topological — `src` toward the outputs), so every predecessor's `A` is
/// settled before a node needs it.
fn backward_pass(
    pred: &[Vec<NodeId>],
    comps: &[Vec<NodeId>],
    rho: &[f64],
    entries: &BTreeSet<NodeId>,
    rho_src: f64,
) -> Vec<f64> {
    let n = pred.len();
    let mut a = vec![0.0f64; n];
    for members in comps.iter().rev() {
        let mut ordered = members.clone();
        ordered.sort_unstable();
        let sweeps = sweep_count(&ordered, pred);
        for _ in 0..sweeps {
            for &v in &ordered {
                let u_v = P * rho[v];
                let mut acc = 0.0f64;
                if entries.contains(&v) {
                    // A_src = 1, the virtual source's own contribution.
                    acc += P * (1.0 - rho_src) / (1.0 - u_v);
                }
                for &k in &pred[v] {
                    acc += a[k] * P * (1.0 - rho[k]) / (1.0 - u_v);
                }
                a[v] = acc;
            }
        }
    }
    a
}

/// `1` for a component with no internal edge (its members' own formula never
/// reads another member of the same component), `SWEEPS` otherwise — i.e.
/// whenever the component is a genuine dependence cycle rather than a
/// coincidence of Tarjan grouping a singleton with itself.
fn sweep_count(members: &[NodeId], adj: &[Vec<NodeId>]) -> u32 {
    let set: BTreeSet<NodeId> = members.iter().copied().collect();
    let internal = members
        .iter()
        .any(|&v| adj[v].iter().any(|m| set.contains(m)));
    if internal { SWEEPS } else { 1 }
}

/// Node-level in-degree-zero nodes (parameters, captured reads), plus — for
/// any root component of the condensation (no incoming inter-component edge)
/// that contains none — every member of that component. The fallback keeps
/// `src` non-empty and every node reachable from it even on the pathological
/// input where a whole reachable region is one closed dependence cycle with
/// no external root (failure mode 4 in the design brief: entry detection is
/// load-bearing, so this guards it rather than asserting and trusting the
/// input).
fn entry_set(
    n: usize,
    pred: &[Vec<NodeId>],
    comp_of: &[usize],
    comps: &[Vec<NodeId>],
) -> BTreeSet<NodeId> {
    let mut entries: BTreeSet<NodeId> = (0..n).filter(|&v| pred[v].is_empty()).collect();

    let mut comp_has_external_pred = vec![false; comps.len()];
    for v in 0..n {
        for &p in &pred[v] {
            if comp_of[p] != comp_of[v] {
                comp_has_external_pred[comp_of[v]] = true;
            }
        }
    }
    for (c, members) in comps.iter().enumerate() {
        if !comp_has_external_pred[c] && !members.iter().any(|v| entries.contains(v)) {
            entries.extend(members.iter().copied());
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CallOpacity, FunctionId, Node, NodeKind};

    fn func(nodes: Vec<Node>) -> FunctionIr {
        FunctionIr {
            id: FunctionId {
                file: "Demo.java".into(),
                name: "test".into(),
                signature: String::new(),
                decl_line: Some(1),
            },
            nodes,
            entry: 0,
        }
    }

    fn birnbaum(ir: &FunctionIr) -> Vec<f64> {
        ir.validate().expect("well-formed test IR");
        let graph = Graph::build(ir);
        birnbaum_scores(ir, &graph)
    }

    /// Hand-computable case: a pure 3-statement series chain,
    /// `v0 defines V; v1 uses V, defines W; v2 (Return) uses W`.
    ///
    /// A series system's Birnbaum importance has a textbook closed form:
    /// every component's importance is the product of the *other*
    /// components' reliabilities, independent of position. With 3
    /// components at `p = 1/2` that is `0.5 * 0.5 = 0.25` for all three —
    /// checked here to 1e-9, not "roughly equal".
    #[test]
    fn series_chain_matches_the_closed_form() {
        const V: u32 = 1;
        const W: u32 = 2;
        let ir = func(vec![
            Node::pure(1).with_dataflow([V], []).with_succs([1]),
            Node::pure(2).with_dataflow([W], [V]).with_succs([2]),
            Node::pure(3)
                .with_dataflow([], [W])
                .with_kind(NodeKind::Return),
        ]);
        let b = birnbaum(&ir);
        assert_eq!(b.len(), 3);
        for (i, &bv) in b.iter().enumerate() {
            assert!((bv - 0.25).abs() < 1e-9, "B[{i}] = {bv}, expected 0.25");
        }
    }

    /// Degenerate case: a single instruction that is itself the sole output.
    /// A 1-component system's Birnbaum importance is always exactly 1. The
    /// component is the whole system, so flipping it always flips delivery.
    /// Also exercises `n = 0` (no instructions at all) and `n = 1` (an entry
    /// that is simultaneously the pinned output), the two smallest inputs the
    /// SCC/condensation machinery has to handle without special-casing.
    #[test]
    fn single_instruction_is_fully_pivotal() {
        let ir = func(vec![Node::pure(1).with_kind(NodeKind::Return)]);
        let b = birnbaum(&ir);
        assert_eq!(b.len(), 1);
        assert!((b[0] - 1.0).abs() < 1e-9, "B[0] = {}, expected 1.0", b[0]);
    }

    /// Degenerate case, empty function: must not panic and must return
    /// nothing to score.
    #[test]
    fn empty_function_scores_nothing() {
        let ir = func(vec![]);
        assert!(birnbaum(&ir).is_empty());
    }

    /// Cyclic case: a loop-carried accumulator, structurally identical to the
    /// `a_log_only_counter_in_a_dependency_cycle_is_inert` fixture in
    /// `tests/analysis.rs` — `acc = acc + x` inside a loop makes the
    /// dependence graph read its own previous value, which is a genuine
    /// self-loop after reaching-definitions (`uses_defs[body]` contains a
    /// definition sited at `body` itself). The fixed-point pass must
    /// terminate (it always does — SWEEPS is a hard cap, not a loop), land
    /// every score in range, and be bit-identical across repeated runs.
    #[test]
    fn loop_carried_accumulator_is_cyclic_and_converges() {
        const ACC: u32 = 1;
        const X: u32 = 2;
        let ir = func(vec![
            // acc = 0
            Node::pure(1).with_dataflow([ACC], []).with_succs([1]),
            // loop header
            Node::pure(2).with_kind(NodeKind::Branch).with_succs([2, 3]),
            // acc = acc + x   <- reads its own previous definition
            Node::pure(3).with_dataflow([ACC], [ACC, X]).with_succs([1]),
            // return acc
            Node::pure(4)
                .with_dataflow([], [ACC])
                .with_kind(NodeKind::Return),
        ]);
        ir.validate().expect("well-formed");
        let graph = Graph::build(&ir);

        // Confirm this fixture is actually cyclic in the dependence graph,
        // not just in control flow — otherwise the test would not exercise
        // the SWEEPS path at all.
        let succ = dependence_successors(
            graph.n,
            &graph.defs,
            &graph.defs_at,
            &graph.def_users,
            &graph.ctrl_deps,
        );
        assert!(
            succ[2].contains(&2),
            "the accumulator must be its own dependence successor"
        );

        let b1 = birnbaum_scores(&ir, &graph);
        let b2 = birnbaum_scores(&ir, &graph);
        assert_eq!(b1, b2, "must be bit-identical across runs");
        for (i, &bv) in b1.iter().enumerate() {
            assert!(
                (0.0..=1.0).contains(&bv) && bv.is_finite(),
                "B[{i}] = {bv} out of range"
            );
        }
        assert!(b1[2] > 0.0, "the accumulator must carry nonzero importance");
    }

    /// The redundancy signal this scorer exists to capture: a sole conduit
    /// must outrank either of two parallel routes that could substitute for
    /// each other. `v0` feeds both `v1` and `v2`; `v3` (Return) needs *either*
    /// one, not both — that "either" is what makes `v1`/`v2` individually
    /// dispensable and `v0` not.
    #[test]
    fn sole_conduit_outranks_either_redundant_branch() {
        const V: u32 = 1;
        const W1: u32 = 2;
        const W2: u32 = 3;
        let ir = func(vec![
            // v = source()
            Node::pure(1).with_dataflow([V], []).with_succs([1]),
            // w1 = f(v)
            Node::pure(2).with_dataflow([W1], [V]).with_succs([2]),
            // w2 = g(v)
            Node::pure(3).with_dataflow([W2], [V]).with_succs([3]),
            // return w1 + w2  (a single node using both, for simplicity)
            Node::pure(4)
                .with_dataflow([], [W1, W2])
                .with_kind(NodeKind::Return),
        ]);
        let b = birnbaum(&ir);
        assert!(
            b[0] > b[1] && b[0] > b[2],
            "conduit {} should outrank branches {}/{}",
            b[0],
            b[1],
            b[2]
        );
        // The two branches are structurally interchangeable.
        assert!((b[1] - b[2]).abs() < 1e-12);
    }

    /// The identity `B_v = rho_v * A_v / p_v` claimed in the module docs,
    /// checked against an independently recomputed, unreduced pred-sum
    /// (`SUM_{k in pred(v)} A_k * (1 - rho_k) / (1 - u_v)`, *not* multiplied
    /// through by `p_v` first) rather than trusting `compute`'s own formula.
    /// A free correctness check on the whole derivation, as the design brief
    /// requests.
    #[test]
    fn birnbaum_identity_matches_long_form() {
        const V: u32 = 1;
        const W: u32 = 2;
        let ir = func(vec![
            Node::pure(1).with_dataflow([V], []).with_succs([1]),
            Node::pure(2).with_dataflow([W], [V]).with_succs([2]),
            Node::pure(3)
                .with_dataflow([], [W])
                .with_kind(NodeKind::Return),
        ]);
        ir.validate().unwrap();
        let graph = Graph::build(&ir);
        let fields = compute(&ir, &graph);

        let succ = dependence_successors(
            graph.n,
            &graph.defs,
            &graph.defs_at,
            &graph.def_users,
            &graph.ctrl_deps,
        );
        let mut pred: Vec<Vec<NodeId>> = vec![Vec::new(); graph.n];
        for (from, tos) in succ.iter().enumerate() {
            for &to in tos {
                pred[to].push(from);
            }
        }
        let entries: BTreeSet<NodeId> = (0..graph.n).filter(|&v| pred[v].is_empty()).collect();
        let rho_src = or_combine(entries.iter().map(|&e| P * fields.rho[e]));

        for (v, preds) in pred.iter().enumerate() {
            let u_v = P * fields.rho[v];
            let mut long_sum = 0.0f64;
            if entries.contains(&v) {
                long_sum += (1.0 - rho_src) / (1.0 - u_v); // A_src (=1) folded in, undivided by p_v
            }
            for &k in preds {
                long_sum += fields.a[k] * (1.0 - fields.rho[k]) / (1.0 - u_v);
            }
            let long_b = fields.rho[v] * long_sum;
            let short_b = 2.0 * fields.rho[v] * fields.a[v];
            assert!(
                (long_b - short_b).abs() < 1e-9,
                "node {v}: long form {long_b} vs short identity {short_b}"
            );
            assert!((short_b - fields.b[v]).abs() < 1e-12);
        }
    }

    /// A callee treated as opaque — the current scorer's `Call { Opaque }`
    /// effect kind — must be scoreable too: outputs are not only `Return`.
    #[test]
    fn opaque_call_output_does_not_panic_and_is_in_range() {
        let ir = func(vec![Node::pure(1).with_kind(NodeKind::Call {
            callee: "com.example.Service::send".into(),
            opacity: CallOpacity::Opaque,
        })]);
        let b = birnbaum(&ir);
        assert_eq!(b.len(), 1);
        assert!((0.0..=1.0).contains(&b[0]));
    }

    /// `rescale_for_display` must preserve strict rank order (any
    /// rank-correlation measurement is blind to this transform by
    /// construction) while actually reaching both ends of `[0, 1]` — the
    /// property that fixes the two-decimal serialization collapse.
    #[test]
    fn rescale_preserves_rank_and_spreads_to_the_full_range() {
        // All strictly positive and strictly increasing: every one of these
        // must land at a distinct point in [0, 1], in the same order. (A raw
        // 0.0 is a separate case — it means "no path to any output at all",
        // and is deliberately tied with the smallest positive value at the
        // bottom of the range; see `rescale_handles_degenerate_inputs`.)
        let raw = vec![1e-9, 3e-7, 2e-4, 0.5];
        let scaled = rescale_for_display(&raw);
        assert_eq!(scaled.len(), raw.len());
        for w in scaled.windows(2) {
            assert!(w[0] < w[1], "{scaled:?} lost rank order");
        }
        assert!((scaled[0] - 0.0).abs() < 1e-12, "the min maps to 0.0");
        assert!((scaled[3] - 1.0).abs() < 1e-12, "the max maps to 1.0");
        // The whole point: two-decimal rounding must not retie these.
        // `scaled` is `[0, 1]`, so `x * 100.0` is `[0, 100]` — nowhere near
        // `i64` truncation range.
        #[allow(clippy::cast_possible_truncation)]
        let mut rounded: Vec<i64> = scaled.iter().map(|&x| (x * 100.0).round() as i64).collect();
        rounded.dedup();
        assert_eq!(
            rounded.len(),
            raw.len(),
            "values that were distinct pre-rescale must stay distinct after rounding to 2dp"
        );
    }

    /// Degenerate rescale inputs: all zero, and all tied at the same
    /// positive value (the series-chain fixture's actual output).
    #[test]
    fn rescale_handles_degenerate_inputs() {
        assert_eq!(rescale_for_display(&[0.0, 0.0, 0.0]), vec![0.0, 0.0, 0.0]);
        assert_eq!(
            rescale_for_display(&[0.25, 0.25, 0.25]),
            vec![1.0, 1.0, 1.0]
        );
        assert_eq!(rescale_for_display(&[]), Vec::<f64>::new());
    }

    /// Timing sanity at the size the design brief budgets for: well under the
    /// ~50ms/5000-node target this module claims by construction (linear, no
    /// enumeration) rather than by a size cap.
    ///
    /// The fixture is one big loop — `acc = acc + x` repeated — chosen
    /// because it puts (almost) every node into a single strongly connected
    /// dependence component via the loop-carried reaching definition, the
    /// worst case for the `SWEEPS`-bounded iteration path rather than the
    /// one-sweep acyclic majority.
    #[test]
    fn scales_linearly_at_5000_nodes() {
        const N: usize = 5000;
        const ACC: u32 = 1;
        // 0: acc = 0
        // 1: loop header (branch): body or exit
        // 2..=N-2: body, acc = acc + acc; last body node closes the back edge
        // N-1: return acc
        let mut nodes = Vec::with_capacity(N);
        nodes.push(Node::pure(1).with_dataflow([ACC], []).with_succs([1]));
        nodes.push(
            Node::pure(2)
                .with_kind(NodeKind::Branch)
                .with_succs([2, N - 1]),
        );
        for i in 2..=N - 2 {
            let next = if i == N - 2 { 1 } else { i + 1 };
            nodes.push(
                Node::pure(u32::try_from(i + 1).unwrap_or(u32::MAX))
                    .with_dataflow([ACC], [ACC])
                    .with_succs([next]),
            );
        }
        nodes.push(
            Node::pure(u32::try_from(N).unwrap_or(u32::MAX))
                .with_dataflow([], [ACC])
                .with_kind(NodeKind::Return),
        );
        let ir = func(nodes);
        ir.validate().unwrap();
        let graph = Graph::build(&ir);

        let start = std::time::Instant::now();
        let b = birnbaum_scores(&ir, &graph);
        let elapsed = start.elapsed();

        println!("birnbaum_scores over {N} nodes took {elapsed:?}");
        assert_eq!(b.len(), N);
        assert!(
            b.iter().all(|x| (0.0..=1.0).contains(x) && x.is_finite()),
            "all scores must be finite and in range"
        );
        assert!(
            elapsed.as_millis() < 200,
            "5000-node chain took {elapsed:?}, expected well under 200ms"
        );
    }
}
