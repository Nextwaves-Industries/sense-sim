//! Which technology for this site? Guides quote general figures that don't even agree.
//! Here each one is deployed on the real layout and simulated, then costed with your
//! prices over the years you'll own it. Prices left out are placeholders, and the
//! answer says so: replace them with real quotes before deciding anything.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::acceptance::{self, AcceptancePlan};
use super::battery;
use super::stress::Start;
use crate::rtls::deploy::{self, DeployRequest};
use crate::rtls::device::{Device, DeviceKind, Profiles, Zone};
use crate::rtls::io::Hub;
use crate::rtls::{RtlsConfig, RtlsState};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prices {
    pub uwb_anchor: f64,
    pub uwb_tag: f64,
    pub ble_node: f64,
    pub ble_locator: f64,
    pub ble_tag: f64,
    pub wifi_ap: f64,
    pub wifi_tag: f64,
    pub rfid_reader: f64,
    pub rfid_tag: f64,
    /// Fitting and cabling one fixed device.
    pub install_per_device: f64,
    /// Changing one tag's battery or the tag, parts and labour.
    pub tag_service: f64,
    /// Site software or support per year, per fixed device.
    pub annual_per_device: f64,
}

impl Default for Prices {
    /// Placeholders so a comparison can run at all. Not quotes.
    fn default() -> Self {
        Self { uwb_anchor: 150.0, uwb_tag: 35.0, ble_node: 40.0, ble_locator: 250.0, ble_tag: 8.0, wifi_ap: 400.0, wifi_tag: 40.0, rfid_reader: 1500.0, rfid_tag: 0.15, install_per_device: 120.0, tag_service: 15.0, annual_per_device: 20.0 }
    }
}

/// What a technology costs over its life. Passive tags (`life` `None`) need no service.
pub fn cost(fixed: usize, fixed_price: f64, tags: usize, tag_price: f64, life_years: Option<f64>, years: f64, p: &Prices) -> (f64, f64) {
    let up_front = fixed as f64 * (fixed_price + p.install_per_device) + tags as f64 * tag_price;
    let services = life_years.map_or(0.0, |l| (years / l.max(0.01)).floor());
    let running = fixed as f64 * p.annual_per_device * years + tags as f64 * services * p.tag_service;
    (up_front, running)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TechRow {
    pub tech: String,
    pub fixed_devices: usize,
    pub coverage: f32,
    /// Simulated 95th percentile error at the shared test points. `None` where it only reads
    /// (RFID), or too many points had no position to give one.
    pub p95_m: Option<f32>,
    /// Test points where it gave a position. One with none counts against the p95.
    pub points_located: usize,
    pub points_tested: usize,
    pub meets_target: bool,
    pub tag_life_years: Option<f64>,
    pub up_front: f64,
    pub running: f64,
    pub total: f64,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompareReport {
    pub rows: Vec<TechRow>,
    pub tags: usize,
    pub years: f64,
    pub target_m: f32,
    pub prices_are_placeholders: bool,
    pub evidence: String,
    pub cheapest_meeting_target: Option<String>,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct CompareOptions {
    pub techs: Vec<String>,
    pub target_m: f32,
    pub tags: usize,
    pub years: f64,
    pub prices: Prices,
    pub prices_given: bool,
    pub test_s: f64,
    pub moving_share: f64,
    /// Updates per second while moving, for battery life, the same for every technology.
    pub report_hz: f64,
    /// Spots a tag stands at, the same ones for every technology.
    pub points: usize,
    pub settle_s: f64,
}

/// Deploys each technology in turn. Your own devices stay; what auto deploy had placed
/// is put back afterwards.
pub fn run(world: &mut World, o: &CompareOptions) -> CompareReport {
    // Auto deploy tests each technology with different tags on different paths, so the same
    // spots are what make them comparable.
    let points = acceptance::draw(world, o.points, 1.2, 2.0, 7).unwrap_or_default();
    let saved_devices: Vec<(Device, Transform)> = world.query::<(&Device, &Transform)>().iter(world).filter(|(d, _)| d.auto).map(|(d, t)| (d.clone(), *t)).collect();
    let saved_zones: Vec<Zone> = world.query::<&Zone>().iter(world).filter(|z| z.auto).cloned().collect();
    let start = Start::take(world);
    let was_muted = world.resource::<Hub>().muted;
    world.resource_mut::<Hub>().muted = true;
    let mut rows = Vec::new();
    for tech in &o.techs {
        let base = DeployRequest { uwb: false, ble: false, rfid: false, ble_aoa: false, wifi: false, target_m: o.target_m, test_s: o.test_s, workers: 1, ..Default::default() };
        let req = match tech.as_str() {
            "uwb" => DeployRequest { uwb: true, ..base },
            "ble" => DeployRequest { ble: true, ..base },
            "ble_aoa" => DeployRequest { ble: true, ble_aoa: true, ..base },
            "wifi" => DeployRequest { wifi: true, ..base },
            "rfid" => DeployRequest { rfid: true, ..base },
            _ => continue,
        };
        let r = deploy::auto_deploy(world, &req);
        // All that's there now, whoever placed it: it all has to be bought and fitted.
        let infra = match tech.as_str() {
            "uwb" => DeviceKind::UwbAnchor,
            "ble" | "ble_aoa" => DeviceKind::BleNode,
            "wifi" => DeviceKind::WifiAp,
            _ => DeviceKind::RfidReader,
        };
        let fixed = world.query::<&Device>().iter(world).filter(|d| d.enabled && d.kind == infra && !d.profile.contains("esl")).count();
        let plan_coverage = r.plans.first().map_or(r.coverage, |p| p.coverage);
        let p = &o.prices;
        let (fixed_price, tag_price, tag_kind, profile) = match tech.as_str() {
            "uwb" => (p.uwb_anchor, p.uwb_tag, Some(DeviceKind::UwbTag), "uwb-tag"),
            "ble" => (p.ble_node, p.ble_tag, Some(DeviceKind::BleTag), "ble-tag"),
            "ble_aoa" => (p.ble_locator, p.ble_tag, Some(DeviceKind::BleTag), "ble-tag"),
            "wifi" => (p.wifi_ap, p.wifi_tag, Some(DeviceKind::WifiClient), "wifi-client"),
            _ => (p.rfid_reader, p.rfid_tag, None, "rfid-tag"),
        };
        let life = tag_kind.and_then(|_| world.resource::<Profiles>().get(profile).cloned()).map(|tag| {
            let e = battery::typical(&tag, 4);
            battery::life_years(&e, o.report_hz, o.moving_share, o.report_hz / 10.0)
        });
        let (p95, located) = if tech == "rfid" {
            (None, 0)
        } else {
            let sim_tech = match tech.as_str() {
                "uwb" => "uwb",
                "wifi" => "wifi",
                _ => "ble",
            };
            let plan = AcceptancePlan { scene: String::new(), tech: sim_tech.into(), target_m: o.target_m, pass_share: 0.95, confidence: 0.95, points: points.clone(), seed: 7 };
            match acceptance::preview(world, &plan, o.settle_s) {
                Ok(results) => {
                    let s = acceptance::score(&plan, &results, None);
                    (s.p95_m.map(|v| v as f32), s.points.iter().filter(|p| p.error_m.is_some()).count())
                }
                Err(_) => (None, 0),
            }
        };
        let (up_front, running) = cost(fixed, fixed_price, o.tags, tag_price, life, o.years, p);
        let note = if tech == "rfid" {
            "Reads what passes a reader or sits on a shelf: no position to compare against the target".to_string()
        } else if fixed == 0 {
            "Couldn't place any devices".to_string()
        } else if p95.is_none() {
            format!("Only {located} of {} test points got a position, too few to give a 95th percentile", points.len())
        } else if located < points.len() {
            format!("{} of {} test points got no position", points.len() - located, points.len())
        } else {
            String::new()
        };
        rows.push(TechRow { tech: tech.clone(), fixed_devices: fixed, coverage: plan_coverage, p95_m: p95, points_located: located, points_tested: points.len(), meets_target: p95.is_some_and(|v| v <= o.target_m) && fixed > 0, tag_life_years: life, up_front, running, total: up_front + running, note });
    }

    // Put back what auto deploy had placed.
    let auto: Vec<Entity> = {
        let mut q = world.query::<(Entity, Option<&Device>, Option<&Zone>)>();
        q.iter(world).filter(|(_, d, z)| d.is_some_and(|d| d.auto) || z.is_some_and(|z| z.auto)).map(|(e, ..)| e).collect()
    };
    for e in auto {
        world.despawn(e);
    }
    for (d, t) in saved_devices {
        world.spawn((Name::new(format!("{} {}", d.kind.label(), d.id)), d, t));
    }
    for z in saved_zones {
        world.spawn((Name::new(format!("Zone {}", z.name)), z));
    }
    world.flush();
    start.restore(world);
    let seed = world.resource::<RtlsConfig>().settings.seed;
    world.resource_mut::<RtlsState>().restart(seed);
    world.resource_mut::<Hub>().muted = was_muted;

    let best = rows.iter().filter(|r| r.meets_target).min_by(|a, b| a.total.total_cmp(&b.total));
    let cheapest_meeting_target = best.map(|r| r.tech.clone());
    let summary = match best {
        Some(b) => format!("{} is the cheapest that holds {:.0} cm here: about {:.0} over {} years for {} tags, with {} fixed devices. {}", b.tech, o.target_m * 100.0, b.total, o.years, o.tags, b.fixed_devices, if o.prices_given { "From your prices." } else { "These are placeholder prices, so give real quotes before deciding." }),
        None => format!("Nothing tried holds {:.0} cm on this layout in simulation.", o.target_m * 100.0),
    };
    let summary = format!("{summary} Battery life assumes {} updates per second while moving; accuracy is of a tag standing still at the same {} spots for each.", o.report_hz, points.len());
    CompareReport { rows, tags: o.tags, years: o.years, target_m: o.target_m, prices_are_placeholders: !o.prices_given, evidence: "Simulated for accuracy: one tag standing still at the same spots for each technology, one run each. Battery life is an estimate from typical figures, and costs are only as good as the prices. Not validated on hardware".into(), cheapest_meeting_target, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_add_up() {
        let p = Prices { install_per_device: 100.0, annual_per_device: 10.0, tag_service: 5.0, ..Default::default() };
        // 10 fixed at 200 + 100 fit, 100 tags at 8; running: 10·10·6 + 100 tags serviced 3× at 5.
        let (up, run) = cost(10, 200.0, 100, 8.0, Some(2.0), 6.0, &p);
        assert_eq!(up, 3000.0 + 800.0);
        assert_eq!(run, 600.0 + 1500.0);
    }

    #[test]
    fn passive_tags_need_no_service_and_long_lives_need_none_yet() {
        let p = Prices::default();
        let (_, none) = cost(0, 0.0, 1000, 0.1, None, 5.0, &p);
        assert_eq!(none, 0.0);
        let (_, long) = cost(0, 0.0, 1000, 8.0, Some(10.0), 5.0, &p);
        assert_eq!(long, 0.0, "a 10-year tag isn't serviced in 5 years");
    }

    #[test]
    fn prices_read_with_gaps_filled_in() {
        let p: Prices = serde_json::from_str(r#"{"uwb_anchor": 99}"#).unwrap();
        assert_eq!(p.uwb_anchor, 99.0);
        assert_eq!(p.ble_tag, Prices::default().ble_tag);
    }
}
