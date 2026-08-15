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
//! # Known v1 simplifications
//!
//! - `match`/`switch`/`when` and `try`/`except`/`catch` are not specially
//!   modeled: an unmatched node kind falls back to a single `Pure` unit
//!   spanning the whole construct. Calls anywhere inside it are still
//!   extracted (so an opaque call three levels into a `match` arm is still
//!   visible as an effect), but a `return`/`throw` inside one is not - it is
//!   invisible to the effect classification. Not exercised by the required
//!   test surface; flagged here because it is the sharpest edge in this
//!   file. The one exception: a Kotlin expression-bodied function's tail
//!   construct (e.g. `= when (x) { ... }`) is still wrapped in a real
//!   `Return` node by `lower_module`, so effects reachable through *that*
//!   specific `when` are not lost - only ones nested inside a `when`/`try`
//!   used mid-block are. A pattern binding nested inside one of these
//!   (e.g. a match arm's `let`) does get its own id in a `block_scoped`
//!   language, via `collect_nested_binding_names` - it just isn't a real
//!   `Binding` node in the graph, so it has no def/use edges of its own
//!   beyond being folded into the enclosing opaque unit's `uses`.
//! - Closures/lambdas are not modeled: a nested function's (or Kotlin
//!   lambda's) free variables are never folded into the enclosing unit's
//!   uses the way `vikt-js` does for captures, and any call inside one is
//!   attributed to the *enclosing* function rather than kept separate.
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
/// alike, in stable document order.
fn collect_functions<'t>(table: &'static GrammarTable, node: Node<'t>, out: &mut Vec<Node<'t>>) {
    if table.function_kinds.contains(&node.kind()) {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(table, child, out);
    }
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

    for fnode in fn_nodes {
        let bare_name = fnode
            .child_by_field_name(table.function_name_field)
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("<anon>");
        let name = owner_prefix(table, fnode, source).map_or_else(
            || bare_name.to_owned(),
            |owner| format!("{owner}::{bare_name}"),
        );
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

        let params = field_or_kind(
            fnode,
            table.function_params_field,
            table.function_params_kind,
        );
        let raw_body = field_or_kind(fnode, table.function_body_field, table.function_body_kind);
        let mut lowerer = FnCtx::new(table, source, file, &name, decl_line);
        lowerer.lower_params(params);
        if let Some(raw) = raw_body {
            let body = lowerer.unwrap_wrapper(raw);
            if table.block_kinds.contains(&body.kind()) {
                let _ = lowerer.lower_block_children(body, vec![0]);
            } else {
                // Expression body (Kotlin `fun f() = expr`): the tail
                // expression's value is what the function returns, modeled
                // the same way `vikt-js` models an arrow's expr-body.
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
    if table.function_kinds.contains(&node.kind()) {
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
    /// function's body (captures are not modeled).
    fn collect_identifier_texts(&self, node: Node<'t>, out: &mut Vec<&'t str>) {
        if self.table.identifier_kinds.contains(&node.kind()) {
            if let Ok(text) = node.utf8_text(self.source.as_bytes()) {
                out.push(text);
            }
            return;
        }
        if self.table.function_kinds.contains(&node.kind()) {
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
            if skip_id == Some(child.id()) {
                continue;
            }
            self.collect_identifier_texts(child, out);
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
        if t.function_kinds.contains(&node.kind()) {
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
}
