"use strict";

// Pins textops.js behaviour exactly: whitespace, casing, the length-4
// boundary, negatives, zero and the cap boundary all appear so their
// mutants die. Mirrors tests/fixtures/calibrate/test_textops.py line for
// line.

const test = require("node:test");
const assert = require("node:assert/strict");
const { squeeze, runningTotal } = require("../textops.js");

test("squeeze shapes words", () => {
  assert.equal(squeeze(["  Hello ", "WORLD"]), "hell-worl");
  assert.equal(squeeze(["ab", "", "  "]), "ab");
  // Exactly four letters is left whole: kills the `>` -> `>=` mutant.
  assert.equal(squeeze(["Four", "fives"]), "four-five");
});

test("runningTotal accumulates", () => {
  assert.deepEqual(runningTotal([2, 0, 3], 10), [2, 2, 5]);
  // Negatives are skipped, not added: kills the `continue` deletion.
  assert.deepEqual(runningTotal([4, -2, 1], 10), [4, 5]);
  // The cap engages and holds: kills the cap-assignment deletion.
  assert.deepEqual(runningTotal([6, 6, 6], 10), [6, 10, 10]);
  assert.deepEqual(runningTotal([], 5), []);
});
