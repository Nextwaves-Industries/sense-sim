//! Emergency-responder radio coverage, before the building exists. Many fire codes ask
//! for a passing signal in 95 % of general areas and 99 % of critical areas, often floor
//! by floor, and it's normally only proven by a test after construction, when a miss
//! means an in-building booster nobody budgeted for.
//!
//! The signal arrives from outside, through the walls and windows: every spot on a floor
//! takes the kindest way in, from what the facade is made of and what stands between.
//! This is an early warning to size the risk and pick materials. The official test stays
//! the only proof, and local rules set the real numbers.

use bevy::prelude::*;
use proof::{Claim, Dist, Evidence, Factor, RobustOptions, Runs, Target, robust_mut};
use serde::{Deserialize, Serialize};

use super::spec::{MaterialRange, material_named};
use crate::agent::structures::Kind;
use crate::rtls::device::{Device, Zone};
use crate::rtls::radio::{self, Material, MaterialCal};
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::scene::PlacedObject;

/// Loss per metre indoors beyond the walls crossed, dB/m: 3GPP TR 38.901's
/// outdoor-to-indoor figure. Walls are counted too, so this leans cautious.
const INDOOR_DB_PER_M: f32 = 0.5;
/// Ways in that a spot gets to choose from.
const AZIMUTHS: usize = 12;

/// What the local rules ask for. These defaults are the commonly adopted figures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    pub threshold_dbm: f32,
    pub general_share: f64,
    pub critical_share: f64,
}

impl Default for Rules {
    fn default() -> Self {
        Self { threshold_dbm: -95.0, general_share: 0.95, critical_share: 0.99 }
    }
}

/// Typical losses in the public-safety bands. Ranges on purpose: nothing's chosen yet.
pub fn default_ranges() -> Vec<MaterialRange> {
    vec![
        MaterialRange { material: "glass".into(), low_db: 2.0, high_db: 30.0, note: "Plain glass to metal-coated low-E glazing".into() },
        MaterialRange { material: "concrete".into(), low_db: 8.0, high_db: 25.0, note: "Block wall to thick reinforced concrete".into() },
        MaterialRange { material: "generic".into(), low_db: 2.0, high_db: 10.0, note: "Plasterboard partition to foil-backed insulation".into() },
    ]
}

#[derive(Clone)]
pub struct EvalRequest {
    pub floors: Vec<f32>,
    pub cell: f32,
    pub outside_dbm: f32,
    pub wall_db: f32,
    pub threshold_dbm: f32,
    pub general_share: f64,
    pub critical_share: f64,
    pub critical_zones: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloorEval {
    pub height_m: f32,
    pub cells: usize,
    pub general_share: f64,
    pub critical_cells: usize,
    pub critical_share: Option<f64>,
    /// Signal needed outside for this floor to reach both shares, dBm.
    pub outside_needed_dbm: f64,
    /// Spots judged in each named critical zone, in the order they were named.
    pub zone_cells: Vec<usize>,
}

/// Floors in the scene: the tops of its slabs, clustered, with what stands on them.
pub fn floors_system(objects: Query<(&PlacedObject, &Transform), Without<Device>>) -> Vec<f32> {
    let mut tops: Vec<f32> = objects.iter().filter(|(o, _)| Kind::of_asset(&o.asset) == Some(Kind::Floor)).map(|(o, t)| crate::agent::tools::world_bounds(o, t).1.y).filter(|y| *y > -0.5).collect();
    tops.sort_by(f32::total_cmp);
    let mut floors: Vec<f32> = Vec::new();
    for y in tops {
        // Floors are at least a couple of metres apart; anything closer is the same one.
        if floors.last().is_none_or(|f| y - f > 2.0) {
            floors.push(y);
        }
    }
    if floors.is_empty() {
        floors.push(0.0);
    }
    floors
}

/// What a wall of each material is worth in this building, in typical walls: the middle one.
/// A range in dB means a wall like the ones here. Imported walls count more the thicker they
/// are, so taking a stated 25 dB as a 10 cm wall's would cost a 30 cm wall 75 dB.
pub fn typical_weights_system(In(materials): In<Vec<Material>>, objects: Query<&PlacedObject, Without<Device>>) -> Vec<f32> {
    materials
        .iter()
        .map(|m| {
            let mut w: Vec<f32> = objects.iter().map(|o| radio::object_material(&o.name).unwrap_or_else(|| radio::obstruction_of_asset(&o.asset))).filter(|(k, _)| k == m).map(|(_, v)| v).collect();
            w.sort_by(f32::total_cmp);
            w.get(w.len() / 2).copied().unwrap_or_else(|| m.weight()).max(0.01)
        })
        .collect()
}

/// Named critical zones that aren't in the scene.
pub fn missing_zones_system(In(names): In<Vec<String>>, zones: Query<&Zone>) -> Vec<String> {
    names.into_iter().filter(|n| !zones.iter().any(|z| z.name.eq_ignore_ascii_case(n))).collect()
}

/// Signal at every spot of every floor.
pub fn eval_system(In(req): In<EvalRequest>, parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>, zones: Query<&Zone>) -> Vec<FloorEval> {
    let occ = Occluders::build(&parts, &objects);
    let Some((lo, hi)) = occ.bounds() else { return Vec::new() };
    let reach = (hi - lo).length() + 5.0;
    let critical: Vec<(usize, &Zone)> = zones.iter().filter_map(|z| req.critical_zones.iter().position(|n| n.eq_ignore_ascii_case(&z.name)).map(|k| (k, z))).collect();
    let (nx, nz) = (((hi.x - lo.x) / req.cell).ceil().max(1.0) as usize, ((hi.z - lo.z) / req.cell).ceil().max(1.0) as usize);
    req.floors
        .iter()
        .map(|floor| {
            let y = *floor + 1.2;
            let mut losses: Vec<f64> = Vec::new();
            let mut crit: Vec<f64> = Vec::new();
            let mut zone_cells = vec![0usize; req.critical_zones.len()];
            for ix in 0..nx {
                for iz in 0..nz {
                    let p = Vec3::new(lo.x + (ix as f32 + 0.5) * req.cell, y, lo.z + (iz as f32 + 0.5) * req.cell);
                    // Indoors means walled in on all four sides.
                    let walled = [Dir3::X, Dir3::NEG_X, Dir3::Z, Dir3::NEG_Z].iter().all(|d| occ.nearest(Ray3d::new(p, *d), reach).is_some());
                    if !walled {
                        continue;
                    }
                    let mut best = f32::MAX;
                    for k in 0..AZIMUTHS {
                        let a = std::f32::consts::TAU * k as f32 / AZIMUTHS as f32;
                        let to = p + Vec3::new(a.cos(), 0.0, a.sin()) * reach;
                        let path = occ.path(p, to, None);
                        let indoors = occ.trace(p, to).last().map_or(0.0, |x| x.3);
                        best = best.min(req.wall_db * path.wall_eq + INDOOR_DB_PER_M * indoors);
                    }
                    losses.push(best as f64);
                    let mut in_critical = false;
                    for (k, z) in &critical {
                        if z.contains(p) {
                            zone_cells[*k] += 1;
                            in_critical = true;
                        }
                    }
                    if in_critical {
                        crit.push(best as f64);
                    }
                }
            }
            FloorEval { zone_cells, ..summarise(*floor, &losses, &crit, req.outside_dbm as f64, req.threshold_dbm as f64, req.general_share, req.critical_share) }
        })
        .collect()
}

/// The loss that `share` of the spots stay within. The spot itself, not a point between two:
/// between them, a signal that strong would let slightly too few through.
fn loss_for_share(sorted: &[f64], share: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((share * sorted.len() as f64 - 1e-9).ceil() as usize).clamp(1, sorted.len()) - 1]
}

/// One floor's numbers from the loss of each of its spots, and of those in critical areas.
pub fn summarise(height_m: f32, losses: &[f64], critical: &[f64], outside_dbm: f64, threshold_dbm: f64, share: f64, critical_share: f64) -> FloorEval {
    let ok = |v: &[f64]| v.iter().filter(|l| outside_dbm - **l >= threshold_dbm).count();
    let sorted = |v: &[f64]| {
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        s
    };
    let general_loss = loss_for_share(&sorted(losses), share);
    let critical_loss = loss_for_share(&sorted(critical), critical_share);
    FloorEval {
        height_m,
        cells: losses.len(),
        general_share: if losses.is_empty() { 0.0 } else { ok(losses) as f64 / losses.len() as f64 },
        critical_cells: critical.len(),
        critical_share: (!critical.is_empty()).then(|| ok(critical) as f64 / critical.len() as f64),
        outside_needed_dbm: threshold_dbm + if critical_loss.is_nan() { general_loss } else { general_loss.max(critical_loss) },
        zone_cells: Vec::new(),
    }
}

/// How close the building is to passing, as one number that passes at the general
/// share: the worst floor, with critical areas shifted to the same line.
pub fn worst_margin(floors: &[FloorEval], rules: &Rules) -> f64 {
    let mut worst = f64::INFINITY;
    for f in floors.iter().filter(|f| f.cells > 0) {
        worst = worst.min(f.general_share);
        if let Some(c) = f.critical_share {
            worst = worst.min(c - rules.critical_share + rules.general_share);
        }
    }
    if worst.is_finite() { worst } else { f64::NAN }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErcesReport {
    pub rules: Rules,
    pub outside_dbm: f32,
    pub claim: Claim,
    /// Each floor with materials at the middle of their ranges.
    pub floors: Vec<FloorEval>,
    /// Combinations tried and how many passed: a chance with no count behind it says too much.
    pub runs: Option<Runs>,
    pub decide_first: String,
    /// The strongest outdoor signal any floor needs at mid-range materials, dBm.
    pub outside_needed_dbm: f64,
    /// Levels with no walled-in spot to judge, such as a roof. They count for nothing either way.
    pub floors_not_judged: Vec<f32>,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct ErcesOptions {
    pub floors: Option<Vec<f32>>,
    pub cell: f32,
    pub outside_dbm: f32,
    pub outside_sigma_db: f32,
    pub rules: Rules,
    pub critical_zones: Vec<String>,
    pub ranges: Vec<MaterialRange>,
    pub samples: usize,
    pub wall_db: f32,
}

pub fn run(world: &mut World, o: ErcesOptions, scene: &str) -> Result<ErcesReport, String> {
    let floors = match o.floors.clone().filter(|f| !f.is_empty()) {
        Some(f) => f,
        None => world.run_system_cached(floors_system).map_err(|e| e.to_string())?,
    };
    if let Some(bad) = o.ranges.iter().find(|r| material_named(&r.material).is_none()) {
        return Err(format!("unknown material '{}' in the ranges: thin, wood, glass, object, concrete, metal, liquid", bad.material));
    }
    let used: Vec<(Material, MaterialRange)> = o.ranges.iter().filter_map(|r| material_named(&r.material).map(|m| (m, r.clone()))).collect();
    if used.is_empty() {
        return Err("no known materials in the ranges".into());
    }
    // Naming a critical area and not finding it must not read as a pass.
    let lost = world.run_system_cached_with(missing_zones_system, o.critical_zones.clone()).map_err(|e| e.to_string())?;
    if !lost.is_empty() {
        return Err(format!("no zone named {} in this scene, so it can't be judged as critical", lost.join(", ")));
    }
    let typical = world.run_system_cached_with(typical_weights_system, used.iter().map(|u| u.0).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    let saved: Vec<(Material, Option<MaterialCal>)> = used.iter().map(|(m, _)| (*m, MaterialCal::applied(*m))).collect();
    let wall_db = o.wall_db;
    let set = |values: &[f64]| {
        for (((m, _), db), w) in used.iter().zip(values).zip(&typical) {
            MaterialCal::set(*m, Some(MaterialCal { loss_scale: *db as f32 / (wall_db.max(0.1) * w), bias_scale: 1.0, label: "Material range".into(), source: Default::default() }));
        }
    };
    let eval = |world: &mut World, outside: f32| -> Vec<FloorEval> {
        let req = EvalRequest { floors: floors.clone(), cell: o.cell, outside_dbm: outside, wall_db, threshold_dbm: o.rules.threshold_dbm, general_share: o.rules.general_share, critical_share: o.rules.critical_share, critical_zones: o.critical_zones.clone() };
        world.run_system_cached_with(eval_system, req).unwrap_or_default()
    };

    let mid: Vec<f64> = used.iter().map(|(_, r)| ((r.low_db + r.high_db) / 2.0) as f64).collect();
    set(&mid);
    let nominal = eval(world, o.outside_dbm);
    if nominal.iter().all(|f| f.cells == 0) {
        for (m, c) in saved {
            MaterialCal::set(m, c);
        }
        return Err("no spot in this scene is walled in on all sides, so there's no indoors to judge. Import or build the building first.".into());
    }

    // A critical area nobody could judge must not read as one that passed.
    let unjudged: Vec<&str> = o.critical_zones.iter().enumerate().filter(|(k, _)| nominal.iter().map(|f| f.zone_cells.get(*k).copied().unwrap_or(0)).sum::<usize>() == 0).map(|(_, n)| n.as_str()).collect();
    if !unjudged.is_empty() {
        for (m, c) in saved {
            MaterialCal::set(m, c);
        }
        return Err(format!("no walled-in spot on the {} m grid falls inside {}, so it can't be judged: use a smaller cell", o.cell, unjudged.join(", ")));
    }

    let mut factors: Vec<Factor> = used.iter().map(|(m, r)| Factor::new(&format!("{} loss", m.name()), "dB", Dist::Uniform { lo: r.low_db as f64, hi: r.high_db as f64 }, &r.note)).collect();
    factors.push(Factor::new("Outdoor signal", "dBm", Dist::Normal { mean: o.outside_dbm as f64, sigma: o.outside_sigma_db as f64 }, "Depends on the site and on where the radio system's towers are: measure it"));
    let target = Target::at_least(o.rules.general_share);
    let report = robust_mut("Worst floor's coverage", "share", &factors, Some(target), Evidence::Estimate, RobustOptions { samples: o.samples.max(4), seed: 91 }, true, |v| {
        set(&v[..used.len()]);
        worst_margin(&eval(world, v[used.len()] as f32), &o.rules)
    });
    for (m, c) in saved {
        MaterialCal::set(m, c);
    }

    let chance = report.pass_probability.unwrap_or(0.0);
    let decide_first = report.main_driver().map_or_else(String::new, |s| {
        if s.span < 0.005 {
            "None of these changes the outcome much here.".to_string()
        } else {
            format!("{} matters most: across its range the worst floor moves from {:.0} % to {:.0} %.", s.factor, s.at_low.min(s.at_high) * 100.0, s.at_low.max(s.at_high) * 100.0)
        }
    });
    let floors_not_judged: Vec<f32> = nominal.iter().filter(|f| f.cells == 0).map(|f| f.height_m).collect();
    let needed = nominal.iter().filter(|f| f.cells > 0).map(|f| f.outside_needed_dbm).fold(f64::MIN, f64::max);
    let failing: Vec<String> = nominal.iter().filter(|f| f.cells > 0 && (f.general_share < o.rules.general_share || f.critical_share.is_some_and(|c| c < o.rules.critical_share))).map(|f| format!("{:.1} m ({:.0} %{})", f.height_m, f.general_share * 100.0, f.critical_share.map_or(String::new(), |c| format!(", critical areas {:.0} %", c * 100.0)))).collect();
    let summary = format!(
        "{} {} Floors short at mid-range materials: {}. To pass there it needs about {needed:.0} dBm outside.",
        report.runs().map_or_else(|| format!("About {:.0} % chance of passing without a booster.", chance * 100.0), |r| format!("{} of {} combinations pass without a booster, which shows at least {:.0} % at 95 % confidence.", r.passed, r.n, r.lower * 100.0)),
        decide_first,
        if failing.is_empty() { "none".to_string() } else { failing.join(", ") }
    );
    let claim = Claim::from_report(
        &format!("erces:{scene}"),
        "emergency_coverage",
        &format!("Emergency radio coverage without a booster, {scene}"),
        &report,
        Some(target),
        vec![
            "An early warning, not the code's acceptance test, which is the only proof. Local rules set the real thresholds and critical areas".into(),
            format!("{} dBm outside on all sides is assumed unless measured; towers and neighbouring buildings aren't modelled", o.outside_dbm),
            "Signal enters only through the walls and windows at each floor's level, and each element is an upright box".into(),
            "Each dB range is for a wall like the typical one of that material in this building; thicker ones cost more in proportion".into(),
            "Inbound only: the code also tests the radio's signal getting out, which depends on the radios and the system".into(),
        ],
    );
    Ok(ErcesReport { rules: o.rules, outside_dbm: o.outside_dbm, claim, floors: nominal, runs: report.runs(), decide_first, outside_needed_dbm: needed, floors_not_judged, summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_counts_the_spots_above_the_line() {
        // Outside −70, line −95: loss up to 25 dB passes.
        let f = summarise(0.0, &[10.0, 20.0, 25.0, 30.0], &[], -70.0, -95.0, 0.95, 0.99);
        assert_eq!(f.general_share, 0.75);
        assert_eq!(f.critical_share, None);
    }

    #[test]
    fn the_needed_outdoor_signal_is_what_lets_95_percent_through() {
        let losses: Vec<f64> = (0..=100).map(|v| v as f64).collect();
        let f = summarise(0.0, &losses, &[], -70.0, -95.0, 0.95, 0.99);
        assert!((f.outside_needed_dbm - (-95.0 + 95.0)).abs() < 1e-9, "{}", f.outside_needed_dbm);
        // Raising the outdoor signal to exactly that passes.
        let at = summarise(0.0, &losses, &[], f.outside_needed_dbm, -95.0, 0.95, 0.99);
        assert!(at.general_share >= 0.95, "{}", at.general_share);
    }

    #[test]
    fn the_needed_signal_really_passes_with_few_spots() {
        // Nine spots up to 8 dB and one at 30: 95 % of ten is all ten, so it takes the 30.
        let losses: Vec<f64> = (0..9).map(f64::from).chain([30.0]).collect();
        let f = summarise(0.0, &losses, &[], -70.0, -95.0, 0.95, 0.99);
        assert_eq!(f.outside_needed_dbm, -65.0);
        assert_eq!(summarise(0.0, &losses, &[], f.outside_needed_dbm, -95.0, 0.95, 0.99).general_share, 1.0);
    }

    #[test]
    fn a_critical_room_can_be_what_decides_the_signal_needed() {
        let losses = vec![20.0; 99];
        let f = summarise(0.0, &losses, &[50.0], -70.0, -95.0, 0.95, 0.99);
        assert_eq!(f.critical_share, Some(0.0));
        assert_eq!(f.outside_needed_dbm, -45.0);
    }

    #[test]
    fn the_worst_floor_decides_and_critical_areas_have_their_own_line() {
        let rules = Rules::default();
        let floor = |g: f64, c: Option<f64>| FloorEval { cells: 10, general_share: g, critical_share: c, critical_cells: c.map_or(0, |_| 5), ..Default::default() };
        assert_eq!(worst_margin(&[floor(0.99, None), floor(0.96, None)], &rules), 0.96);
        // 98 % in a critical area misses 99 %, even though it beats the general line.
        let m = worst_margin(&[floor(0.99, Some(0.98))], &rules);
        assert!(m < rules.general_share, "{m}");
        let m = worst_margin(&[floor(0.99, Some(1.0))], &rules);
        assert!(m >= rules.general_share, "{m}");
        // A floor with no indoor spots says nothing.
        let empty = FloorEval { cells: 0, ..Default::default() };
        assert_eq!(worst_margin(&[empty.clone(), floor(0.97, None)], &rules), 0.97);
        assert!(worst_margin(&[empty], &rules).is_nan());
    }

    #[test]
    fn the_default_rules_are_the_commonly_adopted_ones() {
        let r = Rules::default();
        assert_eq!((r.threshold_dbm, r.general_share, r.critical_share), (-95.0, 0.95, 0.99));
        for m in default_ranges() {
            assert!(material_named(&m.material).is_some(), "{}", m.material);
        }
    }
}
