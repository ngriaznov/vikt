# salience

Deterministic per-line importance over function bodies.

For every line of every function, one question: **does this line carry
behavior?** The answer is a tier and a score, computed from dominance, loop
structure and def-use reachability over an IR. No model runs. Nothing is
removed. Every span carries the reason that produced it.

```
$ salience OrderProcessor.class --annotate OrderProcessor.java

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
identical. `subtotal` reaches a field write, so it is core; `inspected` reaches
nothing but a log call, so it is inert. No syntactic analysis separates these —
only dependence does.

## Tiers

| tier | meaning |
|---|---|
| `core` | behavior-carrying: branch predicates weighted by what they control-dominate, loop-carried dataflow, statements on def-use chains reaching an effect |
| `boundary` | the frontier where behavior leaves the body: returns, throws, state writes, calls into opaque dependencies |
| `plumbing` | present but not behavior-carrying: local shuffling, results that reach no effect |
| `inert` | denylisted calls (logging, metrics, tracing) and the computation that exists only to feed them |

Alongside the tier, every span carries two numbers:

- **`score`** — `0.0..=1.0` against a fixed scale. For thresholds and policy.
- **`rank`** — percentile within its own function. For heatmaps, and for "show
  me the top of this body". A function whose scores all sit near 0.3 still has a
  most-important line; painting it against the global scale would render the
  whole body one flat colour.

The score is produced by an **instrument panel** — five independent algorithms
combined by weights fitted against blind expert judgement — described in the
next two sections. Every panel member is also selectable alone via `--scorer`,
because a combination whose members cannot be inspected individually cannot be
re-measured honestly.

## The score: an instrument panel

No single structural measure of a dependence graph orders a function's lines
the way an expert reader does. That is a measured result, not a stance: we
implemented nineteen algorithms from nineteen fields (full roster below),
compared each against per-line importance labels written blind by an expert
rater before any scorer ran, and the best solo instrument reached Spearman
ρ ≈ 0.36 — statistically indistinguishable from the null heuristic "later
lines matter more" (0.359). Every algorithm, including the best, also has
functions it ranks *backwards*.

But they fail in **different places**. Deletion-sensitivity understands
duplicated structure that flow measures double-count; confluence order is
robust exactly where reliability analysis is noisy; derivation depth explains
the functions whose importance accumulates toward the end. Importance is not
one quantity — it has facets — so the shipped score is a linear combination of
five instruments plus two structural facts, with ridge weights fitted on the
expert labels (16 stdlib functions, 425 lines; λ chosen by inner
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

Mechanics (all in `salience-core/src/panel.rs`, pure Rust like everything
else): each instrument's per-node scores are projected to lines by `max`,
rank-normalised within the function — a scorer's *scale* is arbitrary across
functions, its *ordering* is the signal — then combined by the fitted weights.
The weights are baked constants with their provenance in the module docs, and
a test fails if anyone edits one without re-fitting: the held-out number is
only true of exactly those values. Tier assignment never varies with scorer
choice; only the continuous score does.

## The search: nineteen algorithms, nineteen fields

The selection principle was symmetry across systems. Mature sciences have
spent decades formalising "which element of this network matters" — for
metabolisms, power grids, food webs, citation graphs, river basins, social
networks — and a function's dependence graph is a small directed network with
sources (parameters, constants), internal transformation, and sinks (returns,
writes, calls). Each candidate was chosen because it is *the* importance
notion of its home field, and transplanted by mapping its home network onto
the dependence graph. Every one was implemented in Rust on its own branch
(`claude/scorer-*`), verified deterministic and genuinely different from the
incumbent, then measured against the blind labels — solo, and by its marginal
contribution to the fitted combination on held-out functions.

**Panel members** (why they earned a seat):

- **`schur` — Schur-complement deletion sensitivity** (linear algebra /
  network zeta). Deleting row and column *v* from the influence system gives,
  in closed form, exactly how much total delivered influence disappears — "what
  breaks if this line is removed", answered exactly rather than by proxy. Best
  all-rounder of round one (solo 0.34); its signature win is `bisect_right`
  (ρ = 0.89 where every flow-style scorer goes negative), because deletion
  sensitivity is immune to the double-counting that near-duplicate code paths
  induce in flow measures.
- **`pivot` — Birnbaum structural importance** (reliability engineering).
  Treat the graph as a coherent system: how often is this component the pivot
  between "system delivers" and "system fails"? Equals the Banzhaf voting
  index. Modest solo (0.13) but stable positive weight in every fit — it sees
  single-points-of-failure that cone sizes miss.
- **`trophic` — trophic levels** (food-web ecology). Basal species eat
  nothing; a predator's level is one plus the mean level of what it eats.
  Transplanted: parameters and constants are basal, and a statement's level is
  its derivation depth. Chosen deliberately as a graph-native replacement for
  the positional null after measurement showed judgement has a real positional
  component; it wins precisely on the functions where position wins
  (`get_close_matches` 0.84, `quantiles` 0.61) and displaced a quarter of raw
  position's fitted weight.
- **`strahler` — Horton–Strahler stream order** (geomorphology). Springs are
  order 1; only when two streams of *equal* order meet does the order rise, so
  high order marks the mainstem of the basin. Transplanted: dataflow is the
  river, and the mainstem is where independent derivations converge. Best solo
  instrument of the entire search (0.357) and the most robust — one backwards
  function out of sixteen, where every other algorithm has two or more. Its
  virtue is what it ignores: it does not care how *much* flows, only where
  independent tributaries join.
- **`current` — the incumbent** described throughout this README: dependence
  cones, control-dominance mass, loop depth, effect kind. Kept because it
  covers short imperative loop kernels (`_siftdown` 0.73) where every imported
  algorithm stumbles.

**Measured and cut** (each was implemented completely, verified, and lost on
the same labels — the branches remain for re-measurement):

- **`flux` — flux balance analysis** (systems biology). Metabolic networks
  route mass from nutrients to biomass; transplant values flowing from
  parameters to effects. Chosen for the strongest source-to-sink analogy of
  round one. Cut: FBA answers a *categorical* question (essential / variable /
  blocked) — three score values cannot rank a function.
- **`mincut` — max-flow/min-cut necessity** (combinatorial optimisation). Is
  this node on every minimum cut between inputs and outputs? Cut: same
  categorical failure; and bottleneck-ness turned out anti-correlated with
  judgement on validation-heavy code.
- **`observ` — structural controllability** (control theory, Liu–Slotine–
  Barabási). Driver/critical nodes of a network. Decent solo (0.27) but fully
  redundant with the panel — zero marginal held-out contribution.
- **`vitality` — resolvent perturbation / T-matrix** (mathematical physics).
  How much the graph's Green's function changes when a node is perturbed.
  Middling solo, negative fitted weight — schur carries the same facet better.
- **`absorb` — absorbing Markov chains** (probability). Expected visits before
  absorption at an effect. The only scorer positive on all three round-one
  targets, but its facet is subsumed by pivot + schur in combination.
- **`leak` — quantitative information flow** (security). Channel capacity from
  a statement to observable outputs. Promising on paper; degenerate gradient on
  a fifth of real functions.
- **`current-flow` — current-flow betweenness** (physics of resistor
  networks). Random-walk betweenness; was briefly pinned, but its fitted
  weight went *negative* once strahler arrived — same facet, noisier.
- **`dirichlet` — Dirichlet forms / effective resistance** (potential
  theory). Energy of the harmonic extension. Fine gradient, no judgement
  signal (solo 0.06).
- **`leverage` — statistical leverage scores** (statistics / randomised
  linear algebra). Row leverage of the dependence matrix. Won one axis in
  early rounds (behavioural leverage), never the judgement axis.
- **`magnitude` — Leinster magnitude** (enriched category theory). The
  "effective number of points" of a metric space. Second place on one early
  target — and flat (single-valued) on 20% of real functions, which is
  disqualifying for a heatmap.
- **`hankel` — Hankel singular values** (control theory, model reduction).
  How much state does this node carry between past and future. Strong on
  contrived fixtures, weak on real code.
- **`rarity` — TF-IDF self-information** (information retrieval). The rare
  operation among routine moves is the formula line. Solo 0.03: statistical
  surprise is only occasionally importance.
- **`broker` — Burt's structural holes** (sociology). Low-constraint nodes
  bridge otherwise-disconnected clusters. Real signal on parser-shaped
  functions (`urlsplit` 0.60), not enough overall (0.16).
- **`disrupt` — the CD/disruption index** (science of science). Does later
  work cite this paper *without* citing its references? The most instructive
  failure: solo **−0.15**, ranking backwards on 13 of 16 functions. The
  citation analogy assumes consumers can cite several generations at once;
  stack-machine dataflow consumes each value exactly once, so the pattern the
  index needs almost never occurs. A reminder that the transplant, not the
  theorem, is what has to survive.

The measurement protocol, the labels, and the fitting scripts live in
`eval/` — `ground-truth-v3.json` (the blind labels), `judgement.py` (the fit
and the held-out evaluation), `panel.py` (a Python reference of the panel that
the Rust implementation is acceptance-tested against, ρ ≥ 0.996), and
`RESULTS-leading-algorithms.md` (the full history, including the dead ends
and the corrections).

## Calibration

Every threshold and weight here was set against real code, not chosen a priori:
~28,000 JVM library functions (Guava, commons-lang3, jackson-databind, OkHttp)
and the Python standard library. `cargo run --release --example evaluate` is the
harness. Three findings changed the design:

- **"Reaches an effect" is not a tier.** It held for 86-100% of statements,
  because real code is dense with calls, and produced a constant map. It is now
  an explanation only; `core` is driven by influence, control mass and
  loop-carried dataflow.
- **Inert needs two conditions, not one.** Keying it on "feeds logging and
  reaches no hard effect" mislabelled 56% of its hits — including a network read
  poisoned by a single `print` ten lines away. Keying it on usefulness alone took
  that to 90%, swallowing `pass` and constant definitions. It now requires both
  no useful sink downstream *and* a denylisted one.
- **Effects must score for being effects.** The cone terms alone rank a `return`
  below the setup lines feeding it, because nothing depends on a return.

## What it's for

The same map serves several consumers, which is why it emits both a
classification and a ranking:

- **Agent reading guidance** — what to read first in an unfamiliar body.
- **Agent edit policy** — a harness hook that demands verification when an edit
  touches a core span, and waves through a logging change.
- **Weighted dependency graphs** — nodes weighted by the salience of what they
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
salience-core   language-neutral. Dominance, post-dominance, control dependence,
                natural loops, reaching definitions, tiering, scoring, projection.
                Knows nothing about any language.
      ^
      |  FunctionIr  (the contract: line, defs, uses, successors, kind)
      |
salience-jvm    .class -> mokapot MokaIR -> FunctionIr
salience-py     .py -> CPython dis -> JSON -> FunctionIr
salience-cli    the `salience` binary
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
matches. An AST shows the syntax someone wrote; the bytecode shows the control
flow that runs. The `LineNumberTable` maps it back to the lines they will edit.

The same argument holds more weakly for Python — comprehensions are separate
code objects, `and`/`or` are jumps, a `for` loop's real exit test is `FOR_ITER`.

## Adding a language

Implement one function: substrate → `Vec<FunctionIr>`.

| language | substrate | line fidelity | status |
|---|---|---|---|
| Java | JVM bytecode via `mokapot` | `LineNumberTable`, needs `javac -g` | **working** |
| Kotlin | same | `LineNumberTable` **plus** JSR-45 SMAP resolution | **working** — see below |
| Python | CPython bytecode via `dis` | PEP 626 `co_lines()`, exact | **working** |
| Rust | MIR via `rustc_public` | MIR spans | blocked: nightly-only |
| C/C++/Swift | LLVM IR `DILocation` | debug info | not attempted |
| JS/TS | no standard IR; Google's JSIR is the exception | source maps | weak |

## Kotlin: the measurement that changed the design

The premise of using bytecode over ASTs is that Kotlin syntax hides real control
flow. That premise cuts both ways, so it was tested rather than assumed. On
`kotlinc 2.1.20`, against an **80-line** fixture:

```
usesInline LineNumberTable:  line 56 -> pc 6
                             line 82 -> pc 11     <-- past end of file
                             line 83 -> pc 16     <-- past end of file
                             line 57 -> pc 19
                             ...
                             line 85 -> pc 108    <-- past end of file
```

Lines 82 through 89 are not source lines. They are positions in a synthetic
composite file that exists only inside the `SourceDebugExtension` attribute:

- **82–85** are the inlined body of `timed`, declared on lines 9–12.
- **86–89** are inlined `map`, from `kotlin/collections/_Collections.kt` — the
  standard library, a file the developer never opened.

A tool that reads `LineNumberTable` and stops there emits spans for lines that
do not exist, and silently attributes standard-library code to the user's file.
That is not a rough edge; for a tool whose entire output is "line N matters", it
is wrong output. **The naive implementation was wrong, and the measurement is
what caught it.**

The fix is the `KotlinDebug` stratum, which exists for exactly this and is what
the IntelliJ debugger steps through:

```
*S KotlinDebug
*L
56#1:82,4      <- output lines 82..85 are all line 56
79#1:86        <- output line 86 is line 79
79#1:87,3      <- output lines 87..89 are line 79
```

Inlined work collapses onto its **call site**, which is also the right answer
semantically: the developer sees `timed("sum") { ... }` on line 56, and the work
the inlined body does really is work that line causes. After resolution, no span
in the fixture exceeds line 79.

### What every other Kotlin construct does

Measured on the same fixture, not inferred:

| construct | what happens to line numbers | handling |
|---|---|---|
| `inline` fun call site | lines past EOF, in a synthetic composite file | **SMAP-resolved to the call site** |
| stdlib inline (`map`, `let`, …) | lines belonging to another file entirely | **SMAP-resolved; unmappable ones dropped and counted** |
| `suspend` fun | lines stay real and in range, but the dispatch makes the CFG **irreducible** | **state machine excised** — see below |
| continuation class (`Foo$bar$1`) | zero line information at all | benign — contributes no spans |
| default arguments | the `$default` bridge is `ACC_SYNTHETIC` but carries a **full** line table duplicating the real method's | **filtered** — otherwise every such line is analyzed twice |
| data class `equals`/`hashCode`/`copy`/`componentN` | not flagged synthetic, but carry no line table | benign |
| `when` over enum | the `WhenMappings` class is entirely unlined | benign |
| enum `$values`, `valueOf` | unlined or partially lined | benign |
| property accessors | all attributed to the class declaration line | minor pile-up on one line |

The `$default` case is the one that would have been easy to miss: it is the only
generated member that carries a *complete* line table, so it looks like real
code. Filtering `ACC_SYNTHETIC` and `ACC_BRIDGE` is also where JaCoCo landed
after years of Kotlin coverage bug reports.

### `suspend` breaks dominance, not attribution

The second Kotlin problem is the opposite shape of the first, which is why
finding one does not warn you about the other. Line numbers in a `suspend fun`
are **correct and in range**. What breaks is the graph.

A `suspend fun` compiles to a state machine in the same JVM method: the prologue
reads a `label` off the continuation and dispatches through a `tableswitch`
whose arms jump straight into the middle of the body, one per suspension point.

```
node  31  switch %64 { 0 => #005C, 1 => #00D0, 2 => #014A, else => #0165 }
node 117  goto #008A                                 <- the loop's back edge
```

Arm `1` enters the loop body without passing the header, so the header stops
dominating the tail. The back edge is still there, but it is no longer a
*dominator* back edge — the graph is irreducible. Measured before the fix:

```
demo/Processor::plain           natural_loops=1   retreating_edges=2
demo/Processor::usesInline      natural_loops=1   retreating_edges=1
demo/Processor::fetchAndTotal   natural_loops=0   retreating_edges=1   <-- suspend
```

A textbook natural-loop detector — which is exactly what the core runs — finds
**zero loops** in a `suspend` function whose loop contains a suspension point.
Every loop-carried definition in it goes unreported, and control dependence is
distorted besides. Given how much modern Kotlin is coroutine code, that is a
large silent hole.

The fix is JaCoCo's: delete the machine. `crates/salience-jvm/src/coroutine.rs`
recognises the dispatch by the `IntrinsicsKt::getCOROUTINE_SUSPENDED` call in the
prologue, prunes every arm but the normal entry, and clears line attribution on
the resume-restore blocks that pruning makes unreachable — they re-execute lines
the normal path already covers, so no source line is lost. After excision
`fetchAndTotal` reports `natural_loops=1`.

Doing this as a **graph rewrite in the frontend** rather than a special case in
the analysis is deliberate: the language-neutral core still has no idea
coroutines exist.

### Reproducing

```bash
kotlinc demo/kotlin/Orders.kt -cp <coroutines.jar> -d demo/kotlin/out
./target/release/salience demo/kotlin/out/demo/Processor.class --format text
javap -p -c -l demo/kotlin/out/demo/Processor.class   # the raw table, for contrast
```

`cargo test -p salience-jvm` compiles Kotlin at test time when `kotlinc` is on
PATH and asserts no span exceeds the file length; with SMAP resolution disabled
that test fails with `span 47-47 exceeds the 45-line source file`. The mapping
itself is pinned hermetically by unit tests in `crates/salience-jvm/src/smap.rs`
using the exact attribute text `kotlinc` emitted, so losing the compiler loses
coverage of the plumbing, not of the mapping.

## Usage

```bash
cargo build --release

salience Foo.class                          # JSON sidecar, panel score (default)
salience foo.py --format text               # one line per span
salience Foo.class --annotate Foo.java      # tiered source view
salience Foo.class --stats                  # histogram and timing
salience Foo.class --inert 'com.acme.Audit' # extend the denylist
salience Foo.class --no-denylist            # treat nothing as inert
salience foo.py --scorer strahler           # one instrument alone
salience foo.py --scorer current            # the incumbent alone
```

`--scorer` selects which algorithm produces the continuous score: `panel`
(default), or any single instrument — `current`, `schur`, `pivot`, `trophic`,
`strahler`. Tier assignment is identical under every choice.

Try it:

```bash
javac -g demo/java/OrderProcessor.java
./target/release/salience demo/java/OrderProcessor.class \
    --format text --annotate demo/java/OrderProcessor.java

./target/release/salience demo/python/orders.py \
    --format text --annotate demo/python/orders.py
```

Both demos are the same program in two languages, and produce the same tiering.

## The artifact

```json
{
  "schema": "salience-sidecar/v1",
  "generator": "salience-jvm/mokapot",
  "file": "OrderProcessor.java",
  "functions": [{
    "name": "OrderProcessor::process",
    "signature": "(Ljava/util/List;, D, Z) -> double",
    "decl_line": 11,
    "coverage": { "instructions": 55, "with_line": 55 },
    "summary": { "core": 6, "boundary": 2, "plumbing": 1, "inert": 4 },
    "spans": [
      { "start": 21, "end": 21, "tier": "core", "score": 0.8,
        "reasons": ["loop-carried definition at nesting depth 1",
                    "reaches state write OrderProcessor#runningTotal in 1 dependence step(s)"] },
      { "start": 22, "end": 22, "tier": "inert", "score": 0.0,
        "reasons": ["builds arguments for a denylisted call only"] }
    ]
  }]
}
```

`coverage` is the honesty field: when a substrate loses line attribution, a
consumer needs to know it is looking at an incomplete map rather than a body
that genuinely has no core.

## Performance

Release build, 3-method class, 62 instructions:

```
lowering    546µs   (file read + class parse + MokaIR lift)
analysis    202µs   -> 67µs per function
```

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
  with many properties concentrates their salience on one line.
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
- **Intraprocedural only**, by design. Calls are atoms at the abstraction
  frontier; a callee's body is never analyzed. That keeps cost per function
  bounded and independent of dependency-graph size.
- **No column granularity.** `co_positions()` offers it for Python; the artifact
  records lines.

## Tests

42 tests: 17 in the core over hand-built IR (pinning the algorithm rather than
any frontend's lowering), 11 over real SMAP attribute text and synthetic state
machines, 8 over real `javac -g` and `kotlinc` output, 5 over real CPython
bytecode, 1 doctest. The JVM and
Python suites assert the *same* behavioral claims against equivalent source,
which is the multi-language claim stated as a test.

```bash
cargo test
```

JVM, Kotlin and Python tests skip rather than fail when no JDK, `kotlinc` or
interpreter is present.

## License

MIT OR Apache-2.0.
