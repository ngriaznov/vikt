"""Pins textops behaviour exactly: whitespace, casing, the length-4 boundary,
negatives, zero and the cap boundary all appear so their mutants die."""

import unittest

import textops


class SqueezeTest(unittest.TestCase):
    def test_shapes_words(self):
        self.assertEqual(textops.squeeze(["  Hello ", "WORLD"]), "hell-worl")
        self.assertEqual(textops.squeeze(["ab", "", "  "]), "ab")
        # Exactly four letters is left whole: kills the `>` -> `>=` mutant.
        self.assertEqual(textops.squeeze(["Four", "fives"]), "four-five")


class RunningTotalTest(unittest.TestCase):
    def test_accumulates(self):
        self.assertEqual(textops.running_total([2, 0, 3], 10), [2, 2, 5])
        # Negatives are skipped, not added: kills the `continue` deletion.
        self.assertEqual(textops.running_total([4, -2, 1], 10), [4, 5])
        # The cap engages and holds: kills the cap-assignment deletion.
        self.assertEqual(textops.running_total([6, 6, 6], 10), [6, 10, 10])
        self.assertEqual(textops.running_total([], 5), [])


if __name__ == "__main__":
    unittest.main()
