//! How far a count of passes can be trusted. 16 passes out of 16 is not "100 %": it
//! shows at least 83 % at 95 % confidence, and the range of a handful of runs is
//! narrower than it looks. Everything here is exact, with no large-sample shortcuts.

/// `P(X ≥ k)` for `X ~ Binomial(n, p)`, in logs so large `n` stays accurate.
pub fn binomial_tail_at_least(n: usize, k: usize, p: f64) -> f64 {
    let (lp, lq) = (p.max(1e-300).ln(), (1.0 - p).max(1e-300).ln());
    let mut log_c = 0.0f64;
    let mut total = 0.0f64;
    for i in 0..=n {
        if i > 0 {
            log_c += ((n - i + 1) as f64).ln() - (i as f64).ln();
        }
        if i >= k {
            total += (log_c + i as f64 * lp + (n - i) as f64 * lq).exp();
        }
    }
    total.min(1.0)
}

/// With `passed` of `n`, the share we can say passes at this confidence: the one-sided
/// Clopper–Pearson lower bound. All `n` passing gives `(1 − confidence)^(1/n)`.
pub fn shown_share(n: usize, passed: usize, confidence: f64) -> f64 {
    if n == 0 || passed == 0 {
        return 0.0;
    }
    let alpha = 1.0 - confidence;
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if binomial_tail_at_least(n, passed.min(n), mid) < alpha {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The same from above: the share we can say passes at most.
pub fn upper_bound(n: usize, passed: usize, confidence: f64) -> f64 {
    if n == 0 {
        return 1.0;
    }
    1.0 - shown_share(n, n - passed.min(n), confidence)
}

/// A two-sided Clopper–Pearson interval at this confidence, for showing a rate.
pub fn interval(n: usize, passed: usize, confidence: f64) -> (f64, f64) {
    let one_side = (1.0 + confidence) / 2.0;
    (shown_share(n, passed, one_side), upper_bound(n, passed, one_side))
}

/// The Wilson score interval with z = 1.645: each end is a 95 % one-sided bound, so
/// the pair is a 90 % interval. Cheaper than Clopper–Pearson and a little less cautious.
pub fn wilson(hits: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let (z, n) = (1.645, n as f64);
    let p = hits as f64 / n;
    let d = 1.0 + z * z / n;
    let c = (p + z * z / (2.0 * n)) / d;
    let h = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
    ((c - h).max(0.0), (c + h).min(1.0))
}

/// Points needed, all passing, to show `share` passes with this confidence.
pub fn points_needed(share: f64, confidence: f64) -> usize {
    ((1.0 - confidence).ln() / share.ln()).ceil() as usize
}

/// Runs below this can't hold 90 % of what comes next, even from lowest to highest:
/// the expected share between the extremes of `n` runs is `(n − 1)/(n + 1)`.
pub const RUNS_FOR_A_90_RANGE: usize = 19;

/// Share of further runs from the same inputs expected inside the range made from `n`.
pub fn expected_range_content(n: usize) -> f64 {
    if n < 2 {
        return 0.0;
    }
    ((n as f64 - 1.0) / (n as f64 + 1.0)).min(0.9)
}

/// Value at quantile `q` of a sorted slice with the plotting position `q (n + 1)`. Unlike
/// the usual interpolation, the expected share between its 5 % and 95 % points really is
/// 90 %, for any `n`. Below [`RUNS_FOR_A_90_RANGE`] those points fall off the ends, which
/// leaves the lowest and highest values.
pub fn range_point(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    match n {
        0 => f64::NAN,
        1 => sorted[0],
        _ => {
            let x = (q.clamp(0.0, 1.0) * (n as f64 + 1.0) - 1.0).clamp(0.0, (n - 1) as f64);
            let i = x.floor() as usize;
            let j = (i + 1).min(n - 1);
            sorted[i] + (sorted[j] - sorted[i]) * (x - i as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_point_counts_are_the_known_ones() {
        assert_eq!(points_needed(0.95, 0.95), 59);
        assert_eq!(points_needed(0.95, 0.90), 45);
        assert_eq!(points_needed(0.90, 0.95), 29);
    }

    #[test]
    fn bounds_match_the_success_run_formula_and_known_values() {
        for n in [4, 16, 29, 59] {
            let want = 0.05f64.powf(1.0 / n as f64);
            assert!((shown_share(n, n, 0.95) - want).abs() < 1e-6, "{n}");
        }
        // The audit's 40 %, 79 % and 70 % are two-sided figures (97.5 % on this side).
        assert!((interval(4, 4, 0.95).0 - 0.3976).abs() < 1e-3);
        assert!((interval(16, 16, 0.95).0 - 0.7941).abs() < 1e-3);
        assert!((interval(16, 15, 0.95).0 - 0.698).abs() < 1e-3);
        // A textbook value: 9 of 10 at 95 %, one-sided, is about 0.606; two-sided, 0.555 to 0.9975.
        assert!((shown_share(10, 9, 0.95) - 0.606).abs() < 0.002);
        let (l, h) = interval(10, 9, 0.95);
        assert!((l - 0.555).abs() < 0.002 && (h - 0.9975).abs() < 0.002, "{l} {h}");
        assert_eq!(shown_share(10, 0, 0.95), 0.0);
        assert_eq!(upper_bound(10, 10, 0.95), 1.0);
        assert!(shown_share(29, 29, 0.95) >= 0.9 && shown_share(28, 28, 0.95) < 0.9, "Likely takes 29 of 29");
    }

    #[test]
    fn wilson_behaves_at_the_edges() {
        let (l, h) = wilson(95, 100);
        assert!(l < 0.95 && h > 0.95 && l > 0.88 && h < 0.99, "{l} {h}");
        assert_eq!(wilson(0, 0), (0.0, 1.0));
        let (l, h) = wilson(100, 100);
        assert!(l > 0.96 && h == 1.0, "{l} {h}");
        // It sits inside the exact interval at the same confidence.
        assert!(wilson(16, 16).0 >= interval(16, 16, 0.90).0);
    }

    #[test]
    fn the_range_point_gives_extremes_at_small_n_and_a_quantile_at_large_n() {
        let v: Vec<f64> = (1..=4).map(f64::from).collect();
        assert_eq!((range_point(&v, 0.05), range_point(&v, 0.95)), (1.0, 4.0));
        let v: Vec<f64> = (1..=19).map(f64::from).collect();
        assert_eq!((range_point(&v, 0.05), range_point(&v, 0.95)), (1.0, 19.0));
        let v: Vec<f64> = (1..=199).map(f64::from).collect();
        assert_eq!((range_point(&v, 0.05), range_point(&v, 0.95)), (10.0, 190.0));
        assert!(range_point(&[], 0.5).is_nan());
        assert_eq!(range_point(&[3.0], 0.05), 3.0);
    }
}
