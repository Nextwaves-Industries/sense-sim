//! The site on its bad days. The same scene, run through what really changes: racks
//! fuller or emptier than planned, people in the aisles, an anchor down, materials
//! worse than assumed. The answer is a range and the conditions that break it.
//!
//! Every run starts from the same tag positions, so differences between scenarios
//! are the scenario's, nothing goes out to live outputs, and the scene ends as it began.

use bevy::prelude::*;
use proof::{Claim, Dist, Evidence, Factor, RobustOptions, Target, robust_mut};
use serde::{Deserialize, Serialize};

use crate::rtls::device::{Device, MotionState};
use crate::rtls::io::Hub;
use crate::rtls::radio::{self, CalBand, CalSource, Material, MaterialCal, Origin, SceneBinding, SiteConditions};
use crate::rtls::{RtlsConfig, RtlsState, sim};
use crate::scene::PlacedObject;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub name: String,
    /// UWB error (m); `None` when no tag got a position at all.
    pub p95_m: Option<f32>,
    pub located: usize,
    pub tags: usize,
    pub rfid_reads: u64,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StressReport {
    pub target_m: f32,
    pub seconds: f64,
    pub scenarios: Vec<ScenarioResult>,
    pub claim: Option<Claim>,
    /// Random combinations where no tag got a position. They count against the target.
    pub combinations_with_no_position: usize,
    pub combinations: usize,
    /// Plain-language finding.
    pub summary: String,
}

/// Where everything stood when the test began.
pub(super) struct Start {
    devices: Vec<(Entity, Transform, Option<MotionState>)>,
}

impl Start {
    pub(super) fn take(world: &mut World) -> Self {
        let mut q = world.query_filtered::<(Entity, &Transform, Option<&MotionState>), With<Device>>();
        Self { devices: q.iter(world).map(|(e, t, m)| (e, *t, m.cloned())).collect() }
    }

    pub(super) fn restore(&self, world: &mut World) {
        for (e, t, m) in &self.devices {
            let Ok(mut entity) = world.get_entity_mut(*e) else { continue };
            entity.insert(*t);
            match m {
                Some(m) => {
                    entity.insert(m.clone());
                }
                None => {
                    entity.remove::<MotionState>();
                }
            }
        }
    }
}

/// Runs the sim for `seconds` from the starting positions and reads the result.
/// `reseed` moves it off the settings' seed, so runs don't all share one noise draw.
fn measure(world: &mut World, start: &Start, seconds: f64, reseed: u64) -> (Option<f32>, usize, usize, u64) {
    start.restore(world);
    let seed = world.resource::<RtlsConfig>().settings.seed.wrapping_add(reseed);
    world.resource_mut::<RtlsState>().restart(seed);
    sim::run_for(world, seconds);
    let st = world.resource::<RtlsState>();
    let (_, _, p95, _) = st.error_stats(Some("uwb"));
    let uwb_tracks = st.tracks.values().filter(|t| t.tech == "uwb").count();
    let located = st.tracks.values().filter(|t| t.tech == "uwb" && !t.lost).count();
    let reads: u64 = st.tracks.values().filter(|t| t.tech == "rfid").map(|t| t.reads as u64).sum();
    ((uwb_tracks > 0 && p95 > 0.0).then_some(p95), located, uwb_tracks, reads)
}

fn with_conditions(world: &mut World, start: &Start, c: SiteConditions, seconds: f64) -> (Option<f32>, usize, usize, u64) {
    reseeded(world, start, c, seconds, 0)
}

fn reseeded(world: &mut World, start: &Start, c: SiteConditions, seconds: f64, reseed: u64) -> (Option<f32>, usize, usize, u64) {
    let before = SiteConditions::current();
    SiteConditions::set(c);
    let out = measure(world, start, seconds, reseed);
    SiteConditions::set(before);
    out
}

/// A combination's error, for the verdict. A tag with no fix has no position to be right
/// about, so one lost tag fails the combination however well the others did.
fn combination_error(p95_m: Option<f32>, located: usize, tags: usize) -> f64 {
    match p95_m {
        Some(e) if located >= tags => e as f64,
        _ => f64::INFINITY,
    }
}

/// The named conditions, for the claim. They aren't in its verdict, which only counts
/// the random combinations, and each is a single run.
fn scenario_notes(scenarios: &[ScenarioResult], target_m: f32) -> Vec<String> {
    scenarios
        .iter()
        .map(|s| {
            let result = if s.located >= s.tags && s.p95_m.is_some_and(|e| e <= target_m) { "meets the target" } else { "misses the target" };
            let error = s.p95_m.map_or("no error figure".to_string(), |e| format!("{:.0} cm", e * 100.0));
            format!("Outside this verdict, one run of \"{}\": {error}, {} of {} tags located, {result}", s.name, s.located, s.tags)
        })
        .collect()
}

/// Sets a worse-than-assumed scale on every obstructing material, on top of whatever is
/// in force in each band, returning the what-ifs it replaced.
pub(super) fn scale_materials(k: f32) -> Vec<(Material, Option<MaterialCal>)> {
    Material::ALL
        .iter()
        .map(|m| {
            let old = MaterialCal::applied(*m);
            let (l, b, relative) = old.as_ref().map_or((1.0, 1.0, true), |o| (o.loss_scale, o.bias_scale, o.source.relative));
            MaterialCal::set(*m, Some(MaterialCal { loss_scale: l * k, bias_scale: b * k, label: "Stress test".into(), source: CalSource { relative, ..Default::default() } }));
            (*m, old)
        })
        .collect()
}

pub(super) fn restore_materials(saved: Vec<(Material, Option<MaterialCal>)>) {
    for (m, c) in saved {
        MaterialCal::set(m, c);
    }
}

/// Spread of the material factor with only built-in values behind it.
const BUILT_IN_SIGMA: f64 = 0.3;
/// However tight a fit, the model itself isn't known better than this.
const MIN_FITTED_SIGMA: f64 = 0.05;

/// What one scale in a UWB position number rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestsOn {
    BuiltIn,
    Fdtd,
    Practice,
    /// A field walk, but of another scene or of this one before it was edited.
    FieldElsewhere,
    FieldHere,
}

impl RestsOn {
    fn evidence(self) -> Evidence {
        match self {
            Self::FieldHere => Evidence::Calibrated,
            Self::BuiltIn | Self::Fdtd | Self::FieldElsewhere => Evidence::Simulated,
            Self::Practice => Evidence::Estimate,
        }
    }
}

struct Input {
    name: String,
    band: CalBand,
    scale: f32,
    sigma: Option<f32>,
    rests_on: RestsOn,
}

/// The scales a UWB position number depends on, read from the calibrations in force:
/// delay and loss (it gates links) of each material present. Metal and liquid kill a link
/// whatever their scale, so a fit of them changes nothing and counts for nothing. Thin
/// barely matters until something says otherwise.
fn inputs(present: &[Material], bands: &[CalBand], scene: SceneBinding) -> Vec<Input> {
    let mut materials: Vec<Material> = Vec::new();
    for m in present {
        if !m.blocks_uwb() && !materials.contains(m) {
            materials.push(*m);
        }
    }
    let mut out = Vec::new();
    for band in bands.iter().copied() {
        for m in materials.iter().copied() {
            let cal = MaterialCal::applied_in(m, band);
            if m == Material::Thin && cal.is_none() {
                continue;
            }
            let c = cal.unwrap_or_default();
            for (what, scale, sigma, origin) in [("delay", c.bias_scale, c.source.delay_sigma, c.source.delay_origin), ("loss", c.loss_scale, c.source.loss_sigma, c.source.loss_origin)] {
                let rests_on = match origin {
                    Origin::Field if c.source.scene == Some(scene) => RestsOn::FieldHere,
                    Origin::Field => RestsOn::FieldElsewhere,
                    Origin::Practice => RestsOn::Practice,
                    Origin::None if scale != 1.0 => RestsOn::Fdtd,
                    Origin::None => RestsOn::BuiltIn,
                };
                out.push(Input { name: format!("{} {what}", m.name()), band, scale, sigma, rests_on });
            }
        }
    }
    out
}

/// A number is only as strong as its weakest input. With nothing a walk could fit, it's
/// a simulation on typical values.
pub(super) fn evidence_of(present: &[Material], bands: &[CalBand], scene: SceneBinding) -> Evidence {
    inputs(present, bands, scene).iter().map(|i| i.rests_on.evidence()).min().unwrap_or(Evidence::Simulated)
}

/// Spread of the stress test's material factor: what a field walk of this scene left of
/// the widest scale, once every scale has been fitted. One scale still on assumption
/// keeps the whole factor at the built-in spread, as the factor moves them all together.
pub(super) fn material_sigma(present: &[Material], bands: &[CalBand], scene: SceneBinding) -> f64 {
    let all = inputs(present, bands, scene);
    if all.is_empty() || all.iter().any(|i| i.rests_on != RestsOn::FieldHere) {
        return BUILT_IN_SIGMA;
    }
    let widest = all.iter().map(|i| (i.sigma.unwrap_or(f32::MAX) / i.scale.max(0.05)) as f64).fold(0.0, f64::max);
    widest.clamp(MIN_FITTED_SIGMA, BUILT_IN_SIGMA)
}

/// What the materials in a number rest on, in words, from the calibrations in force.
fn materials_note(present: &[Material], bands: &[CalBand], scene: SceneBinding) -> Vec<String> {
    let all = inputs(present, bands, scene);
    let list = |r: RestsOn| -> String {
        let shown = |i: &Input| match i.sigma {
            Some(s) => format!("{} ×{:.2} ±{s:.2} at {}", i.name, i.scale, i.band.label()),
            None if r == RestsOn::BuiltIn => i.name.clone(),
            None => format!("{} ×{:.2} at {}", i.name, i.scale, i.band.label()),
        };
        all.iter().filter(|i| i.rests_on == r).map(shown).collect::<Vec<_>>().join(", ")
    };
    let mut out = Vec::new();
    for (r, text) in [
        (RestsOn::FieldHere, "Fitted to a field walk of this scene as it stands"),
        (RestsOn::FieldElsewhere, "Fitted to a walk of another scene, or of this one before it was edited, so it doesn't count as calibration"),
        (RestsOn::Practice, "From a practice walk: made-up readings, not a measurement"),
        (RestsOn::Fdtd, "From an FDTD run: a simulation, not a measurement"),
        (RestsOn::BuiltIn, "Built-in typical values: run a survey walk to replace them"),
    ] {
        let l = list(r);
        if !l.is_empty() {
            out.push(format!("{text}: {l}"));
        }
    }
    if all.is_empty() {
        out.push("Materials are built-in typical values: nothing in this scene is both crossed and fittable by a survey walk".to_string());
    }
    out.push("No walk fits range noise, dropouts, or that metal and liquid stop the first path: those stay assumed".to_string());
    out
}

/// What radio takes each placed object to be, thin ones included.
fn present_materials(world: &mut World) -> Vec<Material> {
    let mut q = world.query_filtered::<&PlacedObject, Without<Device>>();
    q.iter(world).map(|o| radio::object_material(&o.name).map_or_else(|| radio::obstruction_of_asset(&o.asset).0, |x| x.0)).collect()
}

/// The bands the scene's UWB anchors work in.
fn uwb_bands(world: &mut World) -> Vec<CalBand> {
    let anchors: Vec<Device> = world.query::<&Device>().iter(world).filter(|d| d.enabled && d.kind.tech() == "uwb" && !d.kind.is_mobile()).cloned().collect();
    let profiles = world.resource::<crate::rtls::device::Profiles>();
    let mut bands: Vec<CalBand> = Vec::new();
    for d in &anchors {
        let b = CalBand::of_mhz(profiles.for_device(d).freq_mhz);
        if !bands.contains(&b) {
            bands.push(b);
        }
    }
    bands
}

#[derive(Clone, Copy, Debug)]
pub struct StressOptions {
    pub seconds: f64,
    pub target_m: f32,
    /// Random combinations for the overall range (each runs the whole sim).
    pub samples: usize,
    /// Most anchors to try switching off, one at a time.
    pub anchors_down: usize,
}

impl Default for StressOptions {
    fn default() -> Self {
        // 30, because fewer than 29 can't show 90 % at 95 % confidence, even all passing.
        Self { seconds: 10.0, target_m: 0.3, samples: 30, anchors_down: 4 }
    }
}

/// Runs every scenario and leaves the scene as it found it.
pub fn run(world: &mut World, o: StressOptions, scene: &str) -> StressReport {
    let start = Start::take(world);
    let was_muted = world.resource::<Hub>().muted;
    world.resource_mut::<Hub>().muted = true;
    let base = SiteConditions::default();
    let mut scenarios = Vec::new();
    let result = |name: &str, r: (Option<f32>, usize, usize, u64), note: &str| ScenarioResult { name: name.into(), p95_m: r.0, located: r.1, tags: r.2, rfid_reads: r.3, note: note.into() };

    let r = with_conditions(world, &start, base, o.seconds);
    scenarios.push(result("As modelled (racks full)", r, ""));
    for (name, fill) in [("Racks half full", 0.5), ("Racks nearly empty", 0.1)] {
        let r = with_conditions(world, &start, SiteConditions { rack_fill: fill, ..base }, o.seconds);
        scenarios.push(result(name, r, "Empty bays let signals through, and also change what the anchors were planned around"));
    }
    for (name, people) in [("Busy shift (5 people per 100 m²)", 5.0), ("Crowded (20 people per 100 m²)", 20.0)] {
        let r = with_conditions(world, &start, SiteConditions { people_per_100m2: people, ..base }, o.seconds);
        scenarios.push(result(name, r, ""));
    }
    let saved = scale_materials(1.5);
    let r = with_conditions(world, &start, base, o.seconds);
    restore_materials(saved);
    scenarios.push(result("Materials 50 % worse than assumed", r, ""));

    // One anchor down at a time, spread across the list.
    let anchors_total;
    let anchors: Vec<Entity> = {
        let mut q = world.query::<(Entity, &Device)>();
        let mut v: Vec<(String, Entity)> = q.iter(world).filter(|(_, d)| d.enabled && d.kind.tech() == "uwb" && !d.kind.is_mobile()).map(|(e, d)| (d.id.clone(), e)).collect();
        v.sort();
        anchors_total = v.len();
        let step = (v.len() / o.anchors_down.max(1)).max(1);
        v.into_iter().step_by(step).take(o.anchors_down).map(|x| x.1).collect()
    };
    let anchors_tried = anchors.len();
    let mut down_worst: Option<ScenarioResult> = None;
    for e in anchors {
        let id = world.get::<Device>(e).map_or(String::new(), |d| d.id.clone());
        if let Some(mut d) = world.get_mut::<Device>(e) {
            d.enabled = false;
        }
        let r = with_conditions(world, &start, base, o.seconds);
        if let Some(mut d) = world.get_mut::<Device>(e) {
            d.enabled = true;
        }
        let cand = result(&format!("Anchor {id} down"), r, "The worst of the anchors tried");
        let worse = down_worst.as_ref().is_none_or(|w| cand.located < w.located || (cand.located == w.located && severity(&cand) > severity(w)));
        if worse {
            down_worst = Some(cand);
        }
    }
    scenarios.extend(down_worst);

    // The overall range across random combinations.
    let (present, bands, here) = (present_materials(world), uwb_bands(world), super::survey::scene_binding(world));
    let material_spread = material_sigma(&present, &bands, here);
    let material_basis = if material_spread < BUILT_IN_SIGMA { "What a field walk of this scene left uncertain" } else { "Built-in material costs are typical values, not this site's" };
    let factors = vec![
        Factor::new("Rack fill", "share", Dist::Uniform { lo: 0.1, hi: 1.0 }, "Warehouses swing between nearly empty and full"),
        Factor::new("People", "per 100 m²", Dist::Uniform { lo: 0.0, hi: 10.0 }, "Assumed for a working shift"),
        Factor::new("Material scale", "×", Dist::LogNormal { median: 1.0, sigma: material_spread }, material_basis),
    ];
    let target = Target::at_most(o.target_m as f64);
    let opts = RobustOptions { samples: o.samples.max(4), seed: 51 };
    let evidence = evidence_of(&present, &bands, here);
    let mut reseed = 0;
    let report = robust_mut("UWB p95 position error", "m", &factors, Some(target), evidence, opts, false, |v| {
        reseed += 1;
        let saved = scale_materials(v[2] as f32);
        let r = reseeded(world, &start, SiteConditions { rack_fill: v[0] as f32, people_per_100m2: v[1] as f32 }, o.seconds, reseed);
        restore_materials(saved);
        combination_error(r.0, r.1, r.2)
    });
    let usable = report.samples > report.failed;
    let has_tags = scenarios.iter().any(|s| s.tags > 0);
    // Every combination losing a tag is an answer, and the worst one.
    let claim = (usable || has_tags).then(|| {
        let mut assumptions = vec![
            "Rack fill and crowds are ranges chosen for a typical warehouse, not this site's records".to_string(),
            "People are obstacles in links, not modelled bodies".to_string(),
            "Each combination is a fresh random draw of conditions and noise, but every one starts from the same tag positions: it says nothing of the floor they never reach".to_string(),
            format!("Each run is {:.0} s of simulated time; errors count from at most the last 30 s", o.seconds),
        ];
        assumptions.extend(materials_note(&present, &bands, here));
        if report.failed > 0 {
            assumptions.push(format!("In {} of {} combinations at least one tag had no position. They count as misses, and the range leaves them out", report.failed, report.samples));
        }
        assumptions.extend(scenario_notes(&scenarios, o.target_m));
        if anchors_tried < anchors_total {
            assumptions.push(format!("Only {anchors_tried} of the {anchors_total} anchors were tried switched off"));
        }
        Claim::from_report(&format!("site_p95:{scene}"), "site_stress", &format!("UWB position error across conditions, {scene}"), &report, Some(target), assumptions)
    });

    // Back to how it was found.
    start.restore(world);
    let seed = world.resource::<RtlsConfig>().settings.seed;
    world.resource_mut::<RtlsState>().restart(seed);
    world.resource_mut::<Hub>().muted = was_muted;

    let lost: Vec<&str> = scenarios.iter().filter(|s| s.p95_m.is_none() && s.tags > 0).map(|s| s.name.as_str()).collect();
    let worst = scenarios.iter().filter(|s| s.p95_m.is_some()).max_by(|a, b| severity(a).total_cmp(&severity(b)));
    let mut summary = match (&claim, worst) {
        (Some(c), Some(w)) => format!("{}: {} Worst single condition with positions: {} at {:.0} cm, {} of {} tags located.", c.verdict_text(), c.range_text(), w.name, w.p95_m.unwrap_or(0.0) * 100.0, w.located, w.tags),
        (Some(c), None) => format!("{}: no tag kept a position in any condition.", c.verdict_text()),
        _ => "No UWB fixes in this scene under any condition, so there's no position error to judge. Place anchors and tags first.".to_string(),
    };
    if !lost.is_empty() {
        summary.push_str(&format!(" No positions at all under: {}.", lost.join(", ")));
    }
    let partly: Vec<String> = scenarios.iter().filter(|s| s.p95_m.is_some() && s.located < s.tags).map(|s| format!("{} ({} of {})", s.name, s.located, s.tags)).collect();
    if !partly.is_empty() {
        summary.push_str(&format!(" Tags lost under: {}.", partly.join(", ")));
    }
    if report.failed > 0 {
        summary.push_str(&format!(" In {} of {} random combinations at least one tag had no position, which counts as a miss.", report.failed, report.samples));
    }
    StressReport { target_m: o.target_m, seconds: o.seconds, scenarios, claim, combinations_with_no_position: report.failed, combinations: report.samples, summary }
}

fn severity(s: &ScenarioResult) -> f32 {
    if s.located < s.tags {
        return f32::INFINITY;
    }
    s.p95_m.unwrap_or(f32::INFINITY)
}

#[cfg(test)]
mod verdict_tests {
    use super::*;
    use proof::Verdict;

    fn scenario(name: &str, p95: Option<f32>, located: usize, tags: usize) -> ScenarioResult {
        ScenarioResult { name: name.into(), p95_m: p95, located, tags, rfid_reads: 0, note: String::new() }
    }

    // OUT-B7: 9 of 10 tags lost and one with a 20 cm p95 passed a 30 cm target.
    #[test]
    fn a_lost_tag_fails_the_combination_however_good_the_others_are() {
        assert_eq!(combination_error(Some(0.2), 10, 10), 0.2f32 as f64);
        assert!(combination_error(Some(0.2), 1, 10).is_infinite());
        assert!(combination_error(None, 0, 10).is_infinite());
        let f = Factor::new("x", "", Dist::Uniform { lo: 0.0, hi: 1.0 }, "test");
        let target = Target::at_most(0.3);
        let run = |located: usize| robust_mut("p95", "m", std::slice::from_ref(&f), Some(target), Evidence::Simulated, RobustOptions { samples: 30, seed: 1 }, false, |_| combination_error(Some(0.2), located, 10));
        let all = Claim::from_report("a", "site_stress", "s", &run(10), Some(target), vec![]);
        assert_eq!(all.verdict, Verdict::Likely, "{}", all.verdict_text());
        let lost = run(1);
        assert_eq!(lost.pass_probability, Some(0.0));
        assert_eq!(lost.failed, 30);
        let c = Claim::from_report("a", "site_stress", "s", &lost, Some(target), vec![]);
        assert_eq!(c.verdict, Verdict::Unlikely, "{}", c.verdict_text());
        assert!(c.prediction.median.is_nan(), "no good-looking number is left behind: {:?}", c.prediction);
    }

    #[test]
    fn a_scenario_with_a_lost_tag_ranks_worst_and_misses() {
        let lost = scenario("Anchor A down", Some(0.1), 3, 10);
        let fine = scenario("As modelled", Some(0.25), 10, 10);
        assert!(severity(&lost) > severity(&fine));
        assert_eq!(severity(&fine), 0.25);
        // OUT-M11: what the verdict leaves out stays on the claim.
        let notes = scenario_notes(&[lost, fine, scenario("Crowded", Some(0.5), 10, 10), scenario("Dark", None, 0, 10)], 0.3);
        assert!(notes[0].contains("Anchor A down") && notes[0].contains("3 of 10") && notes[0].contains("misses"), "{notes:?}");
        assert!(notes[1].contains("meets"), "{notes:?}");
        assert!(notes[2].contains("50 cm") && notes[2].contains("misses"), "{notes:?}");
        assert!(notes[3].contains("no error figure") && notes[3].contains("misses"), "{notes:?}");
        assert!(notes.iter().all(|n| n.starts_with("Outside this verdict")), "{notes:?}");
    }

    #[test]
    fn the_default_run_can_reach_likely() {
        assert!(proof::stats::shown_share(StressOptions::default().samples, StressOptions::default().samples, 0.95) >= 0.9);
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use crate::rtls::radio::CAL_TEST_LOCK;

    // 8 GHz: no other test simulates it.
    const BAND: CalBand = CalBand::Ghz8;
    const HERE: SceneBinding = SceneBinding { id: 7, geometry: 8 };

    fn cal(loss: f32, delay: f32, loss_origin: Origin, delay_origin: Origin) -> Option<MaterialCal> {
        Some(MaterialCal { loss_scale: loss, bias_scale: delay, source: CalSource { band: BAND, loss_origin, delay_origin, scene: Some(HERE), loss_sigma: Some(0.1), delay_sigma: Some(0.1), ..Default::default() }, ..Default::default() })
    }

    #[test]
    fn the_weakest_input_sets_the_label() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        MaterialCal::set(Material::Wood, cal(1.1, 1.2, Origin::Field, Origin::Field));
        let fitted = evidence_of(&[Material::Wood], &[BAND], HERE);
        let with_unfitted = evidence_of(&[Material::Wood, Material::Glass], &[BAND], HERE);
        let unfitted_spread = material_sigma(&[Material::Wood, Material::Glass], &[BAND], HERE);
        MaterialCal::set(Material::Thin, cal(0.5, 0.5, Origin::Practice, Origin::Practice));
        let with_practice = evidence_of(&[Material::Wood, Material::Thin], &[BAND], HERE);
        MaterialCal::clear_bands();
        assert_eq!(fitted, Evidence::Calibrated);
        assert_eq!(with_unfitted, Evidence::Simulated);
        assert_eq!(unfitted_spread, BUILT_IN_SIGMA, "one scale on assumption keeps the factor wide");
        assert_eq!(with_practice, Evidence::Estimate);
    }

    #[test]
    fn a_delay_alone_is_not_enough_because_loss_gates_links() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        MaterialCal::set(Material::Wood, cal(1.0, 1.2, Origin::None, Origin::Field));
        let e = evidence_of(&[Material::Wood], &[BAND], HERE);
        MaterialCal::clear_bands();
        assert_eq!(e, Evidence::Simulated);
    }

    #[test]
    fn a_fit_of_a_material_that_blocks_anyway_counts_for_nothing() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        MaterialCal::set(Material::Metal, cal(1.3, 0.5, Origin::Field, Origin::Field));
        let only_metal = evidence_of(&[Material::Metal], &[BAND], HERE);
        let with_wood = evidence_of(&[Material::Metal, Material::Wood], &[BAND], HERE);
        MaterialCal::clear_bands();
        assert_eq!(only_metal, Evidence::Simulated, "the link dies whatever the scale");
        assert_eq!(with_wood, Evidence::Simulated);
    }

    #[test]
    fn the_assumptions_say_what_is_in_force() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let present = [Material::Wood, Material::Glass, Material::Concrete, Material::Generic];
        MaterialCal::set(Material::Wood, cal(1.1, 1.2, Origin::Field, Origin::Field));
        MaterialCal::set(Material::Glass, cal(0.2, 0.2, Origin::Practice, Origin::Practice));
        MaterialCal::set(Material::Concrete, Some(MaterialCal { loss_scale: 1.3, source: CalSource { band: BAND, ..Default::default() }, ..Default::default() }));
        let note = materials_note(&present, &[BAND], HERE).join("\n");
        let moved = materials_note(&present, &[BAND], SceneBinding { geometry: 9, ..HERE }).join("\n");
        MaterialCal::clear_bands();
        assert!(note.contains("field walk of this scene") && note.contains("wood delay ×1.20 ±0.10"), "{note}");
        assert!(note.contains("practice walk") && note.contains("glass loss ×0.20"), "{note}");
        assert!(note.contains("FDTD") && note.contains("concrete loss ×1.30"), "{note}");
        assert!(note.contains("Built-in") && note.contains("object delay"), "{note}");
        assert!(moved.contains("before it was edited") && moved.contains("wood delay"), "{moved}");
        assert!(!moved.contains("as it stands"), "{moved}");
    }
}
