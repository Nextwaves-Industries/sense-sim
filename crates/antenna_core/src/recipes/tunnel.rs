//! An RFID tunnel over a conveyor: how fast can the belt run before tags get missed,
//! and how close can cartons ride before reads land on the wrong one?
//!
//! A tag gets a limited time in the field, shared with every other tag in there, and
//! the antennas take turns. So a read needs the link to close at the tag's orientation
//! and from an antenna on a side that isn't blocked by the contents, and needs a slot
//! to answer in while that antenna is on. Link strength comes from [`super::tagging`].
//! The inventory rate uses the same slotted model as the site simulator.
//!
//! A quick estimate for finding the limits and comparing setups. It has not been
//! checked against a real tunnel: run the real carton mix before committing a speed.

use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use super::tagging::{Contents, TagKind, evaluate as tag_state};
use crate::C0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TunnelSpec {
    pub belt_mps: f64,
    pub carton_length_m: f64,
    /// Between one carton's tail and the next one's nose.
    pub gap_m: f64,
    /// Length of belt the field covers.
    pub zone_m: f64,
    pub tags_per_carton: usize,
    /// What the reader manages with few tags in the field, reads per second.
    pub reader_reads_per_s: f64,
    /// Antennas around the belt, taking turns.
    pub antennas: usize,
    pub antenna_distance_m: f64,
    pub eirp_dbm: f64,
    pub f: f64,
    /// Package wall between a tag and the contents.
    pub wall_m: f64,
}

impl Default for TunnelSpec {
    fn default() -> Self {
        Self { belt_mps: 0.5, carton_length_m: 0.4, gap_m: 0.3, zone_m: 1.0, tags_per_carton: 24, reader_reads_per_s: 400.0, antennas: 4, antenna_distance_m: 0.6, eirp_dbm: 36.0, f: 915e6, wall_m: 0.002 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TunnelState {
    /// Tags in the field at once, on average.
    pub tags_in_field: f64,
    /// Reads per second each tag gets while its antenna is on.
    pub reads_per_tag_s: f64,
    pub dwell_s: f64,
    /// Chance a given antenna's link closes for a tag, over orientation and side.
    pub link: f64,
    /// The same for an antenna on the tag's own side of the carton, and across it.
    pub near: f64,
    pub far: f64,
    /// Chance a tag is read during its pass.
    pub read: f64,
    /// Chance every tag in a carton is read.
    pub carton_complete: f64,
    /// Share of a carton's window with a neighbour in the field too: without gating,
    /// reads can land on the wrong carton.
    pub overlap: f64,
    pub cartons_per_min: f64,
}

/// Inventory rate per tag with `n` in the field: slotted Aloha at its best (the site
/// simulator's `rfid_per_tag_rate`), normalised to the reader's rate with few tags.
fn per_tag_rate(reads_per_s: f64, n: f64) -> f64 {
    let n = n.max(1.0);
    reads_per_s * 0.368 * 2.72 / n / (1.0 + 0.002 * n)
}

/// Share of orientations with the tag's pattern above the margin. A dipole's pattern
/// is |sin θ|, and over all orientations P(|sin θ| ≥ s) = √(1 − s²).
fn orientation_share(margin_db: f64) -> f64 {
    if margin_db <= 0.0 {
        return 0.0;
    }
    let s = 10f64.powf(-margin_db / 20.0);
    (1.0 - s * s).max(0.0).sqrt()
}

/// What the contents cost a signal crossing the carton to a tag on its far side, dB.
fn through_loss_db(er: f64, tan_d: f64, metal: bool, depth_m: f64, f: f64) -> f64 {
    if metal {
        return 100.0;
    }
    let n = er.max(1.0).sqrt();
    let g = (n - 1.0) / (n + 1.0);
    let k = std::f64::consts::TAU * f / C0;
    let absorption = k * n * tan_d / 2.0 * 8.686 * depth_m;
    let reflection = -10.0 * (1.0 - g * g).max(1e-6).log10();
    absorption + 2.0 * reflection
}

pub fn evaluate(spec: &TunnelSpec, tag: &TagKind, er: f64, tan_d: f64, metal: bool) -> TunnelState {
    let pitch = (spec.carton_length_m + spec.gap_m).max(1e-3);
    let cartons_in_zone = ((spec.zone_m + spec.carton_length_m) / pitch).max(1.0);
    let tags_in_field = spec.tags_per_carton as f64 * cartons_in_zone;
    let reads_per_tag_s = per_tag_rate(spec.reader_reads_per_s, tags_in_field);
    let dwell_s = spec.zone_m / spec.belt_mps.max(1e-3);
    let range = tag_state(tag, er, tan_d, metal, spec.wall_m, spec.f, spec.eirp_dbm).range_m;
    // A circular reader antenna gives up 3 dB to a linear tag.
    let margin = 20.0 * (range / spec.antenna_distance_m.max(0.01)).max(1e-6).log10() - 3.0;
    let near = orientation_share(margin);
    let far = orientation_share(margin - through_loss_db(er, tan_d, metal, spec.carton_length_m, spec.f));
    // With tags facing any way, one antenna is on the tag's own side half the time.
    let link = 0.5 * (near + far);
    let antennas = spec.antennas.max(1);
    let answered = 1.0 - (-reads_per_tag_s * dwell_s / antennas as f64).exp();
    let read = if antennas == 1 {
        link * answered
    } else {
        // Around the belt, about half of them always are.
        let own_side = antennas.div_ceil(2) as f64;
        let across = antennas as f64 - own_side;
        1.0 - (1.0 - near * answered).powf(own_side) * (1.0 - far * answered).powf(across)
    };
    // A neighbour ahead shares the start of a carton's window and one behind the end, each
    // for zone - gap of the belt.
    let overlap = (2.0 * (spec.zone_m - spec.gap_m).max(0.0) / (spec.zone_m + spec.carton_length_m)).min(1.0);
    TunnelState {
        tags_in_field,
        reads_per_tag_s,
        dwell_s,
        link,
        near,
        far,
        read,
        carton_complete: read.powf(spec.tags_per_carton as f64),
        overlap,
        cartons_per_min: 60.0 * spec.belt_mps / pitch,
    }
}

/// The fastest belt that keeps `read` at or above `min_read`, or `None` if even a
/// crawl doesn't get there (the link is the problem, not the time).
pub fn max_belt_speed(spec: &TunnelSpec, tag: &TagKind, er: f64, tan_d: f64, metal: bool, min_read: f64) -> Option<f64> {
    let at = |belt: f64| evaluate(&TunnelSpec { belt_mps: belt, ..spec.clone() }, tag, er, tan_d, metal).read;
    let (mut lo, mut hi) = (0.01, 10.0);
    if at(lo) < min_read {
        return None;
    }
    if at(hi) >= min_read {
        return Some(hi);
    }
    for _ in 0..50 {
        let mid = (lo * hi as f64).sqrt();
        if at(mid) >= min_read {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// Read rate across what isn't known about the product and the tags.
pub fn predict(spec: &TunnelSpec, tag: &TagKind, c: &Contents, min_read: f64, samples: usize) -> RobustReport {
    let factors = vec![
        Factor::new("Contents permittivity", "", Dist::Uniform { lo: c.er.0, hi: c.er.1.max(c.er.0 + 1e-6) }, "Typical UHF range for this kind of product"),
        Factor::new("Contents loss tangent", "", Dist::Uniform { lo: c.tan_d.0, hi: c.tan_d.1.max(c.tan_d.0 + 1e-6) }, "Typical UHF range for this kind of product"),
        Factor::new("Reader power and tag batch", "dB", Dist::Normal { mean: 0.0, sigma: 2.0 }, "Cable, antenna and tag tolerance, assumed"),
        Factor::new("Reader throughput", "×", Dist::LogNormal { median: 1.0, sigma: 0.25 }, "Depends on reader mode and settings, assumed"),
        Factor::new("Model error", "×", Dist::LogNormal { median: 1.0, sigma: super::tagging::model_error_sigma(true) }, "How far the tag's range can be off. These tags lie outside what the tag model was fitted to, so it's wide. Assumed, not measured"),
    ];
    robust("Tags read per pass", "share", &factors, Some(Target::at_least(min_read)), Evidence::Estimate, RobustOptions { samples, seed: 71 }, |v| {
        // Range goes with the square root of power, so a range error is this much power.
        let s = TunnelSpec { eirp_dbm: spec.eirp_dbm + v[2] + 20.0 * v[4].log10(), reader_reads_per_s: spec.reader_reads_per_s * v[3], ..spec.clone() };
        evaluate(&s, tag, v[0], v[1], c.metal).read
    })
}

pub fn assumptions() -> Vec<String> {
    vec![
        "A quick estimate, not checked against a real tunnel: test the real carton mix before committing a speed".into(),
        "Tag ranges come from the tag estimate, which is for ranking; tags stacked inside a carton also shadow and detune each other, which isn't modelled".into(),
        "Antennas are assumed to surround the belt, and each covers the whole zone".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::tagging::{contents, tag};
    use super::*;

    fn cardboard() -> (f64, f64, bool) {
        let c = contents("cardboard").unwrap();
        (c.er.0, c.tan_d.0, c.metal)
    }

    #[test]
    fn slower_belts_and_fewer_tags_read_better() {
        let t = tag("wet_inlay").unwrap();
        let (er, tan, metal) = cardboard();
        let at = |belt: f64, tags: usize| evaluate(&TunnelSpec { belt_mps: belt, tags_per_carton: tags, ..Default::default() }, t, er, tan, metal);
        assert!(at(0.3, 24).read > at(2.0, 24).read);
        assert!(at(2.0, 24).read < at(2.0, 6).read + 1e-12);
        assert!(at(2.0, 200).read < at(2.0, 6).read);
        assert!(at(0.3, 24).carton_complete <= at(0.3, 24).read);
    }

    #[test]
    fn the_numbers_follow_the_formulas() {
        let t = tag("wet_inlay").unwrap();
        let (er, tan, metal) = cardboard();
        let s = TunnelSpec { belt_mps: 0.5, carton_length_m: 0.4, gap_m: 0.6, zone_m: 1.0, tags_per_carton: 10, reader_reads_per_s: 400.0, antennas: 2, ..Default::default() };
        let r = evaluate(&s, t, er, tan, metal);
        assert!((r.dwell_s - 2.0).abs() < 1e-9);
        assert!((r.tags_in_field - 14.0).abs() < 1e-9, "{r:?}");
        assert!((r.cartons_per_min - 30.0).abs() < 1e-9);
        // Zone 1 m, gap 0.6 m: the one ahead and the one behind each share 0.4 of 1.4 m.
        assert!((r.overlap - 0.8 / 1.4).abs() < 1e-9, "{r:?}");
        let answered = 1.0 - (-r.reads_per_tag_s * r.dwell_s / 2.0).exp();
        assert!((r.read - (1.0 - (1.0 - r.near * answered) * (1.0 - r.far * answered))).abs() < 1e-12);
    }

    #[test]
    fn neighbours_fill_the_window_at_the_default_spacing() {
        // 0.4 m cartons 0.3 m apart in a 1 m zone: one always shares it.
        let t = tag("wet_inlay").unwrap();
        let (er, tan, metal) = cardboard();
        assert_eq!(evaluate(&TunnelSpec::default(), t, er, tan, metal).overlap, 1.0);
    }

    #[test]
    fn antennas_around_the_belt_always_have_one_on_the_tags_side() {
        // Opaque contents: nothing reads across the carton, yet tags still face an antenna.
        let t = tag("on_metal").unwrap();
        let s = TunnelSpec { belt_mps: 0.05, tags_per_carton: 1, antenna_distance_m: 0.3, ..Default::default() };
        let r = evaluate(&s, t, 1.0, 0.0, true);
        assert_eq!(r.far, 0.0);
        assert!(r.near > 0.5, "{r:?}");
        assert!(r.read > 0.9375, "past what coin flips for the side could reach: {r:?}");
        // Alone, it's on the right side half the time.
        let one = evaluate(&TunnelSpec { antennas: 1, ..s }, t, 1.0, 0.0, true);
        assert!(one.read <= 0.5, "{one:?}");
    }

    #[test]
    fn a_gap_as_long_as_the_zone_ends_the_overlap() {
        let t = tag("wet_inlay").unwrap();
        let (er, tan, metal) = cardboard();
        let r = evaluate(&TunnelSpec { zone_m: 1.0, gap_m: 1.0, ..Default::default() }, t, er, tan, metal);
        assert_eq!(r.overlap, 0.0);
    }

    #[test]
    fn orientation_share_matches_the_dipole_pattern() {
        assert_eq!(orientation_share(-1.0), 0.0);
        // 6.02 dB of margin: tags with |sin θ| ≥ ½ read, so √(1 − ¼) of them.
        assert!((orientation_share(6.0206) - 0.75f64.sqrt()).abs() < 1e-3);
        assert!(orientation_share(40.0) > 0.99);
    }

    #[test]
    fn metal_and_water_block_the_far_side_and_cardboard_does_not() {
        let f = 915e6;
        assert_eq!(through_loss_db(1.0, 0.0, true, 0.2, f), 100.0);
        let water = through_loss_db(70.0, 0.2, false, 0.2, f);
        let card = through_loss_db(1.3, 0.05, false, 0.2, f);
        assert!(water > 30.0 && card < 3.0, "{water} {card}");
    }

    #[test]
    fn max_speed_is_the_edge_and_a_dead_link_has_none() {
        let t = tag("wet_inlay").unwrap();
        let (er, tan, metal) = cardboard();
        let s = TunnelSpec::default();
        let v = max_belt_speed(&s, t, er, tan, metal, 0.99).expect("cardboard reads");
        let at = |b: f64| evaluate(&TunnelSpec { belt_mps: b, ..s.clone() }, t, er, tan, metal).read;
        assert!(at(v * 0.98) >= 0.99, "{v}");
        assert!(at(v * 1.1) < 0.99, "{v}");
        // An inlay flat on metal, far from the antennas: no speed helps.
        let far = TunnelSpec { antenna_distance_m: 5.0, ..s };
        assert!(max_belt_speed(&far, t, 1.0, 0.0, true, 0.99).is_none());
    }

    #[test]
    fn prediction_has_a_range_and_a_stated_evidence_level() {
        let c = contents("water_based").unwrap();
        let r = predict(&TunnelSpec::default(), tag("foam_flag").unwrap(), c, 0.99, 200);
        assert_eq!(r.prediction.evidence, Evidence::Estimate);
        assert!(r.prediction.low <= r.prediction.high);
        assert_eq!(r.failed, 0);
    }
}
