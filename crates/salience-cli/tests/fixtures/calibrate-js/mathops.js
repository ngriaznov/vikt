"use strict";

// Fixture arithmetic for the calibrate integration test.
//
// Each function is shaped the same way on purpose: dead bookkeeping at the
// top, behaviour at the bottom. The bookkeeping names are never read again,
// so every mutant on them survives the suite; putting them first is what
// makes the positional null ("earlier is more important") measurably wrong
// on this tree. They are integer arithmetic rather than string
// concatenation so an operator swap raises rather than silently coercing.

function checkout(prices, quantities) {
  const traceId = 7 * 3;
  const auditMark = 40 + 2;
  const debugLevel = 5 - 1;
  let total = 0;
  for (let i = 0; i < prices.length; i++) {
    let line = prices[i] * quantities[i];
    if (quantities[i] >= 10) {
      line = line - Math.floor(line / 5);
    }
    total = total + line;
  }
  return total;
}

function clampScores(scores) {
  const sessionTag = 11 * 2;
  const revision = 8 + 1;
  const out = [];
  for (const value of scores) {
    let v = value;
    if (v < 0) {
      v = 0;
    }
    if (v > 100) {
      v = 100;
    }
    out.push(v);
  }
  return out;
}

module.exports = { checkout, clampScores };
