//! Step 3 of the loop: fit a model's parameters to measurements. Priors keep a
//! handful of readings from swinging a parameter wildly, and the result carries
//! its uncertainty, because "calibrated" doesn't mean "known".
//!
//! Levenberg–Marquardt on the maximum a posteriori objective, with the covariance
//! from the Laplace approximation around the optimum. Readings that disagree badly
//! with everything else are down-weighted (Huber), because a field walk always
//! has a few.

use serde::{Deserialize, Serialize};

use crate::spd_solve;

/// A parameter to fit, with what's believed before any measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub prior_mean: f64,
    /// Prior standard deviation; larger means the data decide.
    pub prior_sigma: f64,
    pub min: f64,
    pub max: f64,
}

impl Param {
    pub fn new(name: &str, prior_mean: f64, prior_sigma: f64, min: f64, max: f64) -> Self {
        Self { name: name.into(), prior_mean, prior_sigma, min, max }
    }
}

pub struct Problem<'a> {
    pub params: &'a [Param],
    /// One measured value per reading.
    pub measured: &'a [f64],
    /// Standard deviation of each reading's noise.
    pub sigma: &'a [f64],
    /// Huber threshold in sigmas; `None` trusts every reading.
    pub robust_k: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FittedParam {
    pub name: String,
    pub prior: f64,
    pub value: f64,
    pub sigma: f64,
    /// How much the data taught us: 1 − posterior variance / prior variance.
    /// Near 0 means this parameter is still mostly the prior.
    pub learned: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub params: Vec<FittedParam>,
    /// Posterior covariance of the parameters.
    pub covariance: Vec<Vec<f64>>,
    pub rms_before: f64,
    pub rms_after: f64,
    pub readings: usize,
    /// Readings the fit treated as outliers (beyond the Huber threshold).
    pub outliers: Vec<usize>,
    pub iterations: usize,
    pub converged: bool,
}

impl Calibration {
    pub fn values(&self) -> Vec<f64> {
        self.params.iter().map(|p| p.value).collect()
    }
}

/// Weighted RMS of (measured − predicted), in the measurement's own units.
fn rms(measured: &[f64], predicted: &[f64]) -> f64 {
    if measured.is_empty() {
        return 0.0;
    }
    (measured.iter().zip(predicted).map(|(m, p)| (m - p).powi(2)).sum::<f64>() / measured.len() as f64).sqrt()
}

/// Fits `predict(params) → one value per reading`. Derivatives are by finite
/// differences, so any model works. Parameters stay within their limits.
pub fn calibrate(problem: &Problem, predict: impl Fn(&[f64]) -> Vec<f64>) -> Result<Calibration, String> {
    let n = problem.params.len();
    let m = problem.measured.len();
    if n == 0 {
        return Err("nothing to fit".into());
    }
    if problem.sigma.len() != m {
        return Err(format!("{m} readings but {} noise values", problem.sigma.len()));
    }
    if let Some(p) = problem.params.iter().find(|p| !(p.prior_sigma > 0.0) || !(p.min < p.max)) {
        return Err(format!("parameter '{}' needs a positive prior sigma and min < max", p.name));
    }
    if let Some(i) = problem.measured.iter().position(|v| !v.is_finite()) {
        return Err(format!("reading {} isn't a number", i + 1));
    }
    let sigma: Vec<f64> = problem.sigma.iter().map(|s| s.max(1e-9)).collect();
    let clamp = |x: &mut [f64]| {
        for (v, p) in x.iter_mut().zip(problem.params) {
            *v = v.clamp(p.min, p.max);
        }
    };
    let mut x: Vec<f64> = problem.params.iter().map(|p| p.prior_mean).collect();
    clamp(&mut x);
    let first = predict(&x);
    if first.len() != m {
        return Err(format!("the model returned {} values for {m} readings", first.len()));
    }
    if let Some(i) = first.iter().position(|v| !v.is_finite()) {
        return Err(format!("the model can't predict reading {}", i + 1));
    }
    let rms_before = rms(problem.measured, &first);

    // Robust weight of a reading given its residual in sigmas.
    let weight = |z: f64| match problem.robust_k {
        Some(k) if z.abs() > k => k / z.abs(),
        _ => 1.0,
    };
    let cost = |x: &[f64], pred: &[f64]| -> f64 {
        let data: f64 = (0..m)
            .map(|i| {
                let z = (problem.measured[i] - pred[i]) / sigma[i];
                match problem.robust_k {
                    Some(k) if z.abs() > k => k * (2.0 * z.abs() - k),
                    _ => z * z,
                }
            })
            .sum();
        let prior: f64 = problem.params.iter().zip(x).map(|(p, v)| ((v - p.prior_mean) / p.prior_sigma).powi(2)).sum();
        data + prior
    };

    let jacobian = |x: &[f64], base: &[f64]| -> Vec<Vec<f64>> {
        let mut jac = vec![vec![0.0; n]; m];
        for j in 0..n {
            let span = problem.params[j].max - problem.params[j].min;
            let mut h = (x[j].abs() * 1e-4).max(span * 1e-5).max(1e-9);
            // Step inwards at an upper limit.
            if x[j] + h > problem.params[j].max {
                h = -h;
            }
            let mut xp = x.to_vec();
            xp[j] += h;
            let up = predict(&xp);
            for i in 0..m {
                jac[i][j] = (up[i] - base[i]) / h;
            }
        }
        jac
    };
    let normal = |jac: &[Vec<f64>], pred: &[f64], x: &[f64]| -> (Vec<Vec<f64>>, Vec<f64>) {
        let mut a = vec![vec![0.0; n]; n];
        let mut g = vec![0.0; n];
        for i in 0..m {
            let r = problem.measured[i] - pred[i];
            let w = weight(r / sigma[i]) / (sigma[i] * sigma[i]);
            for j in 0..n {
                g[j] += w * jac[i][j] * r;
                for l in 0..n {
                    a[j][l] += w * jac[i][j] * jac[i][l];
                }
            }
        }
        for (j, p) in problem.params.iter().enumerate() {
            let w = 1.0 / (p.prior_sigma * p.prior_sigma);
            a[j][j] += w;
            g[j] += w * (p.prior_mean - x[j]);
        }
        (a, g)
    };

    let mut pred = first;
    let mut best = cost(&x, &pred);
    let mut lambda = 1e-3;
    let mut converged = false;
    let mut iterations = 0;
    for it in 0..100 {
        iterations = it + 1;
        let jac = jacobian(&x, &pred);
        let (a, g) = normal(&jac, &pred, &x);
        let mut stepped = false;
        for _ in 0..12 {
            let mut damped = a.clone();
            for j in 0..n {
                damped[j][j] *= 1.0 + lambda;
            }
            let Some((dx, _)) = spd_solve(&damped, &g) else {
                lambda *= 10.0;
                continue;
            };
            let mut nx: Vec<f64> = x.iter().zip(&dx).map(|(a, b)| a + b).collect();
            clamp(&mut nx);
            let np = predict(&nx);
            let c = cost(&nx, &np);
            if c < best {
                let improvement = (best - c) / best.max(1e-12);
                x = nx;
                pred = np;
                best = c;
                lambda = (lambda * 0.3).max(1e-9);
                stepped = true;
                if improvement < 1e-7 {
                    converged = true;
                }
                break;
            }
            lambda *= 4.0;
        }
        if !stepped {
            // Nothing lowered the cost. That's the optimum only if the slope agrees,
            // allowing for a parameter held against its limit.
            converged = (0..n).all(|j| {
                let at_limit = (x[j] <= problem.params[j].min && g[j] < 0.0) || (x[j] >= problem.params[j].max && g[j] > 0.0);
                at_limit || g[j].abs() / a[j][j].max(1e-300).sqrt() < 1e-3
            });
            break;
        }
        if converged {
            break;
        }
    }

    let jac = jacobian(&x, &pred);
    let (a, _) = normal(&jac, &pred, &x);
    let covariance = spd_solve(&a, &vec![0.0; n]).map_or_else(
        // Not identifiable at all: fall back to the prior.
        || {
            let mut c = vec![vec![0.0; n]; n];
            for (j, p) in problem.params.iter().enumerate() {
                c[j][j] = p.prior_sigma * p.prior_sigma;
            }
            c
        },
        |(_, inv)| inv,
    );
    let params = problem
        .params
        .iter()
        .enumerate()
        .map(|(j, p)| {
            let var = covariance[j][j].max(0.0);
            FittedParam { name: p.name.clone(), prior: p.prior_mean, value: x[j], sigma: var.sqrt(), learned: (1.0 - var / (p.prior_sigma * p.prior_sigma)).clamp(0.0, 1.0) }
        })
        .collect();
    let outliers = match problem.robust_k {
        Some(k) => (0..m).filter(|&i| ((problem.measured[i] - pred[i]) / sigma[i]).abs() > k).collect(),
        None => Vec::new(),
    };
    Ok(Calibration { params, covariance, rms_before, rms_after: rms(problem.measured, &pred), readings: m, outliers, iterations, converged })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn line(x: &[f64], at: &[f64]) -> Vec<f64> {
        at.iter().map(|t| x[0] + x[1] * t).collect()
    }

    #[test]
    fn recovers_a_line_through_noise_with_honest_errors() {
        let mut rng = Rng::new(5);
        let at: Vec<f64> = (0..200).map(|i| i as f64 / 20.0).collect();
        let truth = [2.0, -0.5];
        let measured: Vec<f64> = line(&truth, &at).iter().map(|v| v + 0.3 * rng.gauss()).collect();
        let params = [Param::new("a", 0.0, 100.0, -50.0, 50.0), Param::new("b", 0.0, 100.0, -50.0, 50.0)];
        let sigma = vec![0.3; at.len()];
        let c = calibrate(&Problem { params: &params, measured: &measured, sigma: &sigma, robust_k: None }, |x| line(x, &at)).unwrap();
        assert!(c.converged);
        for (p, t) in c.params.iter().zip(truth) {
            assert!((p.value - t).abs() < 3.5 * p.sigma, "{p:?} truth {t}");
            assert!(p.learned > 0.99);
        }
        // The textbook slope error: σ / sqrt(Σ(t − mean)²).
        let mean = at.iter().sum::<f64>() / at.len() as f64;
        let want = 0.3 / at.iter().map(|t| (t - mean).powi(2)).sum::<f64>().sqrt();
        assert!((c.params[1].sigma - want).abs() < 0.05 * want, "{} vs {want}", c.params[1].sigma);
        assert!(c.rms_after < c.rms_before);
    }

    #[test]
    fn without_data_the_prior_stands() {
        // The reading doesn't depend on b, so b stays at its prior with its full uncertainty.
        let params = [Param::new("a", 1.0, 1.0, -10.0, 10.0), Param::new("b", 3.0, 0.5, -10.0, 10.0)];
        let c = calibrate(&Problem { params: &params, measured: &[5.0, 5.0, 5.0], sigma: &[0.1, 0.1, 0.1], robust_k: None }, |x| vec![x[0]; 3]).unwrap();
        assert!((c.params[0].value - 5.0).abs() < 0.05);
        assert!((c.params[1].value - 3.0).abs() < 1e-6);
        assert!((c.params[1].sigma - 0.5).abs() < 1e-6);
        assert!(c.params[1].learned < 1e-6);
    }

    #[test]
    fn a_strong_prior_resists_one_noisy_reading() {
        let params = [Param::new("a", 0.0, 0.1, -10.0, 10.0)];
        let c = calibrate(&Problem { params: &params, measured: &[5.0], sigma: &[1.0], robust_k: None }, |x| vec![x[0]]).unwrap();
        // Conjugate Gaussian: posterior mean = 5 · (1/1) / (1/1 + 1/0.01).
        assert!((c.params[0].value - 5.0 / 101.0).abs() < 1e-4, "{:?}", c.params[0]);
    }

    #[test]
    fn outliers_do_not_drag_the_fit() {
        let mut measured = vec![1.0; 40];
        measured[3] = 50.0;
        measured[17] = -60.0;
        let sigma = vec![0.1; 40];
        let params = [Param::new("a", 0.0, 100.0, -100.0, 100.0)];
        let plain = calibrate(&Problem { params: &params, measured: &measured, sigma: &sigma, robust_k: None }, |x| vec![x[0]; 40]).unwrap();
        let robust = calibrate(&Problem { params: &params, measured: &measured, sigma: &sigma, robust_k: Some(3.0) }, |x| vec![x[0]; 40]).unwrap();
        assert!((plain.params[0].value - 1.0).abs() > 0.1, "{:?}", plain.params[0]);
        assert!((robust.params[0].value - 1.0).abs() < 0.05, "{:?}", robust.params[0]);
        assert_eq!(robust.outliers, vec![3, 17]);
    }

    #[test]
    fn nonlinear_models_and_limits_work() {
        // Exponential decay, then a limit the data would like to cross.
        let at: Vec<f64> = (0..50).map(|i| i as f64 * 0.1).collect();
        let f = |x: &[f64]| -> Vec<f64> { at.iter().map(|t| x[0] * (-x[1] * t).exp()).collect() };
        let measured = f(&[3.0, 0.7]);
        let sigma = vec![0.01; 50];
        let params = [Param::new("a", 1.0, 10.0, 0.0, 10.0), Param::new("k", 0.1, 10.0, 0.0, 10.0)];
        let c = calibrate(&Problem { params: &params, measured: &measured, sigma: &sigma, robust_k: None }, f).unwrap();
        assert!((c.params[0].value - 3.0).abs() < 0.01 && (c.params[1].value - 0.7).abs() < 0.01, "{:?}", c.params);
        let limited = [Param::new("a", 1.0, 10.0, 0.0, 2.0), Param::new("k", 0.1, 10.0, 0.0, 10.0)];
        let c = calibrate(&Problem { params: &limited, measured: &measured, sigma: &sigma, robust_k: None }, f).unwrap();
        assert!(c.params[0].value <= 2.0 + 1e-12);
    }

    #[test]
    fn a_stall_away_from_the_optimum_is_not_called_converged() {
        // A cliff right at the start: the slope says move, every step costs more.
        let params = [Param::new("a", 0.0, 100.0, -10.0, 10.0)];
        let c = calibrate(&Problem { params: &params, measured: &[5.0], sigma: &[1.0], robust_k: None }, |x| if x[0] == 0.0 { vec![0.0] } else { vec![1e6 + x[0]] }).unwrap();
        assert!(!c.converged, "{c:?}");
        // At a limit the data would like to cross, stopping is the right answer.
        let held = [Param::new("a", 0.0, 100.0, 0.0, 2.0)];
        let c = calibrate(&Problem { params: &held, measured: &[5.0], sigma: &[1.0], robust_k: None }, |x| vec![x[0]]).unwrap();
        assert!((c.params[0].value - 2.0).abs() < 1e-9 && c.converged, "{c:?}");
    }

    #[test]
    fn bad_input_is_a_message() {
        let p = [Param::new("a", 0.0, 1.0, -1.0, 1.0)];
        assert!(calibrate(&Problem { params: &[], measured: &[], sigma: &[], robust_k: None }, |_| vec![]).is_err());
        assert!(calibrate(&Problem { params: &p, measured: &[1.0], sigma: &[], robust_k: None }, |_| vec![1.0]).is_err());
        assert!(calibrate(&Problem { params: &p, measured: &[1.0], sigma: &[1.0], robust_k: None }, |_| vec![]).is_err());
        assert!(calibrate(&Problem { params: &p, measured: &[f64::NAN], sigma: &[1.0], robust_k: None }, |_| vec![1.0]).is_err(), "a NaN reading must not pass for a fit");
        assert!(calibrate(&Problem { params: &p, measured: &[1.0], sigma: &[1.0], robust_k: None }, |_| vec![f64::NAN]).is_err());
        let bad = [Param::new("a", 0.0, 0.0, -1.0, 1.0)];
        assert!(calibrate(&Problem { params: &bad, measured: &[1.0], sigma: &[1.0], robust_k: None }, |_| vec![1.0]).is_err());
    }
}
