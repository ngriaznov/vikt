//! The statistic and the decision rule behind `vikt calibrate`.
//!
//! Calibration asks one question of a repository: does the panel's ordering of
//! lines agree with the lines' *measured* behavioural impact, where impact is
//! the fraction of line-targeted mutants the repository's own test suite
//! kills? The comparison is rank-based — Spearman rather than Pearson —
//! because the panel score is a ranking signal by contract: the artifact
//! rounds it to two decimals precisely so nobody reads magnitudes into it.
//!
//! The subprocess machinery (copying the tree, generating mutants, running
//! the suite) lives in the CLI. The statistic and the verdict rule live here
//! so their behaviour can be pinned by tests without a Python toolchain in
//! the loop: the CLI crate has no library target to test against.
//!
//! The verdict is judged against a positional null — score a line by how
//! early it sits in its function and nothing else — computed on the same
//! lines. That baseline is the honesty rule carried over from the scorer
//! bakeoffs: single instruments have been measured at or below it, so a
//! panel that cannot beat it on this repository is not calibrated here,
//! whatever it does elsewhere.

/// Below this many mutated-and-scored lines, no rank correlation is worth
/// reporting: with fewer pairs the rho swings by whole tenths when one line
/// changes side.
pub const MIN_SCORED_LINES: usize = 20;

/// Below this many executed mutants, per-line kill rates are too coarse to
/// rank against — most lines carry one or two mutants and the "rate" is a
/// coin flip.
pub const MIN_EXECUTED_MUTANTS: usize = 30;

/// How far the panel must sit above the positional null to count as
/// calibrated. A margin under a tenth is within the run-to-run movement
/// observed when a single function enters or leaves the sample.
pub const NULL_MARGIN: f64 = 0.1;

/// Absolute floor for the panel's rho. Beating a null that happens to be
/// strongly negative is not agreement with the test suite.
pub const RHO_FLOOR: f64 = 0.3;

/// The calibration verdict for one repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The panel beats the positional null by at least [`NULL_MARGIN`] and
    /// clears [`RHO_FLOOR`] on its own.
    Calibrated,
    /// The panel beats the null, but not by enough to clear both thresholds.
    Marginal,
    /// The panel does not beat the null on this repository.
    Uncalibrated,
    /// Fewer lines or mutants than the floors above: no verdict offered.
    InsufficientData,
}

impl Verdict {
    /// The verdict as it appears in the report.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Calibrated => "calibrated",
            Self::Marginal => "marginal",
            Self::Uncalibrated => "uncalibrated",
            Self::InsufficientData => "insufficient data",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Judges panel rho against the positional null computed on the same lines.
#[must_use]
pub fn verdict(
    panel_rho: f64,
    null_rho: f64,
    scored_lines: usize,
    executed_mutants: usize,
) -> Verdict {
    if scored_lines < MIN_SCORED_LINES || executed_mutants < MIN_EXECUTED_MUTANTS {
        return Verdict::InsufficientData;
    }
    if panel_rho - null_rho >= NULL_MARGIN && panel_rho >= RHO_FLOOR {
        return Verdict::Calibrated;
    }
    if panel_rho > null_rho {
        Verdict::Marginal
    } else {
        Verdict::Uncalibrated
    }
}

/// Spearman rank correlation with average ranks for ties.
///
/// Ties get the mean of the positions they span, and the correlation is the
/// Pearson correlation of the resulting rank vectors — the exact tie-corrected
/// form, not the `1 - 6Σd²/n(n²-1)` shortcut, which is only valid when
/// nothing ties. Kill rates tie constantly (most lines are all-killed or
/// all-survived), so the shortcut would be wrong on precisely the data this
/// exists for.
///
/// Returns `0.0` when either side has no variance or fewer than two pairs:
/// a constant vector ranks nothing, and reporting "no agreement" is the
/// conservative reading. Matches the convention of the eval scripts this
/// mirrors.
///
/// # Panics
///
/// Panics when the slices differ in length: the pairs are constructed
/// together, so a mismatch is a caller bug, not data.
#[must_use]
pub fn spearman(xs: &[f64], ys: &[f64]) -> f64 {
    assert_eq!(xs.len(), ys.len(), "spearman needs paired observations");
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let rx = average_ranks(xs);
    let ry = average_ranks(ys);
    let m = f64::midpoint(n as f64, 1.0); // mean of ranks 1..=n, ties included
    let mut num = 0.0;
    let mut dx = 0.0;
    let mut dy = 0.0;
    for (a, b) in rx.iter().zip(&ry) {
        num += (a - m) * (b - m);
        dx += (a - m) * (a - m);
        dy += (b - m) * (b - m);
    }
    if dx == 0.0 || dy == 0.0 {
        return 0.0;
    }
    num / (dx.sqrt() * dy.sqrt())
}

/// 1-based ranks, with tied values sharing the mean of their positions.
fn average_ranks(v: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..v.len()).collect();
    order.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut ranks = vec![0.0; v.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        // Exact equality is the right tie test here: tied kill rates and tied
        // panel scores are bit-identical by construction, not approximately
        // close, and a tolerance would invent ties between distinct scores.
        #[allow(clippy::float_cmp)]
        while j + 1 < order.len() && v[order[j + 1]] == v[order[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for &k in &order[i..=j] {
            ranks[k] = avg;
        }
        i = j + 1;
    }
    ranks
}
