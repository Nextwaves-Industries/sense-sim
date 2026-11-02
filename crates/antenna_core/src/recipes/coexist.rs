//! Radios in one device hurting each other. Protocols coordinate who talks when;
//! nothing at that level stops one radio's transmit energy from raising another's
//! noise floor. Given the antennas' isolation, this works out how much range each
//! victim loses, and how much isolation or filtering would fix it.

use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use crate::sparam::Network;

/// `kTB` at 290 K, dBm/Hz.
const THERMAL_DBM_HZ: f64 = -174.0;

/// A radio's relevant behaviour. Presets are typical figures, for a first look;
/// replace them with the chip's datasheet numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Radio {
    pub name: String,
    /// Transmit band, Hz.
    pub tx: (f64, f64),
    pub tx_dbm: f64,
    /// Receive band, Hz.
    pub rx: (f64, f64),
    pub rx_bw_hz: f64,
    pub noise_figure_db: f64,
    /// Above this level at its input, the receiver loses sensitivity (blocking), dBm.
    pub blocking_dbm: f64,
    /// Transmit spectrum outside its band: noise density at the antenna, dBm/Hz.
    /// Modelled as the larger of this floor and a slope away from the band edge.
    pub tx_noise_floor_dbm_hz: f64,
    /// Density right at the band edge, dBm/Hz.
    pub tx_edge_dbm_hz: f64,
    /// How fast it falls away, dB per octave of offset from 10 MHz.
    pub tx_slope_db_oct: f64,
    /// Share of time transmitting.
    pub duty: f64,
    /// Path-loss exponent where it's used, to turn sensitivity into range.
    pub path_loss_exp: f64,
    /// Extra attenuation in front of the receiver against other bands, dB.
    pub rx_filter_db: f64,
    /// Its harmonics, against its carrier, dB. Typical: use the real figure.
    #[serde(default = "typical_harmonic_dbc")]
    pub harmonic_dbc: f64,
}

/// A common limit for a transmitter's harmonics; real ones run from about −40 to −70.
fn typical_harmonic_dbc() -> f64 {
    -60.0
}

impl Radio {
    fn base(name: &str, band: (f64, f64), tx_dbm: f64, rx_bw_hz: f64, nf: f64, blocking: f64, duty: f64) -> Self {
        Self { name: name.into(), tx: band, tx_dbm, rx: band, rx_bw_hz, noise_figure_db: nf, blocking_dbm: blocking, tx_noise_floor_dbm_hz: -140.0, tx_edge_dbm_hz: -70.0, tx_slope_db_oct: 9.0, duty, path_loss_exp: 2.2, rx_filter_db: 0.0, harmonic_dbc: typical_harmonic_dbc() }
    }

    pub fn presets() -> Vec<Radio> {
        vec![
            Self::base("BLE", (2.402e9, 2.48e9), 8.0, 2e6, 7.0, -20.0, 0.02),
            Self::base("WiFi 2.4 GHz", (2.412e9, 2.472e9), 20.0, 20e6, 5.0, -10.0, 0.3),
            Self::base("WiFi 5 GHz", (5.15e9, 5.85e9), 20.0, 80e6, 5.0, -10.0, 0.3),
            Self::base("WiFi 6E", (5.925e9, 7.125e9), 18.0, 160e6, 6.0, -10.0, 0.3),
            Radio { tx_noise_floor_dbm_hz: -145.0, tx_edge_dbm_hz: -90.0, ..Self::base("UWB channel 5", (6.24e9, 6.74e9), -14.0, 499.2e6, 6.0, -40.0, 0.02) },
            Radio { tx_noise_floor_dbm_hz: -145.0, tx_edge_dbm_hz: -90.0, ..Self::base("UWB channel 9", (7.74e9, 8.24e9), -14.0, 499.2e6, 6.0, -40.0, 0.02) },
            Radio { tx_noise_floor_dbm_hz: -135.0, tx_edge_dbm_hz: -60.0, ..Self::base("UHF RFID reader", (902e6, 928e6), 30.0, 500e3, 8.0, 10.0, 0.5) },
            Self::base("LTE-M band 13", (746e6, 787e6), 23.0, 1.4e6, 5.0, -15.0, 0.1),
        ]
    }

    pub fn preset(name: &str) -> Option<Radio> {
        Self::presets().into_iter().find(|r| r.name.eq_ignore_ascii_case(name))
    }

    /// What its antenna and front end reject at `f`, away from its own band: about
    /// 12 dB per octave of offset, measured in multiples of its band's width, up to a
    /// practical limit. Coupling, layout and spurious paths stop it going further.
    fn selectivity_db(&self, f: f64) -> f64 {
        let offset = if f < self.rx.0 {
            self.rx.0 - f
        } else if f > self.rx.1 {
            f - self.rx.1
        } else {
            return 0.0;
        };
        let width = (self.rx.1 - self.rx.0).max(1e6);
        (12.0 * (1.0 + offset / width).log2()).min(50.0)
    }

    fn noise_floor_dbm(&self) -> f64 {
        THERMAL_DBM_HZ + 10.0 * self.rx_bw_hz.log10() + self.noise_figure_db
    }

    /// Density this radio puts out at `f`, dBm/Hz.
    fn density_at(&self, f: f64) -> f64 {
        let offset = if f < self.tx.0 {
            self.tx.0 - f
        } else if f > self.tx.1 {
            f - self.tx.1
        } else {
            return self.tx_dbm - 10.0 * (self.tx.1 - self.tx.0).max(1.0).log10();
        };
        (self.tx_edge_dbm_hz - self.tx_slope_db_oct * (offset / 10e6).max(1.0).log2()).max(self.tx_noise_floor_dbm_hz)
    }

    /// Harmonics of its carrier landing in `band`, as (order, frequency).
    fn harmonics_in(&self, band: (f64, f64)) -> Vec<(u32, f64)> {
        let c = (self.tx.0 + self.tx.1) / 2.0;
        (2..=9).filter_map(|n| {
            let (lo, hi) = (self.tx.0 * n as f64, self.tx.1 * n as f64);
            (lo <= band.1 && hi >= band.0).then_some((n, c * n as f64))
        }).collect()
    }
}

/// Isolation between two antennas, dB (positive), against frequency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Isolation {
    /// Flat, e.g. a first guess or a datasheet value.
    Flat(f64),
    /// Read from an S-parameter sweep between these ports.
    Network { i: usize, j: usize },
}

impl Isolation {
    /// Isolation at `f`, or `None` where nothing says: past the sweep's ends a nearby
    /// sample means little, since coupling changes completely from one band to another.
    fn at(&self, f: f64, net: Option<&Network>) -> Option<f64> {
        match (self, net) {
            (Isolation::Flat(v), _) => Some(*v),
            (Isolation::Network { i, j }, Some(n)) if !n.f.is_empty() => {
                if f < n.f[0] || f > n.f[n.f.len() - 1] {
                    return None;
                }
                let db = n.sij_db(*i, *j);
                let k = n.f.partition_point(|x| *x < f).clamp(1, n.f.len().max(2) - 1).min(n.f.len() - 1);
                if n.f.len() == 1 || n.f[k] == n.f[k - 1] {
                    return Some(-db[k]);
                }
                let t = (f - n.f[k - 1]) / (n.f[k] - n.f[k - 1]);
                Some(-(db[k - 1] + (db[k] - db[k - 1]) * t))
            }
            _ => None,
        }
    }

    /// What to use where nothing says: the least isolation on record, or none at all.
    fn worst_case(&self, net: Option<&Network>) -> f64 {
        match (self, net) {
            (Isolation::Flat(v), _) => *v,
            (Isolation::Network { i, j }, Some(n)) if !n.f.is_empty() => n.sij_db(*i, *j).iter().map(|v| -*v).fold(f64::MAX, f64::min),
            _ => 0.0,
        }
    }
}

/// Worst case of one transmitter against one receiver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairResult {
    pub aggressor: String,
    pub victim: String,
    pub isolation_db: f64,
    /// The aggressor's noise inside the victim's band at its input, dBm.
    pub in_band_dbm: f64,
    /// Sensitivity lost to that noise, dB.
    pub desense_db: f64,
    /// The aggressor's carrier at the victim's input, dBm.
    pub carrier_dbm: f64,
    pub blocked: bool,
    /// Isolation that would hold desense to 1 dB and stop blocking, dB.
    pub needed_isolation_db: f64,
    /// Victim's range remaining while the aggressor transmits (0..1).
    pub range_while_on: f64,
    /// Harmonics landing in the victim's band: (order, frequency in Hz).
    pub harmonics: Vec<(u32, f64)>,
    /// Their bands overlap, so no filter can separate them.
    pub overlap: bool,
    /// Plain-language finding.
    pub note: String,
    /// Frequencies the isolation data doesn't cover, where its worst value stood in.
    #[serde(default)]
    pub isolation_unknown_at_hz: Vec<f64>,
}

/// Figures in this model with no source behind them, said wherever it's quoted.
pub fn assumptions() -> Vec<String> {
    vec![
        "Typical out-of-band figures; datasheets rarely give the real ones".into(),
        "Worst moment: while the other radio is transmitting. How often that is isn't in the number".into(),
        "Assumed with no source: 3 dB lost per dB of blocking, 12 dB per octave of receiver selectivity, harmonics at their radio's stated level (typically -60 dBc)".into(),
    ]
}

pub fn pair(aggressor: &Radio, victim: &Radio, isolation: &Isolation, net: Option<&Network>) -> PairResult {
    let overlap = aggressor.tx.0 <= victim.rx.1 && aggressor.tx.1 >= victim.rx.0;
    let n = 16;
    let mut worst_density = f64::MIN;
    let mut worst_iso = f64::MAX;
    let mut worst_in_band = f64::MIN;
    let unknown = std::cell::RefCell::new(Vec::new());
    let iso_at = |f: f64| {
        isolation.at(f, net).unwrap_or_else(|| {
            unknown.borrow_mut().push(f);
            isolation.worst_case(net)
        })
    };
    for k in 0..=n {
        let f = victim.rx.0 + (victim.rx.1 - victim.rx.0) * k as f64 / n as f64;
        let iso = iso_at(f);
        let density = aggressor.density_at(f);
        // In the victim's own band, its filter doesn't help.
        let at_input = density + 10.0 * victim.rx_bw_hz.log10() - iso;
        if at_input > worst_in_band {
            worst_in_band = at_input;
            worst_density = density;
            worst_iso = iso;
        }
    }
    // Harmonics landing in its band are noise it can't filter out: they join the sum.
    let harmonics = aggressor.harmonics_in(victim.rx);
    let add_db = |a: f64, b: f64| 10.0 * (10f64.powf(a / 10.0) + 10f64.powf(b / 10.0)).log10();
    let harmonic_out = aggressor.tx_dbm + aggressor.harmonic_dbc;
    let mut noise_alone = worst_in_band;
    let mut harmonic_in = f64::NEG_INFINITY;
    for (_, f) in &harmonics {
        let at_input = harmonic_out - iso_at(f.clamp(victim.rx.0, victim.rx.1));
        harmonic_in = harmonic_in.max(at_input);
        noise_alone = add_db(noise_alone, at_input);
    }
    let in_band_dbm = noise_alone;
    let floor = victim.noise_floor_dbm();
    let desense_db = 10.0 * (1.0 + 10f64.powf((in_band_dbm - floor) / 10.0)).log10();
    let carrier_f = (aggressor.tx.0 + aggressor.tx.1) / 2.0;
    let carrier_iso = iso_at(carrier_f);
    let filter = if overlap { 0.0 } else { victim.rx_filter_db + victim.selectivity_db(carrier_f) };
    let carrier_dbm = aggressor.tx_dbm - carrier_iso - filter;
    let blocked = carrier_dbm > victim.blocking_dbm;
    let blocking_loss_db = if blocked { 3.0 * (carrier_dbm - victim.blocking_dbm).max(0.0) } else { 0.0 };
    // 1 dB of desense is noise 5.87 dB under the floor. Before any isolation:
    let mut before_isolation = worst_density + 10.0 * victim.rx_bw_hz.log10();
    for _ in &harmonics {
        before_isolation = add_db(before_isolation, harmonic_out);
    }
    let need_noise = before_isolation - (floor - 5.87);
    let need_block = aggressor.tx_dbm - filter - victim.blocking_dbm;
    let needed_isolation_db = need_noise.max(need_block).max(0.0);
    let loss = desense_db + blocking_loss_db;
    let range_while_on = 10f64.powf(-loss / (10.0 * victim.path_loss_exp));
    let note = if overlap {
        format!("{} transmits inside {}'s band: no filter can separate them, so these need isolation and scheduling.", aggressor.name, victim.name)
    } else if blocked {
        format!("{} overloads {}'s receiver ({carrier_dbm:.0} dBm at its input). Needs {needed_isolation_db:.0} dB of isolation or filtering.", aggressor.name, victim.name)
    } else if desense_db >= 1.0 && harmonic_in >= worst_in_band {
        format!("Harmonic {} of {} lands in {}'s band and, at an assumed {:.0} dBc, costs it {desense_db:.1} dB of sensitivity. Check the real harmonic level. Needs {needed_isolation_db:.0} dB of isolation or a filter on the transmitter.", harmonics[0].0, aggressor.name, victim.name, aggressor.harmonic_dbc)
    } else if desense_db >= 1.0 {
        format!("{}'s out-of-band noise costs {} {desense_db:.1} dB of sensitivity. Needs {needed_isolation_db:.0} dB of isolation.", aggressor.name, victim.name)
    } else if !harmonics.is_empty() {
        format!("Harmonic {} of {} lands in {}'s band, under the noise at an assumed {:.0} dBc. Check the real harmonic level.", harmonics[0].0, aggressor.name, victim.name, aggressor.harmonic_dbc)
    } else {
        format!("{} leaves {} alone at this isolation.", aggressor.name, victim.name)
    };
    let mut isolation_unknown_at_hz = unknown.into_inner();
    isolation_unknown_at_hz.sort_by(f64::total_cmp);
    isolation_unknown_at_hz.dedup();
    let note = if isolation_unknown_at_hz.is_empty() { note } else { format!("{note} The isolation data doesn't reach {:.2} GHz, so its worst value stood in there.", isolation_unknown_at_hz[0] / 1e9) };
    PairResult { aggressor: aggressor.name.clone(), victim: victim.name.clone(), isolation_db: worst_iso, in_band_dbm, desense_db, carrier_dbm, blocked, needed_isolation_db, range_while_on, harmonics, overlap, note, isolation_unknown_at_hz }
}

/// Every transmitter against every other receiver, worst first.
pub fn matrix(radios: &[Radio], isolation: impl Fn(usize, usize) -> Isolation, net: Option<&Network>) -> Vec<PairResult> {
    let mut out = Vec::new();
    for (a, ra) in radios.iter().enumerate() {
        for (v, rv) in radios.iter().enumerate() {
            if a != v {
                out.push(pair(ra, rv, &isolation(a, v), net));
            }
        }
    }
    out.sort_by(|x, y| x.range_while_on.total_cmp(&y.range_while_on));
    out
}

/// Range left for the victim with isolation and chip figures uncertain.
pub fn predict(aggressor: &Radio, victim: &Radio, isolation_db: f64, target: f64, samples: usize) -> RobustReport {
    let factors = vec![
        Factor::new("Antenna isolation", "dB", Dist::Normal { mean: isolation_db, sigma: 3.0 }, "Layout and enclosure can move isolation by several dB"),
        Factor::new("Aggressor out-of-band noise", "dB", Dist::Normal { mean: 0.0, sigma: 4.0 }, "Chip and supply dependent; datasheets rarely give it"),
        Factor::new("Victim noise figure", "dB", Dist::Normal { mean: 0.0, sigma: 1.0 }, "Datasheet spread"),
        Factor::new("Aggressor harmonic level", "dB", Dist::Normal { mean: 0.0, sigma: 6.0 }, "Real parts run about 20 dB either side of what's typical, and filters help; only counts if a harmonic lands in the band"),
    ];
    let metric = format!("{} range while {} transmits", victim.name, aggressor.name);
    robust(&metric, "share of normal", &factors, Some(Target::at_least(target)), Evidence::Estimate, RobustOptions { samples, seed: 31 }, |v| {
        let mut a = aggressor.clone();
        a.tx_edge_dbm_hz += v[1];
        a.tx_noise_floor_dbm_hz += v[1];
        a.harmonic_dbc += v[3];
        let mut r = victim.clone();
        r.noise_figure_db += v[2];
        pair(&a, &r, &Isolation::Flat(v[0]), None).range_while_on
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::C64;

    fn get(n: &str) -> Radio {
        Radio::preset(n).unwrap()
    }

    #[test]
    fn no_interference_means_no_desense() {
        let mut a = get("BLE");
        a.tx_edge_dbm_hz = -300.0;
        a.tx_noise_floor_dbm_hz = -300.0;
        let r = pair(&a, &get("UWB channel 9"), &Isolation::Flat(60.0), None);
        assert!(r.desense_db < 1e-6, "{r:?}");
        assert!(!r.blocked);
        assert!((r.range_while_on - 1.0).abs() < 1e-6);
    }

    #[test]
    fn noise_equal_to_the_floor_costs_three_db() {
        // Hand-built so the aggressor's noise at the victim's input equals its floor exactly.
        let mut v = get("BLE");
        v.rx = (2.44e9, 2.442e9);
        v.rx_bw_hz = 2e6;
        v.noise_figure_db = 6.0;
        let floor = -174.0 + 10.0 * 2e6f64.log10() + 6.0;
        let mut a = get("LTE-M band 13");
        a.tx_noise_floor_dbm_hz = -120.0;
        a.tx_edge_dbm_hz = -120.0;
        a.tx_slope_db_oct = 0.0;
        let iso = -120.0 + 10.0 * 2e6f64.log10() - floor;
        let r = pair(&a, &v, &Isolation::Flat(iso), None);
        assert!((r.desense_db - 3.01).abs() < 0.02, "{r:?}");
        // 3 dB less sensitivity at n = 2.2: range × 10^(−3/22).
        assert!((r.range_while_on - 10f64.powf(-3.01 / 22.0)).abs() < 0.01, "{r:?}");
    }

    #[test]
    fn more_isolation_is_always_at_least_as_good() {
        let (a, v) = (get("WiFi 2.4 GHz"), get("BLE"));
        let mut last = 0.0;
        for iso in [10.0, 20.0, 30.0, 40.0, 60.0] {
            let r = pair(&a, &v, &Isolation::Flat(iso), None);
            assert!(r.range_while_on >= last, "{iso}: {r:?}");
            last = r.range_while_on;
        }
    }

    #[test]
    fn wifi_6e_lands_on_uwb_channel_5_but_clears_channel_9() {
        let wifi = get("WiFi 6E");
        let ch5 = pair(&wifi, &get("UWB channel 5"), &Isolation::Flat(20.0), None);
        let ch9 = pair(&wifi, &get("UWB channel 9"), &Isolation::Flat(20.0), None);
        println!("{ch5:#?}\n{ch9:#?}");
        assert!(ch5.overlap, "6E covers channel 5: {ch5:?}");
        assert!(!ch9.overlap);
        assert!(ch5.desense_db > ch9.desense_db + 10.0, "{} vs {}", ch5.desense_db, ch9.desense_db);
    }

    #[test]
    fn needed_isolation_really_is_enough() {
        for (a, v) in [("WiFi 2.4 GHz", "BLE"), ("UHF RFID reader", "BLE"), ("BLE", "UWB channel 5"), ("LTE-M band 13", "WiFi 2.4 GHz")] {
            let (a, v) = (get(a), get(v));
            let first = pair(&a, &v, &Isolation::Flat(5.0), None);
            let fixed = pair(&a, &v, &Isolation::Flat(first.needed_isolation_db + 0.01), None);
            assert!(fixed.desense_db <= 1.01 && !fixed.blocked, "{} -> {}: {fixed:?}", a.name, v.name);
        }
    }

    #[test]
    fn a_far_off_carrier_gets_credit_for_the_receivers_own_rejection() {
        // BLE at 2.44 GHz against UWB at 6.5 GHz, 20 dB apart: nobody's front end sees
        // that carrier at full strength.
        let far = pair(&get("BLE"), &get("UWB channel 5"), &Isolation::Flat(20.0), None);
        assert!(!far.blocked, "{far:?}");
        assert!(far.carrier_dbm < -40.0, "{far:?}");
        // Inside the band there's nothing to reject it.
        let same = pair(&get("WiFi 2.4 GHz"), &get("BLE"), &Isolation::Flat(20.0), None);
        assert!(same.overlap);
        assert!((same.carrier_dbm - 0.0).abs() < 1e-9, "{same:?}");
        assert_eq!(get("BLE").selectivity_db(2.44e9), 0.0);
        assert!(get("UWB channel 5").selectivity_db(915e6) <= 50.0);
    }

    #[test]
    fn isolation_comes_from_the_sweep_when_given() {
        let f = vec![2.4e9, 2.45e9, 2.5e9];
        let s21 = |db: f64| C64::new(10f64.powf(db / 20.0), 0.0);
        let mk = |db: f64| vec![vec![C64::new(0.1, 0.0), s21(db)], vec![s21(db), C64::new(0.1, 0.0)]];
        let n = Network { f, z0: vec![50.0, 50.0], s: vec![mk(-15.0), mk(-15.0), mk(-15.0)] };
        let r = pair(&get("WiFi 2.4 GHz"), &get("BLE"), &Isolation::Network { i: 0, j: 1 }, Some(&n));
        assert!((r.isolation_db - 15.0).abs() < 1e-6, "{r:?}");
    }

    // OUT-M8: harmonics were only a note: RFID into UWB channel 5 read as 0.8 dB and 0.92.
    #[test]
    fn a_harmonic_in_the_band_counts_in_the_number() {
        let (rfid, uwb) = (get("UHF RFID reader"), get("UWB channel 5"));
        let r = pair(&rfid, &uwb, &Isolation::Flat(40.0), None);
        assert!(r.harmonics.iter().any(|h| h.0 == 7), "{r:?}");
        assert!((r.desense_db - 11.3).abs() < 0.5, "the audit's hand figure: {r:?}");
        assert!((r.range_while_on - 0.31).abs() < 0.02, "{r:?}");
        assert!(r.note.contains("Harmonic 7"), "{}", r.note);
        // A clean transmitter loses the effect, and the old figure is back.
        let clean = Radio { harmonic_dbc: -200.0, ..rfid.clone() };
        let c = pair(&clean, &uwb, &Isolation::Flat(40.0), None);
        assert!((c.desense_db - 0.8).abs() < 0.2 && (c.range_while_on - 0.92).abs() < 0.02, "{c:?}");
        // A saved radio with no harmonic figure reads as typical, not as clean.
        let mut v = serde_json::to_value(&rfid).unwrap();
        v.as_object_mut().unwrap().remove("harmonic_dbc");
        assert_eq!(serde_json::from_value::<Radio>(v).unwrap().harmonic_dbc, -60.0);
    }

    fn sweep(f: Vec<f64>, db: impl Fn(f64) -> f64) -> Network {
        let s = f.iter().map(|x| {
            let v = C64::new(10f64.powf(db(*x) / 20.0), 0.0);
            vec![vec![C64::new(0.1, 0.0), v], vec![v, C64::new(0.1, 0.0)]]
        }).collect();
        Network { f, z0: vec![50.0, 50.0], s }
    }

    // OUT-M8: a 2 to 3 GHz sweep answered for 6.5 GHz with its nearest sample.
    #[test]
    fn isolation_outside_the_sweep_is_unknown_not_the_nearest_sample() {
        let n = sweep(vec![2e9, 2.5e9, 3e9], |f| if f < 2.9e9 { -60.0 } else { -40.0 });
        let at = Isolation::Network { i: 0, j: 1 };
        assert_eq!(at.at(6.5e9, Some(&n)), None);
        assert_eq!(at.at(1.0e9, Some(&n)), None);
        let inside = at.at(2.75e9, Some(&n)).unwrap();
        assert!((inside - 50.0).abs() < 1e-6, "interpolated: {inside}");
        let r = pair(&get("WiFi 2.4 GHz"), &get("UWB channel 5"), &at, Some(&n));
        assert!(!r.isolation_unknown_at_hz.is_empty() && r.note.contains("doesn't reach"), "{r:?}");
        assert_eq!(r.isolation_db, 40.0, "the worst on record, not the 3 GHz sample by luck: {r:?}");
        let covered = sweep(vec![2e9, 7e9], |_| -60.0);
        assert!(pair(&get("WiFi 2.4 GHz"), &get("UWB channel 5"), &at, Some(&covered)).isolation_unknown_at_hz.is_empty());
    }

    // OUT-M8: this fell back to a hidden 20 dB.
    #[test]
    fn no_isolation_data_at_all_is_said_and_taken_as_none() {
        let r = pair(&get("WiFi 2.4 GHz"), &get("BLE"), &Isolation::Network { i: 0, j: 1 }, None);
        assert_eq!(r.isolation_db, 0.0);
        assert!(!r.isolation_unknown_at_hz.is_empty(), "{r:?}");
    }

    #[test]
    fn matrix_is_worst_first_and_reports_harmonics() {
        let radios = vec![get("UHF RFID reader"), get("UWB channel 5"), get("BLE")];
        let m = matrix(&radios, |_, _| Isolation::Flat(25.0), None);
        assert_eq!(m.len(), 6);
        for w in m.windows(2) {
            assert!(w[0].range_while_on <= w[1].range_while_on);
        }
        // The 7th harmonic of 915 MHz is 6.4 GHz, in UWB channel 5.
        let h = m.iter().find(|p| p.aggressor == "UHF RFID reader" && p.victim == "UWB channel 5").unwrap();
        assert!(h.harmonics.iter().any(|x| x.0 == 7), "{h:?}");
    }

    #[test]
    fn prediction_ranks_isolation_and_gives_a_range() {
        let r = predict(&get("WiFi 2.4 GHz"), &get("BLE"), 20.0, 0.9, 200);
        assert!(r.prediction.low <= r.prediction.high);
        assert_eq!(r.prediction.evidence, Evidence::Estimate);
    }
}
