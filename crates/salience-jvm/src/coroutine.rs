//! Excising the Kotlin coroutine state machine before graph analysis.
//!
//! A `suspend fun` is compiled into a state machine in the same JVM method. The
//! prologue reads a `label` field off the continuation object and dispatches
//! through a `tableswitch` whose arms jump *directly into the middle of the
//! function body* — one arm per suspension point, plus a `default` arm that
//! throws `IllegalStateException`.
//!
//! That dispatch makes the control-flow graph **irreducible**, and irreducible
//! in the one way that matters here. Measured on `kotlinc 2.1.20`:
//!
//! ```text
//! suspend fun fetchAndTotal(orders: List<Order>, ...): Double {
//!     for (o in orders) { delay(1); total += ... }     // a loop
//! }
//!
//! node  31  switch %64 { 0 => #005C, 1 => #00D0, 2 => #014A, else => #0165 }
//! node 117  goto #008A                                  // the loop's back edge
//! ```
//!
//! The back edge from 117 exists, but arm `1 => #00D0` enters the loop body
//! without passing the loop header, so the header does not dominate the tail.
//! A textbook natural-loop detector — which is exactly what this crate's core
//! runs — therefore finds **zero loops** in a `suspend` function whose loop
//! contains a suspension point. Every loop-carried definition in it goes
//! unreported, and control dependence is distorted besides.
//!
//! The fix is JaCoCo's: delete the machine. `KotlinCoroutineFilter` recognises
//! the same shape and excises the dispatch, and doing so restores reducibility
//! as a side effect. After pruning the resume arms, the header dominates the
//! tail again and the source-level loop reappears.
//!
//! This is a rewrite of the graph rather than a special case in the analysis,
//! which keeps the language-neutral core free of any knowledge that coroutines
//! exist.

use salience_core::ir::{Node, NodeKind};

/// The intrinsic every coroutine state machine calls in its prologue. Its
/// presence is the signal that a method *is* one; JaCoCo triggers on the same
/// call.
const SUSPENDED_INTRINSIC: &str = "IntrinsicsKt::getCOROUTINE_SUSPENDED";

/// What excision did, for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Excision {
    /// Whether this method was recognised as a coroutine state machine.
    pub is_state_machine: bool,
    /// Resume and default arms removed from the dispatch.
    pub arms_pruned: usize,
    /// Instructions left unreachable by the pruning — the resume-restore
    /// blocks and the `IllegalStateException` arm. These are machinery with no
    /// source meaning, so their line attribution is cleared.
    pub machinery_unlined: usize,
}

/// Rewrites a coroutine state machine's graph into its reducible source shape.
///
/// Returns what was done. A method that is not a state machine is left exactly
/// as it was, and this is a cheap scan away from proving that.
pub fn excise_state_machine(nodes: &mut [Node]) -> Excision {
    let mut out = Excision::default();
    if !is_state_machine(nodes) {
        return out;
    }
    out.is_state_machine = true;

    // The dispatch is the first multi-way branch in the method. Everything
    // before it is prologue: reading `label`, allocating or casting the
    // continuation, and fetching the suspended sentinel. User code cannot
    // produce a `switch` there because user code has not started.
    let Some(dispatch) = nodes
        .iter()
        .position(|n| matches!(n.kind, NodeKind::Branch) && n.succs.len() > 2)
    else {
        return out;
    };

    // Arm 0 — the normal entry — always targets the lowest offset: the resume
    // points sit further into the body and the `default` throw is emitted last.
    // Keeping the minimum successor keeps the path a first invocation takes.
    let Some(&normal_entry) = nodes[dispatch].succs.iter().min() else {
        return out;
    };
    out.arms_pruned = nodes[dispatch].succs.len() - 1;
    nodes[dispatch].succs = vec![normal_entry];

    // Resume-restore blocks and the `IllegalStateException` arm are now
    // unreachable. They are reached only by the machine, never by a first
    // invocation, and they correspond to no statement the author wrote — so
    // they must not project onto lines. They stay in the graph, because
    // deleting nodes would renumber every successor index for no gain.
    for id in unreachable_from_entry(nodes) {
        if nodes[id].line.take().is_some() {
            out.machinery_unlined += 1;
        }
    }
    out
}

/// Whether this method body is a coroutine state machine.
fn is_state_machine(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match &n.kind {
        NodeKind::Call { callee, .. } => callee.contains(SUSPENDED_INTRINSIC),
        _ => false,
    })
}

/// Node indices not reachable from node 0.
fn unreachable_from_entry(nodes: &[Node]) -> Vec<usize> {
    let mut seen = vec![false; nodes.len()];
    if nodes.is_empty() {
        return Vec::new();
    }
    let mut stack = vec![0usize];
    seen[0] = true;
    while let Some(n) = stack.pop() {
        for &s in &nodes[n].succs {
            if !seen[s] {
                seen[s] = true;
                stack.push(s);
            }
        }
    }
    (0..nodes.len()).filter(|&i| !seen[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use salience_core::ir::CallOpacity;

    fn call(callee: &str) -> NodeKind {
        NodeKind::Call {
            callee: callee.to_owned(),
            opacity: CallOpacity::Opaque,
        }
    }

    /// A miniature of the measured shape: a dispatch whose arms enter a loop
    /// body directly, defeating dominance.
    fn state_machine() -> Vec<Node> {
        vec![
            // 0: prologue, calls the intrinsic
            Node::pure(1)
                .with_kind(call(
                    "kotlin.coroutines.intrinsics.IntrinsicsKt::getCOROUTINE_SUSPENDED",
                ))
                .with_succs([1]),
            // 1: the dispatch — arm 0 to the header, arm 1 into the body, default to a throw
            Node::pure(2)
                .with_kind(NodeKind::Branch)
                .with_succs([2, 3, 5]),
            // 2: loop header
            Node::pure(3).with_kind(NodeKind::Branch).with_succs([3, 6]),
            // 3: body
            Node::pure(4).with_succs([4]),
            // 4: back edge
            Node::pure(5).with_succs([2]),
            // 5: the IllegalStateException arm
            Node::pure(6).with_kind(NodeKind::Throw),
            // 6: exit
            Node::pure(7).with_kind(NodeKind::Return),
        ]
    }

    #[test]
    fn recognises_and_prunes_the_dispatch() {
        let mut nodes = state_machine();
        let out = excise_state_machine(&mut nodes);
        assert!(out.is_state_machine);
        assert_eq!(out.arms_pruned, 2, "resume arm and default arm");
        assert_eq!(nodes[1].succs, vec![2], "only the normal entry survives");
    }

    /// The point of the whole module: the loop must be visible afterwards.
    #[test]
    fn pruning_restores_the_natural_loop() {
        let mut nodes = state_machine();

        let before = salience_core::Graph::build(&salience_core::ir::FunctionIr {
            id: salience_core::ir::FunctionId {
                file: "T.kt".into(),
                name: "t".into(),
                signature: String::new(),
                decl_line: Some(1),
            },
            nodes: nodes.clone(),
            entry: 0,
        });
        assert_eq!(
            before.loops.len(),
            0,
            "the dispatch should defeat dominance before excision"
        );

        excise_state_machine(&mut nodes);
        let after = salience_core::Graph::build(&salience_core::ir::FunctionIr {
            id: salience_core::ir::FunctionId {
                file: "T.kt".into(),
                name: "t".into(),
                signature: String::new(),
                decl_line: Some(1),
            },
            nodes,
            entry: 0,
        });
        assert_eq!(after.loops.len(), 1, "the source loop must reappear");
        assert_eq!(after.loops[0].header, 2);
    }

    /// The throw arm becomes unreachable machinery and must stop claiming a line.
    #[test]
    fn unreachable_machinery_loses_its_line() {
        let mut nodes = state_machine();
        let out = excise_state_machine(&mut nodes);
        assert_eq!(out.machinery_unlined, 1);
        assert_eq!(nodes[5].line, None);
    }

    /// An ordinary method is untouched, including one with a real `switch`.
    #[test]
    fn leaves_non_coroutine_methods_alone() {
        let mut nodes = vec![
            Node::pure(1)
                .with_kind(NodeKind::Branch)
                .with_succs([1, 2, 3]),
            Node::pure(2).with_succs([3]),
            Node::pure(3).with_succs([3]),
            Node::pure(4).with_kind(NodeKind::Return),
        ];
        let before = nodes.clone();
        let out = excise_state_machine(&mut nodes);
        assert!(!out.is_state_machine);
        assert_eq!(
            nodes[0].succs, before[0].succs,
            "a real `when` must survive"
        );
    }
}
