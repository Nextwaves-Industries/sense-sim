//! The loop every outcome recipe in Sense Sim runs on:
//!
//! 1. **Predict with a range.** [`robust`] pushes uncertain [`Factor`]s through a
//!    model and returns a [`Prediction`] (median, and where about 90 % of runs fall, or
//!    the lowest and highest below 19 runs), how many runs met a target, and which factor
//!    matters most. The range spreads the assumed inputs, not model error.
//! 2. **Measure.** [`plan_measurements`] picks the few measurements that would
//!    narrow the answer most.
//! 3. **Calibrate.** [`calibrate`] fits the model's parameters to the readings,
//!    with priors, and keeps the remaining uncertainty.
//! 4. **Decide.** A [`Claim`] records what is predicted, on what [`Evidence`], and
//!    how well earlier predictions held up. A number never loses track of where
//!    it came from.
//!
//! The crate knows nothing about antennas or sites. A recipe supplies the physics
//! as a closure, so the same loop serves all of them.

pub mod calibrate;
pub mod claim;
pub mod plan;
pub mod robust;
pub mod rng;
pub mod stats;

pub use calibrate::{Calibration, Param, Problem, calibrate};
pub use claim::{Claim, Comparison, Evidence, Ledger, NOT_VALIDATED, Prediction, Removed, Verdict};
pub use plan::{MeasurementPlan, plan_measurements};
pub use rng::Rng;
pub use robust::{Direction, Dist, Factor, RobustOptions, RobustReport, Runs, Swing, Target, robust, robust_mut};

/// Value at quantile `q` (0..1) of an already sorted slice, interpolated.
pub fn quantile(sorted: &[f64], q: f64) -> f64 {
    match sorted.len() {
        0 => f64::NAN,
        1 => sorted[0],
        n => {
            let x = q.clamp(0.0, 1.0) * (n - 1) as f64;
            let i = x.floor() as usize;
            let j = (i + 1).min(n - 1);
            sorted[i] + (sorted[j] - sorted[i]) * (x - i as f64)
        }
    }
}

/// Solves the symmetric positive definite `a x = b` by Cholesky, returning the
/// solution and `a⁻¹`. `None` if `a` isn't positive definite.
pub(crate) fn spd_solve(a: &[Vec<f64>], b: &[f64]) -> Option<(Vec<f64>, Vec<Vec<f64>>)> {
    let n = b.len();
    let mut l = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..=i {
            let s: f64 = (0..j).map(|k| l[i][k] * l[j][k]).sum();
            if i == j {
                let d = a[i][i] - s;
                if !(d > 0.0) || !d.is_finite() {
                    return None;
                }
                l[i][i] = d.sqrt();
            } else {
                l[i][j] = (a[i][j] - s) / l[j][j];
            }
        }
    }
    let solve = |rhs: &[f64]| -> Vec<f64> {
        let mut y = vec![0.0; n];
        for i in 0..n {
            y[i] = (rhs[i] - (0..i).map(|k| l[i][k] * y[k]).sum::<f64>()) / l[i][i];
        }
        let mut x = vec![0.0; n];
        for i in (0..n).rev() {
            x[i] = (y[i] - (i + 1..n).map(|k| l[k][i] * x[k]).sum::<f64>()) / l[i][i];
        }
        x
    };
    let x = solve(b);
    let mut inv = vec![vec![0.0; n]; n];
    for c in 0..n {
        let mut e = vec![0.0; n];
        e[c] = 1.0;
        let col = solve(&e);
        for r in 0..n {
            inv[r][c] = col[r];
        }
    }
    Some((x, inv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_interpolates_and_handles_edges() {
        assert!(quantile(&[], 0.5).is_nan());
        assert_eq!(quantile(&[3.0], 0.9), 3.0);
        assert_eq!(quantile(&[0.0, 10.0], 0.5), 5.0);
        assert_eq!(quantile(&[0.0, 10.0, 20.0], 1.0), 20.0);
        assert_eq!(quantile(&[0.0, 10.0, 20.0], 2.0), 20.0);
    }

    #[test]
    fn spd_solve_inverts_and_rejects_indefinite() {
        let (x, inv) = spd_solve(&[vec![4.0, 1.0], vec![1.0, 3.0]], &[1.0, 2.0]).unwrap();
        assert!((4.0 * x[0] + x[1] - 1.0).abs() < 1e-12 && (x[0] + 3.0 * x[1] - 2.0).abs() < 1e-12);
        assert!((4.0 * inv[0][0] + inv[1][0] - 1.0).abs() < 1e-12);
        assert!(spd_solve(&[vec![1.0, 2.0], vec![2.0, 1.0]], &[1.0, 1.0]).is_none());
    }
}
