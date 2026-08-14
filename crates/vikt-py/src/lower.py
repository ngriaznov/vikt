"""Lower CPython bytecode into vikt-core's neutral IR, as JSON.

Run as: python3 lower.py <source.py>

Why bytecode and not the AST: the same argument that applies to Kotlin applies
here, if less dramatically. Comprehensions become nested code objects, `with`
becomes a setup/cleanup pair with exception edges, `and`/`or` become jumps, and
a `for` loop's real exit test is `FOR_ITER`. The bytecode is what runs, and
since PEP 626 every instruction carries the source line it came from.

The one genuine approximation is the abstract stack. CPython is a stack
machine, so `x = a + b` never mentions `a` and `b` in the same instruction as
`x`; the dependency runs through stack slots. This module simulates the stack
symbolically, giving each pushed value a virtual variable id, so the core sees
`STORE_FAST x` using the value that `BINARY_OP` produced from `a` and `b`.
Opcodes with an exact pop/push shape are listed below; anything else falls back
to `dis.stack_effect`, which gives the net change but not the split, so pops are
estimated. That fallback is why coverage is reported honestly in the artifact
rather than assumed complete.
"""

import dis
import json
import os
import sys

# Variable id spaces, kept disjoint so the core never confuses a local with a
# global or with a stack temporary.
LOCAL_BASE = 0x0100_0000
GLOBAL_BASE = 0x0200_0000
CELL_BASE = 0x0300_0000
STACK_BASE = 0x0400_0000

# Opcodes whose (pops, pushes) shape we state exactly, because they are the ones
# carrying dataflow worth being right about.
EXACT = {
    "NOP": (0, 0),
    "RESUME": (0, 0),
    "CACHE": (0, 0),
    "PRECALL": (0, 0),
    "PUSH_NULL": (0, 1),
    "POP_TOP": (1, 0),
    "COPY": (0, 1),
    "SWAP": (0, 0),
    "LOAD_CONST": (0, 1),
    "LOAD_FAST": (0, 1),
    "LOAD_FAST_CHECK": (0, 1),
    "LOAD_FAST_AND_CLEAR": (0, 1),
    "LOAD_GLOBAL": (0, 1),
    "LOAD_NAME": (0, 1),
    "LOAD_DEREF": (0, 1),
    "LOAD_CLOSURE": (0, 1),
    "STORE_FAST": (1, 0),
    "STORE_GLOBAL": (1, 0),
    "STORE_NAME": (1, 0),
    "STORE_DEREF": (1, 0),
    "DELETE_FAST": (0, 0),
    "LOAD_ATTR": (1, 1),
    "LOAD_METHOD": (1, 2),
    "STORE_ATTR": (2, 0),
    "STORE_SUBSCR": (3, 0),
    "BINARY_OP": (2, 1),
    "BINARY_SUBSCR": (2, 1),
    "COMPARE_OP": (2, 1),
    "IS_OP": (2, 1),
    "CONTAINS_OP": (2, 1),
    "UNARY_NEGATIVE": (1, 1),
    "UNARY_NOT": (1, 1),
    "UNARY_INVERT": (1, 1),
    "RETURN_VALUE": (1, 0),
    "RETURN_CONST": (0, 0),
    "YIELD_VALUE": (1, 1),
    "GET_ITER": (1, 1),
    "FOR_ITER": (0, 1),
    "POP_JUMP_IF_TRUE": (1, 0),
    "POP_JUMP_IF_FALSE": (1, 0),
    "POP_JUMP_IF_NONE": (1, 0),
    "POP_JUMP_IF_NOT_NONE": (1, 0),
    "JUMP_IF_TRUE_OR_POP": (0, 0),
    "JUMP_IF_FALSE_OR_POP": (0, 0),
    "JUMP_FORWARD": (0, 0),
    "JUMP_BACKWARD": (0, 0),
    "RAISE_VARARGS": (1, 0),
    "RERAISE": (1, 0),
    "MAKE_FUNCTION": (1, 1),
    "BUILD_LIST": None,  # arg-dependent, handled below
    "BUILD_TUPLE": None,
    "BUILD_SET": None,
    "BUILD_MAP": None,
    "BUILD_STRING": None,
    "CALL": None,
    "CALL_FUNCTION_EX": None,
    "KW_NAMES": (0, 0),
}

# Opcodes that provably cannot raise, so they need no edge to an enclosing
# handler. Everything not listed here is assumed to be able to raise, which is
# the conservative direction for a set used to *remove* edges.
#
# This exclusion is not an optimisation, it is a correctness fix. Giving every
# instruction inside a `try` an edge to the handler — the textbook conservative
# construction — makes every one of them a multi-successor node, which inflates
# control dependence and destroys the dominance structure the whole analysis
# rests on. Measured on the blind expert labels: the naive version dropped
# os._walk from rho 0.428 to 0.073, because five try blocks cover most of that
# function. Restricting edges to instructions that can genuinely raise keeps the
# handler reachable, which was the point, without turning straight-line code
# into a thicket of branches.
NON_RAISING = {
    "NOP", "RESUME", "CACHE", "PRECALL", "KW_NAMES", "POP_TOP", "COPY", "SWAP",
    "PUSH_NULL", "LOAD_CONST", "LOAD_FAST", "STORE_FAST", "JUMP_FORWARD",
    "JUMP_BACKWARD", "JUMP_BACKWARD_NO_INTERRUPT", "POP_JUMP_IF_TRUE",
    "POP_JUMP_IF_FALSE", "POP_JUMP_IF_NONE", "POP_JUMP_IF_NOT_NONE",
    "JUMP_IF_TRUE_OR_POP", "JUMP_IF_FALSE_OR_POP", "RETURN_VALUE",
    "RETURN_CONST", "IS_OP", "MAKE_CELL", "COPY_FREE_VARS", "RETURN_GENERATOR",
}

TERMINATORS = {"RETURN_VALUE", "RETURN_CONST", "RAISE_VARARGS", "RERAISE"}
UNCONDITIONAL_JUMPS = {"JUMP_FORWARD", "JUMP_BACKWARD", "JUMP_BACKWARD_NO_INTERRUPT"}
CONDITIONAL_JUMPS = {
    "POP_JUMP_IF_TRUE",
    "POP_JUMP_IF_FALSE",
    "POP_JUMP_IF_NONE",
    "POP_JUMP_IF_NOT_NONE",
    "JUMP_IF_TRUE_OR_POP",
    "JUMP_IF_FALSE_OR_POP",
    "FOR_ITER",
}


def shape(instr):
    """(pops, pushes) for one instruction."""
    name = instr.opname
    arg = instr.arg or 0
    if name in ("BUILD_LIST", "BUILD_TUPLE", "BUILD_SET", "BUILD_STRING"):
        return (arg, 1)
    if name == "BUILD_MAP":
        return (arg * 2, 1)
    if name == "CALL":
        # callable, self-or-NULL, then `arg` positional arguments.
        return (arg + 2, 1)
    if name == "CALL_FUNCTION_EX":
        return (3 + (1 if arg & 0x01 else 0), 1)
    if name == "LOAD_GLOBAL":
        # Since 3.11 the low arg bit asks for a NULL to be pushed below the
        # global, so the call sequence is `[NULL, callable, *args]`.
        return (0, 2 if arg & 0x01 else 1)
    exact = EXACT.get(name)
    if exact is not None:
        return exact
    # Fallback: `stack_effect` gives the net change but not how it splits.
    # Assume a binary-ish consumer, which is right far more often than not for
    # the opcodes that reach here.
    try:
        net = dis.stack_effect(instr.opcode, instr.arg)
    except (ValueError, TypeError):
        net = 0
    pops = 2 if net < 0 else 1
    pushes = max(0, pops + net)
    return (pops, pushes)


def resolve_callee(names, argc):
    """Name of the callable a `CALL argc` is about to invoke.

    Read off the simulated stack rather than guessed by walking backwards. At a
    `CALL`, CPython has arranged either `[method, self, *args]` or
    `[NULL, callable, *args]`, so the callable sits at one of the two slots
    below the arguments. Checking both and taking the first named one covers
    each shape without having to know which the compiler chose.

    A miss returns `<dynamic>`, which is the safe direction: an unnamed callee
    is simply opaque, and opaque is the default.
    """
    for depth in (argc + 2, argc + 1):
        if depth <= len(names):
            name = names[-depth]
            if name:
                return name
    return "<dynamic>"


def lower_code(code, file):
    """Lower one code object into a neutral function record."""
    bytecode = dis.Bytecode(code)
    instrs = list(bytecode)
    if not instrs:
        return None
    offsets = {ins.offset: idx for idx, ins in enumerate(instrs)}

    # EXC_EDGE_NOTE: exception edges are OFF by default, and that is a measured
    # decision rather than an oversight.
    #
    # Without them, an `except` body is unreachable from the entry, so its
    # definitions never propagate: lines inside a handler were 7x more likely to
    # score <=0.02 than any other line (14% vs 2%), and csv.has_header's vote
    # accumulator — computed entirely inside except/else — scored 0.00 with the
    # reason "result reaches no effect" while being returned four lines later.
    # That is a real defect, and turning this on fixes it.
    #
    # Turning it on also makes the analysis markedly WORSE at agreeing with a
    # reader. Measured against the blind expert labels, incumbent scorer:
    #
    #   exception edges      _siftup   has_header   _walk    mean
    #   off                    0.210      0.343     0.428    0.327
    #   every instruction      0.210      0.097     0.073    0.127
    #   raising instrs only    0.210      0.101     0.078    0.130
    #   one per try block      0.210      0.101     0.066    0.125
    #
    # Edge count is not the cause — one edge per protected region is as damaging
    # as thirty. The cause is structural: making a handler reachable destroys
    # post-dominance, and control dependence is computed from post-dominance, so
    # everything inside and after a `try` becomes control-dependent on the
    # possibility of an exception. Since the handler usually contains a `return`
    # or a re-raise, it also becomes an effect anchor and pulls the backward
    # slice toward error paths.
    #
    # The deeper point is worth stating plainly: the semantically faithful CFG
    # is not the most useful one for importance. Exceptional control flow is
    # pervasive but peripheral — a reader labels `onerror(error); return` as
    # importance 5, not 10 — and modelling it faithfully lets a rare path
    # dominate the structure of a function whose ordinary path is what matters.
    # Coverage tools include these edges because they must; a importance map is
    # answering a different question and is better off without them.
    #
    # Kept behind a flag rather than deleted, because the right fix is probably
    # to use two graphs — exception edges for reaching definitions so handler
    # bodies are not dead, and no exception edges for dominance and control
    # dependence — which the core's single-successor-list model cannot express
    # today.
    # entry, so its definitions never propagate and the whole handler reads as
    # dead. Measured before this fix: lines inside a handler were 7x more likely
    # to score <=0.02 than any other line (14% vs 2%), and in csv.has_header —
    # whose entire vote is computed inside except/else clauses — the accumulator
    # scored 0.00 with the reason "result reaches no effect" while being
    # returned four lines later.
    #
    # CPython 3.11 replaced the SETUP_FINALLY blocks with a compact exception
    # table, exposed here as `Bytecode.exception_entries`. Each entry covers a
    # half-open offset range that can transfer to `target` at any point, so
    # every instruction in range gets an edge to the handler. That is the
    # conservative reading, and conservative is the right direction: an edge
    # that cannot be taken costs a little precision, a missing one silently
    # deletes a whole branch of the program.
    exception_edges = {}
    # OFF BY DEFAULT. Opt in with VIKT_EXC_EDGES=1. See EXC_EDGE_NOTE.
    if os.environ.get("VIKT_EXC_EDGES"):
        exception_entries = getattr(bytecode, "exception_entries", ())
    else:
        exception_entries = ()
    for entry in exception_entries:
        target = offsets.get(entry.target)
        if target is None:
            continue
        # ONE edge per protected region, from its last raising instruction,
        # rather than one per instruction. See EXC_EDGE_NOTE below.
        last = None
        for idx, ins in enumerate(instrs):
            if entry.start <= ins.offset < entry.end and ins.opname not in NON_RAISING:
                last = idx
        if last is not None:
            exception_edges.setdefault(last, set()).add(target)

    nodes = []
    # Abstract stack of virtual variable ids, one entry per live stack slot,
    # plus a parallel stack of the source-level name each slot came from (or
    # None). The names exist only to resolve call targets for denylist
    # matching; they never influence tiering directly.
    stack = []
    names = []
    next_temp = [0]

    def push(name=None):
        vid = STACK_BASE + next_temp[0]
        next_temp[0] += 1
        stack.append(vid)
        names.append(name)
        return vid

    for i, ins in enumerate(instrs):
        pops, pushes = shape(ins)
        uses = []
        defs = []

        # Resolve the call target before popping, while the callable is still
        # on the stack where CPython left it.
        pending_call = None
        if ins.opname == "CALL":
            pending_call = resolve_callee(names, ins.arg or 0)
        elif ins.opname == "CALL_FUNCTION_EX":
            pending_call = resolve_callee(names, 1)

        # Pop first: whatever this instruction consumes is a use.
        popped_name = []
        for _ in range(min(pops, len(stack))):
            uses.append(stack.pop())
            popped_name.append(names.pop())

        name = ins.opname
        kind = {"t": "pure"}
        pushed_name = None

        if name in ("LOAD_FAST", "LOAD_FAST_CHECK", "LOAD_FAST_AND_CLEAR"):
            uses.append(LOCAL_BASE + (ins.arg or 0))
            pushed_name = str(ins.argval)
        elif name in ("LOAD_GLOBAL", "LOAD_NAME"):
            uses.append(GLOBAL_BASE + ((ins.arg or 0) >> 1))
            pushed_name = str(ins.argval)
        elif name in ("LOAD_DEREF", "LOAD_CLOSURE"):
            uses.append(CELL_BASE + (ins.arg or 0))
            pushed_name = str(ins.argval)
        elif name in ("LOAD_METHOD", "LOAD_ATTR"):
            # The receiver's name was just popped; reuse it to build a dotted
            # path like `LOG.info`, which is the shape a denylist pattern has.
            base = popped_name[0] if popped_name else None
            pushed_name = f"{base}.{ins.argval}" if base else str(ins.argval)
        elif name in ("STORE_FAST", "DELETE_FAST"):
            defs.append(LOCAL_BASE + (ins.arg or 0))
        elif name in ("STORE_GLOBAL", "STORE_NAME"):
            # A module-level binding outlives the call: an effect.
            kind = {"t": "state_write", "target": f"global {ins.argval}"}
        elif name == "STORE_DEREF":
            kind = {"t": "state_write", "target": f"cell {ins.argval}"}
        elif name == "STORE_ATTR":
            kind = {"t": "state_write", "target": f"attr .{ins.argval}"}
        elif name == "STORE_SUBSCR":
            kind = {"t": "state_write", "target": "subscript"}
        elif name in ("RETURN_VALUE", "RETURN_CONST"):
            kind = {"t": "return"}
        elif name in ("YIELD_VALUE", "YIELD_FROM", "SEND"):
            # A generator's yields ARE its output. Lowering them as pure
            # computation made os._walk's three `yield` statements — the entire
            # observable behaviour of the function — score 0.01, 0.22 and 0.31
            # against an expert label of 10/10. Modelled as a write to storage
            # outside the function, which is what handing a value to the caller
            # is, and which makes it a hard effect that anchors the slice.
            kind = {"t": "state_write", "target": "<yielded value>"}
        elif name in ("RAISE_VARARGS", "RERAISE"):
            kind = {"t": "throw"}
        elif name in ("CALL", "CALL_FUNCTION_EX"):
            kind = {
                "t": "call",
                "callee": pending_call or "<dynamic>",
                "opacity": "opaque",
            }
            pushed_name = None
        elif name in CONDITIONAL_JUMPS:
            kind = {"t": "branch"}

        # LOAD_GLOBAL with the low arg bit set pushes a NULL below the global,
        # and LOAD_METHOD pushes the bound method below the receiver. In both
        # shapes the *last* pushed slot is the named one for our purposes, so
        # only the final push carries the name.
        for k in range(pushes):
            defs.append(push(pushed_name if k == pushes - 1 else None))

        # Successors: fall-through unless the instruction always transfers
        # control, plus the jump target when there is one.
        succs = []
        if name not in TERMINATORS and name not in UNCONDITIONAL_JUMPS:
            if i + 1 < len(instrs):
                succs.append(i + 1)
        if ins.opcode in dis.hasjrel or ins.opcode in dis.hasjabs:
            target = ins.argval
            if isinstance(target, int) and target in offsets:
                succs.append(offsets[target])
        succs.extend(exception_edges.get(i, ()))

        nodes.append(
            {
                "line": ins.starts_line if hasattr(ins, "starts_line") else None,
                "kind": kind,
                "defs": sorted(set(defs)),
                "uses": sorted(set(uses)),
                "succs": sorted(set(succs)),
                "label": f"{ins.offset}: {name} {ins.argrepr}".strip(),
            }
        )

    # `starts_line` is only set on the first instruction of each line; carry it
    # forward so every instruction knows where it came from. This is the
    # `co_lines()` contract: ranges are non-decreasing and consecutive.
    current = None
    for node in nodes:
        if node["line"] is not None:
            current = node["line"]
        else:
            node["line"] = current

    return {
        "name": code.co_qualname if hasattr(code, "co_qualname") else code.co_name,
        "signature": "",
        "decl_line": code.co_firstlineno,
        "entry": 0,
        "nodes": nodes,
    }


def walk(code, file, out):
    """Lower this code object and every nested one (functions, comprehensions)."""
    rec = lower_code(code, file)
    if rec is not None:
        out.append(rec)
    for const in code.co_consts:
        if hasattr(const, "co_code"):
            walk(const, file, out)


def main():
    path = sys.argv[1]
    with open(path, "r", encoding="utf-8") as fh:
        src = fh.read()
    code = compile(src, path, "exec")
    functions = []
    walk(code, path, functions)
    functions.sort(key=lambda f: (f["decl_line"], f["name"]))
    json.dump({"file": path, "functions": functions}, sys.stdout)


if __name__ == "__main__":
    main()
