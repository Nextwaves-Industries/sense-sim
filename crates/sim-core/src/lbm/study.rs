//! Takes a [`Study`] through the GPU solver: the body voxelised in a wind tunnel, the flow run to a
//! statistically settled state, and the result as the body's surface and a cut through the air.

use std::sync::Arc;

use anyhow::{Result, bail};

use super::voxel::{self, Grid};
use super::{FLUID, INLET, Lbm, OUTLET, ROUGH_LAYER, SLIP_FLOOR, Setup, WALL};
use crate::catalog::AnalysisId;
use crate::geometry::Partition;
use crate::jobs::JobCtx;
use crate::results::{Field, Location, Plot, ResultMesh, ResultSet, ResultStep, StepKind};
use crate::study::Study;

/// How a run maps real units to the lattice.
#[derive(Debug, Clone, Copy)]
pub struct Scales {
    pub dx: f64,
    /// Seconds per step.
    pub dt: f64,
    /// Free-stream speed in lattice units.
    pub u_lattice: f64,
    pub nu_lattice: f64,
}

/// Lattice speed for the free stream: low enough for the incompressible limit, high enough to stay
/// clear of single-precision round-off.
const U_LATTICE: f64 = 0.05;
/// The same where viscosity matters most. Measured on a cylinder at Re 20, 40 cells across, with this
/// as the peak speed: drag was 3.1% low at twice it, 1.4% low at it and 0.5% low at half of it. So the
/// error halves with the speed on a fixed grid, which is first order, not the second order a plain
/// compressibility error would give, and viscosity falls with the speed in that series too, so this
/// doesn't say which of them is the cause. It costs twice the steps, so it's kept for such flows.
const U_LATTICE_RESOLVED: f64 = 0.025;

pub fn scales(speed: f64, dx: f64, nu: f64) -> Result<Scales> {
    if speed <= 0.0 {
        bail!("the free-stream speed must be above zero");
    }
    let viscosity_at = |u: f64| nu * (u * dx / speed) / (dx * dx);
    let u_lattice = if viscosity_at(U_LATTICE) > 0.01 { U_LATTICE_RESOLVED } else { U_LATTICE };
    let dt = u_lattice * dx / speed;
    Ok(Scales { dx, dt, u_lattice, nu_lattice: viscosity_at(u_lattice) })
}

/// The tunnel around a body: `resolution` cells across its largest side, room around it, a floor
/// under it when asked. Sizes are chosen so that the body stays in the tunnel's middle.
pub fn tunnel(lo: [f64; 3], hi: [f64; 3], resolution: u32, ground: bool) -> Grid {
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let length = size[0].max(size[1]).max(size[2]).max(1e-9);
    let dx = length / f64::from(resolution.max(4));
    let (front, back, side, top) = (1.5 * length, 4.0 * length, 1.5 * length, 1.5 * length);
    let bottom = if ground { 2.0 * dx } else { top };
    let count = |extent: f64| ((extent / dx).ceil() as u32).max(8);
    Grid { origin: [lo[0] - front, lo[1] - side, lo[2] - bottom], dx, size: [count(front + size[0] + back), count(2.0 * side + size[1]), count(bottom + size[2] + top)] }
}

/// A Smagorinsky constant for a run: none where the flow is resolved, the usual value where not.
fn smagorinsky(nu_lattice: f64) -> f32 {
    if nu_lattice > 0.01 { 0.0 } else { 0.16 }
}

/// The viscosity a run uses. Air at building or vehicle scale asks for far less than a lattice can
/// hold: the eddies that would dissipate it aren't on the grid, and the turbulence model carries
/// them instead. So there is a floor, and the run's log says when it applies, because it means the
/// effective Reynolds number is lower than the real one.
fn lattice_viscosity(nu_lattice: f64) -> f32 {
    (nu_lattice as f32).max(5e-4)
}

/// Cell flags: inlet, outlet and solid. Sides are periodic. With `ground`, the bottom is a rough
/// floor and the top row holds the wind's own profile, standing in for the rest of the atmosphere.
pub fn flags(grid: &Grid, solid: &[bool], ground: bool) -> Vec<u32> {
    let [nx, ny, nz] = grid.size;
    let mut out = vec![FLUID; grid.cells()];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = grid.index(x, y, z);
                out[i] = if ground && z == 0 {
                    SLIP_FLOOR
                } else if solid[i] {
                    WALL
                } else if x == 0 || (ground && z == nz - 1) {
                    INLET
                } else if x == nx - 1 {
                    OUTLET
                } else if ground && z == 1 {
                    FLUID | ROUGH_LAYER
                } else {
                    FLUID
                };
            }
        }
    }
    out
}

fn bounds(part: &Partition) -> ([f64; 3], [f64; 3]) {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// Runs a voxel-based study on `part`.
pub fn run_study(study: &Study, part: &Partition, ctx: &JobCtx) -> Result<ResultSet> {
    crate::check::before_run(study, part)?;
    match study.analysis {
        AnalysisId::IncompressibleLbm => {}
        AnalysisId::PedestrianWindComfort => return comfort(study, part, ctx),
        AnalysisId::ConjugateHeatTransferVoxel => return conjugate(study, part, ctx),
        other => bail!("{} isn't run on a design body yet in this build.", crate::catalog::analysis(other).label),
    }
    let (lo, hi) = bounds(part);
    let grid = tunnel(lo, hi, study.settings.resolution, false);
    let (set, _) = run_case(study, part, &grid, [1.0, 0.0, 0.0], false, SETTLE_PASSES, 0.0, ctx, (0.1, 0.95))?;
    Ok(set)
}

/// How many times the air crosses the tunnel in a run that's after a settled answer. Measured on air past
/// a cube at Re 100, two body lengths behind it: 0.00524 m/s at 1 pass, 0.00279 at 2, and 0.00256 at both
/// 4 and 8, with OpenFOAM's on its finer mesh at 0.00208. A single pass, which this once was, stops long
/// before the wake has formed and shows no sign of it, so the panel's answers were off by a factor of two.
const SETTLE_PASSES: f64 = 4.0;

/// How far the drag moved between the two halves of the averaged part of a run, as a share of it.
/// Vortex shedding swings the samples, but not the means of two halves that hold several swings.
fn drag_drift(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let (early, late) = points.split_at(points.len() / 2);
    let mean = |p: &[(f64, f64)]| p.iter().map(|v| v.1).sum::<f64>() / p.len() as f64;
    let (a, b) = (mean(early), mean(late));
    let size = a.abs().max(b.abs());
    if size > 0.0 { (b - a).abs() / size } else { 0.0 }
}

/// Forced-convection cooling of a body: flow on the GPU, then steady heat transfer in the solid and
/// the air on the CPU. The power of its heat-flux conditions is released evenly through the body.
/// The heat carried out of the tunnel must then equal the power put in, which the run checks.
fn conjugate(study: &Study, part: &Partition, ctx: &JobCtx) -> Result<ResultSet> {
    use super::thermal::{self, Cell};
    use crate::study::BcKind;
    let (lo, hi) = bounds(part);
    let grid = tunnel(lo, hi, study.settings.resolution, false);
    // The heat going in: flux times the area of the faces it's on.
    let areas = part.face_areas();
    let mut power = 0.0;
    for c in &study.conditions {
        if let BcKind::HeatFlux { flux } = c.kind {
            power += c.faces.iter().map(|f| flux * areas.get(f.face as usize).copied().unwrap_or(0.0)).sum::<f64>();
        }
    }
    if power <= 0.0 {
        bail!("Add a heat flux on some faces: with no heat going in, nothing warms up.");
    }
    let (mut set, velocity) = run_case(study, part, &grid, [1.0, 0.0, 0.0], false, SETTLE_PASSES, 0.0, ctx, (0.05, 0.6))?;
    ctx.set("Solving heat transfer", 0.62);
    let f = &study.fluid;
    let solid = voxel::fill(part, &grid);
    let n = grid.cells();
    let body_cells = solid.iter().filter(|c| **c).count() as f64;
    let source = power / (body_cells * grid.dx.powi(3));
    let ambient = study.settings.initial_temperature;
    let [nx, ny, nz] = grid.size;
    let mut cells = vec![Cell::Free { conductivity: f.conductivity, source: 0.0 }; n];
    let mut capacity = vec![f.density * f.specific_heat; n];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = grid.index(x, y, z);
                if x == 0 {
                    cells[i] = Cell::Fixed(0.0);
                } else if x == nx - 1 {
                    cells[i] = Cell::Open(0.0);
                } else if solid[i] {
                    cells[i] = Cell::Free { conductivity: study.material.conductivity, source };
                    capacity[i] = study.material.density * study.material.specific_heat;
                }
            }
        }
    }
    // The rise above ambient, not the temperature itself: the lattice velocities aren't exactly free
    // of divergence, and that would act as a heat source in proportion to whatever is solved for.
    let mut temperature = vec![0.0; n];
    let problem = thermal::Problem { size: [nx as usize, ny as usize, nz as usize], dx: grid.dx, cells: &cells, velocity: &velocity, heat_capacity: &capacity, periodic: [false, true, true] };
    let report = thermal::solve(&problem, &mut temperature, 1e-7, 20_000);
    ctx.log(&format!("heat transfer: {} iterations, residual {:.2e}", report.sweeps, report.change));

    // The energy balance: what leaves through the open end must be what went in. The same face flow
    // the solver used, so this is a real check of the solve and not of an interpolation.
    let mut carried = 0.0;
    for z in 0..nz {
        for y in 0..ny {
            let (i, out) = (grid.index(nx - 2, y, z), grid.index(nx - 1, y, z));
            let flow = f.density * f.specific_heat * 0.5 * (velocity[i][0] + velocity[out][0]) * grid.dx * grid.dx;
            if flow > 0.0 {
                carried += flow * temperature[i];
            }
        }
    }
    let imbalance = (carried - power).abs() / power;
    ctx.log(&format!("power in {power:.3} W, carried out {carried:.3} W, {:.1}% apart", imbalance * 100.0));
    if imbalance > 0.25 {
        bail!("The heat balance doesn't close: {power:.2} W went in and {carried:.2} W came out. The result would be wrong, so it isn't shown. Try a higher resolution.");
    }

    // Temperature on what the flow run drew.
    let cell_of = |p: [f64; 3]| -> usize {
        let c = |k: usize| (((p[k] - grid.origin[k]) / grid.dx) as i64).clamp(0, i64::from(grid.size[k]) - 1) as u32;
        grid.index(c(0), c(1), c(2))
    };
    let values: Vec<f32> = set.mesh.nodes.iter().map(|p| (temperature[cell_of(*p)] + ambient) as f32).collect();
    if let Some(step) = set.steps.last_mut() {
        step.fields.insert(0, Field { name: "T".into(), unit: "°C".into(), location: Location::Node, components: vec!["T".into()], data: Arc::from(values) });
    }
    set.plots.push(Plot { name: "heat balance".into(), unit: "W".into(), points: vec![(0.0, power), (1.0, carried)] });
    Ok(set)
}

/// A comfort study: the same run for each wind direction, rotating the wind about the vertical
/// axis, and the worst speed-up seen at each point at pedestrian height.
fn comfort(study: &Study, part: &Partition, ctx: &JobCtx) -> Result<ResultSet> {
    let (lo, hi) = bounds(part);
    let grid = tunnel(lo, hi, study.settings.resolution, true);
    let directions = study.settings.directions.clamp(1, 36);
    let about = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let mut combined: Option<ResultSet> = None;
    let mut worst: Vec<f32> = Vec::new();
    for d in 0..directions {
        let angle = std::f64::consts::TAU * f64::from(d) / f64::from(directions);
        // The wind turns by turning the body the other way: a fixed tunnel stays simple.
        let turned = rotated(part, -angle, about);
        let share = (0.05 + 0.9 * f64::from(d) / f64::from(directions), 0.05 + 0.9 * f64::from(d + 1) / f64::from(directions));
        ctx.set(&format!("Wind direction {} of {directions}", d + 1), share.0 as f32);
        let (set, mean) = run_case(study, &turned, &grid, [1.0, 0.0, 0.0], true, 2.0, 0.03, ctx, share)?;
        let combined = combined.get_or_insert(set);
        if worst.is_empty() {
            worst = vec![0.0; combined.mesh.nodes.len()];
        }
        add_direction(&mut worst, &combined.mesh.nodes, &mean, &grid, -angle, about, study.settings.free_stream);
    }
    let mut set = combined.ok_or_else(|| anyhow::anyhow!("no wind directions"))?;
    if let Some(step) = set.steps.last_mut() {
        step.fields.push(Field { name: "Worst speed-up".into(), unit: String::new(), location: Location::Node, components: vec!["Worst speed-up".into()], data: Arc::from(worst) });
    }
    Ok(set)
}

/// Adds one wind direction to the worst speed-up at each node. The nodes are places around the body
/// as it stands, while that run had the body turned by `angle`. So each node is turned along with it
/// to find where it was in that run's tunnel: the same spot in the tunnel in two runs is a different
/// spot beside the body. A node that fell outside the tunnel, or inside the body, has nothing to add.
fn add_direction(worst: &mut [f32], nodes: &[[f64; 3]], mean: &[[f64; 3]], grid: &Grid, angle: f64, about: [f64; 2], free_stream: f64) {
    let (s, c) = angle.sin_cos();
    for (w, q) in worst.iter_mut().zip(nodes) {
        let (x, y) = (q[0] - about[0], q[1] - about[1]);
        let p = [about[0] + c * x - s * y, about[1] + s * x + c * y, q[2]];
        // Inside the body the velocity is zero: use the nearest cell that isn't, for surface nodes.
        let speed_at = |p: [f64; 3]| -> Option<f64> {
            let cell: [f64; 3] = std::array::from_fn(|k| ((p[k] - grid.origin[k]) / grid.dx).floor());
            if (0..3).any(|k| cell[k] < 0.0 || cell[k] >= f64::from(grid.size[k])) {
                return None;
            }
            let u = mean[grid.index(cell[0] as u32, cell[1] as u32, cell[2] as u32)];
            Some((u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt())
        };
        let Some(mut speed) = speed_at(p) else { continue };
        if speed == 0.0 {
            for r in 1..3 {
                let d = f64::from(r) * grid.dx;
                speed = [[d, 0.0, 0.0], [-d, 0.0, 0.0], [0.0, d, 0.0], [0.0, -d, 0.0], [0.0, 0.0, d]]
                    .iter()
                    .filter_map(|o| speed_at([p[0] + o[0], p[1] + o[1], p[2] + o[2]]))
                    .fold(0.0, f64::max);
                if speed > 0.0 {
                    break;
                }
            }
        }
        *w = w.max((speed / free_stream) as f32);
    }
}

/// The most nodes in the copy of the volume a result keeps for cut planes and streamlines.
const VOLUME_NODES: u64 = 400_000;

/// Every how many cells the volume is sampled, to stay within [`VOLUME_NODES`].
fn volume_stride(size: [u32; 3]) -> u32 {
    let mut stride = 1u32;
    while size.iter().map(|n| u64::from((n - 1) / stride + 1)).product::<u64>() > VOLUME_NODES {
        stride += 1;
    }
    stride
}

fn rotated(part: &Partition, angle: f64, about: [f64; 2]) -> Partition {
    let (s, c) = angle.sin_cos();
    let mut out = part.clone();
    for p in &mut out.positions {
        let (x, y) = (p[0] - about[0], p[1] - about[1]);
        p[0] = about[0] + c * x - s * y;
        p[1] = about[1] + s * x + c * y;
    }
    out
}

/// One run. Returns the result and the time-averaged velocity of every cell in m/s, zero in solids.
/// `roughness` above zero makes the inlet a log-law wind.
#[allow(clippy::too_many_arguments)]
fn run_case(study: &Study, part: &Partition, grid: &Grid, wind: [f64; 3], ground: bool, flow_throughs: f64, roughness: f64, ctx: &JobCtx, progress: (f64, f64)) -> Result<(ResultSet, Vec<[f64; 3]>)> {
    let cells = grid.cells() as u64;
    if cells > 110_000_000 {
        bail!("{cells} cells won't fit in the GPU's memory. Lower the resolution.");
    }
    let f = &study.fluid;
    let nu = f.viscosity / f.density;
    let speed = study.settings.free_stream;
    let s = scales(speed, grid.dx, nu)?;
    ctx.log(&format!("{} x {} x {} cells at {:.3} mm; {:.3} µs per step; lattice viscosity {:.2e}", grid.size[0], grid.size[1], grid.size[2], grid.dx * 1e3, s.dt * 1e6, s.nu_lattice));
    ctx.set("Voxelising", progress.0 as f32);
    let solid = voxel::fill(part, grid);
    let solid_cells = solid.iter().filter(|c| **c).count();
    if solid_cells == 0 {
        bail!("The body is too small for this grid: no cells fell inside it. Raise the resolution.");
    }
    let flags = flags(grid, &solid, ground);
    let reference_height = 10.0 / grid.dx;
    let setup = Setup {
        size: grid.size,
        viscosity: lattice_viscosity(s.nu_lattice),
        smagorinsky: smagorinsky(s.nu_lattice),
        force: [0.0; 3],
        wall_velocity: [0.0; 3],
        inlet_velocity: [(wind[0] * s.u_lattice) as f32, (wind[1] * s.u_lattice) as f32, (wind[2] * s.u_lattice) as f32],
        inlet_z0: if roughness > 0.0 { (roughness / grid.dx) as f32 } else { 0.0 },
        inlet_reference_height: reference_height as f32,
        inlet_channel: 0.0,
        rough: if ground && roughness > 0.0 {
            let z0 = roughness / grid.dx;
            ((0.41 / ((0.5 + z0) / z0).ln()).powi(2)) as f32
        } else {
            0.0
        },
        mixing_max: if ground { (0.1 * f64::from(grid.size[2])) as f32 } else { 0.0 },
    };
    if f64::from(setup.viscosity) > s.nu_lattice * 1.01 {
        ctx.log(&format!("The grid can't hold this flow's Reynolds number: it runs at an effective {:.0}, with turbulence modelled.", s.u_lattice * f64::from(grid.size[0]) / f64::from(setup.viscosity)));
    }
    let mut lbm = Lbm::new(&setup, &flags)?;
    ctx.log(&format!("on {}", lbm.adapter_name()));
    // Start from the free stream everywhere, stopped inside the solid.
    let n = lbm.cells() as usize;
    let u0 = setup.inlet_velocity;
    let u: Vec<[f32; 3]> = solid.iter().map(|c| if *c { [0.0; 3] } else { u0 }).collect();
    lbm.initialise(&vec![0.0; n], &u)?;
    let steps = ((f64::from(grid.size[0]) / s.u_lattice) * flow_throughs).ceil() as u64;
    // Most of the run to settle, then an average over the end.
    let (settle, average) = (steps * 3 / 5, steps * 2 / 5);
    let total = (settle + average).max(1);
    let report = |done: u64| ctx.set("Solving the flow", (progress.0 + (progress.1 - progress.0) * done as f64 / total as f64) as f32);
    lbm.run(settle, &ctx.cancel, |d| report(d))?;
    // Vortex shedding needs many snapshots for a mean: at least 4 per pass of the air along the tunnel.
    let samples = ((flow_throughs * 4.0).ceil() as u64).max(8);
    let mut sum_u = vec![[0.0f32; 3]; n];
    let mut sum_rho = vec![0.0f32; n];
    let mut history = Plot { name: "drag".into(), unit: "N".into(), points: Vec::new() };
    let force_scale = f.density * grid.dx.powi(4) / (s.dt * s.dt);
    for k in 0..samples {
        lbm.run(average / samples, &ctx.cancel, |d| report(settle + k * (average / samples) + d))?;
        let m = lbm.read_macros()?;
        if m.ux.iter().any(|v| !v.is_finite() || v.abs() > 0.5) {
            bail!("The flow solution has gone unstable: lattice speeds have left the valid range. Try a higher resolution or a lower wind speed.");
        }
        for i in 0..n {
            sum_u[i][0] += m.ux[i];
            sum_u[i][1] += m.uy[i];
            sum_u[i][2] += m.uz[i];
            sum_rho[i] += m.rho[i];
        }
        let force = lbm.wall_force(&flags, |_| true)?;
        history.points.push((lbm.steps() as f64 * s.dt, force[0] * force_scale));
    }
    ctx.log(&format!("mean drag {:.4e} N", history.points.iter().map(|p| p.1).sum::<f64>() / history.points.len() as f64));
    let (scale_u, scale_p) = (speed / s.u_lattice, f.density * (speed / s.u_lattice).powi(2) / 3.0);
    let mean_u = |i: usize| [sum_u[i][0] / samples as f32 * scale_u as f32, sum_u[i][1] / samples as f32 * scale_u as f32, sum_u[i][2] / samples as f32 * scale_u as f32];
    let mean_p = |i: usize| (sum_rho[i] / samples as f32 - 1.0) * scale_p as f32;

    // What's drawn: the body's surface, then a plane through the air.
    let mut nodes = part.positions.clone();
    let mut surface = part.triangles.clone();
    let cut_start = surface.len();
    let cell_of = |p: [f64; 3]| -> usize {
        let c = |k: usize| (((p[k] - grid.origin[k]) / grid.dx) as i64).clamp(0, i64::from(grid.size[k]) - 1) as u32;
        grid.index(c(0), c(1), c(2))
    };
    // A body node's values come from the nearest fluid cell, not from inside it.
    let nearest_fluid = |p: [f64; 3]| -> usize {
        let mut best = cell_of(p);
        if !solid[best] {
            return best;
        }
        for r in 1..4i64 {
            for (dx, dy, dz) in [(r, 0, 0), (-r, 0, 0), (0, r, 0), (0, -r, 0), (0, 0, r), (0, 0, -r)] {
                let q = [p[0] + dx as f64 * grid.dx, p[1] + dy as f64 * grid.dx, p[2] + dz as f64 * grid.dx];
                let i = cell_of(q);
                if !solid[i] {
                    best = i;
                    return best;
                }
            }
        }
        best
    };
    let mut speed_field: Vec<f32> = Vec::new();
    let mut pressure: Vec<f32> = Vec::new();
    let mut velocity: Vec<f32> = Vec::new();
    for p in &part.positions {
        let i = nearest_fluid(*p);
        let u = mean_u(i);
        velocity.extend_from_slice(&u);
        speed_field.push((u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt());
        pressure.push(mean_p(i));
    }
    // A plane: through the body's middle along the wind, or at pedestrian height with a floor.
    let (nx, ny, nz) = (grid.size[0], grid.size[1], grid.size[2]);
    let first = nodes.len() as u32;
    let (lo, hi) = bounds(part);
    if ground {
        let height = (1.75 / grid.dx).round().clamp(2.0, f64::from(nz - 1)) as u32;
        for y in 0..ny {
            for x in 0..nx {
                let i = grid.index(x, y, height);
                let u = mean_u(i);
                let m = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
                nodes.push(grid.centre(x, y, height));
                velocity.extend_from_slice(&u);
                speed_field.push(m);
                pressure.push(mean_p(i));
            }
        }
        for y in 0..ny - 1 {
            for x in 0..nx - 1 {
                let v = |a: u32, b: u32| first + a + nx * b;
                surface.push([v(x, y), v(x + 1, y), v(x + 1, y + 1)]);
                surface.push([v(x, y), v(x + 1, y + 1), v(x, y + 1)]);
            }
        }
    } else {
        let row = (((lo[1] + hi[1]) / 2.0 - grid.origin[1]) / grid.dx) as u32;
        for z in 0..nz {
            for x in 0..nx {
                let i = grid.index(x, row, z);
                let u = mean_u(i);
                let m = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
                nodes.push(grid.centre(x, row, z));
                velocity.extend_from_slice(&u);
                speed_field.push(m);
                pressure.push(mean_p(i));
            }
        }
        for z in 0..nz - 1 {
            for x in 0..nx - 1 {
                let v = |a: u32, b: u32| first + a + nx * b;
                surface.push([v(x, z), v(x + 1, z), v(x + 1, z + 1)]);
                surface.push([v(x, z), v(x + 1, z + 1), v(x, z + 1)]);
            }
        }
    }
    // A coarser copy of the volume, so the air can be looked at anywhere afterwards. Cubes wholly
    // inside the body are left out.
    let stride = volume_stride(grid.size);
    let samples_of = |n: u32| (n - 1) / stride + 1;
    let volume_size = [samples_of(nx), samples_of(ny), samples_of(nz)];
    let volume_first = nodes.len() as u32;
    for z in 0..volume_size[2] {
        for y in 0..volume_size[1] {
            for x in 0..volume_size[0] {
                let (a, b, c) = (x * stride, y * stride, z * stride);
                let i = grid.index(a, b, c);
                let u = mean_u(i);
                nodes.push(grid.centre(a, b, c));
                velocity.extend_from_slice(&u);
                speed_field.push((u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt());
                pressure.push(mean_p(i));
            }
        }
    }
    let inside = |x: u32, y: u32, z: u32| solid[grid.index(x * stride, y * stride, z * stride)];
    let mut tets = crate::post::grid_tets(volume_size, |x, y, z| !(0..8u32).all(|b| inside(x + (b & 1), y + ((b >> 1) & 1), z + ((b >> 2) & 1))));
    for t in &mut tets {
        for n in t.iter_mut() {
            *n += volume_first;
        }
    }

    let field = |name: &str, unit: &str, components: Vec<String>, data: Vec<f32>| Field { name: name.into(), unit: unit.into(), location: Location::Node, components, data: Arc::from(data) };
    let fields = vec![
        field("U", "m/s", vec!["U x".into(), "U y".into(), "U z".into()], velocity),
        field("Speed", "m/s", vec!["Speed".into()], speed_field),
        field("p", "Pa", vec!["p".into()], pressure),
    ];
    // A flow still on its way looks just like a settled one, so a run that may be says so with its result.
    let drift = drag_drift(&history.points);
    ctx.log(&format!("the drag moved {:.1}% across the averaged part of the run", drift * 100.0));
    let mut plots = vec![history];
    if drift > 0.05 {
        plots.push(Plot { name: "Drag still moving late in the run, so this flow may not have settled".into(), unit: "%".into(), points: vec![(0.0, drift * 100.0)] });
    }
    let set = ResultSet { mesh: ResultMesh { nodes, tets, surface, cut_start: Some(cut_start) }, steps: vec![ResultStep { kind: StepKind::Time, value: lbm.steps() as f64 * s.dt, fields }], plots };
    let mean: Vec<[f64; 3]> = (0..n).map(|i| if solid[i] { [0.0; 3] } else { mean_u(i).map(f64::from) }).collect();
    Ok((set, mean))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box, faces numbered by side.
    fn block(size: [f64; 3]) -> Partition {
        let [x, y, z] = size;
        let positions = vec![[0., 0., 0.], [x, 0., 0.], [x, y, 0.], [0., y, 0.], [0., 0., z], [x, 0., z], [x, y, z], [0., y, z]];
        let quads = [[0u32, 4, 7, 3], [1, 2, 6, 5], [0, 1, 5, 4], [3, 7, 6, 2], [0, 3, 2, 1], [4, 5, 6, 7]];
        let (mut triangles, mut tri_face) = (Vec::new(), Vec::new());
        for (face, q) in quads.iter().enumerate() {
            triangles.extend([[q[0], q[1], q[2]], [q[0], q[2], q[3]]]);
            tri_face.extend([face as u32, face as u32]);
        }
        Partition { name: "block".into(), positions, triangles, tri_face, faces: 6 }
    }

    /// The wake of a 2:1:1 building, on the comfort route's own tunnel and physics, against a wind tunnel:
    /// Tanaka et al. (2006) at Tokyo Polytechnic University, H 200 mm, D 100 mm, Re about 56,000, a
    /// boundary layer of power index 1/4. Measured: the roof's reverse flow ends 0.53 D from the front
    /// edge, and the flow reattaches on the ground 1.37 D behind the building. The same paper's CFD on this
    /// flow gave 2.25 to 2.89 D from RANS and 2.10 D from LES, so this length is hard to get. It is the
    /// shape of AIJ's Case A, but not its measurements, so it isn't a validation against AIJ.
    ///
    /// This doesn't match the experiment, and it's here to keep that in view. Measured at 32 cells across D
    /// and 6 passes: 3.39 D at 1.5 cells above the floor, 2.89 D at 4.5 cells, and no reverse flow on the
    /// roof. So the wake runs about 2.5 times too long, and past other CFD. The inlet has no turbulence in
    /// it, which is the likely cause and hasn't been isolated. The bounds only catch it getting worse, or
    /// the wake vanishing. Do not read a pass here as agreement.
    #[test]
    #[ignore = "needs a GPU; takes minutes"]
    fn sim_comfort_wake_of_a_tall_building() {
        let env = |name: &str, default: f64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
        let (resolution, flow_throughs) = (env("WAKE_RES", 64.0) as u32, env("WAKE_PASSES", 6.0));
        let d = 20.0;
        let part = block([d, d, 2.0 * d]);
        let mut study = Study::new("wake".into(), AnalysisId::PedestrianWindComfort);
        study.settings.resolution = resolution;
        let (lo, hi) = bounds(&part);
        let grid = tunnel(lo, hi, resolution, true);
        // A log law matching a power index of 1/4 at the building's height: 1 / ln(H / z0) = 1 / 4.
        let roughness = 2.0 * d * (-4.0f64).exp();
        let root = std::env::temp_dir().join(format!("sense-sim-wake-{}", std::process::id()));
        let jobs = crate::jobs::JobManager::new(root.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let (run_part, run_grid) = (part.clone(), grid.clone());
        let id = jobs.submit("wake", "wake", crate::jobs::Kind::Gpu, move |ctx| {
            let (set, mean) = run_case(&study, &run_part, &run_grid, [1.0, 0.0, 0.0], true, flow_throughs, roughness, ctx, (0.0, 1.0))?;
            let _ = tx.send(mean);
            Ok(set)
        });
        let mean = rx.recv().unwrap_or_else(|_| panic!("the run failed: {:?}", jobs.list().into_iter().find(|j| j.id == id).map(|j| j.state)));
        let _ = std::fs::remove_dir_all(root);

        let cell = |at: f64, k: usize| ((at - grid.origin[k]) / grid.dx).floor() as u32;
        let (y, back) = (cell(d / 2.0, 1), cell(hi[0], 0) + 1);
        // Where the mean flow along x stops running backwards, along a line of cells.
        let reattachment = |from: u32, to: u32, z: u32| -> Option<f64> {
            let mut seen_reverse = false;
            for x in from..to {
                let u = mean[grid.index(x, y, z)][0];
                seen_reverse |= u < 0.0;
                if seen_reverse && u >= 0.0 {
                    return Some(grid.origin[0] + (f64::from(x) + 0.5) * grid.dx);
                }
            }
            None
        };
        println!("{} x {} x {} cells, {} cells across D, {flow_throughs} passes", grid.size[0], grid.size[1], grid.size[2], d / grid.dx);
        // The floor's row and the rough layer above it are the wall model's. Above those, the mean flow.
        let mut behind = Vec::new();
        for z in 2..6 {
            let x = reattachment(back, grid.size[0], z).map(|x| (x - hi[0]) / d);
            println!("behind, {:.1} cells above the floor: {x:?} D", f64::from(z) - 0.5);
            behind.extend(x);
        }
        let x_f = behind.first().copied().unwrap_or(f64::NAN);
        let roof = cell(hi[2], 2) + 1;
        let x_r = reattachment(cell(lo[0], 0), cell(hi[0], 0), roof).map(|x| x / d);
        println!("on the roof: {x_r:?} D, measured 0.53. Behind: {x_f:.2} D, measured 1.37; other CFD 2.10 to 2.89");
        assert!(x_f > 0.5 * 1.37 && x_f < 4.0, "{x_f} D behind the building: it was 3.39 D when this was written");
    }

    /// How the flow past a cube at Re 100 settles with run length, at the five places the app's own
    /// cross-check with OpenFOAM compares. To see how long a run this needs. `CUBE_RES`, cells across.
    #[test]
    #[ignore = "needs a GPU; takes minutes"]
    fn sim_cube_settling() {
        let res = std::env::var("CUBE_RES").ok().and_then(|v| v.parse().ok()).unwrap_or(40u32);
        let part = block([0.1, 0.1, 0.1]);
        let (lo, hi) = bounds(&part);
        let grid = tunnel(lo, hi, res, false);
        let (speed, l) = (0.015, 0.1);
        let (y, z) = (0.05, 0.05);
        let places: [(&str, [f64; 3]); 5] = [("ahead", [lo[0] - 0.5 * l, y, z]), ("above", [0.05, y, hi[2] + 0.3 * l]), ("wake 0.5 L", [hi[0] + 0.5 * l, y, z]), ("wake 1 L", [hi[0] + l, y, z]), ("wake 2 L", [hi[0] + 2.0 * l, y, z])];
        for passes in [1.0, 2.0, 4.0, 8.0] {
            let mut study = Study::new("cube".into(), AnalysisId::IncompressibleLbm);
            study.settings.free_stream = speed;
            study.settings.resolution = res;
            let root = std::env::temp_dir().join(format!("sense-sim-cube-{}", std::process::id()));
            let jobs = crate::jobs::JobManager::new(root.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            let (run_part, run_grid) = (part.clone(), grid.clone());
            let id = jobs.submit("cube", "cube", crate::jobs::Kind::Gpu, move |ctx| {
                let (set, mean) = run_case(&study, &run_part, &run_grid, [1.0, 0.0, 0.0], false, passes, 0.0, ctx, (0.0, 1.0))?;
                let _ = tx.send(mean);
                Ok(set)
            });
            let mean = rx.recv().unwrap_or_else(|_| panic!("the run failed: {:?}", jobs.list().into_iter().find(|j| j.id == id).map(|j| j.state)));
            let _ = std::fs::remove_dir_all(root);
            let row: Vec<String> = places.iter().map(|(name, p)| {
                let c: [u32; 3] = std::array::from_fn(|k| ((p[k] - grid.origin[k]) / grid.dx).floor() as u32);
                let u = mean[grid.index(c[0], c[1], c[2])];
                format!("{name} {:.5}", (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt())
            }).collect();
            println!("{passes} passes: {}", row.join(", "));
        }
    }

    #[test]
    fn a_drag_still_moving_is_noticed() {
        let at = |v: &[f64]| v.iter().enumerate().map(|(i, d)| (i as f64, *d)).collect::<Vec<_>>();
        assert!(drag_drift(&at(&[1.0, 1.0, 1.0, 1.0])) < 1e-12);
        assert!((drag_drift(&at(&[1.0, 1.0, 2.0, 2.0])) - 0.5).abs() < 1e-12);
        // Swinging about a steady mean is shedding, not settling.
        assert!(drag_drift(&at(&[0.8, 1.2, 0.8, 1.2, 0.8, 1.2, 0.8, 1.2])) < 1e-12);
        assert_eq!(drag_drift(&[]), 0.0);
        assert_eq!(drag_drift(&at(&[0.0, 0.0])), 0.0);
    }

    #[test]
    fn scales_keep_the_lattice_speed_and_scale_viscosity() {
        let s = scales(10.0, 0.01, 1.5e-5).unwrap();
        assert_eq!(s.u_lattice, U_LATTICE);
        assert!((s.dt - 5e-5).abs() < 1e-12);
        assert!((s.nu_lattice - 7.5e-6).abs() < 1e-12);
        assert!(scales(0.0, 0.01, 1.5e-5).is_err());
        // A slow, viscous flow gets the lower lattice speed.
        let slow = scales(0.001, 0.01, 1.5e-5).unwrap();
        assert_eq!(slow.u_lattice, U_LATTICE_RESOLVED);
        assert!((slow.nu_lattice - 1.5e-5 * slow.dt / 1e-4).abs() < 1e-12);
    }

    #[test]
    fn wind_directions_combine_at_the_same_spot_beside_the_body() {
        // A tunnel 20 cells a side, still everywhere but for a fast spot, in one run's frame.
        let grid = Grid { origin: [-10.0, -10.0, 0.0], dx: 1.0, size: [20, 20, 2] };
        let mut mean = vec![[1.0, 0.0, 0.0]; grid.cells()];
        // The spot is 5 m along +x from the middle, in the tunnel.
        mean[grid.index(15, 10, 0)] = [3.0, 0.0, 0.0];
        // That run had the body turned by -90 degrees, so a place 5 m along +y beside the body as it
        // stands is the one that was at the fast spot: (x, y) turned by -90 degrees is (y, -x), and
        // the spot's cell runs over x 5 to 6 and y 0 to 1.
        let nodes = [[-0.5, 5.5, 0.5], [5.5, 0.5, 0.5], [99.0, 0.0, 0.5]];
        let mut worst = vec![0.0f32; nodes.len()];
        add_direction(&mut worst, &nodes, &mean, &grid, -std::f64::consts::FRAC_PI_2, [0.0, 0.0], 1.0);
        assert_eq!(worst[0], 3.0, "the node the spot was beside: {worst:?}");
        assert_eq!(worst[1], 1.0, "the spot in the same tunnel position, a different place by the body");
        assert_eq!(worst[2], 0.0, "outside the tunnel: nothing to add");
        // A later, slower direction doesn't lower it.
        let calm = vec![[0.5, 0.0, 0.0]; grid.cells()];
        add_direction(&mut worst, &nodes, &calm, &grid, 0.0, [0.0, 0.0], 1.0);
        assert_eq!(worst[0], 3.0);
    }

    #[test]
    fn a_node_inside_the_body_reads_the_air_beside_it() {
        let grid = Grid { origin: [0.0; 3], dx: 1.0, size: [8, 8, 8] };
        let mut mean = vec![[2.0, 0.0, 0.0]; grid.cells()];
        mean[grid.index(4, 4, 4)] = [0.0; 3];
        let mut worst = vec![0.0f32; 1];
        add_direction(&mut worst, &[[4.5, 4.5, 4.5]], &mean, &grid, 0.0, [0.0, 0.0], 1.0);
        assert_eq!(worst[0], 2.0);
    }

    #[test]
    fn the_kept_volume_stays_small_on_a_big_run() {
        assert_eq!(volume_stride([20, 20, 20]), 1);
        for size in [[80, 40, 40], [1000, 400, 300], [2000, 2000, 2000]] {
            let stride = volume_stride(size);
            let kept: u64 = size.iter().map(|n: &u32| u64::from((n - 1) / stride + 1)).product();
            assert!(kept <= VOLUME_NODES, "{size:?}: {kept} nodes");
            assert!(stride == 1 || kept * 8 > VOLUME_NODES / 8, "{size:?}: more detail would fit, {kept} nodes");
        }
    }

    #[test]
    fn the_tunnel_surrounds_the_body() {
        let g = tunnel([0.0, 0.0, 0.0], [1.0, 0.5, 0.5], 32, false);
        assert_eq!(g.dx, 1.0 / 32.0);
        assert!(g.origin[0] < 0.0 && g.origin[1] < 0.0 && g.origin[2] < 0.0);
        let top = g.origin[2] + f64::from(g.size[2]) * g.dx;
        assert!(top > 0.5);
        // More room behind than in front, for the wake.
        let behind = g.origin[0] + f64::from(g.size[0]) * g.dx - 1.0;
        assert!(behind > -g.origin[0]);
    }

    #[test]
    fn a_floor_keeps_the_body_near_the_ground() {
        let g = tunnel([0.0, 0.0, 0.0], [1.0, 1.0, 1.0], 32, true);
        assert!(g.origin[2].abs() <= 3.0 * g.dx);
    }

    #[test]
    fn flags_mark_inlet_outlet_solid_and_floor() {
        let g = Grid { origin: [0.0; 3], dx: 1.0, size: [4, 3, 3] };
        let mut solid = vec![false; g.cells()];
        solid[g.index(2, 1, 1)] = true;
        let f = flags(&g, &solid, true);
        assert_eq!(f[g.index(0, 1, 1)], INLET);
        assert_eq!(f[g.index(3, 1, 1)], OUTLET);
        assert_eq!(f[g.index(2, 1, 1)], WALL);
        assert_eq!(f[g.index(1, 1, 0)], SLIP_FLOOR, "the floor");
        assert_eq!(f[g.index(1, 1, 1)], FLUID | ROUGH_LAYER, "the layer above it");
        assert_eq!(f[g.index(1, 1, 2)], INLET, "the top holds the wind's profile");
    }

    #[test]
    fn a_rotation_keeps_its_centre_and_turns_the_rest() {
        let part = Partition { name: "p".into(), positions: vec![[2.0, 1.0, 5.0]], triangles: vec![], tri_face: vec![], faces: 0 };
        let r = rotated(&part, std::f64::consts::FRAC_PI_2, [1.0, 1.0]);
        let p = r.positions[0];
        assert!((p[0] - 1.0).abs() < 1e-9 && (p[1] - 2.0).abs() < 1e-9 && p[2] == 5.0, "{p:?}");
    }
}
