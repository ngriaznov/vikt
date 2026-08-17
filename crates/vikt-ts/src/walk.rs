//! The generic walker: one statement-granular lowering, driven entirely by a
//! [`GrammarTable`].
//!
//! # Granularity
//!
//! One node per statement, mirroring `vikt-js`'s choice for the same reason:
//! tree-sitter exposes source syntax, and for every language this frontend
//! covers, source control flow *is* program control flow (no separate
//! compiler reshaping it the way JVM/Kotlin bytecode does). Calls are
//! extracted ahead of the statement that contains them, each as its own
//! `Call` node with a synthetic result variable, so a denylisted call can be
//! swept out of the graph independently of its containing statement.
//!
//! # Def/use
//!
//! There is no symbol table here - just node-kind pattern matching - so
//! def/use comes from a shadowing-aware name-based scope stack instead of
//! true binding resolution. A `block_scoped` language (Rust, Java, Kotlin)
//! pushes a fresh scope per `block`, so `let x`/a local declaration in a
//! nested block never clobbers an outer `x`; a non-block-scoped language
//! (Python) keeps one scope for the whole function, matching real Python
//! scoping (only `def` introduces scope). Names that resolve to nothing in
//! scope become per-name ambient variables, shared for the whole function -
//! the same "unresolved reference still tracks a flow" treatment `vikt-js`
//! gives ambient JS globals.
//!
//! # Classes
//!
//! A `class_kinds` node (Java/Kotlin only) isn't its own `FunctionIr`: its
//! methods are collected and named `Type::method`, matching the JVM
//! frontend's naming shape (see `owner_prefix`), and its direct field/
//! property declarations are lowered as ordinary statements into the
//! `<module>` wrapper, at module scope - not nested inside any per-class
//! scope, since this frontend does not model class-level shadowing.
//!
//! # Unfielded grammars
//!
//! `tree-sitter-rust`, `-python` and `-java` field essentially everything
//! this walker reads. `tree-sitter-kotlin-ng` doesn't: `call_expression`,
//! `navigation_expression`, `for_statement`, `property_declaration` and
//! `if_expression`'s then/else are unfielded on their parent (verified
//! against the crate's own `node-types.json`). Every slot that can be
//! affected takes both a field name and a kind-search or positional
//! fallback in [`GrammarTable`]; the `field_or_*` helpers below try the
//! field first and only fall back when it's empty or absent, so Rust/
//! Python/Java - whose fields always resolve - never take the fallback
//! path.
//!
//! # Match
//!
//! `match`/`switch`/`when`, when the construct is itself the statement being
//! lowered (not nested inside a `return`/`let`/assignment - lowering only
//! ever dispatches a whole statement at a time, and those stay one opaque
//! unit exactly as before, extracting inner calls but not seeing inner
//! control flow). [`FnCtx::lower_match`] turns the discriminant into a
//! `Branch` whose successors are every arm's own head node, each arm
//! [`FnCtx::lower_match_arm`] lowers through the ordinary `lower_stmt`/
//! `lower_seq` machinery - own scope for its pattern bindings (a bound name
//! never leaks to a sibling arm), real `Return`/`Throw`/`Call`/`Branch`
//! nodes for anything inside it, joining back together after every arm.
//! Arms are always mutually exclusive; there is no fallthrough modeling even
//! for Java's colon-form `switch` (a deliberate v2 simplification - the
//! table only records enough to fan out to each arm's head, not to chain
//! one arm's fall-through into the next's).  When no arm is a wildcard/
//! default, the discriminant itself stays a live exit, the same
//! conservative "might not match anything" treatment `lower_if_chain` gives
//! an `if` with no final `else`.
//!
//! # Try
//!
//! `try`/`except`/`catch`/`finally` (empty for Rust: no such construct).
//! [`FnCtx::lower_try`] lowers the `try` body as ordinary code and every
//! handler too - a `return`/`throw`/call inside a handler is a real graph
//! node - but never links a handler in from the try body. Mirrors a
//! *measured* decision in `vikt-js`: modeling the exception edge itself
//! drags every scored line toward error paths (agreement with expert labels
//! dropped when tried there). `finally` (and Python's `else`) runs once,
//! sequentially after the try body's own exits - not duplicated along every
//! exit path.
//!
//! # Closures
//!
//! A `closure_kinds` node (Rust closures, Python lambdas, Java lambdas,
//! Kotlin lambda literals - a language's already-`function_kinds` nested
//! `fn`/`def` needed no new handling, `collect_functions` already recursed
//! into nested functions) becomes its own [`FunctionIr`], named via
//! `closure_name_format` instead of a source-given name - see
//! `GrammarTable::closure_name_format`'s docs and [`closure_name`]. A
//! captured outer name is never folded into the *enclosing* unit's uses the
//! way `vikt-js`'s `Facts::captures` does; it simply resolves as an ambient
//! read inside the closure's own body, in the closure's own `FunctionIr` -
//! a deliberately smaller v2 model. What the enclosing unit's scan does
//! guarantee: neither a closure's captured reads nor any call inside it
//! leak into the *enclosing* statement's own uses/calls - `collect_
//! identifier_texts`/`collect_call_nodes_into` both stop at a
//! `closure_kinds` boundary exactly like a `function_kinds` one.
//!
//! # Known v2 simplifications
//!
//! - Loop labels are not modeled: `break`/`continue` always target the
//!   innermost loop.
//! - Java's three-clause `for` and Kotlin's `do`/`while` fall back to the
//!   generic unit above; only the for-each/while forms are modeled.
//! - Java's `object_creation_expression` (`new Foo(...)`) is not extracted
//!   as its own `Call` node - see the comment on `JAVA.call_kinds`.
//! - Call-use accounting is deliberately coarser than `vikt-js`'s: rather
//!   than folding a nested call's temp into exactly the "direct" enclosing
//!   unit, every unit's `uses` includes every identifier in its span
//!   (member/attribute names and nested-function bodies excluded), even
//!   ones that also appear as a nested call's own use. Redundant, never
//!   wrong.
//! - A match/try/closure nested inside a `return`, a binding's value, or an
//!   assignment's right-hand side is not specially modeled: the whole
//!   containing statement stays one opaque unit (inner calls are still
//!   extracted; inner control flow is not) - only when one of these three
//!   constructs is itself the statement does `lower_stmt` dispatch into it.

use std::collections::BTreeMap;

use tree_sitter::Node;
use vikt_core::ir::{CallOpacity, FunctionId, FunctionIr, Node as IrNode, NodeKind, VarId};

use crate::Diagnostic;
use crate::grammar::GrammarTable;

/// Counts every named descendant of `node` (inclusive) and how many of those
/// are `ERROR`/`MISSING` nodes, used to decide whether a file is parseable
/// enough to lower at all.
pub(crate) fn error_stats(node: Node<'_>) -> (usize, usize) {
    let mut total = 0usize;
    let mut bad = 0usize;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        total += 1;
        if n.is_error() || n.is_missing() {
            bad += 1;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    (total, bad)
}

/// Every function-like node anywhere in the tree, module-level and nested
/// alike, in stable document order - paired with whether it is a
/// `closure_kinds` node (anonymous, synthetically named) rather than a
/// `function_kinds` one (source-named). Pre-order: a node is pushed before
/// its own children are visited, so an enclosing function or closure always
/// precedes anything nested inside it - `lower_module` relies on that to
/// look up an already-assigned owner name while naming a nested closure.
fn collect_functions<'t>(
    table: &'static GrammarTable,
    node: Node<'t>,
    out: &mut Vec<(Node<'t>, bool)>,
) {
    if table.function_kinds.contains(&node.kind()) {
        out.push((node, false));
    } else if table.closure_kinds.contains(&node.kind()) {
        out.push((node, true));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(table, child, out);
    }
}

/// Whether `kind` is a `function_kinds` or `closure_kinds` node - the
/// boundary a call/identifier scan never crosses, since both become their
/// own `FunctionIr`.
fn is_function_like(table: &GrammarTable, kind: &str) -> bool {
    table.function_kinds.contains(&kind) || table.closure_kinds.contains(&kind)
}

/// The nearest enclosing `function_kinds`/`closure_kinds` ancestor node -
/// used both to key a closure's per-owner numbering and to look up that
/// owner's already-assigned name (see `collect_functions`'s doc on why the
/// lookup is always safe).
fn enclosing_fn_node<'t>(table: &'static GrammarTable, node: Node<'t>) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if is_function_like(table, n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// A closure's synthetic name: `table.closure_name_format` with `{owner}`
/// and `{idx}` substituted - see the field's docs. `counts` is keyed by the
/// enclosing function/closure node's id (or `usize::MAX` for a closure with
/// no enclosing function at all, e.g. a top-level `<module>` initializer),
/// giving each owner its own 0-based counter in appearance order.
fn closure_name(
    table: &'static GrammarTable,
    fnode: Node<'_>,
    source: &str,
    names: &BTreeMap<usize, String>,
    counts: &mut BTreeMap<usize, u32>,
) -> String {
    let scope = enclosing_fn_node(table, fnode);
    let owner = scope
        .and_then(|n| names.get(&n.id()).cloned())
        .or_else(|| owner_prefix(table, fnode, source))
        .unwrap_or_else(|| "<module>".to_owned());
    let key = scope.map_or(usize::MAX, |n| n.id());
    let counter = counts.entry(key).or_insert(0);
    let idx = *counter;
    *counter += 1;
    table
        .closure_name_format
        .replace("{idx}", &idx.to_string())
        .replace("{owner}", &owner)
}

/// The nearest enclosing `class_kinds` ancestor's name, for `Type::method`
/// naming matching the JVM frontend's shape. `None` for a language with no
/// `class_kinds` (Rust, Python) or a function with no enclosing type
/// (Kotlin's top-level `fun`).
fn owner_prefix(table: &'static GrammarTable, fnode: Node<'_>, source: &str) -> Option<String> {
    let mut cur = fnode.parent();
    while let Some(n) = cur {
        if table.class_kinds.contains(&n.kind()) {
            return n
                .child_by_field_name(table.class_name_field)
                .and_then(|nm| nm.utf8_text(source.as_bytes()).ok())
                .map(str::to_owned);
        }
        cur = n.parent();
    }
    None
}

/// `child_by_field_name`, falling back to the first named child of `kind`
/// when the field is unused (empty) or absent - Kotlin doesn't field
/// `function_body`, `function_value_parameters` or `class_body` on their
/// owning nodes.
fn field_or_kind<'t>(node: Node<'t>, field: &str, kind: &str) -> Option<Node<'t>> {
    if !field.is_empty()
        && let Some(n) = node.child_by_field_name(field)
    {
        return Some(n);
    }
    first_child_of_kind(node, kind)
}

fn first_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if kind.is_empty() {
        return None;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

/// `child_by_field_name`, falling back to the `n`th named child - Kotlin's
/// `if_expression` fields only `condition`; then/else are just the next
/// named children in order.
fn field_or_nth<'t>(node: Node<'t>, field: &str, n: usize) -> Option<Node<'t>> {
    if !field.is_empty()
        && let Some(x) = node.child_by_field_name(field)
    {
        return Some(x);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).nth(n)
}

/// `child_by_field_name`, falling back to the last named child - the body
/// position in a language that doesn't field it (Kotlin's `while_statement`/
/// `for_statement`) is reliably the final child.
fn field_or_last<'t>(node: Node<'t>, field: &str) -> Option<Node<'t>> {
    if !field.is_empty()
        && let Some(x) = node.child_by_field_name(field)
    {
        return Some(x);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).last()
}

/// Resolves a member-access node's object/property children, falling back
/// to positional (first/last named child) when the grammar doesn't field
/// them at all - Kotlin's `navigation_expression`.
fn member_parts<'t>(table: &GrammarTable, node: Node<'t>) -> (Option<Node<'t>>, Option<Node<'t>>) {
    let object = (!table.member_object_field.is_empty())
        .then(|| node.child_by_field_name(table.member_object_field))
        .flatten();
    let property = (!table.member_property_field.is_empty())
        .then(|| node.child_by_field_name(table.member_property_field))
        .flatten();
    if object.is_some() && property.is_some() {
        return (object, property);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
    (children.first().copied(), children.last().copied())
}

/// Lowers a whole parsed tree into one [`FunctionIr`] per function plus a
/// `<module>` wrapper for top-level statements, mirroring `vikt-js`'s shape.
pub(crate) fn lower_module(
    table: &'static GrammarTable,
    root: Node<'_>,
    source: &str,
    file: &str,
) -> (Vec<FunctionIr>, Vec<Diagnostic>) {
    let mut fn_nodes = Vec::new();
    collect_functions(table, root, &mut fn_nodes);

    let mut functions = Vec::new();
    let mut diagnostics = Vec::new();

    let mut module = FnCtx::new(table, source, file, "<module>", 1);
    module.lower_params(None);
    let _ = module.lower_block_children(root, vec![0]);
    let (module_ir, module_diags) = module.finish();
    functions.push(module_ir);
    diagnostics.extend(module_diags);

    // Names are assigned in the same pre-order pass that lowers each
    // function/closure, and recorded here as they go - a closure's name
    // formula reads its enclosing function's name back out of this map (see
    // `closure_name`), which is always already present by the time a nested
    // node is reached (`collect_functions`'s pre-order guarantee).
    let mut names: BTreeMap<usize, String> = BTreeMap::new();
    let mut closure_counts: BTreeMap<usize, u32> = BTreeMap::new();

    for (fnode, is_closure) in fn_nodes {
        let name = if is_closure {
            closure_name(table, fnode, source, &names, &mut closure_counts)
        } else {
            let bare_name = fnode
                .child_by_field_name(table.function_name_field)
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("<anon>");
            owner_prefix(table, fnode, source).map_or_else(
                || bare_name.to_owned(),
                |owner| format!("{owner}::{bare_name}"),
            )
        };
        names.insert(fnode.id(), name.clone());
        let decl_line = line_of(fnode);

        // A parse error anywhere in the body (has_error is recursive, so
        // this also catches errors in a nested function inside it) degrades
        // this function to a stub rather than risk lowering garbage syntax
        // into a graph that looks legitimate.
        if fnode.has_error() {
            diagnostics.push(Diagnostic {
                function: name.clone(),
                message: "body contains a parse error; lowered as an opaque stub".into(),
            });
            functions.push(FunctionIr {
                id: FunctionId {
                    file: file.to_owned(),
                    name,
                    signature: String::new(),
                    decl_line: Some(decl_line),
                },
                nodes: vec![IrNode::pure(decl_line)],
                entry: 0,
            });
            continue;
        }

        let (params, raw_body, bare_children) = if is_closure {
            (
                field_or_kind(fnode, table.closure_params_field, table.closure_params_kind),
                field_or_kind(fnode, table.closure_body_field, table.closure_body_kind),
                table.closure_body_is_bare_children,
            )
        } else {
            (
                field_or_kind(
                    fnode,
                    table.function_params_field,
                    table.function_params_kind,
                ),
                field_or_kind(fnode, table.function_body_field, table.function_body_kind),
                false,
            )
        };
        let mut lowerer = FnCtx::new(table, source, file, &name, decl_line);
        lowerer.lower_params(params);
        if bare_children {
            // Kotlin's `lambda_literal`: no body wrapper to find by field
            // or kind at all - every named child except the parameter list
            // is a statement in the body, run in sequence.
            let param_id = params.map(|p| p.id());
            let mut cursor = fnode.walk();
            let body_nodes: Vec<Node<'_>> = fnode
                .named_children(&mut cursor)
                .filter(|c| Some(c.id()) != param_id)
                .collect();
            let _ = lowerer.lower_seq(body_nodes.into_iter(), vec![0]);
        } else if let Some(raw) = raw_body {
            let body = lowerer.unwrap_wrapper(raw);
            if table.block_kinds.contains(&body.kind()) {
                let _ = lowerer.lower_block_children(body, vec![0]);
            } else {
                // Expression body (Kotlin `fun f() = expr`, a Python
                // lambda's implicit `return`): the tail expression's value
                // is what the function returns, modeled the same way
                // `vikt-js` models an arrow's expr-body.
                let (entry, _) = lowerer.unit(body, NodeKind::Return, vec![]);
                lowerer.link(&[0], entry);
            }
        }
        let (fn_ir, fn_diags) = lowerer.finish();
        functions.push(fn_ir);
        diagnostics.extend(fn_diags);
    }

    (functions, diagnostics)
}

fn line_of(node: Node<'_>) -> u32 {
    u32::try_from(node.start_position().row)
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

/// First line of `node`'s text, truncated to 48 chars at a char boundary.
fn snippet(node: Node<'_>, source: &str) -> String {
    let text = node.utf8_text(source.as_bytes()).unwrap_or_default();
    let line = text.split('\n').next().unwrap_or_default().trim();
    match line.char_indices().nth(48) {
        Some((idx, _)) => line[..idx].to_owned(),
        None => line.to_owned(),
    }
}

/// Dotted callee text for a call target: `helper`, `obj.compute`. Falls back
/// to a snippet for a callee that is neither a bare name nor a member chain
/// (e.g. an immediately-invoked expression).
fn callee_text(table: &'static GrammarTable, node: Node<'_>, source: &str) -> String {
    fn chain(
        table: &'static GrammarTable,
        node: Node<'_>,
        source: &str,
        out: &mut Vec<String>,
    ) -> bool {
        if table.identifier_kinds.contains(&node.kind()) {
            out.push(
                node.utf8_text(source.as_bytes())
                    .unwrap_or_default()
                    .to_owned(),
            );
            return true;
        }
        if table.member_access_kinds.contains(&node.kind()) {
            let (object, property) = member_parts(table, node);
            let object_ok = object.is_some_and(|o| chain(table, o, source, out));
            if let Some(p) = property {
                out.push(
                    p.utf8_text(source.as_bytes())
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            return object_ok;
        }
        false
    }
    let mut parts = Vec::new();
    if chain(table, node, source, &mut parts) && !parts.is_empty() {
        parts.join(".")
    } else {
        snippet(node, source)
    }
}

/// Resolves a call node's callee text across three shapes: a single field
/// pointing to an identifier-or-member-chain (Rust/Python's `function`); two
/// paired fields naming the receiver and method separately, receiver
/// optional (Java's `method_invocation`); or no fields at all, callee is
/// simply the first named child (Kotlin's `call_expression`).
fn call_callee(table: &'static GrammarTable, node: Node<'_>, source: &str) -> String {
    if !table.call_object_field.is_empty() || !table.call_name_field.is_empty() {
        let name = (!table.call_name_field.is_empty())
            .then(|| node.child_by_field_name(table.call_name_field))
            .flatten()
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("<dynamic>");
        let object = (!table.call_object_field.is_empty())
            .then(|| node.child_by_field_name(table.call_object_field))
            .flatten();
        return match object {
            Some(obj) => format!("{}.{name}", callee_text(table, obj, source)),
            None => name.to_owned(),
        };
    }
    let callee_node = if table.call_callee_field.is_empty() {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).next()
    } else {
        node.child_by_field_name(table.call_callee_field)
    };
    callee_node.map_or_else(|| "<dynamic>".to_owned(), |c| callee_text(table, c, source))
}

/// Whether an `assign_kinds` node is a compound assignment. Only meaningful
/// for a language that reuses one node kind for both and disambiguates via
/// the operator field's text (Java, Kotlin) - always `false` when
/// `assign_operator_field` is unused, since that language instead lists a
/// dedicated `compound_assign_kinds` entry, checked separately.
fn is_compound_assign(table: &GrammarTable, node: Node<'_>, source: &str) -> bool {
    if table.assign_operator_field.is_empty() {
        return false;
    }
    node.child_by_field_name(table.assign_operator_field)
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .is_some_and(|op| op != "=")
}

/// Whether `node` is a bare identifier leaf whose text is one of `words` -
/// used only for Kotlin's `break`/`continue`, which this grammar version
/// tokenizes as plain `identifier`s rather than a dedicated node kind (see
/// `GrammarTable::break_texts`). `words` is empty for every other language,
/// so this can never misfire there.
fn is_word(table: &GrammarTable, node: Node<'_>, source: &str, words: &[&str]) -> bool {
    table.identifier_kinds.contains(&node.kind())
        && node
            .utf8_text(source.as_bytes())
            .is_ok_and(|t| words.contains(&t))
}

/// Every call-kind node in `node`'s subtree, innermost first (so a nested
/// call's own extraction precedes the call that contains it), never
/// crossing into a nested function's body.
fn collect_call_nodes<'t>(table: &'static GrammarTable, node: Node<'t>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    collect_call_nodes_into(table, node, &mut out);
    out
}

fn collect_call_nodes_into<'t>(
    table: &'static GrammarTable,
    node: Node<'t>,
    out: &mut Vec<Node<'t>>,
) {
    if is_function_like(table, node.kind()) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_call_nodes_into(table, child, out);
    }
    if table.call_kinds.contains(&node.kind()) {
        out.push(node);
    }
}

struct LoopCtx {
    /// Where a `continue` in this loop links to. Both loop kinds this
    /// frontend models (`while`, for-each) know their continue target as
    /// soon as the loop's own condition/iterator node is pushed, so unlike
    /// `vikt-js`'s C-style `for`, nothing here needs deferred patching.
    continue_target: usize,
    breaks: Vec<usize>,
}

/// Per-function lowering state.
struct FnCtx<'t> {
    table: &'static GrammarTable,
    source: &'t str,
    file: String,
    name: String,
    decl_line: u32,
    nodes: Vec<IrNode>,
    /// Innermost scope last. Non-block-scoped languages never push past the
    /// one function-root scope created in `new`.
    scopes: Vec<BTreeMap<String, VarId>>,
    /// Ambient/unresolved names, function-wide.
    globals: BTreeMap<String, VarId>,
    next_var: VarId,
    loops: Vec<LoopCtx>,
    /// Statement-level parse-error reports - see the `has_error` check at
    /// the top of `lower_stmt`. Distinct from `lower_module`'s whole-function
    /// `has_error` stubbing: that check guards every node reachable from a
    /// collected function, so these only ever fire for `<module>`-scope
    /// statements, which have no such guard.
    diagnostics: Vec<Diagnostic>,
}

impl<'t> FnCtx<'t> {
    fn new(
        table: &'static GrammarTable,
        source: &'t str,
        file: &str,
        name: &str,
        decl_line: u32,
    ) -> Self {
        Self {
            table,
            source,
            file: file.to_owned(),
            name: name.to_owned(),
            decl_line,
            nodes: Vec::new(),
            scopes: vec![BTreeMap::new()],
            globals: BTreeMap::new(),
            next_var: 0,
            loops: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn push(&mut self, node: IrNode) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn push_diagnostic(&mut self, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            function: self.name.clone(),
            message: message.into(),
        });
    }

    fn link(&mut self, from: &[usize], to: usize) {
        for &f in from {
            if !self.nodes[f].succs.contains(&to) {
                self.nodes[f].succs.push(to);
            }
        }
    }

    fn finish(self) -> (FunctionIr, Vec<Diagnostic>) {
        let ir = FunctionIr {
            id: FunctionId {
                file: self.file,
                name: self.name,
                signature: String::new(),
                decl_line: Some(self.decl_line),
            },
            nodes: self.nodes,
            entry: 0,
        };
        (ir, self.diagnostics)
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn alloc_var(&mut self) -> VarId {
        let id = self.next_var;
        self.next_var += 1;
        id
    }

    /// Binds `name` fresh in the innermost scope (`block_scoped`: always a
    /// new id, real shadowing) or reuses whatever this function already
    /// bound it to (non-block-scoped: assignment-is-binding, same slot).
    fn declare(&mut self, name: &str) -> VarId {
        if !self.table.block_scoped
            && let Some(&id) = self.scopes.last().and_then(|s| s.get(name))
        {
            return id;
        }
        let id = self.alloc_var();
        self.scopes
            .last_mut()
            .expect("function scope always has at least one entry")
            .insert(name.to_owned(), id);
        id
    }

    /// Resolves `name` against the scope stack, innermost first, falling
    /// back to a function-wide ambient id when nothing binds it.
    fn resolve_or_global(&mut self, name: &str) -> VarId {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.get(name) {
                return id;
            }
        }
        if let Some(&id) = self.globals.get(name) {
            return id;
        }
        let id = self.alloc_var();
        self.globals.insert(name.to_owned(), id);
        id
    }

    /// Identifier leaf texts under `node`, skipping a member-access node's
    /// property/attribute position (never a variable) and any nested
    /// function's or closure's body (a closure's own captured reads are
    /// scanned separately, inside its own `FunctionIr` - see `lower_module`
    /// and the module docs' "Closures" section).
    fn collect_identifier_texts(&self, node: Node<'t>, out: &mut Vec<&'t str>) {
        self.collect_identifier_texts_excluding(node, None, out);
    }

    /// [`Self::collect_identifier_texts`], additionally never descending
    /// into the subtree rooted at `skip` (by node id) - used to pull a
    /// match arm's guard clause back out of its pattern scan when the
    /// grammar nests the guard inside the pattern node itself (Rust's
    /// `match_pattern.condition`), so the guard's reads are scanned
    /// separately as uses rather than folded into the pattern's bindings.
    fn collect_identifier_texts_excluding(
        &self,
        node: Node<'t>,
        skip: Option<usize>,
        out: &mut Vec<&'t str>,
    ) {
        if Some(node.id()) == skip {
            return;
        }
        if self.table.identifier_kinds.contains(&node.kind()) {
            if let Ok(text) = node.utf8_text(self.source.as_bytes()) {
                out.push(text);
            }
            return;
        }
        if is_function_like(self.table, node.kind()) {
            return;
        }
        let skip_id = self
            .table
            .member_access_kinds
            .contains(&node.kind())
            .then(|| member_parts(self.table, node).1.map(|n| n.id()))
            .flatten();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if skip_id == Some(child.id()) || Some(child.id()) == skip {
                continue;
            }
            self.collect_identifier_texts_excluding(child, skip, out);
        }
    }

    /// Every `binding_kinds` node's pattern names anywhere under `node`,
    /// skipping nested function bodies - used only by the generic-construct
    /// fallback (`match`/`switch`/`when`, `try`/`except`/`catch`, ...),
    /// which never recurses into `lower_binding` for statements it doesn't
    /// otherwise model. Without pre-declaring these, a pattern binding that
    /// shadows an outer name (e.g. a match arm's `let y` shadowing a
    /// parameter `y`) would resolve straight through to the outer
    /// variable's id when `scan_uses` walks the same span.
    fn collect_nested_binding_names(&self, node: Node<'t>, out: &mut Vec<&'t str>) {
        let t = self.table;
        if is_function_like(t, node.kind()) {
            return;
        }
        if t.binding_kinds.contains(&node.kind()) {
            if t.binding_declarator_field.is_empty() {
                if let (Some(pattern), _) = self.resolve_binding_pattern_value(node) {
                    self.collect_identifier_texts(pattern, out);
                }
            } else {
                let mut cursor = node.walk();
                for decl in node.children_by_field_name(t.binding_declarator_field, &mut cursor) {
                    if let Some(name) = decl.child_by_field_name(t.binding_declarator_name_field) {
                        self.collect_identifier_texts(name, out);
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect_nested_binding_names(child, out);
        }
    }

    fn scan_uses(&mut self, node: Node<'t>, out: &mut Vec<VarId>) {
        let mut names = Vec::new();
        self.collect_identifier_texts(node, &mut names);
        for name in names {
            let v = self.resolve_or_global(name);
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }

    fn scan_defs(&mut self, node: Node<'t>, declare: bool, out: &mut Vec<VarId>) {
        let mut names = Vec::new();
        self.collect_identifier_texts(node, &mut names);
        for name in names {
            let v = if declare {
                self.declare(name)
            } else {
                self.resolve_or_global(name)
            };
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }

    /// Extracts every call in `node`'s subtree as its own `Call` node,
    /// chained in extraction order, and returns their ids.
    fn extract_calls(&mut self, node: Node<'t>) -> Vec<usize> {
        let call_nodes = collect_call_nodes(self.table, node);
        let mut ids = Vec::with_capacity(call_nodes.len());
        for cn in call_nodes {
            let callee = call_callee(self.table, cn, self.source);
            let mut uses = Vec::new();
            self.scan_uses(cn, &mut uses);
            let temp = self.alloc_var();
            let line = line_of(cn);
            let id = self.push(IrNode {
                line: Some(line),
                kind: NodeKind::Call {
                    callee: callee.clone(),
                    opacity: CallOpacity::Opaque,
                },
                defs: vec![temp],
                uses,
                succs: Vec::new(),
                label: callee,
            });
            ids.push(id);
        }
        for w in ids.windows(2) {
            self.link(&[w[0]], w[1]);
        }
        ids
    }

    /// Lowers one unit: extracts calls ahead of it, computes its uses over
    /// the whole span, and returns (entry, exit) - entry is the first
    /// extracted call when there is one, exit is always this node.
    fn unit(&mut self, node: Node<'t>, kind: NodeKind, defs: Vec<VarId>) -> (usize, usize) {
        let calls = self.extract_calls(node);
        let mut uses = Vec::new();
        self.scan_uses(node, &mut uses);
        let line = line_of(node);
        let label = snippet(node, self.source);
        let id = self.push(IrNode {
            line: Some(line),
            kind,
            defs,
            uses,
            succs: Vec::new(),
            label,
        });
        if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.link(&[last], id);
            (first, id)
        } else {
            (id, id)
        }
    }

    /// Pushes a structural node for `node` with no defs/uses/calls of its
    /// own - for a leaf that only matters for its position in the CFG, not
    /// its text (Kotlin's identifier-shaped bare `break`/`continue`).
    fn push_bare(&mut self, node: Node<'t>) -> usize {
        let line = line_of(node);
        let label = snippet(node, self.source);
        self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Pure,
            defs: Vec::new(),
            uses: Vec::new(),
            succs: Vec::new(),
            label,
        })
    }

    /// Repeatedly unwraps a transparent wrapper kind to its `body` field, or
    /// its last named child when there is no such field.
    fn unwrap_wrapper(&self, mut node: Node<'t>) -> Node<'t> {
        while self.table.wrapper_kinds.contains(&node.kind()) {
            let inner = node.child_by_field_name("body").or_else(|| {
                let mut cursor = node.walk();
                node.named_children(&mut cursor).last()
            });
            match inner {
                Some(n) => node = n,
                None => break,
            }
        }
        node
    }

    fn lower_params(&mut self, params: Option<Node<'t>>) {
        let mut defs = Vec::new();
        if let Some(p) = params {
            self.scan_defs(p, true, &mut defs);
        }
        self.push(IrNode {
            line: Some(self.decl_line),
            kind: NodeKind::Pure,
            defs,
            uses: Vec::new(),
            succs: Vec::new(),
            label: "<params>".into(),
        });
    }

    fn lower_seq(
        &mut self,
        stmts: impl Iterator<Item = Node<'t>>,
        mut open: Vec<usize>,
    ) -> Vec<usize> {
        for s in stmts {
            open = self.lower_stmt(s, open);
        }
        open
    }

    /// Lowers `node`'s named children directly, without the scope push a
    /// `block_kinds` dispatch would give it - used where a caller already
    /// owns the surrounding scope (for-loop pattern bindings, the module
    /// root, a class body's fields/methods).
    fn lower_block_children(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        self.lower_seq(children.into_iter(), open)
    }

    fn lower_block(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        if self.table.block_scoped {
            self.push_scope();
        }
        let exits = self.lower_block_children(node, open);
        if self.table.block_scoped {
            self.pop_scope();
        }
        exits
    }

    /// A `class_kinds` node at statement position: not a scope of its own
    /// (see module docs), just its body's statements threaded straight into
    /// the caller's open set - fields become module-scope bindings, methods
    /// become module-scope `<fndecl>` bindings (their bodies were already
    /// collected as their own `Type::method` `FunctionIr`s).
    fn lower_class_body(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let Some(body) = field_or_kind(
            node,
            self.table.class_body_field,
            self.table.class_body_kind,
        ) else {
            return open;
        };
        self.lower_block_children(body, open)
    }

    fn take_break(&mut self, node: usize) {
        if let Some(ctx) = self.loops.last_mut() {
            ctx.breaks.push(node);
        }
    }

    fn take_continue(&mut self, node: usize) {
        if let Some(ctx) = self.loops.last() {
            let target = ctx.continue_target;
            self.link(&[node], target);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_stmt(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let node = self.unwrap_wrapper(node);
        let kind = node.kind();
        let t = self.table;
        // A per-function `has_error` stub (see `lower_module`) keeps this
        // arm from ever firing for a statement inside a collected function -
        // `has_error` is recursive, so a clean function body means none of
        // its statements can trip this either. Only a `<module>`-scope
        // statement can still carry a parse error here. Reported rather
        // than silently folded into the generic fallback below, matching
        // the per-function stub's own philosophy of degrading loudly
        // instead of lowering garbage syntax quietly.
        if node.has_error() {
            self.push_diagnostic("statement contains a parse error; lowered as an opaque unit");
        }
        if t.block_kinds.contains(&kind) {
            return self.lower_block(node, open);
        }
        if t.class_kinds.contains(&kind) {
            return self.lower_class_body(node, open);
        }
        if t.function_kinds.contains(&kind) {
            return self.lower_fn_decl(node, open);
        }
        if t.binding_kinds.contains(&kind) {
            return self.lower_binding(node, open);
        }
        if t.compound_assign_kinds.contains(&kind) {
            return self.lower_assign(node, open, true);
        }
        if t.assign_kinds.contains(&kind) {
            let compound = is_compound_assign(t, node, self.source);
            return self.lower_assign(node, open, compound);
        }
        if t.call_kinds.contains(&kind) {
            return self.lower_call_stmt(node, open);
        }
        if t.if_kinds.contains(&kind) {
            return self.lower_if(node, open);
        }
        if t.while_kinds.contains(&kind) {
            return self.lower_while(node, open);
        }
        if t.loop_kinds.contains(&kind) {
            return self.lower_loop(node, open);
        }
        if t.for_kinds.contains(&kind) {
            return self.lower_for(node, open);
        }
        if t.return_kinds.contains(&kind) {
            let (entry, _) = self.unit(node, NodeKind::Return, vec![]);
            self.link(&open, entry);
            return Vec::new();
        }
        if t.throw_kinds.contains(&kind) {
            let (entry, _) = self.unit(node, NodeKind::Throw, vec![]);
            self.link(&open, entry);
            return Vec::new();
        }
        if t.break_kinds.contains(&kind) {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            self.take_break(exit);
            return Vec::new();
        }
        if t.continue_kinds.contains(&kind) {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            self.take_continue(exit);
            return Vec::new();
        }
        if t.match_kinds.contains(&kind) {
            return self.lower_match(node, open);
        }
        if t.try_kinds.contains(&kind) {
            return self.lower_try(node, open);
        }
        // Kotlin's bare `break`/`continue` (no label) lexes as a plain
        // `identifier`, not a dedicated node kind - going through `unit`
        // here like the kind-based arms above would scan the node's own
        // text as a *use* of a variable literally named "continue", since
        // that's exactly what an identifier leaf normally means. Push a
        // bare node instead: no calls to extract, nothing to use.
        if is_word(t, node, self.source, t.break_texts) {
            let exit = self.push_bare(node);
            self.link(&open, exit);
            self.take_break(exit);
            return Vec::new();
        }
        if is_word(t, node, self.source, t.continue_texts) {
            let exit = self.push_bare(node);
            self.link(&open, exit);
            self.take_continue(exit);
            return Vec::new();
        }
        // Unrecognized construct (match/switch/when, try/except/catch, item
        // declarations, ...): a generic unit keeps the CFG connected
        // without pretending to understand it - see module docs. A pattern
        // binding nested inside (e.g. a match arm's `let`) never goes
        // through `lower_binding`, so pre-declare it in a throwaway scope
        // first - otherwise it would resolve as a use of whatever it
        // shadows once `unit` scans the whole span. Only meaningful for a
        // `block_scoped` language: a non-block-scoped one has no shadowing
        // to guard against, and a throwaway scope would instead hide a
        // legitimately same-variable rebinding from code after this
        // construct.
        if t.block_scoped {
            self.push_scope();
            let mut names = Vec::new();
            self.collect_nested_binding_names(node, &mut names);
            for name in names {
                self.declare(name);
            }
        }
        let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
        if t.block_scoped {
            self.pop_scope();
        }
        self.link(&open, entry);
        vec![exit]
    }

    fn lower_fn_decl(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let name = node
            .child_by_field_name(self.table.function_name_field)
            .and_then(|n| n.utf8_text(self.source.as_bytes()).ok())
            .unwrap_or("<anon>");
        let v = self.declare(name);
        let line = line_of(node);
        let id = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Pure,
            defs: vec![v],
            uses: Vec::new(),
            succs: Vec::new(),
            label: "<fndecl>".into(),
        });
        self.link(&open, id);
        vec![id]
    }

    /// Resolves a `binding_kinds` node's pattern/value when it isn't the
    /// declarator-list shape (`binding_declarator_field` empty): a field
    /// lookup for either slot when the language fields at least one of them
    /// (Rust, Python), else a kind-search for the pattern and "the last
    /// named child, if it isn't the pattern itself" for the value (Kotlin's
    /// `property_declaration`, entirely unfielded).
    fn resolve_binding_pattern_value(
        &self,
        node: Node<'t>,
    ) -> (Option<Node<'t>>, Option<Node<'t>>) {
        let t = self.table;
        if !t.binding_pattern_field.is_empty() || !t.binding_value_field.is_empty() {
            let pattern = (!t.binding_pattern_field.is_empty())
                .then(|| node.child_by_field_name(t.binding_pattern_field))
                .flatten();
            let value = (!t.binding_value_field.is_empty())
                .then(|| node.child_by_field_name(t.binding_value_field))
                .flatten();
            return (pattern, value);
        }
        let pattern = first_child_of_kind(node, t.binding_pattern_kind);
        let mut cursor = node.walk();
        let last = node.named_children(&mut cursor).last();
        let value = match (pattern, last) {
            (Some(p), Some(l)) if p.id() != l.id() => Some(l),
            (None, Some(l)) => Some(l),
            _ => None,
        };
        (pattern, value)
    }

    fn lower_binding(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        if !self.table.binding_declarator_field.is_empty() {
            let mut cursor = node.walk();
            let declarators: Vec<Node<'t>> = node
                .children_by_field_name(self.table.binding_declarator_field, &mut cursor)
                .collect();
            let mut cur = open;
            for decl in declarators {
                let pattern = decl.child_by_field_name(self.table.binding_declarator_name_field);
                let value = decl.child_by_field_name(self.table.binding_declarator_value_field);
                cur = self.lower_one_binding(decl, pattern, value, None, cur);
            }
            return cur;
        }
        let (pattern, value) = self.resolve_binding_pattern_value(node);
        let alt = (!self.table.binding_alt_field.is_empty())
            .then(|| node.child_by_field_name(self.table.binding_alt_field))
            .flatten();
        self.lower_one_binding(node, pattern, value, alt, open)
    }

    /// One binding statement: `span_node` supplies the line/label (a whole
    /// `let`/property decl, or - for a declarator-list binding - just the
    /// one declarator, mirroring `vikt-js`'s "one node per declarator").
    /// `alt` is Rust `let`-else's diverging else-block, `None` everywhere
    /// else (including every declarator-list binding, since none of the
    /// languages with that shape has this construct either); when present
    /// the binding is genuinely two-way (pattern matched vs. didn't) so it
    /// becomes a `Branch` rather than a `Pure` unit, and `alt` is lowered
    /// through `lower_stmt` - like any other block - so a `return`/`break`/
    /// `continue`/call inside it lands in the graph instead of vanishing.
    fn lower_one_binding(
        &mut self,
        span_node: Node<'t>,
        pattern: Option<Node<'t>>,
        value: Option<Node<'t>>,
        alt: Option<Node<'t>>,
        open: Vec<usize>,
    ) -> Vec<usize> {
        let calls = value.map_or_else(Vec::new, |v| self.extract_calls(v));
        let mut uses = Vec::new();
        if let Some(v) = value {
            self.scan_uses(v, &mut uses);
        }
        // Uses come from the (old) value before the pattern's names are
        // declared, so `let x = x + 1;` reads the outer `x`.
        let mut defs = Vec::new();
        if let Some(p) = pattern {
            self.scan_defs(p, true, &mut defs);
        }
        let line = line_of(span_node);
        let label = snippet(span_node, self.source);
        let kind = if alt.is_some() {
            NodeKind::Branch
        } else {
            NodeKind::Pure
        };
        let id = self.push(IrNode {
            line: Some(line),
            kind,
            defs,
            uses,
            succs: Vec::new(),
            label,
        });
        let entry = if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.link(&[last], id);
            first
        } else {
            id
        };
        self.link(&open, entry);
        let mut exits = vec![id];
        if let Some(alt) = alt {
            exits.extend(self.lower_stmt(alt, vec![id]));
        }
        exits
    }

    fn lower_assign(&mut self, node: Node<'t>, open: Vec<usize>, is_compound: bool) -> Vec<usize> {
        let Some(left) = node.child_by_field_name(self.table.assign_left_field) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let right = node.child_by_field_name(self.table.assign_right_field);
        let calls = right.map_or_else(Vec::new, |r| self.extract_calls(r));
        let mut uses = Vec::new();
        if let Some(r) = right {
            self.scan_uses(r, &mut uses);
        }
        let is_member = self.table.member_access_kinds.contains(&left.kind());
        let (kind, defs) = if is_member {
            // A write through a member/attribute access outlives the
            // function: the base object is a use, never a def.
            self.scan_uses(left, &mut uses);
            (
                NodeKind::StateWrite {
                    target: snippet(left, self.source),
                },
                Vec::new(),
            )
        } else {
            let mut defs = Vec::new();
            self.scan_defs(left, self.table.assign_declares, &mut defs);
            if is_compound {
                for &d in &defs {
                    if !uses.contains(&d) {
                        uses.push(d);
                    }
                }
            }
            (NodeKind::Pure, defs)
        };
        let line = line_of(node);
        let label = snippet(node, self.source);
        let id = self.push(IrNode {
            line: Some(line),
            kind,
            defs,
            uses,
            succs: Vec::new(),
            label,
        });
        let entry = if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.link(&[last], id);
            first
        } else {
            id
        };
        self.link(&open, entry);
        vec![id]
    }

    /// A statement-position call: the extracted call node(s) plus a
    /// discard, mirroring `vikt-js`'s treatment so the whole statement is
    /// still sweepable when the call turns out to be denylisted.
    fn lower_call_stmt(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let calls = self.extract_calls(node);
        let mut uses = Vec::new();
        self.scan_uses(node, &mut uses);
        let line = line_of(node);
        let id = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Pure,
            defs: Vec::new(),
            uses,
            succs: Vec::new(),
            label: "<discard>".into(),
        });
        let entry = if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.link(&[last], id);
            first
        } else {
            id
        };
        self.link(&open, entry);
        vec![id]
    }

    fn lower_if(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let t = self.table;
        let (Some(cond), Some(then_)) = (
            field_or_nth(node, t.if_cond_field, 0),
            field_or_nth(node, t.if_then_field, 1),
        ) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let alts: Vec<Node<'t>> = if t.if_alt_field.is_empty() {
            field_or_nth(node, "", 2).into_iter().collect()
        } else {
            let mut cursor = node.walk();
            node.children_by_field_name(t.if_alt_field, &mut cursor)
                .collect()
        };
        self.lower_if_chain(cond, then_, &alts, open)
    }

    /// Lowers a condition/consequence pair plus its chain of
    /// elif-or-else alternatives, threading the false edge through each in
    /// turn. `alts` holds the *remaining* alternatives at this point in the
    /// chain; recursion consumes one per elif.
    fn lower_if_chain(
        &mut self,
        cond: Node<'t>,
        then_: Node<'t>,
        alts: &[Node<'t>],
        open: Vec<usize>,
    ) -> Vec<usize> {
        let (entry, branch) = self.unit(cond, NodeKind::Branch, vec![]);
        self.link(&open, entry);
        let mut exits = self.lower_stmt(then_, vec![branch]);
        match alts.split_first() {
            None => exits.push(branch),
            Some((first, rest)) => {
                let unwrapped = self.unwrap_wrapper(*first);
                if self.table.elif_kind == Some(unwrapped.kind()) {
                    if let (Some(c2), Some(t2)) = (
                        unwrapped.child_by_field_name(self.table.if_cond_field),
                        unwrapped.child_by_field_name(self.table.if_then_field),
                    ) {
                        exits.extend(self.lower_if_chain(c2, t2, rest, vec![branch]));
                    } else {
                        exits.push(branch);
                    }
                } else {
                    exits.extend(self.lower_stmt(unwrapped, vec![branch]));
                }
            }
        }
        exits
    }

    fn lower_while(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let t = self.table;
        let (Some(cond), Some(body)) = (
            field_or_nth(node, t.while_cond_field, 0),
            field_or_last(node, t.while_body_field),
        ) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let (entry, branch) = self.unit(cond, NodeKind::Branch, vec![]);
        self.link(&open, entry);
        self.loops.push(LoopCtx {
            continue_target: branch,
            breaks: Vec::new(),
        });
        let body_exits = self.lower_stmt(body, vec![branch]);
        self.link(&body_exits, branch);
        let ctx = self.loops.pop().expect("while loop context pushed above");
        let mut exits = vec![branch];
        exits.extend(ctx.breaks);
        exits
    }

    /// An unconditional loop (Rust's `loop { .. }`): a `Branch` head with no
    /// condition of its own to evaluate - it always falls into the body -
    /// modeled the same shape as `lower_while`'s so `break`/`continue` and
    /// the back edge work identically. `exits` still carries the head node
    /// itself alongside any `break`s, matching `lower_while`/`lower_for`'s
    /// conservative "the loop might not run forever" over-approximation
    /// rather than trying to prove a bare `loop` with no `break` diverges.
    fn lower_loop(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let t = self.table;
        let Some(body) = field_or_kind(node, t.loop_body_field, "") else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let line = line_of(node);
        let label = snippet(node, self.source);
        let branch = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Branch,
            defs: Vec::new(),
            uses: Vec::new(),
            succs: Vec::new(),
            label,
        });
        self.link(&open, branch);
        self.loops.push(LoopCtx {
            continue_target: branch,
            breaks: Vec::new(),
        });
        let body_exits = self.lower_stmt(body, vec![branch]);
        self.link(&body_exits, branch);
        let ctx = self.loops.pop().expect("loop context pushed above");
        let mut exits = vec![branch];
        exits.extend(ctx.breaks);
        exits
    }

    /// A for-each loop: one `Branch` node models the iterator advance,
    /// def'ing the loop pattern and using the iterated expression, so every
    /// iteration is loop-carried by construction and the back edge is real.
    fn lower_for(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let t = self.table;
        let (Some(pattern), Some(value), Some(body)) = (
            field_or_nth(node, t.for_pattern_field, 0),
            field_or_nth(node, t.for_value_field, 1),
            field_or_last(node, t.for_body_field),
        ) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let calls = self.extract_calls(value);
        let mut uses = Vec::new();
        self.scan_uses(value, &mut uses);
        // The pattern binding must be visible in the body, so this pushes
        // its own scope covering both, ahead of dispatching `body` through
        // `lower_stmt` below (which pushes its own nested scope only when
        // `body` turns out to be a `block_kinds` node in its own right).
        if self.table.block_scoped {
            self.push_scope();
        }
        let mut defs = Vec::new();
        self.scan_defs(pattern, true, &mut defs);
        let line = line_of(node);
        let label = snippet(node, self.source);
        let branch = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Branch,
            defs,
            uses,
            succs: Vec::new(),
            label,
        });
        let entry = if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.link(&[last], branch);
            first
        } else {
            branch
        };
        self.link(&open, entry);
        self.loops.push(LoopCtx {
            continue_target: branch,
            breaks: Vec::new(),
        });
        // `body` is a single `statement` position (Java's `enhanced_for_
        // statement`, for instance, allows an unbraced body just like `if`/
        // `while`), not necessarily a `block_kinds` node - dispatching
        // through `lower_stmt`, exactly like `lower_while` does, is what
        // lowers it as one statement instead of splicing its own children
        // into the loop body as if they were siblings.
        let body_exits = self.lower_stmt(body, vec![branch]);
        self.link(&body_exits, branch);
        let ctx = self.loops.pop().expect("for loop context pushed above");
        if self.table.block_scoped {
            self.pop_scope();
        }
        let mut exits = vec![branch];
        exits.extend(ctx.breaks);
        exits
    }

    /// A `match`/`switch`/`when` construct: the discriminant becomes a
    /// `Branch` whose successors are the arms' own entry nodes, each arm
    /// lowered through `lower_stmt`/`lower_seq` like ordinary code (own
    /// scope for its pattern bindings, real `Return`/`Throw`/`Call` nodes
    /// for anything inside it) - see the module docs' "Match" section.
    /// Arms are treated as mutually exclusive with no fallthrough between
    /// them, even for Java's colon-form `switch` (a v2 simplification: see
    /// module docs).
    fn lower_match(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let t = self.table;
        let subject = field_or_kind(node, t.match_subject_field, t.match_subject_kind);
        let (calls, uses) = match subject {
            Some(s) => {
                let calls = self.extract_calls(s);
                let mut uses = Vec::new();
                self.scan_uses(s, &mut uses);
                (calls, uses)
            }
            None => (Vec::new(), Vec::new()),
        };
        let line = line_of(node);
        let label = snippet(node, self.source);
        let disc = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Branch,
            defs: Vec::new(),
            uses,
            succs: Vec::new(),
            label,
        });
        let entry = if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.link(&[last], disc);
            first
        } else {
            disc
        };
        self.link(&open, entry);

        let container = field_or_kind(node, t.match_body_field, t.match_body_kind).unwrap_or(node);
        let mut cursor = container.walk();
        let arms: Vec<Node<'t>> = container
            .named_children(&mut cursor)
            .filter(|c| t.match_arm_kinds.contains(&c.kind()))
            .collect();

        let mut exits = Vec::new();
        let mut has_default = false;
        for arm in arms {
            let (arm_exits, is_default) = self.lower_match_arm(arm, disc);
            exits.extend(arm_exits);
            has_default |= is_default;
        }
        // No arm unconditionally matches: the value might match nothing
        // (Python's `match` silently falls through; a non-exhaustive `when`
        // is a compile error in Kotlin, but exhaustiveness isn't something
        // this walker can verify from syntax alone), so the discriminant
        // itself stays a live exit - the same conservative treatment
        // `lower_if_chain` gives an `if` with no final `else`.
        if !has_default {
            exits.push(disc);
        }
        exits
    }

    /// One match arm: a `Pure` head node (defs = pattern bindings, uses =
    /// guard/label reads) feeding into the arm's own body statements,
    /// pattern-scoped so a bound name never leaks into a sibling arm.
    /// Returns the arm's own exits and whether it is a wildcard/default arm
    /// (`_`, `default`, or no label at all).
    fn lower_match_arm(&mut self, arm: Node<'t>, disc: usize) -> (Vec<usize>, bool) {
        let t = self.table;
        let labels: Vec<Node<'t>> = if t.match_arm_pattern_multi {
            let mut cursor = arm.walk();
            arm.children_by_field_name(t.match_arm_pattern_field, &mut cursor)
                .collect()
        } else {
            field_or_kind(arm, t.match_arm_pattern_field, t.match_arm_pattern_kind)
                .into_iter()
                .collect()
        };
        let is_default = labels.is_empty()
            || labels
                .iter()
                .all(|l| matches!(snippet(*l, self.source).as_str(), "_" | "default"));

        if t.block_scoped {
            self.push_scope();
        }
        let mut defs = Vec::new();
        let mut uses = Vec::new();
        for label in &labels {
            // A guard nested inside the label itself (Rust's
            // `match_pattern.condition`) must not be scanned as part of the
            // pattern's own bindings - only as a use, once the bindings it
            // reads are already declared.
            let nested_guard = (!t.match_pattern_guard_field.is_empty())
                .then(|| label.child_by_field_name(t.match_pattern_guard_field))
                .flatten();
            if t.match_arm_pattern_declares {
                let mut names = Vec::new();
                self.collect_identifier_texts_excluding(
                    *label,
                    nested_guard.map(|g| g.id()),
                    &mut names,
                );
                for name in names {
                    let v = self.declare(name);
                    if !defs.contains(&v) {
                        defs.push(v);
                    }
                }
            } else {
                self.scan_uses(*label, &mut uses);
            }
            if let Some(g) = nested_guard {
                self.scan_uses(g, &mut uses);
            }
        }
        // A guard as a sibling field on the arm itself (Python's
        // `case_clause.guard`), scanned after every label's bindings are
        // declared so it reads the pattern-bound names, not whatever they
        // shadow.
        let guard = (!t.match_arm_guard_field.is_empty())
            .then(|| arm.child_by_field_name(t.match_arm_guard_field))
            .flatten();
        if let Some(g) = guard {
            self.scan_uses(g, &mut uses);
        }

        let label_ids: Vec<usize> = labels.iter().map(Node::id).collect();
        let guard_id = guard.map(|g| g.id());
        let mut cursor = arm.walk();
        let body_nodes: Vec<Node<'t>> = arm
            .named_children(&mut cursor)
            .filter(|c| !label_ids.contains(&c.id()) && Some(c.id()) != guard_id)
            .collect();

        let head_line = line_of(arm);
        let head_label = snippet(arm, self.source);
        let head = self.push(IrNode {
            line: Some(head_line),
            kind: NodeKind::Pure,
            defs,
            uses,
            succs: Vec::new(),
            label: head_label,
        });
        self.link(&[disc], head);
        let exits = self.lower_seq(body_nodes.into_iter(), vec![head]);
        if t.block_scoped {
            self.pop_scope();
        }
        (exits, is_default)
    }

    /// `try`/`except`/`catch`/`finally`: the `try` body lowers as ordinary
    /// code, every handler is lowered too (so a `return`/`throw`/call
    /// inside one is a real graph node) but never linked in *from* the try
    /// body - modeling the exception edge itself is a measured regression
    /// in `vikt-js` (agreement with expert labels dropped when tried), so
    /// this frontend makes the same call. `finally` (and Python's `else`)
    /// runs once, sequentially after the try body's own exits - not
    /// duplicated along every exit path.
    fn lower_try(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let t = self.table;
        let Some(body) = field_or_kind(node, t.try_body_field, t.try_body_kind) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let mut exits = self.lower_block(body, open);

        let mut cursor = node.walk();
        let catches: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|c| t.catch_kinds.contains(&c.kind()))
            .collect();
        for catch in catches {
            self.lower_catch_handler(catch);
        }

        if !t.try_else_kind.is_empty()
            && let Some(else_node) = first_child_of_kind(node, t.try_else_kind)
            && let Some(else_body) = else_node.child_by_field_name(t.try_else_body_field)
        {
            exits = self.lower_block(else_body, exits);
        }

        if !t.finally_kind.is_empty()
            && let Some(fin) = first_child_of_kind(node, t.finally_kind)
            && let Some(fin_body) = field_or_kind(fin, t.finally_body_field, t.finally_body_kind)
        {
            exits = self.lower_block(fin_body, exits);
        }
        exits
    }

    /// One handler clause: lowered from a synthetic, deliberately
    /// unreachable entry node (defs = the bound exception name, if any) -
    /// see `lower_try`'s docs on why it is never linked from the try body.
    fn lower_catch_handler(&mut self, catch: Node<'t>) {
        let t = self.table;
        let Some(body) = field_or_kind(catch, t.catch_body_field, t.catch_body_kind) else {
            return;
        };
        if t.block_scoped {
            self.push_scope();
        }
        let mut defs = Vec::new();
        if let Some(param) = self.catch_param_node(catch) {
            let mut names = Vec::new();
            self.collect_identifier_texts(param, &mut names);
            for name in names {
                let v = self.declare(name);
                if !defs.contains(&v) {
                    defs.push(v);
                }
            }
        }
        let line = line_of(catch);
        let entry = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Pure,
            defs,
            uses: Vec::new(),
            succs: Vec::new(),
            label: "<catch>".into(),
        });
        let _ = self.lower_block(body, vec![entry]);
        if t.block_scoped {
            self.pop_scope();
        }
    }

    /// A handler's bound exception name, across the two shapes this
    /// frontend's grammars use - see the field docs on `catch_param_kind`
    /// and `catch_param_as_field`.
    fn catch_param_node(&self, catch: Node<'t>) -> Option<Node<'t>> {
        let t = self.table;
        if !t.catch_param_kind.is_empty() {
            let found = first_child_of_kind(catch, t.catch_param_kind)?;
            return if t.catch_param_name_field.is_empty() {
                Some(found)
            } else {
                found.child_by_field_name(t.catch_param_name_field)
            };
        }
        if !t.catch_param_as_field.is_empty() {
            let value = catch.child_by_field_name(t.catch_param_as_field)?;
            if value.kind() == t.catch_param_as_pattern_kind {
                return value.child_by_field_name(t.catch_param_alias_field);
            }
        }
        None
    }
}
