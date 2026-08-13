#!/usr/bin/env python3
"""A per-line importance oracle that does not run through anybody's judgement.

The blind labels in `ground-truth-v*.json` have a defect that no amount of extra
labelling fixes: the person who wrote them also designed the incumbent scorer's
feature set. This file is the answer to that. It measures, for each line of a
function, how much the function's *observable behaviour* changes when that line
is perturbed - and nothing about it depends on anyone's opinion of what matters.

Method, per target function:

  1. Parse the real stdlib module the function lives in.
  2. Enumerate mutation sites inside the function's line span: comparison and
     arithmetic operator swaps, boolean connective swaps, constant perturbation,
     and whole-statement deletion.
  3. Run a driver over a fixed, seeded input corpus against the unmutated module
     to get baseline observables, and record which lines the corpus covers.
  4. For each mutant: rebuild the module in a fresh namespace, re-run the driver,
     and record how far the observable moved, averaged over the corpus. An
     exception where none was raised counts as a total change; two successful
     runs are compared by similarity, so a mutant that drops one file from a
     directory walk scores below one that returns nothing at all.
  5. A line's score is the mean divergence over the mutants that land on it.

The result is a continuous 0..1 *behavioural leverage* per line. It is a
different quantity from a reader's sense of importance and the two are expected
to disagree - a line that only binds a method to a local is behaviourally
load-bearing (delete it and everything raises) while a reader calls it noise.
Reporting both, and their disagreement, is the point.

Accelerator imports (`from _heapq import *`, `from _statistics import
_normal_dist_inv_cdf`) are stripped before execution in both the baseline and
every mutant, so the Python definitions under test are the ones that run.

    ./eval/mutation_oracle.py [--only name,name] [--out truth.json]
"""
import ast
import copy
import difflib
import json
import os
import random
import signal
import string
import sys
import tempfile
from pathlib import Path

sys.setrecursionlimit(10000)

# --------------------------------------------------------------------------
# Input corpora and drivers.
#
# A driver takes the freshly-executed module and one input, and returns a
# comparable observable. It must be deterministic: the same module and input
# must always produce the same observable, or every line looks important.
# --------------------------------------------------------------------------

RNG_SEED = 20260813


def rng():
    return random.Random(RNG_SEED)


def _ints(n, lo=-50, hi=50, size=(0, 24)):
    r = rng()
    return [[r.randint(lo, hi) for _ in range(r.randint(*size))] for _ in range(n)]


def _words(r, n):
    return " ".join(
        "".join(r.choice(string.ascii_lowercase) for _ in range(r.randint(1, 12)))
        for _ in range(n)
    )


def drive_siftup(mod, xs):
    h = list(xs)
    for i in reversed(range(len(h) // 2)):
        mod._siftup(h, i)
    out = []
    while h:
        last = h.pop()
        if h:
            top = h[0]
            h[0] = last
            mod._siftup(h, 0)
            out.append(top)
        else:
            out.append(last)
    return out


def drive_siftdown(mod, xs):
    h = []
    for x in xs:
        h.append(x)
        mod._siftdown(h, 0, len(h) - 1)
    return list(h)


def drive_bisect_right(mod, xs):
    s = sorted(xs)
    plain = [mod.bisect_right(s, v) for v in range(-6, 7)]
    k = sorted(xs, key=abs)
    keyed = [mod.bisect_right(k, v, key=abs) for v in range(-6, 7)]
    sliced = [mod.bisect_right(s, v, 1, max(1, len(s) - 1)) for v in range(-3, 4)]
    return (plain, keyed, sliced)


def drive_insort_right(mod, xs):
    s = []
    for x in xs:
        mod.insort_right(s, x)
    k = []
    for x in xs:
        mod.insort_right(k, x, key=abs)
    return (list(s), list(k))


def _patterns(n):
    r = rng()
    atoms = ["*", "?", "[a-c]", "[!x]", "a", "b", "z", ".", "_", "[]]", "**"]
    return ["".join(r.choice(atoms) for _ in range(r.randint(1, 6))) for _ in range(n)]


NAMES = ["a", "abc", "a.b", "zz", "b_c", "", "x" * 5, "[a]", "A", "a?b", "ac", "]"]


def drive_translate(mod, pat):
    import re

    try:
        rx = mod.translate(pat)
    except Exception as e:  # noqa: BLE001
        return ("translate-raised", type(e).__name__)
    try:
        c = re.compile(rx)
    except Exception as e:  # noqa: BLE001
        return ("compile-raised", type(e).__name__)
    return (rx, tuple(bool(c.match(n)) for n in NAMES))


def _pairs(n):
    r = rng()
    alpha = "abcde"

    def s():
        return "".join(r.choice(alpha) for _ in range(r.randint(0, 30)))

    return [(s(), s()) for _ in range(n)]


def drive_matching_blocks(mod, ab):
    a, b = ab
    m = mod.SequenceMatcher(None, a, b)
    return [tuple(x) for x in m.get_matching_blocks()]


def drive_longest_match(mod, ab):
    a, b = ab
    m = mod.SequenceMatcher(None, a, b)
    return [tuple(m.find_longest_match(0, len(a), 0, len(b)))] + [
        tuple(m.find_longest_match(1, len(a), 1, len(b))) if a and b else ()
    ]


def _texts(n):
    r = rng()
    return [(_words(r, r.randint(3, 30)), r.randint(8, 40)) for _ in range(n)]


def drive_wrap(mod, tw):
    text, width = tw
    return mod.wrap(text, width=width, break_long_words=True)


def drive_long_word(mod, tw):
    text, width = tw
    return mod.wrap(
        text.replace(" ", "")[:80] + " " + text,
        width=max(4, width // 3),
        break_long_words=True,
        break_on_hyphens=True,
    )


def drive_split(mod, tw):
    text, _ = tw
    w = mod.TextWrapper(width=12)
    return w._split(text) + w._split(text.replace(" ", "-"))


def _dedent_inputs(n):
    r = rng()
    out = []
    for _ in range(n):
        lines = []
        for _ in range(r.randint(1, 6)):
            pad = r.choice(["", " ", "  ", "\t", "    ", "\t "])
            lines.append(pad + _words(r, r.randint(0, 4)))
        out.append("\n".join(lines))
    return out


def drive_dedent(mod, text):
    return mod.dedent(text)


def _floats(n):
    r = rng()
    return [[r.uniform(-1e3, 1e3) for _ in range(r.randint(2, 20))] for _ in range(n)]


def drive_sum(mod, xs):
    n, d, t = mod._sum(xs)
    return (n, str(d), str(t))


def _probs(n):
    r = rng()
    return [round(r.uniform(0.001, 0.999), 6) for _ in range(n)]


def drive_inv_cdf(mod, p):
    return round(mod._normal_dist_inv_cdf(p, 0.0, 1.0), 9)


def _urls(n):
    r = rng()
    schemes = ["http", "https", "", "ftp", "mailto", "file"]
    hosts = ["a.com", "[::1]", "u:p@h:8080", "", "h", "1.2.3.4:99"]
    paths = ["/x/y", "", "/", "//z", "/a;b"]
    qs = ["", "?a=1&b=2", "?x", "?a=1;b=2"]
    fr = ["", "#f", "#"]
    out = []
    for _ in range(n):
        s = r.choice(schemes)
        out.append(
            (s + "://" if s else "")
            + r.choice(hosts)
            + r.choice(paths)
            + r.choice(qs)
            + r.choice(fr)
        )
    return out


def drive_urlsplit(mod, u):
    return tuple(mod.urlsplit(u))


def _queries(n):
    r = rng()
    bits = ["a=1", "b", "c=", "=d", "a=1&a=2", "x=%20", "y=+z", "&&", ";q=1"]
    return ["&".join(r.choice(bits) for _ in range(r.randint(1, 5))) for _ in range(n)]


def drive_parse_qsl(mod, q):
    return tuple(mod.parse_qsl(q, keep_blank_values=True))


def _csv_samples(n):
    r = rng()
    out = []
    for _ in range(n):
        header = r.random() < 0.5
        cols = r.randint(2, 5)
        rows = []
        if header:
            rows.append(",".join(f"col{j}" for j in range(cols)))
        for _ in range(r.randint(3, 25)):
            rows.append(
                ",".join(
                    str(r.randint(0, 999))
                    if r.random() < 0.6
                    else "".join(r.choice("abcdef") for _ in range(r.randint(1, 8)))
                    for _ in range(cols)
                )
            )
        out.append("\r\n".join(rows) + "\r\n")
    return out


def drive_has_header(mod, sample):
    try:
        return mod.Sniffer().has_header(sample)
    except Exception as e:  # noqa: BLE001
        return ("raised", type(e).__name__)


_WALK_ROOTS = []


def _walk_trees(n):
    """Build n small directory trees once; reuse them for every mutant."""
    if _WALK_ROOTS:
        return list(_WALK_ROOTS)
    r = rng()
    for i in range(n):
        root = tempfile.mkdtemp(prefix=f"salience-walk-{i}-")
        stack = [root]
        for _ in range(r.randint(3, 12)):
            base = r.choice(stack)
            d = os.path.join(base, f"d{r.randint(0, 99)}")
            os.makedirs(d, exist_ok=True)
            stack.append(d)
            for _ in range(r.randint(0, 3)):
                with open(os.path.join(d, f"f{r.randint(0, 99)}"), "w") as fh:
                    fh.write("x")
        try:
            os.symlink(root, os.path.join(root, "loop"))
        except OSError:
            pass
        _WALK_ROOTS.append(root)
    return list(_WALK_ROOTS)


def drive_walk(mod, root):
    seen = []
    for top, dirs, files in mod._walk(root, None, True, False):
        seen.append(
            (os.path.relpath(top, root), tuple(sorted(dirs)), tuple(sorted(files)))
        )
    for top, dirs, files in mod._walk(root, None, False, True):
        seen.append(
            (os.path.relpath(top, root), tuple(sorted(dirs)), tuple(sorted(files)))
        )
    return sorted(seen)


def _ips(n):
    r = rng()
    out = []
    for _ in range(n):
        style = r.randint(0, 4)
        if style == 0:
            out.append(".".join(str(r.randint(0, 255)) for _ in range(4)))
        elif style == 1:
            out.append(".".join(str(r.randint(0, 999)) for _ in range(4)))
        elif style == 2:
            out.append(".".join(str(r.randint(0, 255)) for _ in range(r.randint(1, 6))))
        elif style == 3:
            out.append("01.2.3.4")
        else:
            out.append("1.2.3.")
    return out


def drive_ipv4(mod, s):
    try:
        return mod.IPv4Address(s)._ip
    except Exception as e:  # noqa: BLE001
        return ("raised", type(e).__name__)


def _jsons(n):
    r = rng()
    out = []
    for _ in range(n):
        body = "".join(
            r.choice(["a", "b", "\\n", "\\u00e9", '\\"', "\\\\", " ", "\\t", "z"])
            for _ in range(r.randint(0, 14))
        )
        out.append('"' + body + '"')
    return out


def drive_scanstring(mod, s):
    try:
        return mod.py_scanstring(s, 1)
    except Exception as e:  # noqa: BLE001
        return ("raised", type(e).__name__)


def _fracs(n):
    r = rng()
    return [
        (r.randint(1, 10**6), r.randint(1, 10**6), r.randint(1, 200)) for _ in range(n)
    ]


def drive_limit_denominator(mod, t):
    a, b, d = t
    try:
        return str(mod.Fraction(a, b).limit_denominator(d))
    except Exception as e:  # noqa: BLE001
        return ("raised", type(e).__name__)


def _structs(n):
    r = rng()

    def gen(depth):
        if depth <= 0:
            return r.choice([1, "a" * r.randint(0, 9), None, True, 1.5, b"z"])
        k = r.randint(0, 3)
        if k == 0:
            return [gen(depth - 1) for _ in range(r.randint(0, 4))]
        if k == 1:
            return {f"k{i}": gen(depth - 1) for i in range(r.randint(0, 4))}
        if k == 2:
            return tuple(gen(depth - 1) for _ in range(r.randint(0, 3)))
        return gen(depth - 1)

    return [gen(3) for _ in range(n)]


def drive_safe_repr(mod, obj):
    p = mod.PrettyPrinter(width=20)
    return p._safe_repr(obj, {}, 2, 0)


def _tokens(n):
    r = rng()
    bits = ["a", '"b c"', "'d'", "e#f", "\\g", "|", ";", "  ", "h\ni", '"', "$x"]
    return ["".join(r.choice(bits) for _ in range(r.randint(1, 8))) for _ in range(n)]


def drive_shlex_split(mod, s):
    try:
        return tuple(mod.split(s))
    except Exception as e:  # noqa: BLE001
        return ("raised", type(e).__name__)


TARGETS = [
    ("heapq", "_siftup", drive_siftup, lambda: _ints(40)),
    ("heapq", "_siftdown", drive_siftdown, lambda: _ints(40)),
    ("bisect", "bisect_right", drive_bisect_right, lambda: _ints(30)),
    ("bisect", "insort_right", drive_insort_right, lambda: _ints(30)),
    ("fnmatch", "translate", drive_translate, lambda: _patterns(40)),
    ("difflib", "SequenceMatcher.get_matching_blocks", drive_matching_blocks,
     lambda: _pairs(30)),
    ("difflib", "SequenceMatcher.find_longest_match", drive_longest_match,
     lambda: _pairs(30)),
    ("textwrap", "TextWrapper._wrap_chunks", drive_wrap, lambda: _texts(30)),
    ("textwrap", "TextWrapper._handle_long_word", drive_long_word, lambda: _texts(20)),
    ("textwrap", "TextWrapper._split", drive_split, lambda: _texts(20)),
    ("textwrap", "dedent", drive_dedent, lambda: _dedent_inputs(30)),
    ("statistics", "_sum", drive_sum, lambda: _floats(30)),
    ("statistics", "_normal_dist_inv_cdf", drive_inv_cdf, lambda: _probs(40)),
    ("urllib.parse", "urlsplit", drive_urlsplit, lambda: _urls(40)),
    ("urllib.parse", "parse_qsl", drive_parse_qsl, lambda: _queries(30)),
    ("csv", "Sniffer.has_header", drive_has_header, lambda: _csv_samples(30)),
    ("os", "_walk", drive_walk, lambda: _walk_trees(6)),
    ("ipaddress", "_BaseV4._ip_int_from_string", drive_ipv4, lambda: _ips(40)),
    ("json.decoder", "py_scanstring", drive_scanstring, lambda: _jsons(40)),
    ("fractions", "Fraction.limit_denominator", drive_limit_denominator,
     lambda: _fracs(30)),
    ("pprint", "PrettyPrinter._safe_repr", drive_safe_repr, lambda: _structs(30)),
    ("shlex", "split", drive_shlex_split, lambda: _tokens(30)),
]

# --------------------------------------------------------------------------
# Mutation machinery
# --------------------------------------------------------------------------

CMP_SWAP = {
    ast.Lt: ast.LtE, ast.LtE: ast.Lt, ast.Gt: ast.GtE, ast.GtE: ast.Gt,
    ast.Eq: ast.NotEq, ast.NotEq: ast.Eq, ast.In: ast.NotIn, ast.NotIn: ast.In,
    ast.Is: ast.IsNot, ast.IsNot: ast.Is,
}
CMP_SWAP2 = {ast.Lt: ast.Gt, ast.Gt: ast.Lt, ast.LtE: ast.GtE, ast.GtE: ast.LtE}
BIN_SWAP = {
    ast.Add: ast.Sub, ast.Sub: ast.Add, ast.Mult: ast.FloorDiv,
    ast.FloorDiv: ast.Mult, ast.Div: ast.Mult, ast.Mod: ast.FloorDiv,
    ast.BitOr: ast.BitAnd, ast.BitAnd: ast.BitOr, ast.LShift: ast.RShift,
    ast.RShift: ast.LShift, ast.Pow: ast.Mult,
}


class Site:
    """One reversible edit to the AST, tagged with the source line it sits on."""

    def __init__(self, line, kind, apply, undo):
        self.line, self.kind, self.apply, self.undo = line, kind, apply, undo


def _const_variants(node):
    v = node.value
    if isinstance(v, bool):
        return [not v]
    if isinstance(v, int):
        return [v + 1, 0] if v != 0 else [1]
    if isinstance(v, float):
        return [v * 1.0001 + 1e-9]
    if isinstance(v, str):
        return [v + "§"] if v else ["§"]
    return []


def collect_sites(tree, lo, hi, with_deletion=True):
    """Every mutation site whose line falls inside [lo, hi]."""
    sites = []

    def in_span(node):
        ln = getattr(node, "lineno", None)
        return ln is not None and lo <= ln <= hi

    for node in ast.walk(tree):
        if with_deletion:
            for _field, value in ast.iter_fields(node):
                if not isinstance(value, list):
                    continue
                for i, item in enumerate(value):
                    if not isinstance(item, ast.stmt) or not in_span(item):
                        continue
                    # Deleting a definition is a structural change, not a
                    # line-level perturbation.
                    if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef,
                                         ast.ClassDef, ast.Global, ast.Nonlocal)):
                        continue
                    sites.append(Site(
                        item.lineno, "delete",
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
                        (lambda n=node, x=i, c=new: n.ops.__setitem__(x, c())),
                        (lambda n=node, x=i, o=op: n.ops.__setitem__(x, o)),
                    ))
        elif isinstance(node, ast.BinOp):
            new = BIN_SWAP.get(type(node.op))
            if new is not None:
                sites.append(Site(
                    node.lineno, "bin",
                    (lambda n=node, c=new: setattr(n, "op", c())),
                    (lambda n=node, o=node.op: setattr(n, "op", o)),
                ))
        elif isinstance(node, ast.AugAssign):
            new = BIN_SWAP.get(type(node.op))
            if new is not None:
                sites.append(Site(
                    node.lineno, "aug",
                    (lambda n=node, c=new: setattr(n, "op", c())),
                    (lambda n=node, o=node.op: setattr(n, "op", o)),
                ))
        elif isinstance(node, ast.BoolOp):
            new = ast.Or if isinstance(node.op, ast.And) else ast.And
            sites.append(Site(
                node.lineno, "bool",
                (lambda n=node, c=new: setattr(n, "op", c())),
                (lambda n=node, o=node.op: setattr(n, "op", o)),
            ))
        elif isinstance(node, ast.Constant):
            for v in _const_variants(node):
                sites.append(Site(
                    node.lineno, "const",
                    (lambda n=node, c=v: setattr(n, "value", c)),
                    (lambda n=node, o=node.value: setattr(n, "value", o)),
                ))
    return sites


def find_span(tree, qualname):
    """Line span of a possibly nested definition, e.g. `Sniffer.has_header`."""
    parts = qualname.split(".")

    def rec(body, parts):
        head, rest = parts[0], parts[1:]
        for n in body:
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) \
                    and n.name == head:
                if not rest:
                    return n
                found = rec(n.body, rest)
                if found is not None:
                    return found
        return None

    n = rec(tree.body, parts)
    if n is None:
        raise KeyError(qualname)
    start = min([n.lineno] + [d.lineno for d in getattr(n, "decorator_list", [])])
    return start, n.end_lineno


def strip_accelerators(tree):
    """Drop the C-accelerator imports, so the Python definitions survive.

    Two shapes matter: `from _heapq import *` at the end of heapq.py, and the
    named form `from _statistics import _normal_dist_inv_cdf`. Both rebind a
    Python definition to a C one; mutating the Python source then changes
    nothing and every line of the function reads as unimportant. Only names the
    module itself defines are stripped, so genuine helper imports survive.
    """
    defined = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            defined.add(node.name)
        elif isinstance(node, ast.Assign):
            for t in node.targets:
                if isinstance(t, ast.Name):
                    defined.add(t.id)

    class T(ast.NodeTransformer):
        def visit_ImportFrom(self, node):
            if not (node.module or "").startswith("_"):
                return node
            if any(a.name == "*" for a in node.names) or \
                    all(a.name in defined for a in node.names):
                # A `pass`, not a deletion: the import is usually the whole body
                # of a `try`, and removing it outright leaves invalid syntax.
                return ast.copy_location(ast.Pass(), node)
            return node

    return T().visit(tree)


class Timeout(Exception):
    pass


def _alarm(signum, frame):
    raise Timeout()


def build_module(tree, path, name):
    ns = {"__name__": name, "__file__": path, "__builtins__": __builtins__,
          "__package__": "", "__spec__": None, "__doc__": None}
    code = compile(ast.fix_missing_locations(copy.deepcopy(tree)), path, "exec")
    exec(code, ns)  # noqa: S102
    return type(sys)("m"), ns


class NS:
    def __init__(self, d):
        self.__dict__ = d


def covered_lines(driver, ns, inputs, path):
    """Lines of `path` the driver actually executes on the unmutated module.

    A mutation on a line the corpus never reaches cannot change anything, so
    such a line would score 0 for a reason that says nothing about the line.
    Standard mutation-testing hygiene: measure coverage, judge only what is
    covered.
    """
    hit = set()

    def trace(frame, event, arg):
        if frame.f_code.co_filename == path:
            hit.add(frame.f_lineno)
            return trace
        return None

    m = NS(ns)
    sys.settrace(trace)
    try:
        for inp in inputs:
            signal.setitimer(signal.ITIMER_REAL, 20.0)
            try:
                driver(m, inp)
            except Exception:  # noqa: BLE001
                pass
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
    finally:
        sys.settrace(None)
    return hit


def divergence(a, b):
    """How far apart two observables are, in 0..1.

    Equality alone is too blunt to rank with. `os._walk` returns the whole
    directory traversal and essentially every mutation changes it *somehow*, so
    a boolean kill test scores forty of its lines at exactly 1.0 and ranks
    nothing. Comparing observables by similarity instead distinguishes a mutant
    that drops one file from one that returns nothing at all.

    A raised exception where none was raised is a total change, as are two
    different exception types. Only two successful runs are compared by content.
    """
    if a == b:
        return 0.0
    if a[0] != "ok" or b[0] != "ok":
        return 1.0
    return 1.0 - difflib.SequenceMatcher(None, a[1], b[1]).ratio()


def observe(driver, ns, inputs, per_call_seconds=2.0, abort_after_timeouts=None):
    """Run the driver over the corpus. Returns (observables, aborted_at).

    A mutant that flips a loop condition does not fail, it hangs, and paying the
    full timeout on every input costs more than the rest of the run. Once
    `abort_after_timeouts` calls have hung the mutant is judged non-terminating
    and the remaining inputs are not attempted.
    """
    out = []
    timeouts = 0
    m = NS(ns)
    for i, inp in enumerate(inputs):
        signal.setitimer(signal.ITIMER_REAL, per_call_seconds)
        try:
            out.append(("ok", repr(driver(m, inp))))
        except Timeout:
            out.append(("timeout", ""))
            timeouts += 1
        except RecursionError:
            out.append(("recursion", ""))
        except Exception as e:  # noqa: BLE001
            out.append(("exc", type(e).__name__))
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
        if abort_after_timeouts and timeouts >= abort_after_timeouts:
            return out, i + 1
    return out, None


def run_target(module, qualname, driver, make_inputs, verbose=True):
    mod = __import__(module, fromlist=["x"])
    path = mod.__file__
    src = Path(path).read_text()
    tree = strip_accelerators(ast.parse(src))
    lo, hi = find_span(tree, qualname)
    inputs = make_inputs()

    _, base_ns = build_module(tree, path, "mut_" + module.replace(".", "_"))
    base, _ = observe(driver, base_ns, inputs)
    ok = sum(1 for k, _ in base if k == "ok")
    if ok < max(3, len(inputs) // 3):
        return None, f"driver failed on baseline ({ok}/{len(inputs)} ok)"
    if len({v for _, v in base}) < 2:
        return None, "driver observable is constant across the corpus"

    # Coverage is measured against a *fresh* namespace. `urllib.parse.urlsplit`
    # is `lru_cache`d, so re-running the baseline corpus on the same module
    # returns cached results without executing a line, and everything reads as
    # uncovered.
    _, cov_ns = build_module(tree, path, "cov_" + module.replace(".", "_"))
    covered = covered_lines(driver, cov_ns, inputs, path)

    sites = collect_sites(tree, lo, hi)
    per_line = {}
    for si, site in enumerate(sites):
        site.apply()
        try:
            _, ns = build_module(tree, path, "mut_" + module.replace(".", "_"))
            got, aborted = observe(driver, ns, inputs, per_call_seconds=0.4,
                                   abort_after_timeouts=3)
            frac = 1.0 if aborted else \
                sum(divergence(a, b) for a, b in zip(base, got)) / len(inputs)
        except Timeout:
            frac = 1.0
        except Exception:  # noqa: BLE001
            # The mutant does not even import: a syntactically valid but
            # semantically fatal change. Maximal behavioural leverage.
            frac = 1.0
        finally:
            site.undo()
        per_line.setdefault(site.line, []).append((site.kind, frac))
        if verbose and si % 25 == 0:
            print(f"    {si}/{len(sites)}", file=sys.stderr, flush=True)

    lines = {}
    for ln, hits in sorted(per_line.items()):
        if ln < lo or ln > hi:
            continue
        vals = [f for _, f in hits]
        nodel = [f for k, f in hits if k != "delete"]
        lines[ln] = {
            "leverage": round(sum(vals) / len(vals), 4),
            "leverage_no_delete": round(sum(nodel) / len(nodel), 4) if nodel else None,
            "mutants": len(vals),
            "covered": ln in covered,
            "kinds": sorted({k for k, _ in hits}),
        }
    n_cov = sum(1 for m in lines.values() if m["covered"])
    return {
        "file": path, "name": qualname, "start": lo, "end": hi,
        "inputs": len(inputs), "mutants": len(sites),
        "covered_lines": n_cov, "lines": lines,
    }, None


def main():
    only = None
    if "--only" in sys.argv:
        only = set(sys.argv[sys.argv.index("--only") + 1].split(","))
    out_path = Path(sys.argv[sys.argv.index("--out") + 1]) if "--out" in sys.argv \
        else Path(__file__).parent / "mutation-oracle.json"

    signal.signal(signal.SIGALRM, _alarm)
    results, skipped = [], []
    for module, qualname, driver, make_inputs in TARGETS:
        key = f"{module}.{qualname}"
        if only and key not in only and qualname not in only:
            continue
        print(f"  {key}", file=sys.stderr, flush=True)
        try:
            res, err = run_target(module, qualname, driver, make_inputs)
        except Exception as e:  # noqa: BLE001
            res, err = None, f"{type(e).__name__}: {e}"
        if res is None:
            print(f"    SKIP {err}", file=sys.stderr)
            skipped.append((key, err))
            continue
        print(f"    {len(res['lines'])} lines, {res['mutants']} mutants",
              file=sys.stderr)
        results.append(res)

    doc = {
        "schema": "salience-mutation-oracle/v1",
        "seed": RNG_SEED,
        "python": sys.version.split()[0],
        "functions": results,
        "skipped": skipped,
    }
    out_path.write_text(json.dumps(doc, indent=1))
    print(f"\nwrote {out_path}: {len(results)} functions, "
          f"{sum(len(r['lines']) for r in results)} lines", file=sys.stderr)


if __name__ == "__main__":
    main()
