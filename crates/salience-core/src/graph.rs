//! Graph analyses over [`FunctionIr`]: dominance, post-dominance, control
//! dependence, natural loops, and reaching definitions.
//!
//! Everything here is textbook and deterministic. No heuristics, no thresholds,
//! no inference — the only tunable in the whole crate lives in
//! [`crate::salience`], and it is a scoring weight, not a classification rule.
//!
//! Iteration order is fixed everywhere it could be observed: sets are
//! `BTreeSet`, maps are `BTreeMap`, and worklists drain in index order. Two runs
//! over the same bytes produce byte-identical output.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ir::{FunctionIr, NodeId, VarId};

/// Post-dominance plus both directions of the control-dependence relation, as
/// returned by [`post_dominance_and_control_dependence`].
type ControlDependence = (
    Vec<Option<NodeId>>,
    Vec<BTreeSet<NodeId>>,
    Vec<BTreeSet<NodeId>>,
);

/// Index into [`Graph::defs`] identifying one definition site.
pub type DefId = usize;

/// A definition site: a node and the variable it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DefSite {
    /// Node performing the definition.
    pub node: NodeId,
    /// Variable being defined.
    pub var: VarId,
}

/// A natural loop discovered from a back edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    /// The loop header — the target of the back edge, which dominates the body.
    pub header: NodeId,
    /// Every node in the loop body, including the header.
    pub body: BTreeSet<NodeId>,
}

/// All derived graph facts for one function.
///
/// Built once by [`Graph::build`] and then queried by the tiering pass. Nothing
/// here depends on the source language.
#[derive(Debug, Clone)]
pub struct Graph {
    /// Number of nodes, cached.
    pub n: usize,
    /// Reverse post-order of the reachable subgraph from entry.
    pub rpo: Vec<NodeId>,
    /// Predecessors, transposed from the IR's successor lists.
    pub preds: Vec<Vec<NodeId>>,
    /// Immediate dominator per node. `None` for the entry and for unreachable
    /// nodes.
    pub idom: Vec<Option<NodeId>>,
    /// Immediate post-dominator per node, computed over the reversed graph
    /// against a virtual exit. `None` when the node post-dominates nothing
    /// reachable — in practice only the virtual exit itself.
    pub ipdom: Vec<Option<NodeId>>,
    /// For each node, the set of branch nodes it is control-dependent on.
    pub ctrl_deps: Vec<BTreeSet<NodeId>>,
    /// For each branch node, the set of nodes control-dependent on it. The
    /// transpose of [`Graph::ctrl_deps`], and the basis of the dominance weight.
    pub controls: Vec<BTreeSet<NodeId>>,
    /// Natural loops, ordered by header.
    pub loops: Vec<Loop>,
    /// Loop nesting depth per node.
    pub loop_depth: Vec<u32>,
    /// Every definition site in the function.
    pub defs: Vec<DefSite>,
    /// Definitions reaching the entry of each node.
    pub rd_in: Vec<BTreeSet<DefId>>,
    /// For each node, the definitions its uses read — the def-use chain,
    /// oriented use → def.
    pub uses_defs: Vec<BTreeSet<DefId>>,
    /// For each definition, the nodes that read it — oriented def → use.
    pub def_users: Vec<BTreeSet<NodeId>>,
    /// Definitions made at each node, indexed rather than searched.
    ///
    /// Callers used to scan all of [`Graph::defs`] per node to find this, which
    /// is `O(nodes x defs)` inside a fixpoint. On a 5,150-instruction static
    /// initialiser in Guava that cost 1.9 seconds for one function.
    pub defs_at: Vec<Vec<DefId>>,
    /// How much of the function transitively depends on each node, as a count
    /// of distinct dependent nodes.
    ///
    /// This is the forward dependence cone over data *and* control edges, and
    /// it is the one genuinely continuous quantity the analysis produces: it
    /// ranges over the whole width of the function rather than over a handful
    /// of buckets. A statement everything downstream reads is not the same
    /// kind of statement as one nothing reads, and no categorical tier can say
    /// so.
    pub influence: Vec<u32>,
    /// How much of the function each node transitively depends *on* — the
    /// backward cone.
    ///
    /// The mirror of [`Graph::influence`], and it earns its place by breaking
    /// ties the forward cone cannot. A call whose result nobody reads has zero
    /// influence no matter how much work went into its arguments; without this
    /// term every such statement scores identically, and a heatmap of identical
    /// values is a blank wall. Read the pair as "how much depends on this" and
    /// "how much this depends on".
    pub depends_on: Vec<u32>,
}

impl Graph {
    /// Runs every analysis over `ir`.
    ///
    /// Cost is dominated by the reaching-definitions fixpoint, which is
    /// `O(nodes x defs)` in the worst case and far below that in practice
    /// because function bodies are small and definitions are local.
    #[must_use]
    pub fn build(ir: &FunctionIr) -> Self {
        let n = ir.nodes.len();
        let preds = transpose(ir);
        let rpo = reverse_post_order(ir);
        let idom = dominators(ir, &rpo, &preds);
        let (post_idom, ctrl_deps, controls) = post_dominance_and_control_dependence(ir, &preds);
        let loops = natural_loops(ir, &idom, &preds);
        let loop_depth = loop_depths(n, &loops);
        let (defs, rd_in) = reaching_definitions(ir, &preds, &rpo);
        let (uses_defs, def_users) = def_use_chains(ir, &defs, &rd_in);

        let mut defs_at: Vec<Vec<DefId>> = vec![Vec::new(); n];
        for (id, site) in defs.iter().enumerate() {
            defs_at[site.node].push(id);
        }
        let succ = dependence_successors(n, &defs, &defs_at, &def_users, &ctrl_deps);
        let influence = cone_sizes(n, &succ);
        let mut pred: Vec<Vec<NodeId>> = vec![Vec::new(); n];
        for (from, tos) in succ.iter().enumerate() {
            for &to in tos {
                pred[to].push(from);
            }
        }
        let depends_on = cone_sizes(n, &pred);

        Self {
            n,
            rpo,
            preds,
            idom,
            ipdom: post_idom,
            ctrl_deps,
            controls,
            loops,
            loop_depth,
            defs,
            rd_in,
            uses_defs,
            def_users,
            defs_at,
            influence,
            depends_on,
        }
    }

    /// [`Graph::influence`] as a fraction of the function, in `0.0..=1.0`.
    #[must_use]
    pub fn influence_fraction(&self, node: NodeId) -> f64 {
        if self.n <= 1 {
            return 0.0;
        }
        f64::from(self.influence[node]) / (self.n - 1) as f64
    }

    /// Whether `a` dominates `b`, by walking the dominator tree upward.
    #[must_use]
    pub fn dominates(&self, a: NodeId, b: NodeId) -> bool {
        if a == b {
            return true;
        }
        let mut cur = b;
        while let Some(p) = self.idom[cur] {
            if p == a {
                return true;
            }
            if p == cur {
                break;
            }
            cur = p;
        }
        false
    }

    /// The fraction of the function this branch control-dominates, in `0.0..=1.0`.
    ///
    /// This is the "weighted by how much of the body it controls" quantity: a
    /// predicate guarding two lines of a fifty-line function scores low, one
    /// guarding forty scores high. Non-branch nodes score zero.
    #[must_use]
    pub fn control_weight(&self, node: NodeId) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        // Exclude the branch itself so a self-controlling edge cannot inflate
        // the weight of a predicate that guards nothing else.
        let governed = self.controls[node].iter().filter(|&&m| m != node).count();
        governed as f64 / self.n as f64
    }

    /// Whether this definition survives a loop iteration boundary.
    ///
    /// The criterion is that the definition sits inside a loop and also reaches
    /// that loop's header. A value redefined and consumed entirely within one
    /// iteration does not reach the header from inside; an accumulator does.
    /// That is precisely loop-carried dataflow, and it falls straight out of
    /// reaching definitions with no extra fixpoint.
    #[must_use]
    pub fn is_loop_carried(&self, def: DefId) -> bool {
        let site = self.defs[def];
        self.loops
            .iter()
            .any(|l| l.body.contains(&site.node) && self.rd_in[l.header].contains(&def))
    }
}

/// Forward dependence edges: who consumes what this node produces, and who is
/// controlled by it.
/// `pub(crate)` rather than private: the `pivot` scorer needs the same edge
/// set the incumbent's cones are built from; sharing this function keeps the
/// two scorers honestly comparable.
pub(crate) fn dependence_successors(
    n: usize,
    defs: &[DefSite],
    defs_at: &[Vec<DefId>],
    def_users: &[BTreeSet<NodeId>],
    ctrl_deps: &[BTreeSet<NodeId>],
) -> Vec<Vec<NodeId>> {
    let _ = defs;
    let mut out: Vec<Vec<NodeId>> = vec![Vec::new(); n];
    for node in 0..n {
        for &d in &defs_at[node] {
            out[node].extend(def_users[d].iter().copied());
        }
    }
    // A branch influences everything control-dependent on it.
    for (dependent, branches) in ctrl_deps.iter().enumerate() {
        for &b in branches {
            out[b].push(dependent);
        }
    }
    for v in &mut out {
        v.sort_unstable();
        v.dedup();
    }
    out
}

/// Size of each node's transitive cone in an arbitrary adjacency list.
///
/// Computed by condensing the dependence graph into strongly connected
/// components and unioning reachability sets in reverse topological order, so
/// each edge is relaxed once. The naive alternative — a per-node search, or a
/// union-to-fixpoint over the cyclic graph — is quadratic or worse, and
/// function bodies in real libraries reach 5,000 instructions.
fn cone_sizes(n: usize, succ: &[Vec<NodeId>]) -> Vec<u32> {
    if n == 0 {
        return Vec::new();
    }
    let (comp_of, comps) = strongly_connected(n, succ);
    let ncomp = comps.len();

    // Condensed edges, deduplicated.
    let mut cedges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); ncomp];
    for node in 0..n {
        for &s in &succ[node] {
            if comp_of[node] != comp_of[s] {
                cedges[comp_of[node]].insert(comp_of[s]);
            }
        }
    }

    // `strongly_connected` emits components in reverse topological order, so a
    // component's successors are always already complete when it is processed.
    let words = n.div_ceil(64);
    let mut reach: Vec<Vec<u64>> = vec![vec![0u64; words]; ncomp];
    for c in 0..ncomp {
        // Every member of the component reaches every other member.
        for &m in &comps[c] {
            reach[c][m / 64] |= 1u64 << (m % 64);
        }
        let succ_comps: Vec<usize> = cedges[c].iter().copied().collect();
        for sc in succ_comps {
            let (lo, hi) = if sc < c {
                let (a, b) = reach.split_at_mut(c);
                (&mut b[0], &a[sc])
            } else {
                let (a, b) = reach.split_at_mut(sc);
                (&mut a[c], &b[0])
            };
            for w in 0..words {
                lo[w] |= hi[w];
            }
        }
    }

    (0..n)
        .map(|node| {
            let c = comp_of[node];
            let mut count: u32 = reach[c].iter().map(|w| w.count_ones()).sum();
            // A node does not depend on itself.
            if reach[c][node / 64] & (1u64 << (node % 64)) != 0 {
                count -= 1;
            }
            count
        })
        .collect()
}

/// Iterative Tarjan. Returns each node's component id and the members of each
/// component, with components emitted in reverse topological order.
///
/// `pub(crate)` rather than private: the schur, trophic and strahler scorers
/// reuse it verbatim over their own (differently-indexed) dependence graphs
/// rather than re-deriving Tarjan's algorithm three more times.
pub(crate) fn strongly_connected(n: usize, succ: &[Vec<NodeId>]) -> (Vec<usize>, Vec<Vec<NodeId>>) {
    const UNVISITED: usize = usize::MAX;
    let mut index = vec![UNVISITED; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<NodeId> = Vec::new();
    let mut comp_of = vec![UNVISITED; n];
    let mut comps: Vec<Vec<NodeId>> = Vec::new();
    let mut next_index = 0usize;

    // (node, next successor to visit)
    let mut call: Vec<(NodeId, usize)> = Vec::new();
    for root in 0..n {
        if index[root] != UNVISITED {
            continue;
        }
        call.push((root, 0));
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;

        while let Some(&mut (v, ref mut i)) = call.last_mut() {
            if let Some(&w) = succ[v].get(*i) {
                *i += 1;
                if index[w] == UNVISITED {
                    index[w] = next_index;
                    low[w] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some(&mut (parent, _)) = call.last_mut() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut members = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp_of[w] = comps.len();
                        members.push(w);
                        if w == v {
                            break;
                        }
                    }
                    comps.push(members);
                }
            }
        }
    }
    (comp_of, comps)
}

/// Builds the predecessor lists.
fn transpose(ir: &FunctionIr) -> Vec<Vec<NodeId>> {
    let mut preds = vec![Vec::new(); ir.nodes.len()];
    for (id, node) in ir.nodes.iter().enumerate() {
        for &s in &node.succs {
            preds[s].push(id);
        }
    }
    for p in &mut preds {
        p.sort_unstable();
        p.dedup();
    }
    preds
}

/// Reverse post-order over nodes reachable from the entry.
///
/// Iterative rather than recursive: deeply nested generated code (Kotlin
/// coroutine state machines in particular) produces long chains, and blowing
/// the stack on a pathological input is not an acceptable failure mode for a
/// library that runs inside an editor hook.
fn reverse_post_order(ir: &FunctionIr) -> Vec<NodeId> {
    let n = ir.nodes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut seen = vec![false; n];
    let mut post = Vec::with_capacity(n);
    // (node, index of next successor to visit)
    let mut stack: Vec<(NodeId, usize)> = vec![(ir.entry, 0)];
    seen[ir.entry] = true;

    while let Some(&mut (node, ref mut next)) = stack.last_mut() {
        if let Some(&s) = ir.nodes[node].succs.get(*next) {
            *next += 1;
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(node);
            stack.pop();
        }
    }
    post.reverse();
    post
}

/// Cooper-Harvey-Kennedy iterative dominators.
///
/// Chosen over Lengauer-Tarjan deliberately: at function scale the iterative
/// algorithm is faster in practice, and it is short enough to audit.
fn dominators(ir: &FunctionIr, rpo: &[NodeId], preds: &[Vec<NodeId>]) -> Vec<Option<NodeId>> {
    let n = ir.nodes.len();
    let mut idom: Vec<Option<NodeId>> = vec![None; n];
    if n == 0 {
        return idom;
    }
    // Position in RPO, for the intersection walk.
    let mut order = vec![usize::MAX; n];
    for (i, &node) in rpo.iter().enumerate() {
        order[node] = i;
    }
    idom[ir.entry] = Some(ir.entry);

    let intersect = |mut a: NodeId, mut b: NodeId, idom: &[Option<NodeId>]| -> NodeId {
        while a != b {
            while order[a] > order[b] {
                a = idom[a].expect("processed node always has an idom");
            }
            while order[b] > order[a] {
                b = idom[b].expect("processed node always has an idom");
            }
        }
        a
    };

    let mut changed = true;
    while changed {
        changed = false;
        for &node in rpo {
            if node == ir.entry {
                continue;
            }
            let mut new_idom: Option<NodeId> = None;
            for &p in &preds[node] {
                if idom[p].is_none() {
                    continue;
                }
                new_idom = Some(match new_idom {
                    None => p,
                    Some(cur) => intersect(p, cur, &idom),
                });
            }
            if let Some(new) = new_idom
                && idom[node] != Some(new)
            {
                idom[node] = Some(new);
                changed = true;
            }
        }
    }
    // The entry dominates itself, but reporting that as an immediate dominator
    // makes upward walks non-terminating for callers.
    idom[ir.entry] = None;
    idom
}

/// Post-dominance and, from it, control dependence.
///
/// A virtual exit node is appended so that functions with several returns — or
/// with none, because every path throws or spins — still have a unique sink.
/// Without it, post-dominance is undefined for exactly the functions whose
/// control flow is most worth tiering.
#[allow(clippy::too_many_lines)]
fn post_dominance_and_control_dependence(
    ir: &FunctionIr,
    preds: &[Vec<NodeId>],
) -> ControlDependence {
    let n = ir.nodes.len();
    if n == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let exit = n; // virtual
    let size = n + 1;

    // Reversed graph: successors of the reverse graph are the original preds.
    // Terminal nodes (no successors) gain an edge to the virtual exit.
    let mut rsuccs: Vec<Vec<NodeId>> = vec![Vec::new(); size];
    for (id, node) in ir.nodes.iter().enumerate() {
        if node.succs.is_empty() {
            rsuccs[exit].push(id);
        }
        for &s in &node.succs {
            rsuccs[s].push(id);
        }
    }
    // Nodes trapped in a loop with no path to any terminal would be unreachable
    // in the reverse graph, leaving their post-dominance undefined. Anchoring
    // the exit to them as well keeps the relation total.
    let reach_from_exit = reachable(&rsuccs, exit, size);
    for id in 0..n {
        if !reach_from_exit[id] && ir.nodes[id].succs.iter().all(|&s| !reach_from_exit[s]) {
            rsuccs[exit].push(id);
        }
    }
    for r in &mut rsuccs {
        r.sort_unstable();
        r.dedup();
    }

    let rpo_r = rpo_of(&rsuccs, exit, size);
    let mut order = vec![usize::MAX; size];
    for (i, &node) in rpo_r.iter().enumerate() {
        order[node] = i;
    }
    let mut rpreds: Vec<Vec<NodeId>> = vec![Vec::new(); size];
    for (id, ss) in rsuccs.iter().enumerate() {
        for &s in ss {
            rpreds[s].push(id);
        }
    }

    let mut ipdom: Vec<Option<NodeId>> = vec![None; size];
    ipdom[exit] = Some(exit);
    let intersect = |mut a: NodeId, mut b: NodeId, ipdom: &[Option<NodeId>]| -> NodeId {
        while a != b {
            while order[a] != usize::MAX && order[b] != usize::MAX && order[a] > order[b] {
                a = ipdom[a].expect("processed node always has an ipdom");
            }
            while order[a] != usize::MAX && order[b] != usize::MAX && order[b] > order[a] {
                b = ipdom[b].expect("processed node always has an ipdom");
            }
            if order[a] == usize::MAX || order[b] == usize::MAX {
                return exit;
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &node in &rpo_r {
            if node == exit {
                continue;
            }
            let mut new: Option<NodeId> = None;
            for &p in &rpreds[node] {
                if ipdom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(cur) => intersect(p, cur, &ipdom),
                });
            }
            if let Some(new) = new
                && ipdom[node] != Some(new)
            {
                ipdom[node] = Some(new);
                changed = true;
            }
        }
    }

    // Ferrante-Ottenstein-Warren: for every edge a -> b where b does not
    // post-dominate a, every node on the post-dominator path from b up to
    // (but excluding) ipdom(a) is control-dependent on a.
    let mut ctrl_deps: Vec<BTreeSet<NodeId>> = vec![BTreeSet::new(); n];
    let mut controls: Vec<BTreeSet<NodeId>> = vec![BTreeSet::new(); n];
    for a in 0..n {
        if ir.nodes[a].succs.len() < 2 {
            continue; // only real branches induce control dependence
        }
        let stop = ipdom[a];
        for &b in &ir.nodes[a].succs {
            let mut cur = Some(b);
            while let Some(c) = cur {
                if c == exit || Some(c) == stop {
                    break;
                }
                if c < n {
                    ctrl_deps[c].insert(a);
                    controls[a].insert(c);
                }
                let next = ipdom[c];
                if next == Some(c) {
                    break;
                }
                cur = next;
            }
        }
    }
    let _ = preds;

    ipdom.truncate(n);
    (ipdom, ctrl_deps, controls)
}

/// Nodes reachable from `start` in an adjacency list.
fn reachable(succs: &[Vec<NodeId>], start: NodeId, size: usize) -> Vec<bool> {
    let mut seen = vec![false; size];
    let mut q = VecDeque::from([start]);
    seen[start] = true;
    while let Some(x) = q.pop_front() {
        for &s in &succs[x] {
            if !seen[s] {
                seen[s] = true;
                q.push_back(s);
            }
        }
    }
    seen
}

/// Reverse post-order over an arbitrary adjacency list.
fn rpo_of(succs: &[Vec<NodeId>], start: NodeId, size: usize) -> Vec<NodeId> {
    let mut seen = vec![false; size];
    let mut post = Vec::with_capacity(size);
    let mut stack: Vec<(NodeId, usize)> = vec![(start, 0)];
    seen[start] = true;
    while let Some(&mut (node, ref mut next)) = stack.last_mut() {
        if let Some(&s) = succs[node].get(*next) {
            *next += 1;
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(node);
            stack.pop();
        }
    }
    post.reverse();
    post
}

/// Natural loops, one per back edge, merged by header.
fn natural_loops(ir: &FunctionIr, idom: &[Option<NodeId>], preds: &[Vec<NodeId>]) -> Vec<Loop> {
    let n = ir.nodes.len();
    let dominates = |a: NodeId, b: NodeId| -> bool {
        if a == b {
            return true;
        }
        let mut cur = b;
        while let Some(p) = idom[cur] {
            if p == a {
                return true;
            }
            if p == cur {
                break;
            }
            cur = p;
        }
        false
    };

    let mut by_header: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for tail in 0..n {
        for &header in &ir.nodes[tail].succs {
            if !dominates(header, tail) {
                continue;
            }
            // Back edge tail -> header. Body is the header plus everything that
            // reaches the tail without passing through the header.
            let body = by_header.entry(header).or_default();
            body.insert(header);
            let mut stack = vec![tail];
            while let Some(x) = stack.pop() {
                if x == header || !body.insert(x) {
                    continue;
                }
                for &p in &preds[x] {
                    stack.push(p);
                }
            }
        }
    }
    by_header
        .into_iter()
        .map(|(header, body)| Loop { header, body })
        .collect()
}

/// Nesting depth per node: the number of loops whose body contains it.
fn loop_depths(n: usize, loops: &[Loop]) -> Vec<u32> {
    let mut depth = vec![0u32; n];
    for l in loops {
        for &node in &l.body {
            depth[node] += 1;
        }
    }
    depth
}

/// Classic forward reaching-definitions fixpoint.
///
/// Computing this in the core rather than requiring SSA from frontends is what
/// makes a CPython frontend over mutable `STORE_FAST` locals exactly as sound
/// as a frontend over an already-SSA IR.
fn reaching_definitions(
    ir: &FunctionIr,
    preds: &[Vec<NodeId>],
    rpo: &[NodeId],
) -> (Vec<DefSite>, Vec<BTreeSet<DefId>>) {
    let n = ir.nodes.len();
    let mut defs: Vec<DefSite> = Vec::new();
    let mut defs_by_node: Vec<Vec<DefId>> = vec![Vec::new(); n];
    let mut defs_by_var: BTreeMap<VarId, Vec<DefId>> = BTreeMap::new();
    for (node, ir_node) in ir.nodes.iter().enumerate() {
        for &var in &ir_node.defs {
            let id = defs.len();
            defs.push(DefSite { node, var });
            defs_by_node[node].push(id);
            defs_by_var.entry(var).or_default().push(id);
        }
    }

    let mut rd_in: Vec<BTreeSet<DefId>> = vec![BTreeSet::new(); n];
    let mut rd_out: Vec<BTreeSet<DefId>> = vec![BTreeSet::new(); n];

    let mut changed = true;
    while changed {
        changed = false;
        for &node in rpo {
            let mut in_set: BTreeSet<DefId> = BTreeSet::new();
            for &p in &preds[node] {
                in_set.extend(rd_out[p].iter().copied());
            }
            // OUT = GEN u (IN - KILL), where KILL is every other definition of
            // any variable this node writes.
            let mut out = in_set.clone();
            for &d in &defs_by_node[node] {
                let var = defs[d].var;
                if let Some(siblings) = defs_by_var.get(&var) {
                    for &s in siblings {
                        out.remove(&s);
                    }
                }
            }
            for &d in &defs_by_node[node] {
                out.insert(d);
            }
            if rd_in[node] != in_set {
                rd_in[node] = in_set;
                changed = true;
            }
            if rd_out[node] != out {
                rd_out[node] = out;
                changed = true;
            }
        }
    }
    (defs, rd_in)
}

/// Def-use chains in both directions, derived from reaching definitions.
fn def_use_chains(
    ir: &FunctionIr,
    defs: &[DefSite],
    rd_in: &[BTreeSet<DefId>],
) -> (Vec<BTreeSet<DefId>>, Vec<BTreeSet<NodeId>>) {
    let n = ir.nodes.len();
    let mut uses_defs: Vec<BTreeSet<DefId>> = vec![BTreeSet::new(); n];
    let mut def_users: Vec<BTreeSet<NodeId>> = vec![BTreeSet::new(); defs.len()];
    for (node, ir_node) in ir.nodes.iter().enumerate() {
        for &var in &ir_node.uses {
            for &d in &rd_in[node] {
                if defs[d].var == var {
                    uses_defs[node].insert(d);
                    def_users[d].insert(node);
                }
            }
        }
    }
    (uses_defs, def_users)
}
