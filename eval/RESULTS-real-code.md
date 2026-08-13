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
