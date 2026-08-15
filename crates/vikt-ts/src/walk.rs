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
//! true binding resolution. A `block_scoped` language (Rust) pushes a fresh
//! scope per `block`, so `let x` in a nested block never clobbers an outer
//! `x`; a non-block-scoped language (Python) keeps one scope for the whole
//! function, matching real Python scoping (only `def` introduces scope).
//! Names that resolve to nothing in scope become per-name ambient variables,
//! shared for the whole function - the same "unresolved reference still
//! tracks a flow" treatment `vikt-js` gives ambient JS globals.
//!
//! # Known v1 simplifications
//!
//! - `match`/`switch` and `try`/`except` are not specially modeled: an
//!   unmatched node kind falls back to a single `Pure` unit spanning the
//!   whole construct. Calls anywhere inside it are still extracted (so an
//!   opaque call three levels into a `match` arm is still visible as an
//!   effect), but a `return`/`throw` inside one is not - it is invisible to
//!   the effect classification. Not exercised by the required test surface;
//!   flagged here because it is the sharpest edge in this file.
//! - Closures are not modeled: a nested function's free variables are never
//!   folded into the enclosing unit's uses the way `vikt-js` does for
//!   captures.
//! - Loop labels are not modeled: `break`/`continue` always target the
//!   innermost loop.
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
    functions.push(module.finish());

    for fnode in fn_nodes {
        let name = fnode
            .child_by_field_name(table.function_name_field)
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("<anon>")
            .to_owned();
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

        let params = fnode.child_by_field_name(table.function_params_field);
        let body = fnode.child_by_field_name(table.function_body_field);
        let mut lowerer = FnCtx::new(table, source, file, &name, decl_line);
        lowerer.lower_params(params);
        if let Some(b) = body {
            let _ = lowerer.lower_block_children(b, vec![0]);
        }
        functions.push(lowerer.finish());
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
            let object_ok = node
                .child_by_field_name(table.member_object_field)
                .is_some_and(|o| chain(table, o, source, out));
            if let Some(p) = node.child_by_field_name(table.member_property_field) {
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
        }
    }

    fn push(&mut self, node: IrNode) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn link(&mut self, from: &[usize], to: usize) {
        for &f in from {
            if !self.nodes[f].succs.contains(&to) {
                self.nodes[f].succs.push(to);
            }
        }
    }

    fn finish(self) -> FunctionIr {
        FunctionIr {
            id: FunctionId {
                file: self.file,
                name: self.name,
                signature: String::new(),
                decl_line: Some(self.decl_line),
            },
            nodes: self.nodes,
            entry: 0,
        }
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
            .then(|| {
                node.child_by_field_name(self.table.member_property_field)
                    .map(|n| n.id())
            })
            .flatten();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if skip_id == Some(child.id()) {
                continue;
            }
            self.collect_identifier_texts(child, out);
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
            let callee = cn
                .child_by_field_name(self.table.call_callee_field)
                .map_or_else(
                    || "<dynamic>".to_owned(),
                    |c| callee_text(self.table, c, self.source),
                );
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
    /// root).
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
        if t.block_kinds.contains(&kind) {
            return self.lower_block(node, open);
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
            return self.lower_assign(node, open, false);
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
        // Unrecognized construct (match/switch, try/except, item
        // declarations, ...): a generic unit keeps the CFG connected
        // without pretending to understand it - see module docs.
        let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
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

    fn lower_binding(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let pattern = node.child_by_field_name(self.table.binding_pattern_field);
        let value = node.child_by_field_name(self.table.binding_value_field);
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
        let line = line_of(node);
        let label = snippet(node, self.source);
        let id = self.push(IrNode {
            line: Some(line),
            kind: NodeKind::Pure,
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
        let (Some(cond), Some(then_)) = (
            node.child_by_field_name(self.table.if_cond_field),
            node.child_by_field_name(self.table.if_then_field),
        ) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let mut cursor = node.walk();
        let alts: Vec<Node<'t>> = node
            .children_by_field_name(self.table.if_alt_field, &mut cursor)
            .collect();
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
        let (Some(cond), Some(body)) = (
            node.child_by_field_name(self.table.while_cond_field),
            node.child_by_field_name(self.table.while_body_field),
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

    /// A for-each loop: one `Branch` node models the iterator advance,
    /// def'ing the loop pattern and using the iterated expression, so every
    /// iteration is loop-carried by construction and the back edge is real.
    fn lower_for(&mut self, node: Node<'t>, open: Vec<usize>) -> Vec<usize> {
        let (Some(pattern), Some(value), Some(body)) = (
            node.child_by_field_name(self.table.for_pattern_field),
            node.child_by_field_name(self.table.for_value_field),
            node.child_by_field_name(self.table.for_body_field),
        ) else {
            let (entry, exit) = self.unit(node, NodeKind::Pure, vec![]);
            self.link(&open, entry);
            return vec![exit];
        };
        let calls = self.extract_calls(value);
        let mut uses = Vec::new();
        self.scan_uses(value, &mut uses);
        // The pattern binding must be visible in the body, so this pushes
        // its own scope covering both rather than letting a later
        // `lower_block` dispatch on `body` push a second one.
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
        let body_exits = self.lower_block_children(body, vec![branch]);
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
