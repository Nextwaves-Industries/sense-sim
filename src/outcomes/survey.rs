//! Fit a site's radio model to a short walk with a real tag. Ranges and signal
//! strengths taken at known spots say how much each material really costs here; the
//! model is then trusted for the points nobody measured. Whole spots are held back in
//! turn and predicted with exactly what would be applied, so the improvement reported
//! is on places the fit never saw, and a fit that doesn't improve is not applied.

use anyhow::{Result, bail};
use bevy::prelude::*;
use proof::{Calibration, Param, Problem, calibrate, plan_measurements};
use serde::{Deserialize, Serialize};

use crate::rtls::device::{Device, Profiles};
use crate::rtls::radio::{CalBand, CalSource, Material, MaterialCal, Origin, Path, SceneBinding};
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::scene::PlacedObject;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingKind {
    /// UWB range, metres.
    Range,
    /// Received signal strength, dBm.
    Rssi,
}

/// One value taken at a known spot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub device: String,
    /// Where the tag really was (scene coordinates, metres).
    pub tag: [f32; 3],
    pub kind: ReadingKind,
    pub value: f32,
    /// Made up by a practice walk, not measured. Never counts as calibration.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
}

/// Reads a walk from a file: JSON lines, or CSV with the header
/// `device,x,y,z,kind,value`. `kind` is `range` or `rssi`.
pub fn parse_readings(text: &str) -> Result<Vec<Reading>> {
    let mut out = Vec::new();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).enumerate().peekable();
    if lines.peek().is_some_and(|(_, l)| l.to_ascii_lowercase().starts_with("device,")) {
        lines.next();
    }
    for (i, l) in lines {
        if l.starts_with('{') {
            out.push(serde_json::from_str(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1))?);
            continue;
        }
        let p: Vec<&str> = l.split(',').map(str::trim).collect();
        if p.len() < 6 {
            bail!("line {}: expected device,x,y,z,kind,value and got {} columns", i + 1, p.len());
        }
        let num = |s: &str, what: &str| -> Result<f32> { s.parse::<f32>().map_err(|_| anyhow::anyhow!("line {}: {what} '{s}' isn't a number", i + 1)) };
        let kind = match p[4].to_ascii_lowercase().as_str() {
            "range" => ReadingKind::Range,
            "rssi" => ReadingKind::Rssi,
            other => bail!("line {}: kind '{other}' should be range or rssi", i + 1),
        };
        out.push(Reading { device: p[0].to_string(), tag: [num(p[1], "x")?, num(p[2], "y")?, num(p[3], "z")?], kind, value: num(p[5], "value")?, synthetic: false });
    }
    if let Some(r) = out.iter().find(|r| !r.value.is_finite() || r.tag.iter().any(|v| !v.is_finite())) {
        bail!("a reading from {} has a value that isn't a number", r.device);
    }
    if out.is_empty() {
        bail!("no readings in the file");
    }
    Ok(out)
}

/// What a walk says it is. Nothing counts as a field survey unless someone said so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalkOrigin {
    #[default]
    Undeclared,
    Practice,
    Field,
}

/// A reading's value, to the centimetre, whatever file format carried it: converting
/// a practice walk to CSV or pasting it inline doesn't change what it is.
fn fingerprint(r: &Reading) -> u64 {
    let cm = |v: f32| (v as f64 * 100.0).round() as i64;
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |bytes: &[u8]| bytes.iter().for_each(|b| h = (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3));
    mix(r.device.as_bytes());
    r.tag.iter().for_each(|v| mix(&cm(*v).to_le_bytes()));
    mix(&[(r.kind == ReadingKind::Rssi) as u8]);
    mix(&cm(r.value).to_le_bytes());
    h
}

/// Every practice walk written on this machine, to know one when it comes back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PracticeWalks {
    walks: Vec<Vec<u64>>,
}

impl PracticeWalks {
    fn path() -> std::path::PathBuf {
        let base = crate::app_paths::config_base_or_current();
        base.join("SenseSim").join("practice_walks.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_string(self)?)?;
        Ok(())
    }

    pub fn record(&mut self, readings: &[Reading]) {
        self.walks.push(readings.iter().map(fingerprint).collect());
        // Plenty: a walk is only refused against what's kept here.
        let extra = self.walks.len().saturating_sub(500);
        self.walks.drain(..extra);
    }

    /// Half of these readings came from one practice walk.
    pub fn matches(&self, readings: &[Reading]) -> bool {
        let seen = |walk: &Vec<u64>| readings.iter().filter(|r| walk.contains(&fingerprint(r))).count();
        !readings.is_empty() && self.walks.iter().any(|w| seen(w) * 2 >= readings.len())
    }
}

/// What this walk counts as. Made-up readings can't be declared real.
pub fn resolve_origin(declared: Option<WalkOrigin>, readings: &[Reading], practice: &PracticeWalks) -> Result<WalkOrigin> {
    let made_up = readings.iter().any(|r| r.synthetic) || practice.matches(readings);
    match (declared, made_up) {
        (Some(WalkOrigin::Field), true) => bail!("this is a practice walk, written by site_survey_practice, so it can't count as a field survey. Its readings are made up"),
        (_, true) => Ok(WalkOrigin::Practice),
        (Some(o), false) => Ok(o),
        (None, false) => Ok(WalkOrigin::Undeclared),
    }
}

/// A reading with what lies on its path.
#[derive(Clone, Debug)]
pub struct Link {
    pub reading: Reading,
    pub path: Path,
    /// The band of the device that took it.
    pub band: CalBand,
    /// Excess range per typical wall (bias + mean spread), metres.
    pub per_wall_m: f32,
    /// Of which the random part per typical wall, metres: a blocked link is far
    /// noisier than a clear one.
    pub spread_m: f32,
    pub rssi_at_1m: f32,
    pub path_loss_exp: f32,
    pub wall_db: f32,
}

/// What each parameter of the fit means, in the order they were given.
#[derive(Clone, Debug, PartialEq)]
enum Slot {
    Bias(usize),
    Loss(usize),
    Delay(String),
    Offset(String),
    Exponent,
}

/// The model's expected value for a link under these parameters.
fn predict_link(l: &Link, slots: &[Slot], x: &[f64]) -> f64 {
    let d = l.path.distance as f64;
    match l.reading.kind {
        ReadingKind::Range => {
            let mut r = d;
            for (s, v) in slots.iter().zip(x) {
                match s {
                    Slot::Bias(m) => r += l.path.per_material[*m] as f64 * v * l.per_wall_m as f64,
                    Slot::Delay(dev) if *dev == l.reading.device => r += v,
                    _ => {}
                }
            }
            r
        }
        ReadingKind::Rssi => {
            let mut n = l.path_loss_exp as f64;
            let (mut loss, mut off) = (0.0, 0.0);
            for (s, v) in slots.iter().zip(x) {
                match s {
                    Slot::Loss(m) => loss += l.path.per_material[*m] as f64 * v * l.wall_db as f64,
                    Slot::Offset(dev) if *dev == l.reading.device => off += v,
                    Slot::Exponent => n = *v,
                    _ => {}
                }
            }
            l.rssi_at_1m as f64 - 10.0 * n * d.max(0.1).log10() - loss + off
        }
    }
}

/// Labels on what a survey applied, for people to read. Nothing decides on them:
/// `CalSource` says what a calibration counts for.
pub const FIELD: &str = "Field survey";
pub const PRACTICE: &str = "Practice walk";

const RANGE_SIGMA: f64 = 0.15;
const RSSI_SIGMA: f64 = 4.0;

/// Readings this close together are one spot: they share their multipath.
const SPOT_M: f32 = 0.25;
/// What it takes to apply a material's scale. From one anchor it can't be told apart
/// from that anchor's delay, and at five spots a group can be held out and four remain.
pub const MIN_SPOTS: usize = 5;
pub const MIN_DEVICES: usize = 2;
/// Posterior 1σ of a scale, absolute so a wide prior can't buy it. Tighter than the 0.3
/// the stress test assumes for a built-in value, or a fit couldn't narrow a range.
pub const MAX_SCALE_SIGMA: f64 = 0.25;
/// Share of its uncertainty the readings removed, so a narrow prior can't buy it either.
const MIN_LEARNED: f64 = 0.5;
/// Typical residual of the fitted model in reading sigmas. A model that misses by more
/// than this doesn't describe the site, however well its scale is pinned. A median, so a
/// few bad readings don't decide it: they're what `MAX_OUTLIER_SHARE` is for.
const MAX_RESIDUAL: f64 = 2.0;
const MAX_OUTLIER_SHARE: f64 = 0.2;
/// Groups of spots held out in turn.
const FOLDS: usize = 5;

fn spot_of(tag: [f32; 3]) -> [i32; 3] {
    tag.map(|v| (v / SPOT_M).round() as i32)
}

/// Repeats at a spot through a device become their median. A stationary burst has one
/// multipath, so it's one observation, however many lines it fills.
fn merge_repeats(links: &[Link]) -> Vec<Link> {
    let mut groups: Vec<(Link, Vec<f32>)> = Vec::new();
    for l in links {
        let same = |g: &&mut (Link, Vec<f32>)| spot_of(g.0.reading.tag) == spot_of(l.reading.tag) && g.0.reading.device == l.reading.device && g.0.reading.kind == l.reading.kind;
        match groups.iter_mut().find(|g| same(g)) {
            Some(g) => g.1.push(l.reading.value),
            None => groups.push((l.clone(), vec![l.reading.value])),
        }
    }
    groups
        .into_iter()
        .map(|(mut l, mut values)| {
            values.sort_by(f32::total_cmp);
            let n = values.len();
            l.reading.value = if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 };
            l
        })
        .collect()
}

/// Distinct spots and devices a material was seen from, by this kind of reading.
fn seen_from(links: &[Link], m: usize, kind: ReadingKind) -> (usize, usize) {
    let on: Vec<&Link> = links.iter().filter(|l| l.reading.kind == kind && l.path.per_material[m] > 0.0).collect();
    let mut spots: Vec<[i32; 3]> = on.iter().map(|l| spot_of(l.reading.tag)).collect();
    spots.sort();
    spots.dedup();
    let mut devices: Vec<&str> = on.iter().map(|l| l.reading.device.as_str()).collect();
    devices.sort();
    devices.dedup();
    (spots.len(), devices.len())
}

/// The one band this kind of reading was taken in. A scale fitted across two bands
/// would describe neither.
fn band_of_kind(links: &[Link], kind: ReadingKind) -> Result<CalBand> {
    let mut bands: Vec<CalBand> = links.iter().filter(|l| l.reading.kind == kind).map(|l| l.band).collect();
    bands.dedup();
    let first = bands.first().copied().unwrap_or_default();
    if bands.iter().any(|b| *b != first) {
        bail!("this walk has {kind:?} readings in more than one band ({} and more): fit each band as its own walk", first.label());
    }
    Ok(first)
}

/// Held-out error per kind of reading: (built-in values, what would be applied).
#[derive(Default)]
struct Score {
    spots: usize,
    readings: usize,
    range: Option<(f64, f64)>,
    rssi: Option<(f64, f64)>,
}

/// Holds out each group of spots in turn and predicts it from a fit to the others, with
/// only the `chosen` parameters moved off their built-in values: that's all `apply`
/// writes. Readings the whole fit set aside as outliers aren't scored, as one bad line
/// would decide the comparison alone.
fn held_out_score(links: &[Link], params: &[Param], slots: &[Slot], chosen: &[usize], outliers: &[usize]) -> Result<Score> {
    let mut spots: Vec<[i32; 3]> = Vec::new();
    for l in links {
        let s = spot_of(l.reading.tag);
        if !spots.contains(&s) {
            spots.push(s);
        }
    }
    let folds = FOLDS.min(spots.len());
    if folds < 2 {
        return Ok(Score::default());
    }
    let group = |l: &Link| spots.iter().position(|s| *s == spot_of(l.reading.tag)).unwrap_or(0) % folds;
    let built_in: Vec<f64> = params.iter().map(|p| p.prior_mean).collect();
    let mut err: [(Vec<f64>, Vec<f64>); 2] = Default::default();
    let mut used_spots: Vec<[i32; 3]> = Vec::new();
    for g in 0..folds {
        let train: Vec<Link> = links.iter().filter(|l| group(l) != g).cloned().collect();
        let fitted = fit_on(&train, params, slots)?.values();
        let mut deployed = built_in.clone();
        for c in chosen {
            deployed[*c] = fitted[*c];
        }
        for (_, l) in links.iter().enumerate().filter(|(i, l)| group(l) == g && !outliers.contains(i)) {
            let v = l.reading.value as f64;
            let e = &mut err[(l.reading.kind == ReadingKind::Rssi) as usize];
            e.0.push((predict_link(l, slots, &built_in) - v).powi(2));
            e.1.push((predict_link(l, slots, &deployed) - v).powi(2));
            let s = spot_of(l.reading.tag);
            if !used_spots.contains(&s) {
                used_spots.push(s);
            }
        }
    }
    let rms = |v: &[f64]| (v.iter().sum::<f64>() / v.len() as f64).sqrt();
    let of = |e: &(Vec<f64>, Vec<f64>)| (!e.0.is_empty()).then(|| (rms(&e.0), rms(&e.1)));
    Ok(Score { spots: used_spots.len(), readings: err[0].0.len() + err[1].0.len(), range: of(&err[0]), rssi: of(&err[1]) })
}

/// Parameters this set of links can say anything about. `prior_sigma` is how
/// unsure we are of each material's built-in cost (a photo's guess can narrow it).
fn parameters(links: &[Link], prior_sigma: f64) -> (Vec<Param>, Vec<Slot>) {
    let (mut params, mut slots) = (Vec::new(), Vec::new());
    let any = |k: ReadingKind| links.iter().any(|l| l.reading.kind == k);
    for (m, mat) in Material::ALL.iter().enumerate() {
        let crossed = |k: ReadingKind| links.iter().any(|l| l.reading.kind == k && l.path.per_material[m] > 0.0);
        if crossed(ReadingKind::Range) {
            params.push(Param::new(&format!("{} delay scale", mat.name()), 1.0, prior_sigma, 0.0, 6.0));
            slots.push(Slot::Bias(m));
        }
        if crossed(ReadingKind::Rssi) {
            params.push(Param::new(&format!("{} loss scale", mat.name()), 1.0, prior_sigma, 0.0, 6.0));
            slots.push(Slot::Loss(m));
        }
    }
    let mut devices: Vec<(&str, ReadingKind)> = links.iter().map(|l| (l.reading.device.as_str(), l.reading.kind)).collect();
    devices.sort_by_key(|d| (d.0, d.1 == ReadingKind::Rssi));
    devices.dedup();
    for (d, k) in devices {
        match k {
            ReadingKind::Range => {
                params.push(Param::new(&format!("{d} antenna delay (m)"), 0.0, 0.3, -2.0, 2.0));
                slots.push(Slot::Delay(d.to_string()));
            }
            ReadingKind::Rssi => {
                params.push(Param::new(&format!("{d} level offset (dB)"), 0.0, 4.0, -20.0, 20.0));
                slots.push(Slot::Offset(d.to_string()));
            }
        }
    }
    if any(ReadingKind::Rssi) {
        let n = links.iter().filter(|l| l.reading.kind == ReadingKind::Rssi).map(|l| l.path_loss_exp as f64).sum::<f64>() / links.iter().filter(|l| l.reading.kind == ReadingKind::Rssi).count().max(1) as f64;
        params.push(Param::new("Path loss exponent", n, 0.4, 1.4, 4.5));
        slots.push(Slot::Exponent);
    }
    (params, slots)
}

/// How noisy this reading is expected to be. Excess delay through an obstruction
/// spreads as much as its own mean, so a blocked link says much less than a clear one.
fn reading_sigma(l: &Link) -> f64 {
    match l.reading.kind {
        ReadingKind::Range => {
            let spread: f64 = l.path.per_material.iter().map(|w| *w as f64 * l.spread_m as f64).sum();
            (RANGE_SIGMA * RANGE_SIGMA + spread * spread).sqrt()
        }
        ReadingKind::Rssi => RSSI_SIGMA,
    }
}

fn fit_on(links: &[Link], params: &[Param], slots: &[Slot]) -> Result<Calibration> {
    let measured: Vec<f64> = links.iter().map(|l| l.reading.value as f64).collect();
    let sigma: Vec<f64> = links.iter().map(reading_sigma).collect();
    calibrate(&Problem { params, measured: &measured, sigma: &sigma, robust_k: Some(3.0) }, |x| links.iter().map(|l| predict_link(l, slots, x)).collect()).map_err(|e| anyhow::anyhow!(e))
}

/// A fitted parameter, in words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Learned {
    pub name: String,
    pub prior: f64,
    pub value: f64,
    pub sigma: f64,
    /// How much of its uncertainty the readings removed (0..1).
    pub learned: f64,
}

/// The result of fitting a walk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SurveyFit {
    /// Lines in the walk, and what they come to once repeats at a spot are merged.
    pub readings: usize,
    #[serde(default)]
    pub distinct_readings: usize,
    #[serde(default)]
    pub spots: usize,
    /// The readings came from a practice walk: nothing was measured.
    pub synthetic: bool,
    /// What the walk counts as. Set from what was declared (`resolve_origin`).
    #[serde(default)]
    pub origin: WalkOrigin,
    pub unknown_devices: Vec<String>,
    pub params: Vec<Learned>,
    pub outliers: usize,
    #[serde(default)]
    pub converged: bool,
    /// Typical miss of the fitted model, in reading sigmas (robust: 1.48 × the median).
    #[serde(default)]
    pub residual_sigmas: f64,
    /// Prediction error at spots the fit never saw, with the built-in values and with
    /// only what would be applied. Ranges in metres, levels in dB. `held_out` is how many
    /// readings that rests on, from `held_out_spots` spots.
    pub held_out: usize,
    #[serde(default)]
    pub held_out_spots: usize,
    pub holdout_range_before: Option<f64>,
    pub holdout_range_after: Option<f64>,
    pub holdout_rssi_before: Option<f64>,
    pub holdout_rssi_after: Option<f64>,
    /// Bands the readings were taken in, which is where a fit applies.
    #[serde(default)]
    pub range_band: CalBand,
    #[serde(default)]
    pub rssi_band: CalBand,
    /// Material scales that passed every check, to apply.
    pub applied: Vec<String>,
    /// Materials that didn't, worth another pass.
    pub still_unknown: Vec<String>,
    /// Why, in words.
    #[serde(default)]
    pub not_applied: Vec<String>,
}

/// Fits the links. Priors can be tightened where a photo or datasheet says more. A
/// material's scale goes on `applied` only if enough places and anchors saw it, it's
/// pinned down in absolute terms by the readings themselves, the fitted model explains
/// the walk, and that scale predicts spots held out of the fit better than the built-in.
pub fn fit(links: &[Link], prior_sigma: f64) -> Result<SurveyFit> {
    let merged = merge_repeats(links);
    if merged.len() < 5 {
        bail!("{} distinct readings ({} lines) is too few to fit anything; take at least 10 at different spots", merged.len(), links.len());
    }
    let (range_band, rssi_band) = (band_of_kind(&merged, ReadingKind::Range)?, band_of_kind(&merged, ReadingKind::Rssi)?);
    let (params, slots) = parameters(&merged, prior_sigma);
    let cal = fit_on(&merged, &params, &slots)?;
    let learned: Vec<Learned> = cal.params.iter().map(|p| Learned { name: p.name.clone(), prior: p.prior, value: p.value, sigma: p.sigma, learned: p.learned }).collect();

    let mut not_applied = Vec::new();
    let mut chosen = Vec::new();
    for (i, (slot, p)) in slots.iter().zip(&learned).enumerate() {
        let (m, kind) = match slot {
            Slot::Bias(m) => (*m, ReadingKind::Range),
            Slot::Loss(m) => (*m, ReadingKind::Rssi),
            _ => continue,
        };
        let (spots, devices) = seen_from(&merged, m, kind);
        let why = if spots < MIN_SPOTS || devices < MIN_DEVICES {
            Some(format!("seen from {spots} spots and {devices} devices; it takes {MIN_SPOTS} and {MIN_DEVICES}"))
        } else if p.sigma > MAX_SCALE_SIGMA {
            Some(format!("still ±{:.2}; it takes ±{MAX_SCALE_SIGMA:.2}", p.sigma))
        } else if p.learned < MIN_LEARNED {
            Some("still mostly the assumed value".to_string())
        } else if p.value <= 1e-3 || p.value >= 6.0 - 1e-3 {
            Some(format!("stuck at its limit ({:.2}), so the model can't explain these readings with it", p.value))
        } else {
            None
        };
        match why {
            Some(w) => not_applied.push(format!("{}: {w}", p.name)),
            None => chosen.push(i),
        }
    }

    let x = cal.values();
    let mut misses: Vec<f64> = merged.iter().map(|l| ((predict_link(l, &slots, &x) - l.reading.value as f64) / reading_sigma(l)).abs()).collect();
    misses.sort_by(f64::total_cmp);
    let residual_sigmas = 1.4826 * misses[misses.len() / 2];
    let mut model_ok = true;
    if !cal.converged {
        not_applied.push("The fit didn't settle on an answer".to_string());
        model_ok = false;
    }
    if residual_sigmas > MAX_RESIDUAL {
        not_applied.push(format!("The fitted model still misses these readings by {residual_sigmas:.1} sigma (at most {MAX_RESIDUAL:.1}): it doesn't describe this site"));
        model_ok = false;
    }
    if cal.outliers.len() as f64 > MAX_OUTLIER_SHARE * merged.len() as f64 {
        not_applied.push(format!("{} of {} readings don't fit anything else", cal.outliers.len(), merged.len()));
        model_ok = false;
    }

    let score = held_out_score(&merged, &params, &slots, &chosen, &cal.outliers)?;
    let better = |kind: &str, unit: &str, s: Option<(f64, f64)>, wanted: bool, why: &mut Vec<String>| -> bool {
        match s {
            Some((b, a)) if a < b => true,
            Some((b, a)) if wanted => {
                why.push(format!("Held-out {kind} error isn't better with this fit: {b:.2} {unit} built-in, {a:.2} {unit} fitted"));
                false
            }
            None if wanted => {
                why.push(format!("No {kind} readings could be held out to score this fit"));
                false
            }
            _ => false,
        }
    };
    let has = |pick: fn(&Slot) -> bool| chosen.iter().any(|c| pick(&slots[*c]));
    let range_better = better("range", "m", score.range, has(|s| matches!(s, Slot::Bias(_))), &mut not_applied);
    let rssi_better = better("signal", "dB", score.rssi, has(|s| matches!(s, Slot::Loss(_))), &mut not_applied);

    let goes = |i: usize| {
        model_ok
            && chosen.contains(&i)
            && match slots[i] {
                Slot::Bias(_) => range_better,
                Slot::Loss(_) => rssi_better,
                _ => false,
            }
    };
    let is_material = |s: &Slot| matches!(s, Slot::Bias(_) | Slot::Loss(_));
    let applied = (0..slots.len()).filter(|i| goes(*i)).map(|i| learned[i].name.clone()).collect();
    let still_unknown = (0..slots.len()).filter(|i| is_material(&slots[*i]) && !goes(*i)).map(|i| learned[i].name.clone()).collect();
    let mut spots: Vec<[i32; 3]> = merged.iter().map(|l| spot_of(l.reading.tag)).collect();
    spots.sort();
    spots.dedup();
    let synthetic = links.iter().any(|l| l.reading.synthetic);
    Ok(SurveyFit {
        readings: links.len(),
        distinct_readings: merged.len(),
        spots: spots.len(),
        synthetic,
        origin: if synthetic { WalkOrigin::Practice } else { WalkOrigin::Undeclared },
        unknown_devices: Vec::new(),
        params: learned,
        outliers: cal.outliers.len(),
        converged: cal.converged,
        residual_sigmas,
        held_out: score.readings,
        held_out_spots: score.spots,
        holdout_range_before: score.range.map(|s| s.0),
        holdout_range_after: score.range.map(|s| s.1),
        holdout_rssi_before: score.rssi.map(|s| s.0),
        holdout_rssi_after: score.rssi.map(|s| s.1),
        range_band,
        rssi_band,
        applied,
        still_unknown,
        not_applied,
    })
}

/// Puts the material scales that passed `fit`'s checks into the model, in the band they
/// were read in, tied to this scene. A walk's other findings (path-loss exponent, device
/// delays, level offsets) go no further than its report. A scale this walk didn't fit
/// keeps its earlier value only if that came from the same kind of walk of the same
/// scene: nothing is inherited across origins. Returns how many materials it wrote; an
/// undeclared walk writes none.
pub fn apply(f: &SurveyFit, source: &str, scene: SceneBinding) -> usize {
    let (origin, word) = match f.origin {
        WalkOrigin::Field => (Origin::Field, FIELD),
        WalkOrigin::Practice => (Origin::Practice, PRACTICE),
        WalkOrigin::Undeclared => return 0,
    };
    let mut written = 0;
    for mat in Material::ALL {
        let scale = |suffix: &str| {
            let name = format!("{} {suffix}", mat.name());
            f.params.iter().find(|p| p.name == name && f.applied.contains(&name)).map(|p| (p.value as f32, p.sigma as f32))
        };
        let (loss, delay) = (scale("loss scale"), scale("delay scale"));
        let bands: &[CalBand] = if f.range_band == f.rssi_band { &[f.range_band] } else { &[f.range_band, f.rssi_band] };
        for band in bands.iter().copied() {
            let loss = loss.filter(|_| band == f.rssi_band);
            let delay = delay.filter(|_| band == f.range_band);
            if loss.is_none() && delay.is_none() {
                continue;
            }
            let prev = MaterialCal::applied_in(mat, band);
            let keep = |o: fn(&CalSource) -> Origin| prev.as_ref().filter(|p| o(&p.source) == origin && p.source.scene == Some(scene));
            let (loss_scale, loss_origin, loss_sigma) = match (loss, keep(|s| s.loss_origin)) {
                (Some((v, s)), _) => (v, origin, Some(s)),
                (None, Some(p)) => (p.loss_scale, origin, p.source.loss_sigma),
                (None, None) => (1.0, Origin::None, None),
            };
            let (bias_scale, delay_origin, delay_sigma) = match (delay, keep(|s| s.delay_origin)) {
                (Some((v, s)), _) => (v, origin, Some(s)),
                (None, Some(p)) => (p.bias_scale, origin, p.source.delay_sigma),
                (None, None) => (1.0, Origin::None, None),
            };
            let what = match (loss_origin != Origin::None, delay_origin != Origin::None) {
                (true, true) => "loss and delay",
                (true, false) => "loss",
                _ => "delay",
            };
            let source = CalSource { path: source.into(), band, delay_origin, loss_origin, scene: Some(scene), delay_sigma, loss_sigma, relative: false };
            MaterialCal::set(mat, Some(MaterialCal { loss_scale, bias_scale, label: format!("{word} {what} · {} · {}", band.label(), source.path.display()), source }));
            written += 1;
        }
    }
    written
}

/// Clears everything a survey applied, in every band.
pub fn clear_applied() {
    for m in Material::ALL {
        for band in CalBand::ALL.into_iter().skip(1) {
            if MaterialCal::applied_in(m, band).is_some_and(|c| c.source.delay_origin != Origin::None || c.source.loss_origin != Origin::None) {
                MaterialCal::clear_in(m, band);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scene side

#[derive(Resource, Default)]
pub struct SurveyState {
    pub last: Option<SurveyFit>,
    pub plan: Option<WalkPlan>,
}

/// What a fit depends on in one object: where it is, what it is, and what radio takes
/// it for. No entity ids (not stable across a tab's parking) and no calibration scales
/// (applying a fit must not change what the fit was bound to).
fn object_signature(name: &str, asset: &std::path::Path, t: &Transform, parts: usize, radio: (Material, f32)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    asset.hash(&mut h);
    for v in t.translation.to_array().into_iter().chain(t.rotation.to_array()).chain(t.scale.to_array()) {
        v.to_bits().hash(&mut h);
    }
    parts.hash(&mut h);
    (radio.0 as u8).hash(&mut h);
    radio.1.to_bits().hash(&mut h);
    h.finish()
}

/// `sim::scene_signature` also moves with every calibration and with entity order, so
/// it can't say whether the geometry a fit saw is still there. Summed, as query order
/// isn't stable either.
fn geometry_system(parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>) -> u64 {
    let mut count: std::collections::HashMap<Entity, usize> = std::collections::HashMap::new();
    for (of, _, _) in &parts {
        *count.entry(of.0).or_default() += 1;
    }
    objects.iter().fold(0u64, |sum, (e, o, t)| {
        let radio = crate::rtls::radio::object_material(&o.name).unwrap_or_else(|| crate::rtls::radio::obstruction_of_asset(&o.asset));
        sum.wrapping_add(object_signature(&o.name, &o.asset, t, count.get(&e).copied().unwrap_or(0), radio))
    })
}

/// The scene showing now: its tab and its geometry as it stands.
pub fn scene_binding(world: &mut World) -> SceneBinding {
    let id = world.get_resource::<crate::tabs::Tabs>().map_or(0, |t| t.active_id());
    let geometry = world.run_system_cached(geometry_system).unwrap_or(0);
    SceneBinding { id, geometry }
}

/// Raises the rays of a walk through the scene.
pub fn links_system(
    In(readings): In<Vec<Reading>>,
    parts: OccluderParts,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    devices: Query<(&Device, &Transform)>,
    profiles: Res<Profiles>,
) -> (Vec<Link>, Vec<String>) {
    let occ = Occluders::build(&parts, &objects);
    let mut unknown = Vec::new();
    let mut out = Vec::new();
    for r in readings {
        let Some((dev, t)) = devices.iter().find(|(d, _)| d.id == r.device) else {
            if !unknown.contains(&r.device) {
                unknown.push(r.device.clone());
            }
            continue;
        };
        let prof = profiles.for_device(dev);
        let path = occ.path(Vec3::from_array(r.tag), t.translation, None);
        out.push(Link { per_wall_m: prof.nlos_bias_m + prof.nlos_sigma_m, spread_m: prof.nlos_sigma_m, rssi_at_1m: prof.rssi_at_1m, path_loss_exp: prof.path_loss_exp, wall_db: prof.wall_loss_db, band: CalBand::of_mhz(prof.freq_mhz), reading: r, path });
    }
    (out, unknown)
}

/// Where to stand, and what the walk should teach.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WalkPlan {
    pub points: Vec<WalkPoint>,
    /// (material, sigma now, expected sigma after the walk).
    pub expected: Vec<(String, f64, f64)>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalkPoint {
    pub at: [f32; 3],
    /// Devices whose readings there teach the most.
    pub devices: Vec<String>,
}

/// Picks up to `count` spots on the floor that best pin down the materials and
/// device delays. A spot is scored by what its links can teach.
pub fn plan_system(
    In((count, height, step)): In<(usize, f32, f32)>,
    parts: OccluderParts,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    devices: Query<(&Device, &Transform)>,
    profiles: Res<Profiles>,
) -> WalkPlan {
    let occ = Occluders::build(&parts, &objects);
    let infra: Vec<(String, Vec3, f32, f32)> = devices
        .iter()
        .filter(|(d, _)| d.enabled && !d.kind.is_mobile() && d.kind.tech() == "uwb")
        .map(|(d, t)| {
            let p = profiles.for_device(d);
            (d.id.clone(), t.translation, p.nlos_bias_m + p.nlos_sigma_m, p.nlos_sigma_m)
        })
        .collect();
    if infra.is_empty() {
        return WalkPlan { note: "Place UWB anchors first: a walk is planned against the anchors that will measure it.".into(), ..Default::default() };
    }
    let (lo, hi) = occ.bounds().map_or_else(
        || infra.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), i| (a.min(i.1), b.max(i.1))),
        |b| b,
    );
    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut owner: Vec<(usize, usize)> = Vec::new();
    let mut spots: Vec<Vec3> = Vec::new();
    let mut noise: Vec<f64> = Vec::new();
    let (nx, nz) = (((hi.x - lo.x) / step).ceil().max(1.0) as usize, ((hi.z - lo.z) / step).ceil().max(1.0) as usize);
    let crossed: Vec<usize> = (0..7).collect();
    let nparam = crossed.len() + infra.len();
    for ix in 0..=nx {
        for iz in 0..=nz {
            let p = Vec3::new(lo.x + step * ix as f32, height, lo.z + step * iz as f32);
            if occ.inside(p) {
                continue;
            }
            let spot = spots.len();
            spots.push(p);
            for (k, (_, pos, per_wall, spread)) in infra.iter().enumerate() {
                let path = occ.path(p, *pos, None);
                let mut row = vec![0.0; nparam];
                for m in &crossed {
                    row[*m] = path.per_material[*m] as f64 * *per_wall as f64;
                }
                row[crossed.len() + k] = 1.0;
                rows.push(row);
                // As in the fit: a blocked link is far noisier, so it teaches much less.
                let noisy: f64 = path.per_material.iter().map(|w| *w as f64 * *spread as f64).sum();
                noise.push((RANGE_SIGMA * RANGE_SIGMA + noisy * noisy).sqrt());
                owner.push((spot, k));
            }
        }
    }
    let mut params: Vec<Param> = Material::ALL.iter().map(|m| Param::new(m.name(), 1.0, 0.5, 0.0, 6.0)).collect();
    params.extend(infra.iter().map(|(id, ..)| Param::new(id, 0.0, 0.3, -2.0, 2.0)));
    // Many pairs per spot: ask for more picks than spots, then keep the first spots.
    let plan = plan_measurements(&params, &rows, &noise, (count * infra.len().min(4)).max(count));
    let mut chosen: Vec<WalkPoint> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    for c in &plan.chosen {
        let (spot, k) = owner[*c];
        match seen.iter().position(|s| *s == spot) {
            Some(i) => chosen[i].devices.push(infra[k].0.clone()),
            None if chosen.len() < count => {
                seen.push(spot);
                chosen.push(WalkPoint { at: spots[spot].to_array(), devices: vec![infra[k].0.clone()] });
            }
            None => {}
        }
    }
    let expected = (0..7).filter(|m| plan.sigma_after[*m] < plan.sigma_before[*m] - 1e-3).map(|m| (Material::ALL[m].name().to_string(), plan.sigma_before[m], plan.sigma_after[m])).collect();
    WalkPlan { note: format!("{} spots across {} anchors. At each spot, log the range from every anchor you can hear.", chosen.len(), infra.len()), points: chosen, expected }
}

/// What a ray crosses between two points, for checking what the model believes.
pub fn trace_system(In((a, b)): In<(Vec3, Vec3)>, parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>) -> Vec<(String, Material, f32, f32)> {
    Occluders::build(&parts, &objects).trace(a, b)
}

/// A practice walk: readings as a real tag would log them if the site's materials
/// were `truth` (scale per material name) and each anchor had a random delay. For
/// trying the loop and testing it against a known answer, never for a real site.
pub fn practice_system(
    In((truth, spots, seed)): In<(std::collections::BTreeMap<String, f32>, Vec<[f32; 3]>, u64)>,
    parts: OccluderParts,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    devices: Query<(&Device, &Transform)>,
    profiles: Res<Profiles>,
) -> Vec<Reading> {
    let occ = Occluders::build(&parts, &objects);
    let mut rng = crate::rtls::rng::Rng::new(seed);
    let mut anchors: Vec<(String, Vec3, f32, f32, f32)> = devices
        .iter()
        .filter(|(d, _)| d.enabled && !d.kind.is_mobile() && d.kind.tech() == "uwb")
        .map(|(d, t)| {
            let p = profiles.for_device(d);
            (d.id.clone(), t.translation, p.nlos_bias_m, p.nlos_sigma_m, 0.0)
        })
        .collect();
    anchors.sort_by(|a, b| a.0.cmp(&b.0));
    for a in anchors.iter_mut() {
        a.4 = rng.gauss(0.1);
    }
    let mut out = Vec::new();
    for at in spots {
        for (id, pos, bias, spread, delay) in &anchors {
            let path = occ.path(Vec3::from_array(at), *pos, None);
            let mut r = path.distance + delay + rng.gauss(0.1);
            for (m, mat) in Material::ALL.iter().enumerate() {
                let w = path.per_material[m];
                if w > 0.0 {
                    let s = truth.get(mat.name()).copied().unwrap_or(1.0);
                    r += w * s * bias + rng.exp(w * s * spread);
                }
            }
            out.push(Reading { device: id.clone(), tag: at, kind: ReadingKind::Range, value: r, synthetic: true });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcomes::stress::{evidence_of, material_sigma};
    use crate::rtls::radio::CAL_TEST_LOCK;
    use crate::rtls::rng::Rng;
    use proof::Evidence;

    /// Tests write band 8 GHz: no other test simulates it.
    const BAND: CalBand = CalBand::Ghz8;
    const SCENE: SceneBinding = SceneBinding { id: 1, geometry: 2 };

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A link crossing some material, its reading made by a hidden truth.
    fn mk(rng: &mut Rng, device: &str, d: f32, metal: f32, wood: f32, truth: (f32, f32), delay: f32) -> Link {
        let mut path = Path { distance: d, ..Default::default() };
        path.per_material[Material::Metal as usize] = metal;
        path.per_material[Material::Wood as usize] = wood;
        let per_wall = 0.6;
        let value = d + metal * truth.0 * per_wall + wood * truth.1 * per_wall + delay + rng.gauss(0.1);
        Link { reading: Reading { device: device.into(), tag: [0.0; 3], kind: ReadingKind::Range, value, synthetic: false }, path, band: BAND, per_wall_m: per_wall, spread_m: 0.0, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 }
    }

    /// Each reading somewhere else, as a walk takes them.
    fn spread(mut links: Vec<Link>) -> Vec<Link> {
        for (i, l) in links.iter_mut().enumerate() {
            l.reading.tag = [i as f32, 1.2, 0.0];
        }
        links
    }

    fn walk(n: usize, truth: (f32, f32)) -> Vec<Link> {
        let mut rng = Rng::new(5);
        spread(
            (0..n)
                .map(|i| {
                    let d = 3.0 + (i % 17) as f32;
                    let (metal, wood) = match i % 4 {
                        0 => (0.0, 0.0),
                        1 => (1.0, 0.0),
                        2 => (0.0, 1.0),
                        _ => (1.0, 1.0),
                    };
                    // Not in step with the materials: each one is seen from both anchors.
                    mk(&mut rng, if i % 3 == 0 { "A1" } else { "A2" }, d, metal * 6.0, wood * 0.4, truth, if i % 3 == 0 { 0.1 } else { -0.05 })
                })
                .collect(),
        )
    }

    #[test]
    fn recovers_a_hidden_material_scale_and_predicts_held_out_readings_better() {
        // The site's metal delays half as much as the built-in value says.
        let f = fit(&walk(60, (0.5, 1.0)), 0.5).unwrap();
        println!("{f:#?}");
        let metal = f.params.iter().find(|p| p.name == "metal delay scale").unwrap();
        assert!((metal.value - 0.5).abs() < 0.1, "{metal:?}");
        assert!(metal.learned > 0.9, "{metal:?}");
        let (before, after) = (f.holdout_range_before.unwrap(), f.holdout_range_after.unwrap());
        assert!(after < 0.5 * before, "held out {before} -> {after}");
        // Every spot gets held out once, with a model that never saw it.
        assert_eq!((f.held_out, f.held_out_spots), (60, 60));
        assert!(f.applied.contains(&"metal delay scale".to_string()));
    }

    #[test]
    fn a_blocked_links_noise_is_owned_up_to() {
        // Delay through metal spreads exponentially, as on a real blocked link. The fit
        // must land near the truth with an uncertainty that actually covers it.
        let mut rng = Rng::new(21);
        let (bias, spread_m, truth) = (0.35f32, 0.25f32, 0.5f32);
        let links: Vec<Link> = spread(
            (0..120)
                .map(|i| {
                    let metal = if i % 2 == 0 { 6.0 } else { 0.0 };
                    let d = 3.0 + (i % 11) as f32;
                    let mut path = Path { distance: d, ..Default::default() };
                    path.per_material[Material::Metal as usize] = metal;
                    let value = d + metal * truth * bias + rng.exp(metal * truth * spread_m) + rng.gauss(0.1);
                    Link { reading: Reading { device: "A1".into(), tag: [0.0; 3], kind: ReadingKind::Range, value, synthetic: false }, path, band: BAND, per_wall_m: bias + spread_m, spread_m, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 }
                })
                .collect(),
        );
        let f = fit(&links, 0.5).unwrap();
        let m = f.params.iter().find(|p| p.name == "metal delay scale").unwrap();
        println!("{m:?}");
        assert!((m.value - truth as f64).abs() < 3.0 * m.sigma, "{m:?}");
        assert!(m.sigma > 0.02, "a noisy link can't give a razor-thin answer: {m:?}");
    }

    fn scale(name: &str, value: f64) -> Learned {
        Learned { name: name.into(), prior: 1.0, value, sigma: 0.1, learned: 0.9 }
    }

    /// A fit that passed its checks, as `fit` would hand it to `apply`.
    fn passed(origin: WalkOrigin, params: Vec<Learned>) -> SurveyFit {
        SurveyFit { origin, synthetic: origin == WalkOrigin::Practice, applied: params.iter().map(|p| p.name.clone()).collect(), params, range_band: BAND, rssi_band: BAND, ..Default::default() }
    }

    fn evidence(m: Material, scene: SceneBinding) -> Evidence {
        evidence_of(&[m], &[BAND], scene)
    }

    #[test]
    fn a_practice_walk_never_counts_as_a_calibration() {
        let _g = lock();
        // Wood: no other test relies on it (the calibration is process-wide).
        let both = || vec![scale("wood loss scale", 2.0), scale("wood delay scale", 2.0)];
        apply(&passed(WalkOrigin::Practice, both()), "walk", SCENE);
        let c = MaterialCal::applied_in(Material::Wood, BAND).unwrap();
        assert!(c.label.starts_with(PRACTICE), "{}", c.label);
        assert_eq!((c.source.delay_origin, c.source.loss_origin), (Origin::Practice, Origin::Practice));
        assert_eq!(evidence(Material::Wood, SCENE), Evidence::Estimate, "a made-up truth is less than a simulation");
        clear_applied();
        assert!(MaterialCal::applied_in(Material::Wood, BAND).is_none());
        apply(&passed(WalkOrigin::Field, both()), "walk", SCENE);
        let c = MaterialCal::applied_in(Material::Wood, BAND).unwrap();
        assert!(c.label.starts_with(FIELD), "{}", c.label);
        assert_eq!(evidence(Material::Wood, SCENE), Evidence::Calibrated);
        clear_applied();
        apply(&passed(WalkOrigin::Field, vec![scale("wood loss scale", 2.0)]), "walk", SCENE);
        assert_eq!(evidence(Material::Wood, SCENE), Evidence::Simulated, "only loss was fitted");
        clear_applied();
    }

    #[test]
    fn practice_then_a_real_loss_walk_is_not_a_delay_fit() {
        let _g = lock();
        apply(&passed(WalkOrigin::Practice, vec![scale("wood delay scale", 2.0)]), "practice.jsonl", SCENE);
        apply(&passed(WalkOrigin::Field, vec![scale("wood loss scale", 1.1)]), "real.csv", SCENE);
        let c = MaterialCal::applied_in(Material::Wood, BAND).unwrap();
        let e = evidence(Material::Wood, SCENE);
        clear_applied();
        assert_eq!(c.source.delay_origin, Origin::None, "{c:?}");
        assert_eq!(c.bias_scale, 1.0, "the synthetic delay must not survive into a real fit: {c:?}");
        assert_eq!(c.source.loss_origin, Origin::Field);
        assert_ne!(e, Evidence::Calibrated);
    }

    #[test]
    fn a_practice_walk_takes_nothing_from_a_real_one_either() {
        let _g = lock();
        apply(&passed(WalkOrigin::Field, vec![scale("wood delay scale", 1.4), scale("wood loss scale", 1.1)]), "real.csv", SCENE);
        apply(&passed(WalkOrigin::Practice, vec![scale("wood loss scale", 0.2)]), "practice.jsonl", SCENE);
        let c = MaterialCal::applied_in(Material::Wood, BAND).unwrap();
        let e = evidence(Material::Wood, SCENE);
        clear_applied();
        assert_eq!((c.source.delay_origin, c.bias_scale), (Origin::None, 1.0), "{c:?}");
        assert_eq!(e, Evidence::Estimate);
    }

    #[test]
    fn a_later_real_walk_keeps_what_an_earlier_real_walk_of_this_scene_fitted() {
        let _g = lock();
        apply(&passed(WalkOrigin::Field, vec![scale("wood delay scale", 1.4)]), "a.csv", SCENE);
        apply(&passed(WalkOrigin::Field, vec![scale("wood loss scale", 1.1)]), "b.csv", SCENE);
        let c = MaterialCal::applied_in(Material::Wood, BAND).unwrap();
        let e = evidence(Material::Wood, SCENE);
        // The geometry changed in between: the first walk saw another building.
        apply(&passed(WalkOrigin::Field, vec![scale("wood loss scale", 1.1)]), "b.csv", SceneBinding { geometry: 9, ..SCENE });
        let moved = MaterialCal::applied_in(Material::Wood, BAND).unwrap();
        clear_applied();
        assert_eq!((c.bias_scale, c.loss_scale), (1.4, 1.1));
        assert_eq!(e, Evidence::Calibrated);
        assert_eq!((moved.bias_scale, moved.source.delay_origin), (1.0, Origin::None));
    }

    #[test]
    fn a_delay_in_a_file_name_is_not_a_delay_fit() {
        let _g = lock();
        apply(&passed(WalkOrigin::Field, vec![scale("object loss scale", 1.1)]), "delay_walk.csv", SCENE);
        let c = MaterialCal::applied_in(Material::Generic, BAND).unwrap();
        let e = evidence(Material::Generic, SCENE);
        clear_applied();
        assert!(c.label.contains("delay_walk"), "{}", c.label);
        assert_eq!(c.source.delay_origin, Origin::None);
        assert_eq!(e, Evidence::Simulated);
    }

    #[test]
    fn an_undeclared_walk_writes_nothing() {
        let _g = lock();
        assert_eq!(apply(&passed(WalkOrigin::Undeclared, vec![scale("wood delay scale", 2.0)]), "walk.csv", SCENE), 0);
        assert!(MaterialCal::applied_in(Material::Wood, BAND).is_none());
    }

    /// `n` readings through concrete from each device, at `spots` places.
    fn through_concrete(spots: usize, devices: &[&str], weight: f32, spread_m: f32, repeats: usize) -> Vec<Link> {
        let mut rng = Rng::new(4);
        let mut out = Vec::new();
        for s in 0..spots {
            for d in devices {
                for _ in 0..repeats {
                    let mut path = Path { distance: 6.0, ..Default::default() };
                    path.per_material[Material::Concrete as usize] = weight;
                    let value = 6.0 + weight * 0.6 + rng.gauss(0.1);
                    out.push(Link { reading: Reading { device: (*d).into(), tag: [s as f32, 1.2, 1.0], kind: ReadingKind::Range, value, synthetic: false }, path, band: BAND, per_wall_m: 0.6, spread_m, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 });
                }
            }
        }
        out
    }

    #[test]
    fn five_readings_at_one_spot_through_one_anchor_apply_nothing() {
        // The audit's case: repeats are one observation, so there's nothing to fit.
        let e = fit(&through_concrete(1, &["A1"], 2.0, 0.25, 5), 0.5).unwrap_err().to_string();
        assert!(e.contains("too few"), "{e}");
    }

    #[test]
    fn five_spots_through_one_anchor_apply_nothing() {
        // Its arithmetic gave learned = 0.71: enough for the old check.
        let f = fit(&through_concrete(5, &["A1"], 2.0, 0.25, 1), 0.5).unwrap();
        assert!(f.applied.is_empty(), "{f:#?}");
        assert!(f.not_applied.iter().any(|w| w.contains("1 devices")), "{:?}", f.not_applied);
    }

    #[test]
    fn a_burst_at_each_spot_counts_once() {
        let f = fit(&through_concrete(3, &["A1", "A2"], 2.0, 0.25, 10), 0.5).unwrap();
        assert_eq!((f.readings, f.distinct_readings, f.spots), (60, 6, 3));
        assert!(f.applied.is_empty(), "{f:#?}");
    }

    #[test]
    fn a_wide_prior_does_not_buy_a_calibration() {
        // Enough places, little lever: the readings can't say much, whatever was assumed.
        let f = fit(&through_concrete(5, &["A1", "A2"], 0.5, 0.25, 1), 5.0).unwrap();
        let p = f.params.iter().find(|p| p.name == "concrete delay scale").unwrap();
        assert!(p.learned > 0.5, "the old check would have applied it: {p:?}");
        assert!(f.applied.is_empty(), "{f:#?}");
        assert!(f.not_applied.iter().any(|w| w.contains("still ±")), "{:?}", f.not_applied);
    }

    #[test]
    fn a_narrow_prior_does_not_buy_one_either() {
        let f = fit(&through_concrete(5, &["A1", "A2"], 0.5, 0.25, 1), 0.01).unwrap();
        let p = f.params.iter().find(|p| p.name == "concrete delay scale").unwrap();
        assert!(p.sigma < MAX_SCALE_SIGMA, "{p:?}");
        assert!(f.applied.is_empty(), "{f:#?}");
    }

    #[test]
    fn the_held_out_score_is_of_the_model_that_gets_applied() {
        // The site really has exponent 2.8, which `apply` never writes. Scoring the
        // fitted exponent would report an improvement nobody gets.
        let mut rng = Rng::new(9);
        let links: Vec<Link> = (0..100)
            .map(|i| {
                let d = 2.0 + (i % 23) as f32;
                let mut path = Path { distance: d, ..Default::default() };
                let c = (i % 2) as f32;
                path.per_material[Material::Concrete as usize] = c * 2.0;
                let value = -59.0 - 28.0 * d.log10() - c * 2.0 * 6.0 + rng.gauss(1.0);
                Link { reading: Reading { device: if i % 2 == 0 { "B1".into() } else { "B2".into() }, tag: [i as f32, 1.2, 0.0], kind: ReadingKind::Rssi, value, synthetic: false }, path, band: BAND, per_wall_m: 0.6, spread_m: 0.0, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 }
            })
            .collect();
        let f = fit(&links, 0.5).unwrap();
        let (before, after) = (f.holdout_rssi_before.unwrap(), f.holdout_rssi_after.unwrap());
        assert!(after > 0.8 * before, "the exponent isn't deployed, so the score can't drop to the noise: {before} -> {after}");
    }

    /// Every link through concrete, every spot off by something both anchors share, adding
    /// up to nothing: each group of spots is pulled the wrong way by the others.
    fn misleading_walk() -> Vec<Link> {
        let off = [0.3, -0.3, 0.3, -0.3, 0.0, 0.3, -0.3, 0.3, -0.3, 0.0];
        let mut out = Vec::new();
        for (s, e) in off.iter().enumerate() {
            for d in ["A1", "A2"] {
                let mut path = Path { distance: 6.0, ..Default::default() };
                path.per_material[Material::Concrete as usize] = 2.0;
                out.push(Link { reading: Reading { device: d.into(), tag: [s as f32, 1.2, 0.0], kind: ReadingKind::Range, value: 6.0 + 1.2 + e, synthetic: false }, path, band: BAND, per_wall_m: 0.6, spread_m: 0.0, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 });
            }
        }
        out
    }

    #[test]
    fn a_worse_held_out_score_applies_nothing() {
        let f = fit(&misleading_walk(), 0.5).unwrap();
        let (before, after) = (f.holdout_range_before.unwrap(), f.holdout_range_after.unwrap());
        assert!(after > before, "{before} -> {after}");
        assert!(f.applied.is_empty(), "{f:#?}");
        assert!(f.not_applied.iter().any(|w| w.contains("isn't better")), "{:?}", f.not_applied);
    }

    #[test]
    fn whole_spots_are_held_out_and_the_sample_is_reported() {
        let f = fit(&through_concrete(12, &["A1", "A2", "A3", "A4"], 2.0, 0.0, 1), 0.5).unwrap();
        assert_eq!((f.spots, f.held_out_spots, f.held_out), (12, 12, 48));
    }

    #[test]
    fn a_model_that_misses_the_walk_applies_nothing() {
        let mut links = walk(60, (0.5, 1.0));
        // Half a metre of scatter nothing in the model can explain, on 15 cm readings.
        let mut rng = Rng::new(31);
        links.iter_mut().for_each(|l| l.reading.value += rng.gauss(0.5));
        let f = fit(&links, 0.5).unwrap();
        assert!(f.residual_sigmas > MAX_RESIDUAL, "{}", f.residual_sigmas);
        assert!(f.applied.is_empty(), "{f:#?}");
        assert!(f.not_applied.iter().any(|w| w.contains("misses")), "{:?}", f.not_applied);
    }

    #[test]
    fn a_scale_stuck_at_its_limit_applies_nothing() {
        // Readings say concrete delays far more than the model can reach.
        let mut links = through_concrete(10, &["A1", "A2"], 0.5, 0.0, 1);
        links.iter_mut().for_each(|l| l.reading.value += 3.0);
        let f = fit(&links, 5.0).unwrap();
        let p = f.params.iter().find(|p| p.name == "concrete delay scale").unwrap();
        assert!(p.value > 5.9, "{p:?}");
        assert!(f.applied.is_empty(), "{f:#?}");
    }

    #[test]
    fn a_material_nobody_crossed_much_stays_labelled_unknown() {
        // A thin sheet of wood adds so little that a few readings can't say much about its scale.
        let mut rng = Rng::new(3);
        let links: Vec<Link> = spread((0..12).map(|i| mk(&mut rng, "A1", 3.0 + i as f32, if i % 2 == 0 { 6.0 } else { 0.0 }, 0.05, (1.0, 1.0), 0.0)).collect());
        let f = fit(&links, 0.5).unwrap();
        assert!(f.still_unknown.contains(&"wood delay scale".to_string()), "{f:#?}");
        assert!(!f.applied.contains(&"wood delay scale".to_string()));
    }

    #[test]
    fn signal_strength_fits_exponent_and_material_loss() {
        let mut rng = Rng::new(9);
        let links: Vec<Link> = spread(
            (0..150)
                .map(|i| {
                    let d = 2.0 + (i % 23) as f32;
                    let concrete = (i % 3) as f32;
                    let mut path = Path { distance: d, ..Default::default() };
                    path.per_material[Material::Concrete as usize] = concrete * 2.0;
                    // The truth: exponent 2.8, concrete costs 1.5× the built-in.
                    let value = -59.0 - 28.0 * d.log10() - concrete * 2.0 * 6.0 * 1.5 + rng.gauss(2.0);
                    Link { reading: Reading { device: if i % 2 == 0 { "B1".into() } else { "B2".into() }, tag: [0.0; 3], kind: ReadingKind::Rssi, value, synthetic: false }, path, band: BAND, per_wall_m: 0.6, spread_m: 0.0, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 }
                })
                .collect(),
        );
        let f = fit(&links, 0.5).unwrap();
        let get = |n: &str| f.params.iter().find(|p| p.name == n).unwrap().value;
        assert!((get("Path loss exponent") - 2.8).abs() < 0.3, "{f:#?}");
        assert!((get("concrete loss scale") - 1.5).abs() < 0.2, "{f:#?}");
        assert!(f.holdout_rssi_after.unwrap() < f.holdout_rssi_before.unwrap());
    }

    #[test]
    fn a_bad_reading_is_set_aside() {
        let mut links = walk(60, (1.0, 1.0));
        links[10].reading.value += 40.0;
        let f = fit(&links, 0.5).unwrap();
        assert!(f.outliers >= 1, "{f:#?}");
        let metal = f.params.iter().find(|p| p.name == "metal delay scale").unwrap();
        assert!((metal.value - 1.0).abs() < 0.15, "{metal:?}");
    }

    #[test]
    fn a_real_delay_fit_counts_only_for_its_own_scene() {
        let _g = lock();
        apply(&passed(WalkOrigin::Field, vec![scale("wood delay scale", 1.2), scale("wood loss scale", 1.2)]), "walk.csv", SCENE);
        let here = evidence(Material::Wood, SCENE);
        let other_tab = evidence(Material::Wood, SceneBinding { id: 2, ..SCENE });
        let edited = evidence(Material::Wood, SceneBinding { geometry: 3, ..SCENE });
        let other_band = evidence_of(&[Material::Wood], &[CalBand::Ghz5], SCENE);
        clear_applied();
        assert_eq!(here, Evidence::Calibrated);
        assert_eq!(other_tab, Evidence::Simulated);
        assert_eq!(edited, Evidence::Simulated, "the geometry changed since the walk");
        assert_eq!(other_band, Evidence::Simulated);
    }

    /// A walk that passes everything: ranges and signal strength, both kinds through
    /// concrete, from two anchors, at twenty spots.
    fn good_walk() -> Vec<Link> {
        let mut rng = Rng::new(12);
        let mut out = Vec::new();
        for s in 0..20 {
            for dev in ["A1", "A2"] {
                let walled = s % 2 == 0;
                let d = 4.0 + (s % 7) as f32;
                let mut path = Path { distance: d, ..Default::default() };
                path.per_material[Material::Concrete as usize] = if walled { 2.0 } else { 0.0 };
                let w = path.per_material[Material::Concrete as usize];
                let base = |kind, value| Link { reading: Reading { device: dev.into(), tag: [s as f32, 1.2, 0.0], kind, value, synthetic: false }, path, band: BAND, per_wall_m: 0.6, spread_m: 0.1, rssi_at_1m: -59.0, path_loss_exp: 2.2, wall_db: 6.0 };
                out.push(base(ReadingKind::Range, d + w * 0.6 * 0.5 + rng.gauss(0.05)));
                out.push(base(ReadingKind::Rssi, -59.0 - 22.0 * d.log10() - w * 6.0 * 1.6 + rng.gauss(1.0)));
            }
        }
        out
    }

    #[test]
    fn predict_again_narrows_the_range_after_a_real_walk() {
        let _g = lock();
        let mut f = fit(&good_walk(), 0.5).unwrap();
        assert!(f.applied.contains(&"concrete delay scale".to_string()) && f.applied.contains(&"concrete loss scale".to_string()), "{f:#?}");
        let before = material_sigma(&[Material::Concrete], &[BAND], SCENE);
        f.origin = WalkOrigin::Field;
        assert_eq!(apply(&f, "walk.csv", SCENE), 1);
        let c = MaterialCal::applied_in(Material::Concrete, BAND).unwrap();
        let (e, after, edited) = (evidence(Material::Concrete, SCENE), material_sigma(&[Material::Concrete], &[BAND], SCENE), material_sigma(&[Material::Concrete], &[BAND], SceneBinding { geometry: 9, ..SCENE }));
        clear_applied();
        assert_eq!(e, Evidence::Calibrated);
        assert!((c.bias_scale - 0.5).abs() < 0.15 && (c.loss_scale - 1.6).abs() < 0.2, "{c:?}");
        assert!(c.source.delay_sigma.is_some_and(|s| s > 0.0) && c.source.loss_sigma.is_some_and(|s| s > 0.0), "{c:?}");
        assert_eq!(before, 0.3);
        assert!(after < 0.3, "{after}");
        assert_eq!(edited, 0.3, "a fit of another geometry narrows nothing");
    }

    #[test]
    fn a_walk_in_two_bands_of_one_kind_is_refused() {
        let mut links = walk(40, (1.0, 1.0));
        links[3].band = CalBand::Ghz6;
        assert!(fit(&links, 0.5).unwrap_err().to_string().contains("more than one band"));
    }

    #[test]
    fn range_and_signal_walks_write_to_their_own_bands() {
        let _g = lock();
        let mut f = passed(WalkOrigin::Field, vec![scale("wood delay scale", 1.2), scale("wood loss scale", 1.4)]);
        f.rssi_band = CalBand::Ghz5;
        assert_eq!(apply(&f, "walk.csv", SCENE), 2);
        let (uwb, ble) = (MaterialCal::scales_in(Material::Wood, BAND), MaterialCal::scales_in(Material::Wood, CalBand::Ghz5));
        clear_applied();
        assert_eq!(uwb, (1.0, 1.2));
        assert_eq!(ble, (1.4, 1.0));
    }

    fn practice_readings() -> Vec<Reading> {
        (0..12).map(|i| Reading { device: format!("A{}", i % 3), tag: [i as f32 * 0.7, 1.2, 2.0], kind: ReadingKind::Range, value: 3.137 + i as f32 * 0.41, synthetic: true }).collect()
    }

    #[test]
    fn a_practice_walk_cant_come_back_as_a_real_one() {
        let walk = practice_readings();
        let mut known = PracticeWalks::default();
        known.record(&walk);
        let declare_field = |r: &[Reading]| resolve_origin(Some(WalkOrigin::Field), r, &known);
        // With its flags, as written.
        assert!(declare_field(&walk).is_err());
        // Pasted inline without them, as an agent would.
        let bare: Vec<Reading> = walk.iter().cloned().map(|r| Reading { synthetic: false, ..r }).collect();
        assert!(declare_field(&bare).is_err());
        // Converted to CSV: the flag has nowhere to go, the values do.
        let csv: String = walk.iter().map(|r| format!("{},{:.3},{:.3},{:.3},range,{:.3}\n", r.device, r.tag[0], r.tag[1], r.tag[2], r.value)).collect();
        let parsed = parse_readings(&csv).unwrap();
        assert!(parsed.iter().all(|r| !r.synthetic));
        assert!(declare_field(&parsed).is_err());
        // Never declared, it's still known for what it is.
        assert_eq!(resolve_origin(None, &parsed, &known).unwrap(), WalkOrigin::Practice);
        assert_eq!(resolve_origin(Some(WalkOrigin::Practice), &parsed, &known).unwrap(), WalkOrigin::Practice);
        // Half of it mixed into a real walk doesn't launder it.
        let mut mixed = parsed[..6].to_vec();
        mixed.extend((0..6).map(|i| Reading { device: "A9".into(), tag: [i as f32, 0.0, 0.0], kind: ReadingKind::Range, value: 9.0, synthetic: false }));
        assert!(declare_field(&mixed).is_err());
    }

    #[test]
    fn a_csv_row_is_not_field_data_until_someone_says_so() {
        let parsed = parse_readings("A1,1,1,1,range,4.5\nA2,2,1,1,range,5.5\n").unwrap();
        let known = PracticeWalks::default();
        assert_eq!(resolve_origin(None, &parsed, &known).unwrap(), WalkOrigin::Undeclared);
        assert_eq!(resolve_origin(Some(WalkOrigin::Field), &parsed, &known).unwrap(), WalkOrigin::Field);
    }

    #[test]
    fn the_record_of_practice_walks_survives_a_restart() {
        let mut known = PracticeWalks::default();
        known.record(&practice_readings());
        let back: PracticeWalks = serde_json::from_str(&serde_json::to_string(&known).unwrap()).unwrap();
        assert!(back.matches(&practice_readings()));
    }

    #[test]
    fn the_geometry_signature_follows_the_objects_and_not_their_order() {
        let at = |x: f32| Transform::from_xyz(x, 0.0, 0.0);
        let wall = |x: f32| object_signature("wall", std::path::Path::new("a.usd"), &at(x), 1, (Material::Concrete, 2.0));
        let shelf = object_signature("shelf", std::path::Path::new("b.usd"), &at(1.0), 3, (Material::Metal, 6.0));
        assert_eq!(wall(0.0).wrapping_add(shelf), shelf.wrapping_add(wall(0.0)));
        assert_ne!(wall(0.0), wall(0.5), "a moved wall is another building");
        let stated = object_signature("wall", std::path::Path::new("a.usd"), &at(0.0), 1, (Material::Thin, 0.15));
        assert_ne!(wall(0.0), stated, "so is a wall now said to be drywall");
    }

    #[test]
    fn readings_that_arent_numbers_are_refused() {
        assert!(parse_readings("A1,1,1,1,range,nan").unwrap_err().to_string().contains("isn't a number"));
        assert!(parse_readings("A1,inf,1,1,range,3").is_err());
    }

    #[test]
    fn a_device_read_both_ways_gets_one_parameter_of_each() {
        let mut rng = Rng::new(2);
        let mut links = walk(20, (1.0, 1.0));
        for (i, l) in links.iter_mut().enumerate() {
            l.reading.device = "A1".into();
            if i % 2 == 0 {
                l.reading.kind = ReadingKind::Rssi;
                l.reading.value = -60.0 + rng.gauss(1.0);
            }
        }
        let (params, _) = parameters(&links, 0.5);
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).filter(|n| n.starts_with("A1")).collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn too_few_readings_is_a_message() {
        assert!(fit(&walk(3, (1.0, 1.0)), 0.5).is_err());
    }

    #[test]
    fn files_parse_in_both_forms_and_errors_name_the_line() {
        let csv = "device,x,y,z,kind,value\nA1,1,1.2,3,range,4.5\n# a comment\nB1,0,1,0,RSSI,-70\n";
        let r = parse_readings(csv).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].kind, ReadingKind::Range);
        assert_eq!(r[1].kind, ReadingKind::Rssi);
        assert_eq!(r[1].value, -70.0);
        let j = r#"{"device":"A1","tag":[1,2,3],"kind":"range","value":2.5}"#;
        assert_eq!(parse_readings(j).unwrap()[0].tag, [1.0, 2.0, 3.0]);
        let e = parse_readings("A1,1,2").unwrap_err().to_string();
        assert!(e.contains("line 1"), "{e}");
        let e = parse_readings("A1,x,2,3,range,1").unwrap_err().to_string();
        assert!(e.contains("isn't a number"), "{e}");
        assert!(parse_readings("device,x,y,z,kind,value\n").is_err());
    }

    #[test]
    fn applying_sets_only_what_was_learned_and_clearing_undoes_it() {
        let _g = lock();
        // Glass only: no other test relies on it (the calibration is process-wide).
        let f = SurveyFit {
            origin: WalkOrigin::Field,
            params: vec![
                Learned { name: "glass loss scale".into(), prior: 1.0, value: 2.0, sigma: 0.1, learned: 0.9 },
                Learned { name: "glass delay scale".into(), prior: 1.0, value: 9.0, sigma: 0.5, learned: 0.1 },
            ],
            applied: vec!["glass loss scale".into()],
            range_band: BAND,
            rssi_band: BAND,
            ..Default::default()
        };
        apply(&f, "walk.csv", SCENE);
        let (l, b) = MaterialCal::scales_in(Material::Glass, BAND);
        assert_eq!(l, 2.0);
        assert_eq!(b, 1.0, "barely learned, so it stays at the default");
        clear_applied();
        assert_eq!(MaterialCal::scales_in(Material::Glass, BAND), (1.0, 1.0));
    }
}
