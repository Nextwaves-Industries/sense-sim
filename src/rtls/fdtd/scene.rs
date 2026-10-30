//! Whole-scene coverage: the whole room (every placed object voxelised, a
//! concrete floor slab) simulated once per transmitter (anchor, BLE node,
//! RFID reader), and the field at tag height turned into link gain for the
//! floor map, field planes for the viewport and a fit of the ray model's
//! material losses.
//!
//! The room is far too many wavelengths across for UWB (and usually BLE) at
//! the real frequency, so those runs are frequency-scaled: the highest
//! frequency that fits the cell budget, with the materials' εr and σ taken at
//! the real band. Wall attenuation per metre (≈ σ·η/2 in low-loss media) does
//! not depend on frequency and received power uses the real wavelength, so
//! shadowing and the local-mean level hold; fading and diffraction happen at
//! the simulated wavelength, which is why the link gain is averaged over a
//! wavelength and over the channel.

use std::path::Path;

use anyhow::{Result, bail};
use bevy::math::{Vec2, Vec3};
use bevy::prelude::Entity;

use super::Slice;
use super::antenna::{Band, Quality};
use super::csx::{Bx, C0, Dielectric, Dump, DumpKind, P3, Sim, auto_mesh};
use super::post;
use super::region::{SceneObj, Voxels, electrical, to_sim, to_world, voxelise};
use crate::rtls::planning::Tech;
use crate::rtls::radio::Material;

/// Free-space wave impedance (Ω).
const ETA0: f64 = 376.730_313_668;
/// Directivity assumed for the simulated sources: a half-wave dipole, and a
/// dipole λ/4 in front of a 0.6 λ reflector.
pub const DIPOLE_DBI: f32 = 2.15;
pub const REFLECTOR_DBI: f32 = 7.0;
/// Absorbing boundary and the cells around it (openEMS PML_8 plus margin).
const PAD_CELLS: usize = 10;
/// Frequencies sampled over the channel, averaged (frequency diversity).
const N_FREQS: usize = 5;

/// One transmitter of the run.
#[derive(Clone, Debug)]
pub struct Tx {
    pub entity: Entity,
    pub name: String,
    pub pos: Vec3,
    pub fwd: Vec3,
    /// Aimed antenna (patch, directional): simulated with a reflector behind.
    pub directional: bool,
    /// The device's real antenna peak gain minus the simulated one (dB).
    pub gain_fix_db: f32,
    /// Added to the link gain (dB) to get the received power at the tag (dBm).
    pub rx_offset_db: f32,
    /// Ray model of this device, for the calibration fit: dB per typical
    /// wall, and the BLE path-loss exponent (`None`: free-space loss).
    pub wall_db: f32,
    pub exponent: Option<f32>,
}

#[derive(Clone)]
pub struct SceneRequest {
    pub tech: Tech,
    pub band: Band,
    pub quality: Quality,
    pub tag_height: f32,
    /// Floor area (world x, z).
    pub lo: Vec2,
    pub hi: Vec2,
    pub objects: Vec<SceneObj>,
    pub txs: Vec<Tx>,
}

/// Size, frequency and cost of a run, before anything is voxelised.
#[derive(Clone, Debug, Default)]
pub struct ScenePlan {
    pub f_real: f64,
    pub f_sim: f64,
    /// f_real / f_sim (1 = the real frequency).
    pub scale: f64,
    pub cell: f64,
    /// Simulated box in sim coordinates, on the cell grid (PML outside it).
    pub lo: P3,
    pub hi: P3,
    /// Cells per run (PML included).
    pub cells: u64,
    pub memory: u64,
    pub steps: u64,
    pub seconds_per_tx: f64,
    /// Heights of the horizontal field planes (world y).
    pub levels: Vec<f32>,
    pub budget: f64,
}

impl ScenePlan {
    pub fn scaled(&self) -> bool {
        self.scale > 1.001
    }
    /// Frequencies dumped (sim): the channel, scaled.
    pub fn freqs(&self, band: Band) -> Vec<f64> {
        let (a, b) = band.channel();
        post::linspace(a / self.scale, b / self.scale, N_FREQS)
    }
    pub fn lambda_sim(&self) -> f64 {
        C0 / self.f_sim
    }
}

fn cells_per_lambda(q: Quality) -> f64 {
    match q {
        Quality::Fast => 10.0,
        Quality::Normal => 12.0,
        Quality::Fine => 15.0,
    }
}

/// Cells per run the quality allows (memory ≈ 100 B a cell).
pub fn budget(q: Quality) -> f64 {
    match q {
        Quality::Fast => 30e6,
        Quality::Normal => 60e6,
        Quality::Fine => 120e6,
    }
}

/// Top of the simulated room: above the highest object and transmitter.
fn top_of(req: &SceneRequest) -> f32 {
    let inside = |o: &SceneObj| o.max.x > req.lo.x && o.min.x < req.hi.x && o.max.z > req.lo.y && o.min.z < req.hi.y;
    let objs = req.objects.iter().filter(|o| inside(o)).map(|o| o.max.y).fold(0.0f32, f32::max);
    let txs = req.txs.iter().map(|t| t.pos.y).fold(0.0f32, f32::max);
    (objs.max(txs).max(req.tag_height) + 0.5).clamp(2.0, 6.0)
}

const FLOOR_BOTTOM: f32 = -0.15;

pub fn plan(req: &SceneRequest) -> ScenePlan {
    let f_real = req.band.center();
    let top = top_of(req);
    let size = Vec3::new(req.hi.x - req.lo.x, top - FLOOR_BOTTOM, req.hi.y - req.lo.y).max(Vec3::splat(0.1));
    let n = cells_per_lambda(req.quality);
    // Cell for a simulation at f: λ/n at the top of the pulse (f · 1.15).
    let cell_at = |f: f64| C0 / (f * 1.15) / n;
    let cells_at = |f: f64| -> f64 { (0..3).map(|k| (size[k] as f64 / cell_at(f)).ceil() + 2.0 * PAD_CELLS as f64).product() };
    let budget = budget(req.quality);
    let mut f_sim = f_real;
    if cells_at(f_sim) > budget {
        // Cells grow as f³: jump close, then step down until it fits.
        f_sim = f_real * (budget / cells_at(f_real)).cbrt();
        while cells_at(f_sim) > budget && f_sim > 1e6 {
            f_sim *= 0.98;
        }
    }
    let cell = cell_at(f_sim);
    let wlo = Vec3::new(req.lo.x, FLOOR_BOTTOM, req.lo.y);
    let lo = to_sim(wlo);
    let dims: [f64; 3] = std::array::from_fn(|k| (size[[0, 2, 1][k]] as f64 / cell).ceil());
    let hi: P3 = std::array::from_fn(|k| lo[k] + dims[k] * cell);
    let cells = cells_at(f_sim) as u64;
    // Time: the pulse plus six crossings of the room (reverberation).
    let diag = size.length() as f64;
    let dt = cell / (C0 * 3f64.sqrt());
    let steps = ((6.0 * diag / C0 + 3.0 / (0.15 * f_sim)) / dt).ceil() as u64;
    let mut levels: Vec<f32> = (1..).map(|i| i as f32 * 0.5).take_while(|y| *y <= (top - 0.2).min(3.0)).collect();
    if !levels.iter().any(|y| (y - req.tag_height).abs() < 0.1) {
        levels.push(req.tag_height);
        levels.sort_by(f32::total_cmp);
    }
    ScenePlan {
        f_real,
        f_sim,
        scale: f_real / f_sim,
        cell,
        lo,
        hi,
        cells,
        memory: cells * 100,
        steps,
        // openEMS on a desktop: ~150 MC/s with all cores.
        seconds_per_tx: cells as f64 * steps as f64 / 150e6,
        levels,
        budget,
    }
}

/// Grid-snapped coordinate along sim axis `k`.
fn snap(plan: &ScenePlan, k: usize, v: f64) -> f64 {
    plan.lo[k] + ((v - plan.lo[k]) / plan.cell).round() * plan.cell
}

/// A box on whole cells that contains `b`.
fn snap_out(plan: &ScenePlan, b: Bx) -> Bx {
    let c = plan.cell;
    Bx::new(
        std::array::from_fn(|k| plan.lo[k] + ((b.min[k] - plan.lo[k]) / c).floor() * c),
        std::array::from_fn(|k| plan.lo[k] + ((b.max[k] - plan.lo[k]) / c).ceil() * c),
    )
}

/// Removes `keep` from x-run boxes (one cell thick in y and z, as `voxelise`
/// makes them): the antenna must not sit inside a wall or a shelf.
fn carve(boxes: &[Bx], keep: &Bx) -> Vec<Bx> {
    let mut out = Vec::with_capacity(boxes.len());
    for b in boxes {
        let overlaps = (0..3).all(|k| b.min[k] < keep.max[k] - 1e-9 && keep.min[k] < b.max[k] - 1e-9);
        if !overlaps {
            out.push(*b);
            continue;
        }
        if b.min[0] < keep.min[0] - 1e-9 {
            out.push(Bx { min: b.min, max: [keep.min[0], b.max[1], b.max[2]] });
        }
        if b.max[0] > keep.max[0] + 1e-9 {
            out.push(Bx { min: [keep.max[0], b.min[1], b.min[2]], max: b.max });
        }
    }
    out
}

/// The horizontal sim axis (0 or 1) and sign `fwd` points along most.
fn facing(fwd: Vec3) -> (usize, f64) {
    let s = to_sim(fwd);
    if s[0].abs() >= s[1].abs() { (0, s[0].signum()) } else { (1, s[1].signum()) }
}

/// Source on the cell grid (no finer cells, so the time step stays that of
/// the room): a vertical strip dipole two cells wide with a two-cell feed gap
/// (the port probes sit on its centre, which must be a grid line too), about
/// λ/2 long, and for aimed devices a reflector λ/4 behind it. Arms, port, and
/// the box kept clear of scene geometry around it.
fn source(plan: &ScenePlan, tx: &Tx) -> (Vec<Bx>, super::csx::Port, Bx) {
    let c = plan.cell;
    let lambda = plan.lambda_sim();
    let p = to_sim(tx.pos);
    let (ax, sign) = facing(tx.fwd);
    // Strip across the facing direction, in the plane through the device.
    let w_axis = 1 - ax;
    let x0 = snap(plan, w_axis, p[w_axis] - c);
    let sheet = snap(plan, ax, p[ax]);
    let z_gap = snap(plan, 2, p[2] - c);
    let arm = ((0.47 * lambda - 2.0 * c) / 2.0 / c).round().max(1.0) * c;
    let bx = |z0: f64, z1: f64| {
        let mut a = [0.0; 3];
        let mut b = [0.0; 3];
        a[w_axis] = x0;
        b[w_axis] = x0 + 2.0 * c;
        a[ax] = sheet;
        b[ax] = sheet;
        a[2] = z0;
        b[2] = z1;
        Bx::new(a, b)
    };
    let mut metal = vec![bx(z_gap + 2.0 * c, z_gap + 2.0 * c + arm), bx(z_gap - arm, z_gap)];
    let port = super::csx::Port { nr: 1, gap: bx(z_gap, z_gap + 2.0 * c), dir: 2, r_ohm: 73.0, excite: true };
    let mut keep = bx(z_gap - arm, z_gap + 2.0 * c + arm);
    let margin = (lambda / 10.0).max(c);
    for k in 0..3 {
        keep.min[k] -= margin;
        keep.max[k] += margin;
    }
    if tx.directional {
        let back = snap(plan, ax, p[ax] - sign * lambda / 4.0);
        let half = (0.3 * lambda / c).round().max(1.0) * c;
        let mut a = [0.0; 3];
        let mut b = [0.0; 3];
        a[w_axis] = x0 + c - half;
        b[w_axis] = x0 + c + half;
        a[ax] = back;
        b[ax] = back;
        a[2] = z_gap + c - half;
        b[2] = z_gap + c + half;
        let r = Bx::new(a, b);
        metal.push(r);
        for k in 0..3 {
            keep.min[k] = keep.min[k].min(r.min[k] - c);
            keep.max[k] = keep.max[k].max(r.max[k] + c);
        }
    }
    (metal, port, snap_out(plan, keep))
}

/// Simulated domain, PML included.
fn domain(plan: &ScenePlan) -> Bx {
    let pad = PAD_CELLS as f64 * plan.cell;
    Bx::new(plan.lo.map(|v| v - pad), plan.hi.map(|v| v + pad))
}

/// Voxelises the room once and makes one simulation per transmitter.
pub fn build(req: &SceneRequest, plan: &ScenePlan) -> Result<(Vec<Sim>, Voxels)> {
    if req.txs.is_empty() {
        bail!("no {} placed: add some first", req.tech.infra().label());
    }
    let (wlo, whi) = {
        let (a, b) = (to_world(plan.lo), to_world(plan.hi));
        (a.min(b), a.max(b))
    };
    let inside: Vec<&SceneObj> = req.objects.iter().filter(|o| o.min.cmplt(whi).all() && wlo.cmplt(o.max).all()).collect();
    let vox = voxelise(&inside, plan.lo, plan.hi, plan.cell, plan.f_real, 400_000_000)?;
    // Concrete floor slab, unless the scene brings its own floor.
    let floor = (electrical(Material::Concrete, plan.f_real), to_sim(Vec3::new(wlo.x, -0.12, wlo.z)), to_sim(Vec3::new(whi.x, 0.0, whi.z)));
    let freqs = plan.freqs(req.band);
    let f = plan.f_sim;
    let steps = plan.steps;
    let mut sims = Vec::with_capacity(req.txs.len());
    for tx in &req.txs {
        let (arms, port, keep) = source(plan, tx);
        let mut sim = Sim { f0: f, fc: 0.15 * f, end_criteria: 1e-3, max_timesteps: steps.max(5000), ..Default::default() };
        sim.metal = carve(&vox.metal, &keep);
        sim.metal.extend(arms);
        sim.dielectrics = vox.dielectrics.iter().map(|d| Dielectric { boxes: carve(&d.boxes, &keep), ..d.clone() }).collect();
        if let (Some((eps_r, kappa)), a, b) = floor {
            sim.dielectrics.push(Dielectric { name: "floor".into(), eps_r, kappa, boxes: vec![snap_out(plan, Bx::new(a, b))] });
        }
        sim.ports.push(port);
        let (lo, hi) = (plan.lo, plan.hi);
        for (i, y) in plan.levels.iter().enumerate() {
            let z = snap(plan, 2, *y as f64);
            sim.dumps.push(Dump { name: format!("level{i}"), kind: DumpKind::EFreq, freqs: freqs.clone(), region: Bx::new([lo[0], lo[1], z], [hi[0], hi[1], z]) });
        }
        let p = to_sim(tx.pos);
        let ys = snap(plan, 1, p[1]);
        let xs = snap(plan, 0, p[0]);
        sim.dumps.push(Dump { name: "vx".into(), kind: DumpKind::EFreq, freqs: freqs.clone(), region: Bx::new([lo[0], ys, lo[2]], [hi[0], ys, hi[2]]) });
        sim.dumps.push(Dump { name: "vy".into(), kind: DumpKind::EFreq, freqs: freqs.clone(), region: Bx::new([xs, lo[1], lo[2]], [xs, hi[1], hi[2]]) });
        sim.mesh = auto_mesh(&sim, domain(plan), plan.cell);
        sims.push(sim);
    }
    Ok((sims, vox))
}

/// Cache key of one transmitter's run: geometry, position, antenna and grid.
pub fn tx_key(geometry: u64, plan: &ScenePlan, tx: &Tx) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    geometry.hash(&mut h);
    plan.f_sim.to_bits().hash(&mut h);
    plan.cell.to_bits().hash(&mut h);
    plan.levels.iter().map(|v| v.to_bits()).collect::<Vec<_>>().hash(&mut h);
    to_sim(tx.pos).map(|v| (v / plan.cell * 4.0).round() as i64).hash(&mut h);
    tx.directional.hash(&mut h);
    if tx.directional {
        let (a, s) = facing(tx.fwd);
        (a, s > 0.0).hash(&mut h);
    }
    h.finish()
}

// ---------------------------------------------------------------------------
// Results

/// A field plane on a uniform grid: link gain (dB) from the transmitter's
/// port to an isotropic receiver, `NEG_INFINITY` where unknown.
#[derive(Clone, Debug, Default)]
pub struct Plane2 {
    /// The two sim axes it spans and the sim coordinate it is fixed at.
    pub axes: [usize; 2],
    pub fixed: f64,
    pub u0: f64,
    pub v0: f64,
    pub step: f64,
    pub nu: usize,
    pub nv: usize,
    /// `db[i + nu·j]` at (u0 + i·step, v0 + j·step).
    pub db: Vec<f32>,
}

impl Plane2 {
    /// Bilinear sample at sim coordinates (u, v).
    pub fn at(&self, u: f64, v: f64) -> Option<f32> {
        let x = (u - self.u0) / self.step;
        let y = (v - self.v0) / self.step;
        if self.nu < 2 || self.nv < 2 || x < 0.0 || y < 0.0 || x > (self.nu - 1) as f64 || y > (self.nv - 1) as f64 {
            return None;
        }
        let (i, j) = ((x as usize).min(self.nu - 2), (y as usize).min(self.nv - 2));
        let (fx, fy) = ((x - i as f64) as f32, (y - j as f64) as f32);
        let g = |a: usize, b: usize| self.db[a + self.nu * b];
        let v = g(i, j) * (1.0 - fx) * (1.0 - fy) + g(i + 1, j) * fx * (1.0 - fy) + g(i, j + 1) * (1.0 - fx) * fy + g(i + 1, j + 1) * fx * fy;
        v.is_finite().then_some(v)
    }

    /// As a viewport slice; `value` maps link gain (dB) to the 0..1 colour scale.
    pub fn to_slice(&self, value: impl Fn(f32) -> f32) -> Slice {
        let fixed_axis = 3 - self.axes[0] - self.axes[1];
        let mut c = [0.0; 3];
        c[self.axes[0]] = self.u0;
        c[self.axes[1]] = self.v0;
        c[fixed_axis] = self.fixed;
        let mut eu = [0.0; 3];
        eu[self.axes[0]] = (self.nu - 1) as f64 * self.step;
        let mut ev = [0.0; 3];
        ev[self.axes[1]] = (self.nv - 1) as f64 * self.step;
        Slice {
            nu: self.nu,
            nv: self.nv,
            u: (self.u0, self.u0 + eu[self.axes[0]]),
            v: (self.v0, self.v0 + ev[self.axes[1]]),
            corner: to_world(c),
            du: to_world(eu),
            dv: to_world(ev),
            abs: self.db.iter().map(|d| if d.is_finite() { value(*d) } else { 0.0 }).collect(),
            frames: Vec::new(),
        }
    }
}

/// Link gain (linear) to an isotropic receiver at the real wavelength, from
/// |E|² and the power the port accepted (same spectral units).
pub fn link_gain(e2: f64, p_acc: f64, f_real: f64) -> f64 {
    let lambda = C0 / f_real;
    e2 / (2.0 * ETA0) * lambda * lambda / (4.0 * std::f64::consts::PI) / p_acc.max(1e-300)
}

/// Value of a rectilinear axis at fractional index position.
fn locate(c: &[f64], x: f64) -> Option<(usize, f64)> {
    if c.len() < 2 || x < c[0] - 1e-9 || x > c[c.len() - 1] + 1e-9 {
        return None;
    }
    let i = c.partition_point(|v| *v <= x).clamp(1, c.len() - 1) - 1;
    let t = ((x - c[i]) / (c[i + 1] - c[i])).clamp(0.0, 1.0);
    Some((i, t))
}

/// Separable moving average over `r` cells either side (NaN-free input).
fn box_blur(data: &mut [f64], nu: usize, nv: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut tmp = vec![0.0; data.len()];
    for j in 0..nv {
        for i in 0..nu {
            let (a, b) = (i.saturating_sub(r), (i + r).min(nu - 1));
            tmp[i + nu * j] = (a..=b).map(|k| data[k + nu * j]).sum::<f64>() / (b - a + 1) as f64;
        }
    }
    for j in 0..nv {
        for i in 0..nu {
            let (a, b) = (j.saturating_sub(r), (j + r).min(nv - 1));
            data[i + nu * j] = (a..=b).map(|k| tmp[i + nu * k]).sum::<f64>() / (b - a + 1) as f64;
        }
    }
}

/// Resamples power on a rectilinear plane to a uniform grid of `step` and
/// averages it over `window` (local mean), in dB.
pub fn local_mean_plane(axes: [usize; 2], fixed: f64, cu: &[f64], cv: &[f64], power: &[f64], step: f64, window: f64) -> Plane2 {
    let (u0, v0) = (cu[0], cv[0]);
    let nu = (((cu[cu.len() - 1] - u0) / step).floor() as usize + 1).max(2);
    let nv = (((cv[cv.len() - 1] - v0) / step).floor() as usize + 1).max(2);
    let n_src = cu.len();
    let mut lin = vec![0.0; nu * nv];
    for j in 0..nv {
        let Some((jv, tv)) = locate(cv, v0 + j as f64 * step) else { continue };
        for i in 0..nu {
            let Some((iu, tu)) = locate(cu, u0 + i as f64 * step) else { continue };
            let g = |a: usize, b: usize| power[a + n_src * b];
            lin[i + nu * j] = g(iu, jv) * (1.0 - tu) * (1.0 - tv) + g(iu + 1, jv) * tu * (1.0 - tv) + g(iu, jv + 1) * (1.0 - tu) * tv + g(iu + 1, jv + 1) * tu * tv;
        }
    }
    box_blur(&mut lin, nu, nv, (window / 2.0 / step).round() as usize);
    let db = lin.iter().map(|p| if *p > 0.0 { (10.0 * p.log10()) as f32 } else { f32::NEG_INFINITY }).collect();
    Plane2 { axes, fixed, u0, v0, step, nu, nv, db }
}

/// Reads one dumped plane of a run as link gain: |E|² over the channel
/// samples, each over the port's accepted power at that frequency.
pub fn read_link_plane(dir: &Path, name: &str, freqs: &[f64], p_acc: &[f64], plan: &ScenePlan) -> Result<Plane2> {
    let mut acc: Option<(post::Grid, Vec<f64>)> = None;
    for (f, p) in freqs.iter().zip(p_acc) {
        let g = post::read_vtr(&post::fd_file(dir, name, *f, "abs"))?;
        let e2: Vec<f64> = (0..g.len()).map(|k| g.at(k).iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / p.max(1e-300)).collect();
        match &mut acc {
            Some((g0, sum)) if g0.len() == g.len() => sum.iter_mut().zip(e2).for_each(|(s, v)| *s += v),
            Some(_) => bail!("{name}: planes differ between frequencies"),
            None => acc = Some((g, e2)),
        }
    }
    let Some((g, sum)) = acc else { bail!("{name}: no frequencies") };
    let lambda = C0 / plan.f_real;
    let k = 1.0 / (2.0 * ETA0) * lambda * lambda / (4.0 * std::f64::consts::PI) / freqs.len() as f64;
    let power: Vec<f64> = sum.iter().map(|v| v * k).collect();
    let coords = [&g.x, &g.y, &g.z];
    let axes: Vec<usize> = (0..3).filter(|a| coords[*a].len() > 1).collect();
    if axes.len() != 2 {
        bail!("{name} is not a plane");
    }
    let fixed = coords[3 - axes[0] - axes[1]][0];
    // One wavelength of the simulation smooths its fading pattern away.
    let window = plan.lambda_sim().max(2.0 * plan.cell);
    Ok(local_mean_plane([axes[0], axes[1]], fixed, coords[axes[0]], coords[axes[1]], &power, plan.cell, window))
}

/// One transmitter's field.
#[derive(Clone, Debug)]
pub struct TxField {
    pub tx: Tx,
    pub key: u64,
    /// Horizontal planes at `SceneResult::levels`.
    pub levels: Vec<Plane2>,
    /// Vertical cuts through the transmitter.
    pub cuts: Vec<Plane2>,
}

pub struct SceneResult {
    pub tech: Tech,
    pub band: Band,
    pub quality: Quality,
    pub plan: ScenePlan,
    pub tag_height: f32,
    pub per_tx: Vec<TxField>,
    /// Occupancy only (solver boxes dropped), to skip points inside objects.
    pub voxels: Voxels,
    /// Runs simulated this time (the others were reused).
    pub simulated: usize,
    pub seconds: f64,
    pub dir: std::path::PathBuf,
}

impl SceneResult {
    pub fn region(&self) -> (Vec3, Vec3) {
        let (a, b) = (to_world(self.plan.lo), to_world(self.plan.hi));
        (a.min(b), a.max(b))
    }

    /// Link gain (dB) from transmitter `i` to an isotropic receiver at `p`,
    /// from the horizontal plane nearest its height.
    pub fn link_db_at(&self, i: usize, p: Vec3) -> Option<f32> {
        let t = self.per_tx.get(i)?;
        let k = self.plan.levels.iter().enumerate().min_by(|a, b| (a.1 - p.y).abs().total_cmp(&(b.1 - p.y).abs()))?.0;
        let s = to_sim(p);
        t.levels.get(k)?.at(s[0], s[1])
    }

    pub fn index_of(&self, e: Entity) -> Option<usize> {
        self.per_tx.iter().position(|t| t.tx.entity == e)
    }

    /// Received power (dBm) planes for the viewport: every level (or only the
    /// one nearest the tag height) and the vertical cuts, for one transmitter
    /// or the strongest of all; `value` maps dBm to the colour scale.
    pub fn planes(&self, tx: Option<usize>, all_levels: bool, value: impl Fn(f32) -> f32 + Copy) -> Vec<Slice> {
        let pick: Vec<&TxField> = match tx {
            Some(i) => self.per_tx.get(i).into_iter().collect(),
            None => self.per_tx.iter().collect(),
        };
        let Some(first) = pick.first() else { return Vec::new() };
        let tag_k = self.plan.levels.iter().enumerate().min_by(|a, b| (a.1 - self.tag_height).abs().total_cmp(&(b.1 - self.tag_height).abs())).map(|x| x.0);
        let mut out = Vec::new();
        for k in 0..first.levels.len() {
            if !all_levels && Some(k) != tag_k {
                continue;
            }
            // Strongest received power over the transmitters shown.
            let mut p = first.levels[k].clone();
            for (n, v) in p.db.iter_mut().enumerate() {
                *v = pick.iter().filter_map(|t| t.levels.get(k).and_then(|l| l.db.get(n)).map(|d| d + t.tx.rx_offset_db)).fold(f32::NEG_INFINITY, f32::max);
            }
            out.push(p.to_slice(value));
        }
        for t in &pick {
            for c in &t.cuts {
                let mut c = c.clone();
                c.db.iter_mut().for_each(|d| *d += t.tx.rx_offset_db);
                out.push(c.to_slice(value));
            }
        }
        out
    }
}

/// Reads a finished run: accepted power, every plane, then deletes the dumps.
pub fn read_tx(dir: &Path, plan: &ScenePlan, band: Band, tx: Tx, key: u64) -> Result<TxField> {
    let freqs = plan.freqs(band);
    let p_acc = post::port_accepted_power(dir, 1, 73.0, &freqs)?;
    if p_acc.iter().any(|p| !(*p > 0.0)) {
        bail!("{}: no power accepted by the source", tx.name);
    }
    let mut levels = Vec::new();
    for i in 0..plan.levels.len() {
        levels.push(read_link_plane(dir, &format!("level{i}"), &freqs, &p_acc, plan)?);
    }
    let mut cuts = Vec::new();
    for n in ["vx", "vy"] {
        cuts.push(read_link_plane(dir, n, &freqs, &p_acc, plan)?);
    }
    // The dumps are large; the planes above are all that is kept.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "vtr") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(TxField { tx, key, levels, cuts })
}

// ---------------------------------------------------------------------------
// Ray-model calibration

/// One floor point seen from one transmitter.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Propagation loss FDTD measured (dB, isotropic ends).
    pub loss: f32,
    /// The ray model's loss without walls (free space / log-distance).
    pub base: f32,
    /// dB per typical wall in that model.
    pub wall_db: f32,
    /// Uncalibrated weight crossed per material (`Path::per_material`).
    pub w: [f32; 7],
}

#[derive(Clone, Debug)]
pub struct MatFit {
    pub material: Material,
    pub samples: usize,
    pub current: f32,
    pub fitted: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Fit {
    pub mats: Vec<MatFit>,
    /// Mean difference FDTD − ray model left over (dB), e.g. multipath gain
    /// indoors; shown, not applied.
    pub offset_db: f32,
    pub rms_before: f32,
    pub rms_after: f32,
    pub samples: usize,
}

/// Fewest samples crossing a material for it to be fitted.
pub const MIN_SAMPLES: usize = 30;

/// Solves `A x = b` (small, dense) by Gaussian elimination with pivoting.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for c in 0..n {
        let p = (c..n).max_by(|x, y| a[*x][c].abs().total_cmp(&a[*y][c].abs()))?;
        if a[p][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let k = a[r][c] / a[c][c];
            for j in c..n {
                a[r][j] -= k * a[c][j];
            }
            b[r] -= k * b[c];
        }
    }
    let mut x = vec![0.0; n];
    for c in (0..n).rev() {
        x[c] = (b[c] - (c + 1..n).map(|j| a[c][j] * x[j]).sum::<f64>()) / a[c][c];
    }
    Some(x)
}

/// Fits the ray model's per-material loss scales to FDTD:
/// `loss − base ≈ c + wall_db · Σ w_m · s_m`, least squares with a weak pull
/// of each s_m towards 1 and s_m ≥ 0. Materials crossed by fewer than
/// `MIN_SAMPLES` samples keep their `current` scale.
pub fn fit_calibration(samples: &[Sample], current: [f32; 7]) -> Option<Fit> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let counts: Vec<usize> = (0..7).map(|m| samples.iter().filter(|s| s.w[m] > 0.0).count()).collect();
    let mut free: Vec<usize> = (0..7).filter(|m| counts[*m] >= MIN_SAMPLES && Material::ALL[*m] != Material::Metal).collect();
    // Typical loss one crossing of each material adds (dB at scale 1).
    let lever: Vec<f64> = (0..7)
        .map(|m| {
            let l: Vec<f64> = samples.iter().filter(|s| s.w[m] > 0.0).map(|s| (s.wall_db * s.w[m]) as f64).collect();
            l.iter().sum::<f64>() / l.len().max(1) as f64
        })
        .collect();
    let residual = |s: &Sample, scales: &[f32; 7]| -> f64 { (s.loss - s.base) as f64 - (0..7).map(|m| s.wall_db as f64 * s.w[m] as f64 * scales[m] as f64).sum::<f64>() };
    let mut scales = current;
    // Active set: refit without materials driven negative (fixed at 0).
    for _ in 0..8 {
        let n = 1 + free.len();
        let mut ata = vec![vec![0.0; n]; n];
        let mut atb = vec![0.0; n];
        let mut add = |row: &[f64], b: f64| {
            for i in 0..n {
                atb[i] += row[i] * b;
                for j in 0..n {
                    ata[i][j] += row[i] * row[j];
                }
            }
        };
        let mut fixed = scales;
        free.iter().for_each(|m| fixed[*m] = 0.0);
        for s in samples {
            let mut row = vec![1.0];
            row.extend(free.iter().map(|m| s.wall_db as f64 * s.w[*m] as f64));
            add(&row, residual(s, &fixed));
        }
        // Prior: as much as five typical samples of that material saying "scale 1".
        for (i, m) in free.iter().enumerate() {
            let w = 5f64.sqrt() * lever[*m];
            let mut row = vec![0.0; n];
            row[1 + i] = w;
            add(&row, w);
        }
        let x = solve(ata, atb)?;
        for (i, m) in free.iter().enumerate() {
            scales[*m] = x[1 + i] as f32;
        }
        let neg: Vec<usize> = free.iter().copied().filter(|m| scales[*m] < 0.0).collect();
        if neg.is_empty() {
            break;
        }
        for m in &neg {
            scales[*m] = 0.0;
        }
        free.retain(|m| !neg.contains(m));
    }
    let rms = |sc: &[f32; 7]| -> (f32, f32) {
        let r: Vec<f64> = samples.iter().map(|s| residual(s, sc)).collect();
        let mean = r.iter().sum::<f64>() / r.len() as f64;
        let var = r.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / r.len() as f64;
        (mean as f32, var.sqrt() as f32)
    };
    let (_, before) = rms(&current);
    let (offset, after) = rms(&scales);
    let mats = (0..7)
        .filter(|m| counts[*m] >= MIN_SAMPLES && Material::ALL[*m] != Material::Metal)
        .map(|m| MatFit { material: Material::ALL[m], samples: counts[m], current: current[m], fitted: scales[m] })
        .collect();
    Some(Fit { mats, offset_db: offset, rms_before: before, rms_after: after, samples: samples.len() })
}

/// Directivity (dBi) of the simulated source towards `dir`: a vertical
/// dipole (sin² θ). Used to take the source pattern out of the FDTD loss.
pub fn source_gain_db(dir: Vec3) -> f32 {
    let s = dir.normalize_or_zero();
    let sin2 = (1.0 - s.y * s.y).max(0.01);
    DIPOLE_DBI + 10.0 * sin2.log10()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn wall(x0: f32, x1: f32, z0: f32, z1: f32, h: f32, m: Material) -> SceneObj {
        let (a, b) = (Vec3::new(x0, 0.0, z0), Vec3::new(x1, h, z1));
        let v = |x: bool, y: bool, z: bool| [if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }];
        let pos: Vec<[f32; 3]> = (0..8).map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0)).collect();
        let idx = [0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5, 0, 4, 5, 0, 5, 1, 2, 3, 7, 2, 7, 6, 0, 2, 6, 0, 6, 4, 1, 5, 7, 1, 7, 3];
        SceneObj { name: "wall".into(), material: m, weight: m.weight(), parts: vec![(Arc::new(crate::bvh::Bvh::new(&pos, &idx)), bevy::math::Affine3A::IDENTITY)], min: a, max: b }
    }

    fn tx(pos: Vec3) -> Tx {
        Tx { entity: Entity::PLACEHOLDER, name: "a1".into(), pos, fwd: Vec3::NEG_Z, directional: false, gain_fix_db: 0.0, rx_offset_db: 0.0, wall_db: 6.0, exponent: None }
    }

    fn request(band: Band, lo: Vec2, hi: Vec2) -> SceneRequest {
        SceneRequest { tech: Tech::Rfid, band, quality: Quality::Fast, tag_height: 1.2, lo, hi, objects: vec![wall(-0.1, 0.1, -3.0, 3.0, 3.0, Material::Concrete)], txs: vec![tx(Vec3::new(-2.0, 2.0, 0.0))] }
    }

    #[test]
    fn plan_scales_only_what_does_not_fit() {
        // The warehouse sample: 14.5 × 12 m plus the 1 m margin.
        let (lo, hi) = (Vec2::new(-8.25, -7.0), Vec2::new(8.25, 7.0));
        let rfid = plan(&request(Band::Rfid, Vec2::new(-4.0, -3.5), Vec2::new(4.0, 3.5)));
        assert!(!rfid.scaled(), "small room at UHF runs at the real frequency, {} cells", rfid.cells);
        for band in [Band::Ble, Band::Uwb5, Band::Uwb9, Band::Rfid] {
            let p = plan(&request(band, lo, hi));
            assert!(p.cells as f64 <= p.budget, "{band:?}: {} cells", p.cells);
            assert!((p.f_sim * p.scale - band.center()).abs() < 1.0);
            assert!(p.f_sim <= band.center());
            if band != Band::Rfid {
                assert!(p.scaled(), "{band:?}");
            }
            // Planes up to 3 m and one at the tag height.
            assert!(p.levels.iter().any(|y| (y - 1.2).abs() < 0.1));
            assert!(p.levels.iter().all(|y| *y <= 3.0));
        }
    }

    #[test]
    fn build_keeps_real_materials_and_a_uniform_grid() {
        let req = request(Band::Uwb5, Vec2::new(-4.0, -3.0), Vec2::new(4.0, 3.0));
        let p = plan(&req);
        assert!(p.scaled());
        let (sims, vox) = build(&req, &p).unwrap();
        assert_eq!(sims.len(), 1);
        assert!(vox.voxels > 0);
        let s = &sims[0];
        // Concrete at the real band, not the simulated one.
        let (eps, kappa) = electrical(Material::Concrete, Band::Uwb5.center()).unwrap();
        let c = s.dielectrics.iter().find(|d| d.name == "concrete").unwrap();
        assert!((c.eps_r - eps).abs() < 1e-9 && (c.kappa - kappa).abs() < 1e-12);
        assert!(s.dielectrics.iter().any(|d| d.name == "floor"));
        // No cell finer than the grid: the source sits on it.
        for m in &s.mesh {
            let min = m.windows(2).map(|w| w[1] - w[0]).fold(f64::MAX, f64::min);
            assert!(min > p.cell * 0.99, "min cell {min} vs {}", p.cell);
        }
        // Every plane samples the channel at N_FREQS frequencies.
        assert_eq!(s.dumps.len(), p.levels.len() + 2);
        assert!(s.dumps.iter().all(|d| d.freqs.len() == N_FREQS));
        assert!(s.to_xml().contains("<FD_Samples>"));
        assert_eq!(s.ports.len(), 1);
    }

    #[test]
    fn source_inside_a_wall_is_kept_clear() {
        let mut req = request(Band::Rfid, Vec2::new(-3.0, -3.0), Vec2::new(3.0, 3.0));
        req.txs = vec![tx(Vec3::new(0.0, 1.5, 0.0))];
        let p = plan(&req);
        let (sims, _) = build(&req, &p).unwrap();
        let port = sims[0].ports[0].gap;
        let c: P3 = std::array::from_fn(|k| (port.min[k] + port.max[k]) / 2.0);
        let inside = |b: &Bx| (0..3).all(|k| b.min[k] <= c[k] && c[k] <= b.max[k]);
        assert!(!sims[0].dielectrics.iter().filter(|d| d.name != "floor").flat_map(|d| &d.boxes).any(inside));
    }

    #[test]
    fn link_gain_matches_friis_for_a_dipole() {
        // Far field of a dipole fed with P: |E|² = 2·η·P·G / (4π d²).
        let (f, d, p, g) = (2.44e9, 5.0, 1e-3, 10f64.powf(0.215));
        let e2 = 2.0 * ETA0 * p * g / (4.0 * std::f64::consts::PI * d * d);
        let db = 10.0 * link_gain(e2, p, f).log10();
        let friis = 10.0 * g.log10() - crate::rtls::solver::fspl_db(d, f / 1e6);
        assert!((db - friis).abs() < 0.05, "{db} vs {friis}");
    }

    #[test]
    fn local_mean_resamples_and_smooths() {
        // Power alternating 0 / 2 on a 1 cm grid averages to 1 (0 dB).
        let cu: Vec<f64> = (0..101).map(|i| i as f64 * 0.01).collect();
        let cv = cu.clone();
        let pw: Vec<f64> = (0..101 * 101).map(|k| if (k % 101) % 2 == 0 { 2.0 } else { 0.0 }).collect();
        let pl = local_mean_plane([0, 1], 1.0, &cu, &cv, &pw, 0.01, 0.2);
        let v = pl.at(0.5, 0.5).unwrap();
        assert!(v.abs() < 0.5, "{v}");
        assert!(pl.at(2.0, 0.5).is_none());
    }

    #[test]
    fn calibration_fit_recovers_material_scales() {
        let mut samples = Vec::new();
        let mut rng = crate::rtls::rng::Rng::new(7);
        let truth = [1.0, 0.6, 1.0, 1.0, 1.4, 1.0, 1.0];
        for i in 0..400 {
            let mut w = [0.0f32; 7];
            if i % 2 == 0 {
                w[4] = 2.0 * (1 + i % 3) as f32;
            }
            if i % 3 == 0 {
                w[1] = 0.4 * (1 + i % 2) as f32;
            }
            let base = 40.0 + (i % 17) as f32;
            let walls: f32 = (0..7).map(|m| 6.0 * w[m] * truth[m]).sum();
            samples.push(Sample { loss: base + walls - 3.0 + rng.gauss(0.5), base, wall_db: 6.0, w });
        }
        let fit = fit_calibration(&samples, [1.0; 7]).unwrap();
        let get = |m: Material| fit.mats.iter().find(|f| f.material == m).unwrap().fitted;
        assert!((get(Material::Concrete) - 1.4).abs() < 0.07, "{:?}", fit);
        assert!((get(Material::Wood) - 0.6).abs() < 0.07, "{:?}", fit);
        assert!((fit.offset_db + 3.0).abs() < 0.5);
        assert!(fit.rms_after < fit.rms_before);
    }

    #[test]
    fn dipole_source_pattern() {
        assert!((source_gain_db(Vec3::X) - DIPOLE_DBI).abs() < 1e-4);
        assert!(source_gain_db(Vec3::Y) < -15.0);
    }
}
