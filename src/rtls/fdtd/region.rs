//! S21 through a piece of the scene: two tuned dipoles on either side of an
//! object (wall, rack, door…), the objects around it voxelised from their real
//! meshes with ITU-R P.2040 material properties, and a free-space twin run so
//! the obstruction loss and excess delay come out directly.
//!
//! Scene (Y up) → simulation (Z up): sim = (world.x, world.z, world.y).

use std::sync::Arc;

use bevy::math::{Affine3A, Vec3};

use super::antenna::{Band, Quality};
use super::csx::{Bx, C0, Dielectric, Dump, DumpKind, P3, Port, Sim, auto_mesh};
use crate::bvh::Bvh;
use crate::rtls::radio::Material;

pub fn to_sim(w: Vec3) -> P3 {
    [w.x as f64, w.z as f64, w.y as f64]
}

pub fn to_world(s: P3) -> Vec3 {
    Vec3::new(s[0] as f32, s[2] as f32, s[1] as f32)
}

/// Relative permittivity and conductivity (S/m) at `f` (Hz), ITU-R P.2040-3
/// table 3 where it has the material; `None` = perfect conductor.
pub fn electrical(m: Material, f: f64) -> Option<(f64, f64)> {
    let g = f / 1e9;
    Some(match m {
        Material::Metal => return None,
        Material::Concrete => (5.24, 0.0462 * g.powf(0.7822)),
        Material::Wood => (1.99, 0.0047 * g.powf(1.0718)),
        Material::Glass => (6.31, 0.0036 * g.powf(1.3394)),
        // Not in the table: water-filled goods, cardboard / plastic, unknown objects.
        Material::Liquid => (72.0, 0.7 * g.powf(1.2)),
        Material::Thin => (2.0, 0.002 * g),
        Material::Generic => (3.0, 0.01 * g),
    })
}

/// A placed object as the voxeliser needs it.
#[derive(Clone)]
pub struct SceneObj {
    pub name: String,
    pub material: Material,
    /// Obstruction weight the floor map's ray model gives it (thickness included).
    pub weight: f32,
    /// Mesh parts: BVH (mesh space) and mesh → world transform.
    pub parts: Vec<(Arc<Bvh>, Affine3A)>,
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Clone)]
pub struct Request {
    pub band: Band,
    pub quality: Quality,
    /// The object the signal goes through.
    pub target: String,
    pub target_min: Vec3,
    pub target_max: Vec3,
    /// Every placed object (only those touching the region are used).
    pub objects: Vec<SceneObj>,
}

/// Where things ended up, in world coordinates, for the overlay and the report.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub tx: Vec3,
    pub rx: Vec3,
    pub region_min: Vec3,
    pub region_max: Vec3,
    pub slice_y: f32,
    /// Field dump planes written by the run (see `build`).
    pub planes: Vec<String>,
    pub objects: Vec<(String, Material, f64, f64)>,
    pub voxels: usize,
    pub cell: f64,
    /// What the ray model predicts for this obstruction (dB, cm), with any
    /// FDTD calibration of its material already applied.
    pub ray_loss_db: f32,
    pub ray_bias_cm: f32,
    /// The object's material and uncalibrated ray-model weight, to turn an
    /// FDTD result into a calibration for that material.
    pub target_material: Material,
    pub target_weight: f32,
}

/// Builds the obstructed and the free-space simulations (same mesh).
pub fn build(req: &Request) -> anyhow::Result<(Sim, Sim, Layout)> {
    let f0 = req.band.center();
    let (fa, fb) = (f0 * 0.85, f0 * 1.15);
    let lambda0 = C0 / f0;
    let n = match req.quality {
        Quality::Fast => 10.0,
        Quality::Normal => 15.0,
        Quality::Fine => 20.0,
    };
    let size = req.target_max - req.target_min;
    // Through the thinnest horizontal side.
    let through = if size.x <= size.z { 0 } else { 2 };
    let centre = (req.target_min + req.target_max) * 0.5;
    let height = (1.2f32).clamp(req.target_min.y + 0.1, (req.target_max.y - 0.1).max(req.target_min.y + 0.1));
    let half_through = size[through] / 2.0;
    let gap = (0.75 * lambda0) as f32;
    let mut tx = centre;
    tx.y = height;
    let mut rx = tx;
    tx[through] -= half_through + gap;
    rx[through] += half_through + gap;
    // The region wraps the whole object (its full width and height, so the
    // field can be shown over all of it), at least 1.5 λ around the antennas.
    let side = (1.5 * lambda0) as f32;
    let margin = (0.5 * lambda0) as f32;
    let mut rmin = (req.target_min - Vec3::splat(margin)).min(Vec3::new(centre.x - side, height - side, centre.z - side));
    let mut rmax = (req.target_max + Vec3::splat(margin)).max(Vec3::new(centre.x + side, height + side, centre.z + side));
    rmin[through] = tx[through] - margin;
    rmax[through] = rx[through] + margin;
    rmin.y = rmin.y.max(-0.01);

    // Materials present decide the cell size (capped: water would need a micro-mesh).
    let touching = |lo: Vec3, hi: Vec3| -> Vec<&SceneObj> { req.objects.iter().filter(|o| o.min.cmplt(hi).all() && lo.cmplt(o.max).all()).collect() };
    let eps_max = touching(rmin, rmax).iter().filter_map(|o| electrical(o.material, f0)).map(|e| e.0).fold(1.0, f64::max).min(10.0);
    let cell = C0 / fb / n / eps_max.sqrt();
    // Cell budget (run time grows with it): shrink the height, then the width,
    // around the antennas until the region fits.
    let budget = match req.quality {
        Quality::Fast => 6e6,
        Quality::Normal => 12e6,
        Quality::Fine => 24e6,
    };
    let cells = |lo: Vec3, hi: Vec3| ((hi - lo) / cell as f32).ceil().max(Vec3::ONE).to_array().iter().map(|v| *v as f64).product::<f64>();
    let transverse = 2 - through;
    for _ in 0..40 {
        if cells(rmin, rmax) <= budget {
            break;
        }
        let shrink = |lo: &mut f32, hi: &mut f32, c: f32, min_half: f32| {
            *lo = (*lo + (c - *lo) * 0.1).min(c - min_half);
            *hi = (*hi - (*hi - c) * 0.1).max(c + min_half);
        };
        let (h, w) = (rmax.y - rmin.y, rmax[transverse] - rmin[transverse]);
        if h >= w && h > 2.0 * side {
            shrink(&mut rmin.y, &mut rmax.y, height, side);
        } else if w > 2.0 * side {
            shrink(&mut rmin[transverse], &mut rmax[transverse], centre[transverse], side);
        } else {
            break;
        }
    }
    rmin.y = rmin.y.max(-0.01);
    let inside = touching(rmin, rmax);


    // ---- Voxelise on a uniform grid (sim axes).
    let smin = to_sim(rmin);
    let smax = to_sim(rmax);
    let lo = [smin[0].min(smax[0]), smin[1].min(smax[1]), smin[2].min(smax[2])];
    let hi = [smin[0].max(smax[0]), smin[1].max(smax[1]), smin[2].max(smax[2])];
    let vox = voxelise(&inside, lo, hi, cell, f0, 60_000_000)?;
    let mut layout = Layout { tx, rx, region_min: rmin, region_max: rmax, slice_y: height, cell, ..Default::default() };
    layout.voxels = vox.voxels;
    layout.objects = vox.objects.clone();

    // ---- Antennas: vertical dipoles tuned to the band, strip across the path.
    let mut base = Sim { f0: (fa + fb) / 2.0, fc: (fb - fa) / 2.0, end_criteria: 1e-4, max_timesteps: 400_000, ..Default::default() };
    for (nr, pos) in [(1u32, tx), (2u32, rx)] {
        // Strip width along the transverse horizontal sim axis.
        let tr = if through == 0 { 1 } else { 0 };
        let (arms, port) = dipole(to_sim(pos), f0, tr, nr, nr == 1);
        base.metal.extend(arms);
        base.ports.push(port);
    }
    // Field planes: horizontal at antenna height ("slice"), a vertical cut
    // along the path through the whole object ("vcut"), and horizontal levels
    // over its height ("level<i>"), together a 3D picture of the field.
    let slice = Bx::new([lo[0], lo[1], to_sim(tx)[2]], [hi[0], hi[1], to_sim(tx)[2]]);
    base.dumps.push(Dump { name: "slice".into(), kind: DumpKind::EFreq, freqs: vec![f0], region: slice });
    layout.planes.push("slice".into());
    // Transverse world axis (x or z) is sim axis 0 or 1.
    let tr_sim = if through == 0 { 1 } else { 0 };
    let (mut a, mut b) = (lo, hi);
    a[tr_sim] = to_sim(tx)[tr_sim];
    b[tr_sim] = to_sim(tx)[tr_sim];
    base.dumps.push(Dump { name: "vcut".into(), kind: DumpKind::EFreq, freqs: vec![f0], region: Bx::new(a, b) });
    layout.planes.push("vcut".into());
    let (y0, y1) = (lo[2] + 2.0 * cell, hi[2] - 2.0 * cell);
    let levels = 5;
    for i in 0..levels {
        let y = y0 + (y1 - y0) * (i as f64 + 0.5) / levels as f64;
        if (y - to_sim(tx)[2]).abs() < 0.08 {
            continue;
        }
        let name = format!("level{i}");
        base.dumps.push(Dump { name: name.clone(), kind: DumpKind::EFreq, freqs: vec![f0], region: Bx::new([lo[0], lo[1], y], [hi[0], hi[1], y]) });
        layout.planes.push(name);
    }

    let mut with = base.clone();
    with.metal.extend(vox.metal);
    with.dielectrics.extend(vox.dielectrics);
    let pml = 12.0 * cell;
    let domain = Bx::new(lo.map(|v| v - pml), hi.map(|v| v + pml));
    // One mesh for both runs, from the obstructed geometry.
    let mesh = auto_mesh(&with, domain, cell);
    with.mesh = mesh.clone();
    let mut free = base;
    free.mesh = mesh;

    // What the ray model would say about this obstruction.
    let (mat, weight) = req.objects.iter().find(|o| o.name == req.target).map(|o| (o.material, o.weight)).unwrap_or((Material::Generic, 1.0));
    let (loss, delay) = crate::rtls::radio::MaterialCal::scales(mat);
    layout.target_material = mat;
    layout.target_weight = weight;
    layout.ray_loss_db = 6.0 * weight * loss;
    layout.ray_bias_cm = weight * delay * crate::rtls::device::Profile::default().nlos_bias_m * 100.0;
    Ok((with, free, layout))
}

/// Scene objects on a uniform grid (sim axes): the solver geometry and the
/// occupancy itself, so callers can tell which cells are inside something.
#[derive(Clone, Debug, Default)]
pub struct Voxels {
    pub lo: P3,
    pub cell: f64,
    pub dims: [usize; 3],
    /// 0 = air, else index + 1 into `mats`; cell `(i, j, k)` at `i + dims[0]·(j + dims[1]·k)`.
    pub grid: Vec<u16>,
    pub mats: Vec<Material>,
    pub metal: Vec<Bx>,
    pub dielectrics: Vec<Dielectric>,
    pub voxels: usize,
    /// Name, material, εr and σ (S/m; infinite = metal) of each object voxelised.
    pub objects: Vec<(String, Material, f64, f64)>,
}

impl Voxels {
    /// Material of the cell holding sim point `p`, if any.
    pub fn at(&self, p: P3) -> Option<Material> {
        let c: [i64; 3] = std::array::from_fn(|k| ((p[k] - self.lo[k]) / self.cell).floor() as i64);
        if !(0..3).all(|k| c[k] >= 0 && (c[k] as usize) < self.dims[k]) {
            return None;
        }
        let m = self.grid[c[0] as usize + self.dims[0] * (c[1] as usize + self.dims[1] * c[2] as usize)];
        (m != 0).then(|| self.mats[m as usize - 1])
    }

    /// A hash of the occupancy, to tell whether the scene changed between runs.
    pub fn hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.dims.hash(&mut h);
        self.lo.map(f64::to_bits).hash(&mut h);
        self.cell.to_bits().hash(&mut h);
        self.grid.hash(&mut h);
        self.mats.iter().map(|m| *m as u8).collect::<Vec<_>>().hash(&mut h);
        h.finish()
    }
}

/// Voxelises `objects` in the sim box `lo`..`hi` with cubes of `cell`, materials
/// evaluated at `f_mat`, and turns runs of equal voxels along x into boxes.
pub fn voxelise(objects: &[&SceneObj], lo: P3, hi: P3, cell: f64, f_mat: f64, max_cells: usize) -> anyhow::Result<Voxels> {
    let dims: [usize; 3] = std::array::from_fn(|k| ((hi[k] - lo[k]) / cell).ceil().max(1.0) as usize);
    let total = dims[0] * dims[1] * dims[2];
    if total > max_cells {
        anyhow::bail!("region needs {} M cells: pick a lower band or Fast quality", total / 1_000_000);
    }
    let (wmin, wmax) = {
        let (a, b) = (to_world(lo), to_world(hi));
        (a.min(b), a.max(b))
    };
    // 0 = air, else index + 1 into `mats`.
    let mut grid = vec![0u16; total];
    let mut mats: Vec<Material> = Vec::new();
    let mut listed = Vec::new();
    let idx = |i: usize, j: usize, k: usize| i + dims[0] * (j + dims[1] * k);
    for o in objects {
        let m = match mats.iter().position(|m| *m == o.material) {
            Some(p) => p as u16 + 1,
            None => {
                mats.push(o.material);
                mats.len() as u16
            }
        };
        let (eps, kappa) = electrical(o.material, f_mat).unwrap_or((0.0, f64::INFINITY));
        listed.push((o.name.clone(), o.material, eps, kappa));
        let omin = to_sim(o.min);
        let omax = to_sim(o.max);
        let cell_range = |k: usize, a: f64, b: f64| {
            let (a, b) = (a.min(b), a.max(b));
            let i0 = (((a - lo[k]) / cell).floor().max(0.0)) as usize;
            let i1 = (((b - lo[k]) / cell).ceil().max(0.0) as usize).min(dims[k]);
            i0..i1
        };
        let (rx_, ry, rz) = (cell_range(0, omin[0], omax[0]), cell_range(1, omin[1], omax[1]), cell_range(2, omin[2], omax[2]));
        // Solid parts: parity of crossings along +x, from outside the object
        // (it may extend beyond the region).
        let x_start = lo[0].min(omin[0].min(omax[0])) - cell;
        let x_end = hi[0].max(omin[0].max(omax[0])) + cell;
        for k in rz.clone() {
            for j in ry.clone() {
                let y = lo[1] + (j as f64 + 0.5) * cell;
                let z = lo[2] + (k as f64 + 0.5) * cell;
                let start = to_world([x_start, y, z]);
                let len = (x_end - x_start) as f32;
                let mut cross: Vec<f32> = Vec::new();
                for (bvh, to_world_m) in &o.parts {
                    let inv = to_world_m.inverse();
                    let d = inv.transform_vector3(Vec3::X);
                    cross.extend(bvh.hits(inv.transform_point3(start), d, len));
                }
                cross.sort_by(f32::total_cmp);
                // Voxel i (centre at x = lo + (i + 0.5)·cell, t = x - x_start) is
                // solid between an entering and a leaving crossing.
                let off = x_start - lo[0];
                for pair in cross.chunks_exact(2) {
                    let first = ((pair[0] as f64 + off) / cell - 0.5).ceil().max(0.0) as usize;
                    let last = ((pair[1] as f64 + off) / cell - 0.5).floor();
                    if last < 0.0 {
                        continue;
                    }
                    for i in first.max(rx_.start)..=(last as usize).min(dims[0] - 1) {
                        grid[idx(i, j, k)] = m;
                    }
                }
            }
        }
        // Thin metal (shelves, wire decks, sheet) has no inside: mark every cell a
        // triangle passes through.
        if o.material == Material::Metal {
            for (bvh, to_world_m) in &o.parts {
                for t in bvh.triangles() {
                    let w = t.map(|p| to_world_m.transform_point3(p));
                    let tmin = w[0].min(w[1]).min(w[2]);
                    let tmax = w[0].max(w[1]).max(w[2]);
                    if !(tmin.cmple(wmax).all() && wmin.cmple(tmax).all()) {
                        continue;
                    }
                    let steps = (((w[1] - w[0]).length().max((w[2] - w[0]).length()) as f64 / (cell * 0.5)).ceil() as usize).clamp(1, 400);
                    for a in 0..=steps {
                        for b in 0..=(steps - a) {
                            let (u, v) = (a as f32 / steps as f32, b as f32 / steps as f32);
                            let p = to_sim(w[0] + (w[1] - w[0]) * u + (w[2] - w[0]) * v);
                            let c: [i64; 3] = std::array::from_fn(|k| ((p[k] - lo[k]) / cell).floor() as i64);
                            if (0..3).all(|k| c[k] >= 0 && (c[k] as usize) < dims[k]) {
                                grid[idx(c[0] as usize, c[1] as usize, c[2] as usize)] = m;
                            }
                        }
                    }
                }
            }
        }
    }
    let voxels = grid.iter().filter(|v| **v != 0).count();

    // ---- Runs of equal voxels along x → boxes.
    let mut metal = Vec::new();
    let mut diel: Vec<Vec<Bx>> = vec![Vec::new(); mats.len()];
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            let mut i = 0;
            while i < dims[0] {
                let m = grid[idx(i, j, k)];
                let s = i;
                while i < dims[0] && grid[idx(i, j, k)] == m {
                    i += 1;
                }
                if m == 0 {
                    continue;
                }
                let b = Bx::new(
                    [lo[0] + s as f64 * cell, lo[1] + j as f64 * cell, lo[2] + k as f64 * cell],
                    [lo[0] + i as f64 * cell, lo[1] + (j + 1) as f64 * cell, lo[2] + (k + 1) as f64 * cell],
                );
                match mats[m as usize - 1] {
                    Material::Metal => metal.push(b),
                    _ => diel[m as usize - 1].push(b),
                }
            }
        }
    }
    let mut dielectrics = Vec::new();
    for (m, boxes) in mats.iter().zip(diel) {
        if let Some((eps_r, kappa)) = electrical(*m, f_mat)
            && !boxes.is_empty()
        {
            dielectrics.push(Dielectric { name: format!("{m:?}").to_lowercase(), eps_r, kappa, boxes });
        }
    }
    Ok(Voxels { lo, cell, dims, grid, mats, metal, dielectrics, voxels, objects: listed })
}

/// A vertical (sim z) strip half-wave dipole tuned to `f`, centred on `p`, its
/// strip width along sim axis `width_axis`: the two arms and the feed port.
pub fn dipole(p: P3, f: f64, width_axis: usize, nr: u32, excite: bool) -> (Vec<Bx>, Port) {
    let lambda = C0 / f;
    // The proportions of `Design::tuned` for a dipole.
    let (len, w, g) = (0.47 * lambda, lambda / 50.0, lambda / 60.0);
    let arm = (len - g) / 2.0;
    let (mut a, mut b) = (p, p);
    a[width_axis] -= w / 2.0;
    b[width_axis] += w / 2.0;
    let arm_box = |z0: f64, z1: f64| {
        let (mut p1, mut p2) = (a, b);
        p1[2] = z0;
        p2[2] = z1;
        Bx::new(p1, p2)
    };
    let arms = vec![arm_box(p[2] + g / 2.0, p[2] + g / 2.0 + arm), arm_box(p[2] - g / 2.0 - arm, p[2] - g / 2.0)];
    (arms, Port { nr, gap: arm_box(p[2] - g / 2.0, p[2] + g / 2.0), dir: 2, r_ohm: 73.0, excite })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall() -> SceneObj {
        // A 2 m × 2 m × 0.2 m box, as 12 triangles.
        let (a, b) = (Vec3::new(-1.0, 0.0, -0.1), Vec3::new(1.0, 2.0, 0.1));
        let v = |x: bool, y: bool, z: bool| [if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }];
        let pos: Vec<[f32; 3]> = (0..8).map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0)).collect();
        let idx = [0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5, 0, 4, 5, 0, 5, 1, 2, 3, 7, 2, 7, 6, 0, 2, 6, 0, 6, 4, 1, 5, 7, 1, 7, 3];
        SceneObj { name: "wall_1".into(), material: Material::Concrete, weight: Material::Concrete.weight(), parts: vec![(Arc::new(Bvh::new(&pos, &idx)), Affine3A::IDENTITY)], min: a, max: b }
    }

    #[test]
    fn wall_region_is_voxelised_between_the_antennas() {
        let w = wall();
        let req = Request { band: Band::Rfid, quality: Quality::Fast, target: w.name.clone(), target_min: w.min, target_max: w.max, objects: vec![w] };
        let (with, free, layout) = build(&req).unwrap();
        // Through the 0.2 m side (world z): antennas on either side of it.
        assert!(layout.tx.z < -0.1 && layout.rx.z > 0.1);
        assert_eq!(with.ports.len(), 2);
        assert!(free.dielectrics.is_empty() && !with.dielectrics.is_empty());
        let conc = &with.dielectrics[0];
        assert!((conc.eps_r - 5.24).abs() < 1e-9);
        // The voxelised wall spans its thickness (sim y = world z), within a cell.
        let ymin = conc.boxes.iter().map(|b| b.min[1]).fold(f64::MAX, f64::min);
        let ymax = conc.boxes.iter().map(|b| b.max[1]).fold(f64::MIN, f64::max);
        assert!((ymin + 0.1).abs() <= layout.cell * 1.01 && (ymax - 0.1).abs() <= layout.cell * 1.01, "{ymin} {ymax} cell {}", layout.cell);
        assert_eq!(with.mesh, free.mesh);
    }
}
