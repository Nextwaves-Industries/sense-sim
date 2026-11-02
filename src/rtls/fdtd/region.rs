//! S21 through a piece of the scene: two dipoles on either side of an object
//! (wall, rack, door…), the objects around it voxelised from their real meshes
//! (ITU-R P.2040 properties for concrete, wood and glass, typical values for the
//! rest), and a free-space twin run so the obstruction loss and excess delay come
//! out directly.
//!
//! Scene (Y up) → simulation (Z up): sim = (world.x, world.z, world.y).

use std::sync::Arc;

use bevy::math::{Affine3A, Vec3};

use super::antenna::{Band, Quality};
use super::csx::{Bx, C0, Dielectric, Dump, DumpKind, P3, Port, Sim, auto_mesh};
use crate::bvh::Bvh;
use crate::rtls::radio::{CalBand, CalSource, Material, MaterialCal};

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
    /// A storage rack with some bays empty: its id and the share of bays with goods.
    pub rack: Option<(u64, f32)>,
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
    /// dB the floor map's ray model charges a typical wall at this band.
    pub wall_db: f32,
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
    /// dB per typical wall that weight means at this band.
    pub wall_db: f32,
    /// The cell budget cut the region inside the object.
    pub shrunk: bool,
    /// The material the mesh resolves worst, and its cells per wavelength.
    pub worst: Option<(Material, f64)>,
    /// How each object came out on the grid.
    pub thickness: Vec<ObjectVoxels>,
}

impl Layout {
    /// What this run's result as a calibration of its object's material would be:
    /// the loss the floor map's model has to charge to match it, at this band's real
    /// dB per wall. `None` when the object isn't really in the simulation.
    pub fn calibration(&self, band: Band, target: &str, dir: &std::path::Path, loss_db: f64, bias_cm: Option<f32>, nlos_bias_m: f32) -> Option<MaterialCal> {
        let gone = self.thickness.iter().any(|t| t.name == target && t.vanished());
        if gone || self.target_weight <= 0.0 || self.wall_db <= 0.0 {
            return None;
        }
        let bias_scale = bias_cm.map_or(1.0, |b| (b / (self.target_weight * nlos_bias_m * 100.0)).max(0.0));
        Some(MaterialCal {
            loss_scale: (loss_db as f32 / (self.wall_db * self.target_weight)).max(0.0),
            // Delay only measured with a group delay; keep the built-in one otherwise.
            bias_scale,
            label: format!("FDTD · {target} · {}", band.label()),
            source: CalSource { path: dir.to_path_buf(), band: CalBand::of_mhz((band.center() / 1e6) as f32), ..Default::default() },
        })
    }
}

/// Cells per wavelength in the material the mesh resolves worst, among `mats`, given
/// how many it has per wavelength in air.
pub fn worst_cells(mats: impl Iterator<Item = Material>, f_mat: f64, cells_in_air: f64) -> Option<(Material, f64)> {
    mats.filter_map(|m| electrical(m, f_mat).map(|e| (m, cells_in_air / e.0.sqrt()))).min_by(|a, b| a.1.total_cmp(&b.1))
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
    let shrunk = req.target_min.cmplt(rmin).any() || req.target_max.cmpgt(rmax).any();

    // ---- Voxelise on a uniform grid (sim axes).
    let smin = to_sim(rmin);
    let smax = to_sim(rmax);
    let lo = [smin[0].min(smax[0]), smin[1].min(smax[1]), smin[2].min(smax[2])];
    let hi = [smin[0].max(smax[0]), smin[1].max(smax[1]), smin[2].max(smax[2])];
    let vox = voxelise(&inside, lo, hi, cell, f0, 60_000_000)?;
    // On the grid, so nothing adds a cell finer than the rest.
    let hi: P3 = std::array::from_fn(|k| lo[k] + vox.dims[k] as f64 * cell);
    let mut layout = Layout { tx, rx, region_min: rmin, region_max: rmax, slice_y: height, cell, shrunk, wall_db: req.wall_db, ..Default::default() };
    layout.voxels = vox.voxels;
    layout.objects = vox.objects.clone();
    layout.thickness = vox.thickness.clone();
    layout.worst = worst_cells(inside.iter().map(|o| o.material), f0, C0 / fb / cell);

    // ---- Antennas: vertical dipoles on the voxel grid, strip across the path.
    let mut base = Sim { f0: (fa + fb) / 2.0, fc: (fb - fa) / 2.0, end_criteria: 1e-4, max_timesteps: 400_000, ..Default::default() };
    for (nr, pos) in [(1u32, tx), (2u32, rx)] {
        // Strip width along the transverse horizontal sim axis.
        let tr = if through == 0 { 1 } else { 0 };
        let (arms, port) = grid_dipole(lo, cell, to_sim(pos), lambda0, tr, nr, nr == 1);
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

    let pml = 12.0 * cell;
    let domain = Bx::new(lo.map(|v| v - pml), hi.map(|v| v + pml));
    // What the region's edge cuts carries on through the absorbing boundary: a cut edge
    // would diffract round the object, and the reading would be of its edges.
    let onward = |b: &Bx| {
        let mut o = *b;
        for k in 0..3 {
            if (b.min[k] - lo[k]).abs() < cell * 1e-3 {
                o.min[k] = domain.min[k];
            }
            if (b.max[k] - hi[k]).abs() < cell * 1e-3 {
                o.max[k] = domain.max[k];
            }
        }
        o
    };
    let mut with = base.clone();
    with.metal.extend(vox.metal.iter().map(onward));
    with.dielectrics.extend(vox.dielectrics.iter().map(|d| Dielectric { boxes: d.boxes.iter().map(onward).collect(), ..d.clone() }));
    // One mesh for both runs, from the obstructed geometry.
    let mesh = auto_mesh(&with, domain, cell);
    with.mesh = mesh.clone();
    let mut free = base;
    free.mesh = mesh;

    // What the ray model would say about this obstruction.
    let (mat, weight) = req.objects.iter().find(|o| o.name == req.target).map(|o| (o.material, o.weight)).unwrap_or((Material::Generic, 1.0));
    let (loss, delay) = MaterialCal::scales_in(mat, CalBand::of_mhz((f0 / 1e6) as f32));
    layout.target_material = mat;
    layout.target_weight = weight;
    layout.ray_loss_db = req.wall_db * weight * loss;
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
    /// How each of those came out on the grid.
    pub thickness: Vec<ObjectVoxels>,
}

/// One object on the grid, measured through its thinnest side: a thin wall can vanish
/// between cell centres, or come out a whole cell thicker than it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectVoxels {
    pub name: String,
    pub material: Material,
    /// Thickness of the mesh through that side, and of its voxels (m).
    pub true_m: f64,
    pub voxel_m: f64,
    /// A rack with empty bays: its voxels are meant to be missing.
    pub emptied: bool,
    /// Nothing to measure: no mesh, or only its corner is in the region.
    pub not_measured: bool,
}

impl ObjectVoxels {
    /// There's mesh here but no voxels: the object isn't in the simulation.
    pub fn vanished(&self) -> bool {
        !self.not_measured && !self.emptied && self.voxel_m <= 0.0
    }

    /// What's wrong with it, in words. Metal blocks at any thickness, so only its
    /// absence matters.
    pub fn problem(&self) -> Option<String> {
        if self.not_measured || self.emptied {
            return None;
        }
        if self.voxel_m <= 0.0 {
            return Some(if self.true_m <= 0.0 {
                format!("{}: a single surface with no inside, so it isn't in the simulation", self.name)
            } else {
                format!("{}: {:.0} mm thick, which vanished between the cells: it isn't in the simulation", self.name, self.true_m * 1e3)
            });
        }
        if self.material == Material::Metal || self.true_m <= 0.0 {
            return None;
        }
        let off = self.voxel_m / self.true_m - 1.0;
        (off.abs() > MAX_THICKNESS_ERROR).then(|| format!("{}: {:.0} mm thick, simulated as {:.0} mm ({:+.0} %)", self.name, self.true_m * 1e3, self.voxel_m * 1e3, off * 100.0))
    }
}

/// Past this the object's loss is the grid's, not the object's.
const MAX_THICKNESS_ERROR: f64 = 0.25;

/// Measures `o` on the grid, in up to 8 × 8 columns through its thinnest side.
fn measure_object(o: &SceneObj, mat: u16, grid: &[u16], dims: [usize; 3], lo: P3, cell: f64) -> ObjectVoxels {
    let (a, b) = (to_sim(o.min), to_sim(o.max));
    let low: P3 = std::array::from_fn(|k| a[k].min(b[k]));
    let high: P3 = std::array::from_fn(|k| a[k].max(b[k]));
    let axis = (0..3).min_by(|x, y| (high[*x] - low[*x]).total_cmp(&(high[*y] - low[*y]))).unwrap_or(0);
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let range = |k: usize| (((low[k] - lo[k]) / cell).floor().max(0.0) as usize)..(((high[k] - lo[k]) / cell).ceil().max(0.0) as usize).min(dims[k]);
    let picks = |r: std::ops::Range<usize>| -> Vec<usize> {
        let n = r.len();
        let m = n.min(8);
        (0..m).map(|s| r.start + (2 * s + 1) * n / (2 * m)).collect()
    };
    let mut out = ObjectVoxels { name: o.name.clone(), material: o.material, emptied: o.rack.is_some_and(|r| r.1 < 1.0), ..Default::default() };
    let (mut columns, mut chord_sum, mut voxel_sum) = (0usize, 0.0, 0.0);
    let (mut solid, mut any_voxels) = (0usize, 0usize);
    let edge = lo[axis] + dims[axis] as f64 * cell;
    for iu in picks(range(u)) {
        for iv in picks(range(v)) {
            columns += 1;
            let mut c = [0usize; 3];
            c[u] = iu;
            c[v] = iv;
            let count = (0..dims[axis])
                .filter(|i| {
                    let mut c = c;
                    c[axis] = *i;
                    grid[c[0] + dims[0] * (c[1] + dims[1] * c[2])] == mat
                })
                .count();
            let mut start = [0.0; 3];
            start[u] = lo[u] + (iu as f64 + 0.5) * cell;
            start[v] = lo[v] + (iv as f64 + 0.5) * cell;
            start[axis] = low[axis] - cell;
            let mut dir = [0.0; 3];
            dir[axis] = 1.0;
            let (world_start, world_dir) = (to_world(start), to_world(dir));
            let len = (high[axis] - low[axis] + 2.0 * cell) as f32;
            let mut hits: Vec<f32> = Vec::new();
            for (bvh, m) in &o.parts {
                let inv = m.inverse();
                hits.extend(bvh.hits(inv.transform_point3(world_start), inv.transform_vector3(world_dir), len));
            }
            hits.sort_by(f32::total_cmp);
            // Only what lies inside the region counts.
            let (t0, t1) = (lo[axis] - start[axis], edge - start[axis]);
            let chord: f64 = hits.chunks_exact(2).map(|p| (t1.min(p[1] as f64) - t0.max(p[0] as f64)).max(0.0)).sum();
            any_voxels += (count > 0) as usize;
            if chord > 1e-6 {
                solid += 1;
                chord_sum += chord;
                voxel_sum += count as f64 * cell;
            }
        }
    }
    if columns == 0 || o.parts.is_empty() {
        out.not_measured = true;
    } else if solid > 0 {
        out.true_m = chord_sum / solid as f64;
        out.voxel_m = voxel_sum / solid as f64;
    } else if any_voxels > 0 {
        // A sheet with no inside, marked cell by cell (thin metal).
        out.voxel_m = cell;
    }
    out
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
        // A bay without goods is just its frame, as in the ray model: nothing stops the signal.
        if let Some((rack_id, fill)) = o.rack.filter(|r| r.1 < 1.0) {
            // One past each range: a triangle on the far face marks the next cell.
            for k in rz.start..(rz.end + 1).min(dims[2]) {
                for j in ry.start..(ry.end + 1).min(dims[1]) {
                    for i in rx_.start..(rx_.end + 1).min(dims[0]) {
                        let at = to_world([lo[0] + (i as f64 + 0.5) * cell, lo[1] + (j as f64 + 0.5) * cell, lo[2] + (k as f64 + 0.5) * cell]);
                        if grid[idx(i, j, k)] == m && !crate::rtls::radio::bay_has_goods(rack_id, at, fill) {
                            grid[idx(i, j, k)] = 0;
                        }
                    }
                }
            }
        }
    }
    let voxels = grid.iter().filter(|v| **v != 0).count();
    let thickness = objects
        .iter()
        .map(|o| {
            let m = mats.iter().position(|m| *m == o.material).map_or(0, |p| p as u16 + 1);
            measure_object(o, m, &grid, dims, lo, cell)
        })
        .collect();

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
    Ok(Voxels { lo, cell, dims, grid, mats, metal, dielectrics, voxels, objects: listed, thickness })
}

/// A vertical (sim z) strip dipole about 0.47 λ long, on the voxel grid so it adds no
/// finer cell (one would set the time step for the whole run): two cells wide along
/// `width_axis`, with a two-cell feed gap whose centre, where the port probes sit, is a
/// grid line. The upper arm, the lower arm and the port.
pub fn grid_dipole(lo: P3, cell: f64, p: P3, lambda: f64, width_axis: usize, nr: u32, excite: bool) -> (Vec<Bx>, Port) {
    let snap = |k: usize, v: f64| lo[k] + ((v - lo[k]) / cell).round() * cell;
    let sheet_axis = 1 - width_axis;
    let x0 = snap(width_axis, p[width_axis] - cell);
    let sheet = snap(sheet_axis, p[sheet_axis]);
    let z_gap = snap(2, p[2] - cell);
    let arm = ((0.47 * lambda - 2.0 * cell) / 2.0 / cell).round().max(1.0) * cell;
    let bx = |z0: f64, z1: f64| {
        let mut a = [0.0; 3];
        let mut b = [0.0; 3];
        a[width_axis] = x0;
        b[width_axis] = x0 + 2.0 * cell;
        a[sheet_axis] = sheet;
        b[sheet_axis] = sheet;
        a[2] = z0;
        b[2] = z1;
        Bx::new(a, b)
    };
    let gap = bx(z_gap, z_gap + 2.0 * cell);
    (vec![bx(z_gap + 2.0 * cell, z_gap + 2.0 * cell + arm), bx(z_gap - arm, z_gap)], Port { nr, gap, dir: 2, r_ohm: 73.0, excite })
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
        SceneObj { name: "wall_1".into(), material: Material::Concrete, weight: Material::Concrete.weight(), parts: vec![(Arc::new(Bvh::new(&pos, &idx)), Affine3A::IDENTITY)], min: a, max: b, rack: None }
    }

    /// A box from `a` to `b` of `m`.
    fn solid(name: &str, a: Vec3, b: Vec3, m: Material) -> SceneObj {
        let v = |x: bool, y: bool, z: bool| [if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }];
        let pos: Vec<[f32; 3]> = (0..8).map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0)).collect();
        let idx = [0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5, 0, 4, 5, 0, 5, 1, 2, 3, 7, 2, 7, 6, 0, 2, 6, 0, 6, 4, 1, 5, 7, 1, 7, 3];
        SceneObj { name: name.into(), material: m, weight: m.weight(), parts: vec![(Arc::new(Bvh::new(&pos, &idx)), Affine3A::IDENTITY)], min: a, max: b, rack: None }
    }

    fn through(band: Band, o: SceneObj) -> Request {
        Request { band, quality: Quality::Fast, target: o.name.clone(), target_min: o.min, target_max: o.max, objects: vec![o], wall_db: 6.0 }
    }

    /// A wall `t` thick, on a grid of `cell` with its first face `offset` into a cell.
    fn on_grid(t: f32, cell: f64, offset: f64) -> ObjectVoxels {
        let z0 = (((1.0 / cell).floor() + offset) * cell) as f32;
        let w = solid("w", Vec3::new(0.1, 0.1, z0), Vec3::new(1.9, 1.9, z0 + t), Material::Concrete);
        let vox = voxelise(&[&w], [0.0, 0.0, 0.0], [2.0, 2.0, 2.0], cell, 1e9, 400_000_000).unwrap();
        vox.thickness.into_iter().next().unwrap()
    }

    #[test]
    fn a_wall_that_fits_the_grid_raises_no_warning() {
        let t = on_grid(0.2, 0.02, 0.25);
        assert!((t.true_m - 0.2).abs() < 1e-3 && (t.voxel_m - 0.2).abs() <= 0.02, "{t:?}");
        assert!(t.problem().is_none(), "{t:?}");
    }

    #[test]
    fn a_thin_wall_that_changes_thickness_or_vanishes_is_named() {
        // The audit's cases on a 68 mm cell. A 10 cm wall comes out one or two cells thick.
        let t = on_grid(0.10, 0.068, 0.6);
        assert_eq!((t.voxel_m / 0.068).round() as i32, 1, "{t:?}");
        assert!(t.problem().is_some_and(|p| p.contains("simulated as 68 mm")), "{t:?}");
        let t = on_grid(0.10, 0.068, 0.45);
        assert_eq!((t.voxel_m / 0.068).round() as i32, 2, "{t:?}");
        assert!(t.problem().is_some(), "{t:?}");
        // 12.5 mm drywall between two cell centres: gone.
        let t = on_grid(0.0125, 0.068, 0.6);
        assert!(t.vanished() && t.voxel_m == 0.0, "{t:?}");
        assert!(t.problem().is_some_and(|p| p.contains("isn't in the simulation")), "{t:?}");
        // Over a centre: five times too thick.
        let t = on_grid(0.0125, 0.068, 0.45);
        assert!(t.voxel_m > 0.06 && t.problem().is_some_and(|p| p.contains("+444")), "{t:?}");
    }

    #[test]
    fn a_surface_with_no_inside_is_named() {
        let pos = [[0.1, 0.1, 1.0], [1.9, 0.1, 1.0], [1.9, 1.0, 1.0], [0.1, 1.0, 1.0]];
        let sheet = |m: Material| SceneObj { name: "sheet".into(), material: m, weight: 1.0, parts: vec![(Arc::new(Bvh::new(&pos, &[0, 1, 2, 0, 2, 3])), Affine3A::IDENTITY)], min: Vec3::new(0.1, 0.1, 1.0), max: Vec3::new(1.9, 1.0, 1.0), rack: None };
        let (glass, metal) = (sheet(Material::Glass), sheet(Material::Metal));
        let vox = voxelise(&[&glass], [0.0, 0.0, 0.0], [2.0, 2.0, 2.0], 0.05, 1e9, 400_000_000).unwrap();
        assert!(vox.thickness[0].problem().is_some_and(|p| p.contains("no inside")), "{:?}", vox.thickness[0]);
        // Metal has no inside either, but it's marked cell by cell, and that's right for it.
        let vox = voxelise(&[&metal], [0.0, 0.0, 0.0], [2.0, 2.0, 2.0], 0.05, 1e9, 400_000_000).unwrap();
        assert!(vox.thickness[0].problem().is_none(), "{:?}", vox.thickness[0]);
    }

    #[test]
    fn empty_bays_are_not_voxelised() {
        let rack = |fill: f32| {
            let mut r = solid("rack", Vec3::new(0.0, 0.0, 0.0), Vec3::new(3.0, 3.0, 1.0), Material::Metal);
            r.rack = Some((7, fill));
            r
        };
        let voxels = |fill: f32| voxelise(&[&rack(fill)], [-0.5, -0.5, -0.5], [3.5, 1.5, 3.5], 0.1, 1e9, 400_000_000).unwrap();
        let (full, half, empty) = (voxels(1.0), voxels(0.5), voxels(0.0));
        assert!(full.voxels > 0, "a full rack keeps its voxels");
        assert_eq!(empty.voxels, 0);
        assert!(half.voxels > 0 && half.voxels < full.voxels, "{} of {}", half.voxels, full.voxels);
        assert!(empty.thickness[0].problem().is_none(), "meant to be missing: {:?}", empty.thickness[0]);
    }

    #[test]
    fn a_region_cut_short_says_so_and_carries_the_wall_through_the_boundary() {
        // UWB through a 4 × 3 m, 20 cm concrete wall: only about a 0.3 m patch fits the budget.
        let w = solid("wall", Vec3::new(-2.0, 0.0, -0.1), Vec3::new(2.0, 3.0, 0.1), Material::Concrete);
        let (with, free, l) = build(&through(Band::Uwb5, w)).unwrap();
        assert!(l.shrunk);
        assert!(l.region_max.x - l.region_min.x < 1.0, "{:?} {:?}", l.region_min, l.region_max);
        let wall = with.dielectrics.iter().find(|d| d.name == "concrete").unwrap();
        for k in [0, 2] {
            // Sim x and z run along the wall: it reaches both ends of the domain.
            let (a, b) = (with.mesh[k][0], with.mesh[k][with.mesh[k].len() - 1]);
            let min = wall.boxes.iter().map(|b| b.min[k]).fold(f64::MAX, f64::min);
            let max = wall.boxes.iter().map(|b| b.max[k]).fold(f64::MIN, f64::max);
            assert!((min - a).abs() < 1e-9 && (max - b).abs() < 1e-9, "axis {k}: {min}..{max} in {a}..{b}");
        }
        // Across the wall it stays the thickness it is.
        let min = wall.boxes.iter().map(|b| b.min[1]).fold(f64::MAX, f64::min);
        assert!((min + 0.1).abs() < 2.0 * l.cell, "{min}");
        assert!(free.dielectrics.is_empty());
        // Whole objects in reach aren't touched.
        let (_, _, l) = build(&through(Band::Rfid, wall_small())).unwrap();
        assert!(!l.shrunk);
    }

    fn wall_small() -> SceneObj {
        solid("wall", Vec3::new(-0.5, 0.0, -0.1), Vec3::new(0.5, 1.0, 0.1), Material::Concrete)
    }

    #[test]
    fn the_worst_resolved_material_is_reported() {
        // Liquid: the cell's permittivity cap is 10, so 10 · √(10 / 72) = 3.7 cells per λ.
        let w = solid("tank", Vec3::new(-0.5, 0.0, -0.1), Vec3::new(0.5, 1.0, 0.1), Material::Liquid);
        let (_, _, l) = build(&through(Band::Ble, w)).unwrap();
        let (m, cells) = l.worst.unwrap();
        assert_eq!(m, Material::Liquid);
        assert!((cells - 3.73).abs() < 0.05, "{cells}");
        // Metal has no wavelength inside it.
        let w = solid("plate", Vec3::new(-0.5, 0.0, -0.1), Vec3::new(0.5, 1.0, 0.1), Material::Metal);
        assert!(build(&through(Band::Ble, w)).unwrap().2.worst.is_none());
    }

    #[test]
    fn a_calibration_uses_the_bands_own_wall_loss_and_only_counts_there() {
        // The audit's run: 6.47 dB through a 20 cm wall at UHF, where the model charges 8 dB a wall.
        let (a, b) = (Vec3::new(-1.0, 0.0, -0.1), Vec3::new(1.0, 2.5, 0.1));
        let mut w = solid("wall_1", a, b, Material::Concrete);
        w.weight = 4.0;
        let (_, _, l) = build(&Request { wall_db: 8.0, ..through(Band::Rfid, w) }).unwrap();
        assert_eq!(l.wall_db, 8.0);
        assert!((l.ray_loss_db - 32.0).abs() < 1e-3, "the floor map's figure, not 6 dB a wall: {}", l.ray_loss_db);
        let c = l.calibration(Band::Rfid, "wall_1", std::path::Path::new("run"), 6.47, None, 0.35).unwrap();
        assert!((c.loss_scale * 8.0 * 4.0 - 6.47).abs() < 1e-3, "{c:?}");
        assert_eq!(c.source.band, CalBand::Uhf);
        assert_eq!(c.source.delay_origin, crate::rtls::radio::Origin::None, "a simulation isn't a measurement");
    }

    #[test]
    fn a_vanished_object_cant_calibrate_anything() {
        let mut l = Layout { target_weight: 1.0, wall_db: 6.0, ..Default::default() };
        l.thickness.push(ObjectVoxels { name: "drywall".into(), material: Material::Thin, true_m: 0.0125, voxel_m: 0.0, ..Default::default() });
        assert!(l.calibration(Band::Ble, "drywall", std::path::Path::new("run"), 3.0, None, 0.35).is_none());
    }

    #[test]
    fn antennas_on_the_voxel_grid_keep_the_time_step_of_the_room() {
        for band in [Band::Rfid, Band::Ble, Band::Uwb5] {
            let w = wall();
            let req = Request { band, quality: Quality::Fast, target: w.name.clone(), target_min: w.min, target_max: w.max, objects: vec![w], wall_db: 6.0 };
            let (with, _, layout) = build(&req).unwrap();
            for m in &with.mesh {
                let cells: Vec<f64> = m.windows(2).map(|w| w[1] - w[0]).collect();
                let min = cells.iter().copied().fold(f64::MAX, f64::min);
                assert!(min > 0.5 * layout.cell, "{band:?}: min cell {min} vs {}", layout.cell);
                for c in cells.windows(2) {
                    let r = c[1].max(c[0]) / c[1].min(c[0]);
                    assert!(r < 1.6, "{band:?}: neighbouring cells {r}:1");
                }
            }
        }
    }

    #[test]
    fn wall_region_is_voxelised_between_the_antennas() {
        let w = wall();
        let req = Request { band: Band::Rfid, quality: Quality::Fast, target: w.name.clone(), target_min: w.min, target_max: w.max, objects: vec![w], wall_db: 6.0 };
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
