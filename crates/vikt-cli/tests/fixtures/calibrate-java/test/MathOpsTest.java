import java.util.Arrays;

// Pins MathOps behaviour exactly, boundaries included, so operator and
// constant mutants on the behaviour-carrying lines die while mutants on the
// dead bookkeeping survive. No JUnit, no gradle: a bare `main` that exits
// nonzero on the first failed assertion, run as `java -cp out MathOpsTest`
// after `javac` has produced `out/`. Mirrors
// tests/fixtures/calibrate/test_mathops.py line for line.
public final class MathOpsTest {
    private static int failures;

    public static void main(String[] args) {
        checkoutTotals();
        checkoutDiscountBoundary();
        clampScoresBounds();
        if (failures > 0) {
            System.err.println(failures + " assertion(s) failed");
            System.exit(1);
        }
        System.out.println("all assertions passed");
    }

    private static void checkoutTotals() {
        expectEquals(MathOps.checkout(new int[] {5, 3}, new int[] {2, 1}), 13, "checkout totals #1");
        expectEquals(MathOps.checkout(new int[] {7, 2, 4}, new int[] {1, 12, 3}), 39, "checkout totals #2");
        expectEquals(MathOps.checkout(new int[] {}, new int[] {}), 0, "checkout totals #3 (empty)");
    }

    private static void checkoutDiscountBoundary() {
        // qty exactly 10 earns the bulk discount; 9 does not. This is the
        // case that kills the `>=` -> `>` mutant.
        expectEquals(MathOps.checkout(new int[] {10}, new int[] {9}), 90, "checkout discount boundary #1");
        expectEquals(MathOps.checkout(new int[] {10}, new int[] {10}), 80, "checkout discount boundary #2");
    }

    private static void clampScoresBounds() {
        expectEquals(MathOps.clampScores(new int[] {-5, 3, 250}), new int[] {0, 3, 100}, "clampScores bounds #1");
        expectEquals(MathOps.clampScores(new int[] {0, 100}), new int[] {0, 100}, "clampScores bounds #2");
        expectEquals(MathOps.clampScores(new int[] {101, -1, 50}), new int[] {100, 0, 50}, "clampScores bounds #3");
    }

    private static void expectEquals(int actual, int expected, String label) {
        if (actual != expected) {
            failures++;
            System.err.println("FAIL " + label + ": expected " + expected + " but got " + actual);
        }
    }

    private static void expectEquals(int[] actual, int[] expected, String label) {
        if (!Arrays.equals(actual, expected)) {
            failures++;
            System.err.println("FAIL " + label + ": expected " + Arrays.toString(expected)
                    + " but got " + Arrays.toString(actual));
        }
    }
}
