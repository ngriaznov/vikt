//! Convergence scorer: **where does this function's dataflow converge?** —
//! the [Strahler number](https://en.wikipedia.org/wiki/Strahler_number)
//! (Horton 1945, Strahler 1952) of the data-dependence graph: constants and
//! parameters are order-1 springs; where two order-`k` derivations meet the
//! stream continues as `k + 1`; a higher order absorbs lower tributaries
//! unchanged. The highest-order statement is the one every major derivation
//! chain has funneled into.
//!
//! Distinctive choices: data edges only — control flow is the valley walls,
//! not the water, so predicates contribute no edges (their own inputs still
//! count); SCCs condense to one node (no order gained by looping on
//! yourself) and an externally unfed component is still a spring; upstream
//! orders count once per distinct upstream *component*, not per edge, and
//! promotion to `m + 1` requires at least two distinct order-`m`
//! tributaries. [`score`] min-max normalises the order to `0.0..=1.0` per
//! function; a function whose order never varies scores `0.5` everywhere
//! rather than manufacturing a fake ranking. Cost `O(n + e log e)`;
//! deterministic (`BTreeSet` edges, order-fixed Tarjan, integer-only until
//! the final normalisation).

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
