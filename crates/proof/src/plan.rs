//! Step 2 of the loop: which measurements are worth taking? Given how each
//! candidate reading depends on the parameters, greedily pick those that shrink
//! the parameter uncertainty the most (Bayesian D-optimal design). Measuring every
//! point is what a full survey does; this is how a site gets away with a few dozen.

use serde::{Deserialize, Serialize};

use crate::calibrate::Param;
use crate::spd_solve;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasurementPlan {
    /// Chosen candidates, most useful first.
    pub chosen: Vec<usize>,
    /// Prior sigma of each parameter.
    pub sigma_before: Vec<f64>,
    /// Expected sigma of each parameter after taking the chosen readings.
    pub sigma_after: Vec<f64>,
    /// Expected shrink of the total log-uncertainty after each pick (nats).
    pub gain: Vec<f64>,
}

fn log_det_spd(a: &[Vec<f64>]) -> Option<f64> {
    let n = a.len();
    let mut l = vec![vec![0.0; n]; n];
    let mut log = 0.0;
    for i in 0..n {
        for j in 0..=i {
            let s: f64 = (0..j).map(|k| l[i][k] * l[j][k]).sum();
            if i == j {
                let d = a[i][i] - s;
                if !(d > 0.0) {
                    return None;
                }
                l[i][i] = d.sqrt();
                log += d.ln();
            } else {
                l[i][j] = (a[i][j] - s) / l[j][j];
            }
        }
    }
    Some(log)
}

/// `sensitivity[c][p]` is d(reading c)/d(parameter p) and `noise[c]` that
/// reading's standard deviation. Stops at `count` picks, or sooner once another
/// reading would teach almost nothing.
pub fn plan_measurements(params: &[Param], sensitivity: &[Vec<f64>], noise: &[f64], count: usize) -> MeasurementPlan {
    let n = params.len();
    let mut info = vec![vec![0.0; n]; n];
    for (j, p) in params.iter().enumerate() {
        info[j][j] = 1.0 / (p.prior_sigma * p.prior_sigma).max(1e-18);
    }
    let sigma_before: Vec<f64> = params.iter().map(|p| p.prior_sigma).collect();
    let mut chosen = Vec::new();
    let mut gain = Vec::new();
    let mut used = vec![false; sensitivity.len()];
    let mut base = log_det_spd(&info).unwrap_or(0.0);
    for _ in 0..count.min(sensitivity.len()) {
        let mut best: Option<(usize, f64)> = None;
        for c in 0..sensitivity.len() {
            if used[c] || sensitivity[c].len() != n {
                continue;
            }
            let w = 1.0 / noise.get(c).copied().unwrap_or(1.0).max(1e-9).powi(2);
            let mut trial = info.clone();
            for i in 0..n {
                for j in 0..n {
                    trial[i][j] += w * sensitivity[c][i] * sensitivity[c][j];
                }
            }
            if let Some(ld) = log_det_spd(&trial) {
                let g = 0.5 * (ld - base);
                if best.is_none_or(|b| g > b.1) {
                    best = Some((c, g));
                }
            }
        }
        let Some((c, g)) = best else { break };
        if g < 1e-3 {
            break;
        }
        used[c] = true;
        chosen.push(c);
        gain.push(g);
        let w = 1.0 / noise.get(c).copied().unwrap_or(1.0).max(1e-9).powi(2);
        for i in 0..n {
            for j in 0..n {
                info[i][j] += w * sensitivity[c][i] * sensitivity[c][j];
            }
        }
        base += 2.0 * g;
    }
    let sigma_after = spd_solve(&info, &vec![0.0; n]).map_or_else(|| sigma_before.clone(), |(_, inv)| (0..n).map(|j| inv[j][j].max(0.0).sqrt()).collect());
    MeasurementPlan { chosen, sigma_before, sigma_after, gain }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Vec<Param> {
        vec![Param::new("a", 1.0, 1.0, 0.0, 5.0), Param::new("b", 1.0, 1.0, 0.0, 5.0)]
    }

    #[test]
    fn picks_readings_that_see_each_parameter() {
        // Candidates: only a, only a again (noisier), only b, neither.
        let s = vec![vec![1.0, 0.0], vec![1.0, 0.0], vec![0.0, 1.0], vec![0.0, 0.0]];
        let p = plan_measurements(&two(), &s, &[0.1, 0.5, 0.1, 0.1], 2);
        assert_eq!(p.chosen.len(), 2);
        assert!(p.chosen.contains(&0) && p.chosen.contains(&2), "{:?}", p.chosen);
        assert!(p.sigma_after[0] < 0.11 && p.sigma_after[1] < 0.11, "{:?}", p.sigma_after);
    }

    #[test]
    fn stops_when_readings_teach_nothing() {
        let s = vec![vec![0.0, 0.0], vec![0.0, 0.0]];
        let p = plan_measurements(&two(), &s, &[0.1, 0.1], 2);
        assert!(p.chosen.is_empty());
        assert_eq!(p.sigma_after, p.sigma_before);
    }

    #[test]
    fn expected_sigma_matches_the_conjugate_result() {
        let params = vec![Param::new("a", 0.0, 2.0, -9.0, 9.0)];
        let p = plan_measurements(&params, &[vec![1.0]], &[1.0], 1);
        // 1/σ² = 1/4 + 1/1 → σ = sqrt(0.8).
        assert!((p.sigma_after[0] - 0.8f64.sqrt()).abs() < 1e-9);
    }

    #[test]
    fn gains_shrink_with_each_pick() {
        // Repeating the same kind of reading is worth less each time.
        let s = vec![vec![1.0]; 6];
        let params = vec![Param::new("a", 0.0, 1.0, -9.0, 9.0)];
        let p = plan_measurements(&params, &s, &[0.5; 6], 6);
        for w in p.gain.windows(2) {
            assert!(w[1] <= w[0] + 1e-9, "{:?}", p.gain);
        }
    }
}
