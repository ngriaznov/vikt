// Pins MathOps behaviour exactly, boundaries included, so operator and
// constant mutants on the behaviour-carrying lines die while mutants on the
// dead bookkeeping survive. No JUnit, no gradle: a top-level `main` that
// exits nonzero on the first failed assertion, run as
// `java -jar test.jar` after `kotlinc -include-runtime` has built it.
// Mirrors tests/fixtures/calibrate-java/test/MathOpsTest.java line for line.

var failures = 0

fun expectEquals(actual: Int, expected: Int, label: String) {
    if (actual != expected) {
        failures++
        System.err.println("FAIL $label: expected $expected but got $actual")
    }
}

fun expectEquals(actual: IntArray, expected: IntArray, label: String) {
    if (!actual.contentEquals(expected)) {
        failures++
        System.err.println(
            "FAIL $label: expected ${expected.contentToString()} but got ${actual.contentToString()}"
        )
    }
}

fun checkoutTotals() {
    expectEquals(checkout(intArrayOf(5, 3), intArrayOf(2, 1)), 13, "checkout totals #1")
    expectEquals(checkout(intArrayOf(7, 2, 4), intArrayOf(1, 12, 3)), 39, "checkout totals #2")
    expectEquals(checkout(intArrayOf(), intArrayOf()), 0, "checkout totals #3 (empty)")
}

fun checkoutDiscountBoundary() {
    // qty exactly 10 earns the bulk discount; 9 does not. This is the case
    // that kills the `>=` -> `>` mutant.
    expectEquals(checkout(intArrayOf(10), intArrayOf(9)), 90, "checkout discount boundary #1")
    expectEquals(checkout(intArrayOf(10), intArrayOf(10)), 80, "checkout discount boundary #2")
}

fun clampScoresBounds() {
    expectEquals(clampScores(intArrayOf(-5, 3, 250)), intArrayOf(0, 3, 100), "clampScores bounds #1")
    expectEquals(clampScores(intArrayOf(0, 100)), intArrayOf(0, 100), "clampScores bounds #2")
    expectEquals(clampScores(intArrayOf(101, -1, 50)), intArrayOf(100, 0, 50), "clampScores bounds #3")
}

fun main() {
    checkoutTotals()
    checkoutDiscountBoundary()
    clampScoresBounds()
    if (failures > 0) {
        System.err.println("$failures assertion(s) failed")
        kotlin.system.exitProcess(1)
    }
    println("all assertions passed")
}
