"""Line-targeted mutants for `salience calibrate`, as JSON.

Run as: python3 calibrate.py <source.py> <lo-hi[,lo-hi...]> <limit>

The operator set is the portable core of eval/mutation_oracle.py: comparison
flips, arithmetic swaps, boolean connective swaps, constant perturbation and
whole-statement deletion. What the oracle does *after* generating a mutant —
drivers, observables, divergence — stays in eval; here a mutant's fate is
decided by the repository's own test suite, so all this file emits is the
mutated source.

Each mutant is one reversible AST edit, serialized back with `ast.unparse`.
That drops comments and normalizes formatting, which is safe because the
mutant is executed in a throwaway copy and never shown to anyone; it also
means line numbers in the mutated text drift, so every mutant carries the
line of its site in the *original* source — that is the line the panel
scored, and the only one the caller keys on.

Docstrings in statement position are exempt from both constant perturbation
and deletion: CPython lowers them to no instruction at all, so no panel score
exists for a mutant there to check against.

Output is sorted by (line, kind, discovery order) and `<limit>` caps how many
mutated sources are emitted (0 = count sites, emit none); `total_sites` always
reports the uncapped count so the caller can say when it truncated.
"""

import ast
import json
import sys

CMP_SWAP = {
    ast.Lt: ast.LtE, ast.LtE: ast.Lt, ast.Gt: ast.GtE, ast.GtE: ast.Gt,
    ast.Eq: ast.NotEq, ast.NotEq: ast.Eq, ast.In: ast.NotIn, ast.NotIn: ast.In,
    ast.Is: ast.IsNot, ast.IsNot: ast.Is,
}
# The order-reversing second table exists because Lt->LtE only moves the
# boundary case: on data that never hits equality the mutant is equivalent
# and survives for a reason that says nothing about the line.
CMP_SWAP2 = {ast.Lt: ast.Gt, ast.Gt: ast.Lt, ast.LtE: ast.GtE, ast.GtE: ast.LtE}
BIN_SWAP = {
    ast.Add: ast.Sub, ast.Sub: ast.Add, ast.Mult: ast.FloorDiv,
    ast.FloorDiv: ast.Mult, ast.Div: ast.Mult, ast.Mod: ast.FloorDiv,
    ast.BitOr: ast.BitAnd, ast.BitAnd: ast.BitOr, ast.LShift: ast.RShift,
    ast.RShift: ast.LShift, ast.Pow: ast.Mult,
}


class Site:
    """One reversible edit, tagged with its original source line."""

    def __init__(self, line, kind, detail, apply, undo):
        self.line, self.kind, self.detail = line, kind, detail
        self.apply, self.undo = apply, undo


def _op_name(op):
    return type(op).__name__


def _const_variants(value):
    if isinstance(value, bool):
        return [not value]
    if isinstance(value, int):
        return [value + 1, 0] if value != 0 else [1]
    if isinstance(value, float):
        return [value * 1.0001 + 1e-9]
    if isinstance(value, str):
        return [value + "§"] if value else ["§"]
    return []


def _docstring_exprs(tree):
    """Statement-position string constants: def/class/module docstrings."""
    out = set()
    for node in ast.walk(tree):
        body = getattr(node, "body", None)
        if isinstance(body, list) and body:
            first = body[0]
            if (isinstance(first, ast.Expr)
                    and isinstance(first.value, ast.Constant)
                    and isinstance(first.value.value, str)):
                out.add(id(first))
                out.add(id(first.value))
    return out


def collect_sites(tree, spans):
    """Every mutation site whose original line falls inside one of `spans`."""
    docstrings = _docstring_exprs(tree)

    def in_span(node):
        ln = getattr(node, "lineno", None)
        return ln is not None and any(lo <= ln <= hi for lo, hi in spans)

    sites = []
    for node in ast.walk(tree):
        for _field, value in ast.iter_fields(node):
            if not isinstance(value, list):
                continue
            for i, item in enumerate(value):
                if not isinstance(item, ast.stmt) or not in_span(item):
                    continue
                # Deleting a definition is a structural change, not a
                # line-level perturbation; deleting a Pass is a no-op; a
                # docstring never lowers to an instruction.
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef,
                                     ast.ClassDef, ast.Global, ast.Nonlocal,
                                     ast.Pass, ast.Import, ast.ImportFrom)):
                    continue
                if id(item) in docstrings:
                    continue
                sites.append(Site(
                    item.lineno, "delete", "statement -> pass",
                    (lambda lst=value, idx=i: lst.__setitem__(idx, ast.Pass())),
                    (lambda lst=value, idx=i, o=item: lst.__setitem__(idx, o)),
                ))

        if not in_span(node):
            continue

        if isinstance(node, ast.Compare):
            for i, op in enumerate(node.ops):
                for table in (CMP_SWAP, CMP_SWAP2):
                    new = table.get(type(op))
                    if new is None:
                        continue
                    sites.append(Site(
                        node.lineno, "cmp",
                        f"{_op_name(op)} -> {new.__name__}",
                        (lambda n=node, x=i, c=new: n.ops.__setitem__(x, c())),
                        (lambda n=node, x=i, o=op: n.ops.__setitem__(x, o)),
                    ))
        elif isinstance(node, (ast.BinOp, ast.AugAssign)):
            new = BIN_SWAP.get(type(node.op))
            if new is not None:
                kind = "bin" if isinstance(node, ast.BinOp) else "aug"
                sites.append(Site(
                    node.lineno, kind,
                    f"{_op_name(node.op)} -> {new.__name__}",
                    (lambda n=node, c=new: setattr(n, "op", c())),
                    (lambda n=node, o=node.op: setattr(n, "op", o)),
                ))
        elif isinstance(node, ast.BoolOp):
            new = ast.Or if isinstance(node.op, ast.And) else ast.And
            sites.append(Site(
                node.lineno, "bool",
                f"{_op_name(node.op)} -> {new.__name__}",
                (lambda n=node, c=new: setattr(n, "op", c())),
                (lambda n=node, o=node.op: setattr(n, "op", o)),
            ))
        elif isinstance(node, ast.Constant) and id(node) not in docstrings:
            for v in _const_variants(node.value):
                sites.append(Site(
                    node.lineno, "const",
                    f"{node.value!r} -> {v!r}",
                    (lambda n=node, c=v: setattr(n, "value", c)),
                    (lambda n=node, o=node.value: setattr(n, "value", o)),
                ))
    return sites


def main():
    path = sys.argv[1]
    spans = []
    for part in sys.argv[2].split(","):
        lo, _, hi = part.partition("-")
        spans.append((int(lo), int(hi)))
    limit = int(sys.argv[3])

    with open(path, "r", encoding="utf-8") as fh:
        src = fh.read()
    tree = ast.parse(src, path)

    sites = collect_sites(tree, spans)
    # `ast.walk` order is deterministic but breadth-first; sorting by line
    # makes truncation under a budget cut the tail of the file rather than an
    # arbitrary traversal frontier.
    sites.sort(key=lambda s: (s.line, s.kind, s.detail))

    mutants = []
    for site in sites[:limit]:
        site.apply()
        try:
            source = ast.unparse(ast.fix_missing_locations(tree))
        finally:
            site.undo()
        mutants.append({
            "line": site.line,
            "kind": site.kind,
            "detail": site.detail,
            "source": source + "\n",
        })

    json.dump({"file": path, "total_sites": len(sites), "mutants": mutants},
              sys.stdout)


if __name__ == "__main__":
    main()
