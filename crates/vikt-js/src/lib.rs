//! JavaScript / TypeScript frontend: lowers oxc's semantic-resolved AST into
//! [`FunctionIr`].
//!
//! # Why an AST here, when the README argues for IR
//!
//! The IR-over-AST argument is about compilers that reshape control flow
//! between the source and what runs - Kotlin's coroutine state machines,
//! `inline` copying, `when` lowering. JavaScript has no such compiler: there
//! is no standard bytecode (V8's is deliberately unstable and version-bound),
//! and the source's control flow *is* the program's. A semantic-resolved AST
//! with an explicitly constructed CFG is therefore the faithful substrate,
//! not a compromise. TypeScript rides free: oxc parses it natively and type
//! annotations don't produce lowered nodes.
//!
//! # Granularity
//!
//! One node per *statement* (one per declarator for multi-declarations), plus
//! one node per call expression, extracted in evaluation order ahead of the
//! unit that contains it. Extracting calls is what lets the denylist isolate
//! `console.log(...)` chains exactly the way the bytecode frontends do: the
//! call is its own node with its own callee name, its result is a synthetic
//! temp, and a value whose every use ends in a denylisted call goes inert.
//!
//! Def/use comes from oxc's reference resolution, not from name matching:
//! every identifier reference is attributed to the innermost enclosing unit,
//! write-only references count as defs rather than uses, and unresolved names
//! (`Math`, `console`, anything ambient) get per-name variable ids so flows
//! through globals are still flows.
//!
//! # Function naming
//!
//! A named function expression or declaration keeps its own name. Anything
//! else - an arrow, or a `function` with no name - borrows one from its
//! immediate syntactic context when the context provides one: a `const`/
//! `let` target, an assignment target, an object property (including a
//! shorthand method), or a class field or method. Failing that, it is
//! qualified by its nearest named enclosing function (`outer.<anon@LINE>`)
//! so a bare callback or an IIFE still reads in context; a module-level
//! anonymous with no enclosing function keeps the bare `<fn@LINE>` form.
//! See [`infer_name`] and [`context_name`].
//!
//! # Closure captures
//!
//! A unit whose span contains a nested function - `const f = () => ...`, a
//! function expression, a `function` declaration - gains, as uses, every
//! symbol that function's full span (params included, transitively through
//! any function nested inside *it*) references but does not itself bind.
//! That is what puts a captured parameter on the def-use chain to whatever
//! the closure returns, instead of leaving it dangling: `memoize`'s returned
//! closure now depends on `func` and `resolver` the same way it would if the
//! call were inlined.
//!
//! # Control flow
//!
//! `if`/`while`/`do`/`for`/`for-in`/`for-of`/`switch` (with fallthrough),
//! labelled `break`/`continue`, `return`/`throw`. Loop predicates are their
//! own `Branch` nodes, so dominance and loop analysis see real back edges.
//!
//! Exception edges are deliberately absent: a `catch` body is lowered but
//! unreachable from the entry. This mirrors a *measured* decision in the
//! Python frontend - making handlers reachable destroys post-dominance and
//! drags every scored line toward error paths (agreement with expert labels
//! fell from 0.33 to 0.13 when the edges were added there). `finally` is
//! approximated as sequential code after the `try` body.
//!
//! # Known v1 simplifications
//!
//! - `finally` runs once, after the `try` exits, rather than being duplicated
//!   along every exit path.
//! - Logical short-circuits (`&&`, `||`, `??`, `?:`) stay inside their
//!   statement node rather than becoming branches - statement granularity.

pub mod calibrate;

use std::collections::BTreeMap;
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast::ast::{self, Expression, Statement};
use oxc_parser::Parser;
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::node::NodeId;
use thiserror::Error;
use vikt_core::ir::{CallOpacity, FunctionId, FunctionIr, Node, NodeKind, VarId};

/// Failure to lower a source file.
#[derive(Debug, Error)]
pub enum JsError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The parser gave up entirely.
    #[error("{path}: parse failed: {first}")]
    Syntax {
        /// Offending path.
        path: String,
        /// First error, for the human.
        first: String,
    },
}

/// A lowered module: one [`FunctionIr`] per function plus one for top-level
/// code, mirroring the Python frontend's shape.
#[derive(Debug)]
pub struct LoweredModule {
    /// Source path as given.
    pub file: String,
    /// Every lowered function, module body first.
    pub functions: Vec<FunctionIr>,
}

/// Lowers a `.js` / `.mjs` / `.cjs` / `.jsx` / `.ts` / `.mts` / `.tsx` file.
pub fn lower_file(path: &Path) -> Result<LoweredModule, JsError> {
    let source = std::fs::read_to_string(path).map_err(|e| JsError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    lower_source(&source, &path.display().to_string())
}

/// Lowers source text directly. `file` decides JS-vs-TS by extension and is
/// recorded in every function id.
pub fn lower_source(source: &str, file: &str) -> Result<LoweredModule, JsError> {
    let source_type = SourceType::from_path(file).unwrap_or_else(|_| SourceType::mjs());
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked {
        return Err(JsError::Syntax {
            path: file.to_owned(),
            first: parsed
                .diagnostics
                .first()
                .map_or_else(|| "parser panicked".into(), ToString::to_string),
        });
    }
    // `with_build_nodes(true)`: the default builder skips the AST node table,
    // and everything below - references, calls, bindings, functions - is read
    // from it.
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;

    let line_starts: Vec<u32> = std::iter::once(0u32)
        .chain(
            source
                .bytes()
                .enumerate()
                .filter(|&(_, b)| b == b'\n')
                .map(|(i, _)| u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1)),
        )
        .collect();

    let facts = Facts::collect(&semantic, source);

    let mut functions = Vec::new();
    let mut ml = FnLowerer::new(file, "<module>", 1, &facts, &line_starts, source);
    ml.lower_params(&[]);
    let _ = ml.lower_seq(&parsed.program.body, vec![0]);
    functions.push(ml.finish());

    for f in &facts.functions {
        let mut fl = FnLowerer::new(file, &f.name, f.line, &facts, &line_starts, source);
        fl.lower_params(&f.param_defs);
        match f.body {
            FnBody::Block(stmts) => {
                let _ = fl.lower_seq(stmts, vec![0]);
            }
            FnBody::Expr(e) => {
                let (entry, _) = fl.unit(e.span(), NodeKind::Return, vec![]);
                fl.link(&[0], entry);
            }
        }
        functions.push(fl.finish());
    }

    Ok(LoweredModule {
        file: file.to_owned(),
        functions,
    })
}

/// What a function body is, in oxc terms.
enum FnBody<'a> {
    Block(&'a [Statement<'a>]),
    /// Arrow with an expression body: `x => x + 1`.
    Expr(&'a Expression<'a>),
}

/// One discovered function-like.
struct FnFact<'a> {
    name: String,
    line: u32,
    body: FnBody<'a>,
    param_defs: Vec<SymbolKey>,
}

/// Stable identity for a variable before per-function interning.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SymbolKey {
    /// Resolved binding, by oxc symbol index.
    Sym(u32),
    /// Unresolved - ambient/global, keyed by name.
    Global(String),
    /// Synthetic temp carrying a call result.
    Temp(u32),
}

/// One identifier reference, pre-bucketed.
struct RefFact {
    span: Span,
    key: SymbolKey,
    read: bool,
    write: bool,
}

/// One call or `new` expression, pre-bucketed.
struct CallFact {
    span: Span,
    callee: String,
}

/// Everything span-addressable, collected in one pass over the semantic node
/// table so statement lowering never has to walk expression trees.
struct Facts<'a> {
    refs: Vec<RefFact>,
    calls: Vec<CallFact>,
    functions: Vec<FnFact<'a>>,
    /// Body spans of all nested functions - references inside them belong to
    /// the nested function, never to an enclosing unit.
    fn_spans: Vec<Span>,
    /// Full spans of all nested functions, params included - unlike
    /// `fn_spans` these cover the parameter list too, which is where a
    /// closure's captured symbols get attributed. Used to enrich an
    /// enclosing unit's uses with the closure's captures - see "Closure
    /// captures" in the module docs.
    full_spans: Vec<Span>,
    /// `BindingIdentifier`s: declarations, params, catch params.
    bindings: Vec<(Span, u32)>,
}

impl<'a> Facts<'a> {
    fn collect(semantic: &Semantic<'a>, source: &str) -> Self {
        let scoping = semantic.scoping();
        let mut refs = Vec::new();
        let mut calls = Vec::new();
        let mut functions: Vec<FnFact<'a>> = Vec::new();
        let mut fn_spans = Vec::new();
        let mut full_spans = Vec::new();
        let bindings = bindings_snapshot(semantic);

        for node in semantic.nodes() {
            match node.kind() {
                AstKind::IdentifierReference(id) => {
                    let r = scoping.get_reference(id.reference_id());
                    let key = r.symbol_id().map_or_else(
                        || SymbolKey::Global(id.name.to_string()),
                        |s| SymbolKey::Sym(u32::try_from(s.index()).unwrap_or(u32::MAX)),
                    );
                    refs.push(RefFact {
                        span: id.span,
                        key,
                        read: r.flags().is_read(),
                        write: r.flags().is_write(),
                    });
                }
                AstKind::CallExpression(c) => calls.push(CallFact {
                    span: c.span,
                    callee: callee_text(&c.callee, source),
                }),
                AstKind::NewExpression(c) => calls.push(CallFact {
                    span: c.span,
                    callee: format!("new {}", callee_text(&c.callee, source)),
                }),
                AstKind::Function(f) => {
                    if let Some(body) = &f.body {
                        let own = f.id.as_ref().map(|id| id.name.to_string());
                        let name = infer_name(node.id(), f.span, own, semantic, source);
                        functions.push(FnFact {
                            name,
                            line: line_of_offset(f.span.start, source),
                            body: FnBody::Block(&body.statements),
                            param_defs: param_keys(f.span, body.span, &bindings),
                        });
                        fn_spans.push(body.span);
                        full_spans.push(f.span);
                    }
                }
                AstKind::ArrowFunctionExpression(f) => {
                    let body_span = f.body.span();
                    let b = match &f.body {
                        ast::ArrowFunctionBody::FunctionBody(fb) => FnBody::Block(&fb.statements),
                        expr_body => match expr_body.as_expression() {
                            Some(e) => FnBody::Expr(e),
                            None => continue,
                        },
                    };
                    let name = infer_name(node.id(), f.span, None, semantic, source);
                    functions.push(FnFact {
                        name,
                        line: line_of_offset(f.span.start, source),
                        body: b,
                        param_defs: param_keys(f.span, body_span, &bindings),
                    });
                    fn_spans.push(body_span);
                    full_spans.push(f.span);
                }
                _ => {}
            }
        }

        refs.sort_by_key(|r| (r.span.start, r.span.end));
        calls.sort_by_key(|c| (c.span.start, std::cmp::Reverse(c.span.end)));
        Self {
            refs,
            calls,
            functions,
            fn_spans,
            full_spans,
            bindings,
        }
    }

    /// Is `inner` inside some nested-function body that is itself strictly
    /// inside `outer`? Such spans belong to the nested function.
    fn inside_nested_fn(&self, inner: Span, outer: Span) -> bool {
        self.fn_spans.iter().any(|f| {
            f.start >= outer.start
                && f.end <= outer.end
                && !(f.start <= outer.start && f.end >= outer.end)
                && inner.start >= f.start
                && inner.end <= f.end
        })
    }

    /// The outermost nested-function full spans lying strictly inside
    /// `span`: every entry of `full_spans` contained in `span` that is not
    /// itself contained in another such entry. A capture set computed over
    /// one of these already covers everything referenced by functions nested
    /// inside it, so callers never need to descend further.
    fn nested_fn_full_spans(&self, span: Span) -> Vec<Span> {
        let mut candidates: Vec<Span> = self
            .full_spans
            .iter()
            .copied()
            .filter(|f| f.start >= span.start && f.end <= span.end && *f != span)
            .collect();
        candidates.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        let mut outermost: Vec<Span> = Vec::new();
        for f in candidates {
            let nested = outermost
                .iter()
                .any(|o| f.start >= o.start && f.end <= o.end);
            if !nested {
                outermost.push(f);
            }
        }
        outermost
    }

    /// A function's captured symbols: every `Sym` reference inside `full`
    /// (its full span, params included, transitively through any function
    /// nested inside it) whose defining `BindingIdentifier` lies outside
    /// `full`. `Global` keys are never captures - an ambient name is not
    /// closed over, it is just looked up again wherever it is read.
    fn captures(&self, full: Span) -> Vec<SymbolKey> {
        let mut out: Vec<SymbolKey> = Vec::new();
        for r in &self.refs {
            if r.span.start < full.start || r.span.end > full.end {
                continue;
            }
            let SymbolKey::Sym(sid) = r.key else {
                continue;
            };
            let bound_inside = self.bindings.iter().any(|(bspan, bsid)| {
                *bsid == sid && bspan.start >= full.start && bspan.end <= full.end
            });
            if bound_inside {
                continue;
            }
            let key = SymbolKey::Sym(sid);
            if !out.contains(&key) {
                out.push(key);
            }
        }
        out
    }
}

/// All `BindingIdentifier`s with resolved symbols.
fn bindings_snapshot(semantic: &Semantic<'_>) -> Vec<(Span, u32)> {
    semantic
        .nodes()
        .iter()
        .filter_map(|n| match n.kind() {
            AstKind::BindingIdentifier(b) => b
                .symbol_id
                .get()
                .map(|sid| (b.span, u32::try_from(sid.index()).unwrap_or(u32::MAX))),
            _ => None,
        })
        .collect()
}

/// Symbols bound between a function's start and its body: the parameters.
fn param_keys(full: Span, body: Span, bindings: &[(Span, u32)]) -> Vec<SymbolKey> {
    bindings
        .iter()
        .filter(|(s, _)| s.start >= full.start && s.end <= body.start)
        .map(|(_, sid)| SymbolKey::Sym(*sid))
        .collect()
}

fn line_of_offset(off: u32, source: &str) -> u32 {
    u32::try_from(
        1 + source
            .get(..off as usize)
            .unwrap_or("")
            .bytes()
            .filter(|b| *b == b'\n')
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn anon_name(span: Span, source: &str) -> String {
    format!("<fn@{}>", line_of_offset(span.start, source))
}

/// A function-like's name: its own binding (`f.id`, `None` for an arrow or
/// an unnamed function expression) if it has one; else whatever the
/// immediate syntactic context provides (see [`context_name`]); else
/// `parent.<anon@LINE>`, qualified by the nearest named enclosing function
/// so a bare callback or an IIFE still reads in context; else, for a
/// module-level anonymous with no enclosing function at all, the bare
/// `<fn@LINE>` form.
fn infer_name(
    node_id: NodeId,
    span: Span,
    own: Option<String>,
    semantic: &Semantic<'_>,
    source: &str,
) -> String {
    if let Some(name) = own {
        return name;
    }
    if let Some(name) = context_name(node_id, semantic) {
        return name;
    }
    match enclosing_name(node_id, semantic, source) {
        Some(parent) => format!("{parent}.<anon@{}>", line_of_offset(span.start, source)),
        None => anon_name(span, source),
    }
}

/// A name borrowed from the immediate parent that binds this function-like
/// node: a `const`/`let` declarator (`const f = () => …` → `f`), a plain or
/// member assignment (`x = function(){}` → `x`, `obj.prop = …` → `prop`),
/// an object-literal property or shorthand method (`{ prop: () => … }`,
/// `{ prop() {} }`), or a class field or method. `None` when the function
/// sits in none of these shapes - a bare callback argument, an IIFE, a
/// `return`, an array element, ...
fn context_name(node_id: NodeId, semantic: &Semantic<'_>) -> Option<String> {
    match semantic.nodes().parent_kind(node_id) {
        AstKind::VariableDeclarator(d) => match &d.id {
            ast::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
            _ => None,
        },
        AstKind::AssignmentExpression(a) => assignment_target_name(&a.left),
        AstKind::ObjectProperty(p) => p.key.static_name().map(|n| n.into_owned()),
        AstKind::PropertyDefinition(p) => p.key.static_name().map(|n| n.into_owned()),
        AstKind::MethodDefinition(m) => m.key.static_name().map(|n| n.into_owned()),
        _ => None,
    }
}

/// The bound name of a simple assignment target: a bare identifier
/// (`x = …`) or the trailing property of a static member write
/// (`obj.prop = …`). A computed (`obj[expr] = …`) or destructuring target
/// has no statically-known name.
fn assignment_target_name(target: &ast::AssignmentTarget<'_>) -> Option<String> {
    match target {
        ast::AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.to_string()),
        ast::AssignmentTarget::StaticMemberExpression(m) => Some(m.property.name.to_string()),
        _ => None,
    }
}

/// The nearest enclosing function-like node's own name, computed the same
/// way [`infer_name`] would for it - so a doubly-anonymous closure still
/// chains (`outer.<anon@N>.<anon@M>`) instead of losing context. `None` at
/// module scope, where there is no enclosing function to qualify with.
fn enclosing_name(node_id: NodeId, semantic: &Semantic<'_>, source: &str) -> Option<String> {
    semantic
        .nodes()
        .ancestors_enumerated(node_id)
        .find_map(|(id, node)| match node.kind() {
            AstKind::Function(f) => {
                let own = f.id.as_ref().map(|b| b.name.to_string());
                Some(infer_name(id, f.span, own, semantic, source))
            }
            AstKind::ArrowFunctionExpression(f) => {
                Some(infer_name(id, f.span, None, semantic, source))
            }
            _ => None,
        })
}

fn callee_text(e: &Expression<'_>, source: &str) -> String {
    fn chain(e: &Expression<'_>, out: &mut Vec<String>) -> bool {
        match e {
            Expression::Identifier(id) => {
                out.push(id.name.to_string());
                true
            }
            Expression::ThisExpression(_) => {
                out.push("this".into());
                true
            }
            Expression::StaticMemberExpression(m) => {
                chain(&m.object, out) && {
                    out.push(m.property.name.to_string());
                    true
                }
            }
            Expression::ChainExpression(c) => match c.expression.as_member_expression() {
                Some(ast::MemberExpression::StaticMemberExpression(m)) => {
                    chain(&m.object, out) && {
                        out.push(m.property.name.to_string());
                        true
                    }
                }
                _ => false,
            },
            Expression::ParenthesizedExpression(p) => chain(&p.expression, out),
            _ => false,
        }
    }
    let mut parts = Vec::new();
    if chain(e, &mut parts) {
        return parts.join(".");
    }
    let s = e.span();
    let text = source
        .get(s.start as usize..(s.end as usize).min(source.len()))
        .unwrap_or("<dynamic>");
    let first = text.split(['(', '\n']).next().unwrap_or("<dynamic>").trim();
    if first.is_empty() {
        "<dynamic>".into()
    } else {
        first.to_owned()
    }
}

struct LoopCtx {
    label: Option<String>,
    /// Where `continue` goes, when known at push time (while / for-each).
    /// `for` loops leave it `None` and patch continues to the update node.
    continue_target: Option<usize>,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct FnLowerer<'f, 'a> {
    file: String,
    name: String,
    decl_line: u32,
    facts: &'f Facts<'a>,
    line_starts: &'f [u32],
    source: &'f str,
    nodes: Vec<Node>,
    vars: BTreeMap<SymbolKey, VarId>,
    next_temp: u32,
    loops: Vec<LoopCtx>,
    /// Set by `LabeledStatement` just before delegating to a loop body;
    /// taken by that loop's own arm when it pushes its `LoopCtx`, so the
    /// label rides along on the real loop context instead of a proxy one.
    pending_label: Option<String>,
}

impl<'f, 'a> FnLowerer<'f, 'a> {
    fn new(
        file: &str,
        name: &str,
        decl_line: u32,
        facts: &'f Facts<'a>,
        line_starts: &'f [u32],
        source: &'f str,
    ) -> Self {
        Self {
            file: file.to_owned(),
            name: name.to_owned(),
            decl_line,
            facts,
            line_starts,
            source,
            nodes: Vec::new(),
            vars: BTreeMap::new(),
            next_temp: 0,
            loops: Vec::new(),
            pending_label: None,
        }
    }

    fn line_of(&self, off: u32) -> u32 {
        u32::try_from(self.line_starts.partition_point(|&s| s <= off)).unwrap_or(u32::MAX)
    }

    fn var(&mut self, key: SymbolKey) -> VarId {
        let next = u32::try_from(self.vars.len()).unwrap_or(u32::MAX);
        *self.vars.entry(key).or_insert(next)
    }

    fn temp(&mut self) -> VarId {
        self.next_temp += 1;
        let t = self.next_temp;
        self.var(SymbolKey::Temp(t))
    }

    fn push(&mut self, node: Node) -> usize {
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

    fn snippet(&self, span: Span) -> String {
        let s = self
            .source
            .get(span.start as usize..(span.end as usize).min(self.source.len()))
            .unwrap_or("");
        let line = s.split('\n').next().unwrap_or("").trim();
        // Truncate at a char boundary - zod's source is full of Unicode, and
        // String::truncate panics mid-code-point. Found by sweeping real code.
        match line.char_indices().nth(48) {
            Some((idx, _)) => line[..idx].to_owned(),
            None => line.to_owned(),
        }
    }

    fn lower_params(&mut self, params: &[SymbolKey]) {
        let mut defs: Vec<VarId> = Vec::new();
        for k in params {
            let id = self.var(k.clone());
            if !defs.contains(&id) {
                defs.push(id);
            }
        }
        self.push(Node {
            line: Some(self.decl_line),
            kind: NodeKind::Pure,
            defs,
            uses: vec![],
            succs: vec![],
            label: "<params>".into(),
        });
    }

    /// Adds `full`'s captured symbols (see `Facts::captures`) to `uses`,
    /// interning each into this function's variable space.
    fn add_captures_of(&mut self, full: Span, uses: &mut Vec<VarId>) {
        for key in self.facts.captures(full) {
            let v = self.var(key);
            if !uses.contains(&v) {
                uses.push(v);
            }
        }
    }

    /// Enriches `uses` with the captures of every nested function directly
    /// contained in `span` - the closure-capture rule from the module docs.
    fn add_capture_uses(&mut self, span: Span, uses: &mut Vec<VarId>) {
        for full in self.facts.nested_fn_full_spans(span) {
            self.add_captures_of(full, uses);
        }
    }

    /// Emits extracted call nodes for `span` and computes the containing
    /// unit's uses. Returns (uses, call node ids in evaluation order).
    fn unit_uses(&mut self, span: Span) -> (Vec<VarId>, Vec<usize>) {
        let call_spans: Vec<(Span, String)> = self
            .facts
            .calls
            .iter()
            .filter(|c| {
                c.span.start >= span.start
                    && c.span.end <= span.end
                    && !self.facts.inside_nested_fn(c.span, span)
            })
            .map(|c| (c.span, c.callee.clone()))
            .collect();

        let mut call_nodes: Vec<usize> = Vec::new();
        let mut call_temps: Vec<(Span, VarId)> = Vec::new();
        for (cspan, callee) in &call_spans {
            let inner: Vec<Span> = call_spans
                .iter()
                .map(|(s, _)| *s)
                .filter(|s| s.start >= cspan.start && s.end <= cspan.end && *s != *cspan)
                .collect();
            let keys: Vec<SymbolKey> = self
                .facts
                .refs
                .iter()
                .filter(|r| r.span.start >= cspan.start && r.span.end <= cspan.end)
                .filter(|r| !self.facts.inside_nested_fn(r.span, span))
                .filter(|r| {
                    !inner
                        .iter()
                        .any(|s| r.span.start >= s.start && r.span.end <= s.end)
                })
                .filter(|r| r.read || !r.write)
                .map(|r| r.key.clone())
                .collect();
            let mut uses: Vec<VarId> = Vec::new();
            for k in keys {
                let v = self.var(k);
                if !uses.contains(&v) {
                    uses.push(v);
                }
            }
            for (s, t) in &call_temps {
                let direct = s.start >= cspan.start
                    && s.end <= cspan.end
                    && !inner
                        .iter()
                        .any(|i| *i != *s && s.start >= i.start && s.end <= i.end);
                if direct && !uses.contains(t) {
                    uses.push(*t);
                }
            }
            self.add_capture_uses(*cspan, &mut uses);
            let temp = self.temp();
            let line = self.line_of(cspan.start);
            let id = self.push(Node {
                line: Some(line),
                kind: NodeKind::Call {
                    callee: callee.clone(),
                    opacity: CallOpacity::Opaque,
                },
                defs: vec![temp],
                uses,
                succs: vec![],
                label: callee.clone(),
            });
            call_nodes.push(id);
            call_temps.push((*cspan, temp));
        }
        for w in call_nodes.windows(2) {
            let (a, b) = (w[0], w[1]);
            if !self.nodes[a].succs.contains(&b) {
                self.nodes[a].succs.push(b);
            }
        }

        let keys: Vec<SymbolKey> = self
            .facts
            .refs
            .iter()
            .filter(|r| r.span.start >= span.start && r.span.end <= span.end)
            .filter(|r| !self.facts.inside_nested_fn(r.span, span))
            .filter(|r| {
                !call_spans
                    .iter()
                    .any(|(s, _)| r.span.start >= s.start && r.span.end <= s.end)
            })
            .filter(|r| r.read || !r.write)
            .map(|r| r.key.clone())
            .collect();
        let mut uses: Vec<VarId> = Vec::new();
        for k in keys {
            let v = self.var(k);
            if !uses.contains(&v) {
                uses.push(v);
            }
        }
        for (i, (cspan, t)) in call_temps.iter().enumerate() {
            let nested = call_spans
                .iter()
                .enumerate()
                .any(|(j, (s, _))| j != i && cspan.start >= s.start && cspan.end <= s.end);
            if !nested && !uses.contains(t) {
                uses.push(*t);
            }
        }
        self.add_capture_uses(span, &mut uses);
        (uses, call_nodes)
    }

    /// Write references inside `span` (assignment targets, updates) plus
    /// declared bindings - the unit's defs.
    fn unit_defs(&mut self, span: Span) -> Vec<VarId> {
        let keys: Vec<SymbolKey> = self
            .facts
            .refs
            .iter()
            .filter(|r| r.span.start >= span.start && r.span.end <= span.end && r.write)
            .filter(|r| !self.facts.inside_nested_fn(r.span, span))
            .map(|r| r.key.clone())
            .collect();
        let mut defs: Vec<VarId> = Vec::new();
        for k in keys {
            let v = self.var(k);
            if !defs.contains(&v) {
                defs.push(v);
            }
        }
        let bind: Vec<u32> = self
            .facts
            .bindings
            .iter()
            .filter(|(s, _)| {
                s.start >= span.start && s.end <= span.end && !self.facts.inside_nested_fn(*s, span)
            })
            .map(|(_, sid)| *sid)
            .collect();
        for sid in bind {
            let v = self.var(SymbolKey::Sym(sid));
            if !defs.contains(&v) {
                defs.push(v);
            }
        }
        defs
    }

    /// Lowers one unit; returns (entry, exit) node ids.
    fn unit(&mut self, span: Span, kind: NodeKind, extra_defs: Vec<VarId>) -> (usize, usize) {
        let (uses, calls) = self.unit_uses(span);
        let mut defs = self.unit_defs(span);
        for d in extra_defs {
            if !defs.contains(&d) {
                defs.push(d);
            }
        }
        let line = self.line_of(span.start);
        let label = self.snippet(span);
        let id = self.push(Node {
            line: Some(line),
            kind,
            defs,
            uses,
            succs: vec![],
            label,
        });
        if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.nodes[last].succs.push(id);
            (first, id)
        } else {
            (id, id)
        }
    }

    fn lower_seq(&mut self, stmts: &'a [Statement<'a>], mut open: Vec<usize>) -> Vec<usize> {
        for s in stmts {
            open = self.lower_stmt(s, open);
        }
        open
    }

    fn take_break(&mut self, label: Option<&str>, node: usize) {
        for ctx in self.loops.iter_mut().rev() {
            if label.is_none() || ctx.label.as_deref() == label {
                ctx.breaks.push(node);
                return;
            }
        }
    }

    fn take_continue(&mut self, label: Option<&str>, node: usize) {
        for i in (0..self.loops.len()).rev() {
            let matches = label.is_none() || self.loops[i].label.as_deref() == label;
            if matches {
                if let Some(t) = self.loops[i].continue_target {
                    self.link(&[node], t);
                } else {
                    self.loops[i].continues.push(node);
                }
                return;
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_stmt(&mut self, s: &'a Statement<'a>, open: Vec<usize>) -> Vec<usize> {
        match s {
            Statement::BlockStatement(b) => self.lower_seq(&b.body, open),
            Statement::EmptyStatement(_) | Statement::DebuggerStatement(_) => open,
            Statement::VariableDeclaration(d) => {
                let mut open = open;
                for decl in &d.declarations {
                    let (entry, exit) = self.unit(decl.span, NodeKind::Pure, vec![]);
                    self.link(&open, entry);
                    open = vec![exit];
                }
                open
            }
            Statement::ExpressionStatement(es) => {
                let span = es.span;
                let (entry, exit) = match &es.expression {
                    Expression::AssignmentExpression(a) => {
                        let member_target = matches!(
                            &a.left,
                            ast::AssignmentTarget::ComputedMemberExpression(_)
                                | ast::AssignmentTarget::StaticMemberExpression(_)
                                | ast::AssignmentTarget::PrivateFieldExpression(_)
                        );
                        let global_target = self.facts.refs.iter().any(|r| {
                            r.span.start >= a.left.span().start
                                && r.span.end <= a.left.span().end
                                && r.write
                                && matches!(r.key, SymbolKey::Global(_))
                        });
                        if member_target || global_target {
                            let target = self.snippet(a.left.span());
                            self.unit(span, NodeKind::StateWrite { target }, vec![])
                        } else {
                            self.unit(span, NodeKind::Pure, vec![])
                        }
                    }
                    Expression::CallExpression(_) | Expression::NewExpression(_) => {
                        // Statement-position call: the call nodes plus a
                        // discard consuming the result, so the inert sweep
                        // can absorb the whole statement when the call is
                        // denylisted.
                        let (uses, calls) = self.unit_uses(span);
                        let line = self.line_of(span.start);
                        let id = self.push(Node {
                            line: Some(line),
                            kind: NodeKind::Pure,
                            defs: vec![],
                            uses,
                            succs: vec![],
                            label: "<discard>".into(),
                        });
                        if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
                            self.nodes[last].succs.push(id);
                            (first, id)
                        } else {
                            (id, id)
                        }
                    }
                    _ => self.unit(span, NodeKind::Pure, vec![]),
                };
                self.link(&open, entry);
                vec![exit]
            }
            Statement::ReturnStatement(r) => {
                let (entry, _) = self.unit(r.span, NodeKind::Return, vec![]);
                self.link(&open, entry);
                vec![]
            }
            Statement::ThrowStatement(t) => {
                let (entry, _) = self.unit(t.span, NodeKind::Throw, vec![]);
                self.link(&open, entry);
                vec![]
            }
            Statement::IfStatement(i) => {
                let (entry, cond) = self.unit(i.test.span(), NodeKind::Branch, vec![]);
                self.link(&open, entry);
                let mut exits = self.lower_stmt(&i.consequent, vec![cond]);
                if let Some(alt) = &i.alternate {
                    exits.extend(self.lower_stmt(alt, vec![cond]));
                } else {
                    exits.push(cond);
                }
                exits
            }
            Statement::WhileStatement(w) => {
                let (entry, cond) = self.unit(w.test.span(), NodeKind::Branch, vec![]);
                self.link(&open, entry);
                self.loops.push(LoopCtx {
                    label: self.pending_label.take(),
                    continue_target: Some(entry),
                    breaks: vec![],
                    continues: vec![],
                });
                let body_exits = self.lower_stmt(&w.body, vec![cond]);
                self.link(&body_exits, entry);
                let ctx = self.loops.pop().expect("while ctx");
                let mut exits = vec![cond];
                exits.extend(ctx.breaks);
                exits
            }
            Statement::DoWhileStatement(d) => {
                let body_entry_marker = self.nodes.len();
                self.loops.push(LoopCtx {
                    label: self.pending_label.take(),
                    continue_target: None,
                    breaks: vec![],
                    continues: vec![],
                });
                let body_exits = self.lower_stmt(&d.body, open);
                let (centry, cond) = self.unit(d.test.span(), NodeKind::Branch, vec![]);
                self.link(&body_exits, centry);
                if body_entry_marker < self.nodes.len() {
                    self.link(&[cond], body_entry_marker);
                }
                let ctx = self.loops.pop().expect("do ctx");
                for c in ctx.continues {
                    self.link(&[c], centry);
                }
                let mut exits = vec![cond];
                exits.extend(ctx.breaks);
                exits
            }
            Statement::ForStatement(f) => {
                let mut open = open;
                if let Some(init) = &f.init {
                    if let ast::ForStatementInit::VariableDeclaration(d) = init {
                        for decl in &d.declarations {
                            let (e2, x2) = self.unit(decl.span, NodeKind::Pure, vec![]);
                            self.link(&open, e2);
                            open = vec![x2];
                        }
                    } else {
                        let span = init.span();
                        let (e2, x2) = self.unit(span, NodeKind::Pure, vec![]);
                        self.link(&open, e2);
                        open = vec![x2];
                    }
                }
                let (centry, cond) = if let Some(test) = &f.test {
                    self.unit(test.span(), NodeKind::Branch, vec![])
                } else {
                    let line = self.line_of(f.span.start);
                    let id = self.push(Node {
                        line: Some(line),
                        kind: NodeKind::Branch,
                        defs: vec![],
                        uses: vec![],
                        succs: vec![],
                        label: "<for>".into(),
                    });
                    (id, id)
                };
                self.link(&open, centry);
                self.loops.push(LoopCtx {
                    label: self.pending_label.take(),
                    continue_target: None,
                    breaks: vec![],
                    continues: vec![],
                });
                let body_exits = self.lower_stmt(&f.body, vec![cond]);
                let after_body: Vec<usize> = if let Some(update) = &f.update {
                    let (ue, ux) = self.unit(update.span(), NodeKind::Pure, vec![]);
                    self.link(&body_exits, ue);
                    vec![ux]
                } else {
                    body_exits
                };
                self.link(&after_body, centry);
                let ctx = self.loops.pop().expect("for ctx");
                let cont_target = after_body.first().copied().unwrap_or(centry);
                for c in ctx.continues {
                    self.link(&[c], cont_target);
                }
                let mut exits = vec![cond];
                exits.extend(ctx.breaks);
                exits
            }
            Statement::ForInStatement(f) => {
                self.lower_for_each(f.left.span(), f.right.span(), &f.body, open)
            }
            Statement::ForOfStatement(f) => {
                self.lower_for_each(f.left.span(), f.right.span(), &f.body, open)
            }
            Statement::BreakStatement(b) => {
                let line = self.line_of(b.span.start);
                let id = self.push(Node {
                    line: Some(line),
                    kind: NodeKind::Pure,
                    defs: vec![],
                    uses: vec![],
                    succs: vec![],
                    label: "break".into(),
                });
                self.link(&open, id);
                self.take_break(b.label.as_ref().map(|l| l.name.as_str()), id);
                vec![]
            }
            Statement::ContinueStatement(c) => {
                let line = self.line_of(c.span.start);
                let id = self.push(Node {
                    line: Some(line),
                    kind: NodeKind::Pure,
                    defs: vec![],
                    uses: vec![],
                    succs: vec![],
                    label: "continue".into(),
                });
                self.link(&open, id);
                self.take_continue(c.label.as_ref().map(|l| l.name.as_str()), id);
                vec![]
            }
            Statement::SwitchStatement(sw) => {
                let (entry, disc) = self.unit(sw.discriminant.span(), NodeKind::Branch, vec![]);
                self.link(&open, entry);
                self.loops.push(LoopCtx {
                    label: None,
                    continue_target: None,
                    breaks: vec![],
                    continues: vec![],
                });
                let mut fall: Vec<usize> = vec![];
                let mut has_default = false;
                for case in &sw.cases {
                    if case.test.is_none() {
                        has_default = true;
                    }
                    let mut into = fall.clone();
                    into.push(disc);
                    fall = self.lower_seq(&case.consequent, into);
                }
                let ctx = self.loops.pop().expect("switch ctx");
                let pending: Vec<usize> = ctx.continues;
                for c in pending {
                    self.take_continue(None, c);
                }
                let mut exits = fall;
                exits.extend(ctx.breaks);
                if !has_default {
                    exits.push(disc);
                }
                exits
            }
            Statement::TryStatement(t) => {
                let try_exits = self.lower_seq(&t.block.body, open);
                if let Some(h) = &t.handler {
                    // Lowered but unreachable - see module docs.
                    let defs = h.param.as_ref().map_or_else(Vec::new, |p| {
                        let span = p.pattern.span();
                        let sids: Vec<u32> = self
                            .facts
                            .bindings
                            .iter()
                            .filter(|(s, _)| s.start >= span.start && s.end <= span.end)
                            .map(|(_, sid)| *sid)
                            .collect();
                        sids.into_iter()
                            .map(|sid| self.var(SymbolKey::Sym(sid)))
                            .collect()
                    });
                    let line = self.line_of(h.span.start);
                    let centry = self.push(Node {
                        line: Some(line),
                        kind: NodeKind::Pure,
                        defs,
                        uses: vec![],
                        succs: vec![],
                        label: "<catch>".into(),
                    });
                    let _ = self.lower_seq(&h.body.body, vec![centry]);
                }
                if let Some(fin) = &t.finalizer {
                    self.lower_seq(&fin.body, try_exits)
                } else {
                    try_exits
                }
            }
            Statement::LabeledStatement(l) => {
                let label = l.label.name.to_string();
                match &l.body {
                    // Loop bodies carry the label on their own `LoopCtx`
                    // (taken from `pending_label` when they push it), so a
                    // labelled `continue`/`break` resolves through
                    // `take_continue`/`take_break` exactly like an
                    // unlabelled one - no proxy context needed.
                    Statement::WhileStatement(_)
                    | Statement::DoWhileStatement(_)
                    | Statement::ForStatement(_)
                    | Statement::ForInStatement(_)
                    | Statement::ForOfStatement(_) => {
                        self.pending_label = Some(label);
                        self.lower_stmt(&l.body, open)
                    }
                    _ => self.lower_stmt(&l.body, open),
                }
            }
            Statement::FunctionDeclaration(f) => {
                let defs =
                    f.id.as_ref()
                        .and_then(|id| id.symbol_id.get())
                        .map(|s| {
                            let v = self
                                .var(SymbolKey::Sym(u32::try_from(s.index()).unwrap_or(u32::MAX)));
                            vec![v]
                        })
                        .unwrap_or_default();
                // A declared function's captures are read when the
                // declaration binds - same enrichment as a closure value,
                // rooted at the function's own full span rather than an
                // enclosing unit's.
                let mut uses: Vec<VarId> = Vec::new();
                self.add_captures_of(f.span, &mut uses);
                let line = self.line_of(f.span.start);
                let id = self.push(Node {
                    line: Some(line),
                    kind: NodeKind::Pure,
                    defs,
                    uses,
                    succs: vec![],
                    label: "<fndecl>".into(),
                });
                self.link(&open, id);
                vec![id]
            }
            Statement::ClassDeclaration(c) => {
                let defs =
                    c.id.as_ref()
                        .and_then(|id| id.symbol_id.get())
                        .map(|s| {
                            let v = self
                                .var(SymbolKey::Sym(u32::try_from(s.index()).unwrap_or(u32::MAX)));
                            vec![v]
                        })
                        .unwrap_or_default();
                let line = self.line_of(c.span.start);
                let id = self.push(Node {
                    line: Some(line),
                    kind: NodeKind::Pure,
                    defs,
                    uses: vec![],
                    succs: vec![],
                    label: "<classdecl>".into(),
                });
                self.link(&open, id);
                vec![id]
            }
            other => {
                // Imports, exports, TS declarations, anything else: a generic
                // unit keeps the CFG connected without pretending to
                // understand it.
                let span = other.span();
                let (entry, exit) = self.unit(span, NodeKind::Pure, vec![]);
                self.link(&open, entry);
                vec![exit]
            }
        }
    }

    fn lower_for_each(
        &mut self,
        left: Span,
        right: Span,
        body: &'a Statement<'a>,
        open: Vec<usize>,
    ) -> Vec<usize> {
        // One Branch node models the iterator advance: defs = the loop
        // bindings, uses = the iterated expression. Loop-carried by
        // construction.
        let mut defs: Vec<VarId> = Vec::new();
        let sids: Vec<u32> = self
            .facts
            .bindings
            .iter()
            .filter(|(s, _)| s.start >= left.start && s.end <= left.end)
            .map(|(_, sid)| *sid)
            .collect();
        for sid in sids {
            let v = self.var(SymbolKey::Sym(sid));
            if !defs.contains(&v) {
                defs.push(v);
            }
        }
        let wkeys: Vec<SymbolKey> = self
            .facts
            .refs
            .iter()
            .filter(|r| r.span.start >= left.start && r.span.end <= left.end && r.write)
            .map(|r| r.key.clone())
            .collect();
        for k in wkeys {
            let v = self.var(k);
            if !defs.contains(&v) {
                defs.push(v);
            }
        }
        let (uses, calls) = self.unit_uses(right);
        let line = self.line_of(left.start);
        let cond = self.push(Node {
            line: Some(line),
            kind: NodeKind::Branch,
            defs,
            uses,
            succs: vec![],
            label: "<for-each>".into(),
        });
        let entry = if let (Some(&first), Some(&last)) = (calls.first(), calls.last()) {
            self.nodes[last].succs.push(cond);
            first
        } else {
            cond
        };
        self.link(&open, entry);
        self.loops.push(LoopCtx {
            label: self.pending_label.take(),
            continue_target: Some(cond),
            breaks: vec![],
            continues: vec![],
        });
        let body_exits = self.lower_stmt(body, vec![cond]);
        self.link(&body_exits, cond);
        let ctx = self.loops.pop().expect("for-each ctx");
        let mut exits = vec![cond];
        exits.extend(ctx.breaks);
        exits
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
}
