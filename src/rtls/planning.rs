//! Deployment planning: coverage / DOP / predicted-error heatmap over the
//! floor, RFID read zones, and the placement optimiser (the search itself is in
//! `placement`). All of it ray casts against the real scene geometry.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::antenna::{AntennaChoice, AntennaLibrary, Antennas};
use super::device::{Device, DeviceKind, Profile, Profiles, UwbMode};
use super::placement::{self, Acc, Candidate, Column, Entry, Method, Mount, Mover, Planner, Point, RadioScene, SAME_SPOT_M};
use super::radio::{self, Path};
use super::sim::{OccluderParts, Occluders};
use super::solver::{self, Dim};
use crate::scene::PlacedObject;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Tech {
    #[default]
    Uwb,
    Ble,
    Rfid,
    Wifi,
}

impl Tech {
    pub const ALL: [Tech; 4] = [Tech::Uwb, Tech::Ble, Tech::Rfid, Tech::Wifi];
    pub fn label(self) -> &'static str {
        match self {
            Tech::Uwb => "UWB",
            Tech::Ble => "BLE",
            Tech::Rfid => "RFID",
            Tech::Wifi => "WiFi",
        }
    }
    pub fn infra(self) -> DeviceKind {
        match self {
            Tech::Uwb => DeviceKind::UwbAnchor,
            Tech::Ble => DeviceKind::BleNode,
            Tech::Rfid => DeviceKind::RfidReader,
            Tech::Wifi => DeviceKind::WifiAp,
        }
    }
    pub fn parse(s: &str) -> Option<Tech> {
        match s.to_ascii_lowercase().as_str() {
            "uwb" => Some(Tech::Uwb),
            "ble" => Some(Tech::Ble),
            "rfid" => Some(Tech::Rfid),
            "wifi" => Some(Tech::Wifi),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Cell {
    /// Infrastructure in line of sight and range.
    pub los: u32,
    /// In range including obstructed links.
    pub heard: u32,
    pub dop: f32,
    /// Predicted horizontal RMS error (m); NaN = no fix. RFID has no position: there it is
    /// the best link margin (dB) of a read chance the optimiser accepts, for colouring.
    pub err: f32,
    /// Strongest received signal (dBm): UWB first path, BLE RSSI, RFID power at the tag.
    pub signal: f32,
    /// Heard, but by too few for a position: how far "near the strongest node" is off (m).
    /// NaN elsewhere.
    pub proximity_m: f32,
}

/// What `Cell::err` once held where a radio is heard but there are too few for a position.
/// Those cells now have no `err`; see `Cell::proximity_m`.
pub const PROXIMITY_ERR_M: f32 = 4.0;

/// When a reading counts as correct for the Quality view: the position lands
/// within `target_m` of the truth, and a spot passes when that happens for at
/// least `min_share` of the readings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QualitySpec {
    pub target_m: f32,
    pub min_share: f32,
}

impl Default for QualitySpec {
    fn default() -> Self {
        Self { target_m: 0.3, min_share: 0.75 }
    }
}

/// Share of position readings within `target` of the truth, for a predicted
/// horizontal RMS error `err` (circular normal error: P(r ≤ R) = 1 − e^(−R²/err²)).
/// 0 where there is no fix.
pub fn fix_share(err: f32, target: f32) -> f32 {
    if !err.is_finite() {
        return 0.0;
    }
    if err <= 0.0 {
        return 1.0;
    }
    1.0 - (-(target / err).powi(2)).exp()
}

#[derive(Clone, Debug)]
pub struct CoverageRequest {
    pub tech: Tech,
    pub height: f32,
    pub cell: f32,
}

#[derive(Resource, Default)]
pub struct Coverage {
    pub tech: Tech,
    pub cell: f32,
    pub origin: Vec2,
    pub nx: usize,
    pub nz: usize,
    pub cells: Vec<Cell>,
    pub visible: bool,
    pub view: MapView,
    pub dirty: bool,
    pub summary: String,
    pub suggestions: Vec<Vec3>,
    pub suggestion_facing: Vec<Vec3>,
    pub suggestion_note: String,
    /// Pass criterion of the Quality view.
    pub quality: QualitySpec,
    /// Tag height of the last scan (m).
    pub height: f32,
    /// Where the propagation loss comes from, and what that means for this map.
    pub source: MapSource,
    pub source_note: String,
    entity: Option<Entity>,
}

impl Coverage {
    /// The floor map's entity, if it has one.
    pub fn heatmap(&self) -> Option<Entity> {
        self.entity
    }
}

/// Propagation model behind the floor map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapSource {
    /// Straight-line ray casts with material weights (fast, live).
    #[default]
    Ray,
    /// The latest whole-scene FDTD run of the technology.
    Fdtd,
}

/// An antenna simulated with FDTD (openEMS), used by the floor map for one
/// technology instead of the profiles' ideal antennas.
#[derive(Resource, Default)]
pub struct AppliedAntenna(pub Option<FdtdAntenna>);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FdtdAntenna {
    pub tech: Tech,
    /// e.g. "Dipole · UWB ch5 · 6.5 GHz".
    pub label: String,
    /// Result folder, to tell which FDTD run is applied.
    pub source: std::path::PathBuf,
    /// Gain `d[θ][φ]` in dBi on a regular grid: directivity with its losses taken off.
    pub theta_step: f32,
    pub phi_step: f32,
    pub d: Vec<Vec<f32>>,
    /// Mismatch loss at the band centre, 10·log10(1 − |S11|²), dB (≤ 0).
    pub mismatch_db: f32,
    /// Patch: the antenna axis (θ = 0) is the device's facing direction.
    /// Wire antennas (dipole, monopole) stand upright: the axis is world up.
    pub axis_forward: bool,
    /// UWB range shift per direction (m), on the same grid as `d`, once analysed.
    /// The average has been removed: a normal antenna-delay calibration takes it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bias_m: Option<Vec<Vec<f32>>>,
}

impl FdtdAntenna {
    /// Grid position (θ, φ) of `dir` for a device facing `fwd`.
    fn angles(&self, fwd: Vec3, dir: Vec3) -> (f32, f32) {
        let z = if self.axis_forward { fwd.normalize_or(Vec3::NEG_Z) } else { Vec3::Y };
        let x = if self.axis_forward { Vec3::Y.cross(z).normalize_or(Vec3::X) } else { Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::X) };
        let y = z.cross(x);
        let d = dir.normalize_or_zero();
        (d.dot(z).clamp(-1.0, 1.0).acos().to_degrees(), d.dot(y).atan2(d.dot(x)).to_degrees().rem_euclid(360.0))
    }

    /// Realised gain (dBi, mismatch included) from a device facing `fwd`
    /// towards `dir`.
    pub fn gain(&self, fwd: Vec3, dir: Vec3) -> f32 {
        let (theta, phi) = self.angles(fwd, dir);
        let Some(row) = self.d.get(((theta / self.theta_step).round() as usize).min(self.d.len().saturating_sub(1))) else { return self.mismatch_db };
        let v = row.get(((phi / self.phi_step).round() as usize).min(row.len().saturating_sub(1))).copied().unwrap_or(0.0);
        v.max(-30.0) + self.mismatch_db
    }

    /// Range shift (m) of a UWB link towards `dir`; 0 until analysed.
    pub fn bias_m(&self, fwd: Vec3, dir: Vec3) -> f32 {
        let Some(grid) = &self.bias_m else { return 0.0 };
        let (theta, phi) = self.angles(fwd, dir);
        let Some(row) = grid.get(((theta / self.theta_step).round() as usize).min(grid.len().saturating_sub(1))) else { return 0.0 };
        row.get(((phi / self.phi_step).round() as usize).min(row.len().saturating_sub(1))).copied().filter(|v| v.is_finite()).unwrap_or(0.0)
    }
}

impl Coverage {
    /// Share of the whole floor that meets the quality criterion.
    pub fn quality_share(&self) -> f32 {
        if self.cells.is_empty() {
            return 0.0;
        }
        let q = self.quality;
        self.cells.iter().filter(|c| fix_share(c.err, q.target_m) >= q.min_share).count() as f32 / self.cells.len() as f32
    }

    /// Share of the floor with a fix, mean / p95 predicted error.
    pub fn stats(&self) -> (f32, f32, f32) {
        if self.cells.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut errs: Vec<f32> = self.cells.iter().filter(|c| c.err.is_finite()).map(|c| c.err).collect();
        let covered = errs.len() as f32 / self.cells.len() as f32;
        if errs.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        errs.sort_by(f32::total_cmp);
        let mean = errs.iter().sum::<f32>() / errs.len() as f32;
        (covered, mean, errs[((errs.len() - 1) as f32 * 0.95).ceil() as usize])
    }

    /// Share of the floor heard but without a position, and the worst that "near the strongest
    /// node" is off there (m).
    pub fn proximity_only(&self) -> (f32, f32) {
        let far: Vec<f32> = self.cells.iter().map(|c| c.proximity_m).filter(|d| d.is_finite()).collect();
        (far.len() as f32 / self.cells.len().max(1) as f32, far.iter().copied().fold(0.0, f32::max))
    }
}

/// Floor area to analyse: scene objects + devices, padded.
pub fn area(objects: &Query<(&PlacedObject, &GlobalTransform)>, devices: &Query<(Entity, &Device, &Transform)>) -> (Vec2, Vec2) {
    // A building floor bounds the area: nothing outside its walls is tracked.
    let mut flo = Vec2::splat(f32::MAX);
    let mut fhi = Vec2::splat(f32::MIN);
    for (o, gt) in objects {
        if crate::agent::structures::Kind::of_asset(&o.asset) == Some(crate::agent::structures::Kind::Floor) {
            let (a, b) = o.aabb();
            for x in [a.x, b.x] {
                for z in [a.z, b.z] {
                    let p = gt.transform_point(Vec3::new(x, a.y, z));
                    flo = flo.min(p.xz());
                    fhi = fhi.max(p.xz());
                }
            }
        }
    }
    if flo.x < fhi.x {
        return (flo, fhi);
    }
    let mut lo = Vec2::splat(f32::MAX);
    let mut hi = Vec2::splat(f32::MIN);
    for (o, gt) in objects {
        let (a, b) = o.aabb();
        for x in [a.x, b.x] {
            for z in [a.z, b.z] {
                let p = gt.transform_point(Vec3::new(x, a.y, z));
                lo = lo.min(p.xz());
                hi = hi.max(p.xz());
            }
        }
    }
    for (_, _, t) in devices {
        lo = lo.min(t.translation.xz());
        hi = hi.max(t.translation.xz());
    }
    if lo.x > hi.x {
        return (Vec2::splat(-10.0), Vec2::splat(10.0));
    }
    (lo - Vec2::splat(1.0), hi + Vec2::splat(1.0))
}

struct Node {
    entity: Entity,
    pos: Vec3,
    fwd: Vec3,
    prof: Profile,
    antenna: Option<AntennaChoice>,
}

/// The tag a plan or map is made for. The tags in the scene decide how UWB is measured:
/// TDoA if all of them blink.
fn planned_tag<'a>(profiles: &Profiles, tech: Tech, devices: impl Iterator<Item = &'a Device>) -> Profile {
    let default = tag_profile(profiles, tech);
    let tags: Vec<Profile> = devices.filter(|d| d.enabled && d.kind == default.kind).map(|d| profiles.for_device(d)).collect();
    if !tags.is_empty() && tags.iter().all(|t| t.mode == UwbMode::Tdoa) { tags[0].clone() } else { default }
}

/// The tag profile the floor map assumes for a technology.
pub fn tag_profile(profiles: &Profiles, tech: Tech) -> Profile {
    let kind = match tech {
        Tech::Uwb => DeviceKind::UwbTag,
        Tech::Ble => DeviceKind::BleTag,
        Tech::Rfid => DeviceKind::RfidTag,
        Tech::Wifi => DeviceKind::WifiClient,
    };
    profiles.default_for(kind).cloned().unwrap_or_default()
}

/// Received power (dBm) with a propagation term from FDTD: `prop` is the
/// link gain of the path including the node's antenna (dB, usually negative).
/// UWB: total power at the tag; BLE: RSSI at the node (the tag transmits;
/// `rssi_at_1m` is its power 1 m away, isotropic); RFID: forward-link power at the tag.
pub fn fdtd_rx_dbm(tech: Tech, node: &Profile, tag: &Profile, tag_gain: f32, prop: f32) -> f32 {
    match tech {
        Tech::Uwb if tag.mode == UwbMode::Tdoa => radio::uwb_tx_total_dbm(tag) + prop + tag_gain,
        Tech::Uwb => radio::uwb_tx_total_dbm(node) + prop + tag_gain,
        Tech::Ble | Tech::Wifi => radio::rssi_1m(tag) + solver::fspl_db(1.0, node.freq_mhz as f64) as f32 + prop + tag_gain,
        Tech::Rfid => node.tx_power_dbm + prop + tag.antenna_gain_dbi,
    }
}

/// Evaluates one floor point against a set of infrastructure nodes, with the
/// planner's own link model, so the map shows what the optimiser scores. Each
/// path carries the antenna gain of both ends (`Path::gain_db`, UWB, BLE, WiFi);
/// RFID readers' gain towards `p` comes from `ants`. Where `props[i]` is set
/// (FDTD), it replaces the ray model's distance and wall loss for node `i`;
/// line of sight and what blocks a UWB first path still come from `paths`.
#[allow(clippy::too_many_arguments)]
fn eval(tech: Tech, p: Vec3, nodes: &[Node], paths: &[Path], props: &[Option<f32>], tag: &Profile, tag_gain: f32, ants: Antennas) -> Cell {
    let mut c = Cell { err: f32::NAN, signal: f32::NEG_INFINITY, proximity_m: f32::NAN, ..default() };
    // Adds a heard node's measurement. FDTD can pass a link the ray model
    // calls out of range: then it counts with the node's own plain noise.
    let measure = |acc: &mut Acc, m: Method, n: &Node, path: &Path, i: usize| -> bool {
        let to = p - n.pos;
        let u = to.normalize_or_zero().to_array();
        let e = match placement::link_info(m, &n.prof, tag, path, n.fwd.angle_between(to), 0.0) {
            Some(l) => Entry { pt: 0, p: l.p, w: l.w, v: l.v, b: l.b, u, bearing: l.bearing },
            None if m == Method::Aoa => return false,
            None => {
                let s = if m == Method::Rssi { radio::rssi_distance_sigma(&n.prof, path.distance) } else { n.prof.range_sigma_m.max(0.02) };
                Entry { pt: 0, p: 0.9, w: 0.9 / (s * s), v: 0.9 / (s * s), b: 0.0, u, bearing: false }
            }
        };
        acc.add(i as u32, &e);
        true
    };
    match tech {
        Tech::Uwb => {
            let mut los = Vec::new();
            let mut all = Vec::new();
            let method = if tag.mode == UwbMode::Tdoa { Method::Tdoa } else { Method::Twr };
            let mut acc = Acc::default();
            for (i, ((n, path), prop)) in nodes.iter().zip(paths).zip(props).enumerate() {
                let prof = &n.prof;
                let passes = match prop {
                    Some(prop) => {
                        let rx = fdtd_rx_dbm(tech, prof, tag, tag_gain, *prop);
                        if path.distance <= prof.max_range_m {
                            c.signal = c.signal.max(rx);
                        }
                        rx >= prof.sensitivity_dbm && path.distance <= prof.max_range_m && path.blocker.is_none() && path.wall_eq <= prof.max_walls as f32 + 1e-3
                    }
                    None if method == Method::Tdoa => {
                        if path.distance <= prof.max_range_m {
                            c.signal = c.signal.max(radio::uwb_blink_rx_power(prof, tag, *path));
                        }
                        path.uwb_blink_passes(prof, tag)
                    }
                    None => {
                        if path.distance <= prof.max_range_m {
                            c.signal = c.signal.max(radio::uwb_rx_power(prof, *path));
                        }
                        path.uwb_passes(prof)
                    }
                };
                if !passes {
                    continue;
                }
                all.push(n.pos.as_dvec3());
                if path.los() {
                    los.push(n.pos.as_dvec3());
                }
                measure(&mut acc, method, n, path, i);
            }
            c.los = los.len() as u32;
            c.heard = all.len() as u32;
            if all.len() >= method.need() {
                c.dop = solver::dop(Dim::Plane(p.y as f64), if los.len() >= 3 { &los } else { &all }, p.as_dvec3()) as f32;
                let e = acc.err(method, false);
                if e.is_finite() {
                    c.err = e;
                }
            }
        }
        Tech::Ble | Tech::Wifi => {
            let mut heard = Vec::new();
            let mut acc = Acc::default();
            let mut need = usize::MAX;
            let mut strongest: Option<(f32, Vec3)> = None;
            for (i, ((n, path), prop)) in nodes.iter().zip(paths).zip(props).enumerate() {
                // `rssi_at_1m` is calibrated with isotropic antennas: add the
                // antenna gains of both ends (on the path).
                let rssi = match prop {
                    Some(prop) => fdtd_rx_dbm(tech, &n.prof, tag, tag_gain, *prop),
                    None => radio::ble_mean_rssi(&n.prof, tag, *path),
                };
                if path.distance <= n.prof.max_range_m {
                    c.signal = c.signal.max(rssi);
                }
                if rssi >= n.prof.sensitivity_dbm && path.distance <= n.prof.max_range_m {
                    let m = Method::of(tech, &n.prof, tag);
                    if measure(&mut acc, m, n, path, i) {
                        need = need.min(m.need());
                        heard.push(n.pos.as_dvec3());
                        if strongest.is_none_or(|(s, _)| rssi > s) {
                            strongest = Some((rssi, n.pos));
                        }
                        if path.los() {
                            c.los += 1;
                        }
                    }
                }
            }
            c.heard = heard.len() as u32;
            if heard.len() >= 3 {
                c.dop = solver::dop(Dim::Plane(p.y as f64), &heard, p.as_dvec3()) as f32;
            }
            if heard.len() >= need {
                // AoA and RSSI bearings and ranges add up the same way.
                let e = acc.err(Method::Rssi, false);
                if e.is_finite() {
                    c.err = e;
                }
            }
            // Too few for a fix: "near the strongest node", as the simulation reports it. That's
            // no position, and it's as wrong as the tag is far from the node.
            if let Some((_, at)) = strongest.filter(|_| !c.err.is_finite()) {
                c.proximity_m = (at - p).xz().length();
            }
        }
        Tech::Rfid => {
            let mut best = f32::MIN;
            let mut miss = 1.0f32;
            for ((n, path), prop) in nodes.iter().zip(paths).zip(props) {
                let (fwd, margin, read) = match prop {
                    Some(prop) => {
                        let rx = fdtd_rx_dbm(tech, &n.prof, tag, tag_gain, *prop);
                        let margin = rx - tag.tag_sensitivity_dbm;
                        (rx, margin, radio::normal_above(margin, radio::RFID_FADE_SIGMA_DB))
                    }
                    None => {
                        let to = (p - n.pos).normalize_or_zero();
                        let g = ants.gain(n.antenna.as_ref(), Tech::Rfid, true, &n.prof, n.fwd, to);
                        let (f, r) = radio::rfid_margins(&n.prof, tag, path, g);
                        // The tag must be energised and its reply heard.
                        (f + tag.tag_sensitivity_dbm, f.min(r), radio::rfid_read_probability(&n.prof, tag, path, g))
                    }
                };
                if path.distance <= n.prof.max_range_m {
                    c.signal = c.signal.max(fwd);
                    miss *= 1.0 - read;
                    if read >= 0.5 {
                        c.heard += 1;
                        if path.los() {
                            c.los += 1;
                        }
                    }
                }
                best = best.max(margin);
            }
            // The optimiser's criterion, so the map shows what it scores.
            if 1.0 - miss >= placement::READ_P {
                c.err = best;
            }
        }
    }
    c
}

#[allow(clippy::too_many_arguments)]
pub fn compute(
    In(req): In<CoverageRequest>,
    mut cov: ResMut<Coverage>,
    profiles: Res<Profiles>,
    devices: Query<(Entity, &Device, &Transform)>,
    objects: Query<(&PlacedObject, &GlobalTransform)>,
    parts: OccluderParts,
    occ_objects: Query<(Entity, &PlacedObject, &Transform)>,
    (applied, library, fdtd): (Res<AppliedAntenna>, Res<AntennaLibrary>, Res<super::fdtd::Fdtd>),
) {
    let ants = Antennas { library: &library, applied: &applied };
    let (lo, hi) = area(&objects, &devices);
    let size = hi - lo;
    // At most ~6000 cells.
    let cell = req.cell.max(0.1).max((size.x * size.y / 6000.0).sqrt());
    let nx = (size.x / cell).ceil().max(1.0) as usize;
    let nz = (size.y / cell).ceil().max(1.0) as usize;
    let occ = Occluders::build(&parts, &occ_objects);
    let nodes: Vec<Node> = devices
        .iter()
        .filter(|(_, d, _)| d.enabled && d.kind == req.tech.infra())
        .map(|(e, d, t)| Node { entity: e, pos: t.translation, fwd: *t.forward(), prof: profiles.for_device(d), antenna: d.radio.antenna.clone() })
        .collect();
    let tag = planned_tag(&profiles, req.tech, devices.iter().map(|(_, d, _)| d));
    // A tag's facing is unknown at a floor point: its omni gain, else isotropic.
    let tag_gain = ants.tag_gain_unaimed(None, &tag);
    // FDTD source: each node's run in the latest whole-scene result, if it is
    // still where it was simulated; the others stay on the ray model.
    let scene = (cov.source == MapSource::Fdtd).then(|| fdtd.scene(req.tech)).flatten();
    // A run that can't carry the map doesn't reach it, however the source was set.
    let blocked = scene.as_ref().and_then(|s| s.use_block());
    let scene = scene.filter(|_| blocked.is_none());
    let runs: Vec<Option<(usize, f32)>> = nodes
        .iter()
        .map(|n| {
            let s = scene.as_ref()?;
            let i = s.index_of(n.entity)?;
            let t = &s.per_tx[i].tx;
            ((t.pos - n.pos).length() < 0.02).then_some((i, t.gain_fix_db))
        })
        .collect();
    let mut cells = Vec::with_capacity(nx * nz);
    for iz in 0..nz {
        for ix in 0..nx {
            let p = Vec3::new(lo.x + (ix as f32 + 0.5) * cell, req.height, lo.y + (iz as f32 + 0.5) * cell);
            let paths: Vec<Path> = nodes
                .iter()
                .map(|n| {
                    if (n.pos - p).length() > n.prof.max_range_m * 1.1 {
                        return Path { distance: f32::MAX, walls: 99, wall_eq: 99.0, bias_eq: 99.0, ..Default::default() };
                    }
                    let path = occ.path(p, n.pos, None).in_band(n.prof.freq_mhz);
                    match req.tech {
                        Tech::Rfid => path,
                        _ => Path { gain_db: ants.gain(n.antenna.as_ref(), req.tech, true, &n.prof, n.fwd, p - n.pos) + tag_gain, ..path },
                    }
                })
                .collect();
            let props: Vec<Option<f32>> = runs.iter().map(|r| r.and_then(|(i, _)| scene.as_ref()?.propagation_db(i, p))).collect();
            cells.push(eval(req.tech, p, &nodes, &paths, &props, &tag, tag_gain, ants));
        }
    }
    let source_note = match (cov.source, &scene) {
        (MapSource::Ray, _) => String::new(),
        (MapSource::Fdtd, None) => match &blocked {
            Some(why) => format!("FDTD run not used, {why}: ray model shown"),
            None => format!("No whole-scene FDTD run for {} yet: ray model shown", req.tech.label()),
        },
        (MapSource::Fdtd, Some(s)) => {
            let used = runs.iter().filter(|r| r.is_some()).count();
            let scaled = if s.plan.scaled() { format!(" · scaled ×{:.1}, shadows up to {:.0} dB too strong", s.plan.scale, s.plan.shadow_error_db()) } else { String::new() };
            let stale = nodes.len() - used;
            let stale = if stale > 0 { format!(" · {stale} of {} moved or new since the run (ray model for those)", nodes.len()) } else { String::new() };
            format!("FDTD · {}{scaled}{stale}", s.band.label())
        }
    };
    *cov = Coverage {
        tech: req.tech,
        cell,
        origin: lo,
        nx,
        nz,
        cells,
        visible: true,
        view: cov.view,
        dirty: true,
        summary: String::new(),
        suggestions: std::mem::take(&mut cov.suggestions),
        suggestion_facing: std::mem::take(&mut cov.suggestion_facing),
        suggestion_note: std::mem::take(&mut cov.suggestion_note),
        quality: cov.quality,
        height: req.height,
        source: cov.source,
        source_note,
        entity: cov.entity,
    };
    let (covered, mean, p95) = cov.stats();
    let (near_only, worst) = cov.proximity_only();
    cov.summary = match req.tech {
        Tech::Rfid => format!("{} readers: {:.0}% of the floor has a read chance over 90% at {:.1} m", nodes.len(), covered * 100.0, req.height),
        _ => format!(
            "{} {}: {:.0}% of the floor has a fix at {:.1} m; predicted RMS error mean {:.2} m, worst 5% from {:.2} m{}",
            nodes.len(),
            if req.tech == Tech::Uwb { "anchors" } else { "nodes" },
            covered * 100.0,
            req.height,
            mean,
            p95,
            if near_only > 0.0 { format!("; {:.0}% more is heard by too few for a position, up to {worst:.0} m off", near_only * 100.0) } else { String::new() }
        ),
    };
}

#[derive(Clone, Debug, Default)]
pub struct OptimizeRequest {
    pub tech: Tech,
    /// Most devices to add.
    pub add: usize,
    pub height: f32,
    /// Mounting points (position, facing). `None` = perimeter + grid at the profile's mount height.
    pub candidates: Option<Vec<(Vec3, Vec3)>>,
    /// Stop once this share of the free floor has a usable fix (the devices a fix needs heard,
    /// in line of sight or not, and 95 % of readings within 3× the target) or, for RFID, a
    /// read chance over 90 %…
    pub target_coverage: Option<f32>,
    /// …and across the floor, points with no fix included, 95 % of readings land within this.
    pub target_p95_m: Option<f32>,
    /// Devices that must be heard for a covered point (default 3 for UWB/BLE, 1 for RFID).
    /// 4 leaves one spare to reject a blocked range.
    pub min_los: Option<usize>,
    /// Zones (min, max) where tags must be located: sampled more densely,
    /// weighted higher, and each must reach `target_coverage` on its own.
    pub zones: Vec<(Vec3, Vec3)>,
    /// Where each candidate hangs (parallel to `candidates`); inferred when missing.
    pub mounts: Option<Vec<Mount>>,
    /// Infrastructure profile to plan with (default: the kind's first).
    pub profile: Option<String>,
    /// Tag heights to plan for (default: `height`), e.g. 0.3 / 1.0 / 1.8 m.
    pub heights: Vec<f32>,
    /// Plan for full 3D fixes instead of X/Z at a known height.
    pub dim3: bool,
    /// Keep the targets with any one device failed.
    pub redundancy: bool,
    pub min_spacing_m: Option<f32>,
    /// Weight of install cost against coverage (default small).
    pub cost_weight: Option<f32>,
    /// Where the cables run to: far mounts cost more.
    pub head_end: Option<Vec3>,
    /// Slide the chosen devices along their mounting surface afterwards.
    pub refine: bool,
    /// Also try wall, column and pole mounts at other heights.
    pub expand_heights: bool,
    /// Exact spots tags must be read or located at, by group: racks' shelf
    /// levels. They count even inside an object's footprint, and each group
    /// must reach `target_coverage` on its own, as a zone does.
    pub targets: Vec<(Vec3, u16)>,
    /// Spots taken by another technology's devices: unavailable here.
    pub reserved: Vec<Vec3>,
    /// Shared with another technology's device: much cheaper to mount.
    pub shared_mounts: Vec<Vec3>,
    /// Areas the simulation measured badly: much heavier in the score.
    pub heavy: Vec<(Vec3, Vec3)>,
    /// Add at least this many devices even if the prediction says it's enough
    /// (the simulation knows better).
    pub force_add: usize,
}

#[derive(Clone, Debug, Default)]
pub struct OptimizeResult {
    /// Chosen mounting points: (position, facing).
    pub chosen: Vec<(Vec3, Vec3)>,
    pub coverage_after: f32,
    /// How far 95 % of readings land, at the floor's 95th percentile with no-fix points
    /// counted: infinite past 5 % of the floor without a position.
    pub p95_after: f32,
    /// The same over the points with a fix.
    pub p95_located: f32,
    /// The targets asked for are met. Not "as much as could be reached".
    pub targets_met: bool,
    /// Share of the floor every mounting point together could cover.
    pub reachable_coverage: f32,
    /// Covered share of each requested zone (NaN = no walkable floor in it).
    pub zone_coverage: Vec<f32>,
    /// Zones (with walkable floor) that meet the coverage target.
    pub zones_met: usize,
    /// Covered share of each group of `targets`.
    pub target_coverage: Vec<f32>,
    /// p95 with each point's worst single device failure (redundancy on).
    pub p95_k1: f32,
    /// Install cost of the chosen devices.
    pub cost: f32,
    /// RFID: reader pairs in each other's beam.
    pub conflicts: usize,
    /// RFID: mean read chance outside the zones.
    pub stray: f32,
    /// TDoA: anchors without a clock-sync path to the others.
    pub unsynced: usize,
}

/// World-space bounds (min, max) of the placed objects.
pub fn object_boxes(objects: &Query<(&PlacedObject, &GlobalTransform)>) -> Vec<(Vec3, Vec3)> {
    objects
        .iter()
        .map(|(o, gt)| {
            let (a, b) = o.aabb();
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            for x in [a.x, b.x] {
                for y in [a.y, b.y] {
                    for z in [a.z, b.z] {
                        let p = gt.transform_point(Vec3::new(x, y, z));
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
            }
            (lo, hi)
        })
        .collect()
}

/// Inside the footprint of an object standing on the floor (where nobody
/// walks); ceilings and beams overhead do not count.
pub fn blocked(boxes: &[(Vec3, Vec3)], p: Vec2, margin: f32) -> bool {
    boxes.iter().any(|(lo, hi)| p.x >= lo.x - margin && p.x <= hi.x + margin && p.y >= lo.z - margin && p.y <= hi.z + margin && hi.y > 0.15 && lo.y < 0.5)
}

/// Default mounting points: the walls (perimeter, every ~3.5 m) plus an
/// interior grid (ceiling/columns), facing the middle of the area.
pub fn default_candidates(lo: Vec2, hi: Vec2, height: f32) -> Vec<(Vec3, Vec3)> {
    let size = hi - lo;
    let c = (lo + hi) * 0.5;
    let face = |p: Vec2| {
        let d = (c - p).normalize_or_zero();
        Vec3::new(d.x, 0.0, d.y)
    };
    let mut v = Vec::new();
    let inset = 0.3;
    let (a, b) = (lo + Vec2::splat(inset), hi - Vec2::splat(inset));
    let nx = ((b.x - a.x) / 3.5).ceil().max(1.0) as usize;
    let nz = ((b.y - a.y) / 3.5).ceil().max(1.0) as usize;
    for i in 0..=nx {
        let x = a.x + (b.x - a.x) * i as f32 / nx as f32;
        for z in [a.y, b.y] {
            v.push(Vec2::new(x, z));
        }
    }
    for k in 1..nz {
        let z = a.y + (b.y - a.y) * k as f32 / nz as f32;
        for x in [a.x, b.x] {
            v.push(Vec2::new(x, z));
        }
    }
    let cc = (size.x.max(size.y) / 7.0).max(2.5);
    let mut z = a.y + cc;
    while z < b.y - cc * 0.5 {
        let mut x = a.x + cc;
        while x < b.x - cc * 0.5 {
            v.push(Vec2::new(x, z));
            x += cc;
        }
        z += cc;
    }
    v.into_iter().map(|p| (Vec3::new(p.x, height, p.y), face(p))).collect()
}

/// Beyond this many candidates extra heights are not worth the ray casts.
const MAX_EXPANDED_CANDS: usize = 900;

/// A device's measurements from a spot, and the surfaces it may slide on.
struct SceneMover<'a, S, G> {
    scene: &'a S,
    pts: &'a [Point],
    method: Method,
    prof: &'a Profile,
    tag: &'a Profile,
    gain: &'a G,
    lo: Vec2,
    hi: Vec2,
    boxes: &'a [(Vec3, Vec3)],
}

impl<S: RadioScene, G: Fn(&Profile, Vec3, Vec3) -> f32 + Sync> Mover for SceneMover<'_, S, G> {
    fn settle(&self, from: &Candidate, to: Vec3) -> Option<Vec3> {
        if to.x < self.lo.x || to.x > self.hi.x || to.z < self.lo.y || to.z > self.hi.y {
            return None;
        }
        if from.mount.on_surface() {
            // Same gap to the surface behind it, wherever it slides.
            let back = -from.facing.normalize_or_zero();
            let gap = self.scene.surface(from.pos, back, 1.0)?;
            let t = self.scene.surface(to, back, gap + 0.3)?;
            let land = to + back * (t - gap);
            // Rounding a corner onto another surface is not sliding.
            (land.distance(from.pos) <= (to - from.pos).length() + 0.05).then_some(land)
        } else {
            let inside = self.boxes.iter().any(|(a, b)| to.x >= a.x && to.x <= b.x && to.y >= a.y && to.y <= b.y && to.z >= a.z && to.z <= b.z);
            (!inside).then_some(to)
        }
    }

    fn column(&self, pos: Vec3, facing: Vec3) -> Column {
        placement::column(self.scene, self.pts, self.method, self.prof, self.tag, pos, facing, self.gain)
    }
}

/// Chooses mounting points for the most coverage and accuracy at the lowest
/// install cost, or the fewest that meet the targets, with the same link model
/// the simulation runs (see `placement`).
#[allow(clippy::too_many_arguments)]
pub fn optimize(
    In(req): In<OptimizeRequest>,
    mut cov: ResMut<Coverage>,
    profiles: Res<Profiles>,
    devices: Query<(Entity, &Device, &Transform)>,
    objects: Query<(&PlacedObject, &GlobalTransform)>,
    parts: OccluderParts,
    occ_objects: Query<(Entity, &PlacedObject, &Transform)>,
    (applied, library): (Res<AppliedAntenna>, Res<AntennaLibrary>),
) -> OptimizeResult {
    let infra_kind = req.tech.infra();
    let wanted = req.profile.as_deref().and_then(|id| profiles.get(id)).filter(|p| p.kind == infra_kind);
    let Some(prof) = wanted.or_else(|| profiles.default_for(infra_kind)).cloned() else { return OptimizeResult::default() };
    let (lo, hi) = area(&objects, &devices);
    let size = hi - lo;
    let boxes = object_boxes(&objects);
    let tag = planned_tag(&profiles, req.tech, devices.iter().map(|(_, d, _)| d));
    let method = Method::of(req.tech, &prof, &tag);
    let reads = method == Method::Read;

    // Evaluation points on the walkable floor, at every tag height.
    let heights: Vec<f32> = if req.heights.is_empty() { vec![req.height] } else { req.heights.clone() };
    let per_height = 1.0 / heights.len() as f32;
    let ec = (size.x * size.y / 500.0).sqrt().max(0.75);
    let mut pts: Vec<Point> = Vec::new();
    let zones_of = |x: f32, z: f32| -> Vec<u16> { req.zones.iter().enumerate().filter(|(_, (a, b))| x >= a.x && x <= b.x && z >= a.z && z <= b.z).map(|(i, _)| i as u16).collect() };
    // Floor inside a zone counts double: that is where tags must be found.
    const ZONE_WEIGHT: f32 = 2.0;
    for &h in &heights {
        let mut z = lo.y + ec * 0.5;
        while z < hi.y {
            let mut x = lo.x + ec * 0.5;
            while x < hi.x {
                if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                    let zones = zones_of(x, z);
                    let weight = per_height * if zones.is_empty() { 1.0 } else { ZONE_WEIGHT };
                    pts.push(Point { pos: Vec3::new(x, h, z), weight, floor: true, zones });
                }
                x += ec;
            }
            z += ec;
        }
    }
    // Zones get a finer grid of their own so a small zone is never judged on
    // one or two points (or none) of the coarse floor grid.
    let zc = (ec * 0.5).max(0.5);
    for &h in &heights {
        for (a, b) in &req.zones {
            let mut z = a.z + zc * 0.5;
            while z < b.z {
                let mut x = a.x + zc * 0.5;
                while x < b.x {
                    if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                        pts.push(Point { pos: Vec3::new(x, h, z), weight: per_height * ZONE_WEIGHT, floor: false, zones: zones_of(x, z) });
                    }
                    x += zc;
                }
                z += zc;
            }
        }
        // What the simulation measured badly weighs a lot more.
        for (a, b) in &req.heavy {
            let mut z = a.z + zc * 0.5;
            while z < b.z {
                let mut x = a.x + zc * 0.5;
                while x < b.x {
                    if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                        pts.push(Point { pos: Vec3::new(x, h, z), weight: per_height * 8.0, floor: false, zones: Vec::new() });
                    }
                    x += zc;
                }
                z += zc;
            }
        }
    }
    let groups = req.targets.iter().map(|t| t.1 as usize + 1).max().unwrap_or(0);
    for (p, g) in &req.targets {
        pts.push(Point { pos: *p, weight: ZONE_WEIGHT, floor: false, zones: vec![(req.zones.len() + *g as usize) as u16] });
    }

    // Candidate mounting points, with what they cost and hang on.
    let spacing = req.min_spacing_m.unwrap_or(if reads { 1.0 } else { 1.5 });
    let default_list = req.candidates.is_none();
    let raw = req.candidates.clone().unwrap_or_else(|| default_candidates(lo, hi, prof.mount_height_m));
    // Rounded and sorted: mount points are derived from objects in the order
    // they finished loading, and that must not change the plan.
    let round = |v: Vec3, k: f32| (v * k).round() / k;
    let mut raw: Vec<(Vec3, Vec3, Option<Mount>)> = raw.into_iter().enumerate().map(|(i, (p, f))| (round(p, 1000.0), round(f, 10000.0), req.mounts.as_ref().and_then(|m| m.get(i)).copied())).collect();
    raw.sort_by(|a, b| {
        let key = |c: &(Vec3, Vec3, Option<Mount>)| [c.0.x, c.0.y, c.0.z, c.1.x, c.1.y, c.1.z];
        key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut cands: Vec<Candidate> = Vec::new();
    for (pos, mut facing, given) in raw {
        let mount = match given {
            Some(m) => m,
            None if default_list => Mount::Free,
            None if facing.y < -0.9 => Mount::Ceiling,
            None => Mount::Wall,
        };
        // A locator's array looks down unless it hangs on a wall.
        if method == Method::Aoa && mount == Mount::Free {
            facing = Vec3::NEG_Y;
        }
        if req.reserved.iter().any(|r| r.distance(pos) < SAME_SPOT_M) || cands.iter().any(|c: &Candidate| c.pos.distance(pos) < SAME_SPOT_M && c.facing.dot(facing) > 0.98) {
            continue;
        }
        let shared = req.shared_mounts.iter().any(|s| s.distance(pos) < SAME_SPOT_M);
        let cost = placement::install_cost(mount, pos, req.head_end) * if shared { 0.3 } else { 1.0 };
        cands.push(Candidate { pos, facing, mount, cost });
    }
    if req.expand_heights && cands.len() * 3 <= MAX_EXPANDED_CANDS {
        // Up to the lowest thing overhead at mid-room, if anything is there.
        let occ = Occluders::build(&parts, &occ_objects);
        let c = (lo + hi) * 0.5;
        let ceiling = RadioScene::surface(&occ, Vec3::new(c.x, 0.5, c.y), Vec3::Y, 30.0).map(|t| t + 0.5);
        cands = placement::expand_heights(&cands, ceiling);
    }
    if reads {
        let mut aim_at = req.zones.clone();
        aim_at.extend(req.targets.iter().map(|(p, _)| (*p, *p)));
        cands = placement::expand_aims(&cands, &aim_at, prof.max_range_m);
    }

    // How strong a link is towards a point: its transmit power and the antenna
    // gains on the link (its own settings, else the profile's). New devices
    // are planned with the defaults.
    let ants = Antennas { library: &library, applied: &applied };
    let tag_gain = ants.tag_gain_unaimed(None, &tag);
    let tech = req.tech;
    let gain_new = |p: &Profile, fwd: Vec3, dir: Vec3| ants.gain(None, tech, true, p, fwd, dir) + if reads { 0.0 } else { tag_gain };
    let occ = Occluders::build(&parts, &occ_objects);
    let cols: Vec<Column> = cands.iter().map(|c| placement::column(&occ, &pts, method, &prof, &tag, c.pos, c.facing, &gain_new)).collect();
    let mut existing: Vec<_> = devices.iter().filter(|(_, d, _)| d.enabled && d.kind == infra_kind).collect();
    existing.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    let fixed: Vec<(Vec3, Column)> = existing
        .into_iter()
        .map(|(_, d, t)| {
            let (pos, fwd, pr, an) = (t.translation, *t.forward(), profiles.for_device(d), d.radio.antenna.clone());
            let gain = |p: &Profile, f: Vec3, dir: Vec3| ants.gain(an.as_ref(), tech, true, p, f, dir) + if reads { 0.0 } else { tag_gain };
            (pos, placement::column(&occ, &pts, Method::of(tech, &pr, &tag), &pr, &tag, pos, fwd, &gain))
        })
        .collect();

    let need = req.min_los.unwrap_or(method.need()) + req.redundancy as usize + if req.dim3 && !reads { 1 } else { 0 };
    let target_m = req.target_p95_m.unwrap_or(method.default_target_m());
    let cfg = placement::Config {
        method,
        dim3: req.dim3,
        need,
        target_m,
        err_cap: target_m * 3.0,
        redundancy: req.redundancy,
        min_spacing_m: spacing,
        cost_weight: req.cost_weight.map_or(placement::Config::default().cost_weight, |w| w as f64),
        zones_only: reads && !(req.zones.is_empty() && req.targets.is_empty()),
        conflict_m: if reads { prof.max_range_m.min(8.0) } else { 0.0 },
        beam_deg: prof.beam_deg,
        target_coverage: req.target_coverage,
        target_p95: req.target_p95_m,
        zone_count: req.zones.len() + groups,
        force_add: req.force_add,
        avoid: req.reserved.clone(),
        ..Default::default()
    };
    let sync_prof = prof.clone();
    let sync_scene = &occ;
    let sync = move |a: Vec3, b: Vec3| RadioScene::path(sync_scene, a, b).in_band(sync_prof.freq_mhz).uwb_passes(&sync_prof);
    let n_cands = cands.len();
    let mut planner = Planner::new(cfg, &pts, cands, cols, fixed, Some(&sync));
    let cov0 = planner.metrics().coverage;
    let mover = SceneMover { scene: &occ, pts: &pts, method, prof: &prof, tag: &tag, gain: &gain_new, lo, hi, boxes: &boxes };
    planner.run(req.add, req.refine.then_some(&mover as &dyn Mover));
    let m = planner.metrics();

    let met = planner.targets_met(&m);
    let zones_met = planner.zones_met(&m, req.zones.len());
    let result = OptimizeResult {
        chosen: planner.chosen.iter().map(|&c| (planner.cands[c].pos, planner.cands[c].facing)).collect(),
        coverage_after: m.coverage,
        p95_after: m.p95,
        p95_located: m.p95_located,
        targets_met: met,
        reachable_coverage: planner.reachable_coverage(),
        zone_coverage: m.zones[..req.zones.len()].to_vec(),
        zones_met,
        target_coverage: m.zones[req.zones.len()..].to_vec(),
        p95_k1: m.p95_k1,
        cost: m.cost,
        conflicts: m.conflicts,
        stray: m.stray,
        unsynced: m.unsynced,
    };
    let mut note = format!(
        "{} suggested {} ({} mounting points): {} {:.0}% → {:.0}%",
        result.chosen.len(),
        prof.name,
        n_cands,
        if reads { "read chance over 90% on" } else { "floor with a fix" },
        cov0 * 100.0,
        m.coverage * 100.0,
    );
    let zone_vals: Vec<f32> = result.zone_coverage.iter().copied().filter(|z| !z.is_nan()).collect();
    if !zone_vals.is_empty() {
        note += &format!(", zones {}/{} fully covered", zone_vals.iter().filter(|c| **c >= 0.999).count(), zone_vals.len());
    }
    if !reads && m.p95.is_finite() {
        note += &format!(", p95 predicted {:.0} cm", m.p95 * 100.0);
    } else if !reads && m.p95_located.is_finite() {
        note += &format!(", p95 predicted {:.0} cm where there is a fix", m.p95_located * 100.0);
    }
    if req.redundancy && m.p95_k1.is_finite() {
        note += &format!(" ({:.0} cm with a device down)", m.p95_k1 * 100.0);
    }
    note += &format!(", install cost {:.1}", m.cost);
    if m.conflicts > 0 {
        note += &format!(". {} reader pair(s) in each other's beam: put them on different channels or time slots", m.conflicts);
    }
    if m.unsynced > 0 {
        note += &format!(". {} anchor(s) can't reach the others for clock sync", m.unsynced);
    }
    cov.suggestions = result.chosen.iter().map(|(p, _)| *p).collect();
    cov.suggestion_facing = result.chosen.iter().map(|(_, f)| *f).collect();
    cov.suggestion_note = note;
    result
}

/// What the floor heatmap shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapView {
    /// Where readings are correct often enough (`QualitySpec`).
    #[default]
    Quality,
    Accuracy,
    Signal,
}

/// dBm range (weak, strong) used to colour the signal map.
pub fn signal_scale(tech: Tech) -> (f32, f32) {
    match tech {
        Tech::Uwb => (-95.0, -65.0),
        Tech::Ble => (-95.0, -60.0),
        Tech::Rfid => (-18.0, 0.0),
        Tech::Wifi => (-85.0, -45.0),
    }
}

fn color_for(tech: Tech, view: MapView, q: QualitySpec, c: &Cell) -> [u8; 4] {
    if view == MapView::Quality && tech != Tech::Rfid {
        if !c.err.is_finite() {
            return [60, 60, 70, 90];
        }
        let p = fix_share(c.err, q.target_m);
        if p >= q.min_share {
            // Pass: green, brighter towards 100 %.
            let t = ((p - q.min_share) / (1.0 - q.min_share).max(0.01)).clamp(0.0, 1.0);
            return [(70.0 - 30.0 * t) as u8, (175.0 + 55.0 * t) as u8, 90, 160];
        }
        // Fail: red (never right) → orange (almost there).
        let t = (p / q.min_share.max(0.01)).clamp(0.0, 1.0);
        return [235, (50.0 + 120.0 * t) as u8, 50, 150];
    }
    if view == MapView::Signal {
        if !c.signal.is_finite() {
            return [60, 60, 70, 90];
        }
        let (lo, hi) = signal_scale(tech);
        let t = ((c.signal - lo) / (hi - lo)).clamp(0.0, 1.0);
        // red (weak) -> yellow -> green (strong)
        let (r, g) = if t < 0.5 { (235.0, 60.0 + 360.0 * t) } else { (235.0 - 380.0 * (t - 0.5), 240.0) };
        return [r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, 70, 150];
    }
    if !c.err.is_finite() {
        return [60, 60, 70, 90];
    }
    match tech {
        Tech::Rfid => {
            let t = (c.err / 15.0).clamp(0.0, 1.0);
            [(40.0 + 30.0 * t) as u8, (120.0 + 100.0 * t) as u8, (200.0 + 55.0 * t) as u8, 150]
        }
        _ => {
            // green (≤10 cm) → yellow (30 cm) → red (≥1 m)
            let e = c.err;
            let (r, g) = if e <= 0.1 {
                (60.0, 210.0)
            } else if e <= 0.3 {
                let t = (e - 0.1) / 0.2;
                (60.0 + 190.0 * t, 210.0)
            } else {
                let t = ((e - 0.3) / 0.7).clamp(0.0, 1.0);
                (250.0, 210.0 * (1.0 - t))
            };
            [r as u8, g as u8, 70, 150]
        }
    }
}

/// Shows the heatmap as a translucent plane just above the floor.
pub fn sync_heatmap(
    mut commands: Commands,
    mut cov: ResMut<Coverage>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut vis: Query<&mut Visibility>,
) {
    if let Some(e) = cov.entity
        && let Ok(mut v) = vis.get_mut(e)
    {
        let want = if cov.visible { Visibility::Visible } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    if !cov.dirty {
        return;
    }
    cov.dirty = false;
    if let Some(e) = cov.entity.take() {
        commands.entity(e).despawn();
    }
    if cov.cells.is_empty() {
        return;
    }
    let mut data = Vec::with_capacity(cov.nx * cov.nz * 4);
    for c in &cov.cells {
        data.extend_from_slice(&color_for(cov.tech, cov.view, cov.quality, c));
    }
    let mut img = Image::new(
        Extent3d { width: cov.nx as u32, height: cov.nz as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.sampler = bevy::image::ImageSampler::nearest();
    let tex = images.add(img);
    let w = cov.nx as f32 * cov.cell;
    let d = cov.nz as f32 * cov.cell;
    let mesh = meshes.add(Plane3d::default().mesh().size(w, d));
    let mat = materials.add(StandardMaterial {
        base_color_texture: Some(tex),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let center = Vec3::new(cov.origin.x + w * 0.5, 0.02, cov.origin.y + d * 0.5);
    let e = commands
        .spawn((
            Name::new("RTLS coverage"),
            Mesh3d(mesh),
            MeshMaterial3d(mat),
            Transform::from_translation(center),
            if cov.visible { Visibility::Visible } else { Visibility::Hidden },
            bevy::picking::Pickable::IGNORE,
        ))
        .id();
    cov.entity = Some(e);
}

/// Keeps a shown map current: rescans shortly after infrastructure of the
/// mapped technology is added, moved, switched or removed, or its
/// transmit power or range changes.
pub fn refresh_on_change(
    cov: Res<Coverage>,
    mut jobs: ResMut<super::ui::PlanJobs>,
    devices: Query<(&Device, &Transform)>,
    profiles: Res<Profiles>,
    time: Res<Time>,
    mut last: Local<Vec<(Vec3, bool, [f32; 4])>>,
    mut due: Local<Option<f64>>,
) {
    if !cov.visible || cov.cells.is_empty() {
        last.clear();
        *due = None;
        return;
    }
    // Position, on/off and radio settings (TX power, range) of each device.
    let now: Vec<(Vec3, bool, [f32; 4])> = devices
        .iter()
        .filter(|(d, _)| d.kind == cov.tech.infra())
        .map(|(d, t)| {
            let p = profiles.resolve(&d.profile, d.kind);
            (t.translation, d.enabled, [p.uwb_tx_dbm_mhz, p.tx_power_dbm, p.max_range_m, p.freq_mhz])
        })
        .collect();
    if *last != now {
        let first = last.is_empty() && !now.is_empty();
        *last = now;
        if !first {
            // Wait until a drag settles.
            *due = Some(time.elapsed_secs_f64() + 0.4);
        }
    }
    if due.is_some_and(|t| time.elapsed_secs_f64() >= t) && jobs.coverage.is_none() {
        *due = None;
        jobs.coverage = Some(CoverageRequest { tech: cov.tech, height: if cov.height > 0.0 { cov.height } else { 1.2 }, cell: cov.cell.max(0.5) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_share_matches_circular_normal() {
        // RMS error equal to the target: 1 − 1/e ≈ 63 % of readings within it.
        assert!((fix_share(0.3, 0.3) - 0.632).abs() < 0.01);
        assert!(fix_share(0.1, 0.3) > 0.99);
        assert!(fix_share(1.0, 0.3) < 0.1);
        assert_eq!(fix_share(f32::NAN, 0.3), 0.0);
        // The 75 % line sits at err = target / sqrt(ln 4) ≈ 0.85 × target.
        let e = 0.3 / 4f32.ln().sqrt();
        assert!((fix_share(e, 0.3) - 0.75).abs() < 1e-4);
    }

    #[test]
    fn quality_colours_pass_and_fail() {
        let q = QualitySpec { target_m: 0.3, min_share: 0.75 };
        let good = Cell { err: 0.1, ..default() };
        let bad = Cell { err: 1.0, ..default() };
        let none = Cell { err: f32::NAN, ..default() };
        let g = color_for(Tech::Uwb, MapView::Quality, q, &good);
        let b = color_for(Tech::Uwb, MapView::Quality, q, &bad);
        assert!(g[1] > g[0], "pass is green");
        assert!(b[0] > b[1], "fail is red");
        assert_eq!(color_for(Tech::Uwb, MapView::Quality, q, &none), [60, 60, 70, 90]);
        let cov = Coverage { cells: vec![good, bad, none, good], quality: q, ..default() };
        assert_eq!(cov.quality_share(), 0.5);
    }

    fn lone_node(prof: Profile, pos: Vec3) -> Node {
        Node { entity: Entity::PLACEHOLDER, pos, fwd: Vec3::NEG_Y, prof, antenna: None }
    }

    #[test]
    fn the_rfid_map_uses_the_optimisers_read_chance() {
        // 1 dB of forward margin is a read chance near 70 %: the optimiser wants 90 %.
        let (lib, applied) = (AntennaLibrary::default(), AppliedAntenna::default());
        let ants = Antennas { library: &lib, applied: &applied };
        let reader = Profile { kind: DeviceKind::RfidReader, tx_power_dbm: 30.0, antenna_gain_dbi: 0.0, beam_deg: 360.0, freq_mhz: 920.5, sensitivity_dbm: -200.0, max_range_m: 500.0, ..default() };
        let tag = Profile { kind: DeviceKind::RfidTag, tag_sensitivity_dbm: -18.0, antenna_gain_dbi: 2.0, ..default() };
        let at = |margin: f32| 10f32.powf((30.0 + 2.0 + 18.0 - margin - 20.0 * 920.5f32.log10() + 27.55) / 20.0);
        let cell = |margin: f32| {
            let d = at(margin);
            let n = lone_node(reader.clone(), Vec3::new(0.0, 2.0, 0.0));
            eval(Tech::Rfid, Vec3::new(d, 2.0, 0.0), &[n], &[Path { distance: d, ..default() }], &[None], &tag, 0.0, ants)
        };
        assert!(!cell(1.0).err.is_finite(), "{:?}", cell(1.0));
        assert!(cell(4.0).err.is_finite(), "{:?}", cell(4.0));
    }

    #[test]
    fn heard_by_too_few_nodes_is_not_a_position() {
        let (lib, applied) = (AntennaLibrary::default(), AppliedAntenna::default());
        let ants = Antennas { library: &lib, applied: &applied };
        let node = Profile { kind: DeviceKind::BleNode, max_range_m: 30.0, ..default() };
        let tag = Profile { kind: DeviceKind::BleTag, ..default() };
        let p = Vec3::new(12.0, 1.0, 0.0);
        let n = lone_node(node, Vec3::new(0.0, 2.4, 0.0));
        let path = Path { distance: n.pos.distance(p), ..default() };
        let c = eval(Tech::Ble, p, &[n], &[path], &[None], &tag, 0.0, ants);
        // The sim's "fix" sits at the node, 12 m away.
        assert_eq!(c.heard, 1);
        assert!(!c.err.is_finite(), "{c:?}");
        assert!((c.proximity_m - 12.0).abs() < 0.01, "{c:?}");
    }

    #[test]
    fn fdtd_fallback_uses_the_nodes_own_noise() {
        // FDTD hears nodes the ray model can't: an RSSI node doesn't range to 10 cm for that.
        let (lib, applied) = (AntennaLibrary::default(), AppliedAntenna::default());
        let ants = Antennas { library: &lib, applied: &applied };
        let node = Profile { kind: DeviceKind::BleNode, max_range_m: 30.0, range_sigma_m: 0.1, ..default() };
        let tag = Profile { kind: DeviceKind::BleTag, ..default() };
        let p = Vec3::new(10.0, 1.0, 10.0);
        let nodes: Vec<Node> = [Vec3::new(0.0, 2.4, 0.0), Vec3::new(20.0, 2.4, 0.0), Vec3::new(10.0, 2.4, 20.0)].into_iter().map(|q| lone_node(node.clone(), q)).collect();
        let paths: Vec<Path> = nodes.iter().map(|n| Path { distance: n.pos.distance(p), walls: 3, wall_eq: 30.0, bias_eq: 30.0, ..default() }).collect();
        let c = eval(Tech::Ble, p, &nodes, &paths, &[Some(-30.0), Some(-30.0), Some(-30.0)], &tag, 0.0, ants);
        assert_eq!(c.heard, 3);
        assert!(c.err > 2.0, "{c:?}");
    }

    /// A dipole-like pattern (sin² θ, 2.15 dBi peak) on a 5° grid.
    fn dipole(axis_forward: bool) -> FdtdAntenna {
        let d = (0..=36)
            .map(|ti| {
                let s = ((ti as f32) * 5.0).to_radians().sin();
                vec![2.15 + 10.0 * (s * s).max(1e-4).log10(); 73]
            })
            .collect();
        FdtdAntenna { tech: Tech::Uwb, label: String::new(), source: Default::default(), theta_step: 5.0, phi_step: 5.0, d, mismatch_db: -0.5, axis_forward, bias_m: None }
    }

    #[test]
    fn fdtd_antenna_gain_follows_pattern_and_mount() {
        // Upright dipole: full gain towards the horizon, deep null straight
        // down, whichever way the device faces; mismatch always applies.
        let up = dipole(false);
        assert!((up.gain(Vec3::NEG_Z, Vec3::X) - 1.65).abs() < 0.01);
        assert!((up.gain(Vec3::X, Vec3::Z) - 1.65).abs() < 0.01);
        assert!(up.gain(Vec3::NEG_Z, Vec3::NEG_Y) < -25.0);
        // An anchor 1.4 m above a tag 5 m away still sees nearly the peak.
        assert!(up.gain(Vec3::NEG_Z, Vec3::new(5.0, -1.4, 0.0)) > 1.0);
        // Axis along the facing direction (patch mount): the null is in front.
        let fwd = dipole(true);
        assert!(fwd.gain(Vec3::X, Vec3::X) < -25.0);
        assert!((fwd.gain(Vec3::X, Vec3::Z) - 1.65).abs() < 0.01);
    }

    #[test]
    fn fdtd_link_replaces_the_ray_loss() {
        let (lib, applied) = (AntennaLibrary::default(), AppliedAntenna::default());
        let ants = Antennas { library: &lib, applied: &applied };
        let tag = Profile::default();
        let node = |x: f32| Node { entity: Entity::PLACEHOLDER, pos: Vec3::new(x, 2.0, 0.0), fwd: Vec3::NEG_Z, prof: Profile::default(), antenna: None };
        let nodes = [node(0.0), node(4.0)];
        let paths = [Path { distance: 3.0, ..default() }, Path { distance: 3.0, ..default() }];
        let p = Vec3::new(2.0, 1.2, 2.0);
        for tech in Tech::ALL {
            let ray = eval(tech, p, &nodes, &paths, &[None, None], &tag, 0.0, ants);
            // A strong FDTD link on one node: that is the strongest signal now.
            let fdtd = eval(tech, p, &nodes, &paths, &[Some(-20.0), None], &tag, 0.0, ants);
            let want = fdtd_rx_dbm(tech, &nodes[0].prof, &tag, 0.0, -20.0);
            assert!((fdtd.signal - want.max(ray.signal)).abs() < 1e-4, "{tech:?}: {} vs {want}", fdtd.signal);
            // A hopeless one: the ray model's other node alone.
            let weak = eval(tech, p, &nodes, &paths, &[Some(-200.0), Some(-200.0)], &tag, 0.0, ants);
            assert_eq!(weak.heard, 0, "{tech:?}");
        }
    }
}
