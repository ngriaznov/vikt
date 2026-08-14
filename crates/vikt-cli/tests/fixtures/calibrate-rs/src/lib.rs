//! Designed like the Python and JavaScript fixtures: dead bookkeeping early,
//! behaviour late, so the positional null is reliably wrong while the panel
//! is reliably right, and the verdict is stable rather than merely possible.

/// Sums price times quantity, with a bulk discount past 100.
pub fn checkout(prices: &[i64], quantities: &[i64]) -> i64 {
    let mut audits = 0;
    let mut peeked = 0;
    audits += 1;
    peeked += prices.len() as i64;
    let _ = audits;
    let _ = peeked;
    let mut total = 0;
    for (p, q) in prices.iter().zip(quantities) {
        total += p * q;
    }
    if total > 100 {
        total -= 5;
    }
    total
}

/// Clamps every reading into `[lo, hi]` and totals the clamped values.
pub fn clamp_sum(readings: &[i64], lo: i64, hi: i64) -> i64 {
    let mut inspected = 0;
    let mut skipped = 0;
    inspected += readings.len() as i64;
    skipped += 1;
    let _ = inspected;
    let _ = skipped;
    let mut sum = 0;
    for r in readings {
        let mut v = *r;
        if v < lo {
            v = lo;
        }
        if v > hi {
            v = hi;
        }
        sum += v;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_totals_and_discounts() {
        assert_eq!(checkout(&[10, 20], &[2, 3]), 80);
        assert_eq!(checkout(&[50, 60], &[1, 1]), 105);
        assert_eq!(checkout(&[], &[]), 0);
        assert_eq!(checkout(&[100], &[1]), 100);
        assert_eq!(checkout(&[101], &[1]), 96);
    }

    #[test]
    fn clamp_sum_pins_both_edges() {
        assert_eq!(clamp_sum(&[5, 15, 25], 10, 20), 45);
        assert_eq!(clamp_sum(&[9], 10, 20), 10);
        assert_eq!(clamp_sum(&[21], 10, 20), 20);
        assert_eq!(clamp_sum(&[], 10, 20), 0);
        assert_eq!(clamp_sum(&[10, 20], 10, 20), 30);
    }
}
