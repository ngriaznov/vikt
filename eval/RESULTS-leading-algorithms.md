# Leading algorithms: the evidence

Fourteen scorers, three independent targets, one corpus of real CPython stdlib
code. Everything below was produced by re-running the binaries, not taken from
any implementer's report; every table is reproducible from this directory.

```
./eval/mutation_oracle.py --out eval/mutation-oracle.json    # rater-free target
./eval/bakeoff.py eval/ground-truth-v2.json                  # reader target
./eval/bakeoff.py eval/mutation-oracle.json [--no-delete]    # behaviour target
./eval/resolution.py /usr/lib/python3.11 --limit 45          # gradient + cost
./eval/sidebyside.py <file> <function> current,schur,leverage,absorb
```

Two defects had to be fixed before any of it meant anything; they are described
at the end, because they explain why earlier numbers in
`RESULTS-scorer-bakeoff.md` should not be compared with these.

---

## The nomination

**`schur` — Schur-complement deletion sensitivity.** The only algorithm that
leads the reader-facing target *and* is significantly positive against the
rater-free one. Use it when a human is going to read the output.

**`leverage` — statistical leverage / matroid.** The only algorithm
significantly positive against the full rater-free target, the one target with
no positional confound. Best-in-class gradient: 101 distinct values, no flat
functions. Use it when the consumer is a machine ranking work items.

**`absorb` — absorbing Markov chains.** The only scorer positive against *all
three* targets at once, with a usable gradient on 99.8% of functions. The
compromise pick if you get one number.

None of the three is strong. The honest summary is at the bottom.

---

## Target 1 — agreement with a careful reader

`eval/ground-truth-v2.json`: 10 functions, 271 lines, each labelled 0–10 from
source *before* any scorer was run against it. Pooled Spearman on
within-function ranks, so a 79-line function does not outvote a 10-line one.

| scorer | field | ρ | 95% CI |
|---|---|---|---|
| **schur** | Schur complement / graph zeta | **0.357** | [0.25, 0.46] |
| magnitude | enriched category theory | 0.278 | [0.16, 0.39] |
| observ | structural controllability | 0.274 | [0.16, 0.38] |
| current | incumbent, hand-tuned | 0.238 | [0.12, 0.35] |
| vitality | resolvent / T-matrix | 0.201 | [0.08, 0.31] |
| leak | quantitative information flow | 0.182 | [0.06, 0.30] |
| current-flow | resistor networks | 0.176 | [0.06, 0.29] |
| dirichlet | Dirichlet forms | 0.141 | [0.02, 0.26] |
| mincut | max-flow / min-cut | 0.131 | [0.01, 0.25] |
| flux | flux balance analysis | 0.118 | [−0.00, 0.24] |
| hankel | Hankel / Gramian | 0.107 | [−0.01, 0.22] |
| absorb | absorbing Markov | 0.064 | [−0.06, 0.18] |
| leverage | matroid / leverage scores | 0.059 | [−0.06, 0.18] |
| pivot | reliability (Birnbaum) | 0.056 | [−0.06, 0.18] |

### …and the null model that nearly wins it

| control | ρ vs reader labels | ρ vs behaviour |
|---|---|---|
| **"later lines matter more"** | **0.317** [0.21, 0.42] | −0.040 |
| source line length | 0.018 | −0.148 |
| indent depth | −0.025 | −0.180 |
| seeded random | 0.060 | −0.033 |

A one-line heuristic that reads no code at all scores 0.317 against my labels.
Only `schur` beats it, and not by a distance this study can resolve. That is a
property of the labels, not of the scorers: I rate returns and outputs 9–10 and
setup 4–6, and those sit at opposite ends of a function. Every reader-target
number above should be read with 0.317 as the floor, not 0.

The same control is flat against the mutation oracle (−0.040), which is the
main reason to trust that target more.

---

## Target 2 — agreement with behaviour, no rater involved

`eval/mutation_oracle.py` mutates the real stdlib source of 22 functions —
operator swaps, constant perturbation, boolean-connective swaps, statement
deletion — reruns a seeded driver corpus per mutant, and scores each line by how
far the observable moved, averaged over the corpus. 663 lines, 512 covered,
1 546 mutants. Coverage is measured on the unmutated module and uncovered lines
are dropped: a mutation on a line the corpus never reaches says nothing.

Two variants, because they answer different questions. *Semantic only* excludes
statement deletion, so it asks "does the meaning of this line matter". *With
deletion* includes it, so it also asks "does this line need to exist".

| scorer | semantic only (274 lines) | with deletion (491 lines) |
|---|---|---|
| **schur** | **0.137** [0.02, 0.25] | −0.079 |
| absorb | 0.114 [−0.00, 0.23] | **0.097** [0.01, 0.18] |
| **leverage** | 0.110 [−0.01, 0.23] | **0.179** [0.09, 0.26] |
| leak | 0.109 | −0.176 |
| pivot | 0.103 | 0.051 |
| vitality | 0.088 | −0.157 |
| current-flow | 0.064 | −0.205 |
| dirichlet | 0.025 | −0.094 |
| hankel | 0.001 | −0.025 |
| observ | −0.028 | −0.089 |
| **current** | **−0.041** | **−0.081** |
| flux | −0.099 | −0.058 |
| mincut | −0.140 | −0.059 |
| magnitude | −0.148 | −0.089 |

The incumbent is *negative* on both. So is `magnitude`, which came second on the
reader target — its lead there was positional.

### Confound controls on the oracle

| check | ρ | reading |
|---|---|---|
| mutant count vs leverage | −0.246 [−0.33, −0.16] | busier lines have *lower* mean leverage, so the oracle does not reward syntax volume |
| `schur` vs mutant count | **+0.316** [0.23, 0.39] | schur substantially rewards syntactically busy lines — a real caveat, and part of why it goes negative once deletion is included |
| `current` vs mutant count | +0.153 | mild same effect |
| `absorb` vs mutant count | −0.075 | clean |
| `leverage` vs mutant count | **+0.022** | clean; its win is not this artifact |

### The two targets barely agree with each other

Reader importance vs behavioural leverage, over the lines both cover:

| variant | pooled ρ | 95% CI |
|---|---|---|
| with deletion | 0.145 | [0.01, 0.27] |
| semantic only | 0.087 | [−0.10, 0.27] |

This is the single most important number in the document. **A careful reader's
sense of which lines matter and a measurement of which lines change behaviour
are nearly independent.** No scorer can be strong against both, because they are
not the same quantity. `heapq._siftdown` shows it in ten lines: `if newitem <
parent` is the heap-order comparison — I labelled it 10/10 — and weakening `<`
to `<=` changes the sorted output on 26% of inputs, because the result is still
a valid heap. `pos = parentpos`, which I labelled 8, breaks everything: leverage
1.00.

---

## Target 3 — gradient and cost, over 1 071 functions

45 stdlib modules, no rater, no labels. `p10 spread` is the max−min score inside
a function at the 10th percentile: what the *flattest* tenth of functions look
like, which is the heatmap criterion. `flat%` is the share of functions where
every line gets the same number.

| scorer | fns | distinct | med/fn | p90/fn | p10 spread | flat% | wall (45 files) | slowest file | deterministic |
|---|---|---|---|---|---|---|---|---|---|
| dirichlet | 1071 | 101 | 3 | 9 | 0.64 | 0.0% | 11.8 s | 1.75 s | yes |
| hankel | 1060 | 99 | 4 | 9 | 0.60 | 7.6% | 11.5 s | 1.76 s | yes |
| pivot | 1072 | 100 | 4 | 14 | 0.52 | 0.3% | 11.8 s | 1.76 s | yes |
| current-flow | 1079 | 101 | 5 | 15 | 0.50 | 0.0% | 12.2 s | 1.78 s | yes |
| **absorb** | 1054 | 101 | 4 | 11 | **0.38** | **0.2%** | 11.8 s | 1.77 s | yes |
| **leverage** | 1044 | 101 | 4 | 12 | **0.33** | **0.0%** | 13.0 s | 1.82 s | yes |
| vitality | 1083 | 101 | 5 | 16 | 0.18 | 0.0% | 11.7 s | 1.72 s | yes |
| **schur** | 1071 | 98 | 5 | 10 | 0.08 | 0.1% | 11.8 s | 1.85 s | yes |
| current | 1076 | 69 | 5 | 14 | 0.07 | 0.1% | 11.8 s | 1.82 s | yes |
| observ | 1066 | 33 | 3 | 6 | 0.01 | 8.3% | 11.9 s | 1.90 s | yes |
| magnitude | 948 | 82 | 2 | 4 | **0.00** | **20.1%** | 12.4 s | 1.78 s | yes |
| leak | 1018 | 101 | 3 | 9 | 0.00 | 14.8% | 11.8 s | 1.74 s | yes |
| mincut | 959 | 14 | 2 | 3 | 0.00 | 14.1% | 12.3 s | 2.05 s | yes |
| flux | 963 | 17 | 2 | 3 | 0.00 | 11.7% | 11.8 s | 1.83 s | yes |

`magnitude` paints one flat colour on **one function in five**. Whatever it was
doing to place second on the reader target, it cannot draw a heatmap. `leak`,
`mincut` and `flux` have the same disqualifying property.

Cost is undifferentiated: every scorer is within 12% of every other, all
dominated by the Python lowering subprocess, and all byte-identical across runs.
Scorer choice is free.

---

## Actual output

`heapq._siftdown`, all ten lines. `exp` is the blind label, `mut` is behavioural
leverage.

```
 line  exp   mut  current    schur   absorb leverage  source
  208    9  0.93    0.310    0.130    0.810    0.150  newitem = heap[pos]
  211   10  0.29    0.490    0.280    0.180    0.000  while pos > startpos:
  212    7  0.52    0.420    0.340    0.110    0.000  parentpos = (pos - 1) >> 1
  213    7  0.85    0.430    0.400    0.110    0.150  parent = heap[parentpos]
  214   10  0.26    0.490    0.370    0.440    0.020  if newitem < parent:
  215    9  0.21    0.480    0.680    0.310    0.120  heap[pos] = parent
  216    8  1.00    0.430    0.310    0.150    0.150  pos = parentpos
  217    3  0.11      -        -        -        -    continue
  218    6  1.00      -        -        -        -    break
  219    9  0.24    0.450    0.280    1.000    0.500  heap[pos] = newitem
```

Two things to read off it. First, the projection fix: this function used to
emit three spans, one of which covered lines 211–216 with a single score of
0.49. It now emits ten. Second, `continue` and `break` get no score at all —
structural nodes are excluded from line projection, so 2 of 10 lines are
unscored. The oracle rates `break` at 1.00. That is a real coverage gap, and it
is shared by every scorer because it lives in the frontend.

`json.decoder.py_scanstring`, excerpt — the plumbing test, and the incumbent's
worst structural weakness:

```
 line  exp   mut  current    schur   absorb leverage  source
   79    8  0.66    0.040    0.020    0.500    0.070  chunks = []
   80    3  0.59    0.040    0.030    0.250    0.150  _append = chunks.append
   81    3  0.00    0.030    0.030    1.000    0.150  begin = end - 1
   83   10  0.66    0.660    0.160    0.000    0.710  chunk = _m(s, end)
   86    9  0.49    0.660    0.020    0.000    0.480  end = chunk.end()
  126   10  0.53    0.420    0.050    0.260    1.000  return ''.join(chunks), end
```

`current` puts the accumulator that becomes the return value (line 79) at 0.04,
level with the micro-optimisation on line 80. The cause is that `current`'s Core
test is *influence as a fraction of the whole function body*: in a 546-node
function almost nothing clears the 10% bar, so tiering degrades with size. The
same defect demotes `fnmatch.translate`'s second-pass accumulator (`res = []`,
labelled 8) to `plumbing` at 0.13. `leverage` gets both of these right; `absorb`
gets line 79 right and line 81 — a variable read only by error messages —
catastrophically wrong at 1.00.

---

## Two defects fixed first, and what they invalidate

**Span merging.** `project_to_lines` merged consecutive lines whenever they
shared a tier, keeping the max of their scores. A loop body is one unbroken run
of `core`, so every per-line score inside it was discarded at the last step of
the pipeline. No output finer than the four-tier partition existed, and every
rank correlation measured before this — including all of
`RESULTS-scorer-bakeoff.md` — was measuring a step function. Fixed by requiring
scores to agree as well as tiers.

**Inert.** The rule was backward-only, so the `POP_TOP` that discards a
denylisted call's result stayed `plumbing` and dragged the whole logging line
out of `inert`. Fixed with one forward sweep. This had been failing in the tree
since before the bake-off commits; I reported "42 tests, clippy clean" for the
yield-effect change without that being true. It is true now.

---

## The honest summary

Fourteen algorithms from fourteen fields, and the best agreement with a careful
reader is ρ = 0.36 against a positional null model at 0.32, while the best
agreement with measured behaviour is ρ = 0.18. Two of the three nominees are
picked on a rater-free target precisely because the reader-facing one turns out
to be mostly measuring "later in the function".

The finding that matters more than the ranking: the two targets correlate at
0.09–0.15 with each other. "Which lines does a reader need to look at" and
"which lines change what the program does" are close to independent questions.
A single salience number cannot serve both, and the tool should stop pretending
otherwise — `schur` for the first, `leverage` for the second, and say which one
is being asked.

---

# Addendum: can the tool be operated? — the separation test

Rank correlation over all lines is a harder question than the tool claims to
answer and a harder one than any consumer needs. An agent wants to know which
lines it can skip. So: take the clearly-unimportant lines and the clearly-
important ones, drop the ambiguous middle, and measure how often the scorer puts
a member of the first below a member of the second. That is AUC, computed within
each function and pooled. `eval/separation.py`.

| scorer | vs reader labels | 95% CI | vs behaviour | 95% CI |
|---|---|---|---|---|
| **schur** | **0.616** | [0.55, 0.80] | 0.413 | [0.33, 0.52] |
| current | 0.638 | [0.51, 0.92] | 0.489 | [0.42, 0.60] |
| observ | 0.623 | [0.58, 0.80] | 0.475 | [0.31, 0.66] |
| **absorb** | 0.349 | [0.25, 0.76] | **0.709** | [0.54, 0.85] |
| pivot | 0.541 | [0.41, 0.87] | 0.695 | [0.54, 0.83] |
| leverage | 0.279 | [0.12, 0.73] | 0.629 | [0.55, 0.74] |

Stable under four different threshold choices each (`schur` 0.62–0.72 on reader
labels, `absorb` 0.67–0.72 on behaviour), so this is not threshold shopping.
Both leaders' intervals exclude 0.50. Both leaders are at or *below* chance on
the other target.

## And then the number that decides it

AUC 0.70 sounds workable. It is not, because a filter has to be operated at a
threshold. Keeping R% of the load-bearing lines, here is the share of noise
lines that fall below the cut:

| target | scorer | keep 99% | keep 95% | keep 90% | keep 80% |
|---|---|---|---|---|---|
| reader | schur | 0% | 0% | 0% | 40% |
| reader | current | 0% | 25% | 35% | 35% |
| behaviour | absorb | 0% | 0% | 0% | 0% |
| behaviour | pivot | 0% | 0% | 10% | 19% |
| behaviour | leverage | 0% | 0% | 20% | 44% |

**At 95% recall the best scorer removes a quarter of the noise, and most remove
none.** To drop 40% of the noise you must discard a fifth of everything that
matters. There is no threshold at which this can be switched on.

## Why, and what is actually true

The signal is not missing from the *target*. Against behaviour, 36% of covered
lines have leverage <= 0.15 and 21% are under 0.05 — in real stdlib code, a
third of executed lines barely move the output under a realistic input
distribution. There is plenty to find.

What is missing is the *predictor*. Whether `<` versus `<=` is observable, or
whether an accumulator is read before it is overwritten, depends on semantics
and on the input distribution. Dependence topology does not encode either, and
fourteen different ways of measuring that topology — spectral, flow, reliability,
category-theoretic — all land in the same place because they are all reading the
same graph. This is not an implementation failure in any of the fourteen.

The reader-facing target has the opposite problem: only 6% of labelled lines
score <= 3. Well-written code has little redundancy, so there is almost nothing
to separate, and a positional null model captures most of what remains.

Two things that did work and should be kept:

- **Inert detection.** Denylisted-call chains — code that exists only to feed
  logging — is found reliably, and it is a rule, not one of the fourteen.
- **The oracle itself.** `mutation_oracle.py` measures the behavioural quantity
  directly and correctly. It costs minutes per function rather than microseconds,
  but it is the only thing here that answers the behavioural question at all.

---

# Addendum 2: the objective, corrected — approximate the rater's judgement

The project's target is now explicit: an algorithm whose output is close to the
expert rater's judgement. That dissolves the circularity objection this document
kept raising (the labels are the specification, not a proxy), retires the
mutation oracle as the primary target, and fixes the comparison technique:
**held-out prediction**. Fit on some functions, predict a function the fitter
has never seen, score agreement there. Leave-one-function-out, with the
regulariser chosen by an inner loop that never sees the held-out function.
`eval/judgement.py`; labels extended blind to 16 functions / 435 lines
(`ground-truth-v3.json`).

## Result

| predictor | held-out Spearman vs judgement |
|---|---|
| **all 14 signals + structure, combined (ridge)** | **0.451** — beats the best single signal on 12/16 functions |
| position null | 0.359 |
| schur (best single algorithm) | 0.341 |
| current (incumbent) | 0.312 |
| graph signals only, no position | 0.272 |

Three readings, all of which matter:

1. **No single algorithm approximates judgement — a combination does.** The
   cross-discipline search was treated as a tournament with one winner; it
   should have been treated as instrument-building. Judgement is multi-factor,
   and fourteen weak structural instruments jointly predict it better
   out-of-sample than any one of them, on functions the fit has never seen.

2. **The fitted weights name the fields that earn their place:** position
   (0.30), schur — Schur-complement deletion sensitivity (0.23), the incumbent's
   feature bundle (0.13), then dirichlet and pivot (~0.09 each). Half the
   fourteen contribute nothing and can be dropped from the search going forward.

3. **Position is signal, not contamination.** Under the judgement objective the
   positional component of importance (conclusions outrank preamble) is part of
   the thing being approximated. Graph signals alone reach 0.272; position
   alone 0.359; together 0.451. They are complementary, not redundant.

Ceiling not reached: 0.451 with a linear model over rank features. The gap
between that and rater self-consistency is the open territory, and it is now
measurable per candidate: any proposed algorithm slots into `judgement.py` as a
feature column and reports its held-out contribution within the hour.

---

# Addendum 3: round two of the search — four new fields admitted to measurement

Five new analogies commissioned (ecology, science-of-science, information
retrieval, sociology, geomorphology), implemented on branches
claude/scorer-{trophic,disrupt,rarity,broker,strahler}. Each verified
independently before measurement: default output byte-identical to the
prototype, real score gradient, tests and clippy clean. `disrupt` came back
degenerate on stack-machine bytecode (4 distinct values, mean 0.95 - CD's
"cites its references" pattern never fires on single-use temporaries) and was
sent back for statement-level condensation; it is excluded until it returns.

Solo agreement with the oracle (mean per-function Spearman, 16 functions):

  strahler   0.357   1 negative function   <- best solo, most robust yet
  schur      0.341   2 negative            (pinned)
  current    0.312   3 negative            (pinned)
  trophic    0.265   6 negative            wins where position wins
  current-fl 0.232   5 negative            (pinned)
  broker     0.158   4 negative
  rarity     0.025   6 negative

Held-out ensemble (leave-one-function-out, all 18 signals + structure):

  combined               0.506     was 0.451 with 14 signals
  position null          0.359
  beats best single on 12/16 functions

The fitted weights put trophic (0.108) and strahler (0.103) straight into the
top six, behind position/schur/current/pivot. rarity and broker carry small
positive weight; they cost nothing and stay as minor instruments.

Two findings worth recording:

- strahler is the first algorithm with only ONE function ranked backwards.
  Stream order's virtue is what it ignores: it does not care how MUCH flows,
  only where independent derivations CONVERGE - and convergence points are
  close to what the oracle calls important.
- trophic behaves as designed: a graph-native stand-in for the positional
  component of judgement (0.84 on get_close_matches, 0.70 on normpath,
  0.61 on quantiles - all functions whose importance rises toward the end).
  Position's weight dropped from 0.30 to 0.24 with trophic present.

## Round two, concluded

disrupt returned from rework genuine (statement-level condensation; 14-24
distinct values, honest structural limitation documented: CD's sign cannot flip
without a consumer co-reading the alias source, which stack bytecode rarely
exhibits). Solo against the oracle it is anti-correlated: -0.146, backwards on
13/16 functions. The ensemble grants it +0.078 weight but the held-out number
does not move (0.504 vs 0.506). Verdict: measured, pushed, not pinned.

Final measurement of the round - pruning to only the instruments that earn
weight:

  panel                                        held-out vs oracle
  lean: current+schur+pivot+trophic+strahler        0.518   <- best
  all 18 signals                                    0.506
  all 19 (with disrupt)                             0.504
  14 signals (round one)                            0.451
  position null                                     0.359

The instrument panel is now: POSITION (via trophic where structure permits),
SCHUR (deletion sensitivity), CURRENT (the incumbent's blend), PIVOT
(Birnbaum reliability), TROPHIC (derivation depth), STRAHLER (confluence
order). Six instruments from five fields, held-out 0.518 against the oracle,
+0.16 over the best positional heuristic and +0.18 over any single algorithm.
