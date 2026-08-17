# vikt

Deterministic per-line importance over function bodies. *Vikt* is Swedish for
weight — the stem of *viktig*, important — which is the whole idea: the weight
of every line, measured.

For every line of every function, one question: **does this line carry
behavior?** The answer is a tier and a score, computed from dominance, loop
structure and def-use reachability over an IR. No model runs. Nothing is
removed. Every span carries the reason that produced it.

```
$ vikt OrderProcessor.class --annotate OrderProcessor.java

inert   11 |         LOG.info("processing " + prices.size() + " prices");
plumb   13 |         String unused = "this value goes nowhere";
inert   14 |         int inspected = 0;
CORE    16 |         double subtotal = 0.0;
CORE    17 |         for (Double price : prices) {
CORE    18 |             if (price == null || price < 0) {
CORE    21 |             subtotal += price;
inert   22 |             inspected++;
CORE    26 |         if (applyTax) {
CORE    27 |             total = subtotal * (1.0 + taxRate);
inert   30 |         LOG.fine("inspected " + inspected + " entries");
BOUND   32 |         this.runningTotal = total;
BOUND   33 |         return total;
```

Lines 21 and 22 are the point. Both are loop-carried accumulators. Both look
identical. `subtotal` reaches a field write, so it is core. `inspected` reaches
nothing but a log call, so it is inert. No syntactic analysis separates these;
only dependence does.

## The idea

Every system has elements that matter more than others: keystone species in
a food web, critical buses in a power grid, the mainstem of a river basin,
pivotal papers in a citation network. Each of those fields (ecology, reliability
engineering, hydrology, control theory, network science) has spent decades
formalising *which element of this network matters*.

The founding bet of this project is that the symmetry runs deep enough to
transplant. A function's dependence graph is a small directed network with
sources (parameters, constants), internal transformation, and sinks (returns,
writes, calls): structurally the same object those sciences already study.
So instead of inventing one more code-specific heuristic, we took the
importance notion of each mature field, mapped its home network onto the
dependence graph, implemented it exactly, and measured it against an expert's
blind per-line judgement.

The result is not a single winning algorithm. **No transplant wins alone**:
the best solo algorithm ties a heuristic that reads no code at all. **They
win together**: each field's notion captures a different facet of importance
(what breaks if deleted, where derivations converge, how deep a value's
lineage runs, what is a single point of failure), and the facets fail in
different places. The shipped score is therefore an instrument panel: five
algorithms from five fields, combined by weights fitted on expert labels and
verified on functions the fit never saw. The full measurement record,
including every candidate that was tried and cut, lives in `eval/`.

## Tiers

| tier | meaning |
|---|---|
| `core` | behavior-carrying: branch predicates weighted by what they control-dominate, loop-carried dataflow, statements on def-use chains reaching an effect |
| `boundary` | the frontier where behavior leaves the body: returns, throws, state writes, calls into opaque dependencies |
| `plumbing` | present but not behavior-carrying: local shuffling, results that reach no effect |
| `inert` | denylisted calls (logging, metrics, tracing) and the computation that exists only to feed them |

With the tier, every span carries two numbers:

- **`score`**: `0.0..=1.0` against a fixed scale. For thresholds and policy.
- **`rank`**: percentile within its own function. For heatmaps, and for "show
  me the top of this body". A function whose scores all sit near 0.3 still has a
  most-important line; painting it against the global scale would render the
  whole body one flat colour.

The score is produced by an **instrument panel**, five independent algorithms
combined by weights fitted against blind expert judgement, described in the
next two sections. Every panel member is also selectable alone via `--scorer`,
because a combination whose members cannot be inspected individually cannot be
re-measured honestly.

## The score: an instrument panel

No single structural measure of a dependence graph orders a function's lines
the way an expert reader does. That is a measured result, not a stance: nineteen
candidate algorithms from as many fields were implemented and compared,
each against per-line importance labels written blind by an expert
rater before any scorer ran, and the best solo instrument reached Spearman
ρ ≈ 0.36, statistically indistinguishable from the null heuristic "later
lines matter more" (0.359). Every algorithm, including the best, also has
functions it ranks *backwards*.

But they fail in **different places**. Deletion-sensitivity understands
duplicated structure that flow measures double-count. Confluence order is
robust exactly where reliability analysis is noisy. Derivation depth explains
the functions whose importance accumulates toward the end. Importance is not
one quantity. It has facets. So the shipped score is a linear combination of
five instruments plus two structural facts, with ridge weights fitted on the
expert labels (16 stdlib functions, 425 lines, λ chosen by inner
cross-validation). Held out on functions the fit never saw: **ρ = 0.517**,
beating every single instrument on 12 of 16 functions.

| instrument | home field | the facet it carries | weight |
|---|---|---|---|
| `current` | this repo's incumbent | dependence cones, control mass, loop carriage | +0.16 |
| `schur` | linear algebra | what is lost if this statement is deleted | +0.17 |
| `pivot` | reliability engineering | is this a single point of failure | +0.19 |
| `trophic` | food-web ecology | how many derivation layers lie beneath | +0.11 |
| `strahler` | geomorphology | do independent derivations converge here | +0.11 |
| position | (structural fact) | judgement's demonstrated positional lean | +0.25 |
| boundary tier | (structural fact) | the tier layer's I/O call | −0.05 |

Mechanics (all in `vikt-core/src/panel.rs`, pure Rust): each instrument's
per-node scores are projected to lines by `max`,
rank-normalised within the function (a scorer's scale is arbitrary across
functions; its ordering is the signal), then combined by the fitted weights.
The weights are baked constants with their provenance in the module docs, and
a test fails if anyone edits one without re-fitting: the held-out number is
only true of exactly those values. Tier assignment never varies with scorer
choice; only the continuous score does.

### Two weight profiles, selected by substrate

The panel carries **two** fitted vectors, because a measured transfer test
showed the instruments are not equally portable between graph shapes:

- **`Instruction`**: for bytecode/MIR frontends (JVM, CPython, Rust). The
  table above.
- **`Statement`**: for every AST-granular frontend — JS/TS (oxc) and the
  tree-sitter fallback (Rust/Python/Java/Kotlin/Go; see below). Refit on blind
  JavaScript / TypeScript labels (9 functions from lodash, express and zod,
  committed before measurement): strahler and trophic rise to the top
  instrument weights while schur and pivot fall, because statement-granular
  graphs feed confluence and depth and starve deletion-sensitivity and
  reliability.

`--scorer panel` picks the profile from the resolved lowering, not the bare
extension — a `.py`/`.rs` file gets `Instruction` or `Statement` depending on
which one ran. Evidence: applying the Python-fitted weights to JS zero-shot
scored 0.603 against the blind labels; the Statement profile plus two
frontend fixes it motivated (closure captures, labelled-continue back edges)
took it to **0.697**, level with the positional null (0.695) that dominates
short utility functions, with lodash `memoize` alone improving 0.41 → 0.60.

## How it works, end to end

Every claim above is produced by one deterministic pipeline. In order:

**1. Lowering: substrate → `FunctionIr`.** Each frontend answers four
questions per instruction or statement: what source line, what does it
define, what does it use, where can control go next, plus a kind
(`Pure`, `Branch`, `Return`, `Throw`, `StateWrite{target}`,
`Call{callee, opacity}`).
  - *JVM*: `mokapot` lifts the class file to MokaIR; the `LineNumberTable`
    maps instructions to lines; JSR-45 SMAP (the `KotlinDebug` stratum)
    collapses Kotlin inline bodies onto call sites; Kotlin `suspend` state
    machines are excised so the author's control flow is analyzed, not the
    compiler's dispatcher.
  - *CPython*: the system interpreter's own `dis` module is driven through a
    pinned JSON contract (the analysis never parses Python); PEP 626
    `co_lines()` gives exact positions; `yield` lowers as a state write,
    because handing a value to the caller is an effect.
  - *JS/TS*: oxc parses and semantically resolves; def/use comes from real
    reference resolution, not name matching; every call expression becomes
    its own node so logging chains stay separable; closures record their
    captured variables as uses; `catch` bodies are lowered unreachable
    (a measured choice; see the design notes at the end of this section).

**2. Graph analyses, all in `vikt-core/src/graph.rs`.** From the raw
node list: control-flow successors → dominance and post-dominance
(Cooper-Harvey-Kennedy), control dependence (Ferrante-Ottenstein-Warren),
natural loops via dominator back edges and per-node loop depth, reaching
definitions (a fixpoint, so frontends need not be SSA), def-use chains, and
transitive dependence cones via bitset closure in reverse topological order
over the SCC condensation.

**3. Tiering.** Rules, not weights, and every span carries its reasons:
  - `boundary`: returns, throws, state writes, calls into opaque
    dependencies. The frontier where behavior escapes the body.
  - `inert`: a denylisted call (logging/metrics/tracing across all five
    languages, `console.*` included), plus every statement that BOTH reaches
    no useful sink AND feeds a denylisted one (the two-condition rule that
    keeps `pass` and constants out of the tier), plus the discard of a
    denylisted result (one forward sweep).
  - `core`: influence over the body, control-dominance mass, loop-carried
    dataflow.
  - `plumbing`: everything present but not behavior-carrying.

**4. The five instruments** score every node independently (each is
selectable alone via `--scorer` for inspection):
  - `current`, the hand-tuned blend: forward/backward dependence cones,
    control mass, loop depth, effect kind, effect proximity.
  - `schur`, deletion sensitivity in closed form: how much total delivered
    influence disappears if this node's row and column are removed
    (Schur-complement identity, exact per SCC).
  - `pivot`, Birnbaum structural importance at p = ½: how often this node
    is the pivot between "the function delivers" and "it doesn't". Equals
    the Banzhaf index, computed by adjoint differentiation.
  - `trophic`, trophic level: 1 + weighted mean level of what it consumes,
    basal nodes (parameters, constants) at level 1, cycles solved exactly on
    the SCC condensation. Derivation depth, made structural.
  - `strahler`, Horton-Strahler order over pure dataflow: sources are
    order 1, and the order rises only where two equal-order tributaries
    merge. Finds the mainstem.

**5. The panel** projects each instrument to lines (`max` over a line's
non-structural nodes), rank-normalises within the function, appends position
and the boundary flag, and takes the dot product with the substrate profile's
weights. Inert floors to zero under every scorer.

**6. Projection and artifact.** Per line: strongest tier, panel score,
within-function percentile `rank` (paint heatmaps from this — it always uses
the full range), and the sorted reasons. Consecutive lines merge into a span
only when tier AND score agree, so the gradient survives. Output is a JSON
sidecar; bodies over `--max-instructions` (default 4096, and every observed
case above it is a generated data table) are skipped loudly, never silently.

**Measured design choices worth knowing about** (each has numbers in
`eval/`): exception edges are OFF everywhere, because making handlers
reachable destroys post-dominance and drags scores toward error paths
(0.33 → 0.13 against expert labels when tried). The semantically faithful
CFG is not the most useful one for importance. And the evaluation protocol behind every
number: blind per-line labels committed before measurement
(`eval/ground-truth-*.json`), leave-one-function-out held-out fits
(`eval/judgement.py`), a rater-free mutation oracle (`eval/mutation_oracle.py`),
and null-model controls. The positional null is reported next to every
headline because reader labels are demonstrably position-correlated.

## What it's for

The same map serves several consumers, which is why it emits both a
classification and a ranking:

- **Agent reading guidance**: what to read first in an unfamiliar body.
- **Agent edit policy** — a harness hook that demands verification when an edit
  touches a core span, and waves through a logging change.
- **Weighted dependency graphs** — nodes weighted by the importance of what they
  contain, rather than by line count.
- **Performance work** — start profiling from the loops with real loop-carried
  dataflow, not from wherever the flame graph happens to be wide.
- **Vulnerability triage** — rank findings by whether the flagged line actually
  reaches an effect.
- **Refactoring** — the plumbing and inert mass is where mechanical change is
  safe.

## What it is not

- **Not compression.** No tokens are removed. The artifact is metadata *about*
  source; a consumer that ignores it sees the file unchanged.
- **Not learned.** No inference at build time or query time.
- **Not criterion-anchored.** Classic slicing answers "what affects *this*".
  This answers "what carries behavior at all", unconditionally — so it is
  computed once and cached rather than recomputed per question.
- **Not repo-level ranking.** The unit is the statement inside one body.

## Architecture

```
vikt-core   language-neutral. Dominance, post-dominance, control dependence,
                natural loops, reaching definitions, tiering, scoring, projection.
                Knows nothing about any language.
      ^
      |  FunctionIr  (the contract: line, defs, uses, successors, kind)
      |
vikt-jvm    .class -> mokapot MokaIR -> FunctionIr
vikt-py     .py -> CPython dis -> JSON -> FunctionIr
vikt-cli    the `vikt` binary
```

The seam is `FunctionIr`. A frontend answers four questions per instruction —
what line, what does it define, what does it use, where can control go — and
gets everything else.

Two properties make that seam hold:

- **The graph is instruction-level, not block-level.** Frontends never have to
  discover basic blocks.
- **Definitions need not be in SSA form.** The core computes reaching
  definitions itself, so the Python frontend over mutable `STORE_FAST` locals is
  exactly as sound as the JVM frontend over an already-SSA IR.

The Python frontend speaks the contract as JSON, which demonstrates it is
implementable from outside Rust — a JVM agent, a `rustc` driver, a compiler
plugin.

## Why IR and not ASTs

For Java the two agree closely enough that it barely matters. For Kotlin they do
not: a `suspend` function's real control flow is a compiler-generated state
machine, an `inline` function's body is physically copied into each call site,
and a `when` becomes a `tableswitch` or a comparison chain depending on what it
matches. An AST shows the syntax someone wrote. The bytecode shows the control
flow that runs. The `LineNumberTable` maps it back to the lines they will edit.

The same argument holds more weakly for Python — comprehensions are separate
code objects, `and`/`or` are jumps, a `for` loop's real exit test is `FOR_ITER`.

## Adding a language

Implement one function: substrate → `Vec<FunctionIr>`.

| language | substrate | line fidelity | status |
|---|---|---|---|
| Java | JVM bytecode via `mokapot` | `LineNumberTable`, needs `javac -g` | **working** |
| Kotlin | same | `LineNumberTable` **plus** JSR-45 SMAP resolution | **working** |
| Python | CPython bytecode via `dis` | PEP 626 `co_lines()`, exact | **working** |
| JS/TS | oxc semantic-resolved AST + constructed CFG | AST spans, exact | **working** — see `vikt-js` |
| Rust | MIR via `rustc_public`, through a nightly-pinned helper | MIR spans, macro expansions dropped as foreign | **working** — see below |
| Go | tree-sitter AST (`vikt-ts`) | AST spans, exact | **working** — analyzable, not yet calibratable |
| C/C++/Swift | LLVM IR `DILocation` | debug info | not attempted |

Rust analysis needs one extra build step: `rustc_public` is nightly-only, so
the MIR lowerer lives in `tools/rust-lower`, outside the stable workspace,
with its own pinned `rust-toolchain.toml`. Build it once
(`cd tools/rust-lower && cargo build --release`; rustup fetches the pin
automatically) and the stable `vikt` binary finds it on `PATH`, via
`VIKT_RUST_LOWER`, or at its build location. The helper speaks the same
JSON contract the Python frontend does, so the main workspace never links a
compiler internal and stays on stable forever.

Whole packages work through cargo: point `vikt` at a package directory
or `Cargo.toml` (with `--package` to pick one member of a workspace) and it
runs `cargo check` under the pinned toolchain with the helper installed as
`RUSTC_WRAPPER`. Dependencies, build scripts and proc-macros compile
untouched; only the primary package is lowered. Dependency artifacts cache
in `target/vikt` under the package (`VIKT_TARGET_DIR` redirects
them, for analyzing a checkout that must stay pristine), and each function
in the sidecar carries its own source file, since a package spans many.
Single standalone files still work without cargo. MIR is analyzed after
optimization, so a binding the compiler erased gets no span; `println!`
expands to `std::io::_print`, which the denylist knows; derive-macro
bodies attribute to the `#[derive]` line, like Kotlin's property accessors.

JS/TS is the one place the IR-over-AST rule bends, deliberately: there is no
stable JavaScript bytecode to lower from, and no compiler reshapes control
flow between the source and what runs — so a semantic-resolved AST (oxc) with
an explicitly built CFG *is* the faithful substrate. The frontend extracts
every call as its own node, which is what lets the denylist isolate
`console.log` chains exactly as the bytecode frontends do; `catch` bodies are
lowered unreachable, mirroring the measured exception-edge decision from the
Python frontend.

### The lowering ladder

Only Rust keeps a bytecode-class primary: the MIR helper measured well
ahead of the AST lowering (rho 0.868 vs 0.601 on identical mutants), so
`auto` uses it whenever `vikt-rust-lower` is present and falls back to
tree-sitter otherwise — logged, never silent. Everything else scores
through an AST: JS/TS via oxc as always, and Python, Java, Kotlin and Go
source via a generic tree-sitter walker (`vikt-ts`, one walker plus a
per-language grammar table). Python's bytecode lowering measured *behind*
the AST head-to-head (0.635 vs 0.651), so the AST is its default with no
interpreter needed; `.class` files still analyze as bytecode, and
`.java`/`.kt`/`.go` source is a new capability — Go has no bytecode/MIR
substrate in this project at all, so tree-sitter is not a fallback for it,
it's the only lowering it has ever had. Every tree-sitter lowering is
statement-granular, scores under `Statement`, and names its grammar in the
sidecar's `generator` field (`vikt-ts/tree-sitter-rust`, etc.).
`--lowering <auto|primary|ast>`, on `analyze` and `calibrate` both: `auto`
(default) as above, `primary` requires the bytecode/MIR path and errors
without it, `ast` forces tree-sitter. The measurements live in
`eval/calibration/ast-fallback-comparison.md`.

## Usage

```bash
cargo build --release

vikt Foo.class                          # JSON sidecar, panel score (default)
vikt foo.py --format text               # one line per span
vikt Foo.class --annotate Foo.java      # tiered source view
vikt Foo.class --stats                  # histogram and timing
vikt Foo.class --inert 'com.acme.Audit' # extend the denylist
vikt Foo.class --no-denylist            # treat nothing as inert
vikt app.ts                             # TS/JS: statement-profile panel
vikt lib.rs                             # Rust via MIR (build tools/rust-lower once)
vikt Foo.java                           # Java source, tree-sitter (new capability)
vikt foo.go                             # Go source, tree-sitter (the only lowering it has)
vikt path/to/package --package foo      # whole cargo package, deps compiled not analyzed
vikt path/to/folder                     # any directory: every known extension, one sidecar
vikt path/to/repo --scope repo          # call-graph blend across the whole run, not just one file
vikt foo.py --scorer strahler           # one instrument alone
vikt foo.py --scorer current            # the incumbent alone
vikt big.py --max-instructions 0        # lift the data-table size guard
vikt foo.py --format sarif              # SARIF 2.1.0 for code scanning
```

`--scorer` selects which algorithm produces the continuous score: `panel`
(default), or any single instrument — `current`, `schur`, `pivot`, `trophic`,
`strahler`. The panel's weight profile follows the resolved lowering
(bytecode/MIR → Instruction, oxc or tree-sitter → Statement). Tier assignment
is identical under every choice. `--lowering` (see above) picks the lowering
that decides it.

**Do not score with a single instrument.** The panel is the product. The
single-instrument flags exist for measurement, audit, and regression
pinning only. Every solo instrument has been measured at or below a
positional null against expert judgement, and every one has functions it
ranks *backwards*. The panel beats them all precisely because their
failures do not overlap. Use a single instrument to re-fit weights against
new labels, to see which facet drove a surprising score, or to verify a
refactor left a member byte-identical — never as the score a consumer reads.

### Folder input

A directory with no `Cargo.toml` is a first-class multi-language input: `vikt`
walks it (skipping dot-directories, `node_modules`, `target`, `venv`,
`__pycache__`), lowers every file whose extension a frontend claims through
that frontend, and folds the whole tree into one sidecar — a Python module, a
JS build script and a Java helper class all in one run, one JSON artifact,
`FunctionRecord.file` distinguishing them the same way cargo-package mode
already does. A directory *with* `Cargo.toml` keeps cargo mode for its `.rs`
files exactly as before, and now additionally lowers any other-language
sources the package directory contains, noted on stderr.

### File and repo scope

By default every line's score blends two layers: its standing inside its own
function (the panel above) and its function's standing in the file — a
weight from four rank-normalised call-graph signals (call-graph depth,
fan-in, size share, boundary density), computed over the file's intra-file
call graph with conservative name matching. The blend is re-ranked across
the file and emitted as `file_score` on every sidecar span; `score`, `rank`
and tiers are untouched, and files never compete with each other. `--scope
function` drops the layer and restores the pre-file-scope artifact
byte-for-byte.

`--scope repo` is the same apparatus one rung up: the four call-graph
signals and the blend are computed *once* across every scored function of
the run, cross-file edges allowed (still conservative name matching — an
ambiguous callee resolves to no edge, never a guess), and the re-rank runs
over every scored line of the run at once instead of per file. Emitted as
`repo_score`, additive next to `file_score`; a single-file input can still
ask for it, but it earns its keep on a folder, a cargo package, or anything
spanning more than one file. `vikt calibrate` measures whichever scope it is
given (default `file`, against a file-wide positional null; `repo` pairs the
same cross-file blend against that same per-file positional null, pooled
across every file the run touches — line numbers reset per file, so the null
stays file-local even when the panel score no longer is).

### SARIF output

`--format sarif` emits SARIF 2.1.0 instead of the sidecar: one `note`-level
result per reported line, ingestible by GitHub Code Scanning (via
`github/codeql-action/upload-sarif`) and any other SARIF consumer.
`--sarif-tiers` selects which tiers become results — `core` (the default)
or `core,boundary`; plumbing and inert are never emitted, because reporting
them would bury the signal on any real file.

```bash
vikt foo.py --format sarif > vikt.sarif
```

### Self-calibration: `vikt calibrate`

The numbers above say how the panel performs on the corpora it was measured
against. `vikt calibrate` measures it on *your* repository, with no
rater in the loop: it mutates lines the panel scored, lets the repository's
own test suite decide which mutants die, and reports the Spearman
correlation between panel score and per-line kill rate — next to the same
positional null the bakeoffs use, because a panel that cannot beat "earlier
is more important" on a tree has nothing to offer it.

```bash
vikt calibrate path/to/repo --test-cmd "python3 -m unittest"          # Python
vikt calibrate path/to/app  --test-cmd "node --test"                  # JavaScript/TypeScript
vikt calibrate path/to/pkg  --test-cmd "cargo test"                   # Rust (a cargo package)
vikt calibrate path/to/repo --test-cmd "..." --scope repo             # cross-file call graph, not just one file
```

The test command runs with `sh -c` from the root of a temporary copy of the
tree. Every mutation happens in that copy; the input tree is never opened
for writing, and an integration test holds this to byte-identity. The
verdict is `calibrated` (panel ρ beats the null by ≥ 0.1 and clears 0.3 on
its own), `marginal`, `uncalibrated`, or `insufficient data` (fewer than 20
scored lines or 30 executed mutants). With `--gate` the exit code carries
it — 0 for calibrated or marginal, 2 for insufficient data, 3 for
uncalibrated; without the flag, exit status only reports whether the
measurement itself ran.

Per-language mechanics: Python mutants are AST round-trips; JavaScript and
TypeScript mutants are byte-span splices re-parsed with oxc before use
(`node_modules` is symlinked into the copy, never scored or mutated;
TypeScript caveat — a type-invalid mutant is read as killed by the
repository's own toolchain, indistinguishable from a test catch); Rust
targets a cargo package and builds every mutant before running the suite
(default `cargo test --no-run`, `--build-cmd` to change) — a splice the
compiler rejects is *invalid* and excluded from every rate, not a kill,
and because each mutant costs a compile the Rust budget defaults to 60.

Limits: the mutant budget is capped (default 150 over the 12 largest scored
functions, `--budget` and `--sample` to change), and hitting the cap is
reported, never silent; the suite must pass on the unmutated copy before
anything is mutated — a failing baseline aborts the run; JVM sources are
not yet calibratable.

`--emit-dataset <path>` additionally writes one JSON line per mutated,
panel-scored line — the seven per-line panel features, the panel score, and
the observed kill counts — which is the raw material for refitting the
panel weights offline against measured behaviour instead of rater labels.

Reproduce the scale run (fetches six production codebases, ~2M instructions,
and prints the per-corpus summary; see `eval/RESULTS-corpus-scale.md` for the
measured results):

```bash
./eval/fetch-corpus.sh && ./eval/run-corpus.sh
```

Try it:

```bash
javac -g demo/java/OrderProcessor.java
./target/release/vikt demo/java/OrderProcessor.class \
    --format text --annotate demo/java/OrderProcessor.java

./target/release/vikt demo/python/orders.py \
    --format text --annotate demo/python/orders.py
```

Both demos are the same program in two languages, and produce the same tiering.

## The artifact

```json
{
  "schema": "vikt-sidecar/v2",
  "generator": "vikt-jvm/mokapot",
  "file": "OrderProcessor.java",
  "functions": [{
    "name": "OrderProcessor::process",
    "signature": "(Ljava/util/List;, D, Z) -> double",
    "decl_line": 11,
    "coverage": { "instructions": 55, "with_line": 55 },
    "summary": { "core": 6, "boundary": 2, "plumbing": 1, "inert": 4 },
    "spans": [
      { "start": 21, "end": 21, "tier": "core", "function_score": 0.8,
        "reasons": ["loop-carried definition at nesting depth 1",
                    "reaches state write OrderProcessor#runningTotal in 1 dependence step(s)"] },
      { "start": 22, "end": 22, "tier": "inert", "function_score": 0.0,
        "reasons": ["builds arguments for a denylisted call only"] }
    ]
  }]
}
```

`coverage` is the honesty field: when a substrate loses line attribution, a
consumer needs to know it is looking at an incomplete map rather than a body
that genuinely has no core.

## Performance

Measured at scale on production code (single thread, panel scorer, release
build; `eval/fetch-corpus.sh && eval/run-corpus.sh` reproduces):

| corpus | functions | p50 / fn | p99 / fn | wall |
|---|---|---|---|---|
| Python stdlib (complete) | 18,888 | 103 µs | 8.8 ms | 87 s |
| django + sqlalchemy + flask + rich | 31,257 | ~100 µs | < 9 ms | 130 s |
| guava + gson + commons-lang3 (JVM) | 18,449 | 32 µs | 0.8 ms | 3.9 s |
| lodash + zod (JS/TS) | 16,413 | ~30 µs | < 1 ms | 5.6 s |

Zero parse failures and zero panics on all of it; 99–100% of instructions
carry a source line. Wall time on Python is dominated by the per-file
CPython lowering subprocess; JVM and JS/TS lower in-process. Running all
five instruments costs ~12% over the incumbent alone, invisible in wall
time.

Lowering and analysis are reported apart because they are paid at different
times. Lowering happens once per file. Analysis is the part that would run
inside an editor hook, and the part the caching story is about.

Output is byte-identical across runs — every set is a `BTreeSet`, every map a
`BTreeMap`, every worklist drains in index order. That is what makes the
artifact cacheable and diffable.

## The one deliberate soundness trade

The inert rule absorbs opaque calls. A statement is inert when it feeds a
denylisted call *and* reaches no hard effect (return, throw, state write).

`prices.size()` inside `LOG.info("processing " + prices.size())` is an opaque
call, and in principle an opaque call could have side effects — so demoting it
because its result only feeds a log is not conservative. But a denylist that
stops at the logging call and leaves every argument expression tiered as
behavior does not do the job it exists to do. Returns, throws and state writes
are observable without seeing inside any callee, and are never absorbed, so the
trade is bounded to the frontier we already declined to cross.

The rule is backward reachability, not an "every consumer is inert" fixpoint,
because the most valuable case is a dependency *cycle*: a counter incremented
only to be logged reads its own previous value, and a least-fixpoint never
enters that loop.

## Known limitations

- **Def-use breaks across a suspension point.** Locals live across an `await`
  are spilled to and restored from continuation fields (`I$0`, `L$0`, `D$0`),
  so a value's chain is cut at every suspension. In the demo this surfaces as
  `reaches state write demo.Processor$fetchAndTotal$1#D$0` — a spill slot being
  read as real state. Excising the dispatch fixes loops and control dependence
  but not this; stitching spilled locals back together is the next fix.
- **`inline` and `suspend` composed is untested.** An inlined `suspend` lambda
  body is relocated past EOF *and* split across a state machine. Both mechanisms
  are handled separately; their interaction has no coverage.
- **Property accessors pile onto the class declaration line.** Kotlin attributes
  every generated getter to the line the class is declared on, so a data class
  with many properties concentrates their importance on one line.
- **Only the `KotlinDebug` stratum is understood.** Other JSR-45 producers
  (JSP, Scala, Groovy) emit different strata; those fall back to the default
  stratum, and lines resolving to a foreign file are dropped and counted rather
  than mapped.
- **SSA renames can vanish.** `double total = subtotal;` compiles to a pure
  rename that MokaIR erases, so that line gets no span.
- **`mokapot`'s MokaIR is behind an `unstable-moka-ir` feature** with no
  stability promise across 0.x. Pinned to `=0.26.0`.
- **Python's stack simulation is exact for ~50 opcodes** and estimates pops from
  `dis.stack_effect` for the rest.
- **JS/TS `finally` is approximated** as straight-line code after the `try`,
  not duplicated along every exit path, and logical short-circuits
  (`&&`, `||`, `??`, `?:`) stay inside their statement rather than branching —
  statement granularity is the frontend's unit.
- **Intraprocedural only**, by design. Calls are atoms at the abstraction
  frontier; a callee's body is never analyzed. That keeps cost per function
  bounded and independent of dependency-graph size.
- **No column granularity.** `co_positions()` offers it for Python; the artifact
  records lines.

## Tests

112 tests: 32 in the core over hand-built IR (pinning each instrument's
algorithm and the panel's weights rather than any frontend's lowering), 17
integration tests over the analysis pipeline, 10 over the SARIF projection
and the calibration statistics, 11 over real SMAP attribute text and
synthetic state machines, 8 over real `javac -g` and `kotlinc` output, 5
over real CPython bytecode, 4 over the Python mutant generator, 10 over
real oxc lowering of JavaScript and TypeScript, 4 over real MIR lowering
of Rust, 10 over the CLI (including a calibration run that asserts the
input tree comes out byte-identical), 1 doctest. The JVM, Python and JS suites assert
the *same* behavioral claims against equivalent source — the accumulator
that reaches a state write is core, the counter that only feeds logging is
inert — which is the multi-language claim stated as a test.

```bash
cargo test
```

JVM, Kotlin and Python tests skip rather than fail when no JDK, `kotlinc` or
interpreter is present.

## Development setup

One-time, per clone — activates the versioned git hooks in `.githooks/`
(currently a `commit-msg` hook that strips AI attribution trailers from
commit messages):

```bash
git config core.hooksPath .githooks
```

## License

MIT OR Apache-2.0.
