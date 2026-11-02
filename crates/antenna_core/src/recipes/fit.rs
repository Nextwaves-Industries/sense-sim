//! Closing the loop for a small team: one VNA sweep of a built antenna corrects the
//! model, and production tolerance is judged by what it costs in the field, not by
//! whether S11 stays under a line.

use anyhow::{Result, bail};
use proof::{Calibration, Dist, Evidence, Factor, Param, Problem, RobustOptions, RobustReport, Target, calibrate, robust};
use serde::{Deserialize, Serialize};

use crate::C64;
use crate::sparam::Network;

/// A simulated S11 corrected to explain a measurement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeasuredFit {
    /// measured(f) ≈ simulated(f · scale): above 1, the real antenna resonates low.
    pub f_scale: f64,
    pub f_scale_sigma: f64,
    /// Extra series resistance the real antenna has, ohms. From S11 alone it can't be told
    /// whether that's loss or radiation.
    pub r_extra: f64,
    /// Other values of `r_extra` that explain the sweep about as well: a given |S11| has
    /// a resistance on each side of Z0. Empty when the sweep or its phase settles it.
    #[serde(default)]
    pub r_extra_alternatives: Vec<f64>,
    /// What to keep in mind when reading this fit.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Resonance of each, Hz.
    pub f_res_simulated: Option<f64>,
    pub f_res_measured: Option<f64>,
    /// RMS difference in |S11| over the sweep, dB.
    pub rms_before_db: f64,
    pub rms_after_db: f64,
    /// A substrate's εr that would explain the shift, given the modelled one.
    pub implied_er: Option<f64>,
    pub calibration: Calibration,
}

fn interp_z(net: &Network, port: usize, z: &[C64], f: f64) -> C64 {
    let fs = &net.f;
    if f <= fs[0] {
        return z[0];
    }
    if f >= *fs.last().unwrap() {
        return z[z.len() - 1];
    }
    let i = fs.partition_point(|x| *x < f).clamp(1, fs.len() - 1);
    let t = (f - fs[i - 1]) / (fs[i] - fs[i - 1]);
    let _ = port;
    z[i - 1] + (z[i] - z[i - 1]) * t
}

fn deepest(net: &Network, port: usize) -> Option<f64> {
    let db = net.sij_db(port, port);
    let (k, v) = db.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?;
    (*v < -3.0).then(|| net.f[k])
}

/// Fits a frequency scale and a series loss so the simulation explains a measured
/// S11 (`from_touchstone`). `er_model`: the substrate εr the simulation used, to
/// report what the real one seems to be; pass `None` when there's no dielectric.
pub fn fit_measured(measured: &Network, simulated: &Network, port: usize, er_model: Option<f64>) -> Result<MeasuredFit> {
    if measured.f.len() < 5 || simulated.f.len() < 5 {
        bail!("need at least 5 frequency points in both sweeps");
    }
    if port >= measured.ports() || port >= simulated.ports() {
        bail!("port {} isn't in both sweeps", port + 1);
    }
    // Compare where both cover, leaving room for the frequency scale to move.
    let (lo, hi) = (simulated.f[0], *simulated.f.last().unwrap());
    let fs: Vec<f64> = measured.f.iter().copied().filter(|f| *f >= lo * 0.98 && *f <= hi * 1.02).collect();
    if fs.len() < 5 {
        bail!("the measurement ({:.3}–{:.3} GHz) and the simulation ({:.3}–{:.3} GHz) barely overlap", measured.f[0] / 1e9, measured.f.last().unwrap() / 1e9, lo / 1e9, hi / 1e9);
    }
    let db_measured = measured.sij_db(port, port);
    // A perfect notch computes as −300 dB and would outweigh the whole sweep. No VNA
    // reads that deep on an antenna, so neither curve is compared below this.
    const FLOOR_DB: f64 = -40.0;
    let m: Vec<f64> = measured.f.iter().zip(&db_measured).filter(|(f, _)| **f >= lo * 0.98 && **f <= hi * 1.02).map(|(_, v)| v.max(FLOOR_DB)).collect();
    let z_sim = simulated.zin(port);
    let z0 = simulated.z0[port];
    let predict = |x: &[f64]| -> Vec<f64> {
        fs.iter()
            .map(|f| {
                let z = interp_z(simulated, port, &z_sim, f * x[0]) + x[1];
                crate::db20(((z - z0) / (z + z0)).norm()).max(FLOOR_DB)
            })
            .collect()
    };
    let params = [Param::new("Frequency scale", 1.0, 0.1, 0.7, 1.4), Param::new("Extra series resistance (Ω)", 0.0, 10.0, 0.0, 60.0)];
    // A sweep's points share one calibration and one fixture, so 200 of them aren't 200
    // independent readings: however many there are, they count as at most 20.
    let sigma = vec![(fs.len() as f64 / 20.0).max(1.0).sqrt(); fs.len()];
    let mut best = (f64::MAX, 1.0);
    // A coarse scan first: the dB curve has local minima a few percent apart.
    for k in 0..=120 {
        let s = 0.8 + 0.4 * k as f64 / 120.0;
        let p = predict(&[s, 0.0]);
        let e: f64 = p.iter().zip(&m).map(|(a, b)| (a - b).powi(2)).sum();
        if e < best.0 {
            best = (e, s);
        }
    }
    let fit_from = |scale: f64, r: f64| {
        let params = [Param { prior_mean: scale, ..params[0].clone() }, Param { prior_mean: r, ..params[1].clone() }];
        calibrate(&Problem { params: &params, measured: &m, sigma: &sigma, robust_k: None }, predict).map_err(|e| anyhow::anyhow!(e))
    };
    let mut calibration = fit_from(best.1, 0.0)?;

    // |S11| alone has two answers, a resistance either side of Z0. Find every resistance
    // that explains the magnitudes about as well, and let the phase choose if it can.
    let rms_with = |r: f64| {
        let p = predict(&[calibration.params[0].value, r]);
        (p.iter().zip(&m).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / m.len() as f64).sqrt()
    };
    let scan: Vec<f64> = (0..=60).map(|r| rms_with(r as f64)).collect();
    let floor = scan.iter().copied().fold(f64::MAX, f64::min);
    let near_best = floor + (0.25 * floor).max(0.25);
    let mut candidates: Vec<f64> = (0..scan.len()).filter(|&r| scan[r] <= near_best && (r == 0 || scan[r] <= scan[r - 1]) && (r + 1 == scan.len() || scan[r] <= scan[r + 1])).map(|r| r as f64).collect();
    let measured_gamma: Vec<C64> = measured.f.iter().zip(&measured.s).filter(|(f, _)| **f >= lo * 0.98 && **f <= hi * 1.02).map(|(_, s)| s[port][port]).collect();
    let phase_error = |r: f64| {
        let scale = calibration.params[0].value;
        let total: f64 = fs.iter().zip(&measured_gamma).map(|(f, g)| {
            let z = interp_z(simulated, port, &z_sim, f * scale) + r;
            (((z - z0) / (z + z0)) - *g).norm_sqr()
        }).sum();
        (total / fs.len() as f64).sqrt()
    };
    let mut by_phase: Vec<(f64, f64)> = candidates.iter().map(|r| (phase_error(*r), *r)).collect();
    by_phase.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Only a close match is evidence: an unknown cable or reference plane spoils the
    // phase, and then a poor match says nothing either way.
    if by_phase.len() > 1 && by_phase[0].0 < 0.05 && by_phase[1].0 > 2.0 * by_phase[0].0 {
        candidates = vec![by_phase[0].1];
    }
    if candidates.len() == 1 && (candidates[0] - calibration.params[1].value).abs() > 3.0 {
        calibration = fit_from(calibration.params[0].value, candidates[0])?;
    }
    let r_extra = calibration.params[1].value;
    let r_extra_alternatives: Vec<f64> = if candidates.len() > 1 { candidates.iter().copied().filter(|r| (r - r_extra).abs() > 3.0).collect() } else { Vec::new() };

    let mut notes = Vec::new();
    if !calibration.converged {
        notes.push("The fit stopped without settling, so don't lean on these values.".to_string());
    }
    if !r_extra_alternatives.is_empty() {
        notes.push(format!("Ambiguous: {r_extra:.0} Ω of extra series resistance and {} explain this sweep about as well. Widen the sweep, or give it calibrated phase, to tell them apart.", r_extra_alternatives.iter().map(|r| format!("{r:.0} Ω")).collect::<Vec<_>>().join(" or ")));
    }
    if r_extra < 0.5 && calibration.rms_after > 1.0 {
        notes.push("The fit can only add resistance. The real antenna may have less than the model.".to_string());
    }
    let outside = fs.iter().filter(|f| **f * calibration.params[0].value > hi || **f * calibration.params[0].value < lo).count();
    if outside > 0 {
        notes.push(format!("{outside} of {} measured points scale past the simulated sweep, where its impedance was held flat: solve a wider sweep.", fs.len()));
    }

    let start = predict(&[1.0, 0.0]);
    let rms_before = (start.iter().zip(&m).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / m.len() as f64).sqrt();
    let f_scale = calibration.params[0].value;
    Ok(MeasuredFit {
        f_scale,
        f_scale_sigma: calibration.params[0].sigma,
        r_extra,
        r_extra_alternatives,
        notes,
        f_res_simulated: deepest(simulated, port),
        f_res_measured: deepest(measured, port),
        rms_before_db: rms_before,
        rms_after_db: calibration.rms_after,
        implied_er: er_model.map(|er| er * f_scale * f_scale),
        calibration,
    })
}

/// Field outcome of an antenna's match: how far a link reaches with this mismatch,
/// relative to perfectly matched.
pub fn range_share_from_mismatch(s11_db: f64, path_loss_exp: f64) -> f64 {
    let g2 = 10f64.powf(s11_db / 10.0).min(0.999_999);
    (1.0 - g2).powf(1.0 / path_loss_exp)
}

/// How a metric responds to each uncertain dimension, from `k + 1` solves. Used so
/// a Monte-Carlo across production tolerances doesn't need a solve per sample.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub names: Vec<String>,
    pub nominal_x: Vec<f64>,
    pub nominal_y: f64,
    pub slope: Vec<f64>,
    /// Half the second derivative. A design centred on its target sits at a peak,
    /// where the slope is zero and a straight line would say tolerance costs nothing.
    pub curve: Vec<f64>,
    pub solves: usize,
}

impl Response {
    pub fn eval(&self, x: &[f64]) -> f64 {
        let mut y = self.nominal_y;
        for k in 0..self.slope.len() {
            let d = x[k] - self.nominal_x[k];
            y += self.slope[k] * d + self.curve[k] * d * d;
        }
        y
    }
}

/// Central differences around the nominal design, stepping by `steps` per variable.
pub fn response(names: &[String], x0: &[f64], steps: &[f64], metric: impl Fn(&[f64]) -> Result<f64>) -> Result<Response> {
    if names.len() != x0.len() || x0.len() != steps.len() {
        bail!("{} names, {} values and {} steps", names.len(), x0.len(), steps.len());
    }
    let y0 = metric(x0)?;
    let mut slope = Vec::with_capacity(x0.len());
    let mut curve = Vec::with_capacity(x0.len());
    let mut solves = 1;
    for k in 0..x0.len() {
        if steps[k] == 0.0 {
            slope.push(0.0);
            curve.push(0.0);
            continue;
        }
        let (mut a, mut b) = (x0.to_vec(), x0.to_vec());
        a[k] -= steps[k];
        b[k] += steps[k];
        let (up, down) = (metric(&b)?, metric(&a)?);
        slope.push((up - down) / (2.0 * steps[k]));
        curve.push((up + down - 2.0 * y0) / (2.0 * steps[k] * steps[k]));
        solves += 2;
    }
    Ok(Response { names: names.to_vec(), nominal_x: x0.to_vec(), nominal_y: y0, slope, curve, solves })
}

/// Yield against a field requirement: dimensions spread as the factory makes them,
/// through the response, against a target. An estimate: the many draws go through a
/// fitted formula from `2k + 1` solves, with no cross terms, so their count says
/// nothing about how far that formula is off.
///
/// `cap` limits the metric from above where it has a physical ceiling: a straight
/// line through the response happily predicts 101 % of a perfect match otherwise.
pub fn field_yield(metric: &str, unit: &str, r: &Response, tolerances: &[f64], target: Target, cap: Option<f64>, samples: usize) -> RobustReport {
    let factors: Vec<Factor> = r
        .names
        .iter()
        .zip(r.nominal_x.iter().zip(tolerances))
        .map(|(n, (x, t))| Factor::new(n, "m", if *t > 0.0 { Dist::Normal { mean: *x, sigma: *t } } else { Dist::Fixed { value: *x } }, "Manufacturing tolerance"))
        .collect();
    robust(metric, unit, &factors, Some(target), Evidence::Estimate, RobustOptions { samples, seed: 41 }, |v| cap.map_or(r.eval(v), |c| r.eval(v).min(c)))
}

/// How far a yield from the fitted formula can be trusted, in points of yield: its own
/// test accepts 40 to 70 % against a true 53 %. Yields are quoted no finer than a third
/// of this, because the 2000 draws behind them only measure the formula.
pub const YIELD_TOLERANCE: f64 = 0.15;

/// A yield rounded as far as the formula behind it can support.
pub fn quoted_yield(y: f64) -> f64 {
    (y / 0.05).round() * 0.05
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A series RLC's S11 sweep.
    fn rlc(f0: f64, q: f64, r: f64, f: &[f64]) -> Network {
        let s = f
            .iter()
            .map(|x| {
                let z = C64::new(r, q * r * (x / f0 - f0 / x));
                vec![vec![(z - 50.0) / (z + 50.0)]]
            })
            .collect();
        Network { f: f.to_vec(), z0: vec![50.0], s }
    }

    fn sweep() -> Vec<f64> {
        (0..201).map(|i| 2.0e9 + 5e6 * i as f64).collect()
    }

    #[test]
    fn recovers_a_known_resonance_shift() {
        let f = sweep();
        let sim = rlc(2.5e9, 12.0, 50.0, &f);
        let measured = rlc(2.4e9, 12.0, 50.0, &f);
        let fit = fit_measured(&measured, &sim, 0, Some(4.4)).unwrap();
        println!("{fit:#?}");
        // measured(f) = sim(f · 2.5/2.4).
        assert!((fit.f_scale - 2.5 / 2.4).abs() < 0.005, "{}", fit.f_scale);
        assert!(fit.rms_after_db < 0.25 * fit.rms_before_db, "{} → {}", fit.rms_before_db, fit.rms_after_db);
        // f ∝ 1/√εr: the real substrate is higher.
        assert!((fit.implied_er.unwrap() - 4.4 * (2.5f64 / 2.4).powi(2)).abs() < 0.05);
        assert!((fit.f_res_measured.unwrap() - 2.4e9).abs() < 6e6);
    }

    #[test]
    fn recovers_extra_loss() {
        let f = sweep();
        let sim = rlc(2.5e9, 12.0, 40.0, &f);
        let measured = rlc(2.5e9, 12.0 * 40.0 / 50.0, 50.0, &f);
        let fit = fit_measured(&measured, &sim, 0, None).unwrap();
        assert!((fit.r_extra - 10.0).abs() < 2.0, "{fit:#?}");
        assert!((fit.f_scale - 1.0).abs() < 0.01);
        assert!(fit.implied_er.is_none());
    }

    #[test]
    fn a_matching_measurement_changes_nothing() {
        let f = sweep();
        let sim = rlc(2.5e9, 12.0, 50.0, &f);
        let fit = fit_measured(&sim, &sim, 0, None).unwrap();
        assert!((fit.f_scale - 1.0).abs() < 2e-3, "{}", fit.f_scale);
        assert!(fit.rms_after_db < 0.5, "{}", fit.rms_after_db);
    }

    // OUT-M9: R = 30 Ω at −12 dB can't separate 0 from +53 Ω on magnitude alone.
    #[test]
    fn a_magnitude_that_fits_two_resistances_is_reported_as_ambiguous() {
        // A narrow sweep around resonance, so reactance can't break the tie.
        let f: Vec<f64> = (0..101).map(|i| 2.475e9 + 0.5e6 * i as f64).collect();
        let sim = rlc(2.5e9, 12.0, 30.0, &f);
        // The same antenna seen through 10 cm of uncalibrated cable: the phase is useless.
        let mut through_cable = sim.clone();
        for (k, x) in f.iter().enumerate() {
            let turn = C64::from_polar(1.0, -2.0 * std::f64::consts::TAU * x * 0.1 / 2e8);
            through_cable.s[k][0][0] *= turn;
        }
        let fit = fit_measured(&through_cable, &sim, 0, None).unwrap();
        println!("{fit:#?}");
        assert!(!fit.r_extra_alternatives.is_empty(), "{:?} {:?}", fit.r_extra, fit.notes);
        let all: Vec<f64> = fit.r_extra_alternatives.iter().copied().chain([fit.r_extra]).collect();
        assert!(all.iter().any(|r| *r < 5.0) && all.iter().any(|r| (*r - 53.0).abs() < 6.0), "{all:?}");
        assert!(fit.notes.iter().any(|n| n.contains("Ambiguous")), "{:?}", fit.notes);
        // With usable phase the same sweep has one answer.
        let fit = fit_measured(&sim, &sim, 0, None).unwrap();
        assert!(fit.r_extra_alternatives.is_empty(), "{:?}", fit.r_extra_alternatives);
        assert!(fit.r_extra < 3.0, "{}", fit.r_extra);
    }

    // OUT-M9: 1 dB on each of N correlated points shrank the error with N.
    #[test]
    fn more_points_on_the_same_sweep_dont_shrink_the_error() {
        let thin: Vec<f64> = (0..201).map(|i| 2.0e9 + 5e6 * i as f64).collect();
        let dense: Vec<f64> = (0..1001).map(|i| 2.0e9 + 1e6 * i as f64).collect();
        // Not matched at resonance: a perfect notch's slope is all in the few points on it.
        let wide = rlc(2.5e9, 12.0, 30.0, &dense);
        let a = fit_measured(&rlc(2.4e9, 12.0, 30.0, &thin), &wide, 0, None).unwrap();
        let b = fit_measured(&rlc(2.4e9, 12.0, 30.0, &dense), &wide, 0, None).unwrap();
        // Five times the points would shrink it by 2.2 if they were independent.
        assert!(b.f_scale_sigma > 0.8 * a.f_scale_sigma, "{} vs {}", b.f_scale_sigma, a.f_scale_sigma);
    }

    #[test]
    fn a_scale_past_the_simulated_sweep_is_noted() {
        // The simulation stops at the measured band's top, so scale 1.04 runs off its end.
        let f = sweep();
        let sim = rlc(2.5e9, 12.0, 50.0, &f);
        let measured = rlc(2.4e9, 12.0, 50.0, &f);
        let fit = fit_measured(&measured, &sim, 0, None).unwrap();
        assert!(fit.notes.iter().any(|n| n.contains("past the simulated sweep")), "{:?}", fit.notes);
    }

    // OUT-M10: "Simulated", from a formula through 2k + 1 solves.
    #[test]
    fn field_yield_is_an_estimate_and_quoted_roughly() {
        let names = vec!["a".to_string()];
        let r = response(&names, &[1.0], &[0.1], |x| Ok(x[0])).unwrap();
        let y = field_yield("y", "", &r, &[0.1], Target::at_least(0.5), None, 2000);
        assert_eq!(y.prediction.evidence, Evidence::Estimate);
        assert_eq!(quoted_yield(0.532), 0.55);
        assert_eq!(quoted_yield(1.0), 1.0);
    }

    #[test]
    fn plain_messages_on_bad_sweeps() {
        let f = sweep();
        let sim = rlc(2.5e9, 12.0, 50.0, &f);
        let far: Vec<f64> = (0..50).map(|i| 8e9 + 1e6 * i as f64).collect();
        let e = fit_measured(&rlc(8e9, 12.0, 50.0, &far), &sim, 0, None).unwrap_err().to_string();
        assert!(e.contains("barely overlap"), "{e}");
        assert!(fit_measured(&sim, &sim, 3, None).is_err());
        let short = rlc(2.5e9, 12.0, 50.0, &f[..3]);
        assert!(fit_measured(&short, &sim, 0, None).is_err());
    }

    #[test]
    fn range_share_follows_the_mismatch() {
        assert!((range_share_from_mismatch(-40.0, 2.0) - 1.0).abs() < 1e-3);
        // −3 dB S11: half the power is reflected: range × √½ at n = 2.
        assert!((range_share_from_mismatch(-3.0103, 2.0) - 0.5f64.sqrt()).abs() < 1e-3);
    }

    #[test]
    fn response_reads_a_linear_function_and_yield_matches_theory() {
        let names = vec!["a".to_string(), "b".to_string()];
        let r = response(&names, &[1.0, 2.0], &[0.1, 0.1], |x| Ok(3.0 * x[0] - 2.0 * x[1] + 5.0)).unwrap();
        assert!((r.slope[0] - 3.0).abs() < 1e-9 && (r.slope[1] + 2.0).abs() < 1e-9);
        assert_eq!(r.solves, 5);
        // y = 3a − 2b + 5 with σa = σb = 0.1: σy = 0.36, mean 4; P(y ≥ 4) = ½.
        let y = field_yield("y", "", &r, &[0.1, 0.1], Target::at_least(4.0), None, 4000);
        assert!((y.pass_probability.unwrap() - 0.5).abs() < 0.03, "{:?}", y.pass_probability);
        assert!((y.prediction.high - y.prediction.low - 2.0 * 1.645 * 0.3606).abs() < 0.08, "{:?}", y.prediction);
        assert_eq!(y.main_driver().unwrap().factor, "a");
    }

    #[test]
    fn a_design_centred_on_its_target_still_shows_what_tolerance_costs() {
        // Series RLC, Q 10, band ±2 %, resonance spread 2 %: the worst of the band
        // peaks at the nominal design, so the slope there is zero. The true yield
        // against a range share of 0.95 is about 53 %.
        let share = |x: &[f64]| -> Result<f64> {
            let f0 = 1e9 * (1.0 + x[0]);
            let worst = [0.98e9, 1.0e9, 1.02e9].iter().map(|f: &f64| {
                let z = C64::new(50.0, 500.0 * (f / f0 - f0 / f));
                crate::db20(((z - 50.0) / (z + 50.0)).norm())
            }).fold(f64::MIN, f64::max);
            Ok(range_share_from_mismatch(worst, 2.2))
        };
        let names = vec!["shift".to_string()];
        let r = response(&names, &[0.0], &[0.02], share).unwrap();
        assert!(r.slope[0].abs() < 1e-6, "{}", r.slope[0]);
        let y = field_yield("y", "", &r, &[0.02], Target::at_least(0.95), Some(1.0), 4000);
        let got = y.pass_probability.unwrap();
        assert!(got > 0.4 && got < 0.7, "{got}");
    }

    #[test]
    fn zero_steps_and_zero_tolerances_are_fine() {
        let names = vec!["a".to_string()];
        let r = response(&names, &[1.0], &[0.0], |x| Ok(x[0])).unwrap();
        assert_eq!(r.slope[0], 0.0);
        let y = field_yield("y", "", &r, &[0.0], Target::at_least(0.5), None, 50);
        assert_eq!(y.pass_probability, Some(1.0));
        let spread = response(&names, &[1.0], &[0.1], |x| Ok(x[0])).unwrap();
        let capped = field_yield("y", "", &spread, &[0.5], Target::at_least(0.5), Some(1.0), 200);
        assert!(capped.prediction.high <= 1.0, "{:?}", capped.prediction);
    }
}
