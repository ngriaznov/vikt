"""Pins mathops behaviour exactly, boundaries included, so that operator and
constant mutants on the behaviour-carrying lines die while mutants on the
dead bookkeeping survive."""

import unittest

import mathops


class CheckoutTest(unittest.TestCase):
    def test_totals(self):
        self.assertEqual(mathops.checkout([5, 3], [2, 1]), 13)
        self.assertEqual(mathops.checkout([7, 2, 4], [1, 12, 3]), 39)
        self.assertEqual(mathops.checkout([], []), 0)

    def test_discount_boundary(self):
        # qty exactly 10 earns the bulk discount; 9 does not. This is the
        # case that kills the `>=` -> `>` mutant.
        self.assertEqual(mathops.checkout([10], [9]), 90)
        self.assertEqual(mathops.checkout([10], [10]), 80)


class ClampTest(unittest.TestCase):
    def test_bounds(self):
        self.assertEqual(mathops.clamp_scores([-5, 3, 250]), [0, 3, 100])
        self.assertEqual(mathops.clamp_scores([0, 100]), [0, 100])
        self.assertEqual(mathops.clamp_scores([101, -1, 50]), [100, 0, 50])


if __name__ == "__main__":
    unittest.main()
