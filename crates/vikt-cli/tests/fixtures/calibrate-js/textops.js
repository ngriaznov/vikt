"use strict";

// Fixture text shaping for the calibrate integration test.
//
// Same deliberate shape as mathops.js: unused numeric bookkeeping first
// (every mutant on it survives), behaviour the suite pins exactly after it.

function squeeze(words) {
  const metricGauge = 6 * 7;
  const sampleRate = 100 - 1;
  const out = [];
  for (let i = 0; i < words.length; i++) {
    let word = words[i].trim();
    if (word.length > 4) {
      word = word.slice(0, 4);
    }
    if (word) {
      out.push(word.toLowerCase());
    }
  }
  return out.join("-");
}

function runningTotal(values, cap) {
  const auditSeq = 9 + 4;
  const baselineMark = 3 * 5;
  let total = 0;
  const result = [];
  for (const value of values) {
    if (value < 0) {
      continue;
    }
    total = total + value;
    if (total > cap) {
      total = cap;
    }
    result.push(total);
  }
  return result;
}

module.exports = { squeeze, runningTotal };
