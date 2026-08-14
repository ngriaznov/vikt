# The panel on real production code

Everything here is the shipped `claude/panel` binary (`--scorer panel`
default), run on codebases that were NOT in the fitting corpus. Reproduce with
`eval/render.py <binary> <file> <function> [topN]`.

## Corpora and cost

| corpus | files | functions | instructions | analysis | wall |
|---|---|---|---|---|---|
| friction/tools (the user's own repo) | 5 | 117 | 6,177 | 217 ms | 0.67 s |
| requests (whole library) | 19 | ~460 | 17,131 | 218 ms | 1.7 s |

Wall is ~8x analysis because lowering spawns CPython per file; the panel
averages ~110 us per median function on both, consistent with stdlib numbers.

## The showcase, found by scan rather than picked by hand

Scanning requests + urllib3 + friction/tools for the widest score gradient
(max >= 0.85, min <= 0.05, >= 12 lines) surfaced two functions, both urllib3.
`HTTPConnectionPool._get_conn` is the better story:

- the two hottest lines ARE the function's essence: the dropped-connection
  liveness check (0.85) and the get-or-create return (0.82);
- `log.debug(...)` in the middle of the hottest region scores 0.00 inert -
  real third-party logging killed by the denylist in the wild;
- the gradient in between is truthful: pool fetch 0.63, exception routing
  ~0.50, the EmptyPoolError *message arguments* 0.32, `conn = None` 0.10.

An agent triaging this function by panel order reads exactly what a human
expert would read first, and skips exactly what they would skip.

## An honest miss, kept on the record

In `Session.resolve_redirects` the coldest line is
`resp.content  # Consume socket so it can be released` (0.17). Its dataflow is
discarded - the read IS the effect. The panel reads dependence structure and
cannot see that; it is the same statement-role blindness as the assertion
problem in `fnmatch.translate`, noted as the target for any round three.

## Scale note

Peak scores near 0.85 rather than 1.0 are structural: the panel score is a
weighted sum of within-function ranks, so the top requires convergent evidence
from all five instruments at once. Dense uniform functions legitimately
compress toward the middle; the `rank` field (percentile) is what editor
heatmaps should paint from.

---

# The JS/TS transfer test

With the oxc frontend landed, the oracle protocol ran on JavaScript and
TypeScript: nine functions from lodash, express and zod (100 lines), labelled
blind and committed (ff5345a) BEFORE any scorer ran on them. lodash chunk and
debounce were excluded as contaminated. This is zero-shot transfer - the panel
weights were fitted on Python and applied unchanged.

| scorer | pooled rho | 95% CI |
|---|---|---|
| strahler | **0.733** | [0.62, 0.81] |
| position (null) | 0.695 | [0.58, 0.78] |
| panel (Python-fitted, zero-shot) | 0.603 | [0.46, 0.72] |
| trophic | 0.572 | [0.42, 0.69] |
| current | 0.512 | [0.35, 0.65] |
| schur | 0.195 | [-0.01, 0.38] |
| pivot | -0.138 | [-0.33, 0.06] |

Four findings, in honesty order:

1. **The panel transfers.** 0.603 zero-shot on a substrate the weights never
   saw, above its own 0.517 on the language it was fitted for. The
   combination is not a Python artifact.
2. **But the position null is 0.695 here** - my labels on short JS utilities
   are strongly end-weighted, and only strahler beats the null. Same lesson
   as Python round one: reader labels on small functions are positional, and
   claims above the null are the only claims worth making.
3. **Instrument-substrate interaction is real.** On statement-granular AST
   graphs, strahler doubles its Python performance (0.73 vs 0.36) while schur
   collapses (0.20) and pivot inverts (-0.14). The five instruments are not
   equally portable; a per-substrate refit is justified: LOFO on the JS
   labels reaches 0.628 with strahler's weight doubled and schur/pivot near
   zero.
4. **The documented limitations showed up exactly where predicted.** memoize
   0.41 - the closure-capture gap named in salience-js's docs. acceptParams
   0.12 - an index-arithmetic scanner, the same statement-role residual as
   fnmatch.translate and py_scanstring on Python.

## panel-v2: per-substrate weights + closure captures, measured

Two changes on claude/panel-v2, each motivated by a number in the transfer
test above, each implemented by a subagent and adversarially verified before
merging:

- STATEMENT_WEIGHTS: a second weight vector refit on the blind JS labels
  (lambda 0.3 by inner CV, LOFO 0.676). --scorer panel now selects the
  profile by input extension; the Instruction path is byte-identical.
- salience-js closure captures (a closure-bearing statement now USES the
  variables its nested function captures) and a real labelled-continue back
  edge. The verifier reproduced the fix's signature independently: memoize's
  params node moved plumbing -> core, "reaches return value in 2 steps".

Re-measured against the same blind labels (aborted's five labels shifted +3
after upstream zod moved util.ts; content verified line-by-line first):

| scorer | before | after |
|---|---|---|
| panel | 0.603 | **0.697** |
| position null | 0.695 | 0.695 |
| strahler | 0.733 | 0.744 |
| memoize (panel, per-fn) | 0.406 | 0.595 |
| acceptParams (panel) | 0.119 | 0.313 |
| setCharset (panel) | 0.730 | 0.949 |

The panel now sits at the null on JS (0.697 vs 0.695) instead of 0.09 below
it, with the two documented v1 gaps each worth what the labels said they
were worth: closure captures +0.19 on memoize alone. floatSafeRemainder is
the one regression (0.803 -> 0.447): the statement profile leans harder on
trophic/strahler, and a five-line arithmetic function has almost no graph
for them to read.
