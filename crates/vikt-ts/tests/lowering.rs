//! Integration tests for the generic tree-sitter fallback.
//!
//! Each test locates a function by name in a lowered module and asserts on
//! its shape - node count/kinds, successor edges, def/use - rather than on
//! exact `VarId` numbering, which is an implementation detail.

use std::collections::BTreeSet;

use vikt_core::ir::{FunctionIr, NodeKind};
use vikt_ts::{Language, lower_source};

fn function<'a>(module: &'a vikt_ts::LoweredModule, name: &str) -> &'a FunctionIr {
    module
        .functions
        .iter()
        .find(|f| f.id.name == name)
        .unwrap_or_else(|| panic!("no function named {name:?} in {:?}", module.file))
}

fn kinds(f: &FunctionIr) -> Vec<&'static str> {
    f.nodes.iter().map(|n| n.kind.tag()).collect()
}

fn callees(f: &FunctionIr) -> Vec<&str> {
    f.nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Call { callee, .. } => Some(callee.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- Rust ----

const RUST_SHADOWING: &str = r"
fn shadow() -> i32 {
    let x = 1;
    {
        let x = x + 1;
        use_x(x);
    }
    x
}
";

#[test]
fn rust_shadowing_gives_distinct_vars() {
    let module = lower_source(Language::Rust, RUST_SHADOWING, "shadow.rs").expect("lowers");
    let f = function(&module, "shadow");
    f.validate().expect("valid graph");

    let outer_def = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let x = 1"))
        .and_then(|n| n.defs.first().copied())
        .expect("outer let x");
    let inner_def = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let x = x + 1"))
        .and_then(|n| n.defs.first().copied())
        .expect("inner let x");
    assert_ne!(
        outer_def, inner_def,
        "a nested `let x` must shadow with a fresh VarId, not reuse the outer one"
    );

    let inner_rhs_uses = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let x = x + 1"))
        .map(|n| n.uses.clone())
        .expect("inner let x node");
    assert!(
        inner_rhs_uses.contains(&outer_def),
        "the inner `let x`'s RHS reads the outer x, evaluated before the new binding exists"
    );

    let tail_uses = f
        .nodes
        .iter()
        .find(|n| n.label == "x")
        .map(|n| n.uses.clone())
        .expect("tail expression node");
    assert!(
        tail_uses.contains(&outer_def) && !tail_uses.contains(&inner_def),
        "the function's tail `x` refers to the outer binding, not the shadowed inner one"
    );
}

const RUST_DESTRUCTURING: &str = r"
fn split(pair: (i32, i32)) -> i32 {
    let (a, b) = pair;
    a + b
}
";

#[test]
fn rust_tuple_destructuring_binds_every_name() {
    let module = lower_source(Language::Rust, RUST_DESTRUCTURING, "split.rs").expect("lowers");
    let f = function(&module, "split");
    f.validate().expect("valid graph");

    let binding = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let (a, b)"))
        .expect("destructuring let");
    assert_eq!(
        binding.defs.len(),
        2,
        "both `a` and `b` must be defined: {:?}",
        binding.defs
    );

    let sum = f
        .nodes
        .iter()
        .find(|n| n.label == "a + b")
        .expect("a + b node");
    assert_eq!(
        BTreeSet::from_iter(sum.uses.iter().copied()),
        BTreeSet::from_iter(binding.defs.iter().copied()),
        "a + b must use exactly the two destructured bindings"
    );
}

const RUST_LOOP: &str = r"
fn sum_upto(n: i32) -> i32 {
    let mut total = 0;
    for i in 0..n {
        total += i;
    }
    total
}
";

#[test]
fn rust_for_loop_has_loop_carried_defs_and_a_back_edge() {
    let module = lower_source(Language::Rust, RUST_LOOP, "loop.rs").expect("lowers");
    let f = function(&module, "sum_upto");
    f.validate().expect("valid graph");

    let (branch_id, branch) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("for i in"))
        .expect("the for-loop's iterator-advance Branch node");
    assert!(
        !branch.defs.is_empty(),
        "the loop pattern `i` must be a def on the Branch node"
    );

    let body = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total += i"))
        .expect("loop body statement");
    assert!(
        body.succs.contains(&branch_id),
        "the loop body must edge back to the Branch node (real back edge, not just fallthrough)"
    );
    assert!(
        body.defs.iter().any(|d| body.uses.contains(d)),
        "a compound assignment both reads and writes `total`"
    );
}

const RUST_EARLY_EXIT: &str = r"
fn first_even(xs_len: i32) -> i32 {
    let mut i = 0;
    while i < xs_len {
        if i % 2 == 0 {
            return i;
        }
        i += 1;
    }
    -1
}
";

#[test]
fn rust_return_inside_branch_has_no_successor_and_no_fallthrough_to_increment() {
    let module = lower_source(Language::Rust, RUST_EARLY_EXIT, "early.rs").expect("lowers");
    let f = function(&module, "first_even");
    f.validate().expect("valid graph");

    let ret = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("a Return node");
    assert!(
        ret.succs.is_empty(),
        "a return has no successors - it exits the function"
    );

    let increment = f
        .nodes
        .iter()
        .position(|n| n.label.starts_with("i += 1"))
        .expect("the increment statement");
    assert!(
        !f.nodes
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Return) && n.succs.contains(&increment)),
        "control never falls from the early return into the increment"
    );
}

const RUST_BARE_LOOP: &str = r"
fn find_first(limit: i32) -> i32 {
    let mut i = 0;
    loop {
        if i >= limit {
            return -1;
        }
        if scan(i) {
            return i;
        }
        i += 1;
    }
}
";

#[test]
fn rust_bare_loop_models_returns_and_the_back_edge() {
    let module = lower_source(Language::Rust, RUST_BARE_LOOP, "find_first.rs").expect("lowers");
    let f = function(&module, "find_first");
    f.validate().expect("valid graph");

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(
        returns.len(),
        2,
        "both `return`s inside `loop {{ .. }}` must be modeled as Return nodes: {:?}",
        kinds(f)
    );
    assert!(
        returns.iter().all(|n| n.succs.is_empty()),
        "a return has no successors, even from inside a bare loop"
    );

    let (branch_id, branch) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch))
        .expect("the loop's head Branch node");
    let increment = f
        .nodes
        .iter()
        .position(|n| n.label.starts_with("i += 1"))
        .expect("the increment statement");
    assert!(
        f.nodes[increment].succs.contains(&branch_id),
        "the loop body must edge back to the head node"
    );
    assert!(
        branch.succs.contains(&increment) || f.nodes.iter().any(|n| n.succs.contains(&increment)),
        "the head node must reach into the loop body"
    );
}

const RUST_LET_ELSE: &str = r"
fn unwrap_or_bail(opt: Option<i32>) -> i32 {
    let Some(x) = opt else {
        return -1;
    };
    x + 1
}
";

#[test]
fn rust_let_else_diverging_block_keeps_its_return_visible() {
    let module = lower_source(Language::Rust, RUST_LET_ELSE, "bail.rs").expect("lowers");
    let f = function(&module, "unwrap_or_bail");
    f.validate().expect("valid graph");

    let binding = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let Some(x)"))
        .expect("the let-else binding node");
    assert!(
        matches!(binding.kind, NodeKind::Branch),
        "a let-else binding is genuinely two-way, not a plain Pure unit: {:?}",
        binding.kind
    );

    let ret = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("the else-block's `return -1` must survive as a Return node, not vanish");
    assert!(ret.succs.is_empty());

    let tail = f
        .nodes
        .iter()
        .find(|n| n.label == "x + 1")
        .expect("the fallthrough tail expression");
    assert!(
        tail.uses.iter().any(|u| binding.defs.contains(u)),
        "the tail still reads the pattern-bound `x` on the success path"
    );
}

const RUST_CALLS: &str = r"
fn compute(x: i32) -> i32 {
    let doubled = scale(x);
    let obj = Builder::new();
    obj.build(doubled)
}
";

#[test]
fn rust_extracts_free_and_method_call_targets() {
    let module = lower_source(Language::Rust, RUST_CALLS, "calls.rs").expect("lowers");
    let f = function(&module, "compute");
    f.validate().expect("valid graph");

    let names = callees(f);
    assert!(
        names.contains(&"scale"),
        "free call target extracted: {names:?}"
    );
    // `Builder::new` is a path expression (`scoped_identifier`), not a
    // method/field access, so it falls back to its literal source text
    // rather than the dotted-chain form method calls get.
    assert!(
        names.contains(&"Builder::new"),
        "associated-fn call target extracted: {names:?}"
    );
    assert!(
        names.contains(&"obj.build"),
        "method call target extracted with receiver: {names:?}"
    );
}

const RUST_STATE_WRITE: &str = r"
fn bump(counter: Counter) {
    counter.value = counter.value + 1;
}
";

#[test]
fn rust_field_assignment_is_a_state_write() {
    let module = lower_source(Language::Rust, RUST_STATE_WRITE, "bump.rs").expect("lowers");
    let f = function(&module, "bump");
    f.validate().expect("valid graph");

    let write = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::StateWrite { .. }))
        .expect("a StateWrite node for the field assignment");
    let NodeKind::StateWrite { target } = &write.kind else {
        unreachable!()
    };
    assert_eq!(target, "counter.value");
    assert!(write.kind.is_effect(), "a state write is always an effect");
}

const RUST_DEAD_EARLY_BEHAVIOR_LATE: &str = r"
fn process(flag: bool, value: i32) -> i32 {
    let unused_bookkeeping = value * 2;
    if flag {
        log_call(unused_bookkeeping);
    }
    persist(value)
}
";

#[test]
fn rust_tiering_prefers_the_effectful_tail_over_dead_bookkeeping() {
    let module =
        lower_source(Language::Rust, RUST_DEAD_EARLY_BEHAVIOR_LATE, "process.rs").expect("lowers");
    let f = function(&module, "process");
    f.validate().expect("valid graph");

    // Sanity check on the shape a scorer would consume: the tail call is an
    // effect, and the early bookkeeping binding is not by itself.
    let bookkeeping = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let unused_bookkeeping"))
        .expect("bookkeeping node");
    assert!(!bookkeeping.kind.is_effect());

    let effects: Vec<_> = f.effects().into_iter().map(|id| &f.nodes[id]).collect();
    assert!(
        effects
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Call{callee, ..} if callee == "persist")),
        "the tail call must be classified as an effect: {:?}",
        effects.iter().map(|n| &n.label).collect::<Vec<_>>()
    );
}

const RUST_LET_IN_MODELED_MATCH: &str = r"
fn pick(y: i32, cond: bool) -> i32 {
    match cond {
        true => {
            let y = 99;
            use_x(y);
        }
        false => {}
    };
    y
}
";

#[test]
fn rust_nested_let_in_match_arm_shadows_outer_param() {
    // A `let` nested inside a match arm must shadow the outer parameter of
    // the same name, scoped to just that arm.
    let module =
        lower_source(Language::Rust, RUST_LET_IN_MODELED_MATCH, "pick.rs").expect("lowers");
    let f = function(&module, "pick");
    f.validate().expect("valid graph");

    let param_def = f
        .nodes
        .iter()
        .find(|n| n.label == "<params>")
        .and_then(|n| n.defs.first().copied())
        .expect("y is the first param");

    let arm_let = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let y = 99"))
        .expect("the arm's own `let y`");
    let arm_y = arm_let.defs.first().copied().expect("arm-scoped y");
    assert_ne!(
        arm_y, param_def,
        "the arm's nested `let y` must shadow with a fresh VarId, not reuse the outer param"
    );

    let use_call = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "use_x"))
        .expect("use_x(y) call node");
    assert!(
        use_call.uses.contains(&arm_y) && !use_call.uses.contains(&param_def),
        "use_x(y) inside the arm must read the arm-scoped y, not the outer param"
    );

    let tail_uses = f
        .nodes
        .iter()
        .find(|n| n.label == "y")
        .map(|n| n.uses.clone())
        .expect("tail expression node");
    assert!(
        tail_uses.contains(&param_def),
        "the function's tail `y` must still read the outer parameter"
    );
}

const RUST_MATCH_RETURN_IN_ARM: &str = r"
fn classify(x: i32) -> i32 {
    match x {
        0 => {
            return -1;
        }
        n if n > 10 => {
            log_big(n);
            return n;
        }
        _ => 0,
    }
}
";

#[test]
fn rust_match_models_branch_fanout_and_returns_inside_arms() {
    let module =
        lower_source(Language::Rust, RUST_MATCH_RETURN_IN_ARM, "classify.rs").expect("lowers");
    let f = function(&module, "classify");
    f.validate().expect("valid graph");

    let (disc_id, disc) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("match x"))
        .expect("the discriminant Branch node");
    assert!(
        disc.succs.len() >= 3,
        "the discriminant must fan out to every arm's head: {:?}",
        disc.succs
    );

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(
        returns.len(),
        2,
        "both `return`s inside arms must reach effect classification as real Return nodes: {:?}",
        kinds(f)
    );
    assert!(returns.iter().all(|n| n.succs.is_empty()));

    let log_call = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "log_big"))
        .expect("log_big(n) call inside the guarded arm");
    assert!(
        !f.nodes[disc_id].succs.contains(&log_call),
        "the guarded arm's call must not be directly reachable from the discriminant - it \
         sits behind that arm's own head node"
    );

    // A guard's own reads (`n > 10`) must resolve to the pattern binding
    // `n`, not spawn an unrelated ambient variable.
    let guard_arm_head = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("n if n > 10"))
        .expect("the guarded arm's head node");
    assert_eq!(guard_arm_head.defs.len(), 1, "`n` is the pattern binding");
    assert!(
        guard_arm_head.uses.contains(&guard_arm_head.defs[0]),
        "the guard `n > 10` must read the just-declared pattern binding `n`"
    );
}

const RUST_CLOSURE: &str = r"
fn outer(base: i32) -> i32 {
    let add = |x: i32| helper(x) + base;
    add(5)
}
";

#[test]
fn rust_closure_becomes_its_own_brace_qualified_function() {
    let module = lower_source(Language::Rust, RUST_CLOSURE, "outer.rs").expect("lowers");
    let closure = module
        .functions
        .iter()
        .find(|f| f.id.name.contains("{closure#"))
        .expect("the closure must lower as its own FunctionIr");
    assert_eq!(closure.id.name, "outer::{closure#0}");
    closure.validate().expect("valid graph");

    // `x` is the closure's own parameter; `base` and `helper` are captured
    // from the enclosing scope - both must still resolve as uses inside the
    // closure's own body, not vanish.
    let param_def = closure
        .nodes
        .iter()
        .find(|n| n.label == "<params>")
        .and_then(|n| n.defs.first().copied())
        .expect("x param");
    assert!(
        callees(closure).contains(&"helper"),
        "the call inside the closure must be kept as the closure's own Call node: {:?}",
        callees(closure)
    );
    let tail = closure
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("closure tail expression as a Return node");
    assert!(tail.uses.contains(&param_def), "the tail reads `x`");
    assert!(
        tail.uses.len() >= 2,
        "the tail must also read the captured `base`: {:?}",
        tail.uses
    );

    // Neither the capture nor the inner call may leak into the enclosing
    // `let add = ...` statement - the whole closure subtree is opaque to it.
    let outer = function(&module, "outer");
    let let_add = outer
        .nodes
        .iter()
        .find(|n| n.label.starts_with("let add"))
        .expect("let add node");
    assert!(
        let_add.uses.is_empty(),
        "the closure body must not leak identifiers into the enclosing statement: {:?}",
        let_add.uses
    );
    assert!(
        !callees(outer).contains(&"helper"),
        "the call inside the closure must not also be extracted into the enclosing function: {:?}",
        callees(outer)
    );
}

#[test]
fn rust_lowering_is_deterministic() {
    let a = lower_source(Language::Rust, RUST_LOOP, "loop.rs").expect("lowers");
    let b = lower_source(Language::Rust, RUST_LOOP, "loop.rs").expect("lowers");
    assert_eq!(
        kinds(function(&a, "sum_upto")),
        kinds(function(&b, "sum_upto"))
    );
    for (fa, fb) in a.functions.iter().zip(b.functions.iter()) {
        assert_eq!(fa.nodes.len(), fb.nodes.len());
        for (na, nb) in fa.nodes.iter().zip(fb.nodes.iter()) {
            assert_eq!(na.defs, nb.defs);
            assert_eq!(na.uses, nb.uses);
            assert_eq!(na.succs, nb.succs);
            assert_eq!(na.label, nb.label);
        }
    }
}

// -------------------------------------------------------------- Python ----

const PY_SHADOWING: &str = "
def widen(x):
    total = x
    if x > 0:
        total = total + 1
    return total
";

#[test]
fn python_reassignment_reuses_the_same_slot() {
    let module = lower_source(Language::Python, PY_SHADOWING, "widen.py").expect("lowers");
    let f = function(&module, "widen");
    f.validate().expect("valid graph");

    let first = f
        .nodes
        .iter()
        .find(|n| n.label == "total = x")
        .and_then(|n| n.defs.first().copied())
        .expect("first assignment");
    let second = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total = total + 1"))
        .and_then(|n| n.defs.first().copied())
        .expect("reassignment inside if");
    assert_eq!(
        first, second,
        "Python has no block scoping: reassigning `total` inside `if` must reuse the same slot"
    );
}

const PY_DESTRUCTURING: &str = "
def split(pair):
    a, b = pair
    return a + b
";

#[test]
fn python_tuple_unpacking_binds_every_name() {
    let module = lower_source(Language::Python, PY_DESTRUCTURING, "split.py").expect("lowers");
    let f = function(&module, "split");
    f.validate().expect("valid graph");

    let binding = f
        .nodes
        .iter()
        .find(|n| n.label == "a, b = pair")
        .expect("unpacking assignment");
    assert_eq!(
        binding.defs.len(),
        2,
        "both `a` and `b` defined: {:?}",
        binding.defs
    );
}

const PY_LOOP: &str = "
def total_upto(n):
    total = 0
    for i in range(n):
        total += i
    return total
";

#[test]
fn python_for_loop_has_loop_carried_defs_and_a_back_edge() {
    let module = lower_source(Language::Python, PY_LOOP, "loop.py").expect("lowers");
    let f = function(&module, "total_upto");
    f.validate().expect("valid graph");

    let (branch_id, branch) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("for i in"))
        .expect("the for-loop's iterator-advance Branch node");
    assert!(
        !branch.defs.is_empty(),
        "the loop variable `i` must be a def"
    );

    let body = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total += i"))
        .expect("loop body");
    assert!(
        body.succs.contains(&branch_id),
        "back edge from body to the loop's Branch node"
    );
}

const PY_EARLY_EXIT: &str = "
def first_even(n):
    i = 0
    while i < n:
        if i % 2 == 0:
            return i
        i += 1
    return -1
";

#[test]
fn python_return_inside_branch_has_no_successors() {
    let module = lower_source(Language::Python, PY_EARLY_EXIT, "early.py").expect("lowers");
    let f = function(&module, "first_even");
    f.validate().expect("valid graph");

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(
        returns.len(),
        2,
        "two return statements: {:?}",
        returns.iter().map(|n| &n.label).collect::<Vec<_>>()
    );
    assert!(returns.iter().all(|n| n.succs.is_empty()));
}

const PY_CALLS: &str = "
def compute(x):
    doubled = scale(x)
    obj = Builder()
    return obj.build(doubled)
";

#[test]
fn python_extracts_free_and_method_call_targets() {
    let module = lower_source(Language::Python, PY_CALLS, "calls.py").expect("lowers");
    let f = function(&module, "compute");
    f.validate().expect("valid graph");

    let names = callees(f);
    assert!(names.contains(&"scale"), "free call target: {names:?}");
    assert!(
        names.contains(&"Builder"),
        "constructor-style call target: {names:?}"
    );
    assert!(
        names.contains(&"obj.build"),
        "method call target with receiver: {names:?}"
    );
}

const PY_STATE_WRITE: &str = "
def bump(counter):
    counter.value = counter.value + 1
";

#[test]
fn python_attribute_assignment_is_a_state_write() {
    let module = lower_source(Language::Python, PY_STATE_WRITE, "bump.py").expect("lowers");
    let f = function(&module, "bump");
    f.validate().expect("valid graph");

    let write = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::StateWrite { .. }))
        .expect("a StateWrite node for the attribute assignment");
    let NodeKind::StateWrite { target } = &write.kind else {
        unreachable!()
    };
    assert_eq!(target, "counter.value");
}

const PY_RAISE: &str = "
def validate(x):
    if x < 0:
        raise ValueError('negative')
    return x
";

#[test]
fn python_raise_is_throw_and_has_no_successors() {
    let module = lower_source(Language::Python, PY_RAISE, "validate.py").expect("lowers");
    let f = function(&module, "validate");
    f.validate().expect("valid graph");

    let throw = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Throw))
        .expect("a Throw node");
    assert!(throw.succs.is_empty());
    assert!(throw.kind.is_effect());
}

const PY_MATCH_RETURN_IN_ARM: &str = "
def classify(x):
    match x:
        case 0:
            return -1
        case n if n > 10:
            log_big(n)
            return n
        case _:
            return 0
";

#[test]
fn python_match_models_branch_fanout_and_returns_inside_arms() {
    let module =
        lower_source(Language::Python, PY_MATCH_RETURN_IN_ARM, "classify.py").expect("lowers");
    let f = function(&module, "classify");
    f.validate().expect("valid graph");

    let (disc_id, disc) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("match x"))
        .expect("the discriminant Branch node");
    assert!(
        disc.succs.len() >= 3,
        "the discriminant must fan out to every arm's head: {:?}",
        disc.succs
    );

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(
        returns.len(),
        3,
        "every arm's `return` must reach effect classification: {:?}",
        kinds(f)
    );
    assert!(returns.iter().all(|n| n.succs.is_empty()));

    let guard_head = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("case n if n > 10"))
        .expect("the guarded arm's head node");
    assert_eq!(
        guard_head.defs.len(),
        1,
        "`n` is the capture-pattern binding"
    );
    assert!(
        guard_head.uses.contains(&guard_head.defs[0]),
        "the guard `n > 10` must read the just-declared pattern binding `n`"
    );

    let log_call = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "log_big"))
        .expect("log_big(n) call inside the guarded arm");
    assert!(
        !f.nodes[disc_id].succs.contains(&log_call),
        "the guarded arm's call sits behind that arm's own head, not directly off the discriminant"
    );
}

const PY_TRY_CATCH: &str = "
def f(x):
    try:
        risky(x)
    except ValueError as e:
        log(e)
        return -1
    return 0
";

#[test]
fn python_try_models_handler_binding_and_visible_return_but_stays_unreachable() {
    let module = lower_source(Language::Python, PY_TRY_CATCH, "f.py").expect("lowers");
    let f = function(&module, "f");
    f.validate().expect("valid graph");

    let (catch_id, catch_entry) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.label == "<catch>")
        .expect("the handler's entry node");
    assert_eq!(
        catch_entry.defs.len(),
        1,
        "the `as e` binding must be declared on the handler entry"
    );
    assert!(
        !f.nodes.iter().any(|n| n.succs.contains(&catch_id)),
        "the handler must never be linked in from the try body's normal flow"
    );

    let handler_return = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("the handler's own `return -1` must be a real Return node");
    assert!(handler_return.succs.is_empty());

    let log_call = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "log"))
        .expect("log(e) call inside the handler");
    assert!(
        log_call.uses.contains(&catch_entry.defs[0]),
        "log(e) must read the handler-bound `e`"
    );
}

const PY_CLOSURE: &str = "
def outer(base):
    add = lambda x: helper(x) + base
    return add(5)
";

#[test]
fn python_lambda_becomes_its_own_angle_qualified_function() {
    let module = lower_source(Language::Python, PY_CLOSURE, "outer.py").expect("lowers");
    let lam = module
        .functions
        .iter()
        .find(|f| f.id.name.contains("<lambda>"))
        .expect("the lambda must lower as its own FunctionIr");
    assert_eq!(lam.id.name, "outer.<locals>.<lambda>");
    lam.validate().expect("valid graph");
    assert!(
        callees(lam).contains(&"helper"),
        "the call inside the lambda must be the lambda's own Call node: {:?}",
        callees(lam)
    );

    let outer = function(&module, "outer");
    let assign = outer
        .nodes
        .iter()
        .find(|n| n.label.starts_with("add = lambda"))
        .expect("the assignment node");
    assert!(
        assign.uses.is_empty(),
        "the lambda body must not leak identifiers into the enclosing assignment: {:?}",
        assign.uses
    );
    assert!(!callees(outer).contains(&"helper"));
}

#[test]
fn python_lowering_is_deterministic() {
    let a = lower_source(Language::Python, PY_LOOP, "loop.py").expect("lowers");
    let b = lower_source(Language::Python, PY_LOOP, "loop.py").expect("lowers");
    for (fa, fb) in a.functions.iter().zip(b.functions.iter()) {
        assert_eq!(fa.nodes.len(), fb.nodes.len());
        for (na, nb) in fa.nodes.iter().zip(fb.nodes.iter()) {
            assert_eq!(na.defs, nb.defs);
            assert_eq!(na.uses, nb.uses);
            assert_eq!(na.succs, nb.succs);
        }
    }
}

// --------------------------------------------------------- error paths ----

#[test]
fn a_parse_error_degrades_only_its_own_function() {
    let source = r"
fn broken( {
    let x = ;
}

fn fine(x: i32) -> i32 {
    x + 1
}
";
    let module = lower_source(Language::Rust, source, "mixed.rs")
        .expect("file-level ratio is low enough to lower");
    assert!(
        module.diagnostics.iter().any(|d| d.function == "broken"),
        "the broken function must be reported: {:?}",
        module.diagnostics
    );
    let fine = function(&module, "fine");
    assert!(
        fine.len() > 1,
        "the syntactically fine function must still be fully lowered"
    );
}

#[test]
fn a_module_scope_parse_error_is_reported_not_swallowed() {
    // `BROKEN`'s initializer is missing entirely - an ERROR/MISSING node at
    // top level, outside every function, so the per-function `has_error`
    // stub (which only guards `fn_nodes`) never sees it. Padded with enough
    // syntactically fine functions that the file-level bad/total ratio
    // stays well under the 25% rejection threshold.
    let source = r"
const BROKEN: i32 = ;

fn one(x: i32) -> i32 { x + 1 }
fn two(x: i32) -> i32 { x + 2 }
fn three(x: i32) -> i32 { x + 3 }
fn four(x: i32) -> i32 { x + 4 }
fn five(x: i32) -> i32 { x + 5 }
";
    let module = lower_source(Language::Rust, source, "mixed_module.rs")
        .expect("file-level ratio is low enough to lower");
    assert!(
        module.diagnostics.iter().any(|d| d.function == "<module>"),
        "a top-level parse error must be reported against <module>, not silently absorbed: {:?}",
        module.diagnostics
    );
    let five = function(&module, "five");
    assert!(
        five.len() > 1,
        "a syntactically fine function elsewhere in the file must still be fully lowered"
    );
}

#[test]
fn a_mostly_broken_file_fails_loudly() {
    let source = "fn @@@ ( { { { ] ) )) &&& !!!";
    let err = lower_source(Language::Rust, source, "garbage.rs")
        .expect_err("mostly-error file must be rejected");
    assert!(matches!(err, vikt_ts::TsError::Syntax { .. }), "{err:?}");
}

#[test]
fn generator_string_identifies_the_grammar() {
    let module = lower_source(Language::Rust, "fn f() {}", "f.rs").expect("lowers");
    assert_eq!(module.generator, "vikt-ts/tree-sitter-rust");
    let module = lower_source(Language::Python, "def f(): pass", "f.py").expect("lowers");
    assert_eq!(module.generator, "vikt-ts/tree-sitter-python");
    let module = lower_source(Language::Java, "class X {}", "X.java").expect("lowers");
    assert_eq!(module.generator, "vikt-ts/tree-sitter-java");
    let module = lower_source(Language::Kotlin, "fun f() {}", "f.kt").expect("lowers");
    assert_eq!(module.generator, "vikt-ts/tree-sitter-kotlin");
    let module = lower_source(Language::Go, "func f() {}", "f.go").expect("lowers");
    assert_eq!(module.generator, "vikt-ts/tree-sitter-go");
}

// ---------------------------------------------------------------- Java ----

const JAVA_METHOD_NAMING: &str = r"
class Box {
    int value;
    int get() {
        return value;
    }
    void bump(int delta) {
        value = value + delta;
    }
}
";

#[test]
fn java_methods_are_named_type_colon_colon_method() {
    let module = lower_source(Language::Java, JAVA_METHOD_NAMING, "Box.java").expect("lowers");
    assert!(
        module.functions.iter().any(|f| f.id.name == "Box::get"),
        "found: {:?}",
        module
            .functions
            .iter()
            .map(|f| &f.id.name)
            .collect::<Vec<_>>()
    );
    assert!(module.functions.iter().any(|f| f.id.name == "Box::bump"));
}

const JAVA_MULTI_DECLARATOR: &str = r"
class Pair {
    int sum() {
        int a = 1, b = 2;
        return a + b;
    }
}
";

#[test]
fn java_multi_declarator_binds_one_node_per_declarator() {
    let module = lower_source(Language::Java, JAVA_MULTI_DECLARATOR, "Pair.java").expect("lowers");
    let f = function(&module, "Pair::sum");
    f.validate().expect("valid graph");

    let a = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("a = 1"))
        .expect("a's own declarator node");
    let b = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("b = 2"))
        .expect("b's own declarator node");
    assert_eq!(a.defs.len(), 1);
    assert_eq!(b.defs.len(), 1);
    assert_ne!(a.defs[0], b.defs[0]);

    let sum = f
        .nodes
        .iter()
        .find(|n| n.label == "return a + b;")
        .expect("return a + b");
    assert!(sum.uses.contains(&a.defs[0]));
    assert!(sum.uses.contains(&b.defs[0]));
}

const JAVA_LOOP: &str = r"
class Summer {
    int sumPositive(int[] xs) {
        int total = 0;
        for (int x : xs) {
            if (x < 0) {
                continue;
            }
            total += x;
        }
        return total;
    }
}
";

#[test]
fn java_enhanced_for_has_loop_carried_defs_and_a_back_edge() {
    let module = lower_source(Language::Java, JAVA_LOOP, "Summer.java").expect("lowers");
    let f = function(&module, "Summer::sumPositive");
    f.validate().expect("valid graph");

    let (branch_id, branch) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("for (int x"))
        .expect("the enhanced-for's iterator-advance Branch node");
    assert!(
        !branch.defs.is_empty(),
        "the loop variable `x` must be a def"
    );

    let body = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total += x"))
        .expect("loop body statement");
    assert!(
        body.succs.contains(&branch_id),
        "the loop body must edge back to the Branch node"
    );

    let cont = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("continue"))
        .expect("the continue node");
    assert!(
        cont.succs.contains(&branch_id),
        "continue must edge to the loop's Branch node, not fall through to total += x"
    );
}

const JAVA_UNBRACED_FOR_BODY: &str = r"
class Router {
    void route(String[] items) {
        for (String s : items) if (s.isEmpty()) skip(s); else handle(s);
    }
}
";

#[test]
fn java_enhanced_for_with_unbraced_if_body_keeps_branches_mutually_exclusive() {
    // `for (..) if (cond) skip(s); else handle(s);` has no braces around the
    // for-each body at all - `body` is the `if_statement` itself, not a
    // `block` wrapping it. Dispatching it as a sequence of its own children
    // (the old bug) would splice the condition, `skip(s)` and `handle(s)`
    // together as three unconditional back-to-back statements, running both
    // branches every iteration instead of exactly one.
    let module =
        lower_source(Language::Java, JAVA_UNBRACED_FOR_BODY, "Router.java").expect("lowers");
    let f = function(&module, "Router::route");
    f.validate().expect("valid graph");

    let branch_count = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Branch))
        .count();
    assert_eq!(
        branch_count,
        2,
        "the unbraced `if` must still be lowered as its own Branch alongside the for-each's \
         iterator-advance Branch, not spliced flat into the loop body: {:?}",
        kinds(f)
    );

    let skip_id = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "skip"))
        .expect("skip() call node");
    let handle_id = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "handle"))
        .expect("handle() call node");
    let reaches_directly = |from: usize, to: usize| f.nodes[from].succs.contains(&to);
    assert!(
        !reaches_directly(skip_id, handle_id) && !reaches_directly(handle_id, skip_id),
        "skip() and handle() sit on mutually exclusive branches - neither may edge straight \
         into the other as if both ran unconditionally in sequence"
    );
}

const JAVA_CALLS: &str = r"
class Calc {
    int compute(int x) {
        int doubled = scale(x);
        Builder obj = Builder.make();
        return obj.build(doubled);
    }
}
";

#[test]
fn java_extracts_free_static_and_method_call_targets() {
    let module = lower_source(Language::Java, JAVA_CALLS, "Calc.java").expect("lowers");
    let f = function(&module, "Calc::compute");
    f.validate().expect("valid graph");

    let names = callees(f);
    assert!(names.contains(&"scale"), "free call target: {names:?}");
    assert!(
        names.contains(&"Builder.make"),
        "static call target with receiver: {names:?}"
    );
    assert!(
        names.contains(&"obj.build"),
        "method call target with receiver: {names:?}"
    );
}

const JAVA_STATE_WRITE: &str = r"
class Counter {
    void bump(Counter counter) {
        counter.value = counter.value + 1;
    }
}
";

#[test]
fn java_field_assignment_is_a_state_write() {
    let module = lower_source(Language::Java, JAVA_STATE_WRITE, "Counter.java").expect("lowers");
    let f = function(&module, "Counter::bump");
    f.validate().expect("valid graph");

    let write = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::StateWrite { .. }))
        .expect("a StateWrite node for the field assignment");
    let NodeKind::StateWrite { target } = &write.kind else {
        unreachable!()
    };
    assert_eq!(target, "counter.value");
    assert!(write.kind.is_effect());
}

const JAVA_EARLY_EXIT: &str = r"
class Finder {
    int firstEven(int n) {
        int i = 0;
        while (i < n) {
            if (i % 2 == 0) {
                return i;
            }
            i += 1;
        }
        return -1;
    }
}
";

#[test]
fn java_return_inside_branch_has_no_successors() {
    let module = lower_source(Language::Java, JAVA_EARLY_EXIT, "Finder.java").expect("lowers");
    let f = function(&module, "Finder::firstEven");
    f.validate().expect("valid graph");

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(returns.len(), 2);
    assert!(returns.iter().all(|n| n.succs.is_empty()));
}

const JAVA_SWITCH_RETURN_IN_ARM: &str = r"
class Classify {
    int classify(int x) {
        switch (x) {
            case 0:
                return -1;
            case 1:
            case 2:
                logHit(x);
                return x;
            default:
                return 0;
        }
    }
}
";

#[test]
fn java_colon_switch_models_branch_fanout_and_returns_inside_arms() {
    let module =
        lower_source(Language::Java, JAVA_SWITCH_RETURN_IN_ARM, "Classify.java").expect("lowers");
    let f = function(&module, "Classify::classify");
    f.validate().expect("valid graph");

    let (disc_id, disc) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("switch (x)"))
        .expect("the discriminant Branch node");
    assert!(
        disc.succs.len() >= 4,
        "one head per switch_block_statement_group, including the empty `case 1:` fallthrough \
         label: {:?}",
        disc.succs
    );

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(returns.len(), 3, "{:?}", kinds(f));
    assert!(returns.iter().all(|n| n.succs.is_empty()));

    let hit_call = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "logHit"))
        .expect("logHit(x) call inside the `case 2` arm");
    assert!(!f.nodes[disc_id].succs.contains(&hit_call));
}

const JAVA_ARROW_SWITCH: &str = r"
class Classify {
    void classify(int x) {
        switch (x) {
            case 0 -> logZero();
            case 1, 2 -> {
                logHit(x);
                logDone();
            }
            default -> logDefault();
        }
    }
}
";

#[test]
fn java_arrow_switch_arms_lower_through_the_same_generic_path() {
    // The arrow form (`switch_rule`) has a completely different shape from
    // the colon form (label + trailing statements vs. label + one body
    // node) - this only exercises the other half of `match_arm_kinds`. Used
    // as a bare statement (not returned/assigned), since match modeling
    // only applies when the construct is itself the statement - see the
    // module docs.
    let module = lower_source(Language::Java, JAVA_ARROW_SWITCH, "Classify.java").expect("lowers");
    let f = function(&module, "Classify::classify");
    f.validate().expect("valid graph");

    let disc = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("switch (x)"))
        .expect("the discriminant Branch node");
    assert_eq!(
        disc.succs.len(),
        3,
        "one head per switch_rule: {:?}",
        disc.succs
    );
    assert!(
        callees(f).contains(&"logHit"),
        "the call inside the multi-value arrow arm must still be extracted: {:?}",
        callees(f)
    );
}

const JAVA_TRY_CATCH: &str = r"
class Risky {
    int f(int x) {
        try {
            risky(x);
        } catch (IllegalArgumentException e) {
            log(e);
            return -1;
        }
        return 0;
    }
}
";

#[test]
fn java_try_models_handler_binding_and_visible_return_but_stays_unreachable() {
    let module = lower_source(Language::Java, JAVA_TRY_CATCH, "Risky.java").expect("lowers");
    let f = function(&module, "Risky::f");
    f.validate().expect("valid graph");

    let (catch_id, catch_entry) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.label == "<catch>")
        .expect("the handler's entry node");
    assert_eq!(catch_entry.defs.len(), 1, "the caught `e` must be declared");
    assert!(
        !f.nodes.iter().any(|n| n.succs.contains(&catch_id)),
        "the handler must never be linked in from the try body's normal flow"
    );

    let handler_return = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("the handler's own return must be a real Return node");
    assert!(handler_return.succs.is_empty());

    let log_call = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "log"))
        .expect("log(e) call inside the handler");
    assert!(log_call.uses.contains(&catch_entry.defs[0]));
}

const JAVA_CLOSURE: &str = r"
class Outer {
    int outer(int base) {
        java.util.function.IntUnaryOperator add = x -> helper(x) + base;
        return add.applyAsInt(5);
    }
}
";

#[test]
fn java_lambda_becomes_its_own_dollar_qualified_function() {
    let module = lower_source(Language::Java, JAVA_CLOSURE, "Outer.java").expect("lowers");
    let lam = module
        .functions
        .iter()
        .find(|f| f.id.name.starts_with("lambda$"))
        .expect("the lambda must lower as its own FunctionIr");
    assert_eq!(lam.id.name, "lambda$Outer::outer$0");
    lam.validate().expect("valid graph");
    assert!(callees(lam).contains(&"helper"));

    let outer = function(&module, "Outer::outer");
    let decl = outer
        .nodes
        .iter()
        .find(|n| n.label.starts_with("add = x"))
        .expect("the declarator node");
    assert!(
        decl.uses.is_empty(),
        "the lambda body must not leak identifiers into the enclosing declaration: {:?}",
        decl.uses
    );
    assert!(!callees(outer).contains(&"helper"));
}

#[test]
fn java_lowering_is_deterministic() {
    let a = lower_source(Language::Java, JAVA_LOOP, "Summer.java").expect("lowers");
    let b = lower_source(Language::Java, JAVA_LOOP, "Summer.java").expect("lowers");
    for (fa, fb) in a.functions.iter().zip(b.functions.iter()) {
        assert_eq!(fa.nodes.len(), fb.nodes.len());
        for (na, nb) in fa.nodes.iter().zip(fb.nodes.iter()) {
            assert_eq!(na.defs, nb.defs);
            assert_eq!(na.uses, nb.uses);
            assert_eq!(na.succs, nb.succs);
        }
    }
}

// -------------------------------------------------------------- Kotlin ----

const KOTLIN_METHOD_NAMING: &str = "
class Box {
    var value: Int = 0
    fun bump(delta: Int) {
        value = value + delta
    }
}
";

#[test]
fn kotlin_methods_are_named_type_colon_colon_method() {
    let module = lower_source(Language::Kotlin, KOTLIN_METHOD_NAMING, "Box.kt").expect("lowers");
    assert!(
        module.functions.iter().any(|f| f.id.name == "Box::bump"),
        "found: {:?}",
        module
            .functions
            .iter()
            .map(|f| &f.id.name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn kotlin_class_field_belongs_to_the_module_wrapper() {
    let module = lower_source(Language::Kotlin, KOTLIN_METHOD_NAMING, "Box.kt").expect("lowers");
    let m = function(&module, "<module>");
    assert!(
        m.nodes.iter().any(|n| n.label.starts_with("var value")),
        "the class field's initializer must be lowered as a <module> statement: {:?}",
        m.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
    );
}

const KOTLIN_LOOP: &str = "
class Summer {
    fun sumPositive(xs: List<Int>): Int {
        var total = 0
        for (x in xs) {
            if (x < 0) continue
            total += x
        }
        return total
    }
}
";

#[test]
fn kotlin_for_loop_has_loop_carried_defs_and_bare_continue_reaches_the_branch() {
    let module = lower_source(Language::Kotlin, KOTLIN_LOOP, "Summer.kt").expect("lowers");
    let f = function(&module, "Summer::sumPositive");
    f.validate().expect("valid graph");

    let (branch_id, branch) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("for (x in"))
        .expect("the for-loop's iterator-advance Branch node");
    assert!(
        !branch.defs.is_empty(),
        "the loop variable `x` must be a def"
    );

    let body = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total += x"))
        .expect("loop body statement");
    assert!(body.succs.contains(&branch_id));

    // Kotlin's grammar tokenizes a bare `continue` as a plain `identifier`,
    // not a dedicated node kind - this is the one behavior the whole test
    // exists to pin down (see `GrammarTable::continue_texts`).
    let cont = f
        .nodes
        .iter()
        .find(|n| n.label == "continue")
        .expect("the continue node, lowered from what parses as a bare identifier");
    assert!(
        cont.succs.contains(&branch_id),
        "continue must edge to the loop's Branch node, not fall through to total += x"
    );
}

const KOTLIN_CALLS: &str = "
class Calc {
    fun compute(x: Int): Int {
        val doubled = scale(x)
        val obj = Builder()
        return obj.build(doubled)
    }
}
";

#[test]
fn kotlin_extracts_free_constructor_and_method_call_targets() {
    let module = lower_source(Language::Kotlin, KOTLIN_CALLS, "Calc.kt").expect("lowers");
    let f = function(&module, "Calc::compute");
    f.validate().expect("valid graph");

    let names = callees(f);
    assert!(names.contains(&"scale"), "free call target: {names:?}");
    assert!(
        names.contains(&"Builder"),
        "constructor-style call target: {names:?}"
    );
    assert!(
        names.contains(&"obj.build"),
        "method call target with receiver: {names:?}"
    );
}

const KOTLIN_STATE_WRITE: &str = "
class Counter {
    fun bump(counter: Counter) {
        counter.value = counter.value + 1
    }
}
";

#[test]
fn kotlin_property_assignment_is_a_state_write() {
    let module = lower_source(Language::Kotlin, KOTLIN_STATE_WRITE, "Counter.kt").expect("lowers");
    let f = function(&module, "Counter::bump");
    f.validate().expect("valid graph");

    let write = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::StateWrite { .. }))
        .expect("a StateWrite node for the property assignment");
    let NodeKind::StateWrite { target } = &write.kind else {
        unreachable!()
    };
    assert_eq!(target, "counter.value");
    assert!(write.kind.is_effect());
}

const KOTLIN_EXPR_BODY: &str = "
class RateTable {
    fun rateFor(x: Int): Double = compute(x)
}
";

#[test]
fn kotlin_expression_body_lowers_to_a_real_return_node() {
    let module = lower_source(Language::Kotlin, KOTLIN_EXPR_BODY, "RateTable.kt").expect("lowers");
    let f = function(&module, "RateTable::rateFor");
    f.validate().expect("valid graph");

    let ret = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("expression body must still produce a Return node");
    assert!(ret.succs.is_empty());
    assert!(callees(f).contains(&"compute"));
}

const KOTLIN_WHEN_RETURN_IN_ARM: &str = "
class Classify {
    fun classify(x: Int): Int {
        when (x) {
            0 -> {
                return -1
            }
            1, 2 -> {
                logHit(x)
                return x
            }
            else -> return 0
        }
        return -99
    }
}
";

#[test]
fn kotlin_when_models_branch_fanout_and_returns_inside_arms() {
    let module =
        lower_source(Language::Kotlin, KOTLIN_WHEN_RETURN_IN_ARM, "Classify.kt").expect("lowers");
    let f = function(&module, "Classify::classify");
    f.validate().expect("valid graph");

    let (disc_id, disc) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("when (x)"))
        .expect("the discriminant Branch node");
    assert_eq!(
        disc.succs.len(),
        3,
        "one head per when_entry: {:?}",
        disc.succs
    );

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(
        returns.len(),
        4,
        "the three arms' returns, plus the trailing `return -99` (unreachable, since every \
         arm returns, but still a real lowered node): {:?}",
        kinds(f)
    );
    assert!(returns.iter().all(|n| n.succs.is_empty()));

    let hit_call = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "logHit"))
        .expect("logHit(x) call inside the `1, 2` arm");
    assert!(!f.nodes[disc_id].succs.contains(&hit_call));
}

const KOTLIN_TRY_CATCH: &str = "
class Risky {
    fun f(x: Int): Int {
        try {
            risky(x)
        } catch (e: IllegalArgumentException) {
            log(e)
            return -1
        }
        return 0
    }
}
";

#[test]
fn kotlin_try_models_handler_binding_and_visible_return_but_stays_unreachable() {
    let module = lower_source(Language::Kotlin, KOTLIN_TRY_CATCH, "Risky.kt").expect("lowers");
    let f = function(&module, "Risky::f");
    f.validate().expect("valid graph");

    let (catch_id, catch_entry) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.label == "<catch>")
        .expect("the handler's entry node");
    assert_eq!(catch_entry.defs.len(), 1, "the caught `e` must be declared");
    assert!(
        !f.nodes.iter().any(|n| n.succs.contains(&catch_id)),
        "the handler must never be linked in from the try body's normal flow"
    );

    let handler_return = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("the handler's own return must be a real Return node");
    assert!(handler_return.succs.is_empty());

    let log_call = f
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "log"))
        .expect("log(e) call inside the handler");
    assert!(log_call.uses.contains(&catch_entry.defs[0]));
}

const KOTLIN_CLOSURE: &str = "
class Outer {
    fun outer(base: Int): Int {
        val helper = { x: Int ->
            val z = x + base
            z
        }
        return helper(5)
    }
}
";

#[test]
fn kotlin_lambda_becomes_its_own_brace_qualified_function_with_bare_body() {
    let module = lower_source(Language::Kotlin, KOTLIN_CLOSURE, "Outer.kt").expect("lowers");
    let lam = module
        .functions
        .iter()
        .find(|f| f.id.name.contains("{lambda#"))
        .expect("the lambda must lower as its own FunctionIr");
    assert_eq!(lam.id.name, "Outer::outer::{lambda#0}");
    lam.validate().expect("valid graph");

    // Body statements sit directly in the lambda with no wrapping block,
    // but must still lower in sequence: `z`'s declaration must precede its
    // use in the tail.
    let z_decl = lam
        .nodes
        .iter()
        .find(|n| n.label.starts_with("val z"))
        .expect("val z");
    let tail = lam.nodes.iter().find(|n| n.label == "z").expect("tail z");
    assert!(tail.uses.contains(&z_decl.defs[0]));

    let outer = function(&module, "Outer::outer");
    let val_helper = outer
        .nodes
        .iter()
        .find(|n| n.label.starts_with("val helper"))
        .expect("val helper node");
    assert!(
        val_helper.uses.is_empty(),
        "the lambda body must not leak identifiers into the enclosing statement: {:?}",
        val_helper.uses
    );
}

#[test]
fn kotlin_lowering_is_deterministic() {
    let a = lower_source(Language::Kotlin, KOTLIN_LOOP, "Summer.kt").expect("lowers");
    let b = lower_source(Language::Kotlin, KOTLIN_LOOP, "Summer.kt").expect("lowers");
    for (fa, fb) in a.functions.iter().zip(b.functions.iter()) {
        assert_eq!(fa.nodes.len(), fb.nodes.len());
        for (na, nb) in fa.nodes.iter().zip(fb.nodes.iter()) {
            assert_eq!(na.defs, nb.defs);
            assert_eq!(na.uses, nb.uses);
            assert_eq!(na.succs, nb.succs);
        }
    }
}

// ------------------------------------------------------------------ Go ----

const GO_SHADOWING: &str = r"
func shadow() int {
	x := 1
	{
		x := x + 1
		useX(x)
	}
	return x
}
";

#[test]
fn go_short_var_decl_shadows_with_a_fresh_var_in_a_nested_block() {
    let module = lower_source(Language::Go, GO_SHADOWING, "shadow.go").expect("lowers");
    let f = function(&module, "shadow");
    f.validate().expect("valid graph");

    let outer_def = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("x := 1"))
        .and_then(|n| n.defs.first().copied())
        .expect("outer x := 1");
    let inner_def = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("x := x + 1"))
        .and_then(|n| n.defs.first().copied())
        .expect("inner x := x + 1");
    assert_ne!(
        outer_def, inner_def,
        "a nested block's `:=` must shadow with a fresh VarId, not reuse the outer one - Go is \
         block-scoped like Rust/Java/Kotlin, not flat like Python"
    );

    let inner_rhs_uses = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("x := x + 1"))
        .map(|n| n.uses.clone())
        .expect("inner x := x + 1 node");
    assert!(
        inner_rhs_uses.contains(&outer_def),
        "the inner `:=`'s RHS reads the outer x, evaluated before the new binding exists"
    );

    let tail_uses = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .map(|n| n.uses.clone())
        .expect("the return node");
    assert!(
        tail_uses.contains(&outer_def) && !tail_uses.contains(&inner_def),
        "the function's `return x` refers to the outer binding, popped back into scope once the \
         nested block ends, not the shadowed inner one"
    );
}

const GO_METHOD_RECEIVER: &str = r"
type Counter struct {
	total int
}

func (c *Counter) Add(delta int) int {
	c.total = c.total + delta
	return c.total
}

func (c Counter) Get() int {
	return c.total
}
";

#[test]
fn go_methods_are_named_type_colon_colon_method_from_their_own_receiver_field() {
    // Go has no class body to nest a method inside - the owner comes from
    // the method's own `receiver` field instead (`walk::receiver_owner`),
    // pointer and value receivers naming the identical owner.
    let module = lower_source(Language::Go, GO_METHOD_RECEIVER, "counter.go").expect("lowers");
    let names: Vec<&str> = module
        .functions
        .iter()
        .map(|f| f.id.name.as_str())
        .collect();
    assert!(
        names.contains(&"Counter::Add"),
        "pointer receiver `(c *Counter)` must still name the owner `Counter`: {names:?}"
    );
    assert!(
        names.contains(&"Counter::Get"),
        "value receiver `(c Counter)` must name the owner `Counter`: {names:?}"
    );

    let add = function(&module, "Counter::Add");
    add.validate().expect("valid graph");
    let write = add
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::StateWrite { .. }))
        .expect("a StateWrite node for the field assignment through the receiver");
    let NodeKind::StateWrite { target } = &write.kind else {
        unreachable!()
    };
    assert_eq!(target, "c.total");
}

const GO_LOOP: &str = r"
func sumPositive(xs []int) int {
	total := 0
	for _, x := range xs {
		if x < 0 {
			continue
		}
		total += x
	}
	return total
}
";

#[test]
fn go_range_for_has_loop_carried_defs_and_a_back_edge() {
    let module = lower_source(Language::Go, GO_LOOP, "sum.go").expect("lowers");
    let f = function(&module, "sumPositive");
    f.validate().expect("valid graph");

    let (branch_id, branch) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("for _, x"))
        .expect("the range-for's iterator-advance Branch node");
    assert!(
        !branch.defs.is_empty(),
        "the range variable `x` must be a def on the Branch node"
    );

    let body = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total += x"))
        .expect("loop body statement");
    assert!(
        body.succs.contains(&branch_id),
        "the loop body must edge back to the range-for's Branch node"
    );

    let cont = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("continue"))
        .expect("the continue node");
    assert!(
        cont.succs.contains(&branch_id),
        "continue must edge to the loop's Branch node, not fall through to total += x"
    );
}

const GO_INFINITE_LOOP: &str = r"
func findFirst(limit int) int {
	i := 0
	for {
		if i >= limit {
			return -1
		}
		i++
		if scan(i) {
			return i
		}
	}
}
";

#[test]
fn go_bare_for_with_no_clause_models_an_unconditional_loop() {
    // `for { .. }` shares its grammar kind with the range-for and
    // three-clause for above - only one named child (the body), no
    // condition at all - `lower_go_for` must route it to the same
    // `lower_loop` treatment Rust's bare `loop { .. }` gets.
    let module = lower_source(Language::Go, GO_INFINITE_LOOP, "find.go").expect("lowers");
    let f = function(&module, "findFirst");
    f.validate().expect("valid graph");

    let returns: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Return))
        .collect();
    assert_eq!(
        returns.len(),
        2,
        "both `return`s inside `for {{ .. }}` must be modeled as Return nodes: {:?}",
        kinds(f)
    );
    assert!(returns.iter().all(|n| n.succs.is_empty()));

    let (branch_id, _) = f
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| matches!(n.kind, NodeKind::Branch) && n.label.starts_with("for {"))
        .expect("the loop's head Branch node");
    // `i++` is not the last statement in the body (a second `if` follows
    // it), so its own successor is that `if`'s condition, not the head
    // directly - the real back edge comes off the second `if`'s false
    // branch instead. Assert the back edge exists at all, from wherever it
    // actually originates, rather than pinning a specific node to it.
    assert!(
        f.nodes
            .iter()
            .enumerate()
            .any(|(i, n)| i != branch_id && n.succs.contains(&branch_id)),
        "some node inside the loop body must edge back to the head node: {:?}",
        kinds(f)
    );
}

const GO_DEFER_AND_GOROUTINE: &str = r"
func dispatch(total int) {
	defer cleanup()
	go worker(total)
	if total < 0 {
		panic(total)
	}
}
";

#[test]
fn go_defer_and_go_statements_still_extract_their_inner_call() {
    // Neither `defer_statement` nor `go_statement` is itself a `call_kinds`
    // node - both wrap exactly one call - so they fall through to the
    // generic construct fallback, which still extracts the wrapped call as
    // an ordinary `Call` node ahead of the statement's own opaque unit (see
    // the `GO` table's doc comment on the deliberately absent concurrency
    // modeling for `go`). `panic` is likewise an ordinary call - Go has no
    // exception node kind at all.
    let module = lower_source(Language::Go, GO_DEFER_AND_GOROUTINE, "dispatch.go").expect("lowers");
    let f = function(&module, "dispatch");
    f.validate().expect("valid graph");

    let names = callees(f);
    assert!(
        names.contains(&"cleanup"),
        "the deferred call must still be extracted as its own Call node: {names:?}"
    );
    assert!(
        names.contains(&"worker"),
        "the launched goroutine's call must still be extracted as its own Call node: {names:?}"
    );
    assert!(
        names.contains(&"panic"),
        "panic is an ordinary call, not a dedicated throw-like construct: {names:?}"
    );

    // The extracted call precedes the statement's own opaque unit - i.e. it
    // is reachable on the way to it, not orphaned off to one side.
    let cleanup_call = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "cleanup"))
        .expect("cleanup call node");
    let defer_stmt = f
        .nodes
        .iter()
        .position(|n| n.label.starts_with("defer cleanup"))
        .expect("the defer statement's own opaque unit");
    assert!(
        f.nodes[cleanup_call].succs.contains(&defer_stmt),
        "the extracted call must lead into the defer statement's own unit"
    );

    let worker_call = f
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, NodeKind::Call { callee, .. } if callee == "worker"))
        .expect("worker call node");
    let go_stmt = f
        .nodes
        .iter()
        .position(|n| n.label.starts_with("go worker"))
        .expect("the go statement's own opaque unit");
    assert!(
        f.nodes[worker_call].succs.contains(&go_stmt),
        "the extracted call must lead into the go statement's own unit"
    );
}

const GO_VAR_CONST: &str = r"
func multiSpec() int {
	var total int = 10
	const (
		zero = 0
		one  = 1
	)
	return total + one
}
";

#[test]
fn go_var_and_const_blocks_splice_one_node_per_spec_in_the_caller_scope() {
    // `var_declaration`/`const_declaration` are `flatten_kinds`, not
    // `binding_kinds` themselves: each `var_spec`/`const_spec` becomes its
    // own statement in the *same* scope as its neighbours - no transient
    // block scope that would pop the declared names back out of view.
    let module = lower_source(Language::Go, GO_VAR_CONST, "multi.go").expect("lowers");
    let f = function(&module, "multiSpec");
    f.validate().expect("valid graph");

    let total_decl = f
        .nodes
        .iter()
        .find(|n| n.label.starts_with("total int = 10"))
        .expect("var total int = 10");
    let one_decl = f
        .nodes
        .iter()
        .find(|n| n.label == "one  = 1")
        .expect("const one = 1, its own node distinct from zero = 0");

    let tail = f
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Return))
        .expect("return total + one");
    assert!(
        tail.uses.contains(&total_decl.defs[0]) && tail.uses.contains(&one_decl.defs[0]),
        "both `total` (var) and `one` (const, from inside the parenthesized block) must still \
         resolve to their declarations, not vanish with a popped scope: {:?}",
        tail.uses
    );
}

const GO_CALLS: &str = r"
func compute(x int) int {
	doubled := scale(x)
	obj := &Builder{}
	return obj.Build(doubled)
}
";

#[test]
fn go_extracts_free_and_selector_call_targets() {
    let module = lower_source(Language::Go, GO_CALLS, "compute.go").expect("lowers");
    let f = function(&module, "compute");
    f.validate().expect("valid graph");

    let names = callees(f);
    assert!(names.contains(&"scale"), "free call target: {names:?}");
    assert!(
        names.contains(&"obj.Build"),
        "selector call target with receiver: {names:?}"
    );
}

#[test]
fn go_lowering_is_deterministic() {
    let a = lower_source(Language::Go, GO_LOOP, "sum.go").expect("lowers");
    let b = lower_source(Language::Go, GO_LOOP, "sum.go").expect("lowers");
    assert_eq!(
        kinds(function(&a, "sumPositive")),
        kinds(function(&b, "sumPositive"))
    );
    for (fa, fb) in a.functions.iter().zip(b.functions.iter()) {
        assert_eq!(fa.nodes.len(), fb.nodes.len());
        for (na, nb) in fa.nodes.iter().zip(fb.nodes.iter()) {
            assert_eq!(na.defs, nb.defs);
            assert_eq!(na.uses, nb.uses);
            assert_eq!(na.succs, nb.succs);
            assert_eq!(na.label, nb.label);
        }
    }
}
