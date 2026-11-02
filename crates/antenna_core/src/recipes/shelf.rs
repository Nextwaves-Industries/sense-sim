//! An RFID antenna on a real metal shelf, judged by what the shelf is for: read
//! every tag on this level, and none from the levels around it.
//!
//! The reader antenna and the shelf levels are solved together with the method of
//! moments, so the metal's effect on the antenna and the field is in the numbers,
//! not assumed. The field is then sampled where tags sit and turned into a read
//! margin. Products themselves aren't solved (metal-only solver): they enter as a
//! loss range, which is why this needs a real shelf test before it's a commitment.

use std::f64::consts::PI;

use anyhow::{Result, bail};
use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use crate::geometry::{Geometry, P3, PortDef, Shape, Solid};
use crate::mom::{MomOptions, MomProblem};
use crate::optimize::{self, Method, Options};
use crate::{C0, C64, ETA0};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShelfSpec {
    /// Across the front (x), metres.
    pub width: f64,
    /// Front to back (y), metres.
    pub depth: f64,
    /// Between levels (z), metres.
    pub gap: f64,
    /// Levels above and below the target to include.
    pub above: usize,
    pub below: usize,
    pub f: f64,
    /// Tag chip's read threshold, dBm.
    pub tag_sensitivity_dbm: f64,
    pub tag_gain_dbi: f64,
    /// Loss from products around the tag, dB, as nominal and spread.
    pub product_loss_db: (f64, f64),
    /// Reader power allowed at the antenna, dBm.
    pub tx_range_dbm: (f64, f64),
    /// Goals.
    pub min_read: f64,
    pub max_stray: f64,
}

impl Default for ShelfSpec {
    fn default() -> Self {
        Self {
            width: 0.9,
            depth: 0.45,
            gap: 0.4,
            above: 1,
            below: 1,
            f: 915e6,
            tag_sensitivity_dbm: -20.0,
            tag_gain_dbi: 1.0,
            product_loss_db: (3.0, 2.0),
            tx_range_dbm: (10.0, 30.0),
            min_read: 0.995,
            max_stray: 0.01,
        }
    }
}

/// A patch using the target level as its ground, its centre at (`x`, `y`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShelfAntenna {
    /// Resonant length, along y.
    pub l: f64,
    /// Width, along x.
    pub w: f64,
    /// Height above the shelf.
    pub h: f64,
    pub x: f64,
    pub y: f64,
    /// Probe offset from the patch centre, along y.
    pub yf: f64,
    /// A metal rim around the target level, this tall, to hold the field in. 0: none.
    pub lip: f64,
}

impl ShelfAntenna {
    /// A starting point: a low patch in the middle of the shelf.
    pub fn start(spec: &ShelfSpec) -> Self {
        let lambda = C0 / spec.f;
        let l = 0.46 * lambda;
        Self { l, w: 0.6 * lambda, h: 0.025 * lambda, x: 0.0, y: 0.0, yf: 0.22 * l, lip: 0.0 }
    }

    pub const NAMES: [&'static str; 6] = ["l", "w", "h", "x", "y", "lip"];

    pub fn to_vec(&self) -> Vec<f64> {
        vec![self.l, self.w, self.h, self.x, self.y, self.lip]
    }

    pub fn from_vec(&self, v: &[f64]) -> Self {
        let l = v[0];
        Self { l, w: v[1], h: v[2], x: v[3], y: v[4], yf: 0.22 * l, lip: v[5] }
    }
}

fn sheet(name: &str, min: P3, max: P3) -> Solid {
    Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min, max } }
}

/// The shelf levels and the antenna. The target level is at z = 0.
pub fn geometry(spec: &ShelfSpec, a: &ShelfAntenna) -> Geometry {
    let (wx, wy) = (spec.width / 2.0, spec.depth / 2.0);
    let mut solids = vec![sheet("target level", [-wx, -wy, 0.0], [wx, wy, 0.0])];
    for k in 1..=spec.above {
        let z = spec.gap * k as f64;
        solids.push(sheet(&format!("level +{k}"), [-wx, -wy, z], [wx, wy, z]));
    }
    for k in 1..=spec.below {
        let z = -spec.gap * k as f64;
        solids.push(sheet(&format!("level -{k}"), [-wx, -wy, z], [wx, wy, z]));
    }
    solids.push(sheet("patch", [a.x - a.w / 2.0, a.y - a.l / 2.0, a.h], [a.x + a.w / 2.0, a.y + a.l / 2.0, a.h]));
    if a.lip > 1e-3 {
        solids.push(sheet("rim front", [-wx, -wy, 0.0], [wx, -wy, a.lip]));
        solids.push(sheet("rim back", [-wx, wy, 0.0], [wx, wy, a.lip]));
        solids.push(sheet("rim left", [-wx, -wy, 0.0], [-wx, wy, a.lip]));
        solids.push(sheet("rim right", [wx, -wy, 0.0], [wx, wy, a.lip]));
    }
    let lambda = C0 / spec.f;
    let feed = [a.x, a.y + a.yf];
    let ports = vec![PortDef { name: "probe".into(), a: [feed[0], feed[1], 0.0], b: [feed[0], feed[1], a.h], width: lambda / 80.0, z0: 50.0 }];
    Geometry { solids, ports, axis_forward: true, ..Default::default() }
}

/// Where tags sit relative to the levels: a grid in the space above the level at `z0`.
fn tag_points(spec: &ShelfSpec, z0: f64, keep_clear: f64) -> Vec<P3> {
    let (nx, ny, nz) = (9, 5, 3);
    let inset = 0.04;
    let mut out = Vec::with_capacity(nx * ny * nz);
    for ix in 0..nx {
        let x = -spec.width / 2.0 + inset + (spec.width - 2.0 * inset) * ix as f64 / (nx - 1) as f64;
        for iy in 0..ny {
            let y = -spec.depth / 2.0 + inset + (spec.depth - 2.0 * inset) * iy as f64 / (ny - 1) as f64;
            for iz in 0..nz {
                // Tags on goods stand from just above the shelf to most of the way up.
                let z = keep_clear + (0.8 * spec.gap - keep_clear) * iz as f64 / (nz - 1) as f64;
                out.push([x, y, z0 + z]);
            }
        }
    }
    out
}

/// How tags point: along each axis, and mid-way in the shelf plane.
const ORIENTATIONS: [P3; 5] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.7071, 0.7071, 0.0], [0.7071, -0.7071, 0.0]];

/// Power a tag can harvest at each point and orientation, for 0 dBm incident on
/// the reader port (dBm), before its own sensitivity and product losses.
fn harvest_db(field: &[[C64; 3]], spec: &ShelfSpec) -> Vec<f64> {
    let lambda = C0 / spec.f;
    let aperture = 10f64.powf(spec.tag_gain_dbi / 10.0) * lambda * lambda / (4.0 * PI);
    let mut out = Vec::with_capacity(field.len() * ORIENTATIONS.len());
    for e in field {
        for o in ORIENTATIONS {
            let along = e[0] * o[0] + e[1] * o[1] + e[2] * o[2];
            // The field is for 1 W incident: this is mW at the chip per mW incident.
            let watts = along.norm_sqr() / (2.0 * ETA0) * aperture;
            out.push(10.0 * watts.max(1e-30).log10());
        }
    }
    out
}

/// The solved field at the tag positions, as harvested power tables.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShelfField {
    /// Harvest (dB relative to the power incident on the port), per point and orientation.
    pub target: Vec<f64>,
    /// The same on the levels above and below.
    pub stray: Vec<f64>,
    /// |S11| at the design frequency, dB.
    pub s11_db: f64,
    pub unknowns: usize,
}

pub fn solve(spec: &ShelfSpec, a: &ShelfAntenna, edges_per_lambda: f64) -> Result<ShelfField> {
    if a.h <= 0.0 || a.l <= 0.0 || a.w <= 0.0 {
        bail!("the antenna needs a positive size and height");
    }
    let geom = geometry(spec, a);
    let opts = MomOptions { edges_per_lambda, theta_step: 30.0, phi_step: 30.0, ..Default::default() };
    let problem = MomProblem::new(&geom, spec.f, opts)?;
    // Keep tags a mesh edge off the metal, where the sampled field is reliable.
    let lambda = C0 / spec.f;
    let clear = (lambda / edges_per_lambda).max(a.h + 0.02);
    let target_pts = tag_points(spec, 0.0, clear);
    let mut stray_pts = Vec::new();
    for k in 1..=spec.above {
        stray_pts.extend(tag_points(spec, spec.gap * k as f64, lambda / edges_per_lambda));
    }
    for k in 1..=spec.below {
        stray_pts.extend(tag_points(spec, -spec.gap * k as f64, lambda / edges_per_lambda));
    }
    let f_target = problem.near_field(spec.f, 0, &target_pts)?;
    let f_stray = problem.near_field(spec.f, 0, &stray_pts)?;
    let pt = problem.solve(spec.f, false)?;
    let z = pt.z[0][0];
    let g = (z - 50.0) / (z + 50.0);
    Ok(ShelfField { target: harvest_db(&f_target, spec), stray: harvest_db(&f_stray, spec), s11_db: crate::db20(g.norm()), unknowns: problem.unknowns() })
}

fn share_above(values: &[f64], offset_db: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().filter(|v| *v + offset_db >= 0.0).count() as f64 / values.len() as f64
}

/// Read share on the target and stray share around it at this reader power,
/// nominal products and tag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShelfScore {
    pub tx_dbm: f64,
    pub read: f64,
    pub stray: f64,
}

/// Harvest offset for a reader power: incident power + harvest − losses − sensitivity.
fn margin_offset(tx_dbm: f64, sens_dbm: f64, loss_db: f64) -> f64 {
    tx_dbm - loss_db - sens_dbm
}

pub fn score_at(field: &ShelfField, spec: &ShelfSpec, tx_dbm: f64) -> ShelfScore {
    let o = margin_offset(tx_dbm, spec.tag_sensitivity_dbm, spec.product_loss_db.0);
    ShelfScore { tx_dbm, read: share_above(&field.target, o), stray: share_above(&field.stray, o) }
}

/// The reader power that best meets the goals: the lowest that reads enough, or
/// failing that, the best read share against stray.
pub fn best_power(field: &ShelfField, spec: &ShelfSpec) -> ShelfScore {
    let (lo, hi) = spec.tx_range_dbm;
    let steps = ((hi - lo) / 0.5).round().max(1.0) as usize;
    let mut best: Option<(f64, ShelfScore)> = None;
    for k in 0..=steps {
        let s = score_at(field, spec, lo + (hi - lo) * k as f64 / steps as f64);
        let p = penalty(&s, spec);
        if best.is_none_or(|b| p < b.0 - 1e-12) {
            best = Some((p, s));
        }
    }
    best.map_or_else(ShelfScore::default, |b| b.1)
}

/// 0 when both goals are met; in percentage points otherwise.
pub fn penalty(s: &ShelfScore, spec: &ShelfSpec) -> f64 {
    (spec.min_read - s.read).max(0.0) * 100.0 + (s.stray - spec.max_stray).max(0.0) * 100.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShelfDesign {
    pub start: ShelfAntenna,
    pub best: ShelfAntenna,
    pub before: ShelfScore,
    pub after: ShelfScore,
    pub s11_db: f64,
    pub evals: usize,
    pub unknowns: usize,
}

/// Searches the antenna's size, height, position and rim for the shelf's goals.
pub fn design(spec: &ShelfSpec, start: &ShelfAntenna, edges_per_lambda: f64, max_evals: usize) -> Result<ShelfDesign> {
    let eval = |a: &ShelfAntenna| -> Result<(ShelfField, ShelfScore)> {
        let f = solve(spec, a, edges_per_lambda)?;
        let s = best_power(&f, spec);
        Ok((f, s))
    };
    let (_, before) = eval(start)?;
    let lambda = C0 / spec.f;
    let bounds = vec![
        (0.3 * lambda, 0.6 * lambda),
        (0.2 * lambda, (0.9 * lambda).min(spec.width * 0.9)),
        (0.012 * lambda, 0.08 * lambda),
        (-spec.width / 4.0, spec.width / 4.0),
        (-spec.depth / 4.0, spec.depth / 4.0),
        (0.0, 0.5 * spec.gap),
    ];
    let outcome = optimize::minimize(
        |x: &[f64]| eval(&start.from_vec(x)).map_or(f64::NAN, |(_, s)| penalty(&s, spec) - s.read * 0.1 + s.stray * 0.1),
        &start.to_vec(),
        &bounds,
        &Options { method: Method::Surrogate, max_evals, seed: 1, population: Some(10.min(max_evals.max(4) / 2)), tol: 1e-3, parallel: 1 },
        |_| true,
    );
    let start_score = penalty(&before, spec) - before.read * 0.1 + before.stray * 0.1;
    let best = if outcome.value < start_score { start.from_vec(&outcome.x) } else { *start };
    let (f, after) = eval(&best)?;
    Ok(ShelfDesign { start: *start, best, before, after, s11_db: f.s11_db, evals: outcome.evals + 2, unknowns: f.unknowns })
}

/// A read rate and stray rate across the things we don't know, for one design at
/// one reader power. Returns (read report, stray report).
pub fn predict(field: &ShelfField, spec: &ShelfSpec, tx_dbm: f64, samples: usize) -> (RobustReport, RobustReport) {
    let factors = vec![
        Factor::new("Tag sensitivity", "dBm", Dist::Normal { mean: spec.tag_sensitivity_dbm, sigma: 1.5 }, "Spread between tag batches, assumed"),
        Factor::new("Tag gain on product", "dB", Dist::Normal { mean: 0.0, sigma: 1.5 }, "Assumed"),
        Factor::new("Product loss", "dB", Dist::Normal { mean: spec.product_loss_db.0, sigma: spec.product_loss_db.1 }, "Given for this shelf"),
        Factor::new("Reader power at antenna", "dBm", Dist::Normal { mean: tx_dbm, sigma: 0.5 }, "Reader and cable tolerance, assumed"),
    ];
    let model = |table: &[f64], v: &[f64]| share_above(table, v[3] - v[2] - v[0] + v[1]);
    let opts = RobustOptions { samples, seed: 11 };
    let read = robust("Read rate on this level", "share", &factors, Some(Target::at_least(spec.min_read)), Evidence::Estimate, opts, |v| model(&field.target, v));
    let stray = robust("Stray reads on nearby levels", "share", &factors, Some(Target::at_most(spec.max_stray)), Evidence::Estimate, opts, |v| model(&field.stray, v));
    (read, stray)
}

/// What a report on this recipe assumes, for the claim ledger.
pub fn assumptions() -> Vec<String> {
    vec![
        "Products enter as a loss range and aren't solved; liquids and foil can do far worse".into(),
        "Only metal is solved: shelf levels as thin plates, without uprights or brackets".into(),
        "A tag reads when its forward link closes; the reader's receive side and tag collisions aren't checked".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> ShelfSpec {
        ShelfSpec { width: 0.6, depth: 0.35, gap: 0.35, ..Default::default() }
    }

    #[test]
    fn the_shelf_cuts_the_field_on_the_next_level() {
        let spec = small();
        let a = ShelfAntenna::start(&spec);
        let f = solve(&spec, &a, 8.0).unwrap();
        println!("{} unknowns, S11 {:.1} dB", f.unknowns, f.s11_db);
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let (t, s) = (mean(&f.target), mean(&f.stray));
        println!("mean harvest target {t:.1} dB, stray {s:.1} dB");
        assert!(t > s + 3.0, "target {t} vs stray {s}");
        // More power reads more and strays more: it's a trade, and a real one.
        let low = score_at(&f, &spec, 10.0);
        let high = score_at(&f, &spec, 30.0);
        println!("10 dBm {low:?}\n30 dBm {high:?}");
        assert!(high.read >= low.read && high.stray >= low.stray);
        assert!(high.read > low.read, "power must matter");
    }

    #[test]
    fn best_power_prefers_the_lowest_that_meets_the_goals() {
        let spec = ShelfSpec::default();
        // A table where 20 dBm reads everything: harvest −40 → margin 0 at tx − 3 + 20 − 40 = 0, tx = 23.
        let field = ShelfField { target: vec![-40.0; 100], stray: vec![-90.0; 100], s11_db: -20.0, unknowns: 0 };
        let s = best_power(&field, &spec);
        assert!((s.tx_dbm - 23.0).abs() < 0.51, "{s:?}");
        assert_eq!(s.read, 1.0);
        assert_eq!(s.stray, 0.0);
    }

    #[test]
    fn prediction_gives_a_range_and_names_the_driver() {
        let spec = ShelfSpec::default();
        // A smooth spread of harvest values so the read share responds to the factors.
        let target: Vec<f64> = (0..500).map(|i| -60.0 + 30.0 * i as f64 / 500.0).collect();
        let stray: Vec<f64> = (0..500).map(|i| -95.0 + 30.0 * i as f64 / 500.0).collect();
        let field = ShelfField { target, stray, s11_db: -15.0, unknowns: 0 };
        let (read, stray) = predict(&field, &spec, 27.0, 300);
        assert!(read.prediction.low < read.prediction.high, "{:?}", read.prediction);
        assert_eq!(read.main_driver().unwrap().factor, "Product loss");
        assert_eq!(read.prediction.evidence, Evidence::Estimate);
        assert!(stray.prediction.median < read.prediction.median);
    }

    #[test]
    #[ignore = "real design loop, a few minutes: cargo test -p antenna_core shelf_design -- --ignored --nocapture"]
    fn shelf_design_beats_the_starting_patch() {
        let spec = small();
        let start = ShelfAntenna::start(&spec);
        let t = std::time::Instant::now();
        let d = design(&spec, &start, 8.0, 24).unwrap();
        println!("{d:#?}\nin {:.0} s", t.elapsed().as_secs_f64());
        assert!(penalty(&d.after, &spec) <= penalty(&d.before, &spec) + 1e-9);
    }
}
