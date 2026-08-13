//! Tier assignment and scoring.
//!
//! Two outputs, for two different consumers:
//!
//! - A **tier** — `core` / `boundary` / `plumbing` / `inert` — which is a
//!   classification, meant for policy. An agent harness gating edits wants a
//!   predicate, not a number.
//! - A **score** in `0.0..=1.0`, which is a ranking. A weighted call graph, a
//!   profiler picking where to start, or a vulnerability triage queue wants an
//!   ordering, and collapsing that to four buckets throws away the gradient
//!   between a predicate guarding two lines and one guarding forty.
//!
//! Both are derived from the same graph facts, and neither involves a model.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::graph::Graph;
use crate::ir::{FunctionIr, NodeId, NodeKind};

/// How much behavior a statement carries.
///
/// Ordered by salience so that projecting several instructions onto one source
/// line is a `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Explicitly known to carry no behavior: a denylisted call, or computation
    /// that exists only to feed one.
    Inert,
    /// Present, but not behavior-carrying: local shuffling, temporaries, and
    /// computation whose results never reach an effect.
    Plumbing,
    /// The frontier where behavior becomes observable outside the body —
    /// returns, throws, state writes, and calls into opaque dependencies.
    Boundary,
    /// Behavior-carrying: branch predicates, loop-carried dataflow, and
    /// anything on a def-use chain that reaches an effect.
    Core,
}

impl Tier {
    /// Stable lowercase name used in the artifact.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Boundary => "boundary",
            Self::Plumbing => "plumbing",
            Self::Inert => "inert",
        }
    }
}

/// Why a node landed in its tier.
///
/// Reasons are accumulated rather than replaced: a statement can be both a
/// predicate and loop-carried, and a consumer deciding whether to demand
/// verification benefits from seeing both.
#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    /// A conditional whose outcome governs `weight` of the function body.
    BranchPredicate {
        /// Fraction of the body control-dependent on this branch, `0.0..=1.0`.
        weight: f64,
        /// Number of instructions governed.
        governed: usize,
    },
    /// Defines a value that survives a loop iteration boundary.
    LoopCarried {
        /// Nesting depth of the innermost containing loop.
        depth: u32,
    },
    /// A large share of the function transitively depends on this statement.
    HighInfluence {
        /// Fraction of the body that depends on it, `0.0..=1.0`.
        fraction: f64,
        /// Number of dependent instructions.
        dependents: u32,
    },
    /// On a def-use or control chain reaching an effect. Informational: on real
    /// code almost every statement satisfies this, so it cannot carry a tier.
    ReachesEffect {
        /// Number of dependence steps to the nearest effect.
        distance: u32,
        /// What that effect is.
        effect: String,
    },
    /// Is itself an effect site.
    EffectSite {
        /// The kind of effect: `return`, `throw`, `state-write`, `call`.
        kind: &'static str,
        /// Target or callee, when the frontend supplied one.
        target: String,
    },
    /// Matched the denylist.
    Denylisted {
        /// The callee that matched.
        callee: String,
    },
    /// Every use of this value dead-ends in a denylisted call.
    InertSupport,
    /// Computation whose results reach no effect.
    Unreaching,
}

impl Reason {
    /// The tier this reason on its own implies.
    #[must_use]
    pub fn tier(&self) -> Tier {
        match self {
            Self::BranchPredicate { .. }
            | Self::LoopCarried { .. }
            | Self::HighInfluence { .. } => Tier::Core,
            Self::EffectSite { .. } => Tier::Boundary,
            Self::Denylisted { .. } | Self::InertSupport => Tier::Inert,
            // `ReachesEffect` is deliberately *not* Core. Measured over ~28,000
            // library functions it holds for 86-100% of statements, because real
            // code is dense with calls. A predicate that is almost always true
            // cannot carry a tier; it survives only as an explanation.
            Self::ReachesEffect { .. } | Self::Unreaching => Tier::Plumbing,
        }
    }

    /// One-line human phrasing, used verbatim in the artifact.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::BranchPredicate { weight, governed } => format!(
                "branch predicate governing {governed} instructions ({:.0}% of body)",
                weight * 100.0
            ),
            Self::LoopCarried { depth } => {
                format!("loop-carried definition at nesting depth {depth}")
            }
            Self::HighInfluence {
                fraction,
                dependents,
            } => format!(
                "{dependents} instructions depend on this ({:.0}% of the body)",
                fraction * 100.0
            ),
            Self::ReachesEffect { distance, effect } => {
                format!("reaches {effect} in {distance} dependence step(s)")
            }
            Self::EffectSite { kind, target } if target.is_empty() => (*kind).to_owned(),
            Self::EffectSite { kind, target } => format!("{kind} -> {target}"),
            Self::Denylisted { callee } => format!("denylisted call {callee}"),
            Self::InertSupport => "every use of this value ends in a denylisted call".to_owned(),
            Self::Unreaching => "result reaches no effect".to_owned(),
        }
    }
}

/// Calls known to carry no behavior.
///
/// Matching is literal substring containment, deliberately. A regex engine
/// would invite patterns whose cost is input-dependent, and the whole point of
/// the artifact is a bounded, predictable analysis budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denylist {
    patterns: Vec<String>,
}

impl Denylist {
    /// Logging, metrics and tracing across the substrates the prototype
    /// supports. Intentionally short: every entry is a place where a false
    /// positive silently demotes real behavior, so the list earns its way in.
    pub const DEFAULT: &'static [&'static str] = &[
        // JVM logging
        "org.slf4j.Logger",
        "ch.qos.logback",
        "org.apache.logging.log4j",
        "java.util.logging.Logger",
        "java.io.PrintStream::println",
        "java.io.PrintStream::print",
        // JVM metrics and tracing
        "io.micrometer",
        "io.opentelemetry",
        "io.opentracing",
        "com.codahale.metrics",
        // Python
        "logging.debug",
        "logging.info",
        "logging.warning",
        "logging.error",
        "logging.exception",
        "logging.critical",
        "logging.log",
        "logger.debug",
        "logger.info",
        "logger.warning",
        "logger.error",
        "logger.exception",
        "logger.critical",
        "log.debug",
        "log.info",
        "log.warning",
        "log.error",
        "print",
    ];

    /// The default denylist.
    #[must_use]
    pub fn new() -> Self {
        Self {
            patterns: Self::DEFAULT.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// An empty denylist — nothing is treated as inert.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Adds a pattern.
    #[must_use]
    pub fn with(mut self, pattern: impl Into<String>) -> Self {
        self.patterns.push(pattern.into());
        self
    }

    /// Whether `callee` matches any pattern.
    ///
    /// Matching is case-insensitive and aligned to name-segment boundaries.
    /// Both details are load-bearing rather than conveniences:
    ///
    /// - Logger handles are conventionally spelled `LOG`, `Log` or `logger`
    ///   depending on the codebase, and a case-sensitive list would silently
    ///   match one project's logging and miss the next one's.
    /// - A bare pattern with no separator, such as `print`, matches only a
    ///   callee that *is* that name. Plain substring containment would quietly
    ///   classify `pprint`, `footprint` and `sprint` as inert.
    /// - A qualified pattern must begin at a segment boundary, so `log.info`
    ///   matches `LOG.info` but not `mylog.info`.
    #[must_use]
    pub fn matches(&self, callee: &str) -> bool {
        let callee = callee.to_ascii_lowercase();
        self.patterns.iter().any(|pattern| {
            let pattern = pattern.to_ascii_lowercase();
            let qualified = pattern.contains('.') || pattern.contains(':');
            if !qualified {
                return callee == pattern;
            }
            match callee.find(&pattern) {
                Some(0) => true,
                Some(i) => matches!(callee.as_bytes()[i - 1], b'.' | b':'),
                None => false,
            }
        })
    }
}

impl Default for Denylist {
    fn default() -> Self {
        Self::new()
    }
}

/// Weights composing the continuous score.
///
/// These are the only tunable numbers in the crate. They affect ranking only —
/// tier assignment is categorical and ignores them entirely — so a consumer
/// that dislikes the defaults changes an ordering, never a policy decision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreWeights {
    /// Weight on the share of the function that transitively depends on a
    /// statement. This is the term that gives the score its resolution: it is
    /// continuous over the width of the body, where every other term takes a
    /// handful of values.
    pub influence: f64,
    /// Weight on proximity to the nearest effect.
    pub proximity: f64,
    /// Weight on a branch's control-dominance fraction.
    pub control: f64,
    /// Weight on normalized loop nesting depth.
    pub loop_depth: f64,
    /// Weight on the share of the function a statement transitively depends on.
    pub dependency: f64,
    /// Weight on being an effect rather than merely leading to one.
    ///
    /// Without this the two cone terms systematically bury the point of the
    /// function: a `return` has an empty forward cone because nothing depends
    /// on it, so it scored *below* the setup lines feeding it. Measured on
    /// `shutil._make_zipfile`, `return zip_filename` ranked in the bottom
    /// quartile while `zip_filename = base_name + ".zip"` ranked top. Being the
    /// place behavior becomes observable is its own kind of importance.
    pub effect: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            influence: 0.30,
            proximity: 0.05,
            control: 0.15,
            loop_depth: 0.10,
            dependency: 0.15,
            effect: 0.25,
        }
    }
}

/// Share of the body that must depend on a statement for it to count as
/// behavior-carrying on influence alone.
///
/// Calibrated against ~28,000 JVM library functions and the Python standard
/// library rather than chosen a priori: the previous rule — "is on a def-use
/// chain reaching an effect" — was true of 86-100% of statements and produced a
/// constant map.
pub const CORE_INFLUENCE: f64 = 0.10;

/// Per-node analysis result.
#[derive(Debug, Clone)]
pub struct NodeSalience {
    /// The node this describes.
    pub node: NodeId,
    /// Assigned tier.
    pub tier: Tier,
    /// Continuous salience, `0.0..=1.0`.
    pub score: f64,
    /// Every reason that applied, ordered by descending tier.
    pub reasons: Vec<Reason>,
}

/// The analysis of one function, before line projection.
#[derive(Debug, Clone)]
pub struct FunctionSalience {
    /// Per-node results, indexed by [`NodeId`].
    pub nodes: Vec<NodeSalience>,
    /// The derived graph, retained so callers can inspect loops and dominance
    /// without recomputing.
    pub graph: Graph,
}

/// Runs tiering and scoring over a lowered function.
///
/// # Panics
///
/// Does not panic on any graph shape. Callers should still run
/// [`FunctionIr::validate`] first, which reports frontend bugs with a useful
/// message instead of letting them surface as silently wrong tiers.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn analyze(ir: &FunctionIr, denylist: &Denylist, weights: &ScoreWeights) -> FunctionSalience {
    let graph = Graph::build(ir);
    let n = ir.nodes.len();

    // --- inert set -------------------------------------------------------
    // A statement is inert when *every* chain of uses leading out of it
    // dead-ends in a denylisted call.
    //
    // The previous rule was "feeds a denylisted call and reaches no hard
    // effect", and measurement killed it: 56% of the lines it marked inert on
    // the Python standard library had nothing to do with logging. The failure
    // is instructive. In
    //
    //     line = response.fp.readline(...)     <- the network read
    //     ...
    //     if self.debuglevel > 0: print('header:', line.decode())
    //
    // `line` feeds a denylisted `print`, and its other consumers — `len`, `in`,
    // comparisons — are opaque calls rather than returns or writes, so "reaches
    // no hard effect" held and the read scored 0.00. One debug print poisoned
    // the whole variable.
    //
    // The fix is to ask the opposite question. Compute the set of *useful*
    // statements as backward reachability from useful sinks — every anchor
    // (return, throw, state write) and every call that is not denylisted — and
    // call a statement inert only if it is not useful. Now `len(line)` is a
    // useful sink, so `line` survives; a counter incremented solely to be
    // logged still has no useful sink anywhere downstream and is still inert.
    //
    // Backward reachability rather than a fixpoint over "all consumers are
    // inert", because the valuable case is a dependency cycle: a counter reads
    // its own previous value, and a least-fixpoint never enters that loop.
    let denylisted: Vec<bool> = (0..n)
        .map(|i| match &ir.nodes[i].kind {
            NodeKind::Call { callee, .. } => denylist.matches(callee),
            _ => ir.nodes[i].kind.is_inert(),
        })
        .collect();

    // The closure must follow control dependence as well as data. A branch
    // defines nothing, so a data-only closure can never reach one — and marking
    // every predicate in the program inert is exactly as wrong as it sounds
    // (measured: 58% of lines in large functions).
    // Seeds are the statements that are useful *in themselves*: hard effects,
    // and calls made purely for their side effect — those whose result nobody
    // reads. Everything else earns usefulness by feeding one.
    //
    // "Calls made for their side effect" rather than "all calls" is the
    // distinction that took two attempts to get right. Treating every
    // non-denylisted call as a useful sink looks reasonable until you notice
    // that string concatenation is a call: `"inspected " + inspected` is not
    // denylisted, so a counter incremented solely to be logged became useful
    // through the concatenation built for the log message. Asking instead
    // whether anyone reads the result separates `zf.write(path, arcname)`,
    // whose result is discarded because the write *is* the point, from
    // `makeConcatWithConstants`, whose result is read by a logging call and
    // nothing else.
    let useful = useful_closure(
        &graph,
        (0..n).filter(|&i| {
            let k = &ir.nodes[i].kind;
            is_hard_effect(k)
                || (matches!(k, NodeKind::Call { .. })
                    && !denylisted[i]
                    && result_discarded(ir, &graph, i))
        }),
    );
    // Not-useful alone is *not* inert. `pass` in an `except:` block is useless
    // too, and calling it inert says "this is logging", which is false and
    // measurably common: keying inert on usefulness alone put `pass`,
    // `blocksize = 2 ** 27` and bare `errno.EINVAL)` in the tier, taking the
    // false-positive rate from 56% to 90%.
    //
    // Inert means specifically "exists to feed logging". That needs both halves:
    // no useful sink downstream, *and* a denylisted one. Dead code with no
    // logging downstream is plumbing, which is what that tier is for.
    let feeds_denylist = backward_closure(&graph, (0..n).filter(|&i| denylisted[i]));
    let mut inert: Vec<bool> = (0..n)
        .map(|i| denylisted[i] || (!useful[i] && feeds_denylist[i]))
        .collect();
    // That rule is backward-only, which leaves the *discard* of a denylisted
    // result behind. `LOG.info(...)` as a statement lowers to the call followed
    // by a `POP_TOP`: the call is inert, and the pop consumes its value, defines
    // nothing and reaches no effect - so it landed in plumbing, and because a
    // line takes the strongest tier on it, it dragged the whole logging line out
    // of `inert`. Sweep forward once: a node that defines nothing, is not itself
    // an effect, and reads only values produced by inert nodes belongs to the
    // same dead statement.
    for i in 0..n {
        if inert[i] || !ir.nodes[i].defs.is_empty() || ir.nodes[i].kind.is_effect() {
            continue;
        }
        let sources = &graph.uses_defs[i];
        if !sources.is_empty() && sources.iter().all(|&d| inert[graph.defs[d].node]) {
            inert[i] = true;
        }
    }
    let inert = inert;

    // --- effects and the backward slice ---------------------------------
    // Distance from the nearest effect, over data and control dependence
    // combined. Effects seed at zero; everything reachable backward from them
    // is behavior-carrying, and how far it sits scales the score.
    //
    // Inert nodes are excluded as seeds. If `log.info(...)` — or the
    // `prices.size()` call built for it — anchored the slice, every argument
    // would come back core.
    let mut dist: Vec<Option<u32>> = vec![None; n];
    let mut nearest: Vec<String> = vec![String::new(); n];
    let mut q: VecDeque<NodeId> = VecDeque::new();
    for e in ir.effects() {
        if inert[e] {
            continue;
        }
        dist[e] = Some(0);
        nearest[e] = effect_label(&ir.nodes[e].kind);
        q.push_back(e);
    }
    while let Some(node) = q.pop_front() {
        let d = dist[node].expect("queued nodes always carry a distance");
        let label = nearest[node].clone();
        // Data dependence: whoever defined what this node reads.
        let mut back: BTreeSet<NodeId> = graph.uses_defs[node]
            .iter()
            .map(|&d| graph.defs[d].node)
            .collect();
        // Control dependence: whichever branches decide whether this runs.
        back.extend(graph.ctrl_deps[node].iter().copied());
        for p in back {
            if dist[p].is_none() && !inert[p] {
                dist[p] = Some(d + 1);
                nearest[p].clone_from(&label);
                q.push_back(p);
            }
        }
    }

    // --- per-node tier and score -----------------------------------------
    let peaks = (
        graph.influence.iter().copied().max().unwrap_or(0),
        graph.depends_on.iter().copied().max().unwrap_or(0),
    );
    let mut nodes = Vec::with_capacity(n);
    for node in 0..n {
        let kind = &ir.nodes[node].kind;
        let mut reasons: Vec<Reason> = Vec::new();

        if inert[node] {
            // Only claim a denylist match when one actually happened. Reporting
            // every absorbed call as "denylisted call X" told readers the list
            // matched `response.fp.readline`, which it never did — and a tool
            // whose selling point is auditable reasons cannot afford a reason
            // that is false.
            reasons.push(match kind {
                NodeKind::Call { callee, .. } if denylisted[node] => Reason::Denylisted {
                    callee: callee.clone(),
                },
                _ => Reason::InertSupport,
            });
        } else {
            let weight = graph.control_weight(node);
            if matches!(kind, NodeKind::Branch) && weight > 0.0 {
                reasons.push(Reason::BranchPredicate {
                    weight,
                    governed: graph.controls[node].iter().filter(|&&m| m != node).count(),
                });
            }
            let carried = graph.defs_at[node]
                .iter()
                .any(|&d| graph.is_loop_carried(d));
            if carried {
                reasons.push(Reason::LoopCarried {
                    depth: graph.loop_depth[node],
                });
            }
            if kind.is_effect() {
                reasons.push(Reason::EffectSite {
                    kind: kind.tag(),
                    target: effect_target(kind),
                });
            } else if let Some(d) = dist[node]
                && d > 0
            {
                reasons.push(Reason::ReachesEffect {
                    distance: d,
                    effect: nearest[node].clone(),
                });
            }
            let fraction = graph.influence_fraction(node);
            if fraction >= CORE_INFLUENCE {
                reasons.push(Reason::HighInfluence {
                    fraction,
                    dependents: graph.influence[node],
                });
            }
            if reasons.is_empty() {
                reasons.push(Reason::Unreaching);
            }
        }

        reasons.sort_by_key(|r| std::cmp::Reverse(r.tier()));
        let tier = reasons
            .iter()
            .map(Reason::tier)
            .max()
            .unwrap_or(Tier::Plumbing);
        let score = score_of(tier, node, kind, &graph, dist[node], peaks, weights);
        nodes.push(NodeSalience {
            node,
            tier,
            score,
            reasons,
        });
    }

    FunctionSalience { nodes, graph }
}

/// Composes the continuous score from the same facts that drove the tier.
///
/// The influence term is what makes this a heatmap rather than four shades.
/// It is the fraction of the body that transitively depends on the statement,
/// so it takes as many distinct values as the function has instructions;
/// proximity, control mass and loop depth each contribute a handful.
fn score_of(
    tier: Tier,
    node: NodeId,
    kind: &NodeKind,
    graph: &Graph,
    dist: Option<u32>,
    peaks: (u32, u32),
    w: &ScoreWeights,
) -> f64 {
    if tier == Tier::Inert {
        return 0.0;
    }
    // Normalised against the most influential statement in *this* function
    // rather than against its instruction count. Dividing by size makes every
    // statement in a large body score near zero — measured, half of all lines
    // landed within 0.01 of each other — which is the opposite of what a
    // heatmap needs. Salience is a claim about relative standing inside a body,
    // so the scale should be the body's own.
    let (peak_influence, peak_depends) = peaks;
    let influence = if peak_influence == 0 {
        0.0
    } else {
        w.influence * f64::from(graph.influence[node]) / f64::from(peak_influence)
    };
    let dependency = if peak_depends == 0 {
        0.0
    } else {
        w.dependency * f64::from(graph.depends_on[node]) / f64::from(peak_depends)
    };
    let proximity = match dist {
        Some(d) => w.proximity / f64::from(d + 1),
        None => 0.0,
    };
    let control = w.control * graph.control_weight(node);
    let depth = w.loop_depth * f64::from(graph.loop_depth[node].min(4)) / 4.0;
    // A return, throw or state write is observable without looking inside
    // anything; an opaque call is observable only because we declined to look.
    // Scoring them apart keeps the frontier ranked below the places behavior
    // definitively lands.
    let effect = w.effect
        * match kind {
            NodeKind::Return | NodeKind::Throw | NodeKind::StateWrite { .. } => 1.0,
            NodeKind::Call { .. } => 0.6,
            NodeKind::Pure | NodeKind::Branch => 0.0,
        };
    (influence + dependency + proximity + control + depth + effect).clamp(0.0, 1.0)
}

/// Whether nobody reads what this node produces.
///
/// A node that discards the result — CPython's `POP_TOP`, or a JVM void call
/// with no defined value — does not count as a reader: it is the bytecode's way
/// of saying the result was ignored, which is precisely the signal wanted here.
fn result_discarded(ir: &FunctionIr, graph: &Graph, node: NodeId) -> bool {
    let mut users: BTreeSet<NodeId> = BTreeSet::new();
    for &d in &graph.defs_at[node] {
        users.extend(graph.def_users[d].iter().copied());
    }
    users.remove(&node);
    users
        .iter()
        .all(|&u| ir.nodes[u].defs.is_empty() && !ir.nodes[u].kind.is_effect())
}

/// Every node that can reach one of `seeds` by following def-use *or* control
/// dependence edges backwards — that is, every statement that contributes to a
/// seed either by producing a value it consumes or by deciding whether it runs.
///
/// Used to decide usefulness, where both kinds of contribution count: a
/// predicate guarding a network write is as load-bearing as the value written.
fn useful_closure(graph: &Graph, seeds: impl Iterator<Item = NodeId>) -> Vec<bool> {
    let mut seen = vec![false; graph.n];
    let mut q: VecDeque<NodeId> = VecDeque::new();
    for s in seeds {
        if !seen[s] {
            seen[s] = true;
            q.push_back(s);
        }
    }
    while let Some(node) = q.pop_front() {
        let sources = graph.uses_defs[node]
            .iter()
            .map(|&d| graph.defs[d].node)
            .chain(graph.ctrl_deps[node].iter().copied());
        for src in sources {
            if !seen[src] {
                seen[src] = true;
                q.push_back(src);
            }
        }
    }
    seen
}

/// Every node that can reach one of `seeds` by following def-use edges
/// backwards — that is, every node whose value flows into a seed.
///
/// Data dependence only. The question these closures answer is what a value
/// *contributes to*, and control dependence answers a different one — whether a
/// statement runs at all. Mixing them in would make almost every statement in a
/// guarded block look like it feeds whatever the block eventually does.
fn backward_closure(graph: &Graph, seeds: impl Iterator<Item = NodeId>) -> Vec<bool> {
    let mut seen = vec![false; graph.n];
    let mut q: VecDeque<NodeId> = VecDeque::new();
    for s in seeds {
        if !seen[s] {
            seen[s] = true;
            q.push_back(s);
        }
    }
    while let Some(node) = q.pop_front() {
        for &d in &graph.uses_defs[node] {
            let src = graph.defs[d].node;
            if !seen[src] {
                seen[src] = true;
                q.push_back(src);
            }
        }
    }
    seen
}

/// Effects that stay effects no matter who consumes them.
///
/// The distinction from [`NodeKind::is_effect`] is the whole inert trade: an
/// opaque call is an effect only because we chose not to look inside it, so it
/// may be absorbed when its result feeds nothing but logging. A return, a throw
/// or a state write is observable without looking inside anything, so it never
/// is.
fn is_hard_effect(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Return | NodeKind::Throw | NodeKind::StateWrite { .. }
    )
}

/// Short label for the effect a slice terminates at.
fn effect_label(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Return => "return value".to_owned(),
        NodeKind::Throw => "throw".to_owned(),
        NodeKind::StateWrite { target } => format!("state write {target}"),
        NodeKind::Call { callee, .. } => format!("opaque call {callee}"),
        NodeKind::Pure | NodeKind::Branch => "effect".to_owned(),
    }
}

/// The target string recorded on an effect-site reason.
fn effect_target(kind: &NodeKind) -> String {
    match kind {
        NodeKind::StateWrite { target } => target.clone(),
        NodeKind::Call { callee, .. } => callee.clone(),
        _ => String::new(),
    }
}

/// Accumulator while several instructions are folded onto one source line:
/// the winning tier, the highest score seen, and every distinct reason with the
/// tier it implies.
type LineAccum = (Tier, f64, Vec<(Tier, String)>);

/// One contiguous run of source lines sharing a tier.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpan {
    /// First line, inclusive.
    pub start: u32,
    /// Last line, inclusive.
    pub end: u32,
    /// Tier for every line in the run.
    pub tier: Tier,
    /// Highest score of any instruction in the run.
    pub score: f64,
    /// Where that score falls among this function's other lines, `0.0..=1.0`.
    ///
    /// The absolute score answers "is this line important" against a fixed
    /// scale, which is what a policy threshold needs. This answers "is this
    /// line important *for this function*", which is what a heatmap needs — a
    /// body whose scores all sit near 0.3 still has a most-important line, and
    /// painting it against the global scale would render the whole function one
    /// flat colour.
    pub rank: f64,
    /// Distinct reasons observed across the run, most salient first.
    pub reasons: Vec<String>,
}

/// Projects per-node results onto source lines, then merges runs.
///
/// Aggregation across the instructions on one line is a `max` over tiers, with
/// one deliberate asymmetry: `inert` is the *weakest* tier here, even though a
/// denylist match is the *strongest* signal at the node level. A line holding
/// both a log call and real work is not inert — the log call merely happens to
/// share it. Ordering [`Tier`] by salience makes that fall out of `max`.
#[must_use]
pub fn project_to_lines(ir: &FunctionIr, sal: &FunctionSalience) -> Vec<LineSpan> {
    // Reasons are carried with the tier they imply so the aggregate can be
    // ordered by salience. A line holding both a core statement and a plumbing
    // one must lead with the core reason: a consumer reading only the first
    // entry — a hook message, a gutter tooltip — would otherwise be told the
    // line is inconsequential precisely when it is not.
    let mut per_line: BTreeMap<u32, LineAccum> = BTreeMap::new();
    for ns in &sal.nodes {
        let Some(line) = ir.nodes[ns.node].line else {
            continue; // synthetic instruction with no source position
        };
        if ir.nodes[ns.node].is_structural() {
            continue; // jumps and nops carry no signal about their line
        }
        let entry = per_line
            .entry(line)
            .or_insert((Tier::Inert, 0.0, Vec::new()));
        if ns.tier > entry.0 {
            entry.0 = ns.tier;
        }
        if ns.score > entry.1 {
            entry.1 = ns.score;
        }
        for r in &ns.reasons {
            let text = r.describe();
            if !entry.2.iter().any(|(_, t)| *t == text) {
                entry.2.push((r.tier(), text));
            }
        }
    }

    // Merge consecutive lines that agree on tier *and on score*. Gaps in line
    // numbering break a run: a blank line between two core statements is not
    // itself core.
    //
    // Merging on tier alone - which this did originally - threw away the whole
    // point of computing a per-node score. A loop body is normally one unbroken
    // run of `core`, so `heapq::_siftdown`'s lines 211..216 collapsed into a
    // single span carrying the `max` of their scores: the loop predicate, the
    // index arithmetic, the comparison and both writes were all reported at
    // 0.49. No heatmap finer than the tier partition could be drawn from that,
    // and any rank correlation measured against per-line labels was measuring a
    // step function rather than the scorer. Requiring the scores to agree keeps
    // the compaction where it is real - a run of genuinely identical plumbing
    // still collapses to one span - and keeps the gradient everywhere else.
    let mut spans: Vec<LineSpan> = Vec::new();
    for (line, (tier, score, mut reasons)) in per_line {
        // Stable sort by descending tier keeps ties in discovery order, so the
        // output stays reproducible.
        reasons.sort_by(|a, b| b.0.cmp(&a.0));
        let reasons: Vec<String> = reasons.into_iter().map(|(_, t)| t).collect();
        match spans.last_mut() {
            Some(last)
                if last.tier == tier
                    && last.end + 1 == line
                    && (last.score - score).abs() < 1e-9 =>
            {
                last.end = line;
                for r in reasons {
                    if !last.reasons.contains(&r) {
                        last.reasons.push(r);
                    }
                }
            }
            _ => spans.push(LineSpan {
                start: line,
                end: line,
                tier,
                score,
                rank: 0.0, // filled in below, once every line is known
                reasons,
            }),
        }
    }
    // Rank is assigned after the fact because it is defined against the whole
    // function: percentile of this span's score among all scored spans, with
    // ties sharing the lower rank.
    let mut sorted: Vec<f64> = spans.iter().map(|s| s.score).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let denom = (sorted.len().saturating_sub(1)).max(1) as f64;
    for span in &mut spans {
        let below = sorted.partition_point(|&x| x < span.score);
        span.rank = ((below as f64) / denom).clamp(0.0, 1.0);
    }
    spans
}

/// Which algorithm produces the continuous per-node `score`.
///
/// Tier assignment is never affected: tiers are computed once, by [`analyze`],
/// before the scorer is consulted. Only [`NodeSalience::score`] differs.
///
/// [`Scorer::Panel`] is the shipped strategy — see [`crate::panel`] for why a
/// combination, the provenance of its weights, and the held-out evidence. The
/// single-instrument variants exist so the panel's members stay individually
/// inspectable and re-measurable; they are instruments, not products.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scorer {
    /// The incumbent weighted blend — [`score_of`]. What plain [`analyze`]
    /// computes.
    Current,
    /// Schur-complement deletion sensitivity — [`crate::schur`].
    Schur,
    /// Birnbaum structural importance — [`crate::pivot`].
    Pivot,
    /// Trophic-level derivation depth — [`crate::trophic`].
    Trophic,
    /// Horton–Strahler confluence order — [`crate::strahler`].
    Strahler,
    /// The fitted five-instrument combination — [`crate::panel`]. The default.
    #[default]
    Panel,
}

/// Runs tiering exactly as [`analyze`] does, then replaces the score with the
/// chosen [`Scorer`]'s output.
///
/// An `Inert`-tier node's score is floored to `0.0` regardless of scorer,
/// preserving `current`'s contract that inert means zero: a statement whose
/// only purpose is feeding logging does not become interesting because it
/// sits at a confluence.
#[must_use]
pub fn analyze_with_scorer(
    ir: &FunctionIr,
    denylist: &Denylist,
    weights: &ScoreWeights,
    scorer: Scorer,
) -> FunctionSalience {
    let mut sal = analyze(ir, denylist, weights);
    let replacement = match scorer {
        Scorer::Current => None,
        Scorer::Schur => Some(crate::schur::score(ir, &sal.graph, denylist)),
        Scorer::Pivot => Some(crate::pivot::rescale_for_display(
            &crate::pivot::birnbaum_scores(ir, &sal.graph),
        )),
        Scorer::Trophic => Some(crate::trophic::score(&sal.graph)),
        Scorer::Strahler => Some(crate::strahler::score(&sal.graph)),
        Scorer::Panel => Some(crate::panel::score(ir, &sal, denylist)),
    };
    if let Some(scores) = replacement {
        for (ns, s) in sal.nodes.iter_mut().zip(scores) {
            ns.score = if ns.tier == Tier::Inert { 0.0 } else { s };
        }
    }
    sal
}
