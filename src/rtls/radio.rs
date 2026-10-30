//! Radio channel models. Pure functions of geometry (distance, the objects
//! crossed by the scene ray cast and what they are made of, antenna angle)
//! plus a seeded RNG.

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
        if let Some(k) = Kind::of_asset(asset) {
            return if k == Kind::Beam { Self::Metal } else { Self::Concrete };
        }
        let stem = asset.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
        Self::of_tokens(&tokens(&stem))
    }

    fn of_tokens(toks: &[String]) -> Self {
        const MATERIALS: &[(Material, &[&str])] = &[
            (Material::Liquid, &["water", "oil", "coolant", "lubricant", "lube", "fluid", "antifreeze", "solvent", "fuel", "diesel", "bleach", "disinfectant", "blood", "liquid", "beverage", "wine"]),
            (Material::Metal, &["metal", "steel", "aluminium", "aluminum", "iron", "tin", "chrome", "silver", "gold"]),
            (Material::Glass, &["glass", "window", "windscreen", "windshield", "mirror"]),
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

/// A material measured with FDTD ("through an object", "Use in the floor
/// map"): scales the ray model's built-in loss and UWB delay for it.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialCal {
    pub loss_scale: f32,
    pub bias_scale: f32,
    /// e.g. "FDTD · wall_3 · UWB ch5".
    pub label: String,
    /// Result folder, to tell which FDTD run is applied.
    pub source: std::path::PathBuf,
}

impl Default for MaterialCal {
    fn default() -> Self {
        Self { loss_scale: 1.0, bias_scale: 1.0, label: String::new(), source: Default::default() }
    }
}

/// Process-wide because every ray cast (`Path::cross_weighted`) reads it,
/// deep inside code that has no access to the ECS world.
static MATERIAL_CAL: std::sync::RwLock<[Option<MaterialCal>; 7]> = std::sync::RwLock::new([const { None }; 7]);

impl MaterialCal {
    /// (loss, delay) scale for `m`; 1 = the built-in weights. Called per ray crossing.
    pub fn scales(m: Material) -> (f32, f32) {
        MATERIAL_CAL.read().ok().and_then(|c| c[m as usize].as_ref().map(|c| (c.loss_scale, c.bias_scale))).unwrap_or((1.0, 1.0))
    }

    pub fn applied(m: Material) -> Option<MaterialCal> {
        MATERIAL_CAL.read().ok().and_then(|c| c[m as usize].clone())
    }

    /// Sets (or with `None` clears) the calibration for `m`.
    pub fn set(m: Material, cal: Option<MaterialCal>) {
        if let Ok(mut c) = MATERIAL_CAL.write() {
            c[m as usize] = cal;
        }
    }
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

    /// A usable first path: within the range the anchor's transmit power
    /// reaches, and not blocked (walls are judged by `max_walls`).
    pub fn uwb_passes(&self, anchor: &Profile) -> bool {
        self.distance <= uwb_link_range_gain_m(anchor, self.gain_db) && self.blocker.is_none() && self.wall_eq <= anchor.max_walls as f32 + 1e-3
    }
}

/// UWB two-way ranging between `anchor` and a tag. `None` = no measurement.
pub fn uwb_range(anchor: &Profile, tag: &Profile, path: Path, rng: &mut Rng) -> Option<f32> {
    if !path.uwb_passes(anchor) {
        return None;
    }
    // Dropouts grow with obstructions and near the edge of range.
    let edge = (path.distance / uwb_link_range_gain_m(anchor, path.gain_db)).powi(4);
    let p_drop = (anchor.dropout + 0.08 * path.wall_eq + 0.3 * edge).min(0.95);
    if rng.chance(p_drop) {
        return None;
    }
    let sigma = (anchor.range_sigma_m.powi(2) + tag.range_sigma_m.powi(2)).sqrt() * 0.7071;
    let mut r = path.distance + rng.gauss(sigma);
    if path.walls > 0 {
        // NLOS: the first path is attenuated and slowed by the material and
        // arrives late - barely through cardboard or wood, a lot through concrete.
        r += path.bias_eq * anchor.nlos_bias_m + rng.exp(anchor.nlos_sigma_m * path.bias_eq);
    }
    Some(r.max(0.0))
}

/// UWB first-path power estimate (dBm), for display/diagnostics.
pub fn uwb_rx_power(anchor: &Profile, path: Path) -> f32 {
    uwb_tx_total_dbm(anchor) + path.gain_db - fspl_db(path.distance as f64, anchor.freq_mhz as f64) as f32 - 6.0 * path.wall_eq
}

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
    let budget = uwb_tx_total_dbm(anchor) + gain_db - anchor.sensitivity_dbm;
    let d = 10f32.powf((budget - 20.0 * anchor.freq_mhz.log10() + 27.55) / 20.0);
    d.min(anchor.max_range_m)
}

/// UWB TDoA: arrival time at an anchor in metres (time * c), including the
/// anchor's clock offset and synchronisation jitter.
pub fn uwb_arrival_m(anchor: &Profile, path: Path, clock_offset_m: f32, rng: &mut Rng) -> Option<f32> {
    if !path.uwb_passes(anchor) {
        return None;
    }
    let edge = (path.distance / uwb_link_range_gain_m(anchor, path.gain_db)).powi(4);
    if rng.chance((anchor.dropout + 0.08 * path.wall_eq + 0.3 * edge).min(0.95)) {
        return None;
    }
    let mut t = path.distance + clock_offset_m + rng.gauss(anchor.sync_sigma_ns * C_M_PER_NS) + rng.gauss(anchor.range_sigma_m * 0.5);
    if path.walls > 0 {
        t += path.bias_eq * anchor.nlos_bias_m + rng.exp(anchor.nlos_sigma_m * path.bias_eq);
    }
    Some(t)
}

/// Expected (mean, no shadowing) BLE RSSI between a node and a tag, dBm.
pub fn ble_mean_rssi(node: &Profile, tag: &Profile, path: Path) -> f32 {
    tag.rssi_at_1m - 10.0 * node.path_loss_exp * path.distance.max(0.1).log10() - node.wall_loss_db * path.wall_eq + path.gain_db
}

/// BLE RSSI seen between a fixed node and a mobile tag.
pub fn ble_rssi(node: &Profile, tag: &Profile, path: Path, rng: &mut Rng) -> Option<f32> {
    if path.distance > node.max_range_m {
        return None;
    }
    // `rssi_at_1m` is the calibrated power of the transmitting side (the tag).
    let rssi = tag.rssi_at_1m
        - 10.0 * node.path_loss_exp * path.distance.max(0.1).log10()
        - node.wall_loss_db * path.wall_eq
        + path.gain_db
        + rng.gauss(node.shadow_sigma_db);
    (rssi >= node.sensitivity_dbm && !rng.chance(node.dropout)).then_some(rssi)
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

/// Passive UHF link budget (forward link usually limits range).
pub fn rfid_link(reader: &Profile, tag: &Profile, path: Path, angle: f32, rng: &mut Rng) -> RfidLink {
    let g_r = antenna_gain(reader.antenna_gain_dbi, reader.beam_deg, angle);
    let loss = fspl_db(path.distance as f64, reader.freq_mhz as f64) as f32 + reader.wall_loss_db * path.wall_eq;
    let fading = rng.gauss(2.0);
    let forward = reader.tx_power_dbm + g_r + tag.antenna_gain_dbi - loss + fading;
    // Backscatter: forward power, re-radiated with ~-6 dB modulation loss, back through the same path.
    let rssi = forward - 6.0 + tag.antenna_gain_dbi + g_r - loss + rng.gauss(1.0);
    let lambda = 299.792_46 / reader.freq_mhz;
    let phase = ((4.0 * std::f32::consts::PI * path.distance / lambda).to_degrees() + rng.gauss(3.0)).rem_euclid(360.0);
    let readable = path.distance <= reader.max_range_m && forward >= tag.tag_sensitivity_dbm && rssi >= reader.sensitivity_dbm;
    RfidLink { forward_dbm: forward, rssi_dbm: rssi, phase_deg: phase, readable }
}

/// Expected reads per second for one tag when `n` tags share the field:
/// slotted ALOHA (Gen2 Q algorithm) peaks near 1/e slot efficiency.
pub fn rfid_reads_per_tag(reader: &Profile, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    reader.read_rate_hz * 0.368 * 2.72 / n as f32 / (1.0 + 0.002 * n as f32)
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
        let near = rfid_link(&reader(), &tag(), Path { distance: 2.0, ..Default::default() }, 0.0, &mut rng);
        let far = rfid_link(&reader(), &tag(), Path { distance: 20.0, ..Default::default() }, 0.0, &mut rng);
        let behind = rfid_link(&reader(), &tag(), Path { distance: 2.0, ..Default::default() }, 3.0, &mut rng);
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
        let mean = |crossed: &[Material], rng: &mut Rng| {
            let mut p = Path { distance: 10.0, ..Default::default() };
            crossed.iter().for_each(|m| p.cross(*m));
            let got: Vec<f32> = (0..n).filter_map(|_| uwb_range(&a, &t, p, rng)).collect();
            got.iter().sum::<f32>() / got.len() as f32
        };
        let los = mean(&[], &mut rng);
        let nlos = mean(&[Material::Generic; 2], &mut rng);
        let wood = mean(&[Material::Wood], &mut rng);
        let concrete = mean(&[Material::Concrete], &mut rng);
        assert!((los - 10.0).abs() < 0.02, "{los}");
        assert!(nlos > 10.8, "{nlos}");
        // Wood slows the first path a little, concrete a lot.
        assert!(wood > los + 0.1 && wood < 10.4, "{wood}");
        assert!(concrete > 10.8, "{concrete}");
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
            assert!(uwb_arrival_m(&a, p, 0.0, &mut rng).is_none(), "{m:?}");
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
    fn fdtd_calibration_changes_loss_and_delay() {
        // Wood: no other test relies on its weight (the calibration is process-wide).
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
