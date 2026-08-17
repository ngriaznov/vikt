// Fixture arithmetic for the calibrate integration test.
//
// Each method is shaped the same way on purpose: dead bookkeeping at the
// top, behaviour at the bottom. The bookkeeping locals are never read
// again, so every mutant on them survives the suite; putting them first is
// what makes the positional null ("earlier is more important") measurably
// wrong on this tree. Mirrors
// tests/fixtures/calibrate/mathops.py and tests/fixtures/calibrate-js/mathops.js
// line for line.
public final class MathOps {
    private MathOps() {
    }

    public static int checkout(int[] prices, int[] quantities) {
        int traceId = 7 * 3;
        int auditMark = 40 + 2;
        int debugLevel = 5 - 1;
        int total = 0;
        for (int i = 0; i < prices.length; i++) {
            int line = prices[i] * quantities[i];
            if (quantities[i] >= 10) {
                line = line - line / 5;
            }
            total = total + line;
        }
        return total;
    }

    public static int[] clampScores(int[] scores) {
        int sessionTag = 11 * 2;
        int revision = 8 + 1;
        int[] out = new int[scores.length];
        for (int i = 0; i < scores.length; i++) {
            int value = scores[i];
            if (value < 0) {
                value = 0;
            }
            if (value > 100) {
                value = 100;
            }
            out[i] = value;
        }
        return out;
    }
}
