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

Alongside the tier, every span carries a score in `0.0..=1.0`. The **tier is for
policy** — an edit-gating hook wants a predicate. The **score is for ranking** —
a weighted call graph, a profiler choosing where to start, or a vulnerability
triage queue wants an ordering, and four buckets throw away the gradient between
a predicate guarding two lines and one guarding forty.

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

salience Foo.class                          # JSON sidecar on stdout
salience foo.py --format text               # one line per span
salience Foo.class --annotate Foo.java      # tiered source view
salience Foo.class --stats                  # histogram and timing
salience Foo.class --inert 'com.acme.Audit' # extend the denylist
salience Foo.class --no-denylist            # treat nothing as inert
```

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
