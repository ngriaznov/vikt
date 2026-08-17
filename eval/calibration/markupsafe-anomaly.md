# The markupsafe calibration anomaly

Task: diagnose why the panel loses to the positional null on `eval/calibration/markupsafe.jsonl`
(`panel_rho 0.123` vs `null_rho 0.262`, function scope, `verdict: uncalibrated`).

## Reproduction

```
$ git clone https://github.com/pallets/markupsafe.git /workspace/corpus/markupsafe
$ cd /workspace/corpus/markupsafe && git checkout b2e4d9c7687be25695fffbe93a37622302b24fb1
$ cd /workspace/vikt-r-anomaly && cargo build --release -p vikt-cli

$ ./target/release/vikt calibrate /workspace/corpus/markupsafe \
    --test-cmd "PYTHONPATH=src python3 -m pytest -x -q" \
    --scope function --lowering primary \
    --emit-dataset /tmp/markupsafe-repro.jsonl
...
calibrate: 64 functions scored, 12 sampled (largest first)
calibrate: budget of 150 mutants reached: 150 of 199 candidate sites will run
calibrate: 150 mutants executed: 60 killed, 88 survived, 2 timed out (timeouts count as killed)
calibrate: 85 mutated lines carry a panel score, 8 carry none and are excluded

pooled Spearman rho over 85 lines:
  panel             0.123
  positional null   0.262

verdict: uncalibrated — panel rho 0.123 does not beat the positional null (0.262) on this tree
```

`diff` against the checked-in `eval/calibration/markupsafe.jsonl` on `(file, function, line,
panel, kill_rate)` is empty — this is a byte-exact reproduction, not a re-measurement that
happens to land near the recorded numbers. No C toolchain is available in this environment, so
markupsafe's `_speedups` C extension never builds; `PYTHONPATH=src python3 -m pytest` runs the
pure-Python path throughout, same as whatever produced the committed dataset (`verdict:
uncalibrated`, `mutants_executed: 150`, `invalid: 0` all match `markupsafe.meta.json` exactly).

## Evidence

### 1. 41% of all surviving mutants are unkillable by construction, not by test-suite weakness

Of the 150 executed mutants, 88 survived. Two categories account for 36 of those 88 (41%), and
both are **guaranteed to survive no matter what any scorer says**, because the mutated code
literally never runs under `pytest`:

**a) `setup.py` — 24/24 mutants survived, 0 killed.** Every mutated line (`show_message`,
`ve_build_ext.build_extension`, the `run_setup`/`ext_modules` scaffolding) is packaging code that
only executes under `python setup.py build_ext` — `pytest` never imports `setup.py`. This is
almost exactly the "C-accelerator shadowing" hypothesis from the task brief, except it isn't the
C accelerator itself being shadowed — it's the *build machinery for* the accelerator, and it is
100% inert under the test command regardless of which lines the panel ranks highest.

**b) Type-union annotations, mutated by `bin (BitOr -> BitAnd)` — 12/12 survived, 0 killed.**
`src/markupsafe/__init__.py` opens with `from __future__ import annotations` (PEP 563): every
annotation in the file — `value: str | _HasHTML`, `chars: str | None`, `table: cabc.Mapping[int,
str | int | None]`, etc. — is stored as an unevaluated string and never executes. `vikt-py`'s
mutation generator (`crates/vikt-py/src/calibrate.py::collect_sites`) walks the whole AST with
`ast.walk` and mutates any `ast.BinOp`, including ones that live only inside `arg.annotation` /
`FunctionDef.returns`, with no awareness of PEP 563. All 12 `BitOr` mutants in this run — lines
123, 136, 142, 170, 174, 179, 242, 266, 269, 275, 280×2 — sit on annotation-only unions and cannot
be killed by any test, on this file, ever. This is a real gap in the calibration harness's mutant
generator (it has no annotation-context check), unrelated to markupsafe and unrelated to the
panel's scoring quality — it just burns budget and manufactures survivors on both high- and
low-scored lines alike.

Together, setup.py + dead annotations = 36 of 150 executed mutants (24%), 36 of 88 survivors
(41%), contributing pure noise to whichever correlation is being computed — for the panel and for
the null equally, in principle, though see §3 for why the null benefits from this noise on this
repo and the panel doesn't.

### 2. A second chunk of survivors is real test-suite under-coverage, not panel failure

The remaining ~51 survivors sit on lines that *could* be killed by a test, but aren't, because
markupsafe's suite doesn't exercise that specific method or branch:

- `Markup.replace`/`ljust`/`rjust`/`center`/`expandtabs` (lines 257, 260, 263, 272, 285): the
  mutated tokens are default-parameter values (`count=-1`, `fillchar=" "`, `tabsize=8`) evaluated
  once at `def` time. They *are* live code — mutating them changes what `str.replace`/`ljust`/…
  would do when called without that argument — but markupsafe's test suite never calls these
  wrapper methods at all; their correctness is implicitly delegated to `str`'s own stdlib tests.
- `Markup.__new__`, line 131 — `return super().__new__(cls, object, encoding, errors)`, the
  branch taken when `encoding is not None` (constructing `Markup` from bytes). No test in the
  suite passes an explicit `encoding`; that whole constructor path is untested.
- Several `delete (statement -> pass)` mutants on `return NotImplemented` branches inside
  `__add__`/`__radd__` (lines 140, 146) — the "unsupported operand type" path is never triggered.

These are legitimate gaps in markupsafe's own tests, not evidence about panel quality — but they
still count as "survived" in the pooled correlation, same as everything else.

### 3. Where the panel and the kill rate genuinely disagree

Per-function Spearman (printed by `calibrate` on every run, function scope, `>= 4` lines):

| function | lines | rho |
|---|---:|---:|
| `src/markupsafe/__init__.py:Markup` (class-scope bucket, see §4) | 49 | 0.330 |
| `src/markupsafe/__init__.py:escape` | 5 | 0.671 |
| `src/markupsafe/__init__.py:Markup.__mod__` | 6 | 0.000 |
| `setup.py:show_message` / `ve_build_ext.build_extension` | 4 / 5 | 0.000 / 0.000 |
| `src/markupsafe/__init__.py:Markup.striptags` | 9 | -0.424 |
| `src/markupsafe/__init__.py:Markup.__new__` | 5 | **-0.707** |

`escape` behaves exactly as hoped (rho 0.671) — its lines are all real, all tested (see §4 on why
`_native._escape_inner` itself never got sampled), and the panel ranks them close to their kill
rate. `setup.py`'s two functions report `rho = 0.000` because kill_rate has zero variance there
(every mutant survives — see §1a) — Spearman on a constant vector carries no information, but the
verdict machinery still pools these 9 lines into the top-level rho.

`Markup.__new__` is the sharpest real inversion, and it is worth walking line by line:

| line | code | panel | kill_rate |
|---|---|---:|---:|
| 125 | `if hasattr(object, "__html__"):` | 0.504 | 1.00 |
| 126 | `object = object.__html__()` | 0.253 | 1.00 |
| 128 | `if encoding is None:` | 0.404 | 1.00 |
| 129 | `return super().__new__(cls, object)` | 0.572 | 1.00 |
| 131 | `return super().__new__(cls, object, encoding, errors)` | **0.737** | **0.00** |

Line 131 is the *highest*-scored line in the function (it's a `return`, multi-argument, deepest
in the branch — every one of the panel's structural instruments has reason to like it) and the
*only* one whose mutant survives, because — as in §2 — the encoding constructor path is simply
untested. The panel is not wrong about structure; it's answering a different question than "did
this test suite happen to exercise this branch."

`Markup.striptags` (rho -0.424, n=9) is noisier rather than cleanly inverted: it mixes `cmp`,
`const`, and `delete` mutants on the same lines (the `<!--`/`-->`/`<`/`>` search-and-strip loop),
and the loop-boundary `const (1 -> 2)` mutants on the `+3`/`+1` slice offsets survive more often
than the `delete`/`cmp` mutants on the same lines — a case of mutant-operator heterogeneity and
small-n (9 points, mixed operator strengths) rather than a clean structural miss.

### 4. The dis-lowering's class-scope collapse keeps the one interesting function out of the sample

markupsafe ships a pure-Python fallback for its escape routine, `src/markupsafe/_native.py`:

```python
def _escape_inner(s: str, /) -> str:
    return (
        s.replace("&", "&amp;")
        .replace(">", "&gt;")
        .replace("<", "&lt;")
        .replace("'", "&#39;")
        .replace('"', "&#34;")
    )
```

`src/markupsafe/__init__.py` does `try: from ._speedups import _escape_inner / except ImportError:
from ._native import _escape_inner`. No C toolchain is available in this environment, so the
speedups extension never builds — but `tests/conftest.py` carries a session-scoped, **autouse**
fixture that monkeypatches `markupsafe._escape_inner = _native._escape_inner` for every test
(the `_speedups` half of its parametrization is `skipif`-skipped, never `xfail`-hidden). So
`_native._escape_inner` genuinely *is* exercised by all 39 passing tests — this is the opposite
of "the pure-Python module is never exercised by the suite." It is exercised. It just never got
mutated:

```
$ vikt src/markupsafe/_native.py --stats
functions   2 analyzed / 2 lowered      # module + _escape_inner
lines       4 tiered
```

Four scored lines is far smaller than every function `--sample`'s "largest first" heuristic did
pick. The `vikt-py/dis` (`--lowering primary`) lowering scores a Python **class body** as its own
function-like unit — `Markup`'s `__slots__ = ()` and every one-line delegating wrapper method's
`def` line (`__add__`, `join`, `__getitem__`, `replace`, `ljust`, `rjust`, `lstrip`, `rstrip`,
`center`, `strip`, `expandtabs`, …) is disassembled as part of the class-body code object's own
line range, not as 20-odd separate tiny functions. The result is a single scored unit named
`Markup` whose extent runs from line 120 to at least line 285 (165+ lines) — trivially the largest
"function" in the tree by line count, guaranteed to win `--sample 12`'s "largest first" selection.
With 64 functions scored and only 12 sampled, and the per-file candidate-site budget for the 2
files that *did* get sampled (`setup.py`, `__init__.py`) already exceeding the 150-mutant budget
(199 candidates, 150 run), `_native.py`'s 4-line `_escape_inner` — the one function whose story
really is "accelerator vs. pure-Python fallback" — never entered the sample at all. The budget
went to `setup.py`'s packaging code and `Markup`'s untested wrapper-method surface instead.

### 5. Lowering and scope sensitivity

Same tree, same test command, same 150-mutant budget policy, four combinations:

| lowering | scope | panel rho | null rho | margin | verdict |
|---|---|---:|---:|---:|---|
| primary (`vikt-py/dis`, instruction) | function | 0.123 | 0.262 | -0.139 | uncalibrated |
| primary (`vikt-py/dis`, instruction) | file | 0.218 | 0.499 | -0.281 | uncalibrated |
| ast (`vikt-ts/tree-sitter`, statement) | function | 0.035 | -0.087 | +0.122 | **marginal** |
| ast (`vikt-ts/tree-sitter`, statement) | file | -0.032 | -0.017 | -0.015 | uncalibrated |

Under `--lowering ast`, `vikt-ts` scores each `def` as its own function regardless of body size,
so the class-scope collapse in §4 disappears — but the fix is incidental, not an improvement: the
tiny wrapper methods (1-2 lines) now simply rank too small to be sampled at all under "largest
first" (12 of 57 functions sampled, budget only 117/150 used because too few candidate sites
existed in the sample), so their untested-default mutants are never generated in the first place,
rather than being generated and correctly recognized as noise. Mutant *generation* is identical
across the `--lowering` axis for Python (`vikt_py::calibrate` always mutates through CPython's
`ast` module — see `ast-fallback-comparison.md` — only *scoring* changes), so `Markup.__new__`'s
rho stays exactly -0.707 in every row of this table: that particular inversion is a scoring
disagreement with the test suite's actual coverage, not an artifact of which lowering is used.

`--scope file` makes the null *stronger*, not weaker: pooling the whole file's call-graph-blended
positional extent pushes `escape`/`__new__` (near the top of `__init__.py`, heavily tested) further
from the untested `Markup` wrapper-method tail (near the bottom), so "early in file" tracks
"tested" even more cleanly than "early in function" did. The panel improves too (0.123 -> 0.218)
but the null improves faster, so file scope is the worst gap of the four (-0.281), not the best.

No combination reaches `verdict: calibrated` (needs margin >= 0.1 *and* panel rho >= 0.3). The
`ast`/function-scope cell clears the margin barely (+0.122) but only because the null there is
*negative* (-0.087) — the panel's own rho (0.035) is indistinguishable from chance. There is no
reading of this repository, under any of the four lowering/scope combinations tried, where the
panel is doing real work.

## Diagnosis, stated plainly

The panel loses to the positional null on markupsafe primarily because **the mutation harness and
the sampling heuristic conspired to spend most of the 150-mutant budget on code the test suite
structurally cannot kill**, not because the panel's seven instruments rank markupsafe's actually-
live, actually-tested lines worse than "earlier in file/function" does:

- 24% of executed mutants (36/150 — all of `setup.py`, all 12 annotation-only `BitOr` sites) are
  unkillable by construction: one is packaging code `pytest` never runs, the other is `ast.BinOp`
  nodes inside `from __future__ import annotations`-deferred type hints that never execute
  regardless of test coverage. Neither is markupsafe-specific in kind — the annotation blindness
  in `vikt-py/src/calibrate.py::collect_sites` will manufacture the same kind of unkillable-by-
  construction survivor on *any* PEP-563 codebase, and the dead-file problem will recur on any
  repo with a setup.py/tooling script sitting next to its test-covered package.
- Roughly another third of survivors are genuine test-suite gaps (untested wrapper-method
  defaults, an untested constructor branch) — real code, just not exercised by this particular
  suite's cases.
- On the lines that *are* both live and tested, the panel does show real signal (`escape`: rho
  0.671) and one real, sharp disagreement (`Markup.__new__`: rho -0.707, where the panel's
  single highest-weighted feature — `position`, weight +0.2505 in `INSTRUCTION_WEIGHTS`, the
  largest of all seven — correctly flags the constructor's final `return` as structurally central,
  but that branch happens to be the one the suite never calls).
- The `dis`-lowering's class-body collapse (§4) additionally kept the one function whose story
  really does match the task's "C-accelerator shadowing" hypothesis — `_native._escape_inner`,
  genuinely exercised via `conftest.py`'s autouse fixture — out of the sample entirely, in favor
  of mutating `setup.py` and `Markup`'s untested one-liner wrappers.
- None of this is a lowering artifact in the sense of "the wrong `--lowering` flag was used":
  every one of the four lowering/scope combinations tried lands at `panel rho <= 0.22`, and three
  of the four are outright `uncalibrated`. The one `marginal` cell clears the bar only because its
  null happens to be negative, not because the panel's own rho is any stronger there.

## What this implies for the panel

**Nothing actionable for `panel.rs` or the shipped weights.** This dataset does not show the panel
misjudging markupsafe's actual code; it shows a 150-mutant budget spent mostly on code that cannot
be killed (packaging script, PEP-563-dead annotations) plus a handful of real, small-sample,
test-suite-coverage gaps, pooled into one correlation alongside a positional null that happens —
on this repository's particular file layout, where "early" correlates with "core and tested" and
"late" correlates with "wrapper surface and untested" — to track that untested/tested split better
than seven structural instruments do.

Checking the other seven `eval/calibration/*.meta.json` corpora complicates a purely
markupsafe-specific reading, though: `null_rho` is small or negative on every Rust and JavaScript
corpus (`glob` 0.025, `itoa` 0.020, `rust-shlex` -0.066, `mime-types` -0.003, `ms` -0.151,
`statuses` 0.097), but the *other* Python corpus, `six`, also shows a strong positive null
(0.281, `panel_rho` -0.076, also `uncalibrated`) — and a quick look at `six.jsonl` shows the same
shape of problem: its largest sampled functions (`print_`, 36 lines, mean kill_rate 0.00;
`_update_wrapper`, `exec_`, `write`, all 0.00) are `six`'s Python-2/Python-3 compatibility shims,
version-conditional code this environment's Python 3.11 test run cannot exercise regardless of
what any scorer says — the same "dead-by-construction code, not panel failure" shape as
markupsafe's `setup.py` and annotation sites, just triggered by interpreter-version branching
instead of a packaging script and PEP 563. That is two-for-two on the Python corpora in this set,
against zero-for-five on Rust/JavaScript, which is too small an n to call a language-level pattern
with confidence, but is a specific, checkable lead: `vikt-py`'s mutation generator (`ast`-based,
no knowledge of runtime-conditional branches or deferred annotations) may be more prone than
`vikt-js`'s or Rust's engines to manufacturing structurally-unkillable mutants on real-world Python
packages that carry compatibility shims or packaging scripts — worth a look across a wider Python
sample before concluding it's specific to markupsafe's shape, but not something this single-repo
diagnosis can settle. Either way, it is a harness/generator question, not evidence that
`INSTRUCTION_WEIGHTS` itself is miscalibrated.

Two things surfaced here *are* real defects, just not in the panel, and both are out of scope for
this task ("no production code changes"):

1. `vikt-py/src/calibrate.py::collect_sites` mutates `ast.BinOp` nodes inside type annotations
   with no check for `from __future__ import annotations` — worth a follow-up on any repo that
   uses PEP 563, since it manufactures guaranteed-survivor mutants that inflate `null` and
   `panel` denominators alike without informing either.
2. The `vikt-py/dis` (primary) lowering's class-body collapse makes a Python class's small
   one-line methods invisible to `--sample`'s size-based selection (they get folded into the
   class-scope bucket, which then dominates by sheer accumulated line count) — worth knowing
   before trusting `--sample`'s "largest first" heuristic to pick a representative slice of a
   class-heavy codebase.
