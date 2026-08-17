# File scope vs function scope at corpus scale

Roadmap item #3. Four repos, each calibrated twice with `vikt calibrate` --
identical mutant sample per repo, `--scope function` then `--scope file`
(`--sample 12`, per-language mutant budget, `--timeout-secs 60`). File-scope
runs additionally emit `eval/calibration/<name>-filescope.jsonl` via
`--emit-dataset`. Verdict thresholds: **calibrated** = rho - null >= 0.1 AND
rho >= 0.3; **marginal** = rho > null (but short of calibrated); else
**uncalibrated**.

| repo | lang | mutants (killed/survived/timeout) | scored lines | function rho | function null | function verdict | file rho | file null | file verdict |
|---|---|---|---|---|---|---|---|---|---|
| markupsafe | python | 118 (60/56/2) | 60 | -0.018 | -0.041 | marginal | -0.080 | 0.042 | uncalibrated |
| six | python | 150 (35/115/0) | 98 | -0.083 | 0.336 | uncalibrated | -0.144 | 0.441 | uncalibrated |
| statuses | javascript | 65 (7/58/0) | 37 | -0.007 | 0.097 | uncalibrated | -0.083 | -0.076 | uncalibrated |
| ms | javascript | 60 (55/5/0) | 35 | 0.526 | -0.151 | calibrated | 0.339 | -0.032 | calibrated |

All numbers above are from this run's own logs, not copied from the prior
agent's report. markupsafe, six, statuses, and ms's function-scope run
reproduced the prior agent's reported mutant counts and pooled rho values
exactly (no drift) once one setup issue was corrected: the initial ms run
picked up a stray `coverage/` directory (a gitignored `npm test` artifact
left over from the baseline-verification step) and scored its
`lcov-report/*.js` files as if they were project source, diluting the
6-function panel down to noise (79 "scored lines", panel rho -0.133). Deleting
`coverage/` before calibrating reproduced the prior agent's clean 60-mutant,
35-line, rho 0.526 result byte for byte. The lesson generalizes: `vikt
calibrate` copies whatever is on disk, gitignore or not, so any repo whose
test command leaves generated files behind should be re-cloned or cleaned
before calibrating, not just before the baseline check.

ms's file-scope run was the one piece missing from the prior agent's work.
It reused the same 7 sampled functions and the same 60 mutants (55
killed/5 survived) as function scope -- scope changes how the panel scores a
line, not which mutants run -- and came out **calibrated**: rho 0.339 against
a null of -0.032, a margin of 0.371, comfortably past both thresholds.

## Does file scope beat function scope against mutation kill rates?

On this evidence, mostly no. File scope is strictly worse than function scope
on markupsafe (rho drops from -0.018 to -0.080, and the verdict degrades from
marginal to uncalibrated) and on six (-0.083 to -0.144, uncalibrated either
way but the null jumps to 0.441 against it). statuses is a wash: both scopes
are uncalibrated, and the small rho differences (-0.007 vs -0.083, both under
a null near zero) aren't distinguishable from noise at this sample size.
ms is the one repo where file scope also comes out calibrated, but it does
so at a lower rho than function scope on the same 60 mutants (0.339 vs
0.526) -- coarsening the score from function to file granularity threw away
signal here too, just not enough to cross back over the calibration
threshold. Across all four repos, file scope never beat function scope's
rho; the best it managed was "also calibrated, but by less."

Caveats that keep this from being a strong verdict either way:

- **Kill rates are low and noisy on two of the four repos.** six killed only
  35/150 mutants (23%) and statuses killed only 7/65 (11%); with that few
  kills, both the panel rho and the positional-null rho are estimated from a
  thin, lopsided sample, and both scopes come out uncalibrated on those repos
  regardless of what file vs function scoring changes. Their contribution to
  "file scope is worse" is weak evidence -- there may not be enough kill
  signal in either repo to distinguish any scoring scheme from noise.
- **Four repos is not a corpus.** Two languages, two repos each, all small
  utility libraries. The pattern (file scope flat-to-worse) is consistent
  across all four, which is something, but four data points doesn't rule out
  a repo where file-level clustering of related mutations would help --
  e.g. a codebase with more file-level cohesion or fewer, larger functions
  per file than these four.
- **File scope's original purpose is a different question than kill
  prediction.** It was validated on fixtures for the reading-guidance use
  case (does file-level severity help a human decide what to read first) --
  not for ranking individual lines by how likely a mutation there is to
  survive. This corpus run answers "does file scope predict kills better
  than function scope," not "is file scope useful," and the two can
  legitimately have different answers.

## Reproducing

```
./target/release/vikt calibrate /tmp/corpus/<name> --test-cmd "<cmd>" --scope function --timeout-secs 60
./target/release/vikt calibrate /tmp/corpus/<name> --test-cmd "<cmd>" --scope file --emit-dataset eval/calibration/<name>-filescope.jsonl --timeout-secs 60
```

Pinned commits and test commands:

| repo | commit | test_cmd |
|---|---|---|
| markupsafe | `b2e4d9c7687be25695fffbe93a37622302b24fb1` | `PYTHONPATH=src python3 -m pytest -x -q` |
| six | `c8e394065cd541a16c040515dc0afb85cf22a7c3` | `python3 -m pytest test_six.py -q` |
| statuses | `770a97d931c1bb40ebbfefdbb77f0419601890b5` | `npx mocha --reporter min --check-leaks --bail test/` |
| ms | `4ff48cec099f0514c3e9bbca18706c9c21122bfb` | `npm test` |
