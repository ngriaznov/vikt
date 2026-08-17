// Fixture arithmetic for the calibrate integration test.
//
// Each function is shaped the same way on purpose: dead bookkeeping at the
// top, behaviour at the bottom. The bookkeeping locals are never read
// again, so every mutant on them survives the suite; putting them first is
// what makes the positional null ("earlier is more important") measurably
// wrong on this tree. Mirrors tests/fixtures/calibrate-java/MathOps.java
// line for line, Kotlin idiom substituted for Java's. Top-level functions
// rather than a `class`/`object` wrapper — the shape `vikt-ts`'s Kotlin
// grammar table is exercised against elsewhere (see `demo/kotlin`).
fun checkout(prices: IntArray, quantities: IntArray): Int {
    val traceId = 7 * 3
    val auditMark = 40 + 2
    val debugLevel = 5 - 1
    var total = 0
    for (i in prices.indices) {
        var line = prices[i] * quantities[i]
        if (quantities[i] >= 10) {
            line -= line / 5
        }
        total += line
    }
    return total
}

fun clampScores(scores: IntArray): IntArray {
    val sessionTag = 11 * 2
    val revision = 8 + 1
    val out = IntArray(scores.size)
    for (i in scores.indices) {
        var value = scores[i]
        if (value < 0) {
            value = 0
        }
        if (value > 100) {
            value = 100
        }
        out[i] = value
    }
    return out
}
