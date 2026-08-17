// Pins Checkout/ClampScores behaviour exactly, boundaries included, so
// operator and constant mutants on the behaviour-carrying lines die while
// mutants on the dead bookkeeping survive. Mirrors
// tests/fixtures/calibrate-java/test/MathOpsTest.java line for line.
package mathops

import "testing"

func TestCheckoutTotals(t *testing.T) {
	if got := Checkout([]int{5, 3}, []int{2, 1}); got != 13 {
		t.Errorf("checkout totals #1: got %d, want 13", got)
	}
	if got := Checkout([]int{7, 2, 4}, []int{1, 12, 3}); got != 39 {
		t.Errorf("checkout totals #2: got %d, want 39", got)
	}
	if got := Checkout([]int{}, []int{}); got != 0 {
		t.Errorf("checkout totals #3 (empty): got %d, want 0", got)
	}
}

func TestCheckoutDiscountBoundary(t *testing.T) {
	// qty exactly 10 earns the bulk discount; 9 does not. This is the
	// case that kills the `>=` -> `>` mutant.
	if got := Checkout([]int{10}, []int{9}); got != 90 {
		t.Errorf("checkout discount boundary #1: got %d, want 90", got)
	}
	if got := Checkout([]int{10}, []int{10}); got != 80 {
		t.Errorf("checkout discount boundary #2: got %d, want 80", got)
	}
}

func TestClampScoresBounds(t *testing.T) {
	cases := []struct {
		in, want []int
	}{
		{[]int{-5, 3, 250}, []int{0, 3, 100}},
		{[]int{0, 100}, []int{0, 100}},
		{[]int{101, -1, 50}, []int{100, 0, 50}},
	}
	for _, c := range cases {
		got := ClampScores(c.in)
		if !equalInts(got, c.want) {
			t.Errorf("clampScores bounds: got %v, want %v", got, c.want)
		}
	}
}

func equalInts(a, b []int) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
