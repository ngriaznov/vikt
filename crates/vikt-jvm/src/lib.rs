//! JVM bytecode frontend: `.class` bytes to [`FunctionIr`].
//!
//! Bytecode rather than source is the deliberate choice. For Java the two agree
//! closely enough that it hardly matters; for Kotlin they do not. A `suspend`
//! function's real control flow is a state machine the compiler generates, an
//! `inline` function's body is physically copied into each call site, and a
//! `when` may become a `tableswitch` or a chain of comparisons depending on
//! what it matches. An AST shows the syntax the author wrote. The bytecode
//! shows the control flow that actually runs — and the `LineNumberTable` maps
//! it back to the lines the author will edit.
//!
//! Lowering runs over mokapot's MokaIR, an SSA form lifted from the stack
//! machine, so stack shuffling never reaches the core as spurious dataflow.
//!
//! # Compilation requirements
//!
//! Line attribution needs `javac -g` (or at least `-g:lines`), and `kotlinc`
//! emits line numbers by default. Without them the analysis still runs — tiers
//! are computed over the graph regardless — but every instruction projects onto
//! no line, and [`Coverage`](vikt_core::artifact::Coverage) will say so
//! rather than silently reporting an empty map.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
// `CPython`, `MokaIR`, `JSR`, `SMAP` and friends are proper nouns in prose,
// not identifiers.
#![allow(clippy::doc_markdown)]

pub mod coroutine;
pub mod smap;

use std::collections::BTreeMap;

use mokapot::ir::expression::{ArrayOperation, Expression, FieldAccess};
use mokapot::ir::{Identifier, MokaIRMethodExt, MokaInstruction};
use mokapot::jvm::code::ProgramCounter;
use mokapot::jvm::{Method, method};
use vikt_core::ir::{CallOpacity, FunctionId, FunctionIr, Node, NodeKind, VarId};

use crate::coroutine::excise_state_machine;
use crate::smap::{Resolution, SourceMap};

/// Something went wrong reading or lowering a class.
#[derive(Debug, thiserror::Error)]
pub enum JvmError {
    /// The bytes were not a well-formed class file.
    #[error("failed to parse class file: {0}")]
    Parse(String),
    /// mokapot could not lift a method body into MokaIR.
    #[error("failed to lift {method} into IR: {detail}")]
    Brew {
        /// The method that failed.
        method: String,
        /// mokapot's error, rendered.
        detail: String,
    },
}

/// What a lowered class carries beyond its functions.
#[derive(Debug, Clone)]
pub struct LoweredClass {
    /// Source file recorded in the `SourceFile` attribute, if present.
    pub source_file: Option<String>,
    /// The binary name of the class.
    pub binary_name: String,
    /// Which SMAP stratum line numbers were resolved through, if the class
    /// carried a `SourceDebugExtension`.
    ///
    /// Kotlin emits one for any class containing an inlined body. See
    /// [`smap`] for why reading `LineNumberTable` without it produces spans
    /// pointing past the end of the file.
    pub smap_stratum: Option<String>,
    /// How many methods were recognised as coroutine state machines and had
    /// their dispatch excised so that dominance and loop detection see the
    /// source-level shape. See [`coroutine`].
    pub state_machines_excised: usize,
    /// How many instructions were dropped because their line belonged to
    /// another source file inlined into this class.
    ///
    /// Reported rather than hidden: a class where this is large is one whose
    /// map covers less of the body than the instruction count suggests.
    pub foreign_lines_dropped: usize,
    /// One entry per method with a body.
    pub functions: Vec<FunctionIr>,
}

/// Parses a class file and lowers every method with a body.
///
/// # Errors
///
/// Returns [`JvmError::Parse`] if the bytes are not a class file. A method
/// whose body cannot be lifted is skipped rather than failing the class: one
/// unlowerable method should not cost the caller every other method in the
/// file.
pub fn lower_class(bytes: &[u8]) -> Result<LoweredClass, JvmError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let class = mokapot::jvm::Class::from_reader(&mut cursor)
        .map_err(|e| JvmError::Parse(e.to_string()))?;

    let source_file = class.source_file.clone();
    let file = source_file
        .clone()
        .unwrap_or_else(|| "<unknown>".to_owned());
    let binary_name = class.binary_name.to_string();

    // Parse the SMAP before lowering anything: it decides what every line
    // number in this class actually means.
    let source_map = class
        .source_debug_extension
        .as_ref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(SourceMap::parse);

    let mut functions = Vec::new();
    let mut foreign_lines_dropped = 0;
    let mut state_machines_excised = 0;
    for method in &class.methods {
        if method.body.is_none() {
            continue; // abstract or native: nothing to analyze
        }
        if is_generated(method) {
            continue;
        }
        if let Some((ir, dropped, excised)) =
            lower_method(method, &file, &binary_name, source_map.as_ref())
        {
            foreign_lines_dropped += dropped;
            state_machines_excised += usize::from(excised);
            functions.push(ir);
        }
    }
    // Stable order regardless of how the constant pool happened to lay methods
    // out, so the artifact is reproducible.
    functions.sort_by(|a, b| {
        a.id.decl_line
            .cmp(&b.id.decl_line)
            .then_with(|| a.id.name.cmp(&b.id.name))
            .then_with(|| a.id.signature.cmp(&b.id.signature))
    });

    Ok(LoweredClass {
        source_file,
        binary_name,
        smap_stratum: source_map.as_ref().map(|m| m.stratum().to_owned()),
        state_machines_excised,
        foreign_lines_dropped,
        functions,
    })
}

/// Whether a method is compiler-generated plumbing that should not contribute
/// spans.
///
/// The motivating case is Kotlin's default-argument bridge. `withDefaults$default`
/// is `ACC_SYNTHETIC`, and unlike most synthetic members it carries a *full*
/// line table pointing at the same source lines as the real `withDefaults` —
/// measured at 18/18 instructions attributed, against the real method's 7/10.
/// Left in, every line of a function with default arguments is analyzed twice,
/// once through a body that is nothing but argument defaulting and a delegating
/// call.
///
/// Bridge methods are covariant-return forwarders with no semantics of their
/// own, and go for the same reason. Filtering generated members is also what
/// JaCoCo settled on after years of Kotlin coverage bug reports.
///
/// Note this is *not* the same as filtering everything Kotlin generates: data
/// class `equals`/`hashCode`/`copy`/`componentN` are not flagged synthetic at
/// all, but they carry no line table, so they produce no spans regardless.
fn is_generated(method: &Method) -> bool {
    method
        .access_flags
        .intersects(method::AccessFlags::SYNTHETIC | method::AccessFlags::BRIDGE)
}

/// Lowers one method, returning it with the number of instructions whose
/// line belonged to another file. `None` if MokaIR generation fails.
fn lower_method(
    method: &Method,
    file: &str,
    owner: &str,
    source_map: Option<&SourceMap>,
) -> Option<(FunctionIr, usize, bool)> {
    let moka = method.brew().ok()?;
    let body = method.body.as_ref()?;

    // PC-ordered instruction list. `InstructionList` is backed by a `BTreeMap`,
    // so iteration is already sorted by program counter and the node indices we
    // hand out are stable across runs.
    let pcs: Vec<ProgramCounter> = moka.instructions.iter().map(|(pc, _)| *pc).collect();
    let index: BTreeMap<ProgramCounter, usize> =
        pcs.iter().enumerate().map(|(i, pc)| (*pc, i)).collect();

    // The line table maps a starting PC to a line; an instruction takes the
    // line of the nearest entry at or before it. Sorting once turns the lookup
    // into a binary search instead of a scan per instruction.
    let mut line_table: Vec<(u16, u16)> = body
        .line_number_table
        .as_ref()
        .map(|t| {
            t.iter()
                .map(|e| (u16::from(e.start_pc), e.line_number))
                .collect()
        })
        .unwrap_or_default();
    line_table.sort_unstable();

    let mut nodes: Vec<Node> = Vec::with_capacity(pcs.len());
    let mut foreign = 0usize;
    for (pc, insn) in moka.instructions.iter() {
        let (kind, extra_uses) = classify(insn);
        let mut uses: Vec<VarId> = insn.uses().into_iter().map(var_id).collect();
        uses.extend(extra_uses);
        // `uses()` returns a `HashSet`; sorting is what keeps the lowering
        // deterministic rather than dependent on hash iteration order.
        uses.sort_unstable();
        uses.dedup();

        let defs: Vec<VarId> = insn
            .def()
            .map(|v| vec![var_id(Identifier::Local(v))])
            .unwrap_or_default();

        // Raw table line -> real source line. Without the SMAP step this is
        // where Kotlin inline call sites get lines past the end of the
        // file, and where standard-library code gets attributed to the user's.
        let raw_line = line_for(&line_table, *pc);
        let line = match (raw_line, source_map) {
            (Some(l), Some(map)) => match map.resolve(l) {
                Resolution::Resolved(real) => Some(real),
                Resolution::Foreign { .. } => {
                    foreign += 1;
                    None
                }
            },
            (other, _) => other,
        };

        nodes.push(Node {
            line,
            kind,
            defs,
            uses,
            succs: Vec::new(), // filled from the CFG below
            label: format!("{pc}: {insn}"),
        });
    }

    // Successors come from mokapot's CFG, which already includes exception
    // edges. Keeping them makes control dependence conservative in the right
    // direction: a statement guarded by a `try` really can be skipped.
    for edge in moka.control_flow_graph.edges() {
        if let (Some(&from), Some(&to)) = (index.get(&edge.source), index.get(&edge.target)) {
            nodes[from].succs.push(to);
        }
    }
    for node in &mut nodes {
        node.succs.sort_unstable();
        node.succs.dedup();
    }

    // Rewrite a coroutine state machine into its source shape before anything
    // downstream computes dominance over it. Left in, its dispatch makes the
    // graph irreducible and every loop containing a suspension point vanishes.
    let excision = excise_state_machine(&mut nodes);

    let decl_line = nodes.iter().filter_map(|n| n.line).min();
    Some((
        FunctionIr {
            id: FunctionId {
                file: file.to_owned(),
                name: format!("{owner}::{}", method.name),
                signature: method.descriptor.to_string(),
                decl_line,
            },
            nodes,
            entry: 0,
        },
        foreign,
        excision.is_state_machine,
    ))
}

/// Renders a JVM binary name (`java/util/logging/Logger`) the way source and
/// configuration write it (`java.util.logging.Logger`).
///
/// Denylist patterns are written by humans, in the notation humans use. Leaving
/// callees in internal form would mean every pattern silently failed to match —
/// a bug whose only symptom is that nothing is ever classified inert.
fn binary_to_source(name: &str) -> String {
    name.replace('/', ".")
}

/// The line for a program counter: the nearest table entry at or before it.
fn line_for(table: &[(u16, u16)], pc: ProgramCounter) -> Option<u32> {
    let pc = u16::from(pc);
    let idx = table.partition_point(|&(start, _)| start <= pc);
    if idx == 0 {
        return None;
    }
    Some(u32::from(table[idx - 1].1))
}

/// Maps a MokaIR identifier onto a core variable id.
///
/// The mapping is a pure function of the identifier rather than a
/// first-seen counter, so it cannot depend on traversal order. The four
/// identifier spaces are kept disjoint by construction.
fn var_id(id: Identifier) -> VarId {
    match id {
        Identifier::This => 0,
        Identifier::Arg(n) => 1 + VarId::from(n),
        Identifier::Local(v) => 0x0100_0000 + VarId::from(u16::from(v)),
        Identifier::CaughtException(pc) => 0x0200_0000 + VarId::from(u16::from(pc)),
    }
}

/// Classifies a MokaIR instruction into the neutral [`NodeKind`].
///
/// Returns any additional uses the classification implies — a field or array
/// write reads the object and index it writes through, and those reads are real
/// dataflow that the effect depends on.
fn classify(insn: &MokaInstruction) -> (NodeKind, Vec<VarId>) {
    let none = Vec::new();
    match insn {
        // A subroutine return is control flow left over from pre-Java-6 `jsr`
        // and carries no salience of its own, same as a `nop`.
        MokaInstruction::Nop | MokaInstruction::SubroutineRet(_) => (NodeKind::Pure, none),
        MokaInstruction::Return(_) => (NodeKind::Return, none),
        MokaInstruction::Jump { condition, .. } => {
            if condition.is_some() {
                (NodeKind::Branch, none)
            } else {
                (NodeKind::Pure, none)
            }
        }
        MokaInstruction::Switch { .. } => (NodeKind::Branch, none),
        MokaInstruction::Definition { expr, .. } => classify_expr(expr),
    }
}

/// Classifies the expression driving a MokaIR definition.
fn classify_expr(expr: &Expression) -> (NodeKind, Vec<VarId>) {
    match expr {
        // Every call is reported as opaque. A frontend cannot know which
        // callees carry no behavior; that is the denylist's job, applied in the
        // core so one policy governs every language.
        Expression::Call { method, .. } => (
            NodeKind::Call {
                callee: format!(
                    "{}::{}",
                    binary_to_source(&method.owner.to_string()),
                    method.name
                ),
                opacity: CallOpacity::Opaque,
            },
            Vec::new(),
        ),
        // A closure capture is a call into code we do not analyze, and the
        // captured values escape with it.
        Expression::Closure { name, .. } => (
            NodeKind::Call {
                callee: format!("<closure {name}>"),
                opacity: CallOpacity::Opaque,
            },
            Vec::new(),
        ),
        Expression::Throw(_) => (NodeKind::Throw, Vec::new()),
        Expression::Field(access) => match access {
            FieldAccess::WriteStatic { field, .. } => (
                NodeKind::StateWrite {
                    target: format!(
                        "{}#{}",
                        binary_to_source(&field.owner.to_string()),
                        field.name
                    ),
                },
                Vec::new(),
            ),
            FieldAccess::WriteInstance {
                object_ref, field, ..
            } => (
                NodeKind::StateWrite {
                    target: format!(
                        "{}#{}",
                        binary_to_source(&field.owner.to_string()),
                        field.name
                    ),
                },
                object_ref.iter().copied().map(var_id).collect(),
            ),
            FieldAccess::ReadStatic { .. } | FieldAccess::ReadInstance { .. } => {
                (NodeKind::Pure, Vec::new())
            }
        },
        Expression::Array(op) => match op {
            ArrayOperation::Write {
                array_ref, index, ..
            } => (
                NodeKind::StateWrite {
                    target: "<array element>".to_owned(),
                },
                array_ref
                    .iter()
                    .chain(index.iter())
                    .copied()
                    .map(var_id)
                    .collect(),
            ),
            _ => (NodeKind::Pure, Vec::new()),
        },
        // Monitor enter/exit is observable across threads, so it is a state
        // change rather than pure computation.
        Expression::Synchronization(_) => (
            NodeKind::StateWrite {
                target: "<monitor>".to_owned(),
            },
            Vec::new(),
        ),
        Expression::Const(_)
        | Expression::Math(_)
        | Expression::Conversion(_)
        | Expression::New(_)
        | Expression::Subroutine { .. } => (NodeKind::Pure, Vec::new()),
    }
}
