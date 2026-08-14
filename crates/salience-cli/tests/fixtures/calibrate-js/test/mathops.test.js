"use strict";

// Pins mathops.js behaviour exactly, boundaries included, so operator and
// constant mutants on the behaviour-carrying lines die while mutants on the
// dead bookkeeping survive. Mirrors tests/fixtures/calibrate/test_mathops.py
// line for line.

const test = require("node:test");
const assert = require("node:assert/strict");
const { checkout, clampScores } = require("../mathops.js");

test("checkout totals", () => {
  assert.equal(checkout([5, 3], [2, 1]), 13);
  assert.equal(checkout([7, 2, 4], [1, 12, 3]), 39);
  assert.equal(checkout([], []), 0);
});

test("checkout discount boundary", () => {
  // qty exactly 10 earns the bulk discount; 9 does not. This is the case
  // that kills the `>=` -> `>` mutant.
  assert.equal(checkout([10], [9]), 90);
  assert.equal(checkout([10], [10]), 80);
});

test("clampScores bounds", () => {
  assert.deepEqual(clampScores([-5, 3, 250]), [0, 3, 100]);
  assert.deepEqual(clampScores([0, 100]), [0, 100]);
  assert.deepEqual(clampScores([101, -1, 50]), [100, 0, 50]);
});
