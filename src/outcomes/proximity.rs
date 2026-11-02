//! Forklift and pedestrian alerts, tested at the corners where they matter. A vehicle and
//! a person head for the same spot in the real layout; the alert has to fire early enough
//! for the vehicle to stop. Blocked radio paths, ranging delays from sharing airtime with
//! everyone nearby, and the alarms a person safely off to the side sets off all count.
//!
//! This supports a design. It can't show that a system is safe: that takes the system's
//! own safety validation on real vehicles.

use bevy::prelude::*;
use proof::{Claim, Evidence, Prediction, Target, Verdict};
use serde::{Deserialize, Serialize};

use crate::rtls::device::{Device, DeviceKind, Profile, Profiles};
use crate::rtls::radio;
use crate::rtls::rng::Rng;
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::scene::PlacedObject;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vehicle {
    pub speed_mps: f32,
    pub decel_mps2: f32,
    pub reaction_s: f32,
    pub walker_mps: f32,
    /// People within range who need ranging in turn.
    pub workers: usize,
    pub update_hz: f32,
    /// Airtime one person's ranging takes, seconds.
    pub slot_s: f32,
    /// Extra distance kept in hand, metres.
    pub margin_m: f32,
}

impl Vehicle {
    /// Between two fresh ranges to any one person.
    pub fn cycle_s(&self) -> f32 {
        (1.0 / self.update_hz.max(0.1)).max(self.workers as f32 * self.slot_s)
    }

    /// Time before the shared spot that braking has to start.
    pub fn lead_needed_s(&self) -> f32 {
        self.reaction_s + self.speed_mps / (2.0 * self.decel_mps2.max(0.1))
    }

    /// How far apart they are when braking has to start, with a shared-spot approach. Two
    /// cycles: one for the next range to come round, and one for it to be acted on.
    pub fn alarm_distance_m(&self) -> f32 {
        let t = self.lead_needed_s() + 2.0 * self.cycle_s();
        ((self.speed_mps * t).powi(2) + (self.walker_mps * t).powi(2)).sqrt() + self.margin_m
    }
}

/// How long before the shared spot the alarm first fired, from ranges in time order as
/// (time left to the spot, range measured).
pub fn first_alarm_lead(samples: &[(f32, Option<f32>)], alarm_m: f32) -> Option<f32> {
    samples.iter().find(|(_, r)| r.is_some_and(|v| v <= alarm_m)).map(|s| s.0)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    pub encounters: usize,
    pub in_time: usize,
    pub blind: usize,
    pub blind_in_time: usize,
    pub open_in_time: usize,
    pub never_alarmed: usize,
    pub bystander_passes: usize,
    pub bystander_alarms: usize,
}

/// Runs the encounters on the real layout.
pub fn encounters_system(In((v, count, seed, unit, tag)): In<(Vehicle, usize, u64, Profile, Profile)>, parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>) -> Tally {
    let occ = Occluders::build(&parts, &objects);
    let Some((lo, hi)) = occ.bounds() else { return Tally::default() };
    let mut rng = Rng::new(seed);
    let mut t = Tally::default();
    let (y, lead, alarm, cycle) = (lo.y + 1.2, v.lead_needed_s(), v.alarm_distance_m(), v.cycle_s());
    // Start a little beyond where the alarm should fire.
    let horizon = 1.25 * alarm / (v.speed_mps.powi(2) + v.walker_mps.powi(2)).sqrt().max(0.1);
    let clear = |a: Vec3, b: Vec3| occ.path(a, b, None).walls == 0;
    let in_site = |p: Vec3| p.x >= lo.x && p.x <= hi.x && p.z >= lo.z && p.z <= hi.z;
    let axes = [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z];
    let mut attempts = 0usize;
    while t.encounters < count && attempts < count * 200 {
        attempts += 1;
        let spot = Vec3::new(rng.range(lo.x, hi.x), y, rng.range(lo.z, hi.z));
        let u = axes[(rng.next_u64() % 4) as usize];
        let w = Vec3::new(u.z, 0.0, u.x) * if rng.chance(0.5) { 1.0 } else { -1.0 };
        let (far_v, far_w) = (spot - u * v.speed_mps * horizon, spot - w * v.walker_mps * horizon);
        // Both really can walk these lines, as in an aisle.
        if !in_site(far_v) || !in_site(far_w) || occ.inside(spot) || !clear(far_v, spot) || !clear(far_w, spot) {
            continue;
        }
        t.encounters += 1;
        let blind = !clear(spot - u * v.speed_mps * lead, spot - w * v.walker_mps * lead);
        if blind {
            t.blind += 1;
        }
        let mut samples = Vec::new();
        let mut left = horizon;
        // A range arrives at a random point in its cycle.
        left -= rng.uniform() * cycle;
        while left > 0.0 {
            let (a, b) = (spot - u * v.speed_mps * left, spot - w * v.walker_mps * left);
            let r = radio::uwb_range(&unit, &tag, occ.path(a, b, None).in_band(unit.freq_mhz), &mut rng);
            // The reading is one cycle stale by the time it's acted on.
            samples.push((left - cycle, r));
            left -= cycle;
        }
        match first_alarm_lead(&samples, alarm) {
            Some(l) if l >= lead => {
                t.in_time += 1;
                if blind {
                    t.blind_in_time += 1;
                } else {
                    t.open_in_time += 1;
                }
            }
            Some(_) => {}
            None => t.never_alarmed += 1,
        }
    }
    // Someone at work 2 to 15 m to the side of a clear run, in no danger from it.
    let mut attempts = 0usize;
    while t.bystander_passes < count && attempts < count * 100 {
        attempts += 1;
        let spot = Vec3::new(rng.range(lo.x, hi.x), y, rng.range(lo.z, hi.z));
        let u = axes[(rng.next_u64() % 4) as usize];
        let side = Vec3::new(u.z, 0.0, u.x) * if rng.chance(0.5) { 1.0 } else { -1.0 };
        let person = spot + side * rng.range(2.0, 15.0);
        let far = spot - u * v.speed_mps * horizon;
        if !in_site(person) || !in_site(far) || occ.inside(spot) || occ.inside(person) || !clear(far, spot) {
            continue;
        }
        t.bystander_passes += 1;
        let mut left = horizon;
        let mut alarmed = false;
        while left > 0.0 && !alarmed {
            let a = spot - u * v.speed_mps * left;
            alarmed = radio::uwb_range(&unit, &tag, occ.path(a, person, None).in_band(unit.freq_mhz), &mut rng).is_some_and(|r| r <= alarm);
            left -= cycle;
        }
        if alarmed {
            t.bystander_alarms += 1;
        }
    }
    t
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProximityReport {
    pub tally: Tally,
    pub alarm_distance_m: f32,
    pub lead_needed_s: f32,
    pub cycle_s: f32,
    pub in_time_share: f64,
    /// `None` when the layout had no blind corners to test.
    pub blind_in_time_share: Option<f64>,
    /// `None` when the layout had no clear run to test it on.
    pub bystander_alarm_share: Option<f64>,
    pub claim: Claim,
    pub summary: String,
}

pub fn run(world: &mut World, v: Vehicle, count: usize, min_detect: f64, scene: &str) -> Result<ProximityReport, String> {
    let (unit, tag) = {
        let p = world.resource::<Profiles>();
        (p.default_for(DeviceKind::UwbAnchor).cloned().unwrap_or_default(), p.default_for(DeviceKind::UwbTag).cloned().unwrap_or_default())
    };
    let tally = world.run_system_cached_with(encounters_system, (v, count, 131, unit, tag)).map_err(|e| e.to_string())?;
    if tally.encounters == 0 {
        return Err("no places in this layout where a vehicle and a person can both run up to the same spot with room to stop. It may be too small, or not built yet.".into());
    }
    let share = tally.in_time as f64 / tally.encounters as f64;
    let (low, high) = proof::stats::interval(tally.encounters, tally.in_time, 0.90);
    let verdict = if low >= min_detect {
        Verdict::Likely
    } else if high < min_detect {
        Verdict::Unlikely
    } else {
        Verdict::Uncertain
    }
    .capped(Evidence::Simulated, false);
    let claim = Claim {
        id: format!("proximity:{scene}"),
        recipe: "proximity_alerts".into(),
        subject: format!("Forklift alerts in time, {scene}"),
        prediction: Prediction { metric: "Encounters alerted in time to stop".into(), unit: "share".into(), median: share, low, high, evidence: Evidence::Simulated },
        target: Some(Target::at_least(min_detect)),
        pass_probability: None,
        verdict,
        drivers: if tally.blind > 0 && (tally.blind_in_time as f64 / tally.blind as f64) < (tally.open_in_time as f64 / (tally.encounters - tally.blind).max(1) as f64) { vec!["Blind corners".into()] } else { Vec::new() },
        assumptions: vec![
            "This supports a design and can't show a system is safe: that takes safety validation on real vehicles".into(),
            "A blocked link gives no range at all here. Real reflections may give late or long ranges instead, so blind-corner numbers could go either way".into(),
            format!("{:.1} m/s, {:.1} m/s² braking, {:.1} s to react, and {} people sharing the airtime", v.speed_mps, v.decel_mps2, v.reaction_s, v.workers),
        ],
        checks: Vec::new(),
        created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    };
    let blind_share = (tally.blind > 0).then(|| tally.blind_in_time as f64 / tally.blind as f64);
    let bystander = (tally.bystander_passes > 0).then(|| tally.bystander_alarms as f64 / tally.bystander_passes as f64);
    let summary = format!(
        "{:.1} % of {} encounters alerted in time{}, needing an alarm at {:.1} m. {}",
        share * 100.0,
        tally.encounters,
        blind_share.map_or(String::new(), |b| format!(" ({:.1} % at {} blind corners)", b * 100.0, tally.blind)),
        v.alarm_distance_m(),
        bystander.map_or("There was no clear run to test false alarms on.".to_string(), |b| format!("{:.0} % of passes alarm for someone at work 2 to 15 m to the side, which is how drivers learn to ignore it.", b * 100.0))
    );
    Ok(ProximityReport { alarm_distance_m: v.alarm_distance_m(), lead_needed_s: v.lead_needed_s(), cycle_s: v.cycle_s(), in_time_share: share, blind_in_time_share: blind_share, bystander_alarm_share: bystander, tally, claim, summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v() -> Vehicle {
        Vehicle { speed_mps: 2.5, decel_mps2: 1.5, reaction_s: 1.0, walker_mps: 1.4, workers: 10, update_hz: 10.0, slot_s: 0.002, margin_m: 0.5 }
    }

    #[test]
    fn stopping_follows_the_physics() {
        // 2.5 m/s at 1.5 m/s²: 2.08 m to brake, plus 2.5 m reacting, 4.58 m in all.
        let x = v();
        assert!((x.lead_needed_s() * x.speed_mps - 4.583).abs() < 0.01, "{}", x.lead_needed_s());
        // Faster needs longer, and the distance grows with it.
        let quick = Vehicle { speed_mps: 4.0, ..x };
        assert!(quick.lead_needed_s() > x.lead_needed_s());
        assert!(quick.alarm_distance_m() > x.alarm_distance_m());
    }

    #[test]
    fn crowds_stretch_the_ranging_cycle() {
        let x = v();
        assert!((x.cycle_s() - 0.1).abs() < 1e-6, "the update rate sets it with few people");
        let crowd = Vehicle { workers: 200, ..x };
        assert!((crowd.cycle_s() - 0.4).abs() < 1e-6, "{}", crowd.cycle_s());
        assert!(crowd.alarm_distance_m() > x.alarm_distance_m());
    }

    #[test]
    fn the_first_reading_under_the_line_is_the_alarm() {
        let s = [(5.0, None), (4.0, Some(9.0)), (3.0, Some(6.0)), (2.0, Some(4.0))];
        assert_eq!(first_alarm_lead(&s, 7.0), Some(3.0));
        assert_eq!(first_alarm_lead(&s, 1.0), None);
        assert_eq!(first_alarm_lead(&[(5.0, None)], 7.0), None);
    }
}
