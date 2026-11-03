//! The built-in time-domain solver: turns a [`Geometry`] into a `fdtd_kernel` problem, runs it and
//! returns the same [`SimResult`] as the other solvers.
//!
//! The mesh differs from the one made for openEMS. That one has to put lines around every metal
//! edge and through thin layers, because a cell there either holds metal or doesn't. Here thin metal
//! and thin layers are handled inside the cell, so lines only go where the model truly needs them:
//! the plane of a sheet, a port's gap, the middle of a strip too narrow to hold a line, the box the
//! far field comes from. Fewer small cells means a longer time step, which is where the speed goes.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use fdtd_kernel::assemble::{Assembled, assemble};
use fdtd_kernel::engine::{Engine, Stop, Waves, stable_dt, waves};
use fdtd_kernel::grid::Grid;
use fdtd_kernel::problem::{self as kp, Body, Debye, Extrude, Mat, Port, Problem, Pulse, Sheet, Surface, Wall, Wire, polygon_line};
use fdtd_kernel::waveport::{WavePortDef, WavePortRun, beta, waves_at};
use fdtd_kernel::record::{Recorder, Window};
use rayon::prelude::*;

use crate::csx::{axis_lines_keeping, clip_wire, port_box, width_axis};
use crate::farfield::FarField;
use crate::geometry::{Geometry, P3, Shape, cross, norm, sub};
use crate::material::{self, MatKind};
use crate::project::{FieldPlane, Project, STOPPED_SHORT, SimResult};
use crate::sparam::Network;
use crate::{C0, C64, ETA0, checks, linspace};

/// Copper thickness assumed for real-metal sheets, the same as the openEMS path.
const SHEET_THICKNESS: f64 = 35e-6;
/// Absorber depth in cells.
const PML: usize = 8;
/// More cells than one PC can run, as on the openEMS path.
const MAX_CELLS: f64 = 1e10;

/// Which engine runs the time stepping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Backend {
    /// The GPU if there is one and the model is big enough to be worth it.
    #[default]
    Auto,
    Cpu,
    Gpu,
}

/// Below this many cells the driver's overhead on a step eats what the GPU saves. Measured on an RTX
/// A2000 with other work on the CPU: 2.4 s against 3.5 s at 0.23 M cells, 16.7 s against 26.9 s at 0.6 M.
#[cfg(feature = "gpu")]
pub(crate) const GPU_FROM_CELLS: u64 = 300_000;

/// What a run may be told beyond the project's setup.
#[derive(Clone, Debug)]
pub struct Options {
    /// Worker threads; 0 chooses from the mesh and the machine.
    pub threads: usize,
    /// Correct the cells next to thin metal. Off is the plain grid, for comparison.
    pub subcell: bool,
    /// Prefix for progress messages.
    pub label: String,
    pub backend: Backend,
    /// Extra fine regions per axis, `(from, to, cell)`, as adaptive passes choose them.
    pub refine: [Vec<(f64, f64, f64)>; 3],
    /// The time step as a share of the measured limit.
    pub dt_scale: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self { threads: 0, subcell: true, label: String::new(), backend: Backend::Auto, refine: Default::default(), dt_scale: 1.0 }
    }
}

/// What a run tells whoever started it.
pub struct Hooks<'a> {
    pub cancel: &'a AtomicBool,
    pub busy: &'a (dyn Fn(String, f32) + Sync),
    pub log: &'a (dyn Fn(String) + Sync),
}

/// A near-field or viewing plane, in the grid.
#[derive(Clone, Debug)]
pub struct ViewPlane {
    pub name: String,
    pub window: Window,
    pub magnetic: bool,
}

/// Which engine list a port is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Lumped(usize),
    Wave(usize),
}

/// A wave port in the grid, with the part of the box its line passes through.
#[derive(Clone, Debug)]
pub struct WaveLine {
    pub name: String,
    pub def: WavePortDef,
    /// Coordinates along `axis + 1` and `axis + 2`, inside which the near-field box is the line's own
    /// guided wave leaving, not radiation.
    pub rim: [(f64, f64); 2],
}

/// Everything about a run except which port drives it.
#[derive(Clone, Debug)]
pub struct Plan {
    pub routes: Vec<Route>,
    pub waves: Vec<WaveLine>,
    pub problem: Problem,
    pub nf: [Window; 6],
    /// Faces of the near-field box in use, with their outward signs.
    pub faces: Vec<(usize, f64)>,
    pub ground: bool,
    pub f0: f64,
    pub fc: f64,
    pub end_db: f64,
    pub max_steps: u64,
    pub far_freqs: Vec<f64>,
    pub design_f: f64,
    pub views: Vec<ViewPlane>,
    pub max_cell: f64,
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn cells(&self) -> u64 {
        self.problem.grid.cells()
    }
    pub fn min_cell(&self) -> f64 {
        self.problem.grid.min_cell()
    }
}

/// A sheet's outline in its own `(axis + 1, axis + 2)` coordinates, its plane and its thickness. Metal
/// thinner than `limit` counts as a sheet through its middle: no pair of lines could hold it, and it
/// would otherwise cover no edge and vanish.
fn sheet_outline(shape: &Shape, limit: f64) -> Option<(usize, f64, Vec<[f64; 2]>, f64)> {
    match shape {
        Shape::Box { min, max } => {
            let axis = (0..3).min_by(|a, b| (max[*a] - min[*a]).total_cmp(&(max[*b] - min[*b])))?;
            let thick = (max[axis] - min[axis]).max(0.0);
            if thick >= limit {
                return None;
            }
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            Some((axis, (min[axis] + max[axis]) * 0.5, vec![[min[u], min[v]], [max[u], min[v]], [max[u], max[v]], [min[u], max[v]]], thick))
        }
        Shape::Polygon { axis, at, pts, thick } if *thick < limit => Some((*axis, *at + thick.max(0.0) * 0.5, pts.clone(), thick.max(0.0))),
        _ => None,
    }
}

/// Centre lines of straight strips narrower than `limit`, along whichever way they run. Only a
/// strip that keeps its centre over two samples gets a line: a slanting one would ask for a line
/// per sample, and is stepped along the lines there are.
fn narrow_centres(pts: &[[f64; 2]], along_u: bool, step: f64, limit: f64, out: &mut Vec<f64>) {
    let f = if along_u { 1 } else { 0 };
    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |(l, h), p| (l.min(p[f]), h.max(p[f])));
    let count = (((hi - lo) / step).ceil() as usize).clamp(2, 4000);
    let mut last: Vec<f64> = Vec::new();
    let mut iv = Vec::new();
    for s in 0..count {
        let at = lo + (hi - lo) * (s as f64 + 0.5) / count as f64;
        iv.clear();
        polygon_line(pts, along_u, at, &mut iv);
        let mut here = Vec::new();
        for (a, b) in iv.iter() {
            if b - a < limit {
                let c = (a + b) * 0.5;
                if last.iter().any(|x: &f64| (x - c).abs() < limit * 1e-3) {
                    out.push(c);
                }
                here.push(c);
            }
        }
        last = here;
    }
}

/// The kernel's material for a library one.
fn mat_of(m: &material::Material, design_f: f64) -> Mat {
    match m.kind {
        MatKind::Pec => Mat::Pec,
        MatKind::Metal { sigma } => Mat::Metal { sigma },
        MatKind::Dielectric { eps_r, mu_r, .. } => {
            if m.name == "Water" {
                // Debye at 25 °C, the same curve `dielectric_kappa` follows, in the time domain.
                Mat::Dielectric { eps_r: 5.2, mu_r, kappa: 0.0, debye: Some(Debye { delta_eps: 73.2, tau: 8.27e-12 }) }
            } else {
                Mat::Dielectric { eps_r, mu_r, kappa: m.dielectric_kappa(design_f), debye: None }
            }
        }
    }
}

/// A sheet triangle thickened into a closed prism.
fn prism_tris(t: &[P3; 3], thick: f64) -> Vec<[P3; 3]> {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let len = norm(n).max(1e-300);
    let h = [n[0] / len * thick * 0.5, n[1] / len * thick * 0.5, n[2] / len * thick * 0.5];
    let down = |p: P3| [p[0] - h[0], p[1] - h[1], p[2] - h[2]];
    let up = |p: P3| [p[0] + h[0], p[1] + h[1], p[2] + h[2]];
    let (a, b, c) = (down(t[0]), down(t[1]), down(t[2]));
    let (d, e, f) = (up(t[0]), up(t[1]), up(t[2]));
    vec![[a, c, b], [d, e, f], [a, b, e], [a, e, d], [b, c, f], [b, f, e], [c, a, d], [c, d, f]]
}

/// How wide the narrowest slanting strip in an outline really is, of those narrower across a cut than
/// `limit`. One width all along tells a strip from the tip of a wider shape, which narrows quickly.
fn slanted_strip_width(pts: &[[f64; 2]], limit: f64) -> Option<f64> {
    let mut best = None;
    for along_u in [true, false] {
        let f = if along_u { 1 } else { 0 };
        let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |(l, h), p| (l.min(p[f]), h.max(p[f])));
        let count = (((hi - lo) / (limit * 0.5)).ceil() as usize).clamp(8, 4000);
        let step = (hi - lo) / count as f64;
        let mut last: Vec<(f64, f64)> = Vec::new();
        let mut iv = Vec::new();
        for s in 0..count {
            let at = lo + (s as f64 + 0.5) * step;
            iv.clear();
            polygon_line(pts, along_u, at, &mut iv);
            let mut here = Vec::new();
            for (a, b) in iv.iter() {
                let w = b - a;
                if w >= limit {
                    continue;
                }
                let c = (a + b) * 0.5;
                for (pc, pw) in &last {
                    let slope = (c - pc).abs() / step;
                    if slope > 1e-3 && slope <= 1.0 + 1e-6 && w.min(*pw) > 0.8 * w.max(*pw) {
                        let true_width = w / (1.0 + slope * slope).sqrt();
                        best = Some(best.map_or(true_width, |b: f64| b.min(true_width)));
                    }
                }
                here.push((c, w));
            }
            last = here;
        }
    }
    best
}

/// Which way a wave port's line runs: along one of the axes across its gap, the model has to lie all
/// on one side of the feed. Fails if the feed is inside the model, where its line can't reach the
/// absorber.
fn wave_direction(name: &str, c: P3, gap_axis: usize, lo: P3, hi: P3, tol: f64) -> Result<(usize, f64)> {
    let mut best = None;
    for k in (0..3).filter(|k| *k != gap_axis) {
        let extent = hi[k] - lo[k];
        let edge = if (c[k] - lo[k]).abs() <= tol {
            1.0
        } else if (hi[k] - c[k]).abs() <= tol {
            -1.0
        } else {
            continue;
        };
        // In a corner, the longer way is along the line.
        if best.is_none_or(|(_, _, e)| extent > e) {
            best = Some((k, edge, extent));
        }
    }
    best.map(|(k, d, _)| (k, d)).ok_or_else(|| anyhow::anyhow!("Feed '{name}' is a wave port, but isn't on the edge of the model. Its line has to run straight out through an outer face, so end the line at the board's edge and put the feed there."))
}

/// Builds the mesh and the problem.
pub fn plan(geom: &Geometry, setup: &crate::project::Setup, design_f: f64, subcell: bool, extra: &[Vec<(f64, f64, f64)>; 3]) -> Result<Plan> {
    let built;
    let geom = if geom.stack.is_some() {
        built = geom.with_stack_built();
        &built
    } else {
        geom
    };
    setup.check_band()?;
    let lambda_min = C0 / setup.f_hi;
    let center = (setup.f_lo + setup.f_hi) / 2.0;
    let cpl = setup.cells_per_lambda.clamp(6.0, 200.0);
    let max_cell = lambda_min / cpl;
    let refine = 20.0 / cpl;
    let lib = material::library();
    let known = |n: &str| lib.iter().find(|m| m.name.eq_ignore_ascii_case(n));
    if let Some(s) = geom.solids.iter().find(|s| known(&s.material).is_none()) {
        bail!("Part '{}' is made of '{}', which isn't in the material library.", s.name, s.material);
    }
    let (lo, hi) = geom.bounds();
    let nf_pad = C0 / setup.f_lo / 4.0;
    let pml_pad = (PML as f64 + 4.0) * max_cell;
    let mut nf_min = [lo[0] - nf_pad, lo[1] - nf_pad, lo[2] - nf_pad];
    let nf_max = [hi[0] + nf_pad, hi[1] + nf_pad, hi[2] + nf_pad];
    let mut dom_min = [nf_min[0] - pml_pad, nf_min[1] - pml_pad, nf_min[2] - pml_pad];
    let dom_max = [nf_max[0] + pml_pad, nf_max[1] + pml_pad, nf_max[2] + pml_pad];
    if geom.ground {
        nf_min[2] = 0.0;
        dom_min[2] = 0.0;
    }
    let rough: f64 = (0..3).map(|k| ((dom_max[k] - dom_min[k]) / max_cell).max(1.0)).product();
    if rough > MAX_CELLS {
        bail!("At {:.3} GHz this model comes to about {:.0} billion cells, far more than a run can use. Check the sweep band and the model's size.", setup.f_hi / 1e9, rough / 1e9);
    }

    let mut hard: [Vec<f64>; 3] = Default::default();
    let mut keep: [Vec<f64>; 3] = Default::default();
    let mut regions: [Vec<(f64, f64, f64)>; 3] = Default::default();
    for k in 0..3 {
        hard[k].extend([nf_min[k], nf_max[k]]);
    }
    // The finest cell a hint asks for near a coordinate, for judging what counts as narrow.
    let fine_cell = {
        let mut c = max_cell;
        for h in &geom.hints {
            c = c.min((h.cell * refine).min(max_cell));
        }
        c
    };
    let mut warnings = Vec::new();
    let mut problem_sheets: Vec<Sheet> = Vec::new();
    let mut bodies: Vec<Body> = Vec::new();
    let mut wires: Vec<Wire> = Vec::new();
    // Dielectrics first, so that metal wins where they overlap.
    let order: Vec<&crate::geometry::Solid> = {
        let mut v: Vec<&crate::geometry::Solid> = geom.solids.iter().collect();
        v.sort_by_key(|s| known(&s.material).is_some_and(|m| m.is_conductor()));
        v
    };
    for s in order {
        let m = known(&s.material).unwrap();
        let (a, b) = s.shape.bounds();
        if m.is_conductor() {
            if let Some((axis, at, pts, thick)) = sheet_outline(&s.shape, fine_cell * 0.5) {
                // Kept, not just wanted: two sheets close together merged onto one line are a short.
                keep[axis].push(at);
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let (mut cu, mut cv) = (Vec::new(), Vec::new());
                let limit = fine_cell * 1.5;
                // Cuts along u give centres in u, for a strip running along v, and the other way about.
                narrow_centres(&pts, true, fine_cell * 0.5, limit, &mut cu);
                narrow_centres(&pts, false, fine_cell * 0.5, limit, &mut cv);
                hard[u].extend(cu);
                hard[v].extend(cv);
                // Its current runs on mesh edges, so a slanting strip needs some across it: two cells per width
                // read 0.6 % from the method of moments at 45 degrees, where a strip 10 times narrower than a
                // cell read 31 % low. The cap keeps one hairline from setting the time step of the whole run.
                if let Some(w) = slanted_strip_width(&pts, fine_cell * 2.0) {
                    let cell = (w / 2.0).max(max_cell / 24.0);
                    if cell < fine_cell {
                        let (mut l, mut h) = ([f64::MAX; 2], [f64::MIN; 2]);
                        for q in &pts {
                            for c in 0..2 {
                                l[c] = l[c].min(q[c]);
                                h[c] = h[c].max(q[c]);
                            }
                        }
                        regions[u].push((l[0], h[0], cell));
                        regions[v].push((l[1], h[1], cell));
                        warnings.push(format!("'{}' has a slanting trace {:.2} mm wide, so the mesh around it went down to {:.2} mm. Drawn along x or y it would run far faster.", s.name, w * 1e3, cell * 1e3));
                    }
                }
                let surface = match m.kind {
                    MatKind::Metal { sigma } => Some(Surface::sheet(sigma, if thick > 0.0 { thick } else { SHEET_THICKNESS }, design_f)),
                    _ => None,
                };
                problem_sheets.push(Sheet { axis, at, polys: vec![pts], surface });
                continue;
            }
            match &s.shape {
                Shape::Wire { pts, r } => {
                    let surface = match m.kind {
                        MatKind::Metal { sigma } => Some(Surface::skin(sigma, design_f)),
                        _ => None,
                    };
                    for piece in clip_wire(pts, &geom.ports) {
                        if *r >= fine_cell / 2.0 {
                            for w in piece.windows(2) {
                                bodies.push(Body { solid: kp::Solid::Cylinder { a: w[0], b: w[1], r: *r }, mat: mat_of(m, design_f) });
                            }
                            for k in 0..3 {
                                regions[k].push((a[k] - r, b[k] + r, (*r / 2.0).min(max_cell)));
                            }
                        } else {
                            if piece.len() <= 64 {
                                for p in &piece {
                                    for k in 0..3 {
                                        hard[k].push(p[k]);
                                    }
                                }
                            }
                            wires.push(Wire { pts: piece, r: *r, surface });
                        }
                    }
                    continue;
                }
                Shape::Tris { tris, solid: false, sheet } => {
                    // A slanted sheet: thicker than a cell, or it can fall between lines and leak.
                    let thick = sheet.max(max_cell * 1.2);
                    for k in 0..3 {
                        hard[k].extend([a[k], b[k]]);
                    }
                    for t in tris {
                        bodies.push(Body { solid: kp::Solid::Mesh { tris: prism_tris(t, thick) }, mat: mat_of(m, design_f) });
                    }
                    continue;
                }
                _ => {}
            }
        }
        // Solids, of any material.
        let solid = match &s.shape {
            Shape::Box { min, max } => kp::Solid::Box { min: *min, max: *max },
            Shape::Cylinder { a, b, r } => kp::Solid::Cylinder { a: *a, b: *b, r: *r },
            Shape::Sphere { c, r } => kp::Solid::Sphere { c: *c, r: *r },
            Shape::Polygon { axis, at, pts, thick } => kp::Solid::Prism { axis: *axis, at: *at, pts: pts.clone(), thick: *thick },
            Shape::Tris { tris, .. } => kp::Solid::Mesh { tris: tris.clone() },
            Shape::Wire { pts, r } => {
                // A dielectric wire is a string of cylinders.
                for w in pts.windows(2) {
                    bodies.push(Body { solid: kp::Solid::Cylinder { a: w[0], b: w[1], r: *r }, mat: mat_of(m, design_f) });
                }
                continue;
            }
        };
        match &s.shape {
            Shape::Cylinder { r, .. } => {
                let cell = (*r / 4.0).min(max_cell);
                for k in 0..3 {
                    regions[k].push((a[k], b[k], cell));
                }
            }
            Shape::Sphere { r, .. } => {
                let cell = (*r / 6.0).min(max_cell);
                for k in 0..3 {
                    regions[k].push((a[k], b[k], cell));
                }
            }
            _ => {}
        }
        // Metal solids need their faces on lines to be sharp. A dielectric's faces are averaged
        // into the cells they cut, and a layer thinner than a cell asks for no lines at all.
        if m.is_conductor() {
            for k in 0..3 {
                hard[k].extend([a[k], b[k]]);
            }
        }
        if let MatKind::Dielectric { eps_r, .. } = m.kind {
            let inside = max_cell / eps_r.sqrt();
            for k in 0..3 {
                let ext = b[k] - a[k];
                if ext > 2.0 * inside {
                    regions[k].push((a[k], b[k], inside));
                    hard[k].extend([a[k], b[k]]);
                }
            }
        }
        bodies.push(Body { solid, mat: mat_of(m, design_f) });
    }
    // Ports: the gap's two ends stay on lines however close, and so does its centre if wide.
    let mut ports = Vec::new();
    let mut routes = Vec::with_capacity(geom.ports.len());
    // Name, line axis, direction, ends, sideways axis, and the rim as gap-axis and sideways ranges.
    type Asked = (String, usize, f64, P3, P3, usize, (f64, f64), (f64, f64));
    let mut asked: Vec<Asked> = Vec::new();
    for (index, p) in geom.ports.iter().enumerate() {
        let (a, b, dir) = port_box(p, geom);
        if geom.ground && a[2].abs() < max_cell * 1e-3 && b[2].abs() < max_cell * 1e-3 {
            bail!("Feed '{}' lies flat on the ground plane, where nothing can drive it. Lift it, or run it up from the ground.", p.name);
        }
        keep[dir].extend([a[dir], b[dir]]);
        for k in 0..3 {
            if k != dir {
                hard[k].push((a[k] + b[k]) * 0.5);
            }
        }
        if geom.wave_ports.contains(&index) {
            let c = p.center();
            let (axis, direction) = wave_direction(&p.name, c, dir, lo, hi, fine_cell * 0.01)?;
            let side = 3 - axis - dir;
            keep[axis].push(c[axis]);
            let h = (p.b[dir] - p.a[dir]).abs();
            // Far enough out that the mode has died away, on all sides of the gap and the strip.
            let reach = (6.0 * h).max(3.0 * p.width);
            let low = if geom.ground { (a[dir].min(b[dir]) - reach).max(0.0) } else { a[dir].min(b[dir]) - reach };
            let rim_gap = (low, a[dir].max(b[dir]) + reach);
            let rim_side = (c[side] - p.width / 2.0 - reach, c[side] + p.width / 2.0 + reach);
            hard[dir].extend([rim_gap.0, rim_gap.1]);
            hard[side].extend([rim_side.0, rim_side.1]);
            // The line's impedance comes from this cross-section, so it needs some cells of its own.
            regions[dir].push((a[dir].min(b[dir]), a[dir].max(b[dir]), (h / 2.0).min(max_cell)));
            if p.width > 0.0 {
                regions[side].push((c[side] - p.width / 2.0 - h, c[side] + p.width / 2.0 + h, (p.width / 2.0).min(h).min(max_cell)));
            }
            routes.push(Route::Wave(asked.len()));
            asked.push((p.name.clone(), axis, direction, p.a, p.b, side, rim_gap, rim_side));
            continue;
        }
        let (wa, width) = if p.width > 0.0 { (Some(width_axis(p, geom)), p.width) } else { (None, 0.0) };
        routes.push(Route::Lumped(ports.len()));
        ports.push(Port { a, b, axis: dir, width_axis: wa, width, second: None, z0: p.z0 });
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
    for k in 0..3 {
        regions[k].extend(extra[k].iter().copied());
    }
    // Field planes: through the middle of the antenna, and just above a flat top.
    let mid = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let lambda0 = C0 / center;
    let flat = (hi[2] - lo[2]) < 0.1 * lambda0;
    let z_view = if flat { hi[2] + (max_cell * 0.5).min(lambda0 / 40.0) } else { mid[2] };
    if setup.field_planes {
        hard[2].push(z_view);
        hard[1].push(mid[1]);
        hard[0].push(mid[0]);
    }

    // The same plane reached two ways differs in its last digits. Left as two lines those are a
    // cell a few atoms thick, and a time step to match.
    let same = (fine_cell * 1e-3).max(1e-9);
    for k in 0..3 {
        keep[k].sort_by(f64::total_cmp);
        keep[k].dedup_by(|a, b| (*a - *b).abs() < same);
        for h in hard[k].iter_mut() {
            if let Some(x) = keep[k].iter().find(|x| (**x - *h).abs() < same) {
                *h = *x;
            }
        }
    }
    let lines: [Vec<f64>; 3] = std::array::from_fn(|k| axis_lines_keeping(&hard[k], &keep[k], &regions[k], dom_min[k], dom_max[k], max_cell, 1.4));
    // An absorber reads best on cells that don't change much; these are at the coarse end anyway.
    let grid = Grid::new(lines);
    let cells = grid.cells();
    if grid.min_cell() < max_cell / 200.0 {
        warnings.push(format!("Smallest cell is {:.4} mm against {:.2} mm at most: two features very close together along one axis make the time step short and the run long.", grid.min_cell() * 1e3, max_cell * 1e3));
    }
    if cells > 200_000_000 {
        warnings.push(format!("{:.0} million cells: expect a long run and {:.0} GB of memory.", cells as f64 / 1e6, cells as f64 * 60.0 / 1e9));
    }

    let walls = [Wall::Pml(PML), Wall::Pml(PML), Wall::Pml(PML), Wall::Pml(PML), if geom.ground { Wall::Pec } else { Wall::Pml(PML) }, Wall::Pml(PML)];
    let nf_lo: [usize; 3] = std::array::from_fn(|k| grid.nearest(k, nf_min[k]));
    let nf_hi: [usize; 3] = std::array::from_fn(|k| grid.nearest(k, nf_max[k]));
    let nf: [Window; 6] = std::array::from_fn(|f| {
        let axis = f / 2;
        let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
        Window { axis, plane: if f % 2 == 0 { nf_lo[axis] } else { nf_hi[axis] }, r1: (nf_lo[t1], nf_hi[t1]), r2: (nf_lo[t2], nf_hi[t2]) }
    });
    let mut faces = Vec::new();
    for axis in 0..3 {
        if !(geom.ground && axis == 2) {
            faces.push((axis * 2, -1.0));
        }
        faces.push((axis * 2 + 1, 1.0));
    }
    let mut views = Vec::new();
    if setup.field_planes {
        let window_at = |axis: usize, at: f64| {
            let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
            Window { axis, plane: grid.nearest(axis, at), r1: (nf_lo[t1], nf_hi[t1]), r2: (nf_lo[t2], nf_hi[t2]) }
        };
        views.push(ViewPlane { name: "E_xy".into(), window: window_at(2, z_view), magnetic: false });
        views.push(ViewPlane { name: "E_xz".into(), window: window_at(1, mid[1]), magnetic: false });
        views.push(ViewPlane { name: "E_yz".into(), window: window_at(0, mid[0]), magnetic: false });
        if flat {
            views.push(ViewPlane { name: "H_top".into(), window: window_at(2, z_view), magnetic: true });
        }
    }
    let mut problem = Problem::new(grid);
    problem.walls = walls;
    problem.bodies = bodies;
    problem.sheets = problem_sheets;
    problem.wires = wires;
    problem.ports = ports;
    problem.subcell = subcell;
    let mut waves = Vec::with_capacity(asked.len());
    for (name, axis, direction, a, b, side, rim_gap, rim_side) in asked {
        let g = &problem.grid;
        let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
        let gap = 3 - axis - side;
        let range = |k: usize, r: (f64, f64)| -> (usize, usize) {
            let last = g.n()[k] - 1;
            // Clear of the absorber; a ground plane is a wall that a rim can sit on.
            let floor = if k == 2 && geom.ground { 0 } else { PML + 2 };
            (g.nearest(k, r.0).max(floor), g.nearest(k, r.1).min(last - (PML + 2)))
        };
        let of = |k: usize| if k == gap { rim_gap } else { rim_side };
        let plane = g.nearest(axis, a[axis]);
        if direction > 0.0 && plane <= PML + 1 || direction < 0.0 && plane + PML + 2 >= g.n()[axis] {
            bail!("Feed '{name}' is too near the absorber for a wave port.");
        }
        let def = WavePortDef {
            axis,
            plane,
            r1: range(t1, of(t1)),
            r2: range(t2, of(t2)),
            direction,
            signal: (g.nearest(t1, b[t1]), g.nearest(t2, b[t2])),
            ground: Some((g.nearest(t1, a[t1]), g.nearest(t2, a[t2]))),
        };
        let coords = |k: usize, r: (usize, usize)| (g.lines[k][r.0], g.lines[k][r.1]);
        let rim = [coords(t1, def.r1), coords(t2, def.r2)];
        problem.extrude.push(Extrude { axis, plane, direction });
        waves.push(WaveLine { name, def, rim });
    }
    Ok(Plan {
        problem,
        routes,
        waves,
        nf,
        faces,
        ground: geom.ground,
        f0: center,
        // A narrow sweep needs no narrow pulse: a long pulse only spends time steps.
        fc: ((setup.f_hi - setup.f_lo) / 2.0).max(center * 0.5),
        // Coupling between ports is a small signal: a short ring-down would swamp it.
        end_db: if geom.ports.len() > 1 { setup.end_db.max(50.0) } else { setup.end_db },
        max_steps: setup.max_steps,
        far_freqs: setup.far_freqs.clone(),
        design_f,
        views,
        max_cell,
        warnings,
    })
}

/// One sample of the equivalent currents on the box.
type SurfacePoint = ([f64; 3], [C64; 3], [C64; 3], f64);

fn cx(v: num_complex::Complex64) -> C64 {
    C64::new(v.re, v.im)
}

/// The box's equivalent currents, from the recorded windows: J = n×H, M = −n×E.
fn box_points(plan: &Plan, planes: &[fdtd_kernel::record::Plane]) -> Vec<SurfacePoint> {
    let grid = &plan.problem.grid;
    let mut pts = Vec::new();
    for (face, sign) in &plan.faces {
        let win = plan.nf[*face];
        let Some(rec) = planes.iter().find(|p| p.window == win) else { continue };
        let a = win.axis;
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        // A feed line leaving through this face carries its guided wave across it. That isn't radiation:
        // left in, the small aperture it passes through would be taken for an antenna.
        let lines_out: Vec<[(f64, f64); 2]> = plan.waves.iter().filter(|w| w.def.axis == a && w.def.direction * *sign < 0.0).map(|w| w.rim).collect();
        let (m1, m2) = rec.dims();
        for b in 0..m2 {
            for i in 0..m1 {
                let cell = rec.data[i + m1 * b];
                let (i1, i2) = (win.r1.0 + i, win.r2.0 + b);
                let (l1, l2) = (grid.primal(t1, i1), grid.primal(t2, i2));
                let mut pos = [0.0; 3];
                pos[a] = grid.lines[a][win.plane];
                pos[t1] = grid.lines[t1][i1] + l1 * 0.5;
                pos[t2] = grid.lines[t2][i2] + l2 * 0.5;
                if lines_out.iter().any(|r| pos[t1] > r[0].0 && pos[t1] < r[0].1 && pos[t2] > r[1].0 && pos[t2] < r[1].1) {
                    continue;
                }
                // n×(v1·t1 + v2·t2) = sign·(v1·t2 − v2·t1), as (a, t1, t2) is right-handed.
                let (e1, e2, h1, h2) = (cx(cell[0]), cx(cell[1]), cx(cell[2]), cx(cell[3]));
                let mut j = [C64::new(0.0, 0.0); 3];
                j[t2] = h1 * *sign;
                j[t1] = -h2 * *sign;
                let mut m = [C64::new(0.0, 0.0); 3];
                m[t2] = -e1 * *sign;
                m[t1] = e2 * *sign;
                pts.push((pos, j, m, l1 * l2));
            }
        }
    }
    if plan.ground {
        let img: Vec<SurfacePoint> = pts.iter().map(|(p, j, m, a)| ([p[0], p[1], -p[2]], [-j[0], -j[1], j[2]], [m[0], m[1], -m[2]], *a)).collect();
        pts.extend(img);
    }
    pts
}

/// Near field to far field on the closed box.
fn far_field(plan: &Plan, planes: &[fdtd_kernel::record::Plane], f: f64, step: f64, scale: f64) -> FarField {
    let k = 2.0 * std::f64::consts::PI * f / C0;
    let pts = box_points(plan, planes);
    let mut ff = FarField::grid(f, step, step);
    ff.ground = plan.ground;
    let (thetas, phis) = (ff.theta.clone(), ff.phi.clone());
    let prefactor = C64::new(0.0, k / (4.0 * std::f64::consts::PI) * scale);
    let rows: Vec<(Vec<C64>, Vec<C64>)> = thetas
        .par_iter()
        .map(|th| {
            let (st, ct) = th.to_radians().sin_cos();
            let mut et = vec![C64::new(0.0, 0.0); phis.len()];
            let mut ep = et.clone();
            if plan.ground && *th > 90.0 {
                return (et, ep);
            }
            for (pi, ph) in phis.iter().enumerate() {
                let (sp, cp) = ph.to_radians().sin_cos();
                let r = [st * cp, st * sp, ct];
                let mut n = [C64::new(0.0, 0.0); 3];
                let mut l = [C64::new(0.0, 0.0); 3];
                for (p, jv, mv, a) in &pts {
                    let ph = C64::from_polar(*a, k * (p[0] * r[0] + p[1] * r[1] + p[2] * r[2]));
                    for c in 0..3 {
                        n[c] += jv[c] * ph;
                        l[c] += mv[c] * ph;
                    }
                }
                let tv = [ct * cp, ct * sp, -st];
                let pv = [-sp, cp, 0.0];
                let dot = |v: &[C64; 3], u: [f64; 3]| v[0] * u[0] + v[1] * u[1] + v[2] * u[2];
                let (nt, np, lt, lp) = (dot(&n, tv), dot(&n, pv), dot(&l, tv), dot(&l, pv));
                et[pi] = -prefactor * (lp + nt * ETA0);
                ep[pi] = prefactor * (lt - np * ETA0);
            }
            (et, ep)
        })
        .collect();
    for (i, (et, ep)) in rows.into_iter().enumerate() {
        ff.e_theta[i] = et;
        ff.e_phi[i] = ep;
    }
    ff
}

/// A recorded plane as the viewer wants it: magnitude normalised to its 99th percentile, plus
/// instantaneous frames over one period.
fn view_plane(plan: &Plan, rec: &fdtd_kernel::record::Plane, view: &ViewPlane) -> Option<FieldPlane> {
    let grid = &plan.problem.grid;
    let win = rec.window;
    let (t1, t2) = ((win.axis + 1) % 3, (win.axis + 2) % 3);
    let (m1, m2) = rec.dims();
    if m1 == 0 || m2 == 0 {
        return None;
    }
    let zero = num_complex::Complex64::new(0.0, 0.0);
    let pick = |c: &[num_complex::Complex64; 5]| if view.magnetic { (c[2], c[3], zero) } else { (c[0], c[1], c[4]) };
    let mut env: Vec<f32> = rec.data.iter().map(|c| { let (a, b, n) = pick(c); (a.norm_sqr() + b.norm_sqr() + n.norm_sqr()).sqrt() as f32 }).collect();
    let mut sorted: Vec<f32> = env.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    let k = ((sorted.len() - 1) as f32 * 0.99) as usize;
    let reference = (*sorted.select_nth_unstable_by(k, f32::total_cmp).1).max(1e-30);
    env.iter_mut().for_each(|v| *v /= reference);
    const FRAMES: usize = 16;
    let frames: Vec<Vec<f32>> = (0..FRAMES)
        .map(|n| {
            let turn = num_complex::Complex64::from_polar(1.0, 2.0 * std::f64::consts::PI * n as f64 / FRAMES as f64);
            rec.data.iter().map(|c| { let (a, b, n) = pick(c); let (x, y, w) = ((a * turn).re, (b * turn).re, (n * turn).re); ((x * x + y * y + w * w).sqrt() as f32) / reference }).collect()
        })
        .collect();
    let line = |t: usize, r: (usize, usize)| (grid.lines[t][r.0], grid.lines[t][r.1]);
    let (u, v) = (line(t1, win.r1), line(t2, win.r2));
    let mut corner = [0.0; 3];
    corner[win.axis] = grid.lines[win.axis][win.plane];
    corner[t1] = u.0;
    corner[t2] = v.0;
    let mut du = [0.0; 3];
    du[t1] = u.1 - u.0;
    let mut dv = [0.0; 3];
    dv[t2] = v.1 - v.0;
    Some(FieldPlane { name: view.name.clone(), nu: m1, nv: m2, u, v, corner, du, dv, mag: env, frames })
}

/// Threads worth giving a run of this size.
pub fn threads_for(cells: u64) -> usize {
    let n = std::thread::available_parallelism().map_or(8, |n| n.get());
    // The workers spin between half steps rather than sleep, so even a small model scales this far.
    let by_mesh = (cells / 12_000).clamp(4, 56) as usize;
    by_mesh.min((n / 2).max(1))
}

/// How often to sample the recorded planes: ten times per period of the top frequency is plenty for
/// a transform at a known frequency, and each sample costs as much as a fair part of a step.
fn record_every(dt: f64, f_hi: f64) -> u64 {
    ((1.0 / (10.0 * f_hi * dt)).floor() as u64).max(1)
}

/// Whichever engine this run is on.
enum Stepper {
    Cpu(Box<Engine<f32>>),
    #[cfg(feature = "gpu")]
    Gpu(Box<fdtd_kernel::gpu::GpuEngine>),
}

impl Stepper {
    fn new(asm: &Assembled, dt: f64, threads: usize, every: u64, opts: &Options, cells: u64, hooks: &Hooks) -> Self {
        #[cfg(feature = "gpu")]
        {
            // A small model spends more in the driver than it saves.
            let want = match opts.backend {
                Backend::Auto => cells >= GPU_FROM_CELLS,
                Backend::Gpu => true,
                Backend::Cpu => false,
            };
            if want {
                match fdtd_kernel::gpu::GpuEngine::new(asm, dt, every) {
                    Ok(g) => {
                        (hooks.log)(format!("{}on the GPU: {}", opts.label, g.adapter_name));
                        return Stepper::Gpu(Box::new(g));
                    }
                    Err(e) => (hooks.log)(format!("{}staying on the CPU: {e:#}", opts.label)),
                }
            }
        }
        let _ = (every, cells, hooks, opts);
        Stepper::Cpu(Box::new(Engine::<f32>::new(asm, dt, threads)))
    }

    fn record(&mut self, asm: &Assembled, rec: Recorder, windows: &[Window]) {
        match self {
            Stepper::Cpu(e) => {
                let _ = (asm, windows);
                e.record(rec)
            }
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.record(asm, rec, windows),
        }
    }

    fn reset(&mut self) {
        match self {
            Stepper::Cpu(e) => e.reset(),
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.reset(),
        }
    }

    fn drive(&mut self, port: usize, pulse: Pulse) {
        match self {
            Stepper::Cpu(e) => e.drive(port, pulse),
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.drive(port, pulse),
        }
    }

    fn add_wave(&mut self, run: WavePortRun) {
        match self {
            Stepper::Cpu(e) => e.add_wave_port(run),
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.add_wave_port(run),
        }
    }

    fn drive_wave(&mut self, asm: &Assembled, port: usize, pulse: Pulse) {
        match self {
            Stepper::Cpu(e) => {
                let _ = asm;
                e.drive_wave(port, pulse)
            }
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.drive_wave(asm, port, pulse),
        }
    }

    fn wave_runs(&self) -> &[WavePortRun] {
        match self {
            Stepper::Cpu(e) => &e.wave_ports,
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => &g.wave_ports,
        }
    }

    fn run(&mut self, end_db: f64, max_steps: u64, tick: &mut dyn FnMut(u64, f64) -> bool) -> Result<Stop> {
        match self {
            Stepper::Cpu(e) => Ok(e.run(end_db, max_steps, tick)),
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.run(end_db, max_steps, tick),
        }
    }

    fn step(&self) -> u64 {
        match self {
            Stepper::Cpu(e) => e.step,
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.step,
        }
    }

    fn traces(&self) -> &[fdtd_kernel::engine::PortTrace] {
        match self {
            Stepper::Cpu(e) => &e.traces,
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => &g.traces,
        }
    }

    fn planes(&self, which: usize) -> Vec<fdtd_kernel::record::Plane> {
        match self {
            Stepper::Cpu(e) => e.recorders()[which].planes(),
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.recorder_planes(which),
        }
    }

    fn recorder_count(&self) -> usize {
        match self {
            Stepper::Cpu(e) => e.recorders().len(),
            #[cfg(feature = "gpu")]
            Stepper::Gpu(g) => g.recorder_count(),
        }
    }
}

/// The operator and time step, kept for repeated runs on one mesh.
pub struct Prepared {
    pub plan: Plan,
    pub asm: Assembled,
    pub dt: f64,
}

pub fn prepare(geom: &Geometry, setup: &crate::project::Setup, design_f: f64, opts: &Options) -> Result<Prepared> {
    let t0 = std::time::Instant::now();
    let plan = plan(geom, setup, design_f, opts.subcell, &opts.refine)?;
    let t1 = std::time::Instant::now();
    let asm = assemble(&plan.problem);
    let t2 = std::time::Instant::now();
    let dt = stable_dt(&asm) * opts.dt_scale;
    if std::env::var_os("FIT_TIMING").is_some() {
        eprintln!("timing: plan {:.2} s, assemble {:.2} s, time step {:.2} s", (t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), t2.elapsed().as_secs_f64());
    }
    Ok(Prepared { plan, asm, dt })
}

/// A wave port's mode. The feed's `b` end is taken as the signal, as a gap's voltage is measured to
/// it. If that's the conductor that reaches the rim, the ground plane, it's the other way round: the
/// mode then comes out with the opposite sign to a gap feed drawn the same way, which the second value says.
fn wave_run(asm: &Assembled, line: &WaveLine) -> Result<(WavePortRun, bool)> {
    match WavePortRun::new(asm, line.def.clone()) {
        Ok(run) => Ok((run, false)),
        Err(first) => {
            let Some(g) = line.def.ground else { bail!("Feed '{}': {first}.", line.name) };
            let swapped = WavePortDef { signal: g, ground: Some(line.def.signal), ..line.def.clone() };
            match WavePortRun::new(asm, swapped) {
                Ok(run) => Ok((run, true)),
                Err(second) => bail!("Feed '{}' can't be a wave port: {first}, and the other way round {second}. It needs a line with a separate return conductor, drawn straight for at least a cell into the model.", line.name),
            }
        }
    }
}

/// The whole run: one pass per port.
pub fn simulate(project: &Project, geom: &Geometry, opts: &Options, hooks: &Hooks) -> Result<SimResult> {
    match simulate_tracked(project, geom, opts, hooks, false) {
        Ok(r) => Ok(r.0),
        // The time step is measured, and a mode its measure missed is the one way a correct model
        // can blow up. A shorter step costs time, not accuracy, so it gets one more try.
        Err(e) if e.to_string().contains("diverged") && opts.dt_scale >= 1.0 => {
            (hooks.log)(format!("{}{e:#} Trying again with a shorter time step.", opts.label));
            let shorter = Options { dt_scale: 0.75, ..opts.clone() };
            let mut r = simulate_tracked(project, geom, &shorter, hooks, false)?.0;
            r.warnings.push("The first run diverged, and this one used a shorter time step. Results should be sound, but please report the model.".to_string());
            Ok(r)
        }
        Err(e) => Err(e),
    }
}

/// Where the field energy sat during the first port's run, per plane along each axis, with the
/// lines those planes are on.
pub type Profile = ([Vec<f64>; 3], [Vec<f64>; 3]);

fn simulate_tracked(project: &Project, geom: &Geometry, opts: &Options, hooks: &Hooks, track: bool) -> Result<(SimResult, Option<Profile>)> {
    let t0 = std::time::Instant::now();
    let setup = &project.setup;
    let design_f = project.f0();
    if geom.ports.is_empty() {
        bail!("The model has no feed to drive.");
    }
    (hooks.busy)(format!("{}Building the mesh…", opts.label), -1.0);
    let Prepared { plan, asm, dt } = prepare(geom, setup, design_f, opts)?;
    let n = geom.ports.len();
    let threads = if opts.threads > 0 {
        opts.threads
    } else if setup.threads > 0 {
        setup.threads
    } else {
        threads_for(plan.cells())
    };
    let freqs = linspace(setup.f_lo, setup.f_hi, setup.points.max(2));
    let mut z0: Vec<f64> = geom.ports.iter().map(|p| p.z0).collect();
    let zero = C64::new(0.0, 0.0);
    let mut s = vec![vec![vec![zero; n]; n]; freqs.len()];
    let mut far: Vec<Vec<FarField>> = Vec::with_capacity(n);
    let mut planes_out = Vec::new();
    let mut warnings = plan.warnings.clone();
    warnings.extend(asm.warnings.iter().cloned());
    (hooks.log)(format!("{}{:.2} M cells, smallest {:.3} mm, time step {:.3} ps, {threads} threads", opts.label, plan.cells() as f64 / 1e6, plan.min_cell() * 1e3, dt * 1e12));
    let every = record_every(dt, setup.f_hi);
    let cpu_only = Options { backend: Backend::Cpu, ..opts.clone() };
    let mut eng = Stepper::new(&asm, dt, threads, every, if track { &cpu_only } else { opts }, plan.cells(), hooks);
    if track && let Stepper::Cpu(e) = &mut eng {
        e.track_profile();
    }
    let mut profile = None;
    // Recordings: the box at the far-field frequencies, the views at the design frequency.
    if !plan.far_freqs.is_empty() {
        let windows: Vec<Window> = plan.faces.iter().map(|(f, _)| plan.nf[*f]).collect();
        eng.record(&asm, Recorder::new(&asm.grid, &windows, &plan.far_freqs, every), &windows);
    }
    if !plan.views.is_empty() {
        let mut windows: Vec<Window> = plan.views.iter().map(|v| v.window).collect();
        windows.dedup();
        eng.record(&asm, Recorder::new(&asm.grid, &windows, &[design_f], every), &windows);
    }
    // Wave ports: the line's mode on this mesh sets its impedance, and S is referred to that.
    let mut flipped = Vec::with_capacity(plan.waves.len());
    for (w, line) in plan.waves.iter().enumerate() {
        let (run, flip) = wave_run(&asm, line)?;
        let at = plan.routes.iter().position(|r| *r == Route::Wave(w)).unwrap_or(0);
        (hooks.log)(format!("{}Feed '{}': a {:.1} ohm line, effective permittivity {:.2}", opts.label, line.name, run.mode.z0, run.mode.eps_eff));
        if (run.mode.z0 / z0[at] - 1.0).abs() > 0.1 {
            warnings.push(format!("The line at feed '{}' is {:.1} ohms, not the {:.1} asked for. S at this port is referred to the line itself.", line.name, run.mode.z0, z0[at]));
        }
        z0[at] = run.mode.z0;
        flipped.push(flip);
        eng.add_wave(run);
    }
    // One port at one frequency, at its own plane.
    let wave_at = |eng: &Stepper, port: usize, f: f64| -> Waves {
        match plan.routes[port] {
            Route::Lumped(k) => waves(&eng.traces()[k], dt, f),
            Route::Wave(k) => {
                let run = &eng.wave_runs()[k];
                let (forward, back) = waves_at(run, dt, f);
                // The reading is a little way along the line. The wave going in was earlier at the plane, and
                // the wave coming back gets there later.
                let shift = num_complex::Complex64::from_polar(1.0, beta(&run.mode, f) * run.read_distance(&asm.grid));
                let sign = if flipped[k] { -1.0 } else { 1.0 };
                let (inc, refl) = (forward * shift * sign, back / shift * sign);
                Waves { u: inc + refl, i: (inc - refl) / run.mode.z0, inc, refl }
            }
        }
    };
    let pulse = Pulse { f0: plan.f0, fc: plan.fc };
    let mut speed = 0.0;
    for j in 0..n {
        if hooks.cancel.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        let label = if n > 1 { format!("{}port {} of {n}: ", opts.label, j + 1) } else { opts.label.clone() };
        eng.reset();
        match plan.routes[j] {
            Route::Lumped(k) => eng.drive(k, pulse),
            Route::Wave(k) => eng.drive_wave(&asm, k, pulse),
        }
        let run_start = std::time::Instant::now();
        let end_db = plan.end_db;
        let cells = plan.cells() as f64;
        let stop = eng.run(end_db, plan.max_steps, &mut |step, db| {
            let rate = cells * step as f64 / run_start.elapsed().as_secs_f64().max(1e-6) / 1e6;
            (hooks.busy)(format!("{label}energy {db:.0} of -{end_db:.0} dB · {rate:.0} MC/s"), ((-db / end_db) as f32).clamp(0.0, 1.0));
            !hooks.cancel.load(Ordering::SeqCst)
        })?;
        speed = plan.cells() as f64 * eng.step() as f64 / run_start.elapsed().as_secs_f64().max(1e-6) / 1e6;
        if std::env::var_os("FIT_TIMING").is_some() {
            eprintln!("timing: run {:.2} s, {} steps, {speed:.0} MC/s on {threads} threads", run_start.elapsed().as_secs_f64(), eng.step());
        }
        let post_start = std::time::Instant::now();
        match stop {
            Stop::Cancelled => bail!("cancelled"),
            Stop::Diverged => bail!("The run diverged at step {}. This is a fault in the solver, not in your model: please report it, and use the openEMS solver for now.", eng.step()),
            Stop::StepLimit => warnings.push(format!("{} of {} before the energy decayed to -{:.0} dB: narrow-band results may be off. Raise the step limit.", STOPPED_SHORT, setup.max_steps, plan.end_db)),
            Stop::Decayed => {}
        }
        (hooks.busy)(format!("{label}S-parameters…"), -1.0);
        let driven: Vec<_> = freqs.par_iter().map(|f| wave_at(&eng, j, *f)).collect();
        let (inc, u, i): (Vec<C64>, Vec<C64>, Vec<C64>) = (driven.iter().map(|w| cx(w.inc)).collect(), driven.iter().map(|w| cx(w.u)).collect(), driven.iter().map(|w| cx(w.i)).collect());
        checks::check_port_waves(j, &freqs, &inc, &u, &i)?;
        for p in 0..n {
            let at_p: Vec<_> = freqs.par_iter().map(|f| wave_at(&eng, p, *f)).collect();
            for k in 0..freqs.len() {
                // Power waves: b_i/√Z_i over a_j/√Z_j, here with voltage waves.
                s[k][p][j] = cx(at_p[k].refl) / inc[k] * (z0[j] / z0[p]).sqrt();
            }
        }
        let mut per_f = Vec::with_capacity(plan.far_freqs.len());
        if !plan.far_freqs.is_empty() {
            let recorded = eng.planes(0);
            for f in plan.far_freqs.iter() {
                (hooks.busy)(format!("{label}far field at {:.3} GHz…", f / 1e9), -1.0);
                let w = wave_at(&eng, j, *f);
                let a = cx(w.inc);
                let p_inc = (0.5 * a.norm_sqr() / z0[j]).max(1e-300);
                let p_acc = 0.5 * (cx(w.u) * cx(w.i).conj()).re;
                let here: Vec<_> = recorded.iter().filter(|p| p.f == *f).cloned().collect();
                let mut ff = far_field(&plan, &here, *f, setup.far_step.max(1.0), 1.0 / p_inc.sqrt());
                if !ff.p_rad().is_finite() {
                    bail!("The recorded near field of port {} at {:.3} GHz holds invalid numbers.", j + 1, f / 1e9);
                }
                // The incident wave is the phase reference, as the method of moments reports.
                let turn = a.conj() / a.norm().max(1e-300);
                for row in ff.e_theta.iter_mut().chain(ff.e_phi.iter_mut()) {
                    row.iter_mut().for_each(|v| *v *= turn);
                }
                ff.p_incident = 1.0;
                ff.p_accepted = p_acc / p_inc;
                per_f.push(ff);
            }
        }
        if std::env::var_os("FIT_TIMING").is_some() {
            eprintln!("timing: results {:.2} s", post_start.elapsed().as_secs_f64());
        }
        far.push(per_f);
        if j == 0 && track && let Stepper::Cpu(e) = &eng {
            profile = e.profile().map(|p| (p.clone(), asm.grid.lines.clone()));
        }
        if j == 0 && !plan.views.is_empty() {
            let recorded = eng.planes(eng.recorder_count() - 1);
            for v in &plan.views {
                if let Some(rec) = recorded.iter().find(|p| p.window == v.window)
                    && let Some(fp) = view_plane(&plan, rec, v)
                {
                    planes_out.push(fp);
                }
            }
        }
    }
    (hooks.log)(format!("{}{speed:.0} MC/s", opts.label));
    let network = Network { f: freqs, z0, s };
    let lossless = lossless_here(geom);
    warnings.extend(checks::passivity_warnings(&network, 4.0 * 10f64.powf(-plan.end_db / 20.0)));
    warnings.extend(checks::balance_warnings(&network, &far, lossless, "The far field isn't converged: refine the mesh or lower the stop level."));
    Ok((
        SimResult {
            label: project.name.clone(),
            solver: crate::project::SolverKind::Fit.label().to_string(),
        network,
        far_f: plan.far_freqs.clone(),
        far,
        planes: planes_out,
        currents: None,
        size: plan.cells(),
        seconds: t0.elapsed().as_secs_f64(),
        dir: String::new(),
        vars: project.vars()?,
        warnings,
        density: setup.cells_per_lambda,
        density_unit: "cells per λ".to_string(),
        end_db: plan.end_db,
        metal: project.metal.clone(),
        lossless,
            convergence: None,
        },
        profile,
    ))
}

/// How adaptive passes run.
#[derive(Clone, Copy, Debug)]
pub struct Adaptive {
    /// Stop when no S entry changes by more than this, twice running: one small change can be chance,
    /// and the changes don't shrink evenly.
    pub limit: f64,
    pub max_passes: usize,
    /// The share of the cells along each axis refined per pass.
    pub share: f64,
}

impl Default for Adaptive {
    fn default() -> Self {
        Self { limit: 0.02, max_passes: 6, share: 0.25 }
    }
}

/// One adaptive pass.
#[derive(Clone, Copy, Debug)]
pub struct Pass {
    pub cells: u64,
    pub seconds: f64,
    /// Largest change of any S entry against the pass before.
    pub change: Option<f64>,
}

/// Runs passes, each refining the mesh where the last one held its energy, until S stops changing.
/// A rectilinear grid can only refine whole planes, so the choice is by plane along each axis: a
/// plane is worth splitting in proportion to its share of the energy and the square of its cell.
pub fn simulate_adaptive(project: &Project, geom: &Geometry, opts: &Options, hooks: &Hooks, adapt: Adaptive) -> Result<(SimResult, Vec<Pass>)> {
    let mut pass_opts = opts.clone();
    let mut passes = Vec::new();
    let mut last: Option<SimResult> = None;
    for n in 0..adapt.max_passes.max(1) {
        pass_opts.label = format!("{}pass {}: ", opts.label, n + 1);
        let (result, profile) = match simulate_tracked(project, geom, &pass_opts, hooks, true) {
            Err(e) if e.to_string().contains("diverged") && pass_opts.dt_scale >= 1.0 => {
                (hooks.log)(format!("{}{e:#} Trying again with a shorter time step.", pass_opts.label));
                pass_opts.dt_scale = 0.75;
                simulate_tracked(project, geom, &pass_opts, hooks, true)?
            }
            other => other?,
        };
        let change = last.as_ref().map(|p| checks::delta_s(&p.network, &result.network));
        passes.push(Pass { cells: result.size, seconds: result.seconds, change });
        let settled = |p: &Pass| p.change.is_some_and(|c| c < adapt.limit);
        let done = passes.len() >= 3 && settled(&passes[passes.len() - 1]) && settled(&passes[passes.len() - 2]);
        last = Some(result);
        if done {
            break;
        }
        let Some((energy, lines)) = profile else { break };
        for k in 0..3 {
            pass_opts.refine[k].extend(refine_axis(&energy[k], &lines[k], adapt.share));
        }
    }
    let mut result = last.expect("at least one pass");
    let delta = passes.last().and_then(|p| p.change).unwrap_or(f64::INFINITY);
    let n = passes.len();
    let converged = n >= 3 && delta < adapt.limit && passes[n - 2].change.is_some_and(|c| c < adapt.limit);
    if !converged {
        result.warnings.push(format!("The adaptive mesh stopped after {} passes with S still changing by {delta:.3}, above the {:.3} it was aiming for.", passes.len(), adapt.limit));
    }
    result.convergence = Some(crate::project::MeshCheck { converged, passes: passes.len(), delta_s: delta, remaining: checks::remaining_error(delta, 1.5) });
    Ok((result, passes))
}

/// The cells along one axis most worth splitting, as fine regions, away from the absorber.
fn refine_axis(energy: &[f64], lines: &[f64], share: f64) -> Vec<(f64, f64, f64)> {
    let n = lines.len();
    if n < PML * 2 + 16 {
        return Vec::new();
    }
    let (from, to) = (PML + 4, n - 1 - (PML + 4));
    let total: f64 = energy.iter().sum::<f64>().max(1e-300);
    let mut scored: Vec<(f64, usize)> = (from..to)
        .map(|i| {
            let cell = lines[i + 1] - lines[i];
            let plane_share = (energy[i] + energy[i + 1]) * 0.5 / total;
            (plane_share * cell * cell, i)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let keep = ((to - from) as f64 * share).ceil() as usize;
    scored.into_iter().take(keep).map(|(_, i)| (lines[i], lines[i + 1], (lines[i + 1] - lines[i]) / 1.5)).collect()
}


/// How a simulated port compares with a measured one over a band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Comparison {
    /// Largest `|X_sim - X_meas|` in the band, ohms, and as a share of the measured reactance there.
    pub worst_x: f64,
    pub worst_x_share: f64,
    /// Largest `|S11_sim - S11_meas|` in the band: the figure the parity bar is set on.
    pub worst_s: f64,
    /// Lowest `|S11|` of each, by frequency, if the band holds one.
    pub resonance_meas: Option<f64>,
    pub resonance_sim: Option<f64>,
}

/// Compares port 1 of a simulation with a measurement, on the measurement's own frequencies within
/// the band. The simulation is taken to the measurement's reference impedance first, so a tag
/// simulated against a chip's impedance can be held against a 50 ohm VNA.
pub fn compare_measured(measured: &Network, simulated: &Network, f_lo: f64, f_hi: f64) -> Comparison {
    let freqs: Vec<f64> = measured.f.iter().copied().filter(|f| *f >= f_lo && *f <= f_hi && *f >= simulated.f[0] && *f <= *simulated.f.last().unwrap_or(f)).collect();
    let sim = simulated.interpolate(&freqs);
    let (zs, zm_all) = (sim.zin(0), measured.zin(0));
    let z0 = measured.z0[0];
    let (mut worst_x, mut worst_x_share, mut worst_s) = (0.0f64, 0.0f64, 0.0f64);
    let (mut best_m, mut best_s) = ((f64::MAX, None), (f64::MAX, None));
    for (k, f) in freqs.iter().enumerate() {
        let at = measured.f.iter().position(|v| v == f).unwrap();
        let (zm, zsim) = (zm_all[at], zs[k]);
        let dx = (zsim.im - zm.im).abs();
        worst_x = worst_x.max(dx);
        worst_x_share = worst_x_share.max(dx / zm.im.abs().max(1.0));
        let gamma = |z: C64| (z - z0) / (z + z0);
        let (gm, gs) = (gamma(zm), gamma(zsim));
        worst_s = worst_s.max((gm - gs).norm());
        if gm.norm() < best_m.0 {
            best_m = (gm.norm(), Some(*f));
        }
        if gs.norm() < best_s.0 {
            best_s = (gs.norm(), Some(*f));
        }
    }
    Comparison { worst_x, worst_x_share, worst_s, resonance_meas: best_m.1, resonance_sim: best_s.1 }
}

/// Nothing in this model can lose power here.
fn lossless_here(geom: &Geometry) -> bool {
    let built;
    let geom = if geom.stack.is_some() {
        built = geom.with_stack_built();
        &built
    } else {
        geom
    };
    geom.solids.iter().all(|s| {
        material::by_name(&s.material).is_none_or(|m| match m.kind {
            MatKind::Pec => true,
            MatKind::Metal { .. } => false,
            MatKind::Dielectric { tan_d, .. } => tan_d <= 0.0,
        })
    })
}

pub mod csxrun;
#[cfg(test)]
mod bench_tests;
#[cfg(test)]
mod measured_tests;
#[cfg(test)]
mod study_tests;
#[cfg(test)]
mod tests;
