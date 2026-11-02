//! An antenna inside the product: what the enclosure, battery and nearby metal do
//! to it, and what to change. The solver is passed in, so metal-only designs can use
//! the method of moments here and anything with plastic can use FDTD from the app.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::expr;
use crate::matching;
use crate::project::{ExtraPrim, ExtraSolid, Project, band_worst};
use crate::sparam::Network;

/// Something near the antenna, as a box. Coordinates are expressions, so they can
/// refer to the antenna's own variables.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub material: String,
    pub min: [String; 3],
    pub max: [String; 3],
}

impl Part {
    pub fn solid(&self) -> ExtraSolid {
        ExtraSolid { name: self.name.clone(), material: self.material.clone(), prim: ExtraPrim::Box { min: self.min.clone(), max: self.max.clone() } }
    }
}

/// The project with these parts added.
pub fn with_parts(project: &Project, parts: &[Part]) -> Project {
    let mut p = project.clone();
    p.extra.extend(parts.iter().map(Part::solid));
    p
}

/// A thin plastic box around the antenna's bounds, as six walls: `gap` of air
/// inside and `wall` thick. Returned in metres as literal expressions.
pub fn shell_around(project: &Project, material: &str, gap: f64, wall: f64) -> Result<Vec<Part>> {
    let (lo, hi) = project.geometry()?.bounds();
    let fmt = |v: [f64; 3]| -> [String; 3] { [format!("{:e}", v[0]), format!("{:e}", v[1]), format!("{:e}", v[2])] };
    let inner_lo = [lo[0] - gap, lo[1] - gap, lo[2] - gap];
    let inner_hi = [hi[0] + gap, hi[1] + gap, hi[2] + gap];
    let outer_lo = [inner_lo[0] - wall, inner_lo[1] - wall, inner_lo[2] - wall];
    let outer_hi = [inner_hi[0] + wall, inner_hi[1] + wall, inner_hi[2] + wall];
    let mut out = Vec::new();
    for axis in 0..3 {
        let name = ["x", "y", "z"][axis];
        let mut a = outer_lo;
        let mut b = outer_hi;
        b[axis] = inner_lo[axis];
        out.push(Part { name: format!("wall -{name}"), material: material.into(), min: fmt(a), max: fmt(b) });
        a = outer_lo;
        b = outer_hi;
        a[axis] = inner_hi[axis];
        out.push(Part { name: format!("wall +{name}"), material: material.into(), min: fmt(a), max: fmt(b) });
    }
    Ok(out)
}

/// What the integration did to the antenna.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Detuning {
    pub f_target: f64,
    /// Deepest match frequency of each, Hz.
    pub f_res_before: Option<f64>,
    pub f_res_after: Option<f64>,
    /// (after − before) / before, percent.
    pub shift_pct: Option<f64>,
    /// Worst |S11| across the band, dB.
    pub band_s11_before_db: f64,
    pub band_s11_after_db: f64,
    /// Power lost to mismatch at the target, dB (≥ 0).
    pub mismatch_loss_before_db: f64,
    pub mismatch_loss_after_db: f64,
    /// Power lost to mismatch at the band's worst point, dB (≥ 0).
    #[serde(default)]
    pub band_loss_before_db: f64,
    #[serde(default)]
    pub band_loss_after_db: f64,
    /// Plain-language verdict.
    pub summary: String,
}

fn deepest(n: &Network, port: usize) -> Option<f64> {
    let db = n.sij_db(port, port);
    let (k, v) = db.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?;
    (*v < -3.0).then(|| n.f[k])
}

fn mismatch_at(n: &Network, port: usize, f: f64) -> f64 {
    let k = n.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map_or(0, |e| e.0);
    let s = n.s.get(k).map_or(1.0, |m| m[port][port].norm_sqr()).min(0.999_999);
    -10.0 * (1.0 - s).log10()
}

pub fn compare(before: &Network, after: &Network, port: usize, f_lo: f64, f_hi: f64) -> Detuning {
    let f_target = (f_lo + f_hi) / 2.0;
    let (fb, fa) = (deepest(before, port), deepest(after, port));
    let shift = match (fb, fa) {
        (Some(b), Some(a)) => Some((a - b) / b * 100.0),
        _ => None,
    };
    let band_s11_before_db = band_worst(before, port, port, f_lo, f_hi).unwrap_or(0.0);
    let band_s11_after_db = band_worst(after, port, port, f_lo, f_hi).unwrap_or(0.0);
    let (lb, la) = (mismatch_at(before, port, f_target), mismatch_at(after, port, f_target));
    let loss_from = |s11_db: f64| -10.0 * (1.0 - 10f64.powf(s11_db / 10.0).min(0.999_999)).log10();
    let (band_loss_before_db, band_loss_after_db) = (loss_from(band_s11_before_db), loss_from(band_s11_after_db));
    let summary = match shift {
        Some(s) if band_s11_after_db <= -10.0 => format!("Resonance moved {s:+.1} %, and the band still matches ({band_s11_after_db:.1} dB)."),
        Some(s) => format!("Resonance moved {s:+.1} %: the band now reaches {band_s11_after_db:.1} dB, costing {band_loss_after_db:.1} dB of power at its worst point."),
        None => format!("The antenna no longer resonates in the sweep: the band reaches {band_s11_after_db:.1} dB."),
    };
    Detuning { f_target, f_res_before: fb, f_res_after: fa, shift_pct: shift, band_s11_before_db, band_s11_after_db, mismatch_loss_before_db: lb, mismatch_loss_after_db: la, band_loss_before_db, band_loss_after_db, summary }
}

/// A change that brings the antenna back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fix {
    /// Change a dimension.
    Retune { variable: String, from: f64, to: f64, band_s11_db: f64, solves: usize },
    /// Add a matching network at the feed.
    Match { name: String, components: Vec<String>, band_s11_db: f64 },
    /// Keep nearby parts this far away.
    KeepOut { distance_m: f64, shift_pct: f64 },
}

/// Component list of a matching network in plain units.
fn describe(net: &matching::Network) -> Vec<String> {
    net.elements
        .iter()
        .map(|e| {
            use matching::Kind as K;
            let v = e.value;
            match e.kind {
                K::SeriesL => format!("Series {:.2} nH", v * 1e9),
                K::ShuntL => format!("Shunt {:.2} nH", v * 1e9),
                K::SeriesC => format!("Series {:.2} pF", v * 1e12),
                K::ShuntC => format!("Shunt {:.2} pF", v * 1e12),
                K::Line { z0, deg, .. } => format!("Line {z0:.0} Ω, {deg:.0}°"),
                K::Stub { z0, deg, shorted, .. } => format!("{} stub {z0:.0} Ω, {deg:.0}°", if shorted { "Shorted" } else { "Open" }),
            }
        })
        .collect()
}

/// The best matching network for the detuned antenna, or `None` if none reaches -10 dB.
pub fn match_fix(after: &Network, port: usize, f_lo: f64, f_hi: f64) -> Option<Fix> {
    let z0 = *after.z0.get(port)?;
    let zl = after.zin(port);
    // Q of ordinary chip parts, so a loss-free fantasy can't win.
    let list = matching::best_for_band(&after.f, &zl, z0, f_lo, f_hi, Some(50.0), Some(200.0));
    let (net, worst) = list.into_iter().next()?;
    (worst <= -10.0).then(|| Fix::Match { name: net.name.clone(), components: describe(&net), band_s11_db: worst })
}

/// Moves a dimension until the deepest match lands on `f_target` (secant steps,
/// assuming resonance roughly follows 1/length). Stops after `max_solves`.
pub fn retune(project: &Project, variable: &str, f_target: f64, f_lo: f64, f_hi: f64, port: usize, max_solves: usize, solve: impl Fn(&Project) -> Result<Network>) -> Result<Fix> {
    let vars = project.vars()?;
    let Some(&from) = vars.get(variable) else { bail!("no variable '{variable}' in the design") };
    if from == 0.0 {
        bail!("'{variable}' is zero, so it can't be scaled");
    }
    let mut x = from;
    let mut solves = 0;
    let mut best = (f64::MAX, from);
    while solves < max_solves {
        let n = solve(&project.with_values(&[(variable.to_string(), x)]))?;
        solves += 1;
        let worst = band_worst(&n, port, port, f_lo, f_hi).unwrap_or(0.0);
        if worst < best.0 {
            best = (worst, x);
        }
        let Some(fr) = deepest(&n, port) else {
            // No resonance in the sweep: nudge towards it and look again.
            x *= 1.05;
            continue;
        };
        if ((fr - f_target) / f_target).abs() < 0.002 {
            break;
        }
        x *= fr / f_target;
    }
    Ok(Fix::Retune { variable: variable.into(), from, to: best.1, band_s11_db: best.0, solves })
}

/// The nearest a part may come, trying `distances` (metres) from close to far.
/// A distance passes when the resonance moves under `max_shift_pct` and the extra
/// power lost to mismatch at the band centre stays under `max_extra_loss_db`. Both
/// matter: metal close to an antenna often barely moves the resonance while
/// wrecking its impedance. `build` places the part at a distance. `None` if even
/// the farthest is too close.
#[allow(clippy::too_many_arguments)]
pub fn keep_out(project: &Project, distances: &[f64], max_shift_pct: f64, max_extra_loss_db: f64, port: usize, f_lo: f64, f_hi: f64, build: impl Fn(f64) -> Vec<Part>, solve: impl Fn(&Project) -> Result<Network>) -> Result<Option<Fix>> {
    let base = solve(project)?;
    for d in distances {
        let n = solve(&with_parts(project, &build(*d)))?;
        let c = compare(&base, &n, port, f_lo, f_hi);
        let extra = c.mismatch_loss_after_db - c.mismatch_loss_before_db;
        if let Some(s) = c.shift_pct
            && s.abs() <= max_shift_pct
            && extra <= max_extra_loss_db
        {
            return Ok(Some(Fix::KeepOut { distance_m: *d, shift_pct: s }));
        }
    }
    Ok(None)
}

/// Validates part coordinates against the project's variables before a long solve.
pub fn check_parts(project: &Project, parts: &[Part]) -> Result<()> {
    let vars = project.vars()?;
    for p in parts {
        for s in p.min.iter().chain(&p.max) {
            expr::eval(s, &vars).map_err(|e| anyhow::anyhow!("part '{}': {e:#}", p.name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mom::MomOptions;
    use crate::recipes::mom_network;
    use crate::{C64, db20};

    fn strip(f0: &str) -> Project {
        Project::from_template("strip_dipole", Some(f0)).unwrap()
    }

    fn net(p: &Project, f_lo: f64, f_hi: f64) -> Result<Network> {
        let g = p.geometry()?;
        mom_network(&g, f_lo, f_hi, 13, 121, MomOptions { edges_per_lambda: 24.0, ..Default::default() })
    }

    fn plate(at: f64, half: f64) -> Part {
        Part { name: "metal part".into(), material: "PEC".into(), min: [format!("{at:e}"), format!("{:e}", -half), format!("{:e}", -half)], max: [format!("{at:e}"), format!("{half:e}"), format!("{half:e}")] }
    }

    #[test]
    fn nearby_metal_moves_the_resonance_and_distance_helps() {
        let p = strip("2.45GHz");
        let (lo, hi) = (1.6e9, 3.2e9);
        let base = net(&p, lo, hi).unwrap();
        let close = net(&with_parts(&p, &[plate(0.012, 0.04)]), lo, hi).unwrap();
        let far = net(&with_parts(&p, &[plate(0.1, 0.04)]), lo, hi).unwrap();
        let c_close = compare(&base, &close, 0, 2.4e9, 2.5e9);
        let c_far = compare(&base, &far, 0, 2.4e9, 2.5e9);
        println!("close: {c_close:#?}\nfar: {c_far:#?}");
        // Metal this close mostly wrecks the impedance rather than moving the resonance.
        assert!(c_close.band_s11_after_db > c_far.band_s11_after_db + 3.0, "{c_close:?} {c_far:?}");
        assert!(c_close.mismatch_loss_after_db > c_far.mismatch_loss_after_db, "{c_close:?} {c_far:?}");
    }

    #[test]
    fn compare_reads_a_known_shift() {
        // Two notches: one at 1.0 GHz, one at 0.9 GHz.
        let f: Vec<f64> = (0..101).map(|i| 0.7e9 + 6e6 * i as f64).collect();
        let notch = |at: f64| -> Network {
            let s = f.iter().map(|x| {
                let d = (x - at) / 30e6;
                let m = (d * d / (1.0 + d * d)).sqrt().max(0.03);
                vec![vec![C64::new(m, 0.0)]]
            }).collect();
            Network { f: f.clone(), z0: vec![50.0], s }
        };
        let c = compare(&notch(1.0e9), &notch(0.9e9), 0, 0.98e9, 1.02e9);
        assert!((c.shift_pct.unwrap() + 10.0).abs() < 0.8, "{c:?}");
        assert!(c.band_s11_before_db < -3.0);
        assert!(c.band_s11_after_db > -1.0, "{c:?}");
        assert!(c.mismatch_loss_after_db > c.mismatch_loss_before_db);
        assert!(c.summary.contains("costing"), "{}", c.summary);
    }

    // OUT-M5: the verdict looked at the band centre only.
    #[test]
    fn the_band_is_judged_at_its_worst_point_not_its_centre() {
        // Matched at the centre, nearly a full reflection at the band's edge.
        let f: Vec<f64> = (0..101).map(|i| 0.9e9 + 2e6 * i as f64).collect();
        let net = Network { f: f.clone(), z0: vec![50.0], s: f.iter().map(|x| vec![vec![C64::new(((x - 1.0e9).abs() / 20e6).min(0.9), 0.0)]]).collect() };
        let c = compare(&net, &net, 0, 0.98e9, 1.02e9);
        assert!(c.mismatch_loss_after_db < 0.01, "{c:?}");
        // |S11| of 0.9 at the band's edge loses 7.2 dB.
        assert!((c.band_loss_after_db - 7.2).abs() < 0.2, "{c:?}");
    }

    #[test]
    fn retuning_brings_it_back_to_the_band() {
        let p = with_parts(&strip("2.45GHz"), &[plate(0.012, 0.04)]);
        let (lo, hi) = (1.6e9, 3.2e9);
        let fix = retune(&p, "L", 2.45e9, 2.4e9, 2.5e9, 0, 5, |q| net(q, lo, hi)).unwrap();
        println!("{fix:?}");
        let Fix::Retune { from, to, band_s11_db, .. } = fix else { panic!() };
        assert_ne!(from, to);
        let before = band_worst(&net(&p, lo, hi).unwrap(), 0, 0, 2.4e9, 2.5e9).unwrap();
        assert!(band_s11_db < before, "retune must help: {band_s11_db} vs {before}");
    }

    #[test]
    fn a_matching_network_is_offered_when_it_can_work() {
        // A series RL load across a band: 25 + j40 Ω.
        let f: Vec<f64> = (0..41).map(|i| 2.4e9 + 2.5e6 * i as f64).collect();
        let s = f.iter().map(|_| {
            let z = C64::new(25.0, 40.0);
            vec![vec![(z - 50.0) / (z + 50.0)]]
        }).collect();
        let n = Network { f, z0: vec![50.0], s };
        assert!(db20(n.s[0][0][0].norm()) > -10.0);
        let Some(Fix::Match { band_s11_db, components, .. }) = match_fix(&n, 0, 2.4e9, 2.5e9) else { panic!("no fix") };
        assert!(band_s11_db <= -10.0);
        assert!(!components.is_empty());
    }

    #[test]
    fn parts_are_named_in_the_right_units() {
        use matching::{Element, Kind, Network};
        let net = Network { name: "t".into(), elements: vec![Element { kind: Kind::ShuntL, value: 5e-9, q: None }, Element { kind: Kind::SeriesC, value: 2e-12, q: None }] };
        assert_eq!(describe(&net), vec!["Shunt 5.00 nH".to_string(), "Series 2.00 pF".to_string()]);
    }

    #[test]
    fn shell_makes_six_walls_clear_of_the_antenna() {
        let p = strip("2.45GHz");
        let walls = shell_around(&p, "ABS plastic", 0.002, 0.002).unwrap();
        assert_eq!(walls.len(), 6);
        check_parts(&p, &walls).unwrap();
        let q = with_parts(&p, &walls);
        let (lo0, hi0) = p.geometry().unwrap().bounds();
        let (lo1, hi1) = q.geometry().unwrap().bounds();
        for k in 0..3 {
            assert!((lo0[k] - lo1[k] - 0.004).abs() < 1e-6 && (hi1[k] - hi0[k] - 0.004).abs() < 1e-6);
        }
        assert!(check_parts(&p, &[Part { name: "bad".into(), material: "PEC".into(), min: ["nope".into(), "0".into(), "0".into()], max: ["0".into(), "0".into(), "0".into()] }]).is_err());
    }

    #[test]
    fn keep_out_finds_the_first_safe_distance() {
        let p = strip("2.45GHz");
        let (lo, hi) = (1.6e9, 3.2e9);
        let fix = keep_out(&p, &[0.01, 0.03, 0.06, 0.12], 3.0, 0.5, 0, 2.4e9, 2.5e9, |d| vec![plate(d, 0.04)], |q| net(q, lo, hi)).unwrap();
        println!("{fix:?}");
        let Some(Fix::KeepOut { distance_m, shift_pct }) = fix else { panic!("{fix:?}") };
        assert!(distance_m > 0.01, "{distance_m}");
        assert!(shift_pct.abs() <= 3.0);
    }
}
