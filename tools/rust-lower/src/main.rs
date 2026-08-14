//! MIR -> salience FunctionIr JSON, via rustc_public.
//!
//! This binary is the Rust analogue of `crates/salience-py/src/lower.py`: a
//! substrate-specific lowerer that speaks the neutral JSON contract on stdout
//! so the stable `salience-rs` crate never links compiler internals. It is
//! nightly-pinned (see rust-toolchain.toml beside this crate) because
//! rustc_public is nightly-only even to read; the pin quarantines that fact
//! here, and the main workspace stays stable.
//!
//! Usage: salience-rust-lower <file.rs> [--edition 2021]
//!
//! Design decisions, mirroring the measured ones in the other frontends:
//! - Unwind/cleanup edges are DROPPED. Exception edges destroyed agreement
//!   with expert labels everywhere they were tried; MIR's unwind targets are
//!   the same structure by another name.
//! - StorageLive/StorageDead/Retag/Nop and friends are not emitted: they are
//!   bookkeeping with no salience signal.
//! - An assignment through a Deref projection, or into a static, is a
//!   StateWrite: the value escapes the frame.
//! - Calls carry their callee path so the denylist can recognise logging
//!   (std::io::_print, log::*, tracing::*).

#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;

use std::collections::BTreeMap;
use std::io::Write;
use std::ops::ControlFlow;

use rustc_public::mir::{
    BasicBlockIdx, Body, ConstOperand, Operand, Place, Rvalue, StatementKind, TerminatorKind,
};
use rustc_public::ty::{RigidTy, TyKind};
use rustc_public::{CrateDef, CrateItems};

#[derive(serde::Serialize)]
struct JNode {
    line: Option<u32>,
    kind: JKind,
    defs: Vec<u32>,
    uses: Vec<u32>,
    succs: Vec<usize>,
    label: String,
}

#[derive(serde::Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum JKind {
    Pure,
    Branch,
    Return,
    Throw,
    StateWrite { target: String },
    Call { callee: String, opacity: String },
}

#[derive(serde::Serialize)]
struct JFunction {
    file: String,
    name: String,
    signature: String,
    decl_line: Option<u32>,
    nodes: Vec<JNode>,
    entry: usize,
}

#[derive(serde::Serialize)]
struct JModule {
    file: String,
    functions: Vec<JFunction>,
}

fn place_local(p: &Place) -> u32 {
    p.local as u32
}

fn place_is_escape(p: &Place) -> bool {
    use rustc_public::mir::ProjectionElem;
    p.projection
        .iter()
        .any(|e| matches!(e, ProjectionElem::Deref))
}

fn operand_uses(op: &Operand, out: &mut Vec<u32>) {
    match op {
        Operand::Copy(p) | Operand::Move(p) => {
            let l = place_local(p);
            if !out.contains(&l) {
                out.push(l);
            }
        }
        Operand::Constant(_) | Operand::RuntimeChecks(_) => {}
    }
}

fn rvalue_uses(rv: &Rvalue, out: &mut Vec<u32>) {
    use Rvalue::*;
    match rv {
        Use(op, _) => operand_uses(op, out),
        Repeat(op, _) | Cast(_, op, _) | UnaryOp(_, op) => operand_uses(op, out),
        BinaryOp(_, a, b) | CheckedBinaryOp(_, a, b) => {
            operand_uses(a, out);
            operand_uses(b, out);
        }
        Ref(_, _, p) | AddressOf(_, p) | Len(p) | Discriminant(p) | CopyForDeref(p) => {
            let l = place_local(p);
            if !out.contains(&l) {
                out.push(l);
            }
        }
        Aggregate(_, ops) => {
            for op in ops {
                operand_uses(op, out);
            }
        }
        ThreadLocalRef(_) => {}
        // Future variants: contributing no uses is conservative for salience
        // (the node still exists; it just pulls in less).
        _ => {}
    }
}

fn callee_name(func: &Operand) -> String {
    if let Operand::Constant(ConstOperand { const_, .. }) = func {
        if let TyKind::RigidTy(RigidTy::FnDef(def, _)) = const_.ty().kind() {
            return def.name();
        }
    }
    "<dynamic>".into()
}

/// Line of `span`, or `None` when the span points into another file - a
/// macro expansion whose tokens live in std's source, for instance. Foreign
/// lines projected onto the local file would be nonsense; a node with no
/// line still participates in the analysis and simply projects nowhere,
/// the same policy the JVM frontend applies to SMAP-foreign lines.
fn line_of(span: rustc_public::ty::Span, local_file: &str) -> Option<u32> {
    if span.get_filename() != local_file {
        return None;
    }
    let li = span.get_lines();
    u32::try_from(li.start_line).ok()
}

fn lower_body(name: &str, file: &str, local_file: &str, body: &Body) -> JFunction {
    // First pass: assign node indices per (block, stmt-or-terminator).
    let mut index: BTreeMap<(BasicBlockIdx, usize), usize> = BTreeMap::new();
    let mut count = 0usize;
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, stmt) in block.statements.iter().enumerate() {
            if emit_statement(&stmt.kind) {
                index.insert((bi, si), count);
                count += 1;
            }
        }
        index.insert((bi, usize::MAX), count); // terminator
        count += 1;
    }
    let block_entry = |bi: usize, body: &Body| -> usize {
        for (si, stmt) in body.blocks[bi].statements.iter().enumerate() {
            if emit_statement(&stmt.kind) {
                return index[&(bi, si)];
            }
        }
        index[&(bi, usize::MAX)]
    };

    let mut nodes: Vec<JNode> = Vec::with_capacity(count);
    let mut decl_line = None;
    for (bi, block) in body.blocks.iter().enumerate() {
        // statements chain to the next emitted node in the block, then the
        // terminator.
        let emitted: Vec<usize> = block
            .statements
            .iter()
            .enumerate()
            .filter(|(_, s)| emit_statement(&s.kind))
            .map(|(si, _)| index[&(bi, si)])
            .collect();
        let term_idx = index[&(bi, usize::MAX)];
        for (k, (si, stmt)) in block
            .statements
            .iter()
            .enumerate()
            .filter(|(_, s)| emit_statement(&s.kind))
            .enumerate()
        {
            let succ = emitted.get(k + 1).copied().unwrap_or(term_idx);
            let line = line_of(stmt.source_info.span, local_file);
            if decl_line.is_none() {
                decl_line = line;
            }
            nodes.push(lower_statement(&stmt.kind, line, succ));
            let _ = si;
        }
        let (kind, defs, uses, succs, label) = lower_terminator(&block.terminator.kind, body);
        let succs: Vec<usize> = succs.into_iter().map(|b| block_entry(b, body)).collect();
        nodes.push(JNode {
            line: line_of(block.terminator.source_info.span, local_file),
            kind,
            defs,
            uses,
            succs,
            label,
        });
    }

    JFunction {
        file: file.to_owned(),
        name: name.to_owned(),
        signature: String::new(),
        decl_line,
        nodes,
        entry: 0,
    }
}

fn emit_statement(k: &StatementKind) -> bool {
    matches!(
        k,
        StatementKind::Assign(..) | StatementKind::SetDiscriminant { .. } | StatementKind::Intrinsic(..)
    )
}

fn lower_statement(k: &StatementKind, line: Option<u32>, succ: usize) -> JNode {
    match k {
        StatementKind::Assign(place, rv) => {
            let mut uses = Vec::new();
            rvalue_uses(rv, &mut uses);
            if place_is_escape(place) {
                let mut u2 = vec![place_local(place)];
                for u in uses {
                    if !u2.contains(&u) {
                        u2.push(u);
                    }
                }
                JNode {
                    line,
                    kind: JKind::StateWrite {
                        target: "*deref".into(),
                    },
                    defs: vec![],
                    uses: u2,
                    succs: vec![succ],
                    label: "assign-deref".into(),
                }
            } else {
                JNode {
                    line,
                    kind: JKind::Pure,
                    defs: vec![place_local(place)],
                    uses,
                    succs: vec![succ],
                    label: "assign".into(),
                }
            }
        }
        StatementKind::SetDiscriminant { place, .. } => JNode {
            line,
            kind: JKind::Pure,
            defs: vec![place_local(place)],
            uses: vec![],
            succs: vec![succ],
            label: "set-discriminant".into(),
        },
        StatementKind::Intrinsic(_) => JNode {
            line,
            kind: JKind::Pure,
            defs: vec![],
            uses: vec![],
            succs: vec![succ],
            label: "intrinsic".into(),
        },
        _ => unreachable!("filtered by emit_statement"),
    }
}

fn lower_terminator(
    k: &TerminatorKind,
    _body: &Body,
) -> (JKind, Vec<u32>, Vec<u32>, Vec<BasicBlockIdx>, String) {
    use TerminatorKind::*;
    match k {
        Goto { target } => (JKind::Pure, vec![], vec![], vec![*target], "goto".into()),
        SwitchInt { discr, targets } => {
            let mut uses = Vec::new();
            operand_uses(discr, &mut uses);
            let mut succs: Vec<BasicBlockIdx> = targets.branches().map(|(_, b)| b).collect();
            succs.push(targets.otherwise());
            (JKind::Branch, vec![], uses, succs, "switch".into())
        }
        Return => (JKind::Return, vec![], vec![0], vec![], "return".into()),
        Unreachable | Abort => (JKind::Pure, vec![], vec![], vec![], "unreachable".into()),
        Resume => (JKind::Pure, vec![], vec![], vec![], "resume".into()),
        Drop { place, target, .. } => (
            // A drop can run arbitrary user Drop code: an opaque call.
            JKind::Call {
                callee: "drop".into(),
                opacity: "opaque".into(),
            },
            vec![],
            vec![place_local(place)],
            vec![*target],
            "drop".into(),
        ),
        Call {
            func,
            args,
            destination,
            target,
            ..
        } => {
            let callee = callee_name(func);
            let mut uses = Vec::new();
            for a in args {
                operand_uses(a, &mut uses);
            }
            let defs = vec![place_local(destination)];
            let succs = target.map(|t| vec![t]).unwrap_or_default();
            (
                JKind::Call {
                    callee: callee.clone(),
                    opacity: "opaque".into(),
                },
                defs,
                uses,
                succs,
                callee,
            )
        }
        Assert { cond, target, .. } => {
            let mut uses = Vec::new();
            operand_uses(cond, &mut uses);
            // Unwind side dropped, by design.
            (JKind::Branch, vec![], uses, vec![*target], "assert".into())
        }
        InlineAsm { .. } => (JKind::Pure, vec![], vec![], vec![], "asm".into()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file = args.get(1).expect("usage: salience-rust-lower <file.rs>");
    let edition = args
        .iter()
        .position(|a| a == "--edition")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "2021".into());

    let rustc_args = vec![
        "rustc".into(),
        file.clone(),
        format!("--edition={edition}"),
        "--crate-type=lib".into(),
        "-Zno-codegen".into(),
    ];

    let file_owned = file.clone();
    let result = rustc_public::run!(&rustc_args, || -> ControlFlow<(), ()> {
        let items: CrateItems = rustc_public::all_local_items();
        let mut functions = Vec::new();
        for item in items {
            let Ok(body) = std::panic::catch_unwind(|| item.expect_body()) else {
                continue;
            };
            let name = item.name();
            functions.push(lower_body(&name, &file_owned, &file_owned, &body));
        }
        let module = JModule {
            file: file_owned.clone(),
            functions,
        };
        let mut out = std::io::stdout().lock();
        serde_json::to_writer(&mut out, &module).expect("stdout write");
        out.flush().ok();
        ControlFlow::Continue(())
    });
    if result.is_err() {
        std::process::exit(1);
    }
}
