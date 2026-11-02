//! Tag battery life against accuracy. Ratings assume nominal conditions and an update
//! rate someone picked for the datasheet. Here the rate is the choice: faster fixes cost
//! charge, slower fixes lag a moving tag, and the simulator shows what each one does to
//! position error at this site.
//!
//! Charge figures are typical for current chips and are a starting point: measure your
//! tag's current and put the real numbers in.

use bevy::prelude::*;
use proof::{Claim, Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use super::stress::Start;
use crate::rtls::device::{Device, DeviceKind, Profile, Profiles, UwbMode};
use crate::rtls::io::Hub;
use crate::rtls::{RtlsConfig, RtlsState, sim};

/// What one update costs, and what's there to spend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Energy {
    pub label: String,
    pub sleep_ua: f64,
    /// Charge per update, microcoulombs.
    pub update_uc: f64,
    pub capacity_mah: f64,
    /// Share of the rated capacity a tag can really use.
    pub usable: f64,
    /// Lost to self-discharge per year, share of capacity.
    pub self_discharge: f64,
}

/// Typical figures for a tag of this profile. `anchors`: how many a two-way tag ranges with.
pub fn typical(p: &Profile, anchors: usize) -> Energy {
    let capacity_mah = if p.battery_mah > 0.0 { p.battery_mah as f64 } else { 1000.0 };
    let (label, update_uc) = match p.kind {
        // About 1.2 ms of radio at 50 mA per anchor, and the CPU awake around it.
        DeviceKind::UwbTag if p.mode == UwbMode::Twr => ("UWB two-way ranging", 60.0 * anchors.max(1) as f64 + 10.0),
        // A blink: about 0.3 ms at 35 mA, and a short wake-up.
        DeviceKind::UwbTag => ("UWB TDoA blink", 13.5),
        // Three advertising channels at about 5 mA for a few ms.
        DeviceKind::BleTag => ("BLE advertising", 15.0),
        // A short association and frame: orders of magnitude above the others.
        DeviceKind::WifiClient => ("WiFi", 25_000.0),
        _ => ("Other", 20.0),
    };
    Energy { label: label.into(), sleep_ua: 3.0, update_uc, capacity_mah, usable: 0.8, self_discharge: 0.01 }
}

/// Years of life at `hz` while moving and `still_hz` while parked.
pub fn life_years(e: &Energy, hz: f64, moving_share: f64, still_hz: f64) -> f64 {
    let rate = moving_share * hz + (1.0 - moving_share) * still_hz;
    let current_ma = e.sleep_ua / 1000.0 + e.update_uc * rate / 1000.0;
    let charge_mah = e.capacity_mah * e.usable;
    let hours_per_year = 8766.0;
    charge_mah / (current_ma * hours_per_year + e.self_discharge * e.capacity_mah)
}

/// How far a tag has gone since its last fix, 95 % of the time: errors are only scored as each
/// fix arrives, so this is what a slow rate really costs.
pub fn lag_p95_m(speed_mps: f64, hz: f64) -> f32 {
    (speed_mps * 0.95 / hz.max(1e-3)) as f32
}

/// Life across what isn't known.
pub fn predict_life(e: &Energy, hz: f64, moving_share: f64, still_hz: f64, target_years: f64, samples: usize) -> RobustReport {
    let factors = vec![
        Factor::new("Usable capacity", "share", Dist::Uniform { lo: (e.usable - 0.25).max(0.2), hi: (e.usable + 0.1).min(1.0) }, "Cold and pulsed loads take a big share of a coin cell's rating"),
        Factor::new("Charge per update", "×", Dist::LogNormal { median: 1.0, sigma: 0.3 }, "Typical figure; varies by chip, range and retries"),
        Factor::new("Time moving", "share", Dist::Uniform { lo: (moving_share * 0.5).max(0.0), hi: (moving_share * 1.5).min(1.0) }, "Depends on how the asset or person is really used"),
    ];
    let metric = format!("Battery life at {hz} Hz");
    robust(&metric, "years", &factors, Some(Target::at_least(target_years)), Evidence::Estimate, RobustOptions { samples, seed: 101 }, |v| {
        let scaled = Energy { usable: v[0], update_uc: e.update_uc * v[1], ..e.clone() };
        life_years(&scaled, hz, v[2], still_hz)
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateRow {
    pub hz: f64,
    pub life_years: f64,
    pub life_low: f64,
    pub life_high: f64,
    /// Position error with a tag moving: what the simulator reports at each fix, plus how far
    /// it has gone since the last one.
    pub p95_m: Option<f32>,
    /// The simulator's own figure, at the moment of each fix.
    pub at_fix_p95_m: Option<f32>,
    pub lag_m: f32,
    /// Tags with a position when the run ended. A tag that's lost has no error to count, so
    /// the p95 alone could pass with most of them gone.
    pub located: usize,
    pub tags: usize,
    pub chance_of_life_target: f64,
    pub meets_accuracy: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatteryReport {
    pub profile: String,
    pub energy: Energy,
    pub rows: Vec<RateRow>,
    pub recommended_hz: Option<f64>,
    pub claim: Option<Claim>,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct BatteryOptions {
    pub profile: Option<String>,
    pub rates: Vec<f64>,
    pub moving_share: f64,
    pub still_hz: f64,
    pub target_m: f32,
    pub target_years: f64,
    /// How fast tags move when they do.
    pub speed_mps: f64,
    pub seconds: f64,
    pub capacity_mah: Option<f64>,
    pub update_uc: Option<f64>,
}

/// Runs the site at each rate. Leaves profiles and tags as they were.
pub fn run(world: &mut World, o: BatteryOptions, scene: &str) -> Result<BatteryReport, String> {
    let tags: Vec<String> = world.query::<&Device>().iter(world).filter(|d| d.enabled && d.kind.is_mobile()).map(|d| d.profile.clone()).collect();
    let id = o.profile.clone().or_else(|| tags.iter().find(|p| p.starts_with("uwb")).or(tags.first()).cloned()).ok_or("no tags in the scene: place one, or name a profile")?;
    let Some(profile) = world.resource::<Profiles>().get(&id).cloned() else { return Err(format!("no profile '{id}'")) };
    let anchors = world.query::<&Device>().iter(world).filter(|d| d.enabled && d.kind == DeviceKind::UwbAnchor).count().clamp(0, 6);
    let mut energy = typical(&profile, if anchors == 0 { 4 } else { anchors });
    if let Some(c) = o.capacity_mah {
        energy.capacity_mah = c;
    }
    if let Some(q) = o.update_uc {
        energy.update_uc = q;
    }
    let tech = profile.kind.tech();
    let start = Start::take(world);
    let was_muted = world.resource::<Hub>().muted;
    world.resource_mut::<Hub>().muted = true;
    let mut rows = Vec::new();
    for hz in &o.rates {
        if let Some(p) = world.resource_mut::<Profiles>().0.iter_mut().find(|p| p.id == id) {
            p.update_hz = *hz as f32;
        }
        start.restore(world);
        let seed = world.resource::<RtlsConfig>().settings.seed;
        world.resource_mut::<RtlsState>().restart(seed);
        sim::run_for(world, o.seconds);
        let st = world.resource::<RtlsState>();
        let (_, _, p95, _) = st.error_stats(Some(tech));
        let tags = st.tracks.values().filter(|t| t.tech == tech).count();
        // By whether it has a position: at a slow rate a track is marked lost between fixes.
        let located = st.tracks.values().filter(|t| t.tech == tech && t.est.is_some()).count();
        let life = predict_life(&energy, *hz, o.moving_share, o.still_hz, o.target_years, 400);
        let at_fix_p95_m = (p95 > 0.0).then_some(p95);
        let lag_m = lag_p95_m(o.speed_mps, *hz);
        let p95_m = at_fix_p95_m.map(|v| v + lag_m);
        rows.push(RateRow { hz: *hz, life_years: life.prediction.median, life_low: life.prediction.low, life_high: life.prediction.high, p95_m, at_fix_p95_m, lag_m, located, tags, chance_of_life_target: life.pass_probability.unwrap_or(0.0), meets_accuracy: tags > 0 && located == tags && p95_m.is_some_and(|v| v <= o.target_m) });
    }
    if let Some(p) = world.resource_mut::<Profiles>().0.iter_mut().find(|p| p.id == id) {
        p.update_hz = profile.update_hz;
    }
    start.restore(world);
    let seed = world.resource::<RtlsConfig>().settings.seed;
    world.resource_mut::<RtlsState>().restart(seed);
    world.resource_mut::<Hub>().muted = was_muted;

    // The slowest rate that holds the accuracy target gets the most life.
    let chosen = o.rates.iter().copied().zip(&rows).filter(|(_, r)| r.meets_accuracy).map(|(h, _)| h).fold(None, |a: Option<f64>, h| Some(a.map_or(h, |x| x.min(h))));
    let claim = chosen.map(|hz| {
        let r = predict_life(&energy, hz, o.moving_share, o.still_hz, o.target_years, 400);
        Claim::from_report(&format!("battery:{scene}:{id}"), "tag_battery", &format!("{} tag life at {hz} Hz, {scene}", energy.label), &r, Some(Target::at_least(o.target_years)), vec!["Charge per update and battery derating are typical figures: measure your tag's current to replace them".into(), format!("{:.0} % of its time moving, and {} Hz while parked", o.moving_share * 100.0, o.still_hz), format!("Position error is the simulator's at each fix plus how far a tag moving at {} m/s goes between fixes", o.speed_mps)])
    });
    let summary = match chosen.and_then(|h| rows.iter().find(|r| r.hz == h)) {
        Some(r) => {
            let short = if r.life_years < o.target_years { format!(" That's short of the {} year target: a slower rate while parked, a bigger cell or a method that costs less per update, such as TDoA, is what to try next.", o.target_years) } else { String::new() };
            format!("{} Hz is the slowest rate tried that holds {:.0} cm here, for about {:.1} years (range {:.1} to {:.1}).{short} Rate-versus-accuracy comes from one simulated run per rate, with every tag located; life comes from typical charge figures.", r.hz, o.target_m * 100.0, r.life_years, r.life_low, r.life_high)
        }
        None => format!("None of the rates tried holds {:.0} cm with every tag located, so update rate isn't what limits this site.", o.target_m * 100.0),
    };
    Ok(BatteryReport { profile: id, energy, rows, recommended_hz: chosen, claim, summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e() -> Energy {
        Energy { label: "t".into(), sleep_ua: 3.0, update_uc: 100.0, capacity_mah: 1000.0, usable: 1.0, self_discharge: 0.0 }
    }

    #[test]
    fn life_follows_the_charge_budget() {
        // 1 Hz of 100 µC is 100 µA, plus 3 µA asleep: 1000 mAh / 0.103 mA = 9709 h.
        let years = life_years(&e(), 1.0, 1.0, 0.0);
        assert!((years - 1000.0 / 0.103 / 8766.0).abs() < 1e-6, "{years}");
    }

    #[test]
    fn a_slow_rate_costs_a_moving_tag_its_accuracy() {
        // Walking pace at 0.5 Hz: 2.47 m gone by the 95th percentile, whatever the radio does.
        assert!((lag_p95_m(1.3, 0.5) - 2.47).abs() < 1e-3);
        assert!(lag_p95_m(1.3, 10.0) < 0.13);
        assert_eq!(lag_p95_m(0.0, 0.5), 0.0);
    }

    #[test]
    fn slowing_down_when_parked_buys_years() {
        let always = life_years(&e(), 1.0, 1.0, 1.0);
        let parked = life_years(&e(), 1.0, 0.1, 0.01);
        assert!(parked > 5.0 * always, "{parked} vs {always}");
    }

    #[test]
    fn self_discharge_caps_a_sleeping_tag() {
        let lazy = Energy { self_discharge: 0.1, ..e() };
        assert!(life_years(&lazy, 0.0, 0.0, 0.0) < 10.0);
    }

    #[test]
    fn two_way_costs_more_than_a_blink_and_grows_with_anchors() {
        let twr = Profile { kind: DeviceKind::UwbTag, mode: UwbMode::Twr, ..Default::default() };
        let blink = Profile { kind: DeviceKind::UwbTag, mode: UwbMode::Tdoa, ..Default::default() };
        assert!(typical(&twr, 4).update_uc > 10.0 * typical(&blink, 4).update_uc);
        assert!(typical(&twr, 6).update_uc > typical(&twr, 4).update_uc);
    }

    #[test]
    fn prediction_is_an_estimate_with_a_range_and_a_driver() {
        let r = predict_life(&typical(&Profile { kind: DeviceKind::BleTag, ..Default::default() }, 0), 1.0, 0.3, 0.1, 3.0, 300);
        assert_eq!(r.prediction.evidence, Evidence::Estimate);
        assert!(r.prediction.low < r.prediction.high);
        assert!(r.main_driver().is_some());
    }
}
