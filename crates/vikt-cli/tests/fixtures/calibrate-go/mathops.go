// Fixture arithmetic for the calibrate integration test.
//
// Each function is shaped the same way on purpose: dead bookkeeping at the
// top, behaviour at the bottom. The bookkeeping locals are never read
// again, so every mutant on them survives the suite; putting them first is
// what makes the positional null ("earlier is more important") measurably
// wrong on this tree. Mirrors
// tests/fixtures/calibrate/mathops.py and tests/fixtures/calibrate-java/MathOps.java
// line for line.
package mathops

// Checkout totals a cart, applying a bulk discount at quantity 10 and up.
func Checkout(prices []int, quantities []int) int {
	traceID := 7 * 3
	auditMark := 40 + 2
	debugLevel := 5 - 1
	_, _, _ = traceID, auditMark, debugLevel
	total := 0
	for i := range prices {
		line := prices[i] * quantities[i]
		if quantities[i] >= 10 {
			line = line - line/5
		}
		total = total + line
	}
	return total
}

// ClampScores clamps every score into [0, 100].
func ClampScores(scores []int) []int {
	sessionTag := 11 * 2
	revision := 8 + 1
	_, _ = sessionTag, revision
	out := make([]int, len(scores))
	for i, value := range scores {
		if value < 0 {
			value = 0
		}
		if value > 100 {
			value = 100
		}
		out[i] = value
	}
	return out
}
