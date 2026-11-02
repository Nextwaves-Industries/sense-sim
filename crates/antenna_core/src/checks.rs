//! Whether a result can be believed: passivity, power balance, broken solver output,
//! and when a mesh study has really converged. These take finished data and need no
//! solver, so the rules can be tested on their own.

use anyhow::{Result, bail};

use crate::farfield::FarField;
use crate::project::SimResult;
use crate::sparam::Network;
use crate::C64;

/// How far from 100 % a lossless model's power balance may read before it is reported.
pub const BALANCE_LIMIT: f64 = 0.02;

/// How much the peak realized gain may still move, in dB, for a mesh to count as converged.
pub const GAIN_LIMIT_DB: f64 = 0.1;

/// The first sample of an openEMS probe file that isn't a finite number, as text. MSVC
/// prints NaN as `-nan(ind)`, which a plain number parser would just skip.
pub fn non_finite_sample(text: &str) -> Option<String> {
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        for token in line.split_whitespace() {
            if !token.parse::<f64>().is_ok_and(f64::is_finite) {
                return Some(format!("'{token}' on line {}", index + 1));
            }
        }
    }
    None
}

/// Whether openEMS said it hit its step limit before the energy decayed. It says so on
/// stderr: "RunFDTD: Warning: Max. number of timesteps was reached before the end-criteria…".
pub fn stopped_by_step_limit(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("number of timesteps was reached") || (text.contains("timesteps") && text.contains("reached before"))
}

/// A port's waves have to be numbers and carry signal, or S is 0/0 and reads as a perfect
/// match. The pulse covers the sweep to about −20 dB, so a far weaker wave is a dead port.
pub fn check_port_waves(port: usize, f: &[f64], inc: &[C64], u: &[C64], i: &[C64]) -> Result<()> {
    let name = port + 1;
    for (k, x) in f.iter().enumerate() {
        if !(inc[k].is_finite() && u[k].is_finite() && i[k].is_finite()) {
            bail!("Port {name} has no valid signal at {:.4} GHz: the run diverged. Try a finer mesh around small features.", x / 1e9);
        }
    }
    let peak = inc.iter().map(|v| v.norm()).fold(0.0, f64::max);
    if peak < 1e-30 {
        bail!("Port {name} is dead: no wave reached it. Its gap may be smaller than the mesh can hold, or nothing metal may touch it.");
    }
    if let Some(k) = inc.iter().position(|v| v.norm() < 1e-6 * peak) {
        bail!("Port {name} has almost no signal at {:.4} GHz, so S there would be meaningless.", f[k] / 1e9);
    }
    Ok(())
}

/// Frequencies where the network makes power: `Σ|Sij|²` above 1 for port `j` driven, which
/// includes a negative input resistance. `slack` allows for what the solver can't resolve.
pub fn passivity_warnings(net: &Network, slack: f64) -> Vec<String> {
    let mut out = Vec::new();
    for j in 0..net.ports() {
        let bad: Vec<f64> = net.f.iter().zip(&net.s).filter(|(_, m)| !(m.iter().map(|row| row[j].norm_sqr()).sum::<f64>() <= 1.0 + slack)).map(|(f, _)| *f).collect();
        if let (Some(a), Some(b)) = (bad.first(), bad.last()) {
            out.push(format!(
                "Port {} is not passive at {} of {} frequencies, {:.4} to {:.4} GHz: it returns more power than it receives, which means a negative resistance. S, VSWR and mismatch there are not physical, and the mesh or the feed needs attention.",
                j + 1,
                bad.len(),
                net.f.len(),
                a / 1e9,
                b / 1e9
            ));
        }
    }
    out
}

/// Radiated power against what the network says went in and stayed, for each far field.
/// On a lossless model it should read 100 %; on any model it can't read above that.
/// `remedy` names what to change, which depends on the solver.
pub fn balance_warnings(net: &Network, far: &[Vec<FarField>], lossless: bool, remedy: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (port, list) in far.iter().enumerate() {
        for ff in list {
            let Some(s) = network_at(net, ff.f) else { continue };
            let Some(column) = s.iter().map(|row| row.get(port)).collect::<Option<Vec<_>>>() else { continue };
            let stays = ff.p_incident.max(0.0) * (1.0 - column.iter().map(|v| v.norm_sqr()).sum::<f64>());
            // Next to nothing going in: the ratio is noise, and passivity covers the rest.
            if stays <= 0.02 * ff.p_incident {
                continue;
            }
            let balance = ff.p_rad() / stays;
            if !balance.is_finite() || balance > 1.0 + BALANCE_LIMIT || (lossless && balance < 1.0 - BALANCE_LIMIT) {
                out.push(format!("Port {} at {:.4} GHz radiates {:.1} % of the power it accepts{}. {remedy}", port + 1, ff.f / 1e9, balance * 100.0, if lossless { ", and a model with no loss should read 100 %" } else { ", more than it can" }));
            }
        }
    }
    out
}

fn network_at(net: &Network, f: f64) -> Option<Vec<Vec<C64>>> {
    if net.s.is_empty() || f < *net.f.first()? || f > *net.f.last()? {
        return None;
    }
    Some(net.interpolate(&[f]).s.swap_remove(0))
}

/// The largest complex change of any S entry between two passes of a mesh study. A shape
/// mismatch or a NaN never reads as small.
pub fn delta_s(a: &Network, b: &Network) -> f64 {
    if a.f.len() != b.f.len() || a.s.is_empty() {
        return f64::INFINITY;
    }
    let mut worst: f64 = 0.0;
    for (ma, mb) in a.s.iter().zip(&b.s) {
        for (ra, rb) in ma.iter().zip(mb) {
            for (x, y) in ra.iter().zip(rb) {
                let d = (x - y).norm();
                if d.is_nan() {
                    return f64::INFINITY;
                }
                worst = worst.max(d);
            }
        }
    }
    worst
}

/// The largest change of peak realized gain over all ports and far-field frequencies, in
/// dB. 0 when the runs have no far field to compare.
pub fn gain_change_db(a: &SimResult, b: &SimResult) -> f64 {
    let mut worst: f64 = 0.0;
    for (pa, pb) in a.far.iter().zip(&b.far) {
        for (fa, fb) in pa.iter().zip(pb) {
            let d = (fa.metrics().peak_realized_dbi - fb.metrics().peak_realized_dbi).abs();
            if d.is_nan() {
                return f64::INFINITY;
            }
            worst = worst.max(d);
        }
    }
    worst
}

/// How much of a first-order error is left after a pass that moved by `delta` with
/// density ratio `ratio`: the rest of the geometric series, `delta / (ratio − 1)`. At a
/// ratio of 1.35 that is 2.9 times what the pass moved.
pub fn remaining_error(delta: f64, ratio: f64) -> f64 {
    delta / (ratio - 1.0).max(1e-9)
}

/// One pass of a mesh study against the one before.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PassChange {
    pub delta_s: f64,
    pub gain_db: f64,
}

/// Converged takes two passes running where the error left is under `limit` and the gain
/// has stopped moving. One small step can be chance: S11 can cross over between meshes.
pub fn study_converged(passes: &[PassChange], ratio: f64, limit: f64) -> bool {
    passes.len() >= 2 && passes[passes.len() - 2..].iter().all(|p| remaining_error(p.delta_s, ratio) < limit && p.gain_db < GAIN_LIMIT_DB)
}

/// The density of each pass of a study: from the project's own, at `ratio` per pass.
pub fn study_densities(start: f64, ratio: f64, passes: usize) -> Vec<f64> {
    (0..passes).map(|i| (start * ratio.powi(i as i32)).round()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: Vec<C64>) -> Network {
        Network { f: vec![1e9; s.len()].iter().enumerate().map(|(k, f)| f + k as f64 * 1e6).collect(), z0: vec![50.0], s: s.into_iter().map(|v| vec![vec![v]]).collect() }
    }

    #[test]
    fn a_probe_with_a_nan_is_caught_however_it_is_written() {
        assert_eq!(non_finite_sample("% time/s\tvoltage/V\n0 0\n1e-12 2.5e-3\n"), None);
        for bad in ["-nan(ind)", "nan", "NaN", "inf", "-inf", "1.#INF", "1.#QNAN"] {
            let text = format!("% header\n0 0\n1e-12 {bad}\n");
            assert!(non_finite_sample(&text).is_some_and(|m| m.contains(bad) && m.contains("line 3")), "{bad}");
        }
    }

    /// The real text from an openEMS run, which only ever goes to stderr.
    #[test]
    fn a_run_cut_at_the_step_limit_is_recognised() {
        let stderr = "ProcessFieldsFD::InitProcess: No frequencies found... skipping this dump!\nRunFDTD: Warning: Max. number of timesteps was reached before the end-criteria of -30dB was reached... \n\tYou may want to choose a higher number of max. timesteps... \n";
        assert!(stopped_by_step_limit(stderr));
        assert!(!stopped_by_step_limit("ProcessFieldsFD::InitProcess: No frequencies found... skipping this dump!\nopenEMS::SetupFDTD: Warning, max. number of timesteps is smaller than three times the excitation.\n"));
    }

    #[test]
    fn dead_or_broken_ports_fail_instead_of_reading_as_matched() {
        let f = [1e9, 1.1e9];
        let good = [C64::new(1e-12, 0.0), C64::new(0.0, 8e-13)];
        assert!(check_port_waves(0, &f, &good, &good, &good).is_ok());
        let zero = [C64::new(0.0, 0.0); 2];
        assert!(check_port_waves(1, &f, &zero, &zero, &zero).unwrap_err().to_string().contains("Port 2 is dead"));
        let nan = [C64::new(f64::NAN, 0.0), C64::new(1.0, 0.0)];
        assert!(check_port_waves(0, &f, &good, &nan, &good).unwrap_err().to_string().contains("diverged"));
        let notch = [C64::new(1e-12, 0.0), C64::new(1e-20, 0.0)];
        assert!(check_port_waves(0, &f, &notch, &good, &good).is_err());
    }

    #[test]
    fn a_negative_resistance_is_reported() {
        let z = |r: f64| vec![vec![C64::new(r, 10.0)]];
        let f = vec![1e9, 1.1e9, 1.2e9];
        let bad = Network::from_z(f.clone(), &[z(30.0), z(-20.0), z(30.0)], vec![50.0]);
        let w = passivity_warnings(&bad, 2e-3);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("not passive") && w[0].contains("1 of 3"), "{w:?}");
        let good = Network::from_z(f, &[z(30.0), z(20.0), z(30.0)], vec![50.0]);
        assert!(passivity_warnings(&good, 2e-3).is_empty());
        // A NaN is not passive either.
        assert!(!passivity_warnings(&net(vec![C64::new(f64::NAN, 0.0), C64::new(0.1, 0.0)]), 2e-3).is_empty());
    }

    fn sphere_of(power: f64) -> FarField {
        let mut ff = FarField::grid(1e9, 5.0, 5.0);
        for row in &mut ff.e_theta {
            row.iter_mut().for_each(|v| *v = C64::new(1.0, 0.0));
        }
        let scale = (power / ff.p_rad()).sqrt();
        for row in &mut ff.e_theta {
            row.iter_mut().for_each(|v| *v *= scale);
        }
        ff.p_incident = 1.0;
        ff
    }

    #[test]
    fn power_balance_is_judged_against_what_stayed_in_the_model() {
        let matched = Network { f: vec![0.9e9, 1.1e9], z0: vec![50.0], s: vec![vec![vec![C64::new(0.1, 0.0)]]; 2] };
        let near = |p: f64, lossless: bool| balance_warnings(&matched, &[vec![sphere_of(p)]], lossless, "Refine it.");
        // 0.99 stays in the model.
        assert!(near(0.99, true).is_empty());
        assert!(near(0.99 * 0.97, true)[0].contains("should read 100 %"), "the monopole's case");
        assert!(near(0.99 * 0.5, false).is_empty(), "real loss is allowed");
        assert!(near(0.99 * 1.05, false)[0].contains("more than it can"));
        // Port 1 coupling to port 2: what leaves there isn't missing from the far field.
        let s = vec![vec![C64::new(0.1, 0.0), C64::new(0.5, 0.0)], vec![C64::new(0.5, 0.0), C64::new(0.1, 0.0)]];
        let two = Network { f: vec![0.9e9, 1.1e9], z0: vec![50.0, 50.0], s: vec![s.clone(), s] };
        assert!(balance_warnings(&two, &[vec![sphere_of(0.74)]], true, "").is_empty(), "1 − 0.01 − 0.25");
    }

    /// The audit's default dipole: ΔS 0.0144 at a ratio of 1.35 used to pass a limit of
    /// 0.02 with 0.0424 of error left.
    #[test]
    fn a_small_step_with_a_small_ratio_is_not_convergence() {
        assert!((remaining_error(0.0144, 1.35) - 0.0411).abs() < 1e-3);
        let two = [PassChange { delta_s: 0.0144, gain_db: 0.01 }, PassChange { delta_s: 0.0144, gain_db: 0.01 }];
        assert!(!study_converged(&two, 1.35, 0.02));
        assert!(study_converged(&two, 2.0, 0.02));
    }

    #[test]
    fn convergence_takes_two_passes_running_and_the_gain_too() {
        let ok = PassChange { delta_s: 0.001, gain_db: 0.01 };
        assert!(!study_converged(&[ok], 2.0, 0.02), "one pass can be chance");
        assert!(study_converged(&[ok, ok], 2.0, 0.02));
        assert!(!study_converged(&[PassChange { delta_s: 0.5, gain_db: 0.0 }, ok], 2.0, 0.02), "the pass before it moved");
        assert!(!study_converged(&[ok, PassChange { delta_s: 0.001, gain_db: 0.5 }], 2.0, 0.02), "S settled and the gain did not");
        assert!(!study_converged(&[ok, PassChange { delta_s: f64::INFINITY, gain_db: 0.0 }], 2.0, 0.02));
    }

    #[test]
    fn delta_s_looks_at_every_entry() {
        let two = |s11: f64, s21: f64| {
            let m = vec![vec![C64::new(s11, 0.0), C64::new(s21, 0.0)], vec![C64::new(s21, 0.0), C64::new(s11, 0.0)]];
            Network { f: vec![1e9], z0: vec![50.0, 50.0], s: vec![m] }
        };
        assert!((delta_s(&two(0.1, 0.1), &two(0.1, 0.3)) - 0.2).abs() < 1e-12, "coupling moved, port 1 didn't");
        assert_eq!(delta_s(&net(vec![C64::new(f64::NAN, 0.0)]), &net(vec![C64::new(0.1, 0.0)])), f64::INFINITY);
    }

    #[test]
    fn the_gain_change_covers_every_port_and_frequency() {
        let at = |p: f64| {
            let mut ff = sphere_of(p);
            ff.p_accepted = 1.0;
            ff
        };
        let a = SimResult { far: vec![vec![at(1.0)], vec![at(1.0)]], ..Default::default() };
        let b = SimResult { far: vec![vec![at(1.0)], vec![at(0.5)]], ..Default::default() };
        assert!((gain_change_db(&a, &b) - 3.0103).abs() < 0.01, "{}", gain_change_db(&a, &b));
    }

    #[test]
    fn a_study_starts_from_the_project_density() {
        assert_eq!(study_densities(24.0, 2.0, 3), vec![24.0, 48.0, 96.0]);
        assert_eq!(study_densities(20.0, 1.5, 3), vec![20.0, 30.0, 45.0]);
    }
}
