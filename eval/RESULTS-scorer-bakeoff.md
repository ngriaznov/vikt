# Scorer bake-off: FBA, min-cut, structural observability vs. the current scorer

Three source–sink algorithms from outside software engineering, each prototyped
on its own branch, each replacing **only** the per-node score so tier assignment
stays constant and the comparison isolates one variable.

All numbers below were measured by re-running the binaries directly, not taken
from the implementers' reports.

| branch | scorer | algorithm |
|---|---|---|
| `claude/scorer-fba` | `--scorer flux` | Flux balance / flux variability analysis (systems biology) |
| `claude/scorer-mincut` | `--scorer mincut` | Dinic max-flow, node-split min-cut necessity |
| `claude/scorer-observ` | `--scorer observ` | Hopcroft–Karp matching, Liu–Slotine–Barabási edge criticality |

## Headline: all three lost

### Agreement with blind expert labels (Spearman ρ, higher is better)

| scorer | `_siftup` | `has_header` | `_walk` | **mean** |
|---|---|---|---|---|
| **current** | 0.210 | **0.343** | **0.428** | **0.327** |
| observ | 0.283 | 0.159 | 0.356 | 0.266 |
| mincut | **0.346** | −0.006 | 0.262 | 0.201 |
| flux | **0.346** | **−0.135** | 0.300 | 0.171 |

Labels are `eval/ground-truth-v1.json`, written before any scorer was run.

### Score resolution — the property the exercise was commissioned to improve

| scorer | distinct values | median distinct **per function** | p90 per function | median spread |
|---|---|---|---|---|
| **current** | **63** | **8** | **19** | 0.36 |
| observ | 22 | 4 | 6 | 0.23 |
| flux | 16 | 3 | 3 | 1.00 |
| mincut | 14 | 2 | 3 | 0.41 |

`flux`'s median spread of 1.00 is a trap: it spans the full range using **three
values**. Spread without granularity paints a three-colour heatmap.

### Engineering

| scorer | tests | clippy | deterministic | wall time, 5 stdlib modules |
|---|---|---|---|---|
| current | 42 | clean | yes | 3156 ms |
| flux | 43 | clean | yes | 3155 ms |
| mincut | 43 | clean | yes | 3225 ms |
| observ | 45 | clean | yes | 3152 ms |

All three implementations are competent: they build, pass the pre-existing 42
tests, add their own, are clippy-clean and byte-identical across runs. Timing is
undifferentiated — everything is dominated by the Python lowering subprocess, and
none came close to the 50 ms per-function budget.

## Actual output, `os._walk`

```
 line  expert  current   flux mincut observ  source
------------------------------------------------------------------------------
  358    10/10    0.37   1.00   0.47   0.65  scandir_it = scandir(top)
  365     9/10    0.31   0.50   0.47   0.45  while True:
  368    10/10    0.39   1.00   0.47   0.65  entry = next(scandir_it)
  377     9/10    0.35   0.50   0.22   0.65  is_dir = entry.is_dir()
  383     9/10    0.31   0.50   0.47   0.65  if is_dir:
  388     7/10    0.14   0.00   0.22   0.41  if not topdown and is_dir:
  400     4/10    0.03   0.00   0.00   0.00  is_symlink = False
  404     7/10    0.37   0.50   0.47   0.65  walk_dirs.append(entry.path)
  407     9/10    0.30   0.00   0.22   0.45  if topdown:
  408    10/10    0.01   0.00   0.00   0.00  yield top, dirs, nondirs
  411     3/10    0.01   0.00   0.00   0.00  islink, join = path.islink, path.join
  413     8/10    0.22   0.00   0.22   0.25  new_path = join(top, dirname)
  419    10/10    0.22   0.00   0.22   0.45  yield from _walk(new_path, ...)
  425    10/10    0.31   0.00   0.22   0.35  yield top, dirs, nondirs
```

Read the columns vertically. `flux` takes three values, `mincut` three, `observ`
seven, `current` nine.

Two things every scorer gets right and wrong together:

- **Right**: line 411, the pure micro-optimisation binding methods to locals, is
  the least important line in the function and all four score it ≈0.
- **Wrong**: lines 408/419/425 are `yield` — the generator's entire output,
  labelled 10/10 — and every scorer scores them near zero. That is a *frontend*
  bug (CPython `YIELD_VALUE` is lowered as `Pure`), shared by all four, and it
  caps every ρ in the table.

## Why the imported algorithms lost

Not implementation quality. A structural mismatch, and it is the mirror image of
the objection that sent us looking for them.

**These are classifiers; we need a ranker.** Max-flow yields integer flows and
cut membership. FVA yields three classes — essential, variable, blocked. Maximum
matching yields critical, intermittent, redundant. Every one is built to answer a
*discrete structural question*, and each answered it: `flux` really does emit its
three FVA classes, `mincut` really does emit necessity. The output is categorical
because the algorithms are categorical, and no amount of normalising fixes that.

The maligned weighted sum of continuous features is, whatever its aesthetic
problems, well-matched to producing a gradient.

**A real signal in the wreckage**: `flux` and `mincut` both beat `current` on
`_siftup` (0.346 vs 0.210) — a tight pure algorithm where flow reasoning is
exactly right — and both collapse on `has_header`, an accumulator-and-vote
function where flow reasoning is meaningless and `flux` goes *negative*. That is
not noise; it says flow methods capture something real on dataflow-shaped code
and nothing on control-shaped code.

## Caveats

- Three functions, one rater, who wrote the tool. Directional, not conclusive.
- ρ = 0.327 is itself weak. "Current wins" is a statement about a low bar.
- The `yield` bug depresses all four; re-run after fixing it.
- `flux` is FBA-inspired rather than true FBA, and `observ` may have approximated
  the edge classification — read each branch's own report before quoting.
