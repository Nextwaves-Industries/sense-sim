//! Judges a finished simulation against what the antenna has to do, says whether the
//! result can be believed, and diagnoses what to change next. This is the engineering
//! judgement the assistant applies after a run, as tested rules.

use serde::{Deserialize, Serialize};

use crate::calc;
use crate::project::{SimResult, band_worst};

/// What the antenna has to do. Frequencies in Hz.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Requirements {
    pub f_lo: f64,
    pub f_hi: f64,
    /// Worst S11 allowed over the band, dB.
    pub s11_db: f64,
    pub min_gain_dbi: Option<f64>,
    /// Total efficiency, 0..1: includes mismatch.
    pub min_efficiency: Option<f64>,
    pub max_axial_ratio_db: Option<f64>,
    pub min_front_to_back_db: Option<f64>,
    /// Largest overall size, metres, in any order.
    pub max_size_m: Option<[f64; 3]>,
}

impl Requirements {
    pub fn center(&self) -> f64 {
        (self.f_lo + self.f_hi) / 2.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub name: String,
    pub pass: bool,
    pub measured: String,
    pub required: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The result can't be believed yet, whatever it says.
    NotTrustworthy,
    NotMet,
    /// Meets the requirements on a mesh nobody has checked.
    MetMeshUnchecked,
    MetMeshChecked,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Assessment {
    pub verdict: Verdict,
    pub checks: Vec<Check>,
    /// Why the numbers may be wrong.
    pub trust_issues: Vec<String>,
    /// Limits of what the numbers mean, which don't stop the design.
    pub notes: Vec<String>,
    /// What the numbers say is happening.
    pub diagnosis: Vec<String>,
    /// What to do next, most useful first.
    pub next: Vec<String>,
}

/// State around the result that the result itself can't know.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Context {
    /// The design changed after this run.
    pub stale: bool,
    /// A mesh study of this design: `Some(converged)`.
    pub mesh_converged: Option<bool>,
    /// Overall size of the model, metres.
    pub size_m: Option<[f64; 3]>,
}

fn ghz(f: f64) -> String {
    format!("{:.4} GHz", f / 1e9)
}

fn check(out: &mut Vec<Check>, name: &str, pass: bool, measured: String, required: String) {
    out.push(Check { name: name.to_string(), pass, measured, required });
}

/// Judges `r`. Only port 1 is judged for match; a multi-port design is flagged.
pub fn assess(r: &SimResult, req: &Requirements, ctx: Context) -> Assessment {
    const PORT: usize = 0;
    let mut checks = Vec::new();
    let mut trust_issues = Vec::new();
    let mut notes = Vec::new();
    let mut diagnosis = Vec::new();
    let mut next = Vec::new();
    let net = &r.network;
    let fc = req.center();

    if ctx.stale {
        trust_issues.push("The design changed after this run, so these numbers describe an older design.".to_string());
        next.push("Call antenna_run, then antenna_assess again.".to_string());
    }
    trust_issues.extend(r.warnings.iter().cloned());

    let covered = net.ports() > 0 && net.f.first().is_some_and(|f| *f <= req.f_lo) && net.f.last().is_some_and(|f| *f >= req.f_hi);
    if covered {
        let worst = band_worst(net, PORT, PORT, req.f_lo, req.f_hi).unwrap_or(f64::INFINITY);
        check(&mut checks, "match over the band", worst <= req.s11_db, format!("worst S11 {worst:.1} dB"), format!("{:.1} dB or lower, {} to {}", req.s11_db, ghz(req.f_lo), ghz(req.f_hi)));
        if worst > req.s11_db {
            diagnose_match(r, req, &mut diagnosis, &mut next);
        }
    } else {
        check(&mut checks, "match over the band", false, "not simulated".to_string(), format!("sweep covering {} to {}", ghz(req.f_lo), ghz(req.f_hi)));
        next.push(format!("The sweep doesn't cover the band: widen it with antenna_set f_lo_ghz and f_hi_ghz to at least {:.4} and {:.4}, then run again.", req.f_lo / 1e9 * 0.9, req.f_hi / 1e9 * 1.1));
    }
    if net.ports() > 1 {
        notes.push(format!("This model has {} ports and only port 1 is judged. Check the coupling in antenna_results.", net.ports()));
    }

    let far = r.far_index(fc).and_then(|k| r.metrics(PORT, k).map(|m| (r.far_f[k], m)));
    let wants_far = req.min_gain_dbi.is_some() || req.min_efficiency.is_some() || req.max_axial_ratio_db.is_some() || req.min_front_to_back_db.is_some();
    match far {
        Some((f_far, m)) => {
            if (f_far - fc).abs() > (req.f_hi - req.f_lo).max(fc * 0.01) {
                notes.push(format!("The far field is at {}, away from the band centre {}.", ghz(f_far), ghz(fc)));
                next.push(format!("Set far_ghz to {:.4} with antenna_set and run again, so the pattern is judged in band.", fc / 1e9));
            }
            if let Some(g) = req.min_gain_dbi {
                check(&mut checks, "gain", m.peak_realized_dbi >= g, format!("{:.2} dBi realized", m.peak_realized_dbi), format!("{g:.2} dBi or more"));
                if m.peak_realized_dbi < g {
                    let lost = m.peak_directivity_dbi - m.peak_realized_dbi;
                    if lost > 1.0 {
                        diagnosis.push(format!("Directivity is {:.2} dBi, but {lost:.1} dB is lost to mismatch and material loss before it counts as realized gain.", m.peak_directivity_dbi));
                    }
                    if m.peak_directivity_dbi < g {
                        diagnosis.push(format!("Even with no loss this shape reaches only {:.2} dBi: more gain needs a larger aperture, more elements or another antenna type.", m.peak_directivity_dbi));
                        next.push("Use search_knowledge on antenna selection, or antenna_array for what several elements give.".to_string());
                    }
                }
            }
            if let Some(e) = req.min_efficiency {
                check(&mut checks, "total efficiency", m.total_efficiency >= e, format!("{:.1} %", m.total_efficiency * 100.0), format!("{:.1} % or more", e * 100.0));
                if m.total_efficiency < e && m.rad_efficiency < e {
                    diagnosis.push(format!("Radiation efficiency alone is {:.1} %, so material loss is the limit, not the match. Consider a lower-loss substrate, a thicker one, or air.", m.rad_efficiency * 100.0));
                }
            }
            if let Some(a) = req.max_axial_ratio_db {
                check(&mut checks, "axial ratio", m.axial_ratio_db <= a, format!("{:.1} dB at the peak", m.axial_ratio_db), format!("{a:.1} dB or lower"));
            }
            if let Some(b) = req.min_front_to_back_db {
                let got = m.front_to_back_db;
                check(&mut checks, "front to back", got.is_some_and(|v| v >= b), got.map_or("not defined".to_string(), |v| format!("{v:.1} dB")), format!("{b:.1} dB or more"));
            }
            // Passing these would mean the solver is wrong, not that the antenna is good.
            if m.peak_gain_dbi > m.peak_directivity_dbi + 0.1 {
                trust_issues.push(format!("Gain {:.2} dBi exceeds directivity {:.2} dBi, which isn't physical.", m.peak_gain_dbi, m.peak_directivity_dbi));
            }
            if r.lossless {
                notes.push("No loss is modelled, so efficiency and gain are upper bounds, not predictions. Use a real metal and substrate for a final answer.".to_string());
            }
            if let Some(s) = ctx.size_m {
                let a = (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt() / 2.0;
                let ka = 2.0 * std::f64::consts::PI * a * fc / 299_792_458.0;
                let limit = 10.0 * calc::harrington_directivity(ka).max(1.5).log10();
                if m.peak_directivity_dbi > limit + 3.0 {
                    notes.push(format!("Directivity {:.1} dBi is {:.1} dB above the normal limit of {limit:.1} dBi for this size. Either the ground plane or an infinite plane in the model isn't counted in the size, or the result needs checking.", m.peak_directivity_dbi, m.peak_directivity_dbi - limit));
                }
            }
        }
        None if wants_far => {
            check(&mut checks, "pattern", false, "no far field".to_string(), "far field in the band".to_string());
            next.push(format!("Set far_ghz to {:.4} with antenna_set and run again.", fc / 1e9));
        }
        None => {}
    }

    if let (Some(max), Some(size)) = (req.max_size_m, ctx.size_m) {
        let sorted = |mut v: [f64; 3]| {
            v.sort_by(f64::total_cmp);
            v
        };
        let (a, b) = (sorted(size), sorted(max));
        let fits = a.iter().zip(&b).all(|(x, y)| x <= y);
        check(&mut checks, "size", fits, format!("{:.1} x {:.1} x {:.1} mm", size[0] * 1e3, size[1] * 1e3, size[2] * 1e3), format!("within {:.1} x {:.1} x {:.1} mm", max[0] * 1e3, max[1] * 1e3, max[2] * 1e3));
    }

    let met = checks.iter().all(|c| c.pass);
    match ctx.mesh_converged {
        Some(true) => {}
        Some(false) => trust_issues.push("A mesh study of this design did not converge, so the numbers still depend on the mesh.".to_string()),
        None if met && trust_issues.is_empty() => next.push("Requirements are met on this mesh. Confirm with antenna_converge before quoting the numbers, or state that the mesh wasn't checked.".to_string()),
        None => {}
    }
    let verdict = if !trust_issues.is_empty() {
        Verdict::NotTrustworthy
    } else if !met {
        Verdict::NotMet
    } else if ctx.mesh_converged == Some(true) {
        Verdict::MetMeshChecked
    } else {
        Verdict::MetMeshUnchecked
    };
    Assessment { verdict, checks, trust_issues, notes, diagnosis, next }
}

/// Why the band isn't matched, in the order an engineer would look.
fn diagnose_match(r: &SimResult, req: &Requirements, diagnosis: &mut Vec<String>, next: &mut Vec<String>) {
    const PORT: usize = 0;
    let net = &r.network;
    let fc = req.center();
    let half = ((req.f_hi - req.f_lo) / 2.0).max(fc * 0.002);
    let k = net.f.iter().enumerate().min_by(|a, b| (a.1 - fc).abs().total_cmp(&(b.1 - fc).abs())).map_or(0, |x| x.0);
    let z0 = net.z0.get(PORT).copied().unwrap_or(50.0);
    let z = net.zin(PORT).get(k).copied();
    let nearest = net.resonances(PORT).into_iter().min_by(|a, b| (a.0 - fc).abs().total_cmp(&(b.0 - fc).abs()));

    let offset = nearest.map(|(f, _)| f - fc);
    // Tuned means the reactance is small at the centre, even if a poor match leaves
    // too shallow a dip to count as a resonance.
    let tuned = z.is_some_and(|z| z.im.abs() < 0.5 * z.re.max(1.0));
    let centred = tuned || offset.is_some_and(|d| d.abs() <= half);
    if let Some((f_res, dip)) = nearest
        && !centred
    {
        let scale = f_res / fc;
        let word = if f_res < fc { "low: the resonant length is too long" } else { "high: the resonant length is too short" };
        diagnosis.push(format!("The nearest resonance is at {} ({dip:.1} dB), {:.1} % {word}.", ghz(f_res), (f_res - fc).abs() / fc * 100.0));
        next.push(format!("Multiply the resonant dimension by about {scale:.3}, since resonant lengths scale as 1/f, then run again. Use antenna_get for the variable, and antenna_sweep if one step doesn't land it."));
    } else if nearest.is_none() && !centred {
        diagnosis.push("No resonance shows in the sweep.".to_string());
        next.push("Widen the sweep to find where it resonates, or check the feed: an unconnected or shorted port shows no dip.".to_string());
    }

    if let Some(z) = z {
        let kind = if z.im > 0.0 { "inductive" } else { "capacitive" };
        diagnosis.push(format!("At the band centre the input is {:.1} {} j{:.1} ohm ({kind}) against {z0:.0} ohm.", z.re, if z.im < 0.0 { "-" } else { "+" }, z.im.abs()));
        if centred && z.im.abs() < z.re.max(1.0) {
            if z.re > z0 * 1.5 {
                diagnosis.push(format!("It resonates in band, but the resistance is too high by {:.1} times.", z.re / z0));
                next.push("Move the feed toward the current maximum (deeper inset, or probe closer to the patch centre), or use antenna_match.".to_string());
            } else if z.re < z0 / 1.5 {
                diagnosis.push(format!("It resonates in band, but the resistance is too low by {:.1} times.", z0 / z.re.max(1e-3)));
                next.push("Move the feed away from the current maximum, use a folded element to raise the resistance, or use antenna_match.".to_string());
            }
        }
    }

    // Tuned, but the band is wider than the antenna: is that fixable by matching?
    let q = net.q_factor(PORT).get(k).copied().filter(|q| q.is_finite() && *q > 0.0);
    if let (Some(q), true) = (q, centred) {
        let needed = req.f_hi - req.f_lo;
        let gamma = 10f64.powf(req.s11_db.min(-0.01) / 20.0);
        let limit = crate::matching::bode_fano_bandwidth(q, fc, gamma);
        if needed > limit {
            diagnosis.push(format!("The antenna's Q of about {q:.0} limits any lossless match to {:.2} % at this S11, and the band needs {:.2} %.", limit / fc * 100.0, needed / fc * 100.0));
            next.push("Matching can't reach this band. Lower the Q: a thicker or lower-permittivity substrate, a wider element, or a broadband antenna type.".to_string());
        } else if net.bandwidths(PORT, req.s11_db).iter().any(|b| b.0 <= fc && fc <= b.1) {
            diagnosis.push(format!("The match holds at the centre but not to the band edges. The Bode-Fano limit is {:.2} % and the band needs {:.2} %, so a network may help.", limit / fc * 100.0, needed / fc * 100.0));
            next.push("Try antenna_match for a band-wide network, and remember its parts add loss.".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::C64;
    use crate::farfield::FarField;
    use crate::sparam::Network;

    /// A series RLC antenna around `f0`.
    fn series_rlc(f0: f64, r: f64, q: f64, f: &[f64]) -> Network {
        let z: Vec<Vec<Vec<C64>>> = f.iter().map(|x| vec![vec![C64::new(r, r * q * (x / f0 - f0 / x))]]).collect();
        Network::from_z(f.to_vec(), &z, vec![50.0])
    }

    fn band(lo: f64, hi: f64, n: usize) -> Vec<f64> {
        (0..n).map(|k| lo + (hi - lo) * k as f64 / (n - 1) as f64).collect()
    }

    fn result(net: Network) -> SimResult {
        SimResult { network: net, ..Default::default() }
    }

    fn req(lo: f64, hi: f64) -> Requirements {
        Requirements { f_lo: lo, f_hi: hi, s11_db: -10.0, min_gain_dbi: None, min_efficiency: None, max_axial_ratio_db: None, min_front_to_back_db: None, max_size_m: None }
    }

    fn sphere(f: f64, power: f64, accepted: f64) -> FarField {
        let mut ff = FarField::grid(f, 5.0, 5.0);
        for row in &mut ff.e_theta {
            row.iter_mut().for_each(|v| *v = C64::new(1.0, 0.0));
        }
        let scale = (power / ff.p_rad()).sqrt();
        for row in &mut ff.e_theta {
            row.iter_mut().for_each(|v| *v *= scale);
        }
        ff.p_incident = accepted;
        ff.p_accepted = accepted;
        ff
    }

    #[test]
    fn a_good_antenna_meets_the_band_but_the_mesh_is_not_yet_checked() {
        let f = band(0.8e9, 1.0e9, 201);
        let a = assess(&result(series_rlc(0.9e9, 50.0, 5.0, &f)), &req(0.89e9, 0.91e9), Context::default());
        assert_eq!(a.verdict, Verdict::MetMeshUnchecked, "{a:?}");
        assert!(a.next.iter().any(|n| n.contains("antenna_converge")), "{a:?}");
        let b = assess(&result(series_rlc(0.9e9, 50.0, 5.0, &f)), &req(0.89e9, 0.91e9), Context { mesh_converged: Some(true), ..Default::default() });
        assert_eq!(b.verdict, Verdict::MetMeshChecked);
    }

    #[test]
    fn a_resonance_too_low_says_to_shorten_and_by_how_much() {
        let f = band(0.7e9, 1.1e9, 401);
        let a = assess(&result(series_rlc(0.85e9, 50.0, 10.0, &f)), &req(0.89e9, 0.91e9), Context::default());
        assert_eq!(a.verdict, Verdict::NotMet);
        assert!(a.diagnosis.iter().any(|d| d.contains("too long")), "{a:?}");
        // 0.85 / 0.90: the dimension has to shrink.
        assert!(a.next.iter().any(|n| n.contains("0.944")), "{a:?}");
    }

    #[test]
    fn a_resonance_too_high_says_to_lengthen() {
        let f = band(0.7e9, 1.2e9, 501);
        let a = assess(&result(series_rlc(1.0e9, 50.0, 10.0, &f)), &req(0.89e9, 0.91e9), Context::default());
        assert!(a.diagnosis.iter().any(|d| d.contains("too short")), "{a:?}");
        assert!(a.next.iter().any(|n| n.contains("1.111")), "{a:?}");
    }

    #[test]
    fn tuned_with_the_wrong_resistance_points_at_the_feed() {
        let f = band(0.8e9, 1.0e9, 201);
        let high = assess(&result(series_rlc(0.9e9, 300.0, 3.0, &f)), &req(0.895e9, 0.905e9), Context::default());
        assert!(high.diagnosis.iter().any(|d| d.contains("too high")), "{high:?}");
        assert!(high.next.iter().any(|n| n.contains("toward the current maximum")), "{high:?}");
        let low = assess(&result(series_rlc(0.9e9, 8.0, 3.0, &f)), &req(0.895e9, 0.905e9), Context::default());
        assert!(low.diagnosis.iter().any(|d| d.contains("too low")), "{low:?}");
    }

    #[test]
    fn a_band_wider_than_bode_fano_allows_rejects_matching() {
        let f = band(0.7e9, 1.1e9, 801);
        let a = assess(&result(series_rlc(0.9e9, 50.0, 60.0, &f)), &req(0.8e9, 1.0e9), Context::default());
        assert_eq!(a.verdict, Verdict::NotMet);
        assert!(a.next.iter().any(|n| n.contains("Matching can't reach")), "{a:?}");
    }

    #[test]
    fn a_sweep_that_misses_the_band_is_a_failure_not_a_pass() {
        let f = band(0.8e9, 0.85e9, 51);
        let a = assess(&result(series_rlc(0.82e9, 50.0, 5.0, &f)), &req(0.89e9, 0.91e9), Context::default());
        assert_eq!(a.verdict, Verdict::NotMet);
        assert!(a.next.iter().any(|n| n.contains("widen it")), "{a:?}");
    }

    #[test]
    fn solver_warnings_stale_runs_and_unconverged_meshes_are_not_trustworthy() {
        let f = band(0.8e9, 1.0e9, 201);
        let good = series_rlc(0.9e9, 50.0, 5.0, &f);
        let r = req(0.89e9, 0.91e9);
        let warned = SimResult { warnings: vec!["Port 1 is not passive".into()], ..result(good.clone()) };
        assert_eq!(assess(&warned, &r, Context::default()).verdict, Verdict::NotTrustworthy);
        let stale = assess(&result(good.clone()), &r, Context { stale: true, ..Default::default() });
        assert_eq!(stale.verdict, Verdict::NotTrustworthy);
        assert!(stale.next[0].contains("antenna_run"));
        assert_eq!(assess(&result(good), &r, Context { mesh_converged: Some(false), ..Default::default() }).verdict, Verdict::NotTrustworthy);
    }

    #[test]
    fn gain_and_efficiency_are_judged_at_the_band_centre() {
        let f = band(0.8e9, 1.0e9, 201);
        let mut r = result(series_rlc(0.9e9, 50.0, 5.0, &f));
        r.far = vec![vec![sphere(0.9e9, 0.5, 1.0)]];
        r.far_f = vec![0.9e9];
        let mut q = req(0.89e9, 0.91e9);
        q.min_gain_dbi = Some(5.0);
        q.min_efficiency = Some(0.8);
        let a = assess(&r, &q, Context::default());
        assert_eq!(a.verdict, Verdict::NotMet);
        assert!(a.checks.iter().any(|c| c.name == "gain" && !c.pass), "{a:?}");
        assert!(a.diagnosis.iter().any(|d| d.contains("larger aperture")), "isotropic can't reach 5 dBi: {a:?}");
        assert!(a.diagnosis.iter().any(|d| d.contains("material loss")), "{a:?}");
        // Asking for a pattern without a far field fails instead of passing silently.
        let mut none = result(series_rlc(0.9e9, 50.0, 5.0, &f));
        none.far.clear();
        let b = assess(&none, &q, Context::default());
        assert!(b.checks.iter().any(|c| c.name == "pattern" && !c.pass), "{b:?}");
    }

    #[test]
    fn a_lossless_model_is_noted_and_size_is_order_free() {
        let f = band(0.8e9, 1.0e9, 201);
        let mut r = result(series_rlc(0.9e9, 50.0, 5.0, &f));
        r.lossless = true;
        r.far = vec![vec![sphere(0.9e9, 1.0, 1.0)]];
        r.far_f = vec![0.9e9];
        let mut q = req(0.89e9, 0.91e9);
        q.max_size_m = Some([0.1, 0.05, 0.01]);
        let fits = assess(&r, &q, Context { size_m: Some([0.01, 0.1, 0.05]), ..Default::default() });
        assert!(fits.notes.iter().any(|n| n.contains("upper bounds")), "{fits:?}");
        assert!(fits.checks.iter().any(|c| c.name == "size" && c.pass), "{fits:?}");
        let big = assess(&r, &q, Context { size_m: Some([0.2, 0.05, 0.01]), ..Default::default() });
        assert_eq!(big.verdict, Verdict::NotMet);
    }
}
