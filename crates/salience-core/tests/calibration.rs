//! Pins the calibration statistic and verdict rule. The Spearman values are
//! checked against hand-computed exact results, including the tie-corrected
//! case the shortcut formula gets wrong.

use salience_core::calibration::{
    MIN_EXECUTED_MUTANTS, MIN_SCORED_LINES, Verdict, spearman, verdict,
};

/// A strictly monotone relation is perfect rank agreement regardless of the
/// magnitudes involved.
#[test]
fn spearman_monotone_is_one() {
    let xs = [1.0, 2.0, 3.0, 4.0];
    let ys = [0.1, 0.2, 0.9, 100.0];
    assert!((spearman(&xs, &ys) - 1.0).abs() < 1e-12);
    let rev: Vec<f64> = ys.iter().rev().copied().collect();
    assert!((spearman(&xs, &rev) + 1.0).abs() < 1e-12);
}

/// Without ties the tie-corrected form reduces to the classic `1 - 6Σd²/…`
/// value: for ranks (1,2,3,4,5) against (2,1,4,3,5), Σd² = 4 and rho = 0.8.
#[test]
fn spearman_matches_the_untied_shortcut() {
    let xs = [1.0, 2.0, 3.0, 4.0, 5.0];
    let ys = [2.0, 1.0, 4.0, 3.0, 5.0];
    assert!((spearman(&xs, &ys) - 0.8).abs() < 1e-12);
}

/// The tied case, computed by hand: xs (1, 2, 2, 3) ranks to (1, 2.5, 2.5, 4)
/// and against ys (1, 2, 3, 4) the Pearson correlation of the rank vectors is
/// 4.5 / sqrt(4.5 * 5) = 3 / sqrt(10). The shortcut formula would report
/// 0.95 instead; kill rates tie constantly, so this is the case that matters.
#[test]
fn spearman_averages_tied_ranks() {
    let xs = [1.0, 2.0, 2.0, 3.0];
    let ys = [1.0, 2.0, 3.0, 4.0];
    let expected = 3.0 / 10.0_f64.sqrt();
    assert!((spearman(&xs, &ys) - expected).abs() < 1e-12);
}

/// A constant side carries no ranking information; the conservative report is
/// zero agreement, not NaN.
#[test]
fn spearman_degenerate_inputs_are_zero() {
    assert_eq!(spearman(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]), 0.0);
    assert_eq!(spearman(&[0.5], &[0.5]), 0.0);
    assert_eq!(spearman(&[], &[]), 0.0);
}

/// The verdict rule, branch by branch: floors first, then the two-threshold
/// calibrated test, then beat-the-null-weakly, then everything else.
#[test]
fn verdict_covers_every_branch() {
    let (lines, mutants) = (MIN_SCORED_LINES, MIN_EXECUTED_MUTANTS);
    // Floors dominate: a perfect rho on too little data is no verdict.
    assert_eq!(
        verdict(0.9, 0.0, lines - 1, mutants),
        Verdict::InsufficientData
    );
    assert_eq!(
        verdict(0.9, 0.0, lines, mutants - 1),
        Verdict::InsufficientData
    );
    // Both thresholds met.
    assert_eq!(verdict(0.5, 0.1, lines, mutants), Verdict::Calibrated);
    // Beats the null but sits under the absolute floor.
    assert_eq!(verdict(0.25, 0.0, lines, mutants), Verdict::Marginal);
    // Beats the null but by less than the margin.
    assert_eq!(verdict(0.6, 0.55, lines, mutants), Verdict::Marginal);
    // At or below the null.
    assert_eq!(verdict(0.2, 0.2, lines, mutants), Verdict::Uncalibrated);
    assert_eq!(verdict(-0.1, 0.4, lines, mutants), Verdict::Uncalibrated);
}
