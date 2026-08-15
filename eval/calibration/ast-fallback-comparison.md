# AST-fallback quality vs. the primaries

Measures the tree-sitter fallback engine (`vikt-ts`) against each language's
primary lowering, on the same fixtures the primaries are already calibrated
against. Python gets an apples-to-apples comparison (identical mutants,
identical test command, only `--lowering` differs). Rust gets the same
comparison at fixture scale, which is too small to clear calibrate's
significance floor either way — the caveat below is load-bearing, not
boilerplate. Kotlin has no mutation engine at all, so it's scored as
agreement between the two lowerings of the same source rather than against a
kill rate.

## Results

| language | lowering | generator | profile | metric | value | verdict |
|---|---|---|---|---|---|---|
| Python | primary | `vikt-py/dis` | Instruction | pooled rho vs. kill rate | 0.635 | calibrated |
| Python | ast | `vikt-ts/tree-sitter-python` | Statement | pooled rho vs. kill rate | 0.651 | calibrated |
| Rust | primary (MIR) | `vikt-rs/rustc_public` | Instruction | pooled rho vs. kill rate | 0.868 | insufficient data (16 lines / 21 mutants) |
| Rust | ast | `vikt-ts/tree-sitter-rust` | Statement | pooled rho vs. kill rate | 0.601 | insufficient data (17 lines / 23 mutants) |
| Kotlin | primary (bytecode) vs. ast | `vikt-jvm/mokapot` vs. `vikt-ts/tree-sitter-kotlin` | Instruction vs. Statement | Spearman rho, lowering vs. lowering | 0.676 | agreement only, no ground truth |

Positional-null rho is reported alongside panel rho in every calibrate run
below for context; it is not a fallback-quality metric on its own, just the
sanity check calibrate always runs.

## 1. Python — the gold comparison

Same fixture (`crates/vikt-cli/tests/fixtures/calibrate`), same test command,
same `--scope function`, so `vikt_py::calibrate`'s AST-level mutation
generation is byte-identical between the two runs — mutant generation always
goes through CPython's `ast` module regardless of `--lowering`; only
*scoring* the mutated lines changes. Both runs executed 138 mutants (69
killed, 69 survived, 0 timeouts) and scored the same 46 lines, confirming the
mutants really were identical.

```
$ vikt calibrate crates/vikt-cli/tests/fixtures/calibrate \
    --test-cmd "python3 -m unittest discover" --scope function --lowering primary
scored via vikt-py/dis (instruction profile)
pooled Spearman rho: panel 0.635, positional null -0.643
verdict: calibrated (margin 1.278, needs >= 0.1 and rho >= 0.3)

$ vikt calibrate crates/vikt-cli/tests/fixtures/calibrate \
    --test-cmd "python3 -m unittest discover" --scope function --lowering ast
scored via vikt-ts/tree-sitter-python (statement profile)
pooled Spearman rho: panel 0.651, positional null -0.563
verdict: calibrated (margin 1.214, needs >= 0.1 and rho >= 0.3)
```

The tree-sitter fallback is not worse here — its pooled rho is a hair
*higher* (0.651 vs. 0.635) on this fixture, and both clear the calibrated
threshold by a wide margin. That should not be read as "AST beats bytecode
in general" — it is one 4-file, 46-line fixture — but it does mean the
fallback is not leaving obvious quality on the table for Python specifically.
Per-function rho moved in both directions across the five sampled functions
(`clamp_scores` 0.507 -> 0.631, `total_with_tax` 0.949 -> 0.791), consistent
with sampling noise at this scale rather than a systematic gap.

## 2. Rust — MIR vs. tree-sitter

`crates/vikt-cli/tests/fixtures/calibrate-rs` is two functions plus their
`#[cfg(test)]` module, deliberately small (calibrate's own Rust mutants
compile every candidate, which is the expensive part). At `--sample 4
--budget 24` neither lowering reaches calibrate's significance floor (>= 20
scored lines, >= 30 executed mutants) — this is a **fixture-scale
limitation of the measurement, not a verdict on either lowering**. Take the
rho numbers as directional only.

```
$ export VIKT_RUST_LOWER=/path/to/vikt-rust-lower
$ vikt calibrate crates/vikt-cli/tests/fixtures/calibrate-rs \
    --test-cmd "cargo test --quiet" --sample 4 --budget 24 --timeout-secs 240 --scope function
scored via vikt-rs/rustc_public (instruction profile)
2 functions scored, 2 sampled
21 mutants executed (1 invalid, discarded): 11 killed, 10 survived
pooled Spearman rho over 16 lines: panel 0.868, positional null -0.868
verdict: insufficient data (16 scored lines, needs >= 20; 21 executed mutants, needs >= 30)

$ unset VIKT_RUST_LOWER
$ vikt calibrate crates/vikt-cli/tests/fixtures/calibrate-rs \
    --test-cmd "cargo test --quiet" --sample 4 --budget 24 --timeout-secs 240 --scope function --lowering ast
scored via vikt-ts/tree-sitter-rust (statement profile)
4 functions scored, 4 sampled (budget of 24 reached: 24 of 61 candidate sites ran)
23 mutants executed (1 invalid, discarded): 13 killed, 10 survived
pooled Spearman rho over 17 lines: panel 0.601, positional null -0.674
verdict: insufficient data (17 scored lines, needs >= 20; 23 executed mutants, needs >= 30)
```

Two things worth flagging exactly as measured, not smoothed over:

- **Pooled rho is meaningfully lower under `ast`** (0.601 vs. 0.868) on
  this fixture. With only 16-17 lines pooled, a couple of mutants landing on
  differently-ranked lines swings rho by tenths — this is not evidence the
  fallback is systematically worse on Rust, but it is the honest number, and
  it is the one language here where the two lowerings visibly disagree on
  direction rather than just magnitude.
- **`ast` found 4 functions where MIR found 2.** `vikt-rs/rustc_public`
  reads the compiled program, and this fixture is built as a plain library
  (`cargo test --no-run`, not the test harness's own separate compilation
  unit) — its `#[cfg(test)] mod tests` functions are absent from that MIR
  view unless `--sample`/scoring is against the test-enabled build.
  `vikt-ts/tree-sitter-rust` walks the source text unconditionally and has
  no `cfg` evaluation, so it scores the two `#[test]` functions
  (`checkout_totals_and_discounts`, `clamp_sum_pins_both_edges`) as if they
  were ordinary functions too. That is a real, structural gap between the
  lowerings — not a bug in either one, but worth knowing before trusting
  `--lowering ast` output verbatim on any codebase that leans on `cfg`.

## 3. Kotlin — agreement, not calibration

No mutation-testing engine exists for Kotlin (calibrate only mutates
Python/JS through their own AST modules and Rust through MIR-adjacent
textual splices). So this is a same-source, cross-lowering agreement
measurement: compile `demo/kotlin/Orders.kt` with `kotlinc`, score the
resulting `Processor.class` through `vikt-jvm` (bytecode, `Instruction`
profile) and score `Orders.kt` itself through `vikt-ts` (source,
`Statement` profile), then take Spearman rho of `function_score` over every
source line both lowerings scored.

```
$ kotlinc -cp kotlinx-coroutines-core-jvm.jar -d out demo/kotlin/Orders.kt
$ vikt out/demo/Processor.class --annotate demo/kotlin/Orders.kt --format json > kt-bytecode.json
$ vikt demo/kotlin/Orders.kt --format json > kt-ast.json
$ python3 kt_agreement.py kt-bytecode.json kt-ast.json
lines scored by bytecode: 37
lines scored by tree-sitter: 40
lines scored by both: 28
spearman rho (agreement): 0.676
```

0.676 over 28 shared lines is moderate-to-strong rank agreement between a
lowering that sees post-optimization bytecode (inlining, the `$default`
bridge, the suspend-function state machine) and one that sees the source
text a Kotlin author actually wrote. The two disagree most on lines that
route through Kotlin-specific desugaring the bytecode lowering resolves via
the `KotlinDebug` SMAP stratum and tree-sitter has no equivalent for (e.g.
line 31, `bytecode=0.00 / ast=0.32` — inside the inlined `timed {}` call,
where the bytecode view collapses the inlined body onto its call site while
tree-sitter scores it as ordinary statements in place). `kt_agreement.py`
(the small comparison helper) is not part of the crate; it lived at
`/tmp/.../kt_agreement.py` for this measurement and is reproduced above
inline rather than checked in, since it's a one-off scoring diff, not a
calibrate-shaped artifact.

## The fixture-scale caveat, stated plainly

Every number in this file comes from small, hand-built fixtures
(`calibrate`: 4 files; `calibrate-rs`: 1 file, 2 functions; the Kotlin demo:
1 file, 6 functions). That's enough to prove the fallback engine produces
a *coherent* ranking that agrees with test-verified behavior at least as
often as chance, and to catch structural gaps like the Rust `cfg(test)`
visibility difference above. It is not enough to claim a calibrated Rust
verdict for either lowering, and it is not a substitute for running
`calibrate` against a real corpus (see `eval/calibration/*.jsonl` for the
scale that exercise ran at) before trusting `--lowering ast` in production
on an unfamiliar codebase.

## Reproduction

From a checkout of `tree-sitter-fallback`, release build first:

```sh
cargo build --release -q
```

Python (no extra setup — `python3` just needs to be on `PATH`):

```sh
vikt calibrate crates/vikt-cli/tests/fixtures/calibrate \
  --test-cmd "python3 -m unittest discover" --scope function --lowering primary
vikt calibrate crates/vikt-cli/tests/fixtures/calibrate \
  --test-cmd "python3 -m unittest discover" --scope function --lowering ast
```

Rust (needs the MIR helper built once — see the repo's bootstrap notes for
`tools/rust-lower`):

```sh
export VIKT_RUST_LOWER=/path/to/tools/rust-lower/target/release/vikt-rust-lower
vikt calibrate crates/vikt-cli/tests/fixtures/calibrate-rs \
  --test-cmd "cargo test --quiet" --sample 4 --budget 24 --timeout-secs 240 --scope function
unset VIKT_RUST_LOWER
vikt calibrate crates/vikt-cli/tests/fixtures/calibrate-rs \
  --test-cmd "cargo test --quiet" --sample 4 --budget 24 --timeout-secs 240 --scope function --lowering ast
```

Kotlin (needs `kotlinc` and `kotlinx-coroutines-core` on the classpath,
since `demo/kotlin/Orders.kt` uses `delay`):

```sh
kotlinc -cp kotlinx-coroutines-core-jvm.jar -d out demo/kotlin/Orders.kt
vikt out/demo/Processor.class --annotate demo/kotlin/Orders.kt --format json > kt-bytecode.json
vikt demo/kotlin/Orders.kt --format json > kt-ast.json
# then pool function_score by source line and take Spearman rho over the
# lines both JSON documents scored
```
