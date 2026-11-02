//! Preparing a wireless medical device for coexistence testing. Regulators expect
//! coexistence to be addressed, and ANSI C63.27 tests it at a depth set by a risk
//! analysis of what a failed wireless function would mean. This doesn't replace any
//! of that: it estimates which of the networks around the device are the likely
//! trouble, so the lab time goes on the right ones.
//!
//! A packet is lost when another network is on the air during it and hurts more than
//! the link's margin can absorb. The function fails when the data can't get through
//! in the time it's allowed. Tiers aren't assigned here: they come from your risk
//! analysis under AAMI TIR69, and their details are in the standard.

use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use super::coexist::{Isolation, Radio, pair};
use crate::C0;

/// What the device's wireless link has to deliver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceLink {
    pub radio: Radio,
    /// Margin above the receiver's limit with nothing else around, dB.
    pub margin_db: f64,
    /// Attempts per second.
    pub packets_per_s: f64,
    /// Time on air per packet, seconds.
    pub packet_s: f64,
    /// The longest the function can go without data, seconds.
    pub max_gap_s: f64,
}

/// Another network near the device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Neighbour {
    pub name: String,
    pub radio: Radio,
    pub distance_m: f64,
    /// Radios of this kind at about this distance.
    pub count: usize,
    /// Extra loss on the way, walls and so on, dB.
    pub extra_loss_db: f64,
    /// Length of one of its transmissions, seconds.
    pub burst_s: f64,
}

/// Free-space loss between two antennas, dB.
pub fn free_space_db(distance_m: f64, f: f64) -> f64 {
    20.0 * (4.0 * std::f64::consts::PI * distance_m.max(0.01) * f / C0).log10()
}

/// The carrier frequency a neighbour would land on the device with.
fn victim_centre(r: &Radio) -> f64 {
    (r.rx.0 + r.rx.1) / 2.0
}

/// Does this radio move between channels from packet to packet?
fn hops(r: &Radio) -> bool {
    let n = r.name.to_ascii_lowercase();
    n.contains("ble") || n.contains("bluetooth") || n.contains("rfid")
}

/// Chance a neighbour's transmission lands on the channel the device is using. In a shared
/// band nobody coordinates channels and hopping radios move, so it's their widths over the
/// band. Out of band, what gets in is wideband noise, and the channel doesn't matter.
fn channel_overlap(device: &Radio, n: &Radio) -> f64 {
    if device.rx.0.max(n.tx.0) >= device.rx.1.min(n.tx.1) {
        return 1.0;
    }
    let span = (device.rx.1 - device.rx.0).max(n.tx.1 - n.tx.0);
    ((device.rx_bw_hz + n.rx_bw_hz) / span.max(1.0)).min(1.0)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub neighbour: String,
    pub isolation_db: f64,
    pub desense_db: f64,
    pub blocked: bool,
    /// Chance one of them is on the air during one of the device's packets.
    pub overlap: f64,
    /// Chance a packet is lost to this neighbour.
    pub packet_loss: f64,
}

/// One network's effect on the device.
pub fn assess(link: &DeviceLink, n: &Neighbour) -> Finding {
    let isolation_db = free_space_db(n.distance_m, victim_centre(&link.radio)) + n.extra_loss_db;
    let p = pair(&n.radio, &link.radio, &Isolation::Flat(isolation_db), None);
    // A burst of length B overlaps a packet of length P if it starts within B + P of
    // it. Each radio is on air `duty` of the time, in bursts of `burst_s`.
    let per_radio = (n.radio.duty * (n.burst_s + link.packet_s) / n.burst_s.max(1e-6)).min(1.0);
    let count = n.count.max(1) as i32;
    let overlap = 1.0 - (1.0 - per_radio).powi(count);
    let hurts = p.blocked || p.desense_db > link.margin_db;
    // A blocked receiver is deaf on every channel. And the channel only comes up afresh for
    // each packet if one side hops: two on fixed channels either always clash or never, so
    // they're taken as sharing one, which is the case to test.
    let channel = if p.blocked || !(hops(&link.radio) || hops(&n.radio)) { 1.0 } else { channel_overlap(&link.radio, &n.radio) };
    let hit = 1.0 - (1.0 - per_radio * channel).powi(count);
    Finding { neighbour: n.name.clone(), isolation_db, desense_db: p.desense_db, blocked: p.blocked, overlap, packet_loss: if hurts { hit } else { 0.0 } }
}

/// Chance the function goes `max_gap_s` without data, given independent packet losses.
pub fn failure_chance(link: &DeviceLink, findings: &[Finding]) -> f64 {
    let survives: f64 = findings.iter().map(|f| 1.0 - f.packet_loss).product();
    let loss = 1.0 - survives;
    let attempts = (link.max_gap_s * link.packets_per_s).floor().max(1.0);
    loss.powf(attempts)
}

/// How many times an hour the function goes `max_gap_s` without data. A chance per window
/// says little alone: at 50 % loss and 10 tries a second, 0.1 % of windows sounds rare and is
/// about 18 times an hour. These are runs of losses long enough to start one, or at very
/// high loss, when one run never ends, every window.
pub fn gaps_per_hour(link: &DeviceLink, findings: &[Finding]) -> f64 {
    let survives: f64 = findings.iter().map(|f| 1.0 - f.packet_loss).product();
    let loss = 1.0 - survives;
    let attempts = (link.max_gap_s * link.packets_per_s).floor().max(1.0);
    let runs = link.packets_per_s * 3600.0 * (1.0 - loss) * loss.powf(attempts);
    let windows = 3600.0 / link.max_gap_s.max(1e-3) * loss.powf(attempts);
    runs.max(windows)
}

/// The same across what isn't known: how busy the networks are, how far apart, and the margin.
pub fn predict(link: &DeviceLink, neighbours: &[Neighbour], max_gaps_per_hour: f64, samples: usize) -> RobustReport {
    let factors = vec![
        Factor::new("How busy the networks are", "×", Dist::LogNormal { median: 1.0, sigma: 0.6 }, "Hospital traffic varies widely over a day"),
        Factor::new("Distance to them", "×", Dist::LogNormal { median: 1.0, sigma: 0.5 }, "A cart or a phone can end up right next to the device"),
        Factor::new("Device link margin", "dB", Dist::Normal { mean: link.margin_db, sigma: 4.0 }, "Body, bed rails and orientation move it"),
    ];
    robust("Times the function goes too long without data", "per hour", &factors, Some(Target::at_most(max_gaps_per_hour)), Evidence::Estimate, RobustOptions { samples, seed: 81 }, |v| {
        let mut l = link.clone();
        l.margin_db = v[2];
        let findings: Vec<Finding> = neighbours
            .iter()
            .map(|n| {
                let mut m = n.clone();
                m.radio.duty = (n.radio.duty * v[0]).min(1.0);
                m.distance_m = n.distance_m * v[1];
                assess(&l, &m)
            })
            .collect();
        gaps_per_hour(&l, &findings)
    })
}

/// Who a hospital device typically shares the air with. Typical figures, to replace
/// with what's really on the ward.
pub fn typical_hospital() -> Vec<Neighbour> {
    let preset = |name: &str| Radio::preset(name).unwrap_or_else(|| Radio::presets().remove(0));
    vec![
        Neighbour { name: "Hospital WiFi 2.4 GHz access point".into(), radio: preset("WiFi 2.4 GHz"), distance_m: 8.0, count: 2, extra_loss_db: 6.0, burst_s: 0.002 },
        Neighbour { name: "Phone or tablet on WiFi, at the bedside".into(), radio: preset("WiFi 2.4 GHz"), distance_m: 1.0, count: 2, extra_loss_db: 0.0, burst_s: 0.002 },
        Neighbour { name: "Hospital WiFi 5 GHz access point".into(), radio: preset("WiFi 5 GHz"), distance_m: 8.0, count: 2, extra_loss_db: 6.0, burst_s: 0.002 },
        Neighbour { name: "BLE wearables and asset tags".into(), radio: preset("BLE"), distance_m: 2.0, count: 10, extra_loss_db: 0.0, burst_s: 0.0004 },
        Neighbour { name: "RFID reader on a cart or doorway".into(), radio: preset("UHF RFID reader"), distance_m: 3.0, count: 1, extra_loss_db: 0.0, burst_s: 0.05 },
    ]
}

pub fn assumptions() -> Vec<String> {
    vec![
        "For planning tests only: this doesn't show coexistence, and doesn't replace ANSI C63.27 testing or a risk analysis under AAMI TIR69".into(),
        "Antennas are free space and 0 dBi, and radio figures are typical: use your device's and the ward's real ones".into(),
        "One-way only: the device's effect on the networks around it isn't estimated".into(),
        "Nobody coordinates channels here: a radio that hops around busy channels would do better than this. Radios on fixed channels are taken as sharing one".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> DeviceLink {
        DeviceLink { radio: Radio::preset("BLE").unwrap(), margin_db: 15.0, packets_per_s: 10.0, packet_s: 0.0005, max_gap_s: 1.0 }
    }

    fn wifi(distance: f64, count: usize) -> Neighbour {
        Neighbour { name: "wifi".into(), radio: Radio::preset("WiFi 2.4 GHz").unwrap(), distance_m: distance, count, extra_loss_db: 0.0, burst_s: 0.002 }
    }

    #[test]
    fn free_space_loss_matches_the_textbook() {
        // 1 m at 2.4 GHz is about 40 dB, and each doubling adds 6.
        assert!((free_space_db(1.0, 2.4e9) - 40.05).abs() < 0.1);
        assert!((free_space_db(2.0, 2.4e9) - free_space_db(1.0, 2.4e9) - 6.02).abs() < 0.01);
    }

    #[test]
    fn closer_and_busier_is_worse() {
        let l = link();
        let near = assess(&l, &wifi(0.5, 1));
        let far = assess(&l, &wifi(50.0, 1));
        assert!(near.desense_db > far.desense_db, "{near:?} {far:?}");
        let many = assess(&l, &wifi(0.5, 5));
        assert!(many.overlap > near.overlap);
    }

    #[test]
    fn enough_margin_rides_it_out() {
        let mut l = link();
        l.margin_db = 500.0;
        // Same band, right next to it: the carrier still blocks, so it's still lost.
        let blocked = assess(&l, &wifi(0.1, 1));
        assert!(blocked.blocked && blocked.packet_loss > 0.0, "{blocked:?}");
        // Not blocked and within margin: no loss at all.
        let far = assess(&l, &wifi(30.0, 1));
        assert!(!far.blocked, "{far:?}");
        assert_eq!(far.packet_loss, 0.0, "{far:?}");
    }

    #[test]
    fn a_hopping_radio_usually_misses_a_neighbours_channel() {
        let l = link();
        let w = wifi(3.0, 1);
        // 2 MHz on a 78 MHz band against a 20 MHz channel.
        assert!((channel_overlap(&l.radio, &w.radio) - 22.0 / 78.0).abs() < 0.01);
        let f = assess(&l, &w);
        assert!(!f.blocked && f.packet_loss > 0.0 && f.packet_loss < 0.5 * f.overlap, "{f:?}");
        // Out of band the channel doesn't help.
        assert_eq!(channel_overlap(&l.radio, &Radio::preset("UHF RFID reader").unwrap()), 1.0);
        // Losing a whole second of attempts on a typical ward is the exception. Before this,
        // every BLE device on every ward came out at about 93 %.
        let on_ward = DeviceLink { packet_s: 0.002, ..l };
        let findings: Vec<Finding> = typical_hospital().iter().map(|n| assess(&on_ward, n)).collect();
        let chance = failure_chance(&on_ward, &findings);
        assert!(chance > 1e-5 && chance < 0.05, "{chance}");
    }

    #[test]
    fn gaps_have_a_time_base() {
        let l = link();
        let f = |loss: f64| Finding { neighbour: String::new(), isolation_db: 0.0, desense_db: 0.0, blocked: false, overlap: loss, packet_loss: loss };
        // 10 tries a second at 50 % loss: 36 000 × ½ × ½¹⁰ ≈ 17.6 an hour.
        assert!((gaps_per_hour(&l, &[f(0.5)]) - 17.58).abs() < 0.01);
        assert_eq!(gaps_per_hour(&l, &[f(0.0)]), 0.0);
        // Nothing gets through at all: every second of the hour is too long.
        assert_eq!(gaps_per_hour(&l, &[f(1.0)]), 3600.0);
    }

    #[test]
    fn two_radios_on_fixed_channels_are_taken_as_sharing_one() {
        let on_wifi = DeviceLink { radio: Radio::preset("WiFi 2.4 GHz").unwrap(), packet_s: 0.002, ..link() };
        let f = assess(&on_wifi, &wifi(3.0, 1));
        assert!(!f.blocked, "{f:?}");
        assert_eq!(f.packet_loss, f.overlap, "the channel doesn't come up afresh for each packet");
    }

    #[test]
    fn failure_needs_every_attempt_lost() {
        let l = link();
        let f = |loss: f64| Finding { neighbour: String::new(), isolation_db: 0.0, desense_db: 0.0, blocked: false, overlap: loss, packet_loss: loss };
        // 10 attempts in a second at 50 % loss: ½¹⁰.
        assert!((failure_chance(&l, &[f(0.5)]) - 0.5f64.powi(10)).abs() < 1e-12);
        assert_eq!(failure_chance(&l, &[f(0.0)]), 0.0);
        // Independent networks add up.
        let both = failure_chance(&l, &[f(0.5), f(0.5)]);
        assert!((both - 0.75f64.powi(10)).abs() < 1e-12);
        // A tighter deadline fails more easily.
        let mut tight = l.clone();
        tight.max_gap_s = 0.2;
        assert!(failure_chance(&tight, &[f(0.5)]) > failure_chance(&l, &[f(0.5)]));
    }

    #[test]
    fn prediction_is_only_ever_an_estimate() {
        let r = predict(&link(), &typical_hospital(), 0.1, 200);
        assert_eq!(r.prediction.evidence, Evidence::Estimate);
        assert!(r.prediction.low >= 0.0 && r.prediction.high >= r.prediction.low);
        assert!(assumptions()[0].contains("doesn't replace"));
    }
}
