//! FDTD input for openEMS: turns a [`Geometry`] into a graded rectilinear mesh
//! and the CSXCAD XML of one run per driven port, with near-field boxes for the
//! far-field transform and field planes for viewing.

use std::fmt::Write;

use anyhow::{Result, bail};

use crate::geometry::{Geometry, P3, PortDef, Shape, add, dot, norm, scale, sub, unit};
use crate::material::{self, MatKind};
use crate::project::Setup;
use crate::C0;

/// Face names of the near-field box, with their outward normals.
pub const FACES: [(&str, usize, f64); 6] = [("xn", 0, -1.0), ("xp", 0, 1.0), ("yn", 1, -1.0), ("yp", 1, 1.0), ("zn", 2, -1.0), ("zp", 2, 1.0)];

/// Copper thickness assumed for real-metal sheets.
const SHEET_THICKNESS: f64 = 35e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneKind {
    E,
    H,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaneDump {
    pub name: String,
    pub kind: PlaneKind,
    pub min: P3,
    pub max: P3,
}

/// Everything about a run except which port drives it.
#[derive(Clone, Debug)]
pub struct Plan {
    pub mesh: [Vec<f64>; 3],
    pub nf_min: P3,
    pub nf_max: P3,
    pub ground: bool,
    pub f0: f64,
    pub fc: f64,
    pub end_db: f64,
    pub max_steps: u64,
    pub far_freqs: Vec<f64>,
    pub design_f: f64,
    pub planes: Vec<PlaneDump>,
    pub max_cell: f64,
    pub min_cell: f64,
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn cells(&self) -> u64 {
        self.mesh.iter().map(|m| m.len().saturating_sub(1).max(1) as u64).product()
    }

    /// Roughly 5 kB per thousand cells: fields and coefficients of the engine.
    pub fn memory_bytes(&self) -> u64 {
        self.cells() * 90
    }

    /// Run time for one port at a solver speed (megacells per second).
    pub fn estimate_s(&self, speed_mcs: f64) -> f64 {
        let dt = self.min_cell.max(1e-9) / (C0 * 3f64.sqrt());
        // The pulse (2·9/(2π·fc) long), then the ring-down of a moderately resonant
        // antenna (Q about 20, to the end criterion).
        let pulse = 2.86 / self.fc.max(1.0);
        let ring = 2.0 * (self.end_db / 8.686) * 20.0 / (2.0 * std::f64::consts::PI * self.f0.max(1.0));
        let steps = ((pulse + ring) / dt).min(self.max_steps as f64);
        self.cells() as f64 * steps / (speed_mcs.max(1.0) * 1e6)
    }

    /// The faces of the near-field box that exist (no bottom over a ground plane).
    pub fn faces(&self) -> Vec<(&'static str, usize, f64)> {
        FACES.iter().copied().filter(|(n, _, _)| !self.ground || *n != "zn").collect()
    }
}

/// Mesh lines along one axis: every hard line stays, the gap between two hard
/// lines is filled to the smallest cell any covering region asks for, and
/// neighbouring cells differ by at most `ratio`.
pub fn axis_lines(hard: &[f64], regions: &[(f64, f64, f64)], lo: f64, hi: f64, max_cell: f64, ratio: f64) -> Vec<f64> {
    axis_lines_keeping(hard, &[], regions, lo, hi, max_cell, ratio)
}

/// Sorted lines closer than `tol` merge; a line in `keep` always survives the merge.
fn merge_close(h: &mut Vec<f64>, keep: &[f64], tol: f64) {
    h.sort_by(f64::total_cmp);
    let kept = |x: f64| keep.iter().any(|k| (k - x).abs() <= 1e-12 * tol.max(1e-300) || *k == x);
    let mut out: Vec<f64> = Vec::with_capacity(h.len());
    for &x in h.iter() {
        match out.last().copied() {
            Some(y) if (x - y).abs() < tol => {
                if kept(x) && kept(y) {
                    if x > y {
                        out.push(x);
                    }
                } else if kept(x) {
                    *out.last_mut().unwrap() = x;
                }
            }
            _ => out.push(x),
        }
    }
    *h = out;
}

/// [`axis_lines`] where `keep` lines stay however close they are to anything: the two
/// ends of a port's gap, since a port merged into one line is a dead port.
pub fn axis_lines_keeping(hard: &[f64], keep: &[f64], regions: &[(f64, f64, f64)], lo: f64, hi: f64, max_cell: f64, ratio: f64) -> Vec<f64> {
    let finest = regions.iter().map(|r| r.2).fold(max_cell, f64::min);
    // The smallest cell sets the time step of the whole run, so lines closer than
    // half a wanted cell merge: an edge moves far less than a cell, and FDTD snaps
    // edges to lines anyway.
    let tol = (finest * 0.45).max(max_cell * 1e-3);
    let mut h: Vec<f64> = hard.iter().chain(keep).copied().filter(|x| *x > lo && *x < hi).collect();
    h.push(lo);
    h.push(hi);
    merge_close(&mut h, keep, tol);
    // A region's ends only matter where they don't crowd a real line.
    for r in regions {
        for x in [r.0, r.1] {
            if x > lo && x < hi && h.iter().all(|y| (y - x).abs() >= tol) {
                h.push(x);
            }
        }
    }
    merge_close(&mut h, keep, tol);
    if h.len() < 2 {
        return h;
    }
    // Wanted cell in each gap.
    let n = h.len() - 1;
    let mut want: Vec<f64> = (0..n)
        .map(|i| {
            let mid = (h[i] + h[i + 1]) * 0.5;
            regions.iter().filter(|r| r.0 <= mid && mid <= r.1).map(|r| r.2).fold(max_cell, f64::min)
        })
        .collect();
    // A gap can't hold a cell bigger than itself; what touches it must grade to that.
    let mut touch: Vec<f64> = (0..n).map(|i| want[i].min(h[i + 1] - h[i])).collect();
    for i in 1..n {
        touch[i] = touch[i].min(touch[i - 1] * ratio);
    }
    for i in (0..n - 1).rev() {
        touch[i] = touch[i].min(touch[i + 1] * ratio);
    }
    for i in 0..n {
        want[i] = want[i].max(touch[i]);
    }
    let mut out = vec![h[0]];
    for i in 0..n {
        let (a, b) = (h[i], h[i + 1]);
        let cell = want[i];
        if b - a <= cell * 1.0001 {
            out.push(b);
            continue;
        }
        let (mut sl, mut sr) = (if i > 0 { touch[i - 1] } else { cell }, if i + 1 < n { touch[i + 1] } else { cell });
        let (mut pa, mut pb) = (a, b);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        loop {
            sl = (sl * ratio).min(cell);
            sr = (sr * ratio).min(cell);
            if pb - pa <= sl + sr || (sl >= cell && sr >= cell) {
                break;
            }
            pa += sl;
            pb -= sr;
            left.push(pa);
            right.push(pb);
        }
        let rest = pb - pa;
        let k = (rest / cell - 1e-6).ceil().max(1.0) as usize;
        out.extend(left);
        for j in 1..k {
            out.push(pa + rest * j as f64 / k as f64);
        }
        out.extend(right.into_iter().rev());
        out.push(b);
    }
    out
}

/// The axis a port's width runs along: in the plane of the flat sheet it touches.
pub fn width_axis(port: &PortDef, geom: &Geometry) -> usize {
    let dir = port.axis();
    let others = [(dir + 1) % 3, (dir + 2) % 3];
    let default = others[0].min(others[1]);
    let tol = 1e-9 + 1e-6 * norm(sub(port.b, port.a));
    for s in &geom.solids {
        let (lo, hi) = s.shape.bounds();
        let touches = |p: P3| (0..3).all(|k| p[k] >= lo[k] - tol && p[k] <= hi[k] + tol);
        if !(touches(port.a) || touches(port.b)) {
            continue;
        }
        for (i, k) in others.iter().enumerate() {
            if hi[*k] - lo[*k] <= tol {
                return others[1 - i];
            }
        }
    }
    default
}

/// The port as a box: start and stop keep the a → b order along its axis.
pub fn port_box(port: &PortDef, geom: &Geometry) -> (P3, P3, usize) {
    let dir = port.axis();
    let (mut a, mut b) = (port.a, port.b);
    if port.width > 0.0 {
        let w = width_axis(port, geom);
        let c = (a[w] + b[w]) * 0.5;
        a[w] = c - port.width / 2.0;
        b[w] = c + port.width / 2.0;
    }
    (a, b, dir)
}

/// A wire with the port gaps taken out: an FDTD metal edge would short them.
pub fn clip_wire(pts: &[P3], ports: &[PortDef]) -> Vec<Vec<P3>> {
    let mut out = Vec::new();
    let Some(first) = pts.first() else { return out };
    let mut cur = vec![*first];
    for w in pts.windows(2) {
        let (p, q) = (w[0], w[1]);
        let d = sub(q, p);
        let len = norm(d);
        if len <= 0.0 {
            continue;
        }
        let u = unit(d);
        let tol = 1e-9 + len * 1e-6;
        let mut cuts: Vec<(f64, f64)> = ports
            .iter()
            .filter_map(|port| {
                let (ta, tb) = (dot(sub(port.a, p), u), dot(sub(port.b, p), u));
                let off = |x: P3, t: f64| norm(sub(sub(x, p), scale(u, t)));
                let (lo, hi) = (ta.min(tb), ta.max(tb));
                (off(port.a, ta) < tol && off(port.b, tb) < tol && lo >= -tol && hi <= len + tol && hi > lo).then_some((lo.max(0.0), hi.min(len)))
            })
            .collect();
        cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut t = 0.0;
        for (lo, hi) in cuts {
            if lo > t + tol {
                cur.push(add(p, scale(u, lo)));
            }
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            }
            cur = vec![add(p, scale(u, hi))];
            t = hi;
        }
        if t < len - tol {
            cur.push(q);
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out
}

/// More cells than one PC can ever run: 900 GB of memory, and days per port. It
/// takes a typing slip to get here, such as 860 in a field that's in GHz.
pub const MAX_CELLS: f64 = 1e10;

fn check_cells(cells: f64, f_hi: f64) -> Result<()> {
    if cells > MAX_CELLS {
        bail!("At {:.3} GHz this model comes to about {:.0} billion FDTD cells and {:.1} TB of memory, far more than a run can use. Check the sweep band and the model's size.", f_hi / 1e9, cells / 1e9, cells * 90.0 / 1e12);
    }
    Ok(())
}

/// Builds the mesh, near-field box and dumps.
pub fn plan(geom: &Geometry, setup: &Setup, design_f: f64) -> Result<Plan> {
    setup.check_band()?;
    let lambda_min = C0 / setup.f_hi;
    let center = (setup.f_lo + setup.f_hi) / 2.0;
    let cpl = setup.cells_per_lambda.clamp(6.0, 200.0);
    let max_cell = lambda_min / cpl;
    let refine = 20.0 / cpl;

    let (lo, hi) = geom.bounds();
    let lambda0 = C0 / center;
    // A quarter of the longest wavelength: the low edge is what defines a wideband
    // antenna's bandwidth, and at a quarter of the centre's the PML sits in its near field.
    let nf_pad = C0 / setup.f_lo / 4.0;
    let pml_pad = 12.0 * max_cell;
    let mut nf_min = [lo[0] - nf_pad, lo[1] - nf_pad, lo[2] - nf_pad];
    let nf_max = [hi[0] + nf_pad, hi[1] + nf_pad, hi[2] + nf_pad];
    let mut dom_min = [nf_min[0] - pml_pad, nf_min[1] - pml_pad, nf_min[2] - pml_pad];
    let dom_max = [nf_max[0] + pml_pad, nf_max[1] + pml_pad, nf_max[2] + pml_pad];
    if geom.ground {
        nf_min[2] = 0.0;
        dom_min[2] = 0.0;
    }
    // Before any line exists: a half-typed f0 can ask for more of them than there is memory.
    check_cells((0..3).map(|k| ((dom_max[k] - dom_min[k]) / max_cell).max(1.0)).product(), setup.f_hi)?;

    let mut hard: [Vec<f64>; 3] = Default::default();
    let mut regions: [Vec<(f64, f64, f64)>; 3] = Default::default();
    for k in 0..3 {
        hard[k].extend([nf_min[k], nf_max[k], 0.0]);
    }
    let lib = material::library();
    let known = |n: &str| lib.iter().find(|m| m.name.eq_ignore_ascii_case(n));
    if let Some(s) = geom.solids.iter().find(|s| known(&s.material).is_none()) {
        bail!("Part '{}' is made of '{}', which isn't in the material library.", s.name, s.material);
    }
    let eps_max = geom
        .solids
        .iter()
        .filter_map(|s| known(&s.material))
        .map(|m| m.eps_r())
        .fold(1.0, f64::max);
    let mut warnings = Vec::new();

    // The thirds rule. The field is singular at a sheet's edge, which makes a metal
    // edge sitting on a mesh line act about half a cell too long. So in the fine
    // region around a radiator the edge goes between lines, one third of a cell
    // inside and two thirds outside; openEMS snaps the metal to the inner one. Edges
    // that end at a port stay exact, because the port must span them.
    let mut port_lines: [Vec<f64>; 3] = Default::default();
    let mut gap_ends: [Vec<f64>; 3] = Default::default();
    for p in &geom.ports {
        let (a, b, dir) = port_box(p, geom);
        for k in 0..3 {
            port_lines[k].extend([a[k], b[k], (a[k] + b[k]) * 0.5]);
        }
        gap_ends[dir].extend([a[dir], b[dir]]);
    }
    let fine_cell = |k: usize, x: f64| {
        geom.hints.iter().filter(|h| h.max[k] > h.min[k] && x >= h.min[k] - 1e-9 && x <= h.max[k] + 1e-9).map(|h| (h.cell * refine).min(max_cell)).reduce(f64::min)
    };
    let sheet_edge = |hard: &mut Vec<f64>, k: usize, x: f64, inward: f64| match fine_cell(k, x) {
        Some(c) if !port_lines[k].iter().any(|p| (p - x).abs() < c * 0.6) => {
            hard.push(x + inward * c / 3.0);
            hard.push(x - inward * 2.0 * c / 3.0);
        }
        _ => hard.push(x),
    };

    for s in &geom.solids {
        let (a, b) = s.shape.bounds();
        let metal_sheet = known(&s.material).is_some_and(|m| m.is_conductor());
        match &s.shape {
            Shape::Box { min, max } => {
                for k in 0..3 {
                    if metal_sheet && max[k] > min[k] && (0..3).any(|j| max[j] <= min[j]) {
                        sheet_edge(&mut hard[k], k, min[k], 1.0);
                        sheet_edge(&mut hard[k], k, max[k], -1.0);
                    } else {
                        hard[k].push(min[k]);
                        hard[k].push(max[k]);
                    }
                }
            }
            Shape::Cylinder { a: p, b: q, r } => {
                for k in 0..3 {
                    hard[k].extend([p[k], q[k], a[k], b[k]]);
                }
                let cell = (*r / 4.0).min(max_cell);
                for k in 0..3 {
                    regions[k].push((a[k], b[k], cell));
                }
            }
            Shape::Sphere { c, r } => {
                for k in 0..3 {
                    hard[k].extend([c[k] - r, c[k] + r, c[k]]);
                    regions[k].push((c[k] - r, c[k] + r, (*r / 6.0).min(max_cell)));
                }
            }
            Shape::Polygon { axis, at, pts, thick } => {
                hard[*axis].extend([*at, *at + *thick]);
                let (u, v) = ((*axis + 1) % 3, (*axis + 2) % 3);
                for k in [u, v] {
                    if metal_sheet {
                        sheet_edge(&mut hard[k], k, a[k], 1.0);
                        sheet_edge(&mut hard[k], k, b[k], -1.0);
                    } else {
                        hard[k].extend([a[k], b[k]]);
                    }
                }
                // A few corners are worth exact lines; a smooth outline isn't.
                if pts.len() <= 16 {
                    for p in pts {
                        for (k, x) in [(u, p[0]), (v, p[1])] {
                            if x > a[k] + 1e-12 && x < b[k] - 1e-12 {
                                hard[k].push(x);
                            }
                        }
                    }
                }
            }
            Shape::Wire { pts, r } => {
                if pts.len() <= 12 {
                    for p in pts {
                        for k in 0..3 {
                            hard[k].push(p[k]);
                        }
                    }
                }
                if *r >= max_cell / 2.0 {
                    for k in 0..3 {
                        regions[k].push((a[k] - r, b[k] + r, (*r / 2.0).min(max_cell)));
                    }
                }
            }
            Shape::Tris { .. } => {
                for k in 0..3 {
                    hard[k].extend([a[k], b[k]]);
                }
            }
        }
        // Dielectrics: wavelength inside, and at least three cells through a thin one.
        if let Some(m) = known(&s.material)
            && let MatKind::Dielectric { eps_r, .. } = m.kind
        {
            let inside = max_cell / eps_r.sqrt();
            for k in 0..3 {
                let ext = b[k] - a[k];
                if ext <= 0.0 {
                    continue;
                }
                let cell = if ext < 4.0 * inside { ext / 3.0 } else { inside };
                regions[k].push((a[k], b[k], cell));
            }
        }
    }
    for p in &geom.ports {
        let (a, b, _) = port_box(p, geom);
        for k in 0..3 {
            hard[k].extend([a[k], b[k], (a[k] + b[k]) * 0.5]);
        }
        // The gap itself gets at least one cell, and the current probe sits mid-gap.
    }
    for h in &geom.hints {
        let cell = (h.cell * refine).min(max_cell);
        for k in 0..3 {
            if h.max[k] > h.min[k] {
                regions[k].push((h.min[k], h.max[k], cell));
            } else {
                regions[k].push((h.min[k] - 2.0 * cell, h.max[k] + 2.0 * cell, cell));
                hard[k].push(h.min[k]);
            }
        }
    }

    // Field planes: through the middle of the antenna, and just above a flat top.
    let mid = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let mut planes = Vec::new();
    if setup.field_planes {
        let flat = (hi[2] - lo[2]) < 0.1 * lambda0;
        let z_view = if flat { hi[2] + (max_cell * 0.5).min(lambda0 / 40.0) } else { mid[2] };
        let mk = |name: &str, kind: PlaneKind, axis: usize, at: f64| {
            let (mut a, mut b) = (nf_min, nf_max);
            a[axis] = at;
            b[axis] = at;
            PlaneDump { name: name.to_string(), kind, min: a, max: b }
        };
        planes.push(mk("E_xy", PlaneKind::E, 2, z_view));
        planes.push(mk("E_xz", PlaneKind::E, 1, mid[1]));
        planes.push(mk("E_yz", PlaneKind::E, 0, mid[0]));
        if flat {
            planes.push(mk("H_top", PlaneKind::H, 2, z_view));
        }
        hard[2].push(z_view);
        hard[1].push(mid[1]);
        hard[0].push(mid[0]);
    }

    let mesh: [Vec<f64>; 3] = std::array::from_fn(|k| axis_lines_keeping(&hard[k], &gap_ends[k], &regions[k], dom_min[k], dom_max[k], max_cell, 1.4));
    let min_cell = mesh.iter().flat_map(|m| m.windows(2).map(|w| w[1] - w[0])).fold(f64::MAX, f64::min);
    let cells: u64 = mesh.iter().map(|m| m.len().saturating_sub(1).max(1) as u64).product();
    check_cells(cells as f64, setup.f_hi)?;
    if min_cell < max_cell / 60.0 {
        warnings.push(format!("Smallest cell is {:.3} mm against {:.2} mm at most: a very small feature makes the time step short and the run long.", min_cell * 1e3, max_cell * 1e3));
    }
    if cells > 60_000_000 {
        warnings.push(format!("{:.0} million cells: expect a long run and {:.0} GB of memory.", cells as f64 / 1e6, cells as f64 * 90.0 / 1e9));
    }
    if eps_max > 4.0 && cpl < 15.0 {
        warnings.push("A coarse mesh with a high-permittivity material shifts resonances noticeably.".into());
    }
    if geom.solids.iter().any(|s| matches!(s.shape, Shape::Wire { r, .. } if r < max_cell / 2.0)) {
        warnings.push("Thin wires run along mesh lines in FDTD: their radius follows the cell size (about 0.14 cells), not the radius you set.".into());
    }
    let unlossy: Vec<&str> = geom.solids.iter().filter(|s| !is_flat(&s.shape) && known(&s.material).is_some_and(|m| matches!(m.kind, MatKind::Metal { .. }))).map(|s| s.name.as_str()).collect();
    if !unlossy.is_empty() {
        warnings.push(format!("FDTD only models metal loss on flat sheets, so {} stays lossless here and radiation efficiency reads high. The method of moments counts that loss.", unlossy.join(", ")));
    }
    Ok(Plan {
        mesh,
        nf_min,
        nf_max,
        ground: geom.ground,
        f0: center,
        // A narrow sweep needs no narrow pulse: a long pulse only spends time steps.
        fc: ((setup.f_hi - setup.f_lo) / 2.0).max(center * 0.5),
        // Coupling between ports is a small signal: a short ring-down would swamp it.
        end_db: if geom.ports.len() > 1 { setup.end_db.max(50.0) } else { setup.end_db },
        max_steps: setup.max_steps,
        far_freqs: setup.far_freqs.clone(),
        design_f,
        planes,
        max_cell,
        min_cell,
        warnings,
    })
}

fn box_xml(s: &mut String, min: P3, max: P3, prio: i32) {
    let _ = writeln!(
        s,
        r#"          <Box Priority="{prio}"><P1 X="{:.9}" Y="{:.9}" Z="{:.9}"/><P2 X="{:.9}" Y="{:.9}" Z="{:.9}"/></Box>"#,
        min[0], min[1], min[2], max[0], max[1], max[2]
    );
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A sheet triangle thickened into a closed prism (right-handed outward faces).
fn prism(t: &[P3; 3], thick: f64) -> (Vec<P3>, Vec<[usize; 3]>) {
    let n = unit(crate::geometry::cross(sub(t[1], t[0]), sub(t[2], t[0])));
    let h = scale(n, thick.max(1e-6) * 0.5);
    let verts = vec![sub(t[0], h), sub(t[1], h), sub(t[2], h), add(t[0], h), add(t[1], h), add(t[2], h)];
    let faces = vec![[0, 2, 1], [3, 4, 5], [0, 1, 4], [0, 4, 3], [1, 2, 5], [1, 5, 4], [2, 0, 3], [2, 3, 5]];
    (verts, faces)
}

fn polyhedron_xml(s: &mut String, verts: &[P3], faces: &[[usize; 3]], prio: i32) {
    let _ = writeln!(s, r#"          <Polyhedron Priority="{prio}">"#);
    for v in verts {
        let _ = writeln!(s, "            <Vertex>{:.9},{:.9},{:.9}</Vertex>", v[0], v[1], v[2]);
    }
    for f in faces {
        let _ = writeln!(s, "            <Face>{},{},{}</Face>", f[0], f[1], f[2]);
    }
    let _ = writeln!(s, "          </Polyhedron>");
}

fn shape_xml(s: &mut String, shape: &Shape, ports: &[PortDef], prio: i32, max_cell: f64) {
    match shape {
        Shape::Box { min, max } => box_xml(s, *min, *max, prio),
        Shape::Cylinder { a, b, r } => {
            let _ = writeln!(
                s,
                r#"          <Cylinder Priority="{prio}" Radius="{r:.9}"><P1 X="{:.9}" Y="{:.9}" Z="{:.9}"/><P2 X="{:.9}" Y="{:.9}" Z="{:.9}"/></Cylinder>"#,
                a[0], a[1], a[2], b[0], b[1], b[2]
            );
        }
        Shape::Sphere { c, r } => {
            let _ = writeln!(s, r#"          <Sphere Priority="{prio}" Radius="{r:.9}"><Center X="{:.9}" Y="{:.9}" Z="{:.9}"/></Sphere>"#, c[0], c[1], c[2]);
        }
        Shape::Polygon { axis, at, pts, thick } => {
            let (tag, extra) = if *thick > 0.0 { ("LinPoly", format!(r#" Length="{thick:.9}""#)) } else { ("Polygon", String::new()) };
            let _ = writeln!(s, r#"          <{tag} Priority="{prio}" NormDir="{axis}" Elevation="{at:.9}" CoordSystem="0"{extra}>"#);
            for p in pts {
                let _ = writeln!(s, r#"            <Vertex X1="{:.9}" X2="{:.9}"/>"#, p[0], p[1]);
            }
            let _ = writeln!(s, "          </{tag}>");
        }
        Shape::Wire { pts, r } => {
            for piece in clip_wire(pts, ports) {
                if *r >= max_cell / 2.0 {
                    for w in piece.windows(2) {
                        shape_xml(s, &Shape::Cylinder { a: w[0], b: w[1], r: *r }, ports, prio, max_cell);
                    }
                } else {
                    let _ = writeln!(s, r#"          <Curve Priority="{prio}">"#);
                    for p in &piece {
                        let _ = writeln!(s, r#"            <Vertex X="{:.9}" Y="{:.9}" Z="{:.9}"/>"#, p[0], p[1], p[2]);
                    }
                    let _ = writeln!(s, "          </Curve>");
                }
            }
        }
        Shape::Tris { tris, solid, sheet } => {
            if *solid {
                let mut verts = Vec::with_capacity(tris.len() * 3);
                let mut faces = Vec::with_capacity(tris.len());
                for t in tris {
                    let n = verts.len();
                    verts.extend_from_slice(t);
                    faces.push([n, n + 1, n + 2]);
                }
                polyhedron_xml(s, &verts, &faces, prio);
            } else {
                // Thinner than a cell, a tilted sheet can fall between the mesh lines and leak.
                let thick = sheet.max(max_cell * 1.2);
                for t in tris {
                    let (v, f) = prism(t, thick);
                    polyhedron_xml(s, &v, &f, prio);
                }
            }
        }
    }
}

/// Nothing in this model can lose power in an FDTD run: real metal only does on flat
/// sheets, dielectrics only with a loss tangent.
pub fn models_no_loss(geom: &Geometry) -> bool {
    geom.solids.iter().all(|s| {
        material::by_name(&s.material).is_none_or(|m| match m.kind {
            MatKind::Pec => true,
            MatKind::Metal { .. } => !is_flat(&s.shape),
            MatKind::Dielectric { tan_d, .. } => tan_d <= 0.0,
        })
    })
}

/// Flat shapes can be conducting sheets; anything else has to be a solid.
fn is_flat(shape: &Shape) -> bool {
    match shape {
        Shape::Box { min, max } => (0..3).any(|k| max[k] - min[k] <= 0.0),
        Shape::Polygon { thick, .. } => *thick <= 0.0,
        _ => false,
    }
}

/// The whole run with `excite` as the driven port.
pub fn to_xml(geom: &Geometry, plan: &Plan, excite: usize) -> String {
    let mut s = String::new();
    let end = 10f64.powf(-plan.end_db / 10.0);
    let _ = writeln!(s, r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?>"#);
    let _ = writeln!(s, "<openEMS>");
    let _ = writeln!(s, r#"  <FDTD NumberOfTimesteps="{}" endCriteria="{:e}" f_max="{:e}">"#, plan.max_steps, end, plan.f0 + plan.fc);
    let _ = writeln!(s, r#"    <Excitation Type="0" f0="{:e}" fc="{:e}"/>"#, plan.f0, plan.fc);
    let zmin = if plan.ground { "PEC" } else { "PML_8" };
    let _ = writeln!(s, r#"    <BoundaryCond xmin="PML_8" xmax="PML_8" ymin="PML_8" ymax="PML_8" zmin="{zmin}" zmax="PML_8"/>"#);
    let _ = writeln!(s, "  </FDTD>");
    let _ = writeln!(s, r#"  <ContinuousStructure CoordSystem="0">"#);
    let _ = writeln!(s, "    <Properties>");
    let lib = material::library();
    // Group the solids by material; real metal's sheets become conducting sheets.
    let names: Vec<String> = {
        let mut v: Vec<String> = geom.solids.iter().map(|x| x.material.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    for (i, name) in names.iter().enumerate() {
        let m = lib.iter().find(|m| m.name.eq_ignore_ascii_case(name)).cloned().unwrap_or_else(material::Material::pec);
        let of_mat: Vec<&Shape> = geom.solids.iter().filter(|x| &x.material == name).map(|x| &x.shape).collect();
        let tag = xml_escape(&m.name);
        match m.kind {
            MatKind::Dielectric { .. } => {
                let kappa = m.dielectric_kappa(plan.design_f);
                let mu = if let MatKind::Dielectric { mu_r, .. } = m.kind { mu_r } else { 1.0 };
                let _ = writeln!(s, r#"      <Material Name="d{i}_{tag}">"#);
                let _ = writeln!(s, r#"        <Property Epsilon="{}" Kappa="{:e}" Mue="{}"/>"#, m.eps_r(), kappa, mu);
                let _ = writeln!(s, "        <Primitives>");
                for sh in &of_mat {
                    shape_xml(&mut s, sh, &geom.ports, 5, plan.max_cell);
                }
                let _ = writeln!(s, "        </Primitives>");
                let _ = writeln!(s, "      </Material>");
            }
            MatKind::Pec | MatKind::Metal { .. } => {
                let (sheets, solids): (Vec<&Shape>, Vec<&Shape>) = match m.kind {
                    MatKind::Metal { .. } => of_mat.iter().copied().partition(|x| is_flat(x)),
                    _ => (Vec::new(), of_mat.clone()),
                };
                if let MatKind::Metal { sigma } = m.kind
                    && !sheets.is_empty()
                {
                    let _ = writeln!(s, r#"      <ConductingSheet Name="s{i}_{tag}" Conductivity="{sigma:e}" Thickness="{SHEET_THICKNESS:e}">"#);
                    let _ = writeln!(s, "        <Primitives>");
                    for sh in &sheets {
                        shape_xml(&mut s, sh, &geom.ports, 10, plan.max_cell);
                    }
                    let _ = writeln!(s, "        </Primitives>");
                    let _ = writeln!(s, "      </ConductingSheet>");
                }
                if !solids.is_empty() {
                    let _ = writeln!(s, r#"      <Metal Name="m{i}_{tag}">"#);
                    let _ = writeln!(s, "        <Primitives>");
                    for sh in &solids {
                        shape_xml(&mut s, sh, &geom.ports, 10, plan.max_cell);
                    }
                    let _ = writeln!(s, "        </Primitives>");
                    let _ = writeln!(s, "      </Metal>");
                }
            }
        }
    }
    for (idx, p) in geom.ports.iter().enumerate() {
        let n = idx + 1;
        let (a, b, dir) = port_box(p, geom);
        let direction: i32 = if b[dir] >= a[dir] { 1 } else { -1 };
        let lo = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
        let hi = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
        let _ = writeln!(s, r#"      <LumpedElement Name="port_resist_{n}" Direction="{dir}" Caps="1" R="{}">"#, p.z0);
        let _ = writeln!(s, "        <Primitives>");
        box_xml(&mut s, lo, hi, 20);
        let _ = writeln!(s, "        </Primitives>");
        let _ = writeln!(s, "      </LumpedElement>");
        if idx == excite {
            let mut e = [0i32; 3];
            e[dir] = -direction;
            let _ = writeln!(s, r#"      <Excitation Name="port_excite_{n}" Type="0" Excite="{},{},{}">"#, e[0], e[1], e[2]);
            let _ = writeln!(s, "        <Primitives>");
            box_xml(&mut s, lo, hi, 20);
            let _ = writeln!(s, "        </Primitives>");
            let _ = writeln!(s, "      </Excitation>");
        }
        let mut c = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5];
        let (mut ua, mut ub) = (c, c);
        ua[dir] = a[dir];
        ub[dir] = b[dir];
        let _ = writeln!(s, r#"      <ProbeBox Name="port_ut{n}" Type="0" Weight="-1">"#);
        let _ = writeln!(s, "        <Primitives>");
        box_xml(&mut s, ua, ub, 20);
        let _ = writeln!(s, "        </Primitives>");
        let _ = writeln!(s, "      </ProbeBox>");
        c[dir] = (a[dir] + b[dir]) * 0.5;
        let (mut ia, mut ib) = (a, b);
        ia[dir] = c[dir];
        ib[dir] = c[dir];
        let _ = writeln!(s, r#"      <ProbeBox Name="port_it{n}" Type="1" Weight="{direction}" NormDir="{dir}">"#);
        let _ = writeln!(s, "        <Primitives>");
        box_xml(&mut s, ia, ib, 20);
        let _ = writeln!(s, "        </Primitives>");
        let _ = writeln!(s, "      </ProbeBox>");
    }
    // Near-field faces stand λ/4 clear of the antenna, where the field is smooth:
    // one sample per coarse cell is enough, and the DFT runs on every sample.
    let nf_res = format!(r#" OptResolution="{:e}""#, plan.max_cell);
    let dump = |s: &mut String, name: &str, ty: i32, freqs: &[f64], a: P3, b: P3, attr: &str| {
        let _ = writeln!(s, r#"      <DumpBox Name="{}" DumpType="{ty}" DumpMode="1" FileType="0"{attr}>"#, xml_escape(name));
        let f: Vec<String> = freqs.iter().map(|f| format!("{f:e}")).collect();
        let _ = writeln!(s, "        <FD_Samples>{}</FD_Samples>", f.join(","));
        let _ = writeln!(s, "        <Primitives>");
        box_xml(s, a, b, 0);
        let _ = writeln!(s, "        </Primitives>");
        let _ = writeln!(s, "      </DumpBox>");
    };
    for (face, axis, sign) in plan.faces() {
        let (mut a, mut b) = (plan.nf_min, plan.nf_max);
        if sign < 0.0 {
            b[axis] = a[axis];
        } else {
            a[axis] = b[axis];
        }
        dump(&mut s, &format!("nf2ff_E_{face}"), 10, &plan.far_freqs, a, b, &nf_res);
        dump(&mut s, &format!("nf2ff_H_{face}"), 11, &plan.far_freqs, a, b, &nf_res);
    }
    for p in &plan.planes {
        let ty = match p.kind {
            PlaneKind::E => 10,
            PlaneKind::H => 11,
        };
        dump(&mut s, &p.name, ty, &[plan.design_f], p.min, p.max, "");
    }
    let _ = writeln!(s, "    </Properties>");
    let _ = writeln!(s, r#"    <RectilinearGrid DeltaUnit="1" CoordSystem="0">"#);
    for (tag, m) in ["XLines", "YLines", "ZLines"].iter().zip(&plan.mesh) {
        let v: Vec<String> = m.iter().map(|x| format!("{x:.9}")).collect();
        let _ = writeln!(s, "      <{tag}>{}</{tag}>", v.join(","));
    }
    let _ = writeln!(s, "    </RectilinearGrid>");
    let _ = writeln!(s, "  </ContinuousStructure>");
    let _ = writeln!(s, "</openEMS>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::templates;

    #[test]
    fn mesh_keeps_lines_and_grades() {
        let m = axis_lines(&[-0.0005, 0.0005, 0.0105, -0.0105], &[], -0.04, 0.04, 0.002, 1.4);
        for e in [-0.0005, 0.0005, 0.0105, -0.0105, -0.04, 0.04] {
            assert!(m.iter().any(|x| (x - e).abs() < 1e-9), "line {e} missing");
        }
        let cells: Vec<f64> = m.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(cells.iter().all(|c| *c > 0.0 && *c <= 0.002 * 1.0001), "{cells:?}");
        for w in cells.windows(2) {
            let r = w[1].max(w[0]) / w[1].min(w[0]);
            assert!(r < 1.7, "ratio {r} in {cells:?}");
        }
    }

    #[test]
    fn regions_refine_only_where_asked() {
        let m = axis_lines(&[0.0, 0.01], &[(0.0, 0.01, 0.001)], -0.1, 0.1, 0.01, 1.4);
        let inside = m.iter().filter(|x| **x > 0.0 && **x < 0.01).count();
        assert!(inside >= 8, "{inside}");
        let cells: Vec<f64> = m.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(cells.iter().copied().fold(0.0, f64::max) > 0.009);
        assert!(cells.iter().all(|c| *c <= 0.01 * 1.0001));
    }

    #[test]
    fn wires_are_clipped_at_their_ports() {
        let port = PortDef { name: "p".into(), a: [0.0, 0.0, -0.01], b: [0.0, 0.0, 0.01], width: 0.0, z0: 50.0 };
        let v = clip_wire(&[[0.0, 0.0, -1.0], [0.0, 0.0, 1.0]], std::slice::from_ref(&port));
        assert_eq!(v.len(), 2, "{v:?}");
        assert!((v[0][1][2] + 0.01).abs() < 1e-9 && (v[1][0][2] - 0.01).abs() < 1e-9, "{v:?}");
        // A base-fed wire keeps only the part above the gap.
        let base = PortDef { name: "p".into(), a: [0.0, 0.0, 0.0], b: [0.0, 0.0, 0.01], width: 0.0, z0: 50.0 };
        let v = clip_wire(&[[0.0, 0.0, 0.0], [0.0, 0.0, 1.0]], std::slice::from_ref(&base));
        assert_eq!(v.len(), 1);
        assert!((v[0][0][2] - 0.01).abs() < 1e-9);
        // A polyline keeps its corners.
        let v = clip_wire(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]], &[]);
        assert_eq!(v[0].len(), 3);
    }

    #[test]
    fn port_width_follows_the_sheet() {
        let w = |id: &str| {
            let g = Project::from_template(id, None).unwrap().geometry().unwrap();
            width_axis(&g.ports[0], &g)
        };
        assert_eq!(w("strip_dipole"), 0); // plane y = 0, along z
        assert_eq!(w("slot"), 1); // plane z = 0, along x
        assert_eq!(w("horn"), 0); // sheet normal to z, along y
        assert_eq!(w("patch_air"), 0); // between horizontal plates
        assert_eq!(w("vivaldi"), 2); // plane x = 0, along y
    }

    #[test]
    fn every_template_plans_a_run_within_budget() {
        for t in templates::all() {
            if t.id == "custom" {
                continue;
            }
            let p = Project::from_template(t.id, None).unwrap();
            let g = p.geometry().unwrap();
            let plan = plan(&g, &p.setup, p.f0()).unwrap();
            assert!(plan.cells() > 1000, "{}: {} cells", t.id, plan.cells());
            assert!(plan.cells() < 150_000_000, "{}: {} cells", t.id, plan.cells());
            assert!(plan.min_cell > 0.0, "{}", t.id);
            for k in 0..3 {
                assert!(plan.mesh[k].windows(2).all(|w| w[1] > w[0]), "{}: axis {k} not increasing", t.id);
            }
            for port in 0..g.ports.len() {
                let x = to_xml(&g, &plan, port);
                assert!(x.contains(&format!("port_excite_{}", port + 1)), "{}", t.id);
                assert_eq!(x.matches("port_resist_").count(), g.ports.len(), "{}", t.id);
            }
        }
    }

    #[test]
    fn ground_replaces_the_bottom_boundary_and_face() {
        let p = Project::from_template("monopole", None).unwrap();
        let g = p.geometry().unwrap();
        let plan = plan(&g, &p.setup, p.f0()).unwrap();
        let x = to_xml(&g, &plan, 0);
        assert!(x.contains(r#"zmin="PEC""#), "{}", &x[..400]);
        assert!(!x.contains("nf2ff_E_zn"));
        assert_eq!(plan.mesh[2][0], 0.0);
    }

    #[test]
    fn materials_map_to_csx_properties() {
        let mut p = Project::from_template("patch_probe", None).unwrap();
        p.metal = "Copper".into();
        let g = p.geometry().unwrap();
        let plan = plan(&g, &p.setup, p.f0()).unwrap();
        let x = to_xml(&g, &plan, 0);
        assert!(x.contains("<ConductingSheet"), "real metal sheets");
        assert!(x.contains(r#"Conductivity="5.8e7""#));
        assert!(x.contains(r#"Epsilon="4.4""#));
        assert!(x.contains("Kappa="));
    }

    #[test]
    fn port_direction_keeps_its_sign() {
        let mut p = Project::from_template("dipole", None).unwrap();
        p.params.iter_mut().find(|r| r.name == "Z0").unwrap().expr = "50".into();
        let mut g = p.geometry().unwrap();
        let (a, b) = (g.ports[0].a, g.ports[0].b);
        let plan = plan(&g, &p.setup, p.f0()).unwrap();
        assert!(to_xml(&g, &plan, 0).contains(r#"Excite="0,0,-1""#));
        g.ports[0].a = b;
        g.ports[0].b = a;
        assert!(to_xml(&g, &plan, 0).contains(r#"Excite="0,0,1""#));
    }
}

#[cfg(test)]
mod checks {
    use super::*;
    use crate::project::Project;

    /// Clearance is set by the lowest frequency of the sweep, not by its centre.
    #[test]
    fn near_field_box_clears_a_quarter_of_the_longest_wavelength() {
        for id in ["uwb_monopole", "discone", "vivaldi", "dipole", "patch_probe"] {
            let p = Project::from_template(id, None).unwrap();
            let g = p.geometry().unwrap();
            let plan = plan(&g, &p.setup, p.f0()).unwrap();
            let (lo, hi) = g.bounds();
            let want = C0 / p.setup.f_lo / 4.0;
            // What the clearance costs: a low edge moved to the centre gives the old box.
            let mut narrow = p.setup.clone();
            narrow.f_lo = (p.setup.f_lo + p.setup.f_hi) / 2.0;
            println!("{id}: {} cells, {} with a quarter of the centre's wavelength", plan.cells(), super::plan(&g, &narrow, p.f0()).unwrap().cells());
            for k in 0..3 {
                assert!(lo[k] - plan.nf_min[k] >= want * 0.999 || (g.ground && k == 2), "{id} axis {k}: {} of {want}", lo[k] - plan.nf_min[k]);
                assert!(plan.nf_max[k] - hi[k] >= want * 0.999, "{id} axis {k}: {} of {want}", plan.nf_max[k] - hi[k]);
            }
        }
    }

    /// A port's two ends stay on separate lines however small the gap, or it is dead.
    #[test]
    fn a_small_port_gap_keeps_both_its_lines() {
        let mut p = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let g0 = 0.002 * C0 / 1e9;
        p.params.iter_mut().find(|r| r.name == "g").unwrap().expr = format!("{g0:e}");
        let g = p.geometry().unwrap();
        let plan = plan(&g, &p.setup, p.f0()).unwrap();
        let z = &plan.mesh[2];
        for at in [-g0 / 2.0, g0 / 2.0] {
            assert!(z.iter().any(|x| (x - at).abs() < 1e-9), "no line at {at}: nearest of {z:?}");
        }
    }

    #[test]
    fn lossless_real_metal_is_called_out() {
        let says = |id: &str, metal: &str| {
            let mut p = Project::from_template(id, None).unwrap();
            p.metal = metal.into();
            let g = p.geometry().unwrap();
            plan(&g, &p.setup, p.f0()).unwrap().warnings.iter().any(|w| w.contains("lossless"))
        };
        assert!(says("monopole", "Stainless steel"), "wires");
        assert!(!says("monopole", "PEC"), "PEC is meant to be lossless");
        assert!(!says("strip_dipole", "Copper"), "flat sheets carry their loss");
    }

    #[test]
    fn an_unknown_material_cannot_reach_a_run() {
        let p = Project::from_template("dipole", Some("1GHz")).unwrap();
        let mut g = p.geometry().unwrap();
        g.solids[0].material = "Unobtainium".into();
        let e = plan(&g, &p.setup, p.f0()).unwrap_err().to_string();
        assert!(e.contains("Unobtainium"), "{e}");
    }
}

#[cfg(test)]
mod thirds {
    use super::*;
    use crate::project::Project;

    /// A radiating edge sits between lines, one third of a cell inside the metal;
    /// edges at a port stay exactly on a line.
    #[test]
    fn sheet_edges_follow_the_thirds_rule() {
        let p = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let g = p.geometry().unwrap();
        let plan = plan(&g, &p.setup, p.f0()).unwrap();
        let vars = p.vars().unwrap();
        let (l, gap) = (vars["L"], vars["g"]);
        let z = &plan.mesh[2];
        let near = |x: f64| z.iter().copied().min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs())).unwrap();
        // The tip: no line on it, the nearest is just inside.
        let tip = l / 2.0;
        assert!((near(tip) - tip).abs() > 1e-6, "a line sits on the tip");
        assert!(near(tip) < tip, "nearest line {} should be inside {tip}", near(tip));
        // The gap edge: on a line.
        assert!((near(gap / 2.0) - gap / 2.0).abs() < 1e-9, "the port edge must be on a line");
    }
}
