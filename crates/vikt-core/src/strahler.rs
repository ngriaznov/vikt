//! An alternative scorer built on Horton–Strahler stream order.
//!
//! [`crate::importance::score_of`] — the `current` scorer — blends dependence-
//! cone size, control-dominance weight, loop depth and effect-ness by fixed
//! weights. This module answers a narrower, structural question instead:
//! **where does this function's dataflow converge?** — borrowing, literally,
//! the ordering geomorphology uses to rank streams in a river network (Horton
//! 1945, Strasser 1952 — usually cited as Strahler 1952, hence the name).
//!
//! # The analogy
//!
//! A river network's springs, the tributaries with no water flowing into
//! them, are order 1. When two streams of *equal* order `k` meet, the
//! confluence continues as order `k + 1`: two order-1 creeks make an order-2
//! stream, two order-2 streams make an order-3 river. When a higher-order
//! stream absorbs a lower-order tributary, nothing changes: the higher order
//! wins outright, because one more small creek joining a river does not make
//! it a bigger river. The network's mainstem, the Mississippi among its
//! thousands of named creeks, is, by construction, wherever the order is
//! highest: not the longest path, not the most tributaries by count, but the
//! point every major branch has funneled into.
//!
//! In a function, dataflow is the water. Constants and parameters, values
//! with nothing feeding them, are springs. A statement where two
//! independently-derived values are combined is a confluence. The statement
//! carrying the highest order is the one a reader would call the "real" work:
//! not necessarily the busiest node, but the one every major derivation
//! chain has, directly or transitively, funneled into.
//!
//! # Precise rule
//!
//! - **Edges are data edges only.** For every def `d` a node `v` reads (from
//!   [`Graph::uses_defs`]), there is an edge `u -> v` where `u =
//!   `[`Graph::defs`]`[d].node`, provided `u != v`. Parallel edges collapse
//!   to one. This is this scorer's distinctive choice: control flow is the
//!   valley walls that shape *where* the water goes, not the water itself,
//!   so branches contribute no edges here — unlike `current`, which weighs
//!   control-dominance directly, and unlike the `schur` scorer, which mixes
//!   a control-dependence channel into the same graph as data. A predicate
//!   matters to this scorer only through whatever data actually flows out of
//!   it (its own inputs still count as edges *into* it, same as any node).
//! - **Loops condense.** Every strongly connected component of the data
//!   graph — mutually recursive locals inside a loop, most commonly — is a
//!   single node in the confluence order: a stream doesn't gain order by
//!   looping back on itself, and every member shares the component's order.
//! - **Order of a component with no incoming inter-component edges is
//!   `1`.** A component with a self-loop and nothing feeding it externally
//!   is still a spring: internal edges never count toward its own order.
//! - **Otherwise**, let `K` be the multiset of orders of its *distinct
//!   upstream components* — one entry per component with an edge into this
//!   one, not one per edge, so a component fed by three edges from the same
//!   order-1 upstream component is fed by exactly one order-1 tributary, not
//!   three. Let `m = max(K)`. The component's order is `m + 1` if `m`
//!   occurs at least twice in `K` (two or more distinct order-`m` tributaries
//!   truly converge here), otherwise `m` (one dominant tributary absorbs the
//!   rest without being promoted).
//!
//! [`score`] is this order, min-max normalised across the whole function
//! into `0.0..=1.0` — the highest-order statement(s) score `1.0`, the
//! springs score `0.0`, and a function whose order never varies (every node
//! equally deep, including the single-node case) scores every node `0.5`
//! rather than manufacturing a fake ranking.
//!
//! # Algorithm and complexity
//!
//! 1. Build the data-edge adjacency directly from [`Graph::uses_defs`] and
//!    [`Graph::defs`] — `O(e)`, where `e` is the number of def-use edges.
//! 2. Tarjan SCC ([`crate::graph::strongly_connected`], reused rather than
//!    re-derived) condenses it into a DAG — `O(n + e)`.
//! 3. Distinct-predecessor-component sets are built once, as one `BTreeSet`
//!    per component — `O(e log e)` worst case, from the set insertions.
//! 4. Components are visited in a single pass over component ids from
//!    highest to lowest. [`strongly_connected`] emits components in reverse
//!    topological order with respect to the same edge direction this module
//!    uses (a component's out-edges always point to an already-finished,
//!    lower-numbered component — see `crate::graph::cone_sizes` for the
//!    same invariant used the same way) — so walking ids downward visits
//!    every upstream component before any of its downstream confluences,
//!    with no separate topological sort. Each component's confluence rule is
//!    then `O(1)` amortised, since every predecessor-component edge is
//!    inspected exactly once across the whole pass.
//! 5. Normalisation is one linear scan.
//!
//! Total cost is `O(n + e log e)` — linear up to the log factor from sorted
//! deduplication, with no term worse than that anywhere in the pipeline.
//!
//! # Determinism
//!
//! Every edge set is a `BTreeSet` before it is ever iterated, the SCC pass is
//! the same iterative, order-fixed Tarjan the rest of the crate already
//! relies on, and the confluence rule only ever compares integers. There is
//! no floating-point accumulation until the final normalisation, and that
//! step is a single min/max scan over already-fixed integers. Two runs over
//! the same bytes produce byte-identical output.

use std::collections::BTreeSet;

use crate::graph::{Graph, strongly_connected};
use crate::ir::NodeId;

/// Per-node Horton–Strahler confluence score, min-max normalised into
/// `0.0..=1.0`. All nodes equal (including the empty and single-node cases)
/// score `0.5`.
#[must_use]
pub fn score(graph: &Graph) -> Vec<f64> {
    let n = graph.n;
    if n == 0 {
        return Vec::new();
    }
    let succ = data_edges(graph);
    let orders = component_orders(n, &succ);
    normalize(&orders)
}

/// Data-only dependence edges `u -> v`: `v` reads a definition `u` made,
/// deduplicated and sorted. Deliberately narrower than
/// [`crate::graph::dependence_successors`], which also carries control
/// edges — those are exactly what this scorer excludes.
fn data_edges(graph: &Graph) -> Vec<Vec<NodeId>> {
    let n = graph.n;
    let mut acc: Vec<BTreeSet<NodeId>> = vec![BTreeSet::new(); n];
    for consumer in 0..n {
        for &def in &graph.uses_defs[consumer] {
            let producer = graph.defs[def].node;
            if producer != consumer {
                acc[producer].insert(consumer);
            }
        }
    }
    acc.into_iter().map(|s| s.into_iter().collect()).collect()
}

/// Per-node Strahler order: condenses `succ` into strongly connected
/// components, orders the condensation by the confluence rule, and broadcasts
/// each component's order back to its members.
fn component_orders(n: usize, succ: &[Vec<NodeId>]) -> Vec<u32> {
    let (comp_of, comps) = strongly_connected(n, succ);
    let ncomp = comps.len();

    // Distinct predecessor components per component: one entry per upstream
    // component, never per edge. `BTreeSet` both deduplicates and, since only
    // component ids ever go in, gives a fixed iteration order for free.
    let mut upstream: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); ncomp];
    for (producer, consumers) in succ.iter().enumerate() {
        let from = comp_of[producer];
        for &consumer in consumers {
            let to = comp_of[consumer];
            if from != to {
                upstream[to].insert(from);
            }
        }
    }

    // `strongly_connected` emits components such that every out-edge of a
    // component (in this module's `producer -> consumer` sense) points to a
    // component already finished at a lower id — the same invariant
    // `crate::graph::cone_sizes` relies on for the identical reason. So a
    // component's *upstream* neighbours (the ones with an edge pointing INTO
    // it) always sit at a strictly higher id, and visiting ids from high to
    // low finishes every upstream component before its downstream
    // confluence is computed, no separate topological sort needed.
    let mut order = vec![0u32; ncomp];
    for component in (0..ncomp).rev() {
        let Some(&max_upstream) = upstream[component]
            .iter()
            .map(|&p| order[p])
            .collect::<Vec<u32>>()
            .iter()
            .max()
        else {
            order[component] = 1; // no incoming inter-component edges: a spring.
            continue;
        };
        let ties = upstream[component]
            .iter()
            .filter(|&&p| order[p] == max_upstream)
            .count();
        order[component] = if ties >= 2 {
            max_upstream + 1
        } else {
            max_upstream
        };
    }

    (0..n).map(|node| order[comp_of[node]]).collect()
}

/// Min-max normalisation into `0.0..=1.0`. A constant input (including the
/// single-node case) maps every value to `0.5` rather than dividing by zero.
fn normalize(values: &[u32]) -> Vec<f64> {
    let Some((&min, &max)) = values.iter().min().zip(values.iter().max()) else {
        return Vec::new();
    };
    if min == max {
        return vec![0.5; values.len()];
    }
    let span = f64::from(max - min);
    values.iter().map(|&v| f64::from(v - min) / span).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::ir::{FunctionId, FunctionIr, Node, NodeKind};

    fn fid(name: &str) -> FunctionId {
        FunctionId {
            file: "Test.py".into(),
            name: name.into(),
            signature: String::new(),
            decl_line: Some(1),
        }
    }

    fn orders_of(ir: &FunctionIr) -> Vec<u32> {
        let graph = Graph::build(ir);
        component_orders(graph.n, &data_edges(&graph))
    }

    /// `a = spring1; p = a; b = spring2; q = b; m = p + q; return m` — two
    /// independent chains that only meet at `m`. Everything upstream of the
    /// merge — both springs and both single-predecessor hops — stays order
    /// `1`; the merge, fed by two distinct order-1 components, becomes order
    /// `2` and stays `2` through the return that merely forwards it.
    #[test]
    fn confluence_of_two_independent_chains() {
        let ir = FunctionIr {
            id: fid("confluence"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]), // 0: a = spring1
                Node::pure(2).with_dataflow([3], [1]).with_succs([2]), // 1: p = a
                Node::pure(3).with_dataflow([2], []).with_succs([3]), // 2: b = spring2
                Node::pure(4).with_dataflow([4], [2]).with_succs([4]), // 3: q = b
                Node::pure(5).with_dataflow([5], [3, 4]).with_succs([5]), // 4: m = p + q
                Node::pure(6)
                    .with_dataflow([], [5])
                    .with_kind(NodeKind::Return), // 5: return m
            ],
            entry: 0,
        };
        let orders = orders_of(&ir);
        assert_eq!(orders, vec![1, 1, 1, 1, 2, 2], "got {orders:?}");

        let graph = Graph::build(&ir);
        let scores = score(&graph);
        for &upstream in &[0, 1, 2, 3] {
            assert!((scores[upstream] - 0.0).abs() < 1e-9, "{scores:?}");
        }
        for &merge in &[4, 5] {
            assert!((scores[merge] - 1.0).abs() < 1e-9, "{scores:?}");
        }
    }

    /// `p = a1 + a2` (itself a confluence of two order-1 springs, so `p` is
    /// order `2`) merges with an independent order-1 spring `q` at `r`. Since
    /// `p`'s order-2 has no equal-order partner at that merge, `r` stays
    /// order `2` rather than being promoted to `3` — a higher-order stream
    /// absorbing a lower-order tributary keeps its order.
    #[test]
    fn unequal_merge_keeps_the_max() {
        let ir = FunctionIr {
            id: fid("unequal_merge"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]), // 0: a1 = spring
                Node::pure(2).with_dataflow([2], []).with_succs([2]), // 1: a2 = spring
                Node::pure(3).with_dataflow([3], [1, 2]).with_succs([3]), // 2: p = a1 + a2
                Node::pure(4).with_dataflow([4], []).with_succs([4]), // 3: q = spring
                Node::pure(5).with_dataflow([5], [3, 4]).with_succs([5]), // 4: r = p + q
                Node::pure(6)
                    .with_dataflow([], [5])
                    .with_kind(NodeKind::Return), // 5: return r
            ],
            entry: 0,
        };
        let orders = orders_of(&ir);
        assert_eq!(orders, vec![1, 1, 2, 1, 2, 2], "got {orders:?}");
    }

    /// `b = spring; y0 = spring; loop { x = y + b; y = x + 1 }; return y` —
    /// `x` and `y` mutually redefine each other across the back edge, so
    /// nodes 2 and 3 form one strongly connected component. That component
    /// is fed by two distinct order-1 springs (`b` and the initial `y0`), so
    /// the whole cycle — both members alike — condenses to order `2`, which
    /// then carries straight through to the return. Exercises SCC handling
    /// and confluence together: the confluence rule is being applied to the
    /// condensation, not to individual nodes inside the loop.
    #[test]
    fn cycle_condenses_then_confluence_applies() {
        let ir = FunctionIr {
            id: fid("cycle_confluence"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]), // 0: b = spring
                Node::pure(2).with_dataflow([2], []).with_succs([2]), // 1: y0 = spring
                Node::pure(3).with_dataflow([3], [2, 1]).with_succs([3]), // 2: x = y + b
                Node::pure(4)
                    .with_dataflow([2], [3])
                    .with_kind(NodeKind::Branch)
                    .with_succs([2, 4]), // 3: y = x + 1; loop or exit
                Node::pure(5)
                    .with_dataflow([], [2])
                    .with_kind(NodeKind::Return), // 4: return y
            ],
            entry: 0,
        };
        let graph = Graph::build(&ir);
        assert!(
            graph
                .loops
                .iter()
                .any(|l| l.body.contains(&2) && l.body.contains(&3)),
            "fixture must actually loop"
        );
        let orders = orders_of(&ir);
        assert_eq!(orders, vec![1, 1, 2, 2, 2], "got {orders:?}");
    }

    /// Running the same lowered fixture twice, including through the public
    /// [`score`] entry point, must produce byte-identical output: no hash-map
    /// iteration, no floating point accumulation order sensitivity, nothing
    /// time- or memory-address-dependent anywhere in the pipeline.
    #[test]
    fn deterministic_on_a_lowered_fixture() {
        // A function with fan-out, fan-in, a loop and a branch, so every code
        // path in `component_orders` runs at least once.
        let ir = FunctionIr {
            id: fid("determinism"),
            nodes: vec![
                Node::pure(1).with_dataflow([1], []).with_succs([1]), // 0: a = input
                Node::pure(2).with_dataflow([2], [1]).with_succs([2]), // 1: b = a
                Node::pure(3).with_dataflow([3], [1]).with_succs([3]), // 2: c = a
                Node::pure(4).with_dataflow([4], [2, 3]).with_succs([4]), // 3: d = b + c
                Node::pure(5)
                    .with_dataflow([], [4])
                    .with_kind(NodeKind::Branch)
                    .with_succs([5, 8]), // 4: branch on d
                Node::pure(6).with_dataflow([5], [4]).with_succs([6]), // 5: e = d
                Node::pure(7)
                    .with_dataflow([5], [5])
                    .with_kind(NodeKind::Branch)
                    .with_succs([6, 7]), // 6: e = e + 1 (loop)
                Node::pure(8)
                    .with_dataflow([], [5])
                    .with_kind(NodeKind::Return), // 7: return e
                Node::pure(9)
                    .with_dataflow([], [4])
                    .with_kind(NodeKind::Return), // 8: return d
            ],
            entry: 0,
        };
        let graph_a = Graph::build(&ir);
        let graph_b = Graph::build(&ir);
        let first = score(&graph_a);
        let second = score(&graph_b);
        assert_eq!(first, second, "same input must score identically every run");

        let orders_first = orders_of(&ir);
        let orders_second = orders_of(&ir);
        assert_eq!(orders_first, orders_second);
    }

    /// Empty and single-node functions must not panic, and both count as
    /// "all equal" for normalisation purposes.
    #[test]
    fn degenerate_sizes_do_not_panic() {
        let empty = FunctionIr {
            id: fid("empty"),
            nodes: vec![],
            entry: 0,
        };
        let graph = Graph::build(&empty);
        assert_eq!(score(&graph), Vec::<f64>::new());

        let single = FunctionIr {
            id: fid("single"),
            nodes: vec![Node::pure(1).with_kind(NodeKind::Return)],
            entry: 0,
        };
        let graph = Graph::build(&single);
        assert_eq!(score(&graph), vec![0.5]);
    }
}
