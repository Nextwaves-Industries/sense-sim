//! Which RFID tag, placed where, on this product? A tag is a resonant antenna tuned
//! to its chip, and what's behind it decides how much of that survives.
//!
//! A nearby block acts mostly as a lossy mirror, not as something the antenna sits
//! inside. The mirror image collapses the antenna's resistance, which spoils the
//! match to the chip far more than the small shift in resonance does, and a lossy
//! block soaks up what's radiated into it. That picture comes from full-wave solves:
//! a half-wave strip at 915 MHz with water, muscle tissue and polycarbonate 2 to
//! 20 mm behind it (FDTD, `outcomes::hardware::fdtd_checks` in the app), and with
//! metal (method of moments, `tag_over_metal_reference` below). `matches_the_solved_table`
//! holds this estimate to those numbers.
//!
//! It remains a quick estimate for ranking options: a strip dipole in front of a
//! block is not a real inlay on a real package. Confirm on the real product.

use std::f64::consts::{PI, TAU};

use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use crate::C0;

/// What's inside the package, as far as a UHF tag can tell.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contents {
    pub id: &'static str,
    pub label: &'static str,
    /// Relative permittivity range at UHF.
    pub er: (f64, f64),
    /// Loss tangent range at UHF.
    pub tan_d: (f64, f64),
    /// The near side is a conductor (cans, foil).
    pub metal: bool,
}

/// Typical UHF values. Ranges are wide on purpose; a measured value should replace them.
pub const CONTENTS: [Contents; 8] = [
    Contents { id: "dry_goods", label: "Dry goods", er: (1.5, 4.0), tan_d: (0.01, 0.1), metal: false },
    Contents { id: "cardboard", label: "Cardboard only", er: (1.1, 1.5), tan_d: (0.02, 0.1), metal: false },
    Contents { id: "plastic", label: "Plastic items", er: (2.0, 3.5), tan_d: (0.001, 0.02), metal: false },
    Contents { id: "water_based", label: "Water-based (drinks, fresh meat)", er: (50.0, 78.0), tan_d: (0.05, 0.4), metal: false },
    Contents { id: "oily", label: "Oils and fats", er: (2.5, 5.0), tan_d: (0.02, 0.15), metal: false },
    Contents { id: "frozen", label: "Frozen food", er: (3.0, 12.0), tan_d: (0.02, 0.3), metal: false },
    Contents { id: "metal_can", label: "Metal cans", er: (1.0, 1.0), tan_d: (0.0, 0.0), metal: true },
    Contents { id: "foil_lined", label: "Foil-lined pack", er: (1.0, 1.0), tan_d: (0.0, 0.0), metal: true },
];

pub fn contents(id: &str) -> Option<&'static Contents> {
    CONTENTS.iter().find(|c| c.id == id)
}

/// A generic kind of tag, not any vendor's part.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagKind {
    pub id: &'static str,
    pub label: &'static str,
    /// Free-space resonant frequency as built.
    pub f_res: f64,
    /// Radiation resistance in free space, ohms.
    pub r_rad: f64,
    /// Antenna Q in free space; lower is broader and less sensitive to detuning.
    pub q: f64,
    /// Peak gain in free space, dBi.
    pub gain_dbi: f64,
    /// Built-in spacing between the antenna and what it's stuck to, metres.
    pub spacer: f64,
    /// Has its own ground plane, so what's behind it matters little.
    pub own_ground: bool,
}

pub const TAGS: [TagKind; 4] = [
    TagKind { id: "wet_inlay", label: "Standard wet inlay", f_res: 915e6, r_rad: 20.0, q: 10.0, gain_dbi: 1.5, spacer: 0.0002, own_ground: false },
    TagKind { id: "broad_inlay", label: "Broadband inlay, detuning tolerant", f_res: 915e6, r_rad: 25.0, q: 5.0, gain_dbi: 1.0, spacer: 0.0002, own_ground: false },
    TagKind { id: "foam_flag", label: "Foam-spaced label or flag", f_res: 915e6, r_rad: 20.0, q: 10.0, gain_dbi: 1.5, spacer: 0.006, own_ground: false },
    TagKind { id: "on_metal", label: "On-metal tag with its own ground", f_res: 915e6, r_rad: 6.0, q: 25.0, gain_dbi: -6.0, spacer: 0.003, own_ground: true },
];

pub fn tag(id: &str) -> Option<&'static TagKind> {
    TAGS.iter().find(|t| t.id == id)
}

/// Chip input: `r − j/(ωc)`, typical of current UHF chips.
const CHIP_R: f64 = 15.0;
const CHIP_C: f64 = 1.1e-12;
/// Chip's read threshold, dBm.
pub const CHIP_SENS_DBM: f64 = -22.0;

fn chip_x(f: f64) -> f64 {
    -1.0 / (TAU * f * CHIP_C)
}

/// One evaluation of a tag stuck on a product.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagState {
    /// Resonance after the shift, Hz.
    pub f_res: f64,
    /// The antenna's resistance at resonance, ohms.
    pub resistance: f64,
    /// Share of power reaching the chip (0..1).
    pub transfer: f64,
    /// Radiation efficiency on the product (0..1).
    pub efficiency: f64,
    /// Gain from the mirror effect, dB, on top of the free-space pattern.
    pub boost_db: f64,
    /// Read range, metres.
    pub range_m: f64,
}

/// What's behind a tag at a gap `d` (metres), for an antenna of radiation resistance
/// `r0` at wavelength `lambda`: (resonance factor, resistance, efficiency, boost dB).
fn behind(r0: f64, felt: f64, er: f64, tan_d: f64, metal: bool, shield: f64, d: f64, lambda: f64) -> (f64, f64, f64, f64) {
    let x = 2.0 * TAU / lambda * d;
    let sinc = if x < 1e-6 { 1.0 } else { x.sin() / x };
    // The mirror image: exact for a conductor, and within about 30 % of the solved
    // resistance for the dielectrics tried, using the reflection at normal incidence.
    let image = r0 * (1.0 - felt * sinc).max(1e-4);
    if metal {
        // The resonance stays within about 5 %; the gain rises about 5 dB out front.
        return (1.0, image, 1.0, 5.2 * felt);
    }
    // Fitted to the solved shifts. Loss weakens the loading: muscle shifts much less than water.
    let load = 0.91 / (1.0 + 6.0 * tan_d) * felt * (d / 0.002).powf(-0.79);
    let near = r0 * 1.62 * tan_d * (-d / 0.0166).exp() * shield;
    let lossiness = tan_d * er.max(1.0).sqrt();
    let reach = (80.0 * (lossiness / 0.44).max(1e-3).powf(-0.43)).min(150.0) * 1e-3;
    let absorbed = (1.0 - (-3.0 * lossiness).exp()) * (-d / reach).exp() * shield;
    let boost = (felt * felt * (1.0 + 180.0 * d)).min(5.2 * felt * felt);
    (1.0 / (1.0 + load).sqrt(), image + near, 1.0 - absorbed, boost)
}

/// `wall`: total gap to the contents besides the tag's own spacer (package wall,
/// label stock), metres. `eirp_dbm`: reader output with antenna gain.
pub fn evaluate(t: &TagKind, er: f64, tan_d: f64, metal: bool, wall: f64, f: f64, eirp_dbm: f64) -> TagState {
    let lambda = C0 / f;
    let d = (t.spacer + wall).max(1e-5);
    let refl = if metal { 1.0 } else { (er.max(1.0).sqrt() - 1.0) / (er.max(1.0).sqrt() + 1.0) };
    // With a ground plane of its own, the near field stays inside the tag.
    let shield = if t.own_ground { 0.1 } else { 1.0 };
    let (factor, resistance, absorbed_eff, boost_db) = behind(t.r_rad, refl * shield, er, tan_d, metal, shield, d, lambda);
    let boost_db = if metal && t.own_ground { 2.0 } else { boost_db };
    let f_res = t.f_res * factor;
    let r_cond = 0.5;
    let r_total = resistance + r_cond;
    let x_l_at_res = -chip_x(t.f_res);
    // The antenna is conjugate to the chip at its own resonance.
    let x_a = x_l_at_res + t.q * t.r_rad * (f / f_res - f_res / f);
    let efficiency = absorbed_eff * resistance / r_total;
    let x = x_a + chip_x(f);
    let denom = (r_total + CHIP_R).powi(2) + x * x;
    let transfer = (4.0 * r_total * CHIP_R / denom).min(1.0);
    // Friis on the forward link.
    let gain = 10f64.powf((t.gain_dbi + boost_db) / 10.0) * efficiency;
    let margin = 10f64.powf((eirp_dbm - CHIP_SENS_DBM) / 10.0);
    let range_m = lambda / (4.0 * PI) * (gain * transfer * margin).max(0.0).sqrt();
    TagState { f_res, resistance, transfer, efficiency, boost_db, range_m }
}

/// What the tag's stuck on, for a recommendation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub label: &'static str,
    /// Gap from the contents, metres: 0.5 mm on a thin pack, a few cm over a headspace.
    pub wall: (f64, f64),
}

pub const PLACEMENTS: [Placement; 3] = [
    Placement { label: "Directly on the contents (film or thin wall)", wall: (0.0002, 0.001) },
    Placement { label: "On a carton wall", wall: (0.002, 0.005) },
    Placement { label: "Over a gap (bottle neck, headspace, hang tag)", wall: (0.01, 0.04) },
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Option_ {
    pub tag: String,
    pub placement: String,
    pub report: RobustReport,
    /// Where this option is outside what the estimate was fitted to.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// What the solves behind the fit covered: gaps to the contents (metres), dielectric and
/// metal, and the free-space resistance of the strip that was solved (ohms).
const FIT_GAP_M: (f64, f64) = (0.002, 0.02);
const FIT_GAP_METAL_M: (f64, f64) = (0.003, 0.08);
const FIT_R_OHM: (f64, f64) = (50.0, 100.0);

/// Where a tag and placement leave the range the estimate was fitted over.
pub fn outside_fit(t: &TagKind, wall: (f64, f64), metal: bool) -> Vec<String> {
    let mut out = Vec::new();
    let fit = if metal { FIT_GAP_METAL_M } else { FIT_GAP_M };
    let gap = (t.spacer + wall.0, t.spacer + wall.1);
    if gap.0 < fit.0 || gap.1 > fit.1 {
        out.push(format!("Gap of {:.1} to {:.1} mm: the estimate was fitted from {:.0} to {:.0} mm, so this is an extrapolation", gap.0 * 1e3, gap.1 * 1e3, fit.0 * 1e3, fit.1 * 1e3));
    }
    if t.r_rad < FIT_R_OHM.0 || t.r_rad > FIT_R_OHM.1 {
        out.push(format!("Antenna resistance {:.0} Ω: the estimate was fitted to an 81 Ω strip, not this kind of tag", t.r_rad));
    }
    out
}

/// Spread of the estimate itself, on range (log-normal σ). Its own test allows ±8 %
/// resonance, ±40 % resistance and ±1 dB against the solves, about ±3 dB of power or
/// ±40 % of range, taken as 1.65 σ. Outside the fit nothing bounds it, so it's wider.
pub(super) fn model_error_sigma(outside: bool) -> f64 {
    if outside { 0.4 } else { 0.25 }
}

/// Ranks each tag and placement for the contents by chance of reaching
/// `target_range_m`, then by median range. `f` and `eirp_dbm`: 915e6 and 36 under
/// FCC rules; 866.5e6 and about 35 under ETSI.
pub fn recommend(c: &Contents, f: f64, eirp_dbm: f64, target_range_m: f64, samples: usize) -> Vec<Option_> {
    let mut out = Vec::new();
    // A circular reader antenna gives up 3 dB to a linear tag.
    let eirp_at_tag = eirp_dbm - 3.0;
    for t in &TAGS {
        for pl in &PLACEMENTS {
            let warnings = outside_fit(t, pl.wall, c.metal);
            let factors = vec![
                Factor::new("Contents permittivity", "", Dist::Uniform { lo: c.er.0, hi: c.er.1.max(c.er.0 + 1e-6) }, "Typical UHF range for this kind of product"),
                Factor::new("Contents loss tangent", "", Dist::Uniform { lo: c.tan_d.0, hi: c.tan_d.1.max(c.tan_d.0 + 1e-6) }, "Typical UHF range for this kind of product"),
                Factor::new("Gap to the contents", "m", Dist::Uniform { lo: pl.wall.0, hi: pl.wall.1 }, "From the placement"),
                Factor::new("Tag antenna tolerance", "", Dist::Normal { mean: 1.0, sigma: 0.01 }, "Printing and lamination, assumed"),
                Factor::new("Model error", "×", Dist::LogNormal { median: 1.0, sigma: model_error_sigma(!warnings.is_empty()) }, "How far the fit can be off: from its tolerances against the solves, wider outside them. Assumed, not measured"),
            ];
            let report = robust(&format!("Read range, {} {}", t.label, pl.label), "m", &factors, Some(Target::at_least(target_range_m)), Evidence::Estimate, RobustOptions { samples, seed: 21 }, |v| {
                let tuned = TagKind { f_res: t.f_res * v[3], ..*t };
                evaluate(&tuned, v[0], v[1], c.metal, v[2], f, eirp_at_tag).range_m * v[4]
            });
            out.push(Option_ { tag: t.label.to_string(), placement: pl.label.to_string(), report, warnings });
        }
    }
    out.sort_by(|a, b| {
        b.report.pass_probability.unwrap_or(0.0).total_cmp(&a.report.pass_probability.unwrap_or(0.0)).then(b.report.prediction.median.total_cmp(&a.report.prediction.median))
    });
    out
}

pub fn assumptions() -> Vec<String> {
    vec![
        "A fast physical estimate, not a solve of this package: good for ranking options, not for a read-range promise".into(),
        "Generic tag kinds, not specific parts; swap in the datasheet values".into(),
        "Forward link only: readers are assumed to hear whatever wakes up".into(),
        "A circular reader antenna, costing 3 dB to a linear tag".into(),
        "Contents as a uniform block behind the tag; cold, frost and condensation not included".into(),
        "About 12 fitted constants against 8 solved rows of an 81 Ω strip 2 to 20 mm from a block, with no independent check".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: f64 = 915e6;
    const EIRP: f64 = 36.0;

    /// The solves this estimate answers to: a half-wave strip resonant at 907.5 MHz with
    /// 81 ohms, a 40 mm block behind it. (material, er, tan d, gap mm, first resonance
    /// MHz, resistance there, efficiency, gain above the free dipole's 2.15 dBi after
    /// removing efficiency.) From `fdtd_water_behind_a_tag_against_the_quick_estimate`
    /// and `fdtd_other_contents_behind_a_tag` in the app, with openEMS.
    const SOLVED: [(&str, f64, f64, f64, f64, f64, f64, f64); 8] = [
        ("water", 78.0, 0.05, 2.0, 728.0, 15.0, 0.288, 0.72),
        ("water", 78.0, 0.05, 5.0, 798.0, 16.0, 0.314, 0.82),
        ("water", 78.0, 0.05, 10.0, 840.0, 21.0, 0.343, 1.49),
        ("water", 78.0, 0.05, 20.0, 868.0, 27.0, 0.429, 2.76),
        ("muscle", 50.0, 0.35, 5.0, 870.0, 53.0, 0.091, 1.18),
        ("muscle", 50.0, 0.35, 20.0, 902.0, 39.0, 0.387, 2.46),
        ("polycarbonate", 2.9, 0.01, 5.0, 835.0, 70.0, 0.998, 0.73),
        ("polycarbonate", 2.9, 0.01, 20.0, 908.0, 75.0, 1.000, 1.06),
    ];

    #[test]
    fn matches_the_solved_table() {
        let strip = TagKind { id: "strip", label: "strip", f_res: 907.5e6, r_rad: 81.0, q: 8.0, gain_dbi: 2.15, spacer: 0.0, own_ground: false };
        for (name, er, tan, gap, f_solved, r_solved, eff_solved, boost_solved) in SOLVED {
            let s = evaluate(&strip, er, tan, false, gap * 1e-3, 915e6, 36.0);
            let at = format!("{name} at {gap} mm: {s:?}");
            assert!((s.f_res / 1e6 / f_solved - 1.0).abs() < 0.08, "resonance, solved {f_solved}: {at}");
            assert!((s.resistance - r_solved).abs() < 0.4 * r_solved.max(25.0), "resistance, solved {r_solved}: {at}");
            assert!((s.efficiency - eff_solved).abs() < 0.08, "efficiency, solved {eff_solved}: {at}");
            assert!((s.boost_db - boost_solved).abs() < 1.0, "boost, solved {boost_solved}: {at}");
        }
    }

    #[test]
    fn the_metal_image_matches_the_moment_solver() {
        // Resistance of the same strip over a metal plate, method of moments.
        let strip = TagKind { id: "strip", label: "strip", f_res: 907.5e6, r_rad: 81.0, q: 8.0, gain_dbi: 2.15, spacer: 0.0, own_ground: false };
        for (gap_mm, solved) in [(3.0, 0.2), (6.0, 1.0), (10.0, 2.0), (20.0, 8.0), (40.0, 29.0), (80.0, 78.0)] {
            let s = evaluate(&strip, 1.0, 0.0, true, gap_mm * 1e-3, 915e6, 36.0);
            assert!((s.resistance - solved).abs() < 0.6f64.max(0.1 * solved), "{gap_mm} mm: {} vs {solved}", s.resistance);
            assert!((s.f_res - 907.5e6).abs() < 1.0, "{s:?}");
        }
    }

    #[test]
    fn a_tag_in_free_space_reads_at_a_plausible_range() {
        let s = evaluate(&TAGS[0], 1.0, 0.0, false, 0.05, F, EIRP);
        assert!((s.f_res - 915e6).abs() < 1e6, "{s:?}");
        assert!(s.transfer > 0.7, "{s:?}");
        // Current passive tags read around 10–20 m at 4 W EIRP in open air.
        assert!(s.range_m > 8.0 && s.range_m < 30.0, "{s:?}");
    }

    #[test]
    fn water_right_behind_a_standard_inlay_kills_it_and_a_gap_brings_it_back() {
        let close = evaluate(&TAGS[0], 70.0, 0.2, false, 0.0005, F, EIRP);
        let far = evaluate(&TAGS[0], 70.0, 0.2, false, 0.03, F, EIRP);
        println!("close {close:?}\nfar {far:?}");
        assert!(close.range_m < 2.0, "{close:?}");
        assert!(far.range_m > 3.0 * close.range_m, "{far:?}");
        assert!(close.f_res < 0.9 * 915e6, "contents pull the resonance down: {close:?}");
        assert!(close.resistance < 0.6 * TAGS[0].r_rad, "the mirror image takes the resistance: {close:?}");
    }

    #[test]
    fn an_inlay_on_metal_fails_and_an_on_metal_tag_does_not() {
        let inlay = evaluate(&TAGS[0], 1.0, 0.0, true, 0.0002, F, EIRP);
        let on_metal = evaluate(tag("on_metal").unwrap(), 1.0, 0.0, true, 0.0, F, EIRP);
        println!("inlay {inlay:?}\non metal {on_metal:?}");
        // Unusable, though the estimate can't say exactly how short: its resistance is floored.
        assert!(inlay.range_m < 1.5, "{inlay:?}");
        assert!(inlay.efficiency < 0.01, "{inlay:?}");
        assert!(on_metal.range_m > 5.0 * inlay.range_m, "{on_metal:?}");
    }

    #[test]
    fn cardboard_barely_matters() {
        let free = evaluate(&TAGS[0], 1.0, 0.0, false, 0.05, F, EIRP);
        // Corrugated board is mostly air.
        let card = evaluate(&TAGS[0], 1.3, 0.05, false, 0.002, F, EIRP);
        assert!(card.range_m > 0.5 * free.range_m, "{card:?} vs {free:?}");
    }

    #[test]
    fn a_broad_tag_is_more_forgiving_than_a_narrow_one() {
        // A moderate shift, as frozen food or oil would cause.
        let (er, tan) = (6.0, 0.02);
        let narrow = evaluate(&TAGS[0], er, tan, false, 0.003, F, EIRP);
        let broad = evaluate(&TAGS[1], er, tan, false, 0.003, F, EIRP);
        assert!(broad.range_m > narrow.range_m, "{broad:?} vs {narrow:?}");
    }

    #[test]
    fn recommendations_are_sensible_for_each_product() {
        let cans = recommend(contents("metal_can").unwrap(), F, EIRP, 2.0, 100);
        // Stuck straight onto a can, only an on-metal tag works. Standing off it works
        // too, which is why hang tags do.
        let flush = cans.iter().find(|o| o.placement.starts_with("Directly")).unwrap();
        assert_eq!(flush.tag, tag("on_metal").unwrap().label, "flush on a can: {}", flush.tag);
        assert!(cans[0].report.prediction.median > 2.0, "{:?}", cans[0].report.prediction);
        let water = recommend(contents("water_based").unwrap(), F, EIRP, 2.0, 100);
        assert!(water[0].placement.contains("gap") || water[0].tag.contains("On-metal"), "{} / {}", water[0].tag, water[0].placement);
        let dry = recommend(contents("cardboard").unwrap(), F, EIRP, 2.0, 100);
        assert!(dry[0].report.prediction.median > 5.0, "{:?}", dry[0].report.prediction);
        assert_eq!(dry[0].report.prediction.evidence, Evidence::Estimate);
        for c in &CONTENTS {
            for o in recommend(c, F, EIRP, 2.0, 30) {
                assert!(o.report.prediction.median.is_finite(), "{} {}", c.id, o.tag);
                assert_eq!(o.report.failed, 0);
            }
        }
    }

    // OUT-M7: recommendations went from 0.4 to 46 mm and 6 to 25 Ω with no word of it.
    #[test]
    fn a_recommendation_outside_the_fit_says_so_and_spreads_wider() {
        let strip = TagKind { id: "strip", label: "strip", f_res: 907.5e6, r_rad: 81.0, q: 8.0, gain_dbi: 2.15, spacer: 0.0, own_ground: false };
        assert!(outside_fit(&strip, (0.002, 0.02), false).is_empty());
        assert_eq!(outside_fit(&strip, (0.0004, 0.046), false).len(), 1);
        assert!(outside_fit(&strip, (0.0002, 0.001), false)[0].contains("extrapolation"));
        assert!(outside_fit(&TAGS[0], (0.002, 0.01), false)[0].contains("81 Ω strip"));
        for o in recommend(contents("water_based").unwrap(), F, EIRP, 2.0, 100) {
            assert!(!o.warnings.is_empty(), "no tag kind here was solved: {} {}", o.tag, o.placement);
            assert!(o.report.bases.iter().any(|(n, _)| n == "Model error"), "{:?}", o.report.bases);
            assert!(o.report.swings.iter().any(|s| s.factor == "Model error" && s.span > 0.0), "{:?}", o.report.swings);
        }
    }

    // The audit: the free-space inlay computes to 24.0 m against the 10 to 20 m real
    // ones reach, and 3 dB of polarisation loss brings it to 17 m.
    #[test]
    fn free_space_range_includes_the_polarisation_loss() {
        let bare = evaluate(&TAGS[0], 1.0, 0.0, false, 0.05, F, EIRP).range_m;
        assert!((bare - 24.0).abs() < 1.0, "{bare}");
        let with_pol = evaluate(&TAGS[0], 1.0, 0.0, false, 0.05, F, EIRP - 3.0).range_m;
        assert!((10.0..20.0).contains(&with_pol), "{with_pol}");
        let cardboard = recommend(contents("cardboard").unwrap(), F, EIRP, 2.0, 200);
        assert!(cardboard[0].report.prediction.median < 24.0, "{:?}", cardboard[0].report.prediction);
    }

    #[test]
    fn lookups_work_and_ids_match_the_vision_classes() {
        for id in ["dry_goods", "cardboard", "plastic", "water_based", "oily", "frozen", "metal_can", "foil_lined"] {
            assert!(contents(id).is_some(), "{id}");
        }
        assert!(contents("x").is_none());
        assert!(tag("on_metal").is_some());
    }
}

/// Method-of-moments reference for the metal case:
/// `cargo test -p antenna_core tag_over_metal_reference -- --ignored --nocapture`
#[cfg(test)]
mod reference {
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::{ExtraPrim, ExtraSolid, Project};

    #[test]
    #[ignore = "prints a table for calibrating the estimate"]
    fn tag_over_metal_reference() {
        let p = Project::from_template("strip_dipole", Some("915MHz")).unwrap();
        let (lo, hi) = p.geometry().unwrap().bounds();
        let opts = MomOptions { edges_per_lambda: 24.0, theta_step: 15.0, phi_step: 15.0, ..Default::default() };
        for gap in [0.0, 0.003, 0.006, 0.01, 0.02, 0.04, 0.08] {
            let mut q = p.clone();
            if gap > 0.0 {
                let e = |v: f64| format!("{v:e}");
                q.extra.push(ExtraSolid { name: "plate".into(), material: "PEC".into(), prim: ExtraPrim::Box { min: [e(lo[0] - 0.05), e(gap), e(lo[2] - 0.05)], max: [e(hi[0] + 0.05), e(gap), e(hi[2] + 0.05)] } });
            }
            let g = q.geometry().unwrap();
            let problem = MomProblem::new(&g, 1.3e9, opts).unwrap();
            let fs: Vec<f64> = (0..15).map(|i| 600e6 + 50e6 * i as f64).collect();
            let z: Vec<_> = fs.iter().map(|f| problem.solve(*f, false).unwrap().z[0][0]).collect();
            let zero: Vec<f64> = (1..z.len()).filter(|&k| z[k - 1].im < 0.0 && z[k].im >= 0.0).map(|k| {
                let t = -z[k - 1].im / (z[k].im - z[k - 1].im);
                fs[k - 1] + t * (fs[k] - fs[k - 1])
            }).collect();
            let at = |f: f64| z.iter().zip(&fs).min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map(|x| *x.0).unwrap();
            let far = problem.solve(915e6, true).unwrap();
            let m = far.far[0].metrics();
            println!("gap {:>3.0} mm: X=0 at {:?} MHz | R there {:?} | Z(915) {:.1} | peak gain {:.2} dBi", gap * 1e3, zero.iter().map(|f| (f / 1e6).round()).collect::<Vec<_>>(), zero.iter().map(|f| at(*f).re.round()).collect::<Vec<_>>(), at(915e6), m.peak_gain_dbi);
        }
    }
}
