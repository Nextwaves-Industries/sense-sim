//! A building not built yet: nobody has decided the glazing or the insulation, and
//! those can swing coverage by more than any antenna. Each undecided material becomes
//! a range in dB; the answer says how far coverage could fall and which decision
//! matters most, while it can still be changed.

use bevy::prelude::*;
use proof::{Claim, Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust_mut};
use serde::{Deserialize, Serialize};

use crate::rtls::planning::{self, Coverage, CoverageRequest, Tech};
use crate::rtls::radio::{Material, MaterialCal};

/// One undecided material: from the kindest option to the harshest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialRange {
    pub material: String,
    /// Loss through one typical piece, dB.
    pub low_db: f32,
    pub high_db: f32,
    pub note: String,
}

/// Ranges for sub-7 GHz. Vendor figures vary widely, so these are starting points:
/// replace them with the actual product's data once chosen.
pub fn defaults() -> Vec<MaterialRange> {
    vec![
        MaterialRange { material: "glass".into(), low_db: 2.0, high_db: 30.0, note: "Plain glass to metal-coated low-E glazing".into() },
        MaterialRange { material: "concrete".into(), low_db: 10.0, high_db: 30.0, note: "Thin block wall to thick reinforced concrete".into() },
        MaterialRange { material: "generic".into(), low_db: 3.0, high_db: 20.0, note: "Plasterboard to foil-backed insulation".into() },
    ]
}

pub fn material_named(name: &str) -> Option<Material> {
    Material::ALL.iter().copied().find(|m| m.name().eq_ignore_ascii_case(name)).or(match name.to_ascii_lowercase().as_str() {
        "generic" => Some(Material::Generic),
        _ => None,
    })
}

/// The scale that makes a crossing of `typical_weight` walls cost `db`, given the
/// profile's dB per typical wall. A crossing costs dB per wall × its weight × the scale,
/// and BIM weights grow with thickness, so the weight must be what's really in the scene:
/// taking a stated 30 dB as a thin wall's made a 30 cm one cost 90 dB.
pub fn scale_for(db: f32, wall_db: f32, typical_weight: f32) -> f32 {
    db / (wall_db.max(0.1) * typical_weight.max(0.01))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecReport {
    pub report: RobustReport,
    pub claim: Claim,
    /// The decision that moves coverage the most, in words.
    pub decide_first: String,
    pub skipped: Vec<String>,
}

/// Runs coverage across the ranges. Leaves the model as it found it.
/// What counts as covered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpecMetric {
    /// The strongest signal is at least this level (dBm): does a radio reach this spot at all?
    Reach { min_dbm: f32 },
    /// Enough radios are heard to solve a position.
    Fix,
}

impl SpecMetric {
    fn label(self) -> &'static str {
        match self {
            SpecMetric::Reach { .. } => "Floor a signal reaches",
            SpecMetric::Fix => "Floor with a position fix",
        }
    }

    fn of(self, c: &Coverage) -> f64 {
        match self {
            SpecMetric::Reach { min_dbm } => {
                if c.cells.is_empty() {
                    return 0.0;
                }
                c.cells.iter().filter(|x| x.signal >= min_dbm).count() as f64 / c.cells.len() as f64
            }
            SpecMetric::Fix => {
                if c.cells.is_empty() {
                    return 0.0;
                }
                // One radio heard gives a "near it" placeholder, not a position.
                c.cells.iter().filter(|x| x.err.is_finite() && x.err != planning::PROXIMITY_ERR_M).count() as f64 / c.cells.len() as f64
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(world: &mut World, tech: Tech, ranges: &[MaterialRange], height: f32, cell: f32, min_coverage: f64, samples: usize, wall_db: f32, metric: SpecMetric, scene: &str) -> Result<SpecReport, String> {
    let mut used: Vec<(Material, MaterialRange)> = Vec::new();
    let mut skipped = Vec::new();
    for r in ranges {
        match material_named(&r.material) {
            Some(m) => used.push((m, r.clone())),
            None => skipped.push(r.material.clone()),
        }
    }
    if used.is_empty() {
        return Err("no known materials in the ranges (thin, wood, glass, object, concrete, metal, liquid)".into());
    }
    let saved: Vec<(Material, Option<MaterialCal>)> = used.iter().map(|(m, _)| (*m, MaterialCal::applied(*m))).collect();
    let typical = world.run_system_cached_with(super::erces::typical_weights_system, used.iter().map(|u| u.0).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    let factors: Vec<Factor> = used.iter().map(|(m, r)| Factor::new(&format!("{} loss", m.name()), "dB", Dist::Uniform { lo: r.low_db as f64, hi: r.high_db as f64 }, &r.note)).collect();
    let mut evals = 0;
    let target = Target::at_least(min_coverage);
    let report = robust_mut(metric.label(), "share", &factors, Some(target), Evidence::Estimate, RobustOptions { samples: samples.max(4), seed: 61 }, true, |v| {
        evals += 1;
        for (((m, _), db), weight) in used.iter().zip(v).zip(&typical) {
            MaterialCal::set(*m, Some(MaterialCal { loss_scale: scale_for(*db as f32, wall_db, *weight), bias_scale: 1.0, label: "Material range".into(), source: Default::default() }));
        }
        if world.run_system_cached_with(planning::compute, CoverageRequest { tech, height, cell }).is_err() {
            return f64::NAN;
        }
        metric.of(world.resource::<Coverage>())
    });
    for (m, c) in saved {
        MaterialCal::set(m, c);
    }
    // Leave the map showing the scene as it is.
    let _ = world.run_system_cached_with(planning::compute, CoverageRequest { tech, height, cell });
    let decide_first = report.main_driver().map_or_else(String::new, |s| {
        if s.span < 0.005 {
            "None of these materials changes coverage much here.".to_string()
        } else {
            format!("Decide {} first: across its range, coverage moves from {:.0} % to {:.0} %.", s.factor, s.at_low.min(s.at_high) * 100.0, s.at_low.max(s.at_high) * 100.0)
        }
    });
    let claim = Claim::from_report(
        &format!("building_coverage:{scene}"),
        "material_ranges",
        &format!("Coverage with materials undecided, {scene}"),
        &report,
        Some(target),
        vec!["Material losses are ranges from typical values, not the chosen products".into(), "A stated loss is for a wall like the median one of that material here; thicker ones cost proportionally more".into(), format!("{evals} coverage maps; furniture and stock are as placed in the scene")],
    );
    Ok(SpecReport { report, claim, decide_first, skipped })
}

/// Radio material for a BIM element, from its material layers, its name and its
/// class. A wall lists every layer, so what decides is what dominates it: masonry
/// wins over the plaster on it, and a stud partition is mostly plasterboard, however
/// metal its studs are.
pub fn material_for_element(kind: &ifc::ElementKind, material: Option<&str>, name: &str) -> Material {
    let text = format!("{} {}", material.unwrap_or(""), name).to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    use ifc::ElementKind as K;
    if matches!(kind, K::Window) || has(&["glass", "glaz", "low-e", "low e"]) {
        return Material::Glass;
    }
    if has(&["concrete", "beton", "brick", "masonry", "cement", "stone", "block", "cmu"]) {
        return Material::Concrete;
    }
    if has(&["stud", "partition", "plaster", "gypsum", "drywall", "insulat", "mineral", "foam"]) {
        return Material::Generic;
    }
    if has(&["wood", "timber", "plywood", "osb"]) {
        return Material::Wood;
    }
    if has(&["steel", "metal", "alumin", "iron", "zinc", "copper"]) {
        return Material::Metal;
    }
    match kind {
        K::Window => Material::Glass,
        K::Door => Material::Wood,
        K::Beam => Material::Metal,
        K::Column | K::Slab | K::Roof => Material::Concrete,
        _ => Material::Generic,
    }
}

/// One upright box to place, in IFC axes (Z up), with what radio sees it as.
#[derive(Clone, Debug, PartialEq)]
pub struct BimBox {
    pub name: String,
    pub kind: ifc::ElementKind,
    pub center: [f64; 3],
    /// Along its own X, along its own Y, height.
    pub size: [f64; 3],
    pub yaw: f64,
    pub material: (Material, f32),
}

/// The radio material of a BIM element, and how many walls one crossing is worth.
fn radio_of(e: &ifc::Element) -> (Material, f32) {
    let m = material_for_element(&e.kind, e.material.as_deref(), &e.name);
    let thick = e.thickness_m.unwrap_or(0.1) as f32;
    (m, if m == Material::Concrete { m.weight() * (thick / 0.1).max(0.5) } else { m.weight() })
}

/// The building as boxes, ready to place. BIM keeps a wall whole and its windows as
/// separate overlapping objects, so a ray through a window would pay for the wall as
/// well and the glazing decision would look like it didn't matter. Walls are cut
/// around their doors and windows here. Trees, markers and other proxies stay out
/// unless asked for: they say nothing about radio and inflate the building's extent.
pub fn boxes_from_building(b: &ifc::Building, skip: &[String], include_other: bool) -> Vec<BimBox> {
    use ifc::ElementKind as K;
    let kind_name = |k: &K| format!("{k:?}").to_lowercase();
    let kept: Vec<&ifc::Element> = b.elements.iter().filter(|e| (include_other || e.kind != K::Other) && !skip.iter().any(|s| s.eq_ignore_ascii_case(&kind_name(&e.kind)))).collect();
    let openings: Vec<&ifc::Element> = kept.iter().copied().filter(|e| matches!(e.kind, K::Door | K::Window)).collect();
    let mut out = Vec::with_capacity(kept.len());
    for (k, e) in kept.iter().enumerate() {
        // Their own names: generated walls are called wall_1, wall_2 and so on.
        let name = format!("bim_{}_{k}", kind_name(&e.kind));
        let material = radio_of(e);
        let whole = |name: String| BimBox { name, kind: e.kind, center: e.center, size: e.size, yaw: e.yaw_rad, material };
        if e.kind != K::Wall {
            out.push(whole(name));
            continue;
        }
        let (c, s) = (e.yaw_rad.cos(), e.yaw_rad.sin());
        let (half_l, half_t, half_h) = (e.size[0] / 2.0, e.size[1] / 2.0, e.size[2] / 2.0);
        // Openings in this wall, as (from, to, bottom, top) in the wall's own frame.
        let mut cuts: Vec<(f64, f64, f64, f64)> = openings
            .iter()
            .filter_map(|o| {
                let d = [o.center[0] - e.center[0], o.center[1] - e.center[1], o.center[2] - e.center[2]];
                let (x, y) = (d[0] * c + d[1] * s, -d[0] * s + d[1] * c);
                let delta = o.yaw_rad - e.yaw_rad;
                // Its reach along the wall, whichever way its own box is turned.
                let reach = (delta.cos().abs() * o.size[0] + delta.sin().abs() * o.size[1]) / 2.0;
                let inside = x.abs() < half_l && y.abs() <= half_t + 0.15 && d[2].abs() < half_h + o.size[2] / 2.0 && reach < half_l + 0.5;
                inside.then(|| ((x - reach).max(-half_l), (x + reach).min(half_l), (d[2] - o.size[2] / 2.0).max(-half_h), (d[2] + o.size[2] / 2.0).min(half_h)))
            })
            .collect();
        if cuts.is_empty() {
            out.push(whole(name));
            continue;
        }
        cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Overlapping openings count as one.
        let mut merged: Vec<(f64, f64, f64, f64)> = Vec::new();
        for o in cuts {
            match merged.last_mut() {
                Some(m) if o.0 <= m.1 => {
                    m.1 = m.1.max(o.1);
                    m.2 = m.2.min(o.2);
                    m.3 = m.3.max(o.3);
                }
                _ => merged.push(o),
            }
        }
        const SLIVER: f64 = 0.02;
        let mut part = 0;
        let mut piece = |from: f64, to: f64, bottom: f64, top: f64, out: &mut Vec<BimBox>| {
            if to - from < SLIVER || top - bottom < SLIVER {
                return;
            }
            let (mx, mz) = ((from + to) / 2.0, (bottom + top) / 2.0);
            out.push(BimBox { name: format!("{name}_{part}"), kind: K::Wall, center: [e.center[0] + mx * c, e.center[1] + mx * s, e.center[2] + mz], size: [to - from, e.size[1], top - bottom], yaw: e.yaw_rad, material });
            part += 1;
        };
        let mut at = -half_l;
        for (from, to, bottom, top) in &merged {
            piece(at, *from, -half_h, half_h, &mut out);
            piece(*from, *to, -half_h, *bottom, &mut out);
            piece(*from, *to, *top, half_h, &mut out);
            at = *to;
        }
        piece(at, half_l, -half_h, half_h, &mut out);
    }
    out
}

/// The height people walk in at, which becomes the scene's floor: the lowest door,
/// else the lowest wall. Not the lowest thing in the file: foundations and piles go
/// metres underground, and building up from there would bury the ground floor.
pub fn ground_level(boxes: &[BimBox]) -> f64 {
    use ifc::ElementKind as K;
    let lowest = |kinds: &[K]| boxes.iter().filter(|b| kinds.contains(&b.kind)).map(|b| b.center[2] - b.size[2] / 2.0).reduce(f64::min);
    lowest(&[K::Door]).or_else(|| lowest(&[K::Wall])).or_else(|| boxes.iter().map(|b| b.center[2] - b.size[2] / 2.0).reduce(f64::min)).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(kind: ifc::ElementKind, center: [f64; 3], size: [f64; 3], yaw: f64) -> ifc::Element {
        ifc::Element { id: String::new(), name: String::new(), kind, storey: None, material: None, min: [center[0] - size[0], center[1] - size[1], center[2] - size[2] / 2.0], max: [center[0] + size[0], center[1] + size[1], center[2] + size[2] / 2.0], center, size, yaw_rad: yaw, thickness_m: Some(size[1]), exact: true }
    }

    fn building(elements: Vec<ifc::Element>) -> ifc::Building {
        ifc::Building { name: "t".into(), unit_to_m: 1.0, elements, skipped: Vec::new() }
    }

    #[test]
    fn a_wall_is_cut_around_its_window_with_nothing_lost_or_doubled() {
        use ifc::ElementKind as K;
        // A 10 m wall, 3 m tall, centred at z 1.5, with a 2 m wide window from 1 m to 2.2 m up.
        let b = building(vec![element(K::Wall, [0.0, 0.0, 1.5], [10.0, 0.2, 3.0], 0.0), element(K::Window, [1.0, 0.0, 1.6], [2.0, 0.1, 1.2], 0.0)]);
        let boxes = boxes_from_building(&b, &[], false);
        let walls: Vec<&BimBox> = boxes.iter().filter(|x| x.kind == K::Wall).collect();
        assert_eq!(walls.len(), 4, "left, below, above, right: {walls:#?}");
        let area: f64 = walls.iter().map(|w| w.size[0] * w.size[2]).sum();
        assert!((area - (30.0 - 2.0 * 1.2)).abs() < 1e-9, "{area}");
        // A ray through the middle of the window finds no wall there.
        let hit = walls.iter().any(|w| (w.center[0] - 1.0).abs() < w.size[0] / 2.0 && (w.center[2] - 1.6).abs() < w.size[2] / 2.0);
        assert!(!hit, "{walls:#?}");
        assert_eq!(boxes.iter().filter(|x| x.kind == K::Window).count(), 1);
        assert_eq!(boxes.iter().find(|x| x.kind == K::Window).unwrap().material.0, Material::Glass);
    }

    #[test]
    fn the_floor_is_where_people_walk_in_not_the_bottom_of_the_foundations() {
        use ifc::ElementKind as K;
        let boxes = |list: Vec<ifc::Element>| boxes_from_building(&building(list), &[], false);
        let foundation = element(K::Slab, [0.0, 0.0, -1.5], [10.0, 10.0, 0.5], 0.0);
        let wall = element(K::Wall, [0.0, 5.0, 1.5], [10.0, 0.2, 3.0], 0.0);
        let door = element(K::Door, [8.0, 0.0, 1.0], [1.0, 0.1, 2.0], 0.0);
        assert_eq!(ground_level(&boxes(vec![foundation.clone(), wall.clone(), door])), 0.0);
        assert_eq!(ground_level(&boxes(vec![foundation.clone(), wall])), 0.0, "no door: the lowest wall");
        assert_eq!(ground_level(&boxes(vec![foundation])), -1.75, "nothing else to go on");
        assert_eq!(ground_level(&[]), 0.0);
    }

    #[test]
    fn cuts_follow_a_turned_wall() {
        use ifc::ElementKind as K;
        let yaw = std::f64::consts::FRAC_PI_2;
        // The wall runs along +y; the door is 2 m along it, in a box turned the same way.
        let b = building(vec![element(K::Wall, [5.0, 0.0, 1.5], [10.0, 0.2, 3.0], yaw), element(K::Door, [5.0, 2.0, 1.0], [1.0, 0.1, 2.0], yaw)]);
        let boxes = boxes_from_building(&b, &[], false);
        let walls: Vec<&BimBox> = boxes.iter().filter(|x| x.kind == K::Wall).collect();
        assert_eq!(walls.len(), 3, "either side and above the door: {walls:#?}");
        for w in &walls {
            assert!((w.center[0] - 5.0).abs() < 1e-9, "stays on the wall's line: {w:?}");
            assert_eq!(w.yaw, yaw);
        }
    }

    #[test]
    fn a_window_in_another_wall_cuts_nothing_and_proxies_stay_out() {
        use ifc::ElementKind as K;
        let b = building(vec![element(K::Wall, [0.0, 0.0, 1.5], [10.0, 0.2, 3.0], 0.0), element(K::Window, [0.0, 8.0, 1.5], [2.0, 0.1, 1.2], 0.0), element(K::Other, [90.0, 90.0, 0.0], [1.0, 1.0, 1.0], 0.0), element(K::Roof, [0.0, 0.0, 3.0], [10.0, 10.0, 0.2], 0.0)]);
        let boxes = boxes_from_building(&b, &[], false);
        assert_eq!(boxes.iter().filter(|x| x.kind == K::Wall).count(), 1);
        assert!(!boxes.iter().any(|x| x.kind == K::Other));
        assert_eq!(boxes_from_building(&b, &[], true).iter().filter(|x| x.kind == K::Other).count(), 1);
        assert!(!boxes_from_building(&b, &["roof".to_string()], false).iter().any(|x| x.kind == K::Roof));
        let names: Vec<String> = boxes.iter().map(|x| x.name.clone()).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "{names:?}");
    }

    #[test]
    fn scale_makes_a_crossing_cost_the_asked_loss() {
        // Glass weighs 0.5 walls at 6 dB per wall: scale 1 is 3 dB.
        assert!((scale_for(3.0, 6.0, Material::Glass.weight()) - 1.0).abs() < 1e-6);
        assert!((scale_for(30.0, 6.0, Material::Glass.weight()) - 10.0).abs() < 1e-5);
    }

    // OUT-M14: with the material's own weight, a stated 30 dB cost a 30 cm wall 90 dB.
    #[test]
    fn a_stated_loss_is_what_a_thick_bim_wall_really_costs() {
        use ifc::ElementKind as K;
        let thick = radio_of(&element(K::Wall, [0.0, 0.0, 1.5], [10.0, 0.3, 3.0], 0.0));
        assert_eq!(thick.0, Material::Concrete, "{thick:?}");
        let cost = |scale: f32| 6.0 * thick.1 * scale;
        assert!((cost(scale_for(30.0, 6.0, Material::Concrete.weight())) - 90.0).abs() < 0.01, "what the old scale did");
        assert!((cost(scale_for(30.0, 6.0, thick.1)) - 30.0).abs() < 0.01);
    }

    #[test]
    fn names_resolve_and_defaults_are_known() {
        assert_eq!(material_named("GLASS"), Some(Material::Glass));
        assert_eq!(material_named("generic"), Some(Material::Generic));
        assert_eq!(material_named("unobtainium"), None);
        for r in defaults() {
            assert!(material_named(&r.material).is_some(), "{}", r.material);
            assert!(r.low_db < r.high_db);
        }
    }

    #[test]
    fn bim_materials_map_to_radio_materials() {
        use ifc::ElementKind as K;
        assert_eq!(material_for_element(&K::Wall, Some("Reinforced Concrete C30"), ""), Material::Concrete);
        assert_eq!(material_for_element(&K::Window, None, ""), Material::Glass);
        assert_eq!(material_for_element(&K::Window, Some("Aluminium frame"), ""), Material::Glass);
        assert_eq!(material_for_element(&K::Wall, Some("Gypsum board / mineral wool"), ""), Material::Generic);
        assert_eq!(material_for_element(&K::Column, Some("S355 Steel"), ""), Material::Metal);
        assert_eq!(material_for_element(&K::Wall, None, ""), Material::Generic);
        // Names from a real Revit export, whose layers list metal studs.
        assert_eq!(material_for_element(&K::Wall, Some("Metal - Stud Layer; Gypsum Wall Board"), "Basic Wall:Interior - Partition (92mm Stud):138584"), Material::Generic);
        assert_eq!(material_for_element(&K::Wall, Some("Metal - Stud Layer; Brick"), "Basic Wall:Exterior - Brick on Block:138062"), Material::Concrete);
        assert_eq!(material_for_element(&K::Wall, None, "Party Wall - CMU Residential Unit"), Material::Concrete);
    }
}
