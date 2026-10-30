//! Deployment planning: coverage / DOP / predicted-error heatmap over the
//! floor, RFID read zones, and a greedy anchor placement optimiser. All of it
//! ray casts against the real scene geometry.

use bevy::asset::RenderAssetUsages;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::antenna::{AntennaChoice, AntennaLibrary, Antennas};
use super::device::{Device, DeviceKind, Profile, Profiles};
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
}

impl Tech {
    pub const ALL: [Tech; 3] = [Tech::Uwb, Tech::Ble, Tech::Rfid];
    pub fn label(self) -> &'static str {
        match self {
            Tech::Uwb => "UWB",
            Tech::Ble => "BLE",
            Tech::Rfid => "RFID",
        }
    }
    pub fn infra(self) -> DeviceKind {
        match self {
            Tech::Uwb => DeviceKind::UwbAnchor,
            Tech::Ble => DeviceKind::BleNode,
            Tech::Rfid => DeviceKind::RfidReader,
        }
    }
    pub fn parse(s: &str) -> Option<Tech> {
        match s.to_ascii_lowercase().as_str() {
            "uwb" => Some(Tech::Uwb),
            "ble" => Some(Tech::Ble),
            "rfid" => Some(Tech::Rfid),
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
    /// Predicted 1-sigma horizontal error (m); NaN = no fix.
    pub err: f32,
    /// Strongest received signal (dBm): UWB first path, BLE RSSI, RFID power at the tag.
    pub signal: f32,
}

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
    /// Directivity `d[θ][φ]` in dBi on a regular grid.
    pub theta_step: f32,
    pub phi_step: f32,
    pub d: Vec<Vec<f32>>,
    /// Mismatch loss at the band centre, 10·log10(1 − |S11|²), dB (≤ 0).
    pub mismatch_db: f32,
    /// Patch: the antenna axis (θ = 0) is the device's facing direction.
    /// Wire antennas (dipole, monopole) stand upright: the axis is world up.
    pub axis_forward: bool,
}

impl FdtdAntenna {
    /// Realised gain (dBi, mismatch included) from a device facing `fwd`
    /// towards `dir`.
    pub fn gain(&self, fwd: Vec3, dir: Vec3) -> f32 {
        let z = if self.axis_forward { fwd.normalize_or(Vec3::NEG_Z) } else { Vec3::Y };
        let x = if self.axis_forward { Vec3::Y.cross(z).normalize_or(Vec3::X) } else { Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::X) };
        let y = z.cross(x);
        let d = dir.normalize_or_zero();
        let theta = d.dot(z).clamp(-1.0, 1.0).acos().to_degrees();
        let phi = d.dot(y).atan2(d.dot(x)).to_degrees().rem_euclid(360.0);
        let Some(row) = self.d.get(((theta / self.theta_step).round() as usize).min(self.d.len().saturating_sub(1))) else { return self.mismatch_db };
        let v = row.get(((phi / self.phi_step).round() as usize).min(row.len().saturating_sub(1))).copied().unwrap_or(0.0);
        v.max(-30.0) + self.mismatch_db
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
        (covered, mean, errs[((errs.len() - 1) as f32 * 0.95) as usize])
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

/// The tag profile the floor map assumes for a technology.
pub fn tag_profile(profiles: &Profiles, tech: Tech) -> Profile {
    let kind = match tech {
        Tech::Uwb => DeviceKind::UwbTag,
        Tech::Ble => DeviceKind::BleTag,
        Tech::Rfid => DeviceKind::RfidTag,
    };
    profiles.default_for(kind).cloned().unwrap_or_default()
}

/// Received power (dBm) with a propagation term from FDTD: `prop` is the
/// link gain of the path including the node's antenna (dB, usually negative).
/// UWB: total power at the tag; BLE: RSSI at the node (the tag transmits;
/// `rssi_at_1m` is its power 1 m away, isotropic); RFID: forward-link power at the tag.
pub fn fdtd_rx_dbm(tech: Tech, node: &Profile, tag: &Profile, tag_gain: f32, prop: f32) -> f32 {
    match tech {
        Tech::Uwb => radio::uwb_tx_total_dbm(node) + prop + tag_gain,
        Tech::Ble => tag.rssi_at_1m + solver::fspl_db(1.0, node.freq_mhz as f64) as f32 + prop + tag_gain,
        Tech::Rfid => node.tx_power_dbm + prop + tag.antenna_gain_dbi,
    }
}

/// Evaluates one floor point against a set of infrastructure nodes. For UWB
/// and BLE each path carries the antenna gain of both ends (`Path::gain_db`);
/// RFID readers' gain towards `p` comes from `ants`. Where `props[i]` is set
/// (FDTD), it replaces the ray model's distance and wall loss for node `i`;
/// line of sight and what blocks a UWB first path still come from `paths`.
#[allow(clippy::too_many_arguments)]
fn eval(tech: Tech, p: Vec3, nodes: &[Node], paths: &[Path], props: &[Option<f32>], tag: &Profile, tag_gain: f32, ants: Antennas) -> Cell {
    let mut c = Cell { err: f32::NAN, signal: f32::NEG_INFINITY, ..default() };
    match tech {
        Tech::Uwb => {
            let mut los = Vec::new();
            let mut all = Vec::new();
            let mut sigma = 0.1f32;
            for ((n, path), prop) in nodes.iter().zip(paths).zip(props) {
                let prof = &n.prof;
                let passes = match prop {
                    Some(prop) => {
                        let rx = fdtd_rx_dbm(tech, prof, tag, tag_gain, *prop);
                        if path.distance <= prof.max_range_m {
                            c.signal = c.signal.max(rx);
                        }
                        rx >= prof.sensitivity_dbm && path.distance <= prof.max_range_m && path.blocker.is_none() && path.wall_eq <= prof.max_walls as f32 + 1e-3
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
                sigma = n.prof.range_sigma_m.max(0.02);
            }
            c.los = los.len() as u32;
            c.heard = all.len() as u32;
            let dim = Dim::Plane(p.y as f64);
            if los.len() >= 3 {
                c.dop = solver::dop(dim, &los, p.as_dvec3()) as f32;
                c.err = sigma * c.dop;
            } else if all.len() >= 3 {
                c.dop = solver::dop(dim, &all, p.as_dvec3()) as f32;
                // NLOS-dominated: bias-limited.
                c.err = sigma * c.dop + nodes[0].prof.nlos_bias_m;
            }
        }
        Tech::Ble => {
            let mut heard = Vec::new();
            let mut spread = 0.0;
            for ((n, path), prop) in nodes.iter().zip(paths).zip(props) {
                // `rssi_at_1m` is calibrated with isotropic antennas: add the
                // antenna gains of both ends (on the path).
                let rssi = match prop {
                    Some(prop) => fdtd_rx_dbm(tech, &n.prof, tag, tag_gain, *prop),
                    None => tag.rssi_at_1m - 10.0 * n.prof.path_loss_exp * path.distance.max(0.1).log10() - n.prof.wall_loss_db * path.wall_eq + path.gain_db,
                };
                if path.distance <= n.prof.max_range_m {
                    c.signal = c.signal.max(rssi);
                }
                if rssi >= n.prof.sensitivity_dbm && path.distance <= n.prof.max_range_m {
                    heard.push(n.pos.as_dvec3());
                    spread += path.distance * n.prof.shadow_sigma_db * std::f32::consts::LN_10 / (10.0 * n.prof.path_loss_exp);
                    if path.los() {
                        c.los += 1;
                    }
                }
            }
            c.heard = heard.len() as u32;
            if heard.len() >= 3 {
                c.dop = solver::dop(Dim::Plane(p.y as f64), &heard, p.as_dvec3()) as f32;
                c.err = (spread / heard.len() as f32) * c.dop.min(5.0) * 0.6;
            } else if !heard.is_empty() {
                c.err = 4.0;
            }
        }
        Tech::Rfid => {
            let mut best = f32::MIN;
            for ((n, path), prop) in nodes.iter().zip(paths).zip(props) {
                let fwd = match prop {
                    Some(prop) => fdtd_rx_dbm(tech, &n.prof, tag, tag_gain, *prop),
                    None => {
                        let to = (p - n.pos).normalize_or_zero();
                        let g = ants.gain(n.antenna.as_ref(), Tech::Rfid, true, &n.prof, n.fwd, to);
                        n.prof.tx_power_dbm + g + tag.antenna_gain_dbi
                            - solver::fspl_db(path.distance as f64, n.prof.freq_mhz as f64) as f32
                            - n.prof.wall_loss_db * path.wall_eq
                    }
                };
                let margin = fwd - tag.tag_sensitivity_dbm;
                if path.distance <= n.prof.max_range_m {
                    c.signal = c.signal.max(fwd);
                }
                if margin >= 0.0 && path.distance <= n.prof.max_range_m {
                    c.heard += 1;
                    if path.los() {
                        c.los += 1;
                    }
                }
                best = best.max(margin);
            }
            // For RFID "err" encodes the forward-link margin (dB) for colouring.
            if c.heard > 0 {
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
    let tag = tag_profile(&profiles, req.tech);
    // A tag's facing is unknown at a floor point: its omni gain, else isotropic.
    let tag_gain = ants.tag_gain_unaimed(None, &tag);
    // FDTD source: each node's run in the latest whole-scene result, if it is
    // still where it was simulated; the others stay on the ray model.
    let scene = (cov.source == MapSource::Fdtd).then(|| fdtd.scene(req.tech)).flatten();
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
                    let path = occ.path(p, n.pos, None);
                    match req.tech {
                        Tech::Rfid => path,
                        _ => Path { gain_db: ants.gain(n.antenna.as_ref(), req.tech, true, &n.prof, n.fwd, p - n.pos) + tag_gain, ..path },
                    }
                })
                .collect();
            let props: Vec<Option<f32>> = runs.iter().map(|r| r.and_then(|(i, fix)| scene.as_ref()?.link_db_at(i, p).map(|l| l + fix))).collect();
            cells.push(eval(req.tech, p, &nodes, &paths, &props, &tag, tag_gain, ants));
        }
    }
    let source_note = match (cov.source, &scene) {
        (MapSource::Ray, _) => String::new(),
        (MapSource::Fdtd, None) => format!("No whole-scene FDTD run for {} yet: ray model shown", req.tech.label()),
        (MapSource::Fdtd, Some(s)) => {
            let used = runs.iter().filter(|r| r.is_some()).count();
            let scaled = if s.plan.scaled() { format!(" · scaled ×{:.1}", s.plan.scale) } else { String::new() };
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
    cov.summary = match req.tech {
        Tech::Rfid => format!("{} readers: {:.0}% of the floor readable at {:.1} m", nodes.len(), covered * 100.0, req.height),
        _ => format!(
            "{} {}: {:.0}% of the floor has a fix at {:.1} m; predicted error mean {:.2} m, p95 {:.2} m",
            nodes.len(),
            if req.tech == Tech::Uwb { "anchors" } else { "nodes" },
            covered * 100.0,
            req.height,
            mean,
            p95
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
    /// Stop once this share of the free floor is covered (≥3 LOS for UWB/BLE, ≥1 for RFID)…
    pub target_coverage: Option<f32>,
    /// …and the 95th percentile of predicted error is at most this (UWB/BLE).
    pub target_p95_m: Option<f32>,
    /// Anchors that must be in line of sight for a covered point (default 3 for
    /// UWB/BLE, 1 for RFID). 4 leaves one spare to reject a blocked range.
    pub min_los: Option<usize>,
    /// Zones (min, max) where tags must be located: sampled more densely,
    /// weighted higher, and each must reach `target_coverage` on its own.
    pub zones: Vec<(Vec3, Vec3)>,
}

#[derive(Clone, Debug, Default)]
pub struct OptimizeResult {
    /// Chosen mounting points: (position, facing).
    pub chosen: Vec<(Vec3, Vec3)>,
    pub coverage_after: f32,
    pub p95_after: f32,
    pub targets_met: bool,
    /// Covered share of each requested zone (NaN = no walkable floor in it).
    pub zone_coverage: Vec<f32>,
    /// Zones (with walkable floor) that meet the coverage target.
    pub zones_met: usize,
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

/// A usable fix needs reasonable geometry: beyond this DOP a point does not
/// count as covered even with enough anchors in view.
const MAX_DOP: f64 = 5.0;
/// Budgets up to this many devices are planned with beam search.
const BEAM_MAX_ADD: usize = 6;
// Wide enough that 3 anchors are an exhaustive search (one plan scores in
// microseconds).
const BEAM_WIDTH: usize = 2000;

/// Greedy placement: from candidate mounting points, repeatedly add the one
/// that most improves coverage and geometry (DOP), until the targets are met
/// or nothing helps any more.
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
    let Some(prof) = profiles.default_for(infra_kind).cloned() else { return OptimizeResult::default() };
    let (lo, hi) = area(&objects, &devices);
    let size = hi - lo;
    let boxes = object_boxes(&objects);
    // Evaluation points on the walkable floor.
    let ec = (size.x * size.y / 500.0).sqrt().max(0.75);
    let mut pts = Vec::new();
    let mut z = lo.y + ec * 0.5;
    while z < hi.y {
        let mut x = lo.x + ec * 0.5;
        while x < hi.x {
            if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                pts.push(Vec3::new(x, req.height, z));
            }
            x += ec;
        }
        z += ec;
    }
    // Floor coverage is judged on the even grid only; zone points come after.
    let n_floor = pts.len();
    // Zones get a finer grid of their own so a small zone is never judged on
    // one or two points (or none) of the coarse floor grid.
    let zc = (ec * 0.5).max(0.5);
    for (a, b) in &req.zones {
        let mut z = a.z + zc * 0.5;
        while z < b.z {
            let mut x = a.x + zc * 0.5;
            while x < b.x {
                if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                    pts.push(Vec3::new(x, req.height, z));
                }
                x += zc;
            }
            z += zc;
        }
    }
    let in_zone: Vec<Vec<usize>> = pts
        .iter()
        .map(|p| req.zones.iter().enumerate().filter(|(_, (a, b))| p.x >= a.x && p.x <= b.x && p.z >= a.z && p.z <= b.z).map(|(i, _)| i).collect())
        .collect();
    let zone_pts: Vec<usize> = (0..req.zones.len()).map(|i| in_zone.iter().filter(|z| z.contains(&i)).count()).collect();
    // Floor inside a zone counts double: that is where tags must be found.
    const ZONE_WEIGHT: f64 = 2.0;
    // Mounting points closer than this are the same spot (a column face and
    // the wall behind it): keep the first, so two anchors never end up together.
    const SAME_SPOT_M: f32 = 1.0;
    let mut cands: Vec<(Vec3, Vec3)> = Vec::new();
    for c in req.candidates.clone().unwrap_or_else(|| default_candidates(lo, hi, prof.mount_height_m)) {
        if !cands.iter().any(|(p, _)| p.distance(c.0) < SAME_SPOT_M) {
            cands.push(c);
        }
    }
    // How far a device at `a` facing `fwd` reaches towards `p`. For UWB: its
    // transmit power and the antenna gains on the link (its own settings,
    // else the profile's); new devices are planned with the defaults.
    let ants = Antennas { library: &library, applied: &applied };
    let tag_kind = match req.tech {
        Tech::Uwb => DeviceKind::UwbTag,
        Tech::Ble => DeviceKind::BleTag,
        Tech::Rfid => DeviceKind::RfidTag,
    };
    let tag_gain = ants.tag_gain_unaimed(None, &profiles.default_for(tag_kind).cloned().unwrap_or_default());
    let reach = |pr: &Profile, ant: Option<&AntennaChoice>, a: Vec3, fwd: Vec3, p: Vec3| match req.tech {
        Tech::Uwb => radio::uwb_link_range_gain_m(pr, ants.gain(ant, Tech::Uwb, true, pr, fwd, p - a) + tag_gain),
        _ => pr.max_range_m,
    };
    let infra: Vec<(Vec3, Vec3, Profile, Option<AntennaChoice>)> = devices
        .iter()
        .filter(|(_, d, _)| d.enabled && d.kind == infra_kind)
        .map(|(_, d, t)| (t.translation, *t.forward(), profiles.for_device(d), d.radio.antenna.clone()))
        .collect();
    let existing: Vec<Vec3> = infra.iter().map(|(a, ..)| *a).collect();
    let occ = Occluders::build(&parts, &occ_objects);
    // "In view": nothing in the way but thin material (cardboard, wood,
    // glass) that barely slows or weakens the signal. Metal and liquid block.
    let vis = |a: Vec3, reach_m: f32, p: Vec3| {
        (a - p).length() <= reach_m && {
            let path = occ.path(p, a, None);
            path.blocker.is_none() && path.wall_eq <= 0.5
        }
    };
    let ex_vis: Vec<Vec<bool>> = infra.iter().map(|(a, f, pr, an)| pts.iter().map(|p| vis(*a, reach(pr, an.as_ref(), *a, *f, *p), *p)).collect()).collect();
    let cand_vis: Vec<Vec<bool>> = cands.iter().map(|(a, f)| pts.iter().map(|p| vis(*a, reach(&prof, None, *a, *f, *p), *p)).collect()).collect();
    let need = req.min_los.unwrap_or(match req.tech {
        Tech::Uwb | Tech::Ble => 3,
        Tech::Rfid => 1,
    });
    let sigma = match req.tech {
        Tech::Uwb => prof.range_sigma_m.max(0.02) as f64,
        _ => 1.5,
    };
    // Accuracy the plan is judged against (the deploy target, else 30 cm).
    let target_m = req.target_p95_m.unwrap_or(0.3);
    // Score, floor coverage, p95 predicted error and per-zone coverage of a choice.
    #[derive(Clone)]
    struct Eval {
        s: f64,
        cov: f32,
        p95: f32,
        zones: Vec<f32>,
    }
    let eval = |chosen: &[usize]| -> Eval {
        let mut s = 0.0;
        let mut covered = 0usize;
        let mut zone_hit = vec![0usize; req.zones.len()];
        let mut errs: Vec<f32> = Vec::new();
        for (k, p) in pts.iter().enumerate() {
            let w = if in_zone[k].is_empty() { 1.0 } else { ZONE_WEIGHT };
            let mut seen: Vec<DVec3> = Vec::new();
            for (i, a) in existing.iter().enumerate() {
                if ex_vis[i][k] {
                    seen.push(a.as_dvec3());
                }
            }
            for &c in chosen {
                if cand_vis[c][k] {
                    seen.push(cands[c].0.as_dvec3());
                }
            }
            if seen.len() < need {
                // Partial credit so the search can start from zero anchors
                // (one anchor alone never covers a point that needs three).
                s += w * 0.3 * seen.len() as f64 / need as f64;
            } else if need >= 3 {
                // Geometry decides: anchors bunched together or in a line give
                // a huge DOP, so "3 in view" alone is not a usable fix.
                let d = solver::dop(Dim::Plane(p.y as f64), &seen, p.as_dvec3());
                let d = if d.is_finite() { d.min(MAX_DOP * 4.0) } else { MAX_DOP * 4.0 };
                if d <= MAX_DOP {
                    if k < n_floor {
                        covered += 1;
                    }
                    for &z in &in_zone[k] {
                        zone_hit[z] += 1;
                    }
                }
                // Credit by how often a reading here is within the target.
                let q = fix_share((sigma * d) as f32, target_m) as f64;
                s += w * (0.4 + 0.6 * q) + w * 0.25 / (1.0 + d);
                // A 4th anchor makes NLOS rejection possible.
                if seen.len() >= 4 {
                    s += w * 0.15;
                }
                errs.push((sigma * d) as f32);
            } else {
                if k < n_floor {
                    covered += 1;
                }
                for &z in &in_zone[k] {
                    zone_hit[z] += 1;
                }
                s += w;
            }
        }
        errs.sort_by(f32::total_cmp);
        let p95 = if errs.is_empty() { f32::INFINITY } else { errs[((errs.len() - 1) as f32 * 0.95) as usize] };
        let zones = zone_hit.iter().zip(&zone_pts).map(|(&h, &n)| if n == 0 { f32::NAN } else { h as f32 / n as f32 }).collect();
        Eval { s, cov: covered as f32 / n_floor.max(1) as f32, p95, zones }
    };
    // Floor no mounting point can see well enough (deep behind racks, say)
    // stays uncovered however many anchors go up: aim for what is reachable,
    // for the whole floor and for each zone.
    let reach = {
        let all: Vec<usize> = (0..cands.len()).collect();
        eval(&all)
    };
    let target_cov = req.target_coverage.map(|t| t.min(reach.cov * 0.98));
    let zone_target: Vec<f32> = reach.zones.iter().map(|r| if r.is_nan() { 0.0 } else { req.target_coverage.unwrap_or(0.0).min(r * 0.98) }).collect();
    let met = |e: &Eval| {
        target_cov.is_some_and(|t| e.cov >= t)
            && e.zones.iter().zip(&zone_target).all(|(z, t)| z.is_nan() || *z >= *t)
            && (need < 3 || req.target_p95_m.is_none_or(|t| e.p95 <= t))
    };
    let zone_sum = |e: &Eval| e.zones.iter().filter(|z| !z.is_nan()).sum::<f32>();
    let mut chosen: Vec<usize> = Vec::new();
    let mut now = eval(&chosen);
    let cov0 = now.cov;
    // A small budget (say "limit to 3 anchors") is a set to choose jointly:
    // the best 3 are rarely the best 1 plus the best next ones. Beam search
    // keeps the best partial sets at each size instead of one.
    if req.add <= BEAM_MAX_ADD {
        let mut beam: Vec<(Vec<usize>, Eval)> = vec![(Vec::new(), now.clone())];
        let mut done: Option<(Vec<usize>, Eval)> = None;
        for _ in 0..req.add {
            let mut tried = std::collections::HashSet::new();
            let mut next: Vec<(Vec<usize>, Eval)> = Vec::new();
            for (set, _) in &beam {
                for c in 0..cands.len() {
                    if set.contains(&c) {
                        continue;
                    }
                    let mut t = set.clone();
                    t.push(c);
                    let mut key = t.clone();
                    key.sort_unstable();
                    if tried.insert(key) {
                        let r = eval(&t);
                        next.push((t, r));
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            next.sort_by(|a, b| b.1.s.total_cmp(&a.1.s));
            next.truncate(BEAM_WIDTH);
            // The fewest anchors that meet the targets win.
            if let Some(m) = next.iter().find(|(_, e)| met(e)) {
                done = Some(m.clone());
                break;
            }
            beam = next;
        }
        if let Some((set, e)) = done.or_else(|| beam.into_iter().next()) {
            chosen = set;
            now = e;
        }
    }
    for _ in 0..if req.add <= BEAM_MAX_ADD { 0 } else { req.add.min(64) } {
        if met(&now) {
            break;
        }
        let mut best: Option<(Eval, usize)> = None;
        for c in 0..cands.len() {
            if chosen.contains(&c) {
                continue;
            }
            let mut trial = chosen.clone();
            trial.push(c);
            let r = eval(&trial);
            if best.as_ref().is_none_or(|(b, _)| r.s > b.s) {
                best = Some((r, c));
            }
        }
        // Every anchor past the first few must earn its place: at least 1% more
        // floor (or zone) covered, or 1 cm off the predicted p95.
        let worth = |r: &Eval| {
            chosen.len() < need
                || r.cov > now.cov + 0.01
                || zone_sum(r) > zone_sum(&now) + 0.01
                || (now.p95.is_finite() && r.p95 < now.p95 - 0.01)
                || (!now.p95.is_finite() && r.p95.is_finite())
        };
        match best {
            Some((r, c)) if r.s > now.s + 1e-6 && worth(&r) => {
                chosen.push(c);
                now = r;
            }
            _ => break,
        }
    }
    // Swap refinement: greedy picks bunch up (often in the middle, under a
    // ceiling). Try replacing each pick with every unused candidate and keep
    // swaps that improve the score; this spreads anchors toward the corners.
    let swap = |chosen: &mut Vec<usize>, now: &mut Eval| {
        for _ in 0..3 {
            let mut improved = false;
            for k in 0..chosen.len() {
                let mut best: Option<(Eval, usize)> = None;
                for c in 0..cands.len() {
                    if chosen.contains(&c) {
                        continue;
                    }
                    let mut trial = chosen.clone();
                    trial[k] = c;
                    let r = eval(&trial);
                    // Never trade a plan that meets the targets for one that does not.
                    if r.s > now.s + 1e-6 && (met(&r) || !met(now)) && best.as_ref().is_none_or(|(b, _)| r.s > b.s) {
                        best = Some((r, c));
                    }
                }
                if let Some((r, c)) = best {
                    chosen[k] = c;
                    *now = r;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
    };
    // Fewest anchors: greedy adds what helped at the time, and later picks
    // often make earlier ones redundant. Drop every anchor the plan can do
    // without (the least useful first), re-spread the rest, and repeat.
    for _ in 0..4 {
        swap(&mut chosen, &mut now);
        let mut pruned = false;
        loop {
            let was_met = met(&now);
            let ok = |r: &Eval| {
                if was_met {
                    met(r)
                } else {
                    // Short of the target (budget, racks): drop only dead weight.
                    r.cov >= now.cov - 1e-6 && zone_sum(r) >= zone_sum(&now) - 1e-6 && r.p95 <= now.p95 + 0.005
                }
            };
            let mut best: Option<(Eval, usize)> = None;
            for k in 0..chosen.len() {
                let mut trial = chosen.clone();
                trial.remove(k);
                let r = eval(&trial);
                if ok(&r) && best.as_ref().is_none_or(|(b, _)| r.s > b.s) {
                    best = Some((r, k));
                }
            }
            match best {
                Some((r, k)) => {
                    chosen.remove(k);
                    now = r;
                    pruned = true;
                }
                None => break,
            }
        }
        if !pruned {
            break;
        }
    }
    let result = OptimizeResult {
        chosen: chosen.iter().map(|&c| cands[c]).collect(),
        coverage_after: now.cov,
        p95_after: now.p95,
        targets_met: met(&now),
        zone_coverage: now.zones.clone(),
        zones_met: now.zones.iter().zip(&zone_target).filter(|(z, t)| !z.is_nan() && **z >= **t).count(),
    };
    let zones_note = {
        let z: Vec<f32> = now.zones.iter().copied().filter(|z| !z.is_nan()).collect();
        if z.is_empty() { String::new() } else { format!(", zones {}/{} fully covered", z.iter().filter(|c| **c >= 0.999).count(), z.len()) }
    };
    cov.suggestions = result.chosen.iter().map(|(p, _)| *p).collect();
    cov.suggestion_facing = result.chosen.iter().map(|(_, f)| *f).collect();
    cov.suggestion_note = format!(
        "{} suggested {} ({} mounting points): floor with ≥{need} line-of-sight {:.0}% → {:.0}%{}{zones_note}",
        cov.suggestions.len(),
        req.tech.infra().label(),
        cands.len(),
        cov0 * 100.0,
        now.cov * 100.0,
        if need >= 3 && now.p95.is_finite() { format!(", p95 predicted {:.0} cm", now.p95 * 100.0) } else { String::new() }
    );
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
    mut last: Local<Vec<(Vec3, bool, [f32; 3])>>,
    mut due: Local<Option<f64>>,
) {
    if !cov.visible || cov.cells.is_empty() {
        last.clear();
        *due = None;
        return;
    }
    // Position, on/off and radio settings (TX power, range) of each device.
    let now: Vec<(Vec3, bool, [f32; 3])> = devices
        .iter()
        .filter(|(d, _)| d.kind == cov.tech.infra())
        .map(|(d, t)| {
            let p = profiles.resolve(&d.profile, d.kind);
            (t.translation, d.enabled, [p.uwb_tx_dbm_mhz, p.tx_power_dbm, p.max_range_m])
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

    /// A dipole-like pattern (sin² θ, 2.15 dBi peak) on a 5° grid.
    fn dipole(axis_forward: bool) -> FdtdAntenna {
        let d = (0..=36)
            .map(|ti| {
                let s = ((ti as f32) * 5.0).to_radians().sin();
                vec![2.15 + 10.0 * (s * s).max(1e-4).log10(); 73]
            })
            .collect();
        FdtdAntenna { tech: Tech::Uwb, label: String::new(), source: Default::default(), theta_step: 5.0, phi_step: 5.0, d, mismatch_db: -0.5, axis_forward }
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
