//! Radio channel models. Pure functions of geometry (distance, the objects
//! crossed by the scene ray cast and what they are made of, antenna angle)
//! plus a seeded RNG.

use bevy::math::Vec3;

use super::device::Profile;
use super::rng::Rng;
use super::solver::fspl_db;
use crate::agent::catalog::tokens;
use crate::agent::structures::Kind;

pub const C_M_PER_NS: f32 = 0.299_792_46;

/// What an obstructing object is made of, as far as radio cares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Material {
    /// Cardboard, paper, plastic, fabric: almost transparent.
    Thin,
    /// Wood, plywood, pallets: passes, slightly slower and weaker.
    Wood,
    Glass,
    /// Unknown objects: one "typical wall".
    #[default]
    Generic,
    /// Concrete / masonry structure: heavy loss, UWB still gets through.
    Concrete,
    /// Conductors reflect UWB instead of passing it: no direct path.
    Metal,
    /// Water-based contents absorb UWB (6-8 GHz) almost completely.
    Liquid,
}

impl Material {
    /// In discriminant order (`m as usize` indexes it).
    pub const ALL: [Material; 7] = [Self::Thin, Self::Wood, Self::Glass, Self::Generic, Self::Concrete, Self::Metal, Self::Liquid];

    /// Obstruction strength relative to a typical wall (6 dB, `nlos_bias_m`).
    pub fn weight(self) -> f32 {
        match self {
            Self::Thin => 0.15,
            Self::Wood => 0.4,
            Self::Glass => 0.5,
            Self::Generic => 1.0,
            Self::Concrete => 2.0,
            Self::Metal => 6.0,
            Self::Liquid => 4.0,
        }
    }

    /// No UWB first path through this material.
    pub fn blocks_uwb(self) -> bool {
        matches!(self, Self::Metal | Self::Liquid)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Thin => "thin",
            Self::Wood => "wood",
            Self::Glass => "glass",
            Self::Generic => "object",
            Self::Concrete => "concrete",
            Self::Metal => "metal",
            Self::Liquid => "liquid",
        }
    }

    /// Guesses the material from an asset path (`sm_barrier_fence_metal_a02_01.usd`,
    /// generated structures). A material word in the name wins over what the
    /// object usually is (`barrier_wall_plastic` is plastic, not a wall).
    pub fn of_asset(asset: &std::path::Path) -> Self {
        // Given a see-through look: that says more than what it's called or what it was.
        if crate::agent::assetgen::see_through_asset(asset) {
            return Self::Glass;
        }
        if let Some(k) = Kind::of_asset(asset) {
            return if k == Kind::Beam { Self::Metal } else { Self::Concrete };
        }
        let stem = asset.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
        Self::of_tokens(&tokens(&stem))
    }

    fn of_tokens(toks: &[String]) -> Self {
        const MATERIALS: &[(Material, &[&str])] = &[
            (Material::Liquid, &["water", "oil", "coolant", "lubricant", "lube", "fluid", "antifreeze", "solvent", "fuel", "diesel", "bleach", "disinfectant", "blood", "liquid", "beverage", "wine"]),
            // Ahead of metal: an aluminium-framed glass door is mostly glass, as in `material_for_element`.
            (Material::Glass, &["glass", "window", "windscreen", "windshield", "mirror"]),
            (Material::Metal, &["metal", "steel", "aluminium", "aluminum", "iron", "tin", "chrome", "silver", "gold"]),
            (Material::Wood, &["wood", "wooden", "plywood", "timber", "pallet"]),
            (Material::Thin, &["cardboard", "corrugated", "paper", "plastic", "fabric", "cloth", "foam", "microfiber", "towel", "bedding", "drape", "blinds", "sponge", "bag", "carton"]),
        ];
        // What kinds of objects are usually made of (or filled with). Lowercase,
        // as `tokens` returns them.
        const KINDS: &[(Material, &[&str])] = &[
            (Material::Metal, &["rack", "rackable", "shelf", "cabinet", "cart", "trolley", "drum", "cylinder", "tank", "engine", "enginecomponent", "engineassembly", "drivetrain", "brakesystem", "machine", "equipment", "compressor", "aircompressor", "refrigerator", "beveragefridge", "dishwasher", "appliance", "conveyor", "robot", "forklift", "vehicle", "car", "bodypart", "carframe", "workbench", "ladder", "lifttable", "lifter", "fence", "gate", "beam", "truss", "locker", "vise", "radiator", "transmission", "wheelbarrow", "fireextinguisher", "electricbox", "hvgenerator", "generator"]),
            (Material::Liquid, &["bottle", "canister", "reservoir", "jerrycan", "keg", "iv"]),
            (Material::Concrete, &["wall", "column", "pillar", "floor", "ceiling", "slab", "concrete", "brick"]),
            (Material::Wood, &["table", "desk", "bench", "furniture"]),
            (Material::Thin, &["box", "crate", "bin", "chair"]),
        ];
        for table in [MATERIALS, KINDS] {
            for (m, words) in table {
                if toks.iter().any(|t| words.contains(&t.to_ascii_lowercase().as_str())) {
                    return *m;
                }
            }
        }
        Self::Generic
    }
}

/// The radio band a calibration was made in. A material's loss and delay change with
/// frequency, so a result from one band says nothing about another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalBand {
    /// Every band: what-if scalings (stress tests, material ranges), never a measurement.
    #[default]
    Any,
    /// UHF RFID.
    Uhf,
    /// BLE and WiFi at 2.4 GHz.
    Ghz2,
    /// WiFi at 5 GHz.
    Ghz5,
    /// UWB channel 5.
    Ghz6,
    /// UWB channel 9.
    Ghz8,
}

impl CalBand {
    pub const ALL: [CalBand; 6] = [Self::Any, Self::Uhf, Self::Ghz2, Self::Ghz5, Self::Ghz6, Self::Ghz8];

    pub fn of_mhz(f: f32) -> Self {
        match f {
            f if f < 1500.0 => Self::Uhf,
            f if f < 3500.0 => Self::Ghz2,
            f if f < 6000.0 => Self::Ghz5,
            f if f <= 7200.0 => Self::Ghz6,
            _ => Self::Ghz8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "all bands",
            Self::Uhf => "UHF",
            Self::Ghz2 => "2.4 GHz",
            Self::Ghz5 => "5 GHz",
            Self::Ghz6 => "6.5 GHz",
            Self::Ghz8 => "8 GHz",
        }
    }
}

/// What a fitted scale rests on. FDTD and the built-in values are `None`: no reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    None,
    /// A synthetic walk, made up to try the loop. Never evidence.
    Practice,
    /// Readings someone declared as measured on site.
    Field,
}

/// The scene a calibration was made on: which one, and its geometry at the time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneBinding {
    pub id: u64,
    pub geometry: u64,
}

/// Where a calibration came from and what it can be used for. Every decision about
/// evidence reads these, never `MaterialCal::label`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalSource {
    /// Result folder or walk file, to tell which run is applied.
    pub path: std::path::PathBuf,
    pub band: CalBand,
    pub delay_origin: Origin,
    pub loss_origin: Origin,
    pub scene: Option<SceneBinding>,
    /// 1σ of the fitted scales (survey walks).
    pub delay_sigma: Option<f32>,
    pub loss_sigma: Option<f32>,
    /// What-if only: multiplies what is in force instead of replacing it.
    pub relative: bool,
}

/// A material's loss and UWB delay against the ray model's built-in values, from FDTD
/// ("through an object", "Use in the floor map"), a survey walk or a what-if.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialCal {
    pub loss_scale: f32,
    pub bias_scale: f32,
    /// For people, e.g. "FDTD · wall_3 · UWB ch5".
    pub label: String,
    pub source: CalSource,
}

impl Default for MaterialCal {
    fn default() -> Self {
        Self { loss_scale: 1.0, bias_scale: 1.0, label: String::new(), source: Default::default() }
    }
}

/// Process-wide because every ray cast (`Path::cross_weighted`) reads it, deep inside
/// code that has no access to the ECS world. Band first, then material; `CalBand::Any`
/// holds the what-ifs, which only last for one synchronous run.
static MATERIAL_CAL: std::sync::RwLock<[[Option<MaterialCal>; 7]; 6]> = std::sync::RwLock::new([const { [const { None }; 7] }; 6]);

/// Tests that write band calibrations hold this: they share one table.
#[cfg(test)]
pub static CAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// A tab switch moves the whole table, which would pull calibrations from under tests
// running alongside. Only a test about that, holding the lock, turns it on for itself.
#[cfg(test)]
thread_local! {
    pub static SWAPS_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A tab's calibrations while it isn't showing.
#[derive(Default)]
pub struct ParkedCals([[Option<MaterialCal>; 7]; 6]);

impl MaterialCal {
    /// (loss, delay) scale for `m` outside any band: what-ifs only, 1 = the built-in
    /// weights. Called per ray crossing.
    pub fn scales(m: Material) -> (f32, f32) {
        Self::scales_in(m, CalBand::Any)
    }

    /// (loss, delay) scale for `m` as a device in `band` sees it.
    pub fn scales_in(m: Material, band: CalBand) -> (f32, f32) {
        let Ok(t) = MATERIAL_CAL.read() else { return (1.0, 1.0) };
        let measured = if band == CalBand::Any { None } else { t[band as usize][m as usize].as_ref() };
        let base = measured.map_or((1.0, 1.0), |c| (c.loss_scale, c.bias_scale));
        match &t[CalBand::Any as usize][m as usize] {
            Some(w) if w.source.relative => (base.0 * w.loss_scale, base.1 * w.bias_scale),
            Some(w) => (w.loss_scale, w.bias_scale),
            None => base,
        }
    }

    /// The what-if in force for `m`, which a recipe saves and puts back.
    pub fn applied(m: Material) -> Option<MaterialCal> {
        Self::applied_in(m, CalBand::Any)
    }

    /// The calibration of `m` in `band`.
    pub fn applied_in(m: Material, band: CalBand) -> Option<MaterialCal> {
        MATERIAL_CAL.read().ok().and_then(|t| t[band as usize][m as usize].clone())
    }

    /// Stores `cal` in the band it names. `None` clears the what-if only.
    pub fn set(m: Material, cal: Option<MaterialCal>) {
        if let Ok(mut t) = MATERIAL_CAL.write() {
            let band = cal.as_ref().map_or(CalBand::Any, |c| c.source.band);
            t[band as usize][m as usize] = cal;
        }
    }

    pub fn clear_in(m: Material, band: CalBand) {
        if let Ok(mut t) = MATERIAL_CAL.write() {
            t[band as usize][m as usize] = None;
        }
    }

    /// Forgets every calibration of this scene, not the what-ifs of a run in progress.
    pub fn clear_bands() {
        if let Ok(mut t) = MATERIAL_CAL.write() {
            t.iter_mut().skip(1).for_each(|b| *b = Default::default());
        }
    }

    /// Exchanges the calibrations in force with a parked tab's.
    pub fn swap_parked(parked: &mut ParkedCals) {
        #[cfg(test)]
        if !SWAPS_HERE.get() {
            return;
        }
        if let Ok(mut t) = MATERIAL_CAL.write() {
            t.iter_mut().zip(&mut parked.0).skip(1).for_each(|(a, b)| std::mem::swap(a, b));
        }
    }
}

/// Conditions of a site that change from day to day. Scenarios set these; the
/// defaults reproduce a scene exactly as modelled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SiteConditions {
    /// Share of a rack's bays with goods in (0..1). 1 treats a rack as solid, as
    /// the plain model does; lower lets rays pass through the empty bays.
    pub rack_fill: f32,
    /// People moving through the space.
    pub people_per_100m2: f32,
}

impl Default for SiteConditions {
    fn default() -> Self {
        Self { rack_fill: 1.0, people_per_100m2: 0.0 }
    }
}

static SITE: std::sync::RwLock<SiteConditions> = std::sync::RwLock::new(SiteConditions { rack_fill: 1.0, people_per_100m2: 0.0 });

impl SiteConditions {
    pub fn current() -> Self {
        SITE.read().map_or_else(|_| Self::default(), |s| *s)
    }

    pub fn set(c: Self) {
        if let Ok(mut s) = SITE.write() {
            *s = c;
        }
    }

    /// Chance a link of this length has somebody in it: people are obstacles about
    /// 0.5 m wide, scattered at random.
    pub fn person_in_path(&self, distance: f32) -> f32 {
        1.0 - (-(self.people_per_100m2 / 100.0) * 0.5 * distance.max(0.0)).exp()
    }
}

/// Is this bay of a rack holding goods? Stable for a given bay, so a ray through
/// the same spot gets the same answer from tick to tick.
pub fn bay_has_goods(rack_id: u64, hit: Vec3, fill: f32) -> bool {
    if fill >= 1.0 {
        return true;
    }
    if fill <= 0.0 {
        return false;
    }
    // Bays are roughly 1 m wide and 1 m tall.
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ rack_id;
    for v in [(hit.x).floor() as i32, (hit.y).floor() as i32, (hit.z).floor() as i32] {
        for b in v.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h ^= h >> 29;
    ((h % 10_000) as f32 / 10_000.0) < fill
}

/// Does a ray from `from` to `to` through a rack meet goods anywhere? Every bay along
/// it gets its say: a ray running down a half-full rack is almost certain to hit
/// something, while one crossing a single bay is a coin toss.
pub fn crossing_meets_goods(rack_id: u64, from: Vec3, to: Vec3, fill: f32) -> bool {
    if fill >= 1.0 {
        return true;
    }
    let bays = from.distance(to).ceil().max(1.0) as usize;
    (0..bays).any(|k| bay_has_goods(rack_id, from.lerp(to, (k as f32 + 0.5) / bays as f32), fill))
}

/// Everything that changes what a cast ray returns without the scene moving.
pub fn conditions_signature() -> u64 {
    let c = SiteConditions::current();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |v: u32| h = (h ^ v as u64).wrapping_mul(0x0100_0000_01b3);
    mix(c.rack_fill.to_bits());
    if let Ok(t) = MATERIAL_CAL.read() {
        for slot in t.iter().flatten() {
            let (l, b, rel) = slot.as_ref().map_or((1.0, 1.0, false), |c| (c.loss_scale, c.bias_scale, c.source.relative));
            mix(l.to_bits());
            mix(b.to_bits());
            mix(rel as u32);
        }
    }
    if let Ok(objects) = OBJECT_MATERIALS.read() {
        for (name, (m, w)) in objects.iter() {
            name.bytes().for_each(|b| mix(b as u32));
            mix(*m as u32);
            mix(w.to_bits());
        }
    }
    h
}

static OBJECT_MATERIALS: std::sync::RwLock<std::collections::BTreeMap<String, (Material, f32)>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// Says what one placed object is really made of, overriding the guess from its
/// asset name (a photo, a BIM model or a person knows better). `None` clears it.
pub fn set_object_material(name: &str, m: Option<(Material, f32)>) {
    if let Ok(mut o) = OBJECT_MATERIALS.write() {
        match m {
            Some(v) => {
                o.insert(name.to_string(), v);
            }
            None => {
                o.remove(name);
            }
        }
    }
}

/// Forgets every object's stated material: the scene they belonged to is gone.
pub fn clear_object_materials() {
    if let Ok(mut o) = OBJECT_MATERIALS.write() {
        o.clear();
    }
}

/// What's been stated, for saving with the layout: name to (material, weight).
pub fn object_materials_json() -> serde_json::Value {
    let map = OBJECT_MATERIALS.read().map(|o| o.clone()).unwrap_or_default();
    serde_json::Value::Object(map.into_iter().map(|(k, (m, w))| (k, serde_json::json!([m.name(), w]))).collect())
}

/// Restores what a layout saved.
pub fn load_object_materials(v: &serde_json::Value) {
    for (name, entry) in v.as_object().into_iter().flatten() {
        let material = entry[0].as_str().and_then(|n| Material::ALL.iter().copied().find(|m| m.name() == n));
        if let (Some(m), Some(w)) = (material, entry[1].as_f64()) {
            set_object_material(name, Some((m, w as f32)));
        }
    }
}

pub fn object_material(name: &str) -> Option<(Material, f32)> {
    OBJECT_MATERIALS.read().ok().and_then(|o| o.get(name).copied())
}

/// Is this asset a storage rack, whose bays can be full or empty?
pub fn is_rack(asset: &std::path::Path) -> bool {
    let stem = asset.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    tokens(&stem).iter().any(|t| matches!(t.to_ascii_lowercase().as_str(), "rack" | "rackable" | "shelf" | "shelving"))
}

/// Metal-coated (low-E) glazing, in typical walls: about 24 dB at 6 dB a wall. From 3GPP TR 38.901's
/// 23 + 0.3 f dB for coated glass, against about 3 dB for plain glass. A typical figure, not a
/// product's: the coating is what decides it.
pub const COATED_GLASS_WEIGHT: f32 = 4.0;

/// What a body does to a worn tag's link. `angle` is between the way the wearer
/// faces (the tag on their chest) and the way to the other device. Past 90° the
/// signal has to creep around the torso: returns (extra loss in dB, extra path in m).
///
/// Geometry of the arc on a 15 cm radius torso, with a creeping-wave attenuation
/// that grows as (ka)^⅓. An estimate of the mechanism, not a solve of a body.
pub fn body_shadow(angle: f32, freq_mhz: f32) -> (f32, f32) {
    let radius = 0.15f32;
    let psi = (angle - std::f32::consts::FRAC_PI_2).max(0.0);
    if psi <= 0.0 {
        return (0.0, 0.0);
    }
    let k = std::f32::consts::TAU * freq_mhz * 1e6 / 299_792_458.0;
    let per_radian = 7.6 * (k * radius * 0.5).max(1.0).powf(1.0 / 3.0);
    // Two ways around the body arrive together right behind it, so it levels off.
    let loss = (per_radian * psi).min(35.0);
    (loss, radius * (psi - psi.sin()))
}

/// Material of an asset and how strongly one crossing obstructs, in typical
/// walls. Generated walls, columns and blocks scale with their thickness:
/// concrete loses roughly 1-1.5 dB/cm at UWB frequencies, so 10 cm counts as
/// a "typical" concrete obstruction (2) and a 20 cm wall as 4, which a UWB
/// first path doesn't get through (`max_walls` is 2 by default).
pub fn obstruction_of_asset(asset: &std::path::Path) -> (Material, f32) {
    let m = Material::of_asset(asset);
    let solid = matches!(Kind::of_asset(asset), Some(Kind::Wall | Kind::Column | Kind::Block));
    match structure_thickness(asset).filter(|_| solid) {
        Some(t) => (m, m.weight() * (t / 0.10).max(0.5)),
        None => (m, m.weight()),
    }
}

/// Thinnest horizontal size of a generated structure, from its folder
/// `structures/<kind>/<W>x<H>x<D>/` (metres).
fn structure_thickness(asset: &std::path::Path) -> Option<f32> {
    let dir = asset.parent()?.file_name()?.to_string_lossy().to_string();
    let v: Vec<f32> = dir.split('x').map(|s| s.parse().ok()).collect::<Option<_>>()?;
    (v.len() == 3).then(|| v[0].min(v[2]))
}

/// What lies between two devices.
#[derive(Clone, Copy, Debug, Default)]
pub struct Path {
    pub distance: f32,
    /// Distinct scene objects crossed by the straight line.
    pub walls: u32,
    /// Sum of the crossed objects' material weights, in typical walls (loss).
    pub wall_eq: f32,
    /// The same for the UWB first-path delay (differs once a material is
    /// calibrated with FDTD, see `MaterialCal`).
    pub bias_eq: f32,
    /// Crossed metal / liquid: no UWB first path.
    pub blocker: Option<Material>,
    /// Antenna gains of both ends towards each other, dBi (0 = isotropic).
    pub gain_db: f32,
    /// Uncalibrated weight crossed per material (index `Material as usize`),
    /// for fitting `MaterialCal` to FDTD.
    pub per_material: [f32; 7],
    /// UWB range shift from both antennas' shape in this direction (`BiasMap`) and
    /// from a body the signal has to go around, metres. Positive reads long.
    pub antenna_bias_m: f32,
}

impl Path {
    pub fn los(&self) -> bool {
        self.walls == 0
    }

    /// Adds one crossed object of a typical thickness for its material.
    #[cfg(test)]
    pub fn cross(&mut self, m: Material) {
        self.cross_weighted(m, m.weight());
    }

    /// Adds one crossed object that obstructs like `weight` typical walls.
    pub fn cross_weighted(&mut self, m: Material, weight: f32) {
        let (loss, delay) = MaterialCal::scales(m);
        self.walls += 1;
        self.wall_eq += weight * loss;
        self.bias_eq += weight * delay;
        self.per_material[m as usize] += weight;
        if m.blocks_uwb() && self.blocker.is_none_or(|b| m.weight() > b.weight()) {
            self.blocker = Some(m);
        }
    }

    /// This path as a device on `freq_mhz` sees it: only calibrations made in its band
    /// count. A path crossed with `cross_weighted` alone carries built-ins and what-ifs.
    pub fn in_band(mut self, freq_mhz: f32) -> Self {
        if self.per_material.iter().all(|w| *w == 0.0) {
            return self;
        }
        let band = CalBand::of_mhz(freq_mhz);
        let (mut wall, mut bias) = (0.0, 0.0);
        for (m, w) in Material::ALL.iter().zip(self.per_material) {
            if w > 0.0 {
                let (l, d) = MaterialCal::scales_in(*m, band);
                wall += w * l;
                bias += w * d;
            }
        }
        self.wall_eq = wall;
        self.bias_eq = bias;
        self
    }

    /// A usable first path: within the range the anchor's transmit power
    /// reaches, and not blocked (walls are judged by `max_walls`).
    pub fn uwb_passes(&self, anchor: &Profile) -> bool {
        self.distance <= uwb_path_range_m(anchor, self) && self.blocker.is_none() && self.wall_eq <= anchor.max_walls as f32 + 1e-3
    }

    /// The same for a TDoA blink: the tag sends it, so the tag's power sets the range.
    pub fn uwb_blink_passes(&self, anchor: &Profile, tag: &Profile) -> bool {
        self.distance <= uwb_blink_range_m(anchor, tag, self) && self.blocker.is_none() && self.wall_eq <= anchor.max_walls as f32 + 1e-3
    }
}

fn uwb_drop_within(anchor: &Profile, path: &Path, range: f32) -> f32 {
    let edge = (path.distance / range.max(0.01)).powi(4);
    (anchor.dropout + 0.08 * path.wall_eq + 0.3 * edge).min(0.95)
}

/// Chance a passing UWB link loses a measurement: grows with obstructions and
/// near the edge of range. Shared by the simulation and the planner.
pub fn uwb_drop_probability(anchor: &Profile, path: &Path) -> f32 {
    uwb_drop_within(anchor, path, uwb_path_range_m(anchor, path))
}

/// The same for a TDoA blink, which the tag's power carries.
pub fn uwb_blink_drop_probability(anchor: &Profile, tag: &Profile, path: &Path) -> f32 {
    uwb_drop_within(anchor, path, uwb_blink_range_m(anchor, tag, path))
}

/// 1-sigma two-way ranging noise: both ends' noise, doubling at the edge of
/// the link as the first path sinks toward the noise floor.
pub fn uwb_range_sigma(anchor: &Profile, tag: &Profile, path: &Path) -> f32 {
    let base = (anchor.range_sigma_m.powi(2) + tag.range_sigma_m.powi(2)).sqrt() * std::f32::consts::FRAC_1_SQRT_2;
    base * (1.0 + (path.distance / uwb_path_range_m(anchor, path).max(0.01)).powi(2))
}

/// Share of a link's noise variance that stays with it while nothing moves (the rest is
/// new every reading). An even split is an assumption, with no measurement behind it:
/// the total, which is what the profiles give, stays as it was.
pub const HELD_VARIANCE: f32 = 0.5;
/// How far an end must move before multipath and first-path delay are drawn again (m).
/// A few decimetres: also an assumption.
pub const LINK_HOLD_M: f32 = 0.3;

/// Draws that stay with one link while neither end moves: multipath and the late
/// first path belong to the spot, so a parked tag can't average them away.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinkDraw {
    /// Three standard normals.
    pub n1: f32,
    pub n2: f32,
    pub n3: f32,
    /// An exponential of unit mean.
    pub x: f32,
}

impl LinkDraw {
    pub fn new(rng: &mut Rng) -> Self {
        Self { n1: rng.normal(), n2: rng.normal(), n3: rng.normal(), x: rng.exp(1.0) }
    }
}

/// Noise of this sigma, part of it the held draw `unit`, the rest new every reading.
pub fn held_noise(sigma: f32, unit: f32, rng: &mut Rng) -> f32 {
    sigma * (HELD_VARIANCE.sqrt() * unit + (1.0 - HELD_VARIANCE).sqrt() * rng.normal())
}

/// UWB two-way ranging between `anchor` and a tag at a spot it has never been. `None` = no
/// measurement.
pub fn uwb_range(anchor: &Profile, tag: &Profile, path: Path, rng: &mut Rng) -> Option<f32> {
    let held = LinkDraw::new(rng);
    uwb_range_held(anchor, tag, path, held, rng)
}

/// UWB two-way ranging with what this link keeps while nothing moves.
pub fn uwb_range_held(anchor: &Profile, tag: &Profile, path: Path, held: LinkDraw, rng: &mut Rng) -> Option<f32> {
    if !path.uwb_passes(anchor) {
        return None;
    }
    // Dropouts grow with obstructions and near the edge of range.
    if rng.chance(uwb_drop_probability(anchor, &path)) {
        return None;
    }
    let mut r = path.distance + path.antenna_bias_m + held_noise(uwb_range_sigma(anchor, tag, &path), held.n1, rng);
    if path.walls > 0 {
        // NLOS: the first path is attenuated and slowed by the material and
        // arrives late - barely through cardboard or wood, a lot through concrete.
        r += path.bias_eq * anchor.nlos_bias_m + anchor.nlos_sigma_m * path.bias_eq * held.x;
    }
    Some(r.max(0.0))
}

/// One TWR range as the engine sees it.
#[derive(Clone, Copy, Debug)]
pub struct RangeReading {
    pub range: f32,
    /// First-path power the chip reports (dBm).
    pub fp_dbm: f32,
    /// The sigma the solver weighs it with (m).
    pub sigma: f64,
}

/// How far below the power expected at the measured range a first path gets flagged NLOS (dB).
const FP_FLAG_DB: f32 = 4.0;
/// Noise of the chip's first-path power estimate (dB).
const FP_SIGMA_DB: f32 = 1.5;

/// The sigma the solver gives a range the first-path diagnostic did or didn't flag.
pub fn twr_solver_sigma(anchor: &Profile, flagged: bool) -> f32 {
    let base = anchor.range_sigma_m.max(0.02);
    if flagged { base * 6.0 + anchor.nlos_bias_m } else { base }
}

/// A UWB range and the weight the solver gives it. The chip's first-path diagnostic:
/// a blocked path arrives several dB below the free-space value expected at the
/// measured range, and flagged ranges are down-weighted.
pub fn twr_measure(anchor: &Profile, tag: &Profile, path: Path, held: LinkDraw, robust: bool, rng: &mut Rng) -> Option<RangeReading> {
    let r = uwb_range_held(anchor, tag, path, held, rng)?;
    let fp = uwb_rx_power(anchor, path) + rng.gauss(FP_SIGMA_DB);
    let expected = uwb_rx_power(anchor, Path { distance: r, gain_db: path.gain_db, ..Default::default() });
    let flagged = robust && expected - fp > FP_FLAG_DB;
    Some(RangeReading { range: r, fp_dbm: fp, sigma: twr_solver_sigma(anchor, flagged) as f64 })
}

/// Chance the first-path diagnostic flags a range that reads `excess` metres long over
/// this path: the same test `twr_measure` draws.
pub fn first_path_flag_chance(path: &Path, excess: f32) -> f32 {
    let d = path.distance.max(0.01);
    let margin = UWB_WALL_DB * path.wall_eq - FP_FLAG_DB - 20.0 * ((d + excess).max(0.01) / d).log10();
    normal_cdf(margin / FP_SIGMA_DB)
}

/// UWB first-path power estimate (dBm), for display/diagnostics.
pub fn uwb_rx_power(anchor: &Profile, path: Path) -> f32 {
    uwb_tx_total_dbm(anchor) + path.gain_db - fspl_db(path.distance as f64, anchor.freq_mhz as f64) as f32 - UWB_WALL_DB * path.wall_eq
}

/// The same for a TDoA blink: the tag's power, not the anchor's.
pub fn uwb_blink_rx_power(anchor: &Profile, tag: &Profile, path: Path) -> f32 {
    uwb_rx_power(anchor, path) + tag.uwb_tx_dbm_mhz - anchor.uwb_tx_dbm_mhz
}

/// Loss of one typical wall at UWB frequencies (dB), for power and for range alike.
pub const UWB_WALL_DB: f32 = 6.0;

/// Regulatory UWB limit (FCC / ETSI), dBm/MHz.
pub const UWB_MAX_DBM_MHZ: f32 = -41.3;
/// 10·log10 of the 499.2 MHz UWB channel bandwidth.
const UWB_BW_DB: f32 = 26.98;

/// Total UWB transmit power over the channel, dBm.
pub fn uwb_tx_total_dbm(anchor: &Profile) -> f32 {
    anchor.uwb_tx_dbm_mhz + UWB_BW_DB
}

/// Line-of-sight link range with isotropic antennas: where the first path
/// drops to the receiver sensitivity, capped by the profile's `max_range_m`.
pub fn uwb_link_range_m(anchor: &Profile) -> f32 {
    uwb_link_range_gain_m(anchor, 0.0)
}

/// The same with `gain_db` of antenna gain on the link (both ends): every
/// +6 dB doubles the range.
pub fn uwb_link_range_gain_m(anchor: &Profile, gain_db: f32) -> f32 {
    uwb_range_with(anchor, uwb_tx_total_dbm(anchor), gain_db)
}

fn uwb_range_with(anchor: &Profile, tx_total_dbm: f32, gain_db: f32) -> f32 {
    let budget = tx_total_dbm + gain_db - anchor.sensitivity_dbm;
    let d = 10f32.powf((budget - 20.0 * anchor.freq_mhz.log10() + 27.55) / 20.0);
    d.min(anchor.max_range_m)
}

/// How far this path carries: its antenna gains help, the walls it crosses cost.
pub fn uwb_path_range_m(anchor: &Profile, path: &Path) -> f32 {
    uwb_link_range_gain_m(anchor, path.gain_db - UWB_WALL_DB * path.wall_eq)
}

/// The same for a blink: the tag transmits and the anchor receives.
pub fn uwb_blink_range_m(anchor: &Profile, tag: &Profile, path: &Path) -> f32 {
    uwb_range_with(anchor, tag.uwb_tx_dbm_mhz + UWB_BW_DB, path.gain_db - UWB_WALL_DB * path.wall_eq)
}

/// 1-sigma noise of one TDoA arrival. TWR is r = c(Tround − Treply)/2 over four timestamps,
/// so σ_r = cσ_t: one arrival carries the whole of `range_sigma_m`, plus the clock sync.
pub fn uwb_arrival_sigma(anchor: &Profile) -> f32 {
    ((anchor.sync_sigma_ns * C_M_PER_NS).powi(2) + anchor.range_sigma_m.powi(2)).sqrt()
}

/// UWB TDoA at a spot it has never been: arrival time at an anchor in metres (time * c).
#[cfg(test)]
pub fn uwb_arrival_m(anchor: &Profile, tag: &Profile, path: Path, clock_offset_m: f32, rng: &mut Rng) -> Option<f32> {
    let held = LinkDraw::new(rng);
    uwb_arrival_held(anchor, tag, path, clock_offset_m, held, rng)
}

/// UWB TDoA: arrival time at an anchor in metres (time * c), including the
/// anchor's clock offset and synchronisation jitter.
pub fn uwb_arrival_held(anchor: &Profile, tag: &Profile, path: Path, clock_offset_m: f32, held: LinkDraw, rng: &mut Rng) -> Option<f32> {
    if !path.uwb_blink_passes(anchor, tag) {
        return None;
    }
    if rng.chance(uwb_blink_drop_probability(anchor, tag, &path)) {
        return None;
    }
    let mut t = path.distance + path.antenna_bias_m + clock_offset_m + rng.gauss(anchor.sync_sigma_ns * C_M_PER_NS) + held_noise(anchor.range_sigma_m, held.n1, rng);
    if path.walls > 0 {
        t += path.bias_eq * anchor.nlos_bias_m + anchor.nlos_sigma_m * path.bias_eq * held.x;
    }
    Some(t)
}

/// RSSI 1 m from a transmitter: `rssi_at_1m` is calibrated at 0 dBm, and its power adds on top.
pub fn rssi_1m(transmitter: &Profile) -> f32 {
    transmitter.rssi_at_1m + transmitter.tx_power_dbm
}

/// Expected (mean, no shadowing) BLE RSSI at a node from a transmitting tag, dBm.
pub fn ble_mean_rssi(node: &Profile, tag: &Profile, path: Path) -> f32 {
    rssi_1m(tag) - 10.0 * node.path_loss_exp * path.distance.max(0.1).log10() - node.wall_loss_db * path.wall_eq + path.gain_db
}

/// BLE RSSI seen between a fixed node and a mobile tag.
pub fn ble_rssi(node: &Profile, tag: &Profile, path: Path, held: LinkDraw, rng: &mut Rng) -> Option<f32> {
    if path.distance > node.max_range_m {
        return None;
    }
    let rssi = ble_mean_rssi(node, tag, path) + held_noise(node.shadow_sigma_db, held.n1, rng);
    (rssi >= node.sensitivity_dbm && !rng.chance(node.dropout)).then_some(rssi)
}

/// Chance that a Gaussian with this mean margin and spread stays above zero.
pub fn normal_above(margin: f32, sigma: f32) -> f32 {
    // Logistic approximation of the normal CDF (max error under 1%).
    let x = margin / sigma.max(1e-3);
    1.0 / (1.0 + (-1.702 * x).exp())
}

/// P(Z ≤ x), right in the tails too, where `normal_above` is only right near the middle.
pub fn normal_cdf(x: f32) -> f32 {
    let z = (x as f64 * std::f64::consts::FRAC_1_SQRT_2).abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    // erfc, fractional error under 1.2e-7 (Numerical Recipes 6.2).
    let erfc = t * (-z * z - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277))))))))).exp();
    (if x >= 0.0 { 1.0 - 0.5 * erfc } else { 0.5 * erfc }) as f32
}

/// Chance a BLE/WiFi node hears a tag: above sensitivity through shadowing.
pub fn rssi_heard_probability(node: &Profile, tag: &Profile, path: &Path) -> f32 {
    if path.distance > node.max_range_m {
        return 0.0;
    }
    normal_above(ble_mean_rssi(node, tag, *path) - node.sensitivity_dbm, node.shadow_sigma_db) * (1.0 - node.dropout)
}

/// 1-sigma distance error of log-distance ranging at this distance.
pub fn rssi_distance_sigma(node: &Profile, distance: f32) -> f32 {
    (distance * node.shadow_sigma_db * std::f32::consts::LN_10 / (10.0 * node.path_loss_exp.max(0.5))).max(0.3)
}

/// 1-sigma bearing noise of an AoA locator (radians) over this path. An array's aperture
/// shrinks as cos θ off boresight. Strictly only the axis towards boresight grows; applied to
/// both, slightly pessimistic across it.
pub fn aoa_sigma_rad(node: &Profile, path: &Path, angle_from_boresight: f32) -> f32 {
    (node.aoa_sigma_deg.powi(2) + (node.aoa_nlos_deg * path.wall_eq).powi(2)).sqrt().to_radians() / angle_from_boresight.cos().max(0.17)
}

/// A usable AoA bearing: inside the array's field of view, and no metal or
/// liquid eating the direct path.
pub fn aoa_passes(node: &Profile, path: &Path, angle_from_boresight: f32) -> bool {
    path.blocker.is_none() && angle_from_boresight <= (node.aoa_fov_deg * 0.5).to_radians()
}

/// Bearing an AoA locator measures towards the tag: the true unit direction
/// with noise on the two axes across it. `None` = no measurement.
pub fn ble_aoa_bearing(node: &Profile, tag: &Profile, path: Path, dir: Vec3, angle_from_boresight: f32, held: LinkDraw, rng: &mut Rng) -> Option<Vec3> {
    if !aoa_passes(node, &path, angle_from_boresight) || !rng.chance(rssi_heard_probability(node, tag, &path)) {
        return None;
    }
    let sigma = aoa_sigma_rad(node, &path, angle_from_boresight);
    let side = dir.any_orthonormal_vector();
    let up = dir.cross(side);
    Some((dir + side * held_noise(sigma, held.n1, rng) + up * held_noise(sigma, held.n2, rng)).normalize_or(dir))
}

/// WiFi FTM: heard like RSSI, measured by round-trip time.
pub fn wifi_rtt_range(ap: &Profile, client: &Profile, path: Path, held: LinkDraw, rng: &mut Rng) -> Option<f32> {
    if !rng.chance(rssi_heard_probability(ap, client, &path)) {
        return None;
    }
    let mut r = path.distance + held_noise(ap.range_sigma_m, held.n1, rng);
    if path.walls > 0 {
        r += path.bias_eq * ap.nlos_bias_m + ap.nlos_sigma_m * path.bias_eq * held.x;
    }
    Some(r.max(0.0))
}

/// Antenna gain pattern: a cos^n main lobe sized by the 3 dB beam width,
/// with a -20 dB floor for side/back lobes. `angle` from boresight, radians.
pub fn antenna_gain(peak_dbi: f32, beam_deg: f32, angle: f32) -> f32 {
    if beam_deg >= 359.0 {
        return peak_dbi;
    }
    let half = (beam_deg.to_radians() * 0.5).max(0.05);
    // cos(half)^n = 0.5  ->  n = ln 0.5 / ln cos(half)
    let n = 0.5f32.ln() / half.cos().max(1e-3).ln();
    let c = angle.cos();
    let rel = if c > 0.0 { 10.0 * c.powf(n).max(1e-2).log10() } else { -20.0 };
    peak_dbi + rel.max(-20.0)
}

/// Result of one RFID inventory opportunity between a reader and a tag.
#[derive(Clone, Copy, Debug)]
pub struct RfidLink {
    /// Power reaching the tag chip (dBm).
    #[allow(dead_code)]
    pub forward_dbm: f32,
    /// Backscatter power at the reader (dBm).
    pub rssi_dbm: f32,
    pub phase_deg: f32,
    /// Whether the tag is energised and the reply is above reader sensitivity.
    pub readable: bool,
}

/// 1-sigma fade of an RFID link (dB). It belongs to the spot, not the moment: draw it once
/// per tag and reader and hold it until something moves.
pub const RFID_FADE_SIGMA_DB: f32 = 2.0;
/// 1-sigma of the reply, on top of the fade (dB).
const RFID_REPLY_SIGMA_DB: f32 = 1.0;

/// A fade has moved on after half a wavelength.
pub fn rfid_hold_m(reader: &Profile) -> f32 {
    0.5 * 299.792_46 / reader.freq_mhz
}

/// Passive UHF link budget (forward link usually limits range). `g_r` is the reader antenna's
/// gain towards the tag and `fade_db` this link's fade.
pub fn rfid_link(reader: &Profile, tag: &Profile, path: Path, g_r: f32, fade_db: f32, rng: &mut Rng) -> RfidLink {
    let loss = fspl_db(path.distance as f64, reader.freq_mhz as f64) as f32 + reader.wall_loss_db * path.wall_eq;
    let forward = reader.tx_power_dbm + g_r + tag.antenna_gain_dbi - loss + fade_db;
    // Backscatter: forward power, re-radiated with ~-6 dB modulation loss, back through the same
    // path and so through the same fade again.
    let rssi = forward - 6.0 + tag.antenna_gain_dbi + g_r - loss + fade_db + rng.gauss(RFID_REPLY_SIGMA_DB);
    let lambda = 299.792_46 / reader.freq_mhz;
    let phase = ((4.0 * std::f32::consts::PI * path.distance / lambda).to_degrees() + rng.gauss(3.0)).rem_euclid(360.0);
    let readable = path.distance <= reader.max_range_m && forward >= tag.tag_sensitivity_dbm && rssi >= reader.sensitivity_dbm;
    RfidLink { forward_dbm: forward, rssi_dbm: rssi, phase_deg: phase, readable }
}

/// RFID read margins without fading (dB): the forward link (tag energised)
/// and the backscatter (reader hears it). `g_r` is the reader antenna's gain
/// towards the tag.
pub fn rfid_margins(reader: &Profile, tag: &Profile, path: &Path, g_r: f32) -> (f32, f32) {
    let loss = fspl_db(path.distance as f64, reader.freq_mhz as f64) as f32 + reader.wall_loss_db * path.wall_eq;
    let forward = reader.tx_power_dbm + g_r + tag.antenna_gain_dbi - loss;
    let rssi = forward - 6.0 + tag.antenna_gain_dbi + g_r - loss;
    (forward - tag.tag_sensitivity_dbm, rssi - reader.sensitivity_dbm)
}

/// Chance one inventory round reads the tag, through the fading `rfid_link` is given: over
/// spots, as a tag standing still keeps the fade it has.
pub fn rfid_read_probability(reader: &Profile, tag: &Profile, path: &Path, g_r: f32) -> f32 {
    if path.distance > reader.max_range_m {
        return 0.0;
    }
    let (f, r) = rfid_margins(reader, tag, path, g_r);
    let reply = ((2.0 * RFID_FADE_SIGMA_DB).powi(2) + RFID_REPLY_SIGMA_DB.powi(2)).sqrt();
    normal_above(f, RFID_FADE_SIGMA_DB).min(normal_above(r, reply))
}

/// Expected reads per second for one tag when `n` tags share the field. `read_rate_hz` is the
/// rate after the Gen2 Q algorithm, so slot efficiency is already in it.
pub fn rfid_reads_per_tag(reader: &Profile, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    reader.read_rate_hz / n as f32 / (1.0 + 0.002 * n as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtls::device::{DeviceKind, Profile};

    fn reader() -> Profile {
        Profile {
            kind: DeviceKind::RfidReader,
            tx_power_dbm: 30.0,
            antenna_gain_dbi: 6.0,
            beam_deg: 70.0,
            freq_mhz: 915.0,
            sensitivity_dbm: -84.0,
            max_range_m: 12.0,
            ..Default::default()
        }
    }

    fn tag() -> Profile {
        Profile { kind: DeviceKind::RfidTag, tag_sensitivity_dbm: -18.0, antenna_gain_dbi: 2.0, ..Default::default() }
    }

    #[test]
    fn rfid_reads_near_not_far() {
        let mut rng = Rng::new(1);
        let r = reader();
        let gain = |angle: f32| antenna_gain(r.antenna_gain_dbi, r.beam_deg, angle);
        let near = rfid_link(&r, &tag(), Path { distance: 2.0, ..Default::default() }, gain(0.0), 0.0, &mut rng);
        let far = rfid_link(&r, &tag(), Path { distance: 20.0, ..Default::default() }, gain(0.0), 0.0, &mut rng);
        let behind = rfid_link(&r, &tag(), Path { distance: 2.0, ..Default::default() }, gain(3.0), 0.0, &mut rng);
        assert!(near.readable, "{near:?}");
        assert!(!far.readable);
        assert!(behind.forward_dbm < near.forward_dbm - 15.0);
    }

    #[test]
    fn gain_pattern_half_power_at_beam_edge() {
        let g0 = antenna_gain(6.0, 70.0, 0.0);
        let g35 = antenna_gain(6.0, 70.0, 35f32.to_radians());
        assert!((g0 - g35 - 3.0).abs() < 0.1, "{g0} {g35}");
    }

    #[test]
    fn uwb_nlos_is_biased_long() {
        let a = Profile { dropout: 0.0, max_walls: 5, ..Default::default() };
        let t = Profile { range_sigma_m: 0.1, ..Default::default() };
        let mut rng = Rng::new(2);
        let n = 2000;
        // 8 m: at 10 m a 12 dB obstruction would be at the very edge of its range.
        let mean = |crossed: &[Material], rng: &mut Rng| {
            let mut p = Path { distance: 8.0, ..Default::default() };
            crossed.iter().for_each(|m| p.cross(*m));
            let got: Vec<f32> = (0..n).filter_map(|_| uwb_range(&a, &t, p, rng)).collect();
            got.iter().sum::<f32>() / got.len() as f32
        };
        let los = mean(&[], &mut rng);
        let nlos = mean(&[Material::Generic; 2], &mut rng);
        let wood = mean(&[Material::Wood], &mut rng);
        let concrete = mean(&[Material::Concrete], &mut rng);
        assert!((los - 8.0).abs() < 0.02, "{los}");
        assert!(nlos > 8.8, "{nlos}");
        // Wood slows the first path a little, concrete a lot.
        assert!(wood > los + 0.1 && wood < 8.4, "{wood}");
        assert!(concrete > 8.8, "{concrete}");
    }

    #[test]
    fn uwb_metal_and_liquid_block() {
        let a = Profile { dropout: 0.0, max_walls: 5, ..Default::default() };
        let t = Profile::default();
        let mut rng = Rng::new(3);
        for m in [Material::Metal, Material::Liquid] {
            let mut p = Path { distance: 5.0, ..Default::default() };
            p.cross(m);
            assert!(uwb_range(&a, &t, p, &mut rng).is_none(), "{m:?}");
            assert!(uwb_arrival_m(&a, &t, p, 0.0, &mut rng).is_none(), "{m:?}");
        }
        // Several cardboard boxes in a row still pass.
        let mut p = Path { distance: 5.0, ..Default::default() };
        (0..4).for_each(|_| p.cross(Material::Thin));
        assert!(uwb_range(&a, &t, p, &mut rng).is_some());
    }

    #[test]
    fn material_from_asset_name() {
        let m = |s: &str| Material::of_asset(std::path::Path::new(s));
        assert_eq!(m("x/sm_barrier_fence_metal_a02_01.usd"), Material::Metal);
        assert_eq!(m("x/sm_barrier_wall_plastic_orange_a03_01.usd"), Material::Thin);
        assert_eq!(m("x/sm_box_cardboard_a01_01.usd"), Material::Thin);
        assert_eq!(m("x/sm_pallet_wood_a01_01.usd"), Material::Wood);
        assert_eq!(m("x/sm_rack_storage_a01_01.usd"), Material::Metal);
        assert_eq!(m("x/sm_drum_oil_a01_01.usd"), Material::Liquid);
        assert_eq!(m("x/sm_container_water_a01_01.usd"), Material::Liquid);
        assert_eq!(m("x/sm_engineComponent_piston_a01_01.usd"), Material::Metal);
        assert_eq!(m("x/sm_teapot_a01_01.usd"), Material::Generic);
        assert_eq!(m("C:/SenseSim/structures/wall/4.00x3.00x0.20/wall.usda"), Material::Concrete);
    }

    #[test]
    fn antenna_bias_moves_the_range_and_body_shadow_grows_behind_the_wearer() {
        let (anchor, tag) = (Profile { range_sigma_m: 0.0, dropout: 0.0, ..Default::default() }, Profile { range_sigma_m: 0.0, ..Default::default() });
        let mut rng = Rng::new(3);
        let r = uwb_range(&anchor, &tag, Path { distance: 5.0, antenna_bias_m: 0.04, ..Default::default() }, &mut rng).unwrap();
        assert!((r - 5.04).abs() < 1e-3, "{r}");
        assert_eq!(body_shadow(0.5, 6489.6), (0.0, 0.0), "in front of the wearer");
        let side = body_shadow(1.9, 6489.6);
        let behind = body_shadow(std::f32::consts::PI, 6489.6);
        assert!(side.0 > 0.0 && behind.0 > side.0 && behind.1 > side.1, "{side:?} {behind:?}");
        // a(π/2 − 1) on a 15 cm torso.
        assert!((behind.1 - 0.15 * (std::f32::consts::FRAC_PI_2 - 1.0)).abs() < 1e-4, "{behind:?}");
        // Lower frequencies bend around the body more easily.
        assert!(body_shadow(2.0, 2440.0).0 < body_shadow(2.0, 6489.6).0);
    }

    #[test]
    fn bays_fill_at_the_right_rate_and_stay_put() {
        let hits = |fill: f32| (0..2000).filter(|i| bay_has_goods(7, Vec3::new(*i as f32, 1.0, -3.0), fill)).count();
        assert_eq!(hits(1.0), 2000);
        assert_eq!(hits(0.0), 0);
        let half = hits(0.5);
        assert!((half as i32 - 1000).abs() < 120, "{half}");
        let at = Vec3::new(3.2, 1.1, 8.7);
        assert_eq!(bay_has_goods(9, at, 0.5), bay_has_goods(9, at + Vec3::splat(0.05), 0.5), "same bay, same answer");
    }

    #[test]
    fn a_ray_along_a_half_full_rack_nearly_always_meets_goods() {
        let share = |len: f32| (0..400).filter(|i| crossing_meets_goods(*i as u64, Vec3::ZERO, Vec3::new(len, 0.0, 0.0), 0.5)).count() as f32 / 400.0;
        assert!(share(10.0) > 0.97, "{}", share(10.0));
        let one = share(0.8);
        assert!(one > 0.35 && one < 0.65, "{one}");
        assert!(crossing_meets_goods(1, Vec3::ZERO, Vec3::X, 1.0));
        assert!(!crossing_meets_goods(1, Vec3::ZERO, Vec3::X * 9.0, 0.0));
    }

    #[test]
    fn stated_materials_survive_a_save() {
        set_object_material("radio_test_window", Some((Material::Glass, 0.5)));
        let saved = object_materials_json();
        set_object_material("radio_test_window", None);
        assert!(object_material("radio_test_window").is_none());
        load_object_materials(&saved);
        assert_eq!(object_material("radio_test_window"), Some((Material::Glass, 0.5)));
        set_object_material("radio_test_window", None);
    }

    #[test]
    fn people_in_the_path_scale_with_density_and_length() {
        let none = SiteConditions::default();
        assert_eq!(none.person_in_path(20.0), 0.0);
        let busy = SiteConditions { people_per_100m2: 10.0, ..none };
        assert!(busy.person_in_path(20.0) > busy.person_in_path(5.0));
        assert!((busy.person_in_path(10.0) - (1.0 - (-0.5f32).exp())).abs() < 1e-5);
    }

    #[test]
    fn racks_are_recognised_by_name() {
        assert!(is_rack(std::path::Path::new("sm_rackable_a01.usd")));
        assert!(!is_rack(std::path::Path::new("sm_pallet_a01.usd")));
    }

    #[test]
    fn fdtd_calibration_changes_loss_and_delay() {
        // Wood: no other test relies on its weight (the calibration is process-wide).
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let anchor = Profile::default();
        let rx = || {
            let mut p = Path { distance: 5.0, ..Default::default() };
            p.cross(Material::Wood);
            (uwb_rx_power(&anchor, p), p.bias_eq)
        };
        let (before, bias_before) = rx();
        // FDTD measured 15 dB through a wood object the ray model puts at 6 × 0.4 = 2.4 dB.
        MaterialCal::set(Material::Wood, Some(MaterialCal { loss_scale: 15.0 / 2.4, bias_scale: 2.0, ..Default::default() }));
        let (after, bias_after) = rx();
        MaterialCal::set(Material::Wood, None);
        assert!((before - after - (15.0 - 2.4)).abs() < 0.01, "{before} → {after}");
        assert!((bias_after - 2.0 * bias_before).abs() < 1e-5);
        assert_eq!(rx().0, before, "clearing restores the built-in model");
    }

    #[test]
    fn a_calibration_counts_only_in_the_band_it_was_made_in() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A wall measured at 5 GHz: 8 GHz UWB links and 2.4 GHz nodes don't move.
        let cal = MaterialCal { loss_scale: 0.27, bias_scale: 0.5, source: CalSource { band: CalBand::Ghz5, ..Default::default() }, ..Default::default() };
        MaterialCal::set(Material::Wood, Some(cal));
        let mut p = Path { distance: 8.0, ..Default::default() };
        p.cross_weighted(Material::Wood, 4.0);
        let (at5, at8, at2) = (p.in_band(5180.0), p.in_band(7987.2), p.in_band(2440.0));
        MaterialCal::clear_in(Material::Wood, CalBand::Ghz5);
        assert!((at5.wall_eq - 4.0 * 0.27).abs() < 1e-5 && (at5.bias_eq - 2.0).abs() < 1e-5, "{at5:?}");
        assert_eq!((at8.wall_eq, at8.bias_eq), (4.0, 4.0), "another band keeps the built-in model");
        assert_eq!((at2.wall_eq, at2.bias_eq), (4.0, 4.0));
        // The audit's case: a 20 cm wall must not stop blocking UWB because of another band.
        let anchor = Profile { freq_mhz: 7987.2, max_range_m: 100.0, ..Default::default() };
        assert!(!at8.uwb_passes(&anchor));
    }

    #[test]
    fn a_what_if_covers_every_band_and_goes_back() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let band = MaterialCal { loss_scale: 2.0, bias_scale: 3.0, source: CalSource { band: CalBand::Ghz8, ..Default::default() }, ..Default::default() };
        MaterialCal::set(Material::Glass, Some(band));
        // As the stress test scales: on top of what's in force.
        MaterialCal::set(Material::Glass, Some(MaterialCal { loss_scale: 1.5, bias_scale: 1.5, source: CalSource { relative: true, ..Default::default() }, ..Default::default() }));
        assert_eq!(MaterialCal::scales_in(Material::Glass, CalBand::Ghz8), (3.0, 4.5));
        assert_eq!(MaterialCal::scales_in(Material::Glass, CalBand::Uhf), (1.5, 1.5));
        // As the material ranges do: says what the material costs, whatever was there.
        MaterialCal::set(Material::Glass, Some(MaterialCal { loss_scale: 0.7, bias_scale: 1.0, ..Default::default() }));
        assert_eq!(MaterialCal::scales_in(Material::Glass, CalBand::Ghz8), (0.7, 1.0));
        MaterialCal::set(Material::Glass, None);
        assert_eq!(MaterialCal::scales_in(Material::Glass, CalBand::Ghz8), (2.0, 3.0), "the measurement outlives the what-if");
        assert_eq!(MaterialCal::scales(Material::Glass), (1.0, 1.0));
        MaterialCal::clear_in(Material::Glass, CalBand::Ghz8);
    }

    #[test]
    fn calibrations_park_and_come_back() {
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        SWAPS_HERE.set(true);
        let cal = MaterialCal { loss_scale: 2.0, source: CalSource { band: CalBand::Ghz8, ..Default::default() }, ..Default::default() };
        MaterialCal::set(Material::Liquid, Some(cal));
        let mut parked = ParkedCals::default();
        MaterialCal::swap_parked(&mut parked);
        assert_eq!(MaterialCal::scales_in(Material::Liquid, CalBand::Ghz8), (1.0, 1.0));
        MaterialCal::swap_parked(&mut parked);
        assert_eq!(MaterialCal::scales_in(Material::Liquid, CalBand::Ghz8), (2.0, 1.0));
        MaterialCal::clear_bands();
        assert!(MaterialCal::applied_in(Material::Liquid, CalBand::Ghz8).is_none());
    }

    #[test]
    fn antenna_gain_on_the_path_extends_links() {
        let anchor = Profile::default();
        let r0 = uwb_link_range_gain_m(&Profile { max_range_m: 500.0, ..anchor.clone() }, 0.0);
        let r6 = uwb_link_range_gain_m(&Profile { max_range_m: 500.0, ..anchor.clone() }, 6.0);
        assert!((r6 / r0 - 2.0).abs() < 0.02, "+6 dB should double the range: {r0} → {r6}");
        // Just past the isotropic range: fails without gain, passes with it.
        let low = Profile { uwb_tx_dbm_mhz: -55.0, ..anchor };
        let d = uwb_link_range_m(&low) + 1.0;
        assert!(!Path { distance: d, ..Default::default() }.uwb_passes(&low));
        assert!(Path { distance: d, gain_db: 6.0, ..Default::default() }.uwb_passes(&low));
        // BLE: the gain adds straight to the RSSI.
        let (node, tag) = (Profile::default(), Profile::default());
        let p = Path { distance: 5.0, ..Default::default() };
        assert_eq!(ble_mean_rssi(&node, &tag, Path { gain_db: 4.0, ..p }) - ble_mean_rssi(&node, &tag, p), 4.0);
    }

    #[test]
    fn tdoa_arrival_noise_is_one_timestamp_not_half() {
        // TWR is r = c(Tround − Treply)/2 over 4 timestamps, so σ_r = cσ_t: one arrival has σ_r.
        let a = Profile { range_sigma_m: 0.08, sync_sigma_ns: 0.0, dropout: 0.0, ..Default::default() };
        let t = Profile::default();
        let mut rng = Rng::new(5);
        let p = Path { distance: 5.0, ..Default::default() };
        let n = 4000;
        let got: Vec<f32> = (0..n).filter_map(|_| uwb_arrival_m(&a, &t, p, 0.0, &mut rng)).collect();
        let mean = got.iter().sum::<f32>() / got.len() as f32;
        let sd = (got.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / got.len() as f32).sqrt();
        assert!((sd - 0.08).abs() < 0.006, "{sd}");
    }

    #[test]
    fn aoa_noise_grows_off_boresight() {
        let node = Profile { aoa_sigma_deg: 3.0, ..Default::default() };
        let p = Path::default();
        let ratio = aoa_sigma_rad(&node, &p, 70f32.to_radians()) / aoa_sigma_rad(&node, &p, 0.0);
        assert!((ratio - 2.92).abs() < 0.03, "{ratio}");
    }

    #[test]
    fn uwb_range_counts_the_wall_it_goes_through() {
        // 12 dB of 10 cm concrete out of a 32 dB budget past the first metre: about 10 m, not 40.
        let a = Profile::default();
        let through = |d: f32| {
            let mut p = Path { distance: d, ..Default::default() };
            p.cross(Material::Concrete);
            p.uwb_passes(&a)
        };
        assert!(through(9.0));
        assert!(!through(15.0));
        assert!(Path { distance: 15.0, ..Default::default() }.uwb_passes(&a));
    }

    #[test]
    fn a_ble_transmitters_power_moves_the_rssi() {
        let node = Profile::default();
        let p = Path { distance: 5.0, ..Default::default() };
        let low = Profile { tx_power_dbm: 0.0, ..Default::default() };
        let high = Profile { tx_power_dbm: 8.0, ..Default::default() };
        assert!((ble_mean_rssi(&node, &high, p) - ble_mean_rssi(&node, &low, p) - 8.0).abs() < 1e-4);
    }

    #[test]
    fn a_backscatter_goes_through_the_fade_twice() {
        let (r, t) = (reader(), tag());
        let p = Path { distance: 4.0, ..Default::default() };
        let mean = |fade: f32| {
            let mut rng = Rng::new(8);
            let v: Vec<RfidLink> = (0..4000).map(|_| rfid_link(&r, &t, p, 6.0, fade, &mut rng)).collect();
            (v[0].forward_dbm, v.iter().map(|l| l.rssi_dbm).sum::<f32>() / v.len() as f32)
        };
        let (f0, r0) = mean(0.0);
        let (f3, r3) = mean(3.0);
        assert!((f3 - f0 - 3.0).abs() < 1e-3, "{f0} {f3}");
        assert!((r3 - r0 - 6.0).abs() < 1e-3, "{r0} {r3}");
    }

    #[test]
    fn the_flag_chance_matches_the_diagnostics_draws() {
        // What the planner assumes about the first-path check is what the simulation draws.
        let a = Profile { dropout: 0.0, max_walls: 5, ..Default::default() };
        let t = Profile::default();
        let mut rng = Rng::new(11);
        // Weights written out: the calibrations `cross` reads are process-wide, and tests change them.
        for (w, d) in [(1.0, 12.0), (0.4, 12.0), (2.0, 8.0)] {
            let p = Path { distance: d, walls: 1, wall_eq: w, bias_eq: w, ..Default::default() };
            let flagged_sigma = twr_solver_sigma(&a, true) as f64;
            let n = 8000;
            let got = (0..n).filter(|_| {
                let held = LinkDraw::new(&mut rng);
                twr_measure(&a, &t, p, held, true, &mut rng).is_some_and(|r| r.sigma == flagged_sigma)
            }).count() as f32 / n as f32;
            let want = (0..2000).map(|k| {
                let x = -(1.0 - (k as f32 + 0.5) / 2000.0).ln();
                first_path_flag_chance(&p, p.bias_eq * a.nlos_bias_m + a.nlos_sigma_m * p.bias_eq * x)
            }).sum::<f32>() / 2000.0 * (1.0 - uwb_drop_probability(&a, &p));
            assert!((got - want).abs() < 0.03, "{w} walls: drew {got}, expected {want}");
        }
    }

    #[test]
    fn the_normal_tail_is_right_where_the_logistic_is_not() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-5);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!((normal_cdf(-3.0) - 0.00135).abs() < 1e-4);
    }

    #[test]
    fn rfid_rate_has_no_phantom_aloha_factor() {
        let r = reader();
        assert_eq!(rfid_reads_per_tag(&r, 1), r.read_rate_hz / 1.002);
    }

    #[test]
    fn uwb_tx_power_sets_link_range() {
        let full = Profile::default();
        // −41.3 dBm/MHz against −95 dBm at 6.5 GHz: about 40 m.
        assert!((uwb_link_range_m(&full) - 40.0).abs() < 1.5, "{}", uwb_link_range_m(&full));
        // 12 dB less power: a quarter of the range.
        let low = Profile { uwb_tx_dbm_mhz: -53.3, ..Profile::default() };
        let r = uwb_link_range_m(&low);
        assert!((r - uwb_link_range_m(&full) / 4.0).abs() < 1.0, "{r}");
        let at = |d: f32| Path { distance: d, ..Default::default() };
        assert!(at(r - 0.5).uwb_passes(&low) && !at(r + 0.5).uwb_passes(&low));
    }

    #[test]
    fn a_glass_door_is_glass_however_it_got_there() {
        let m = |s: &str| Material::of_asset(std::path::Path::new(s));
        assert_eq!(m("C:/SenseSim/custom_assets/glass_door_1800x2100/usd/glass_door_1800x2100.usda"), Material::Glass);
        // Its frame doesn't make it a metal door.
        assert_eq!(m("x/sm_door_aluminium_glass_a01_01.usd"), Material::Glass);
        assert_eq!(m("x/sm_container_jar_glass_silver_a01_01.usd"), Material::Glass);
        // Made see-through, whatever it was: a wall, or a rack from the library.
        assert_eq!(m("C:/SenseSim/structures/wall/4.00x3.00x0.02_1a2b3c4d_glass/wall.usda"), Material::Glass);
        assert_eq!(m("C:/SenseSim/reskins/sm_rack_storage_a01_01_1a2b3c4d_glass/sm_rack_storage_a01_01.usda"), Material::Glass);
        // A glass look on a thick structure doesn't charge it for concrete's thickness.
        let glass_wall = std::path::Path::new("C:/SenseSim/structures/wall/4.00x3.00x0.30_1a2b3c4d_glass/wall.usda");
        assert_eq!(obstruction_of_asset(glass_wall), (Material::Glass, Material::Glass.weight()));
        // Paint isn't glass.
        assert_eq!(m("C:/SenseSim/structures/wall/4.00x3.00x0.20_1a2b3c4d/wall.usda"), Material::Concrete);
        // Plain and coated glass, as the knowledge base has them: about 3 and 24 dB at 6 dB a wall.
        assert!((Material::Glass.weight() * 6.0 - 3.0).abs() < 1e-6);
        assert!((COATED_GLASS_WEIGHT * 6.0 - 24.0).abs() < 1e-6);
    }

    #[test]
    fn thick_walls_block_uwb_thin_ones_pass() {
        let anchor = Profile::default();
        let through = |asset: &str| {
            let (m, w) = obstruction_of_asset(std::path::Path::new(asset));
            let mut p = Path { distance: 8.0, ..Default::default() };
            p.cross_weighted(m, w);
            p.uwb_passes(&anchor)
        };
        // 20 cm concrete wall: no UWB first path.
        assert!(!through("C:/SenseSim/structures/wall/10.00x3.00x0.20/wall.usda"));
        // 10 cm partition: passes (late, NLOS).
        assert!(through("C:/SenseSim/structures/wall/10.00x3.00x0.10/wall.usda"));
        // Orientation doesn't matter: thickness is the thinner horizontal size.
        assert!(!through("C:/SenseSim/structures/wall/0.25x3.00x6.00/wall.usda"));
        // Ordinary assets keep their material weight.
        assert_eq!(obstruction_of_asset(std::path::Path::new("x/sm_box_cardboard_a01_01.usd")).1, Material::Thin.weight());
    }
}
