//! Runs one simulation of a project: openEMS as a separate process for FDTD,
//! the built-in method of moments otherwise. Both return the same `SimResult`.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use antenna_core::checks;
use antenna_core::csx::{self, Plan};
use antenna_core::farfield::FarField;
use antenna_core::geometry::Geometry;
use antenna_core::mom::{MomOptions, MomProblem};
use antenna_core::project::{CurrentView, FieldPlane, Project, SimResult, SolverKind};
use antenna_core::recipes::gate::{self, GateField, GateSpec};
use antenna_core::reconfig::{Setting, Solved};
use antenna_core::sparam::{self, Network};
use antenna_core::{C0, C64, ETA0, linspace};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use crate::rtls::fdtd::post::{self, Grid};

/// Progress of the job, what the UI shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    /// Message and progress 0..1 (negative: unknown).
    Busy(String, f32),
    Failed(String),
}

#[derive(Default)]
pub struct Live {
    pub status: Option<Status>,
    pub log: std::collections::VecDeque<String>,
    /// Latest openEMS speed in megacells per second, for run-time estimates.
    pub speed_mcs: f64,
}

/// What a running job can touch.
#[derive(Clone)]
pub struct JobCtx {
    pub live: Arc<Mutex<Live>>,
    pub cancel: Arc<AtomicBool>,
    pub children: Arc<Mutex<Vec<(u64, Child)>>>,
}

static NEXT_CHILD: AtomicU64 = AtomicU64::new(1);

impl JobCtx {
    pub fn status(&self, s: Status) {
        self.live.lock().unwrap().status = Some(s);
    }
    pub fn busy(&self, msg: impl Into<String>, p: f32) {
        self.status(Status::Busy(msg.into(), p));
    }
    pub fn log(&self, s: impl Into<String>) {
        let mut l = self.live.lock().unwrap();
        l.log.push_back(s.into());
        while l.log.len() > 300 {
            l.log.pop_front();
        }
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn check(&self) -> Result<()> {
        if self.cancelled() {
            bail!("cancelled");
        }
        Ok(())
    }
}

/// Options one simulation takes beyond the project's setup.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Prefix for progress messages.
    pub label: String,
    /// Keep the run folder (false: results are read, then the folder goes).
    pub keep: bool,
    /// Threads per openEMS process (0: chosen from the mesh and the machine).
    pub threads: usize,
    /// How many runs share the machine at once.
    pub share: usize,
    /// Report progress of this run to the status line.
    pub report: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self { label: String::new(), keep: true, threads: 0, share: 1, report: true }
    }
}

pub fn runs_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("SenseSim").join("antenna").join("runs")
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

fn run_dir(name: &str) -> Result<PathBuf> {
    let root = runs_dir();
    // Keep the last few runs only; each holds field dumps of many megabytes.
    if let Ok(rd) = std::fs::read_dir(&root) {
        let mut old: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        old.sort();
        let n = old.len();
        for p in old.into_iter().take(n.saturating_sub(8)) {
            let _ = std::fs::remove_dir_all(p);
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(24).collect();
    let seq = NEXT_RUN.fetch_add(1, Ordering::SeqCst);
    let d = root.join(format!("{stamp}_{seq:03}_{safe}"));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Threads worth giving one run. Each time step ends in a barrier across all
/// threads, so too many on a small mesh is slower than few: measured on a 56-core
/// Xeon with 650k cells, 2 threads gave 32 MC/s, 8 gave 69, and 32 only 26.
pub fn threads_for(cells: u64, share: usize) -> usize {
    let n = std::thread::available_parallelism().map_or(8, |n| n.get());
    let by_mesh = (cells / 80_000).clamp(2, 24) as usize;
    by_mesh.min((n / 2 / share.max(1)).max(1))
}

/// Runs openEMS on `sim.xml` in `dir`. Returns whether it hit the time-step
/// limit before the energy had decayed.
fn run_openems(ctx: &JobCtx, exe: &Path, dir: &Path, end_db: f64, threads: usize, label: &str, report: bool) -> Result<bool> {
    let mut cmd = Command::new(exe);
    cmd.arg("sim.xml").arg("--engine=multithreaded").arg(format!("--numThreads={threads}")).current_dir(dir).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let mut child = cmd.spawn().with_context(|| format!("starting {}", exe.display()))?;
    let stdout = child.stdout.take().context("no output from openEMS")?;
    let stderr = child.stderr.take();
    let id = NEXT_CHILD.fetch_add(1, Ordering::SeqCst);
    ctx.children.lock().unwrap().push((id, child));
    let err_reader = stderr.map(|mut e| {
        let live = ctx.live.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            if !s.trim().is_empty() {
                live.lock().unwrap().log.push_back(s.trim().to_string());
            }
            s
        })
    });
    let mut last_err = String::new();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let l = line.trim();
        if l.contains("Timestep:") {
            // "[@ 12s] Timestep: 117 || Speed: 44.2 MC/s (…) || Energy: ~3.7e-20 (-12.34dB)"
            let db = l.rsplit('(').next().and_then(|s| s.trim_end_matches(')').trim_end_matches("dB").replace(' ', "").parse::<f64>().ok()).unwrap_or(0.0);
            let speed = l.split("Speed:").nth(1).and_then(|s| s.split("MC/s").next()).and_then(|s| s.trim().parse::<f64>().ok());
            if let Some(v) = speed {
                ctx.live.lock().unwrap().speed_mcs = v;
            }
            if report {
                let p = (-db / end_db).clamp(0.0, 1.0) as f32;
                ctx.busy(format!("{label}energy {db:.0} of -{end_db:.0} dB · {:.0} MC/s", speed.unwrap_or(0.0)), p);
            }
        } else if l.to_ascii_lowercase().contains("error") {
            last_err = l.to_string();
            ctx.log(l);
        }
    }
    let status = {
        let mut c = ctx.children.lock().unwrap();
        let at = c.iter().position(|(i, _)| *i == id);
        at.map(|i| c.remove(i).1)
    }
    .map(|mut c| c.wait());
    // The step-limit warning only ever goes to stderr, so wait for all of it.
    let err_text = err_reader.and_then(|t| t.join().ok()).unwrap_or_default();
    ctx.check()?;
    match status {
        Some(Ok(s)) if s.success() => Ok(checks::stopped_by_step_limit(&err_text)),
        Some(Ok(s)) => bail!("openEMS exited with {s}{}", if last_err.is_empty() { String::new() } else { format!(": {last_err}") }),
        Some(Err(e)) => Err(e.into()),
        None => bail!("openEMS was stopped"),
    }
}

fn cx(v: post::C) -> C64 {
    C64::new(v.re, v.im)
}

/// One face's equivalent currents: position, J = n×H, M = −n×E, area.
type SurfacePoint = ([f64; 3], [C64; 3], [C64; 3], f64);

fn cross_n(axis: usize, sign: f64, v: [C64; 3]) -> [C64; 3] {
    let mut n = [0.0; 3];
    n[axis] = sign;
    [v[2] * n[1] - v[1] * n[2], v[0] * n[2] - v[2] * n[0], v[1] * n[0] - v[0] * n[1]]
}

fn trapezoid(c: &[f64], i: usize) -> f64 {
    if c.len() < 2 {
        return 1.0;
    }
    let l = if i > 0 { c[i] - c[i - 1] } else { 0.0 };
    let r = if i + 1 < c.len() { c[i + 1] - c[i] } else { 0.0 };
    (l + r) * 0.5
}

fn face_points(e: &(Grid, Vec<[post::C; 3]>), h: &(Grid, Vec<[post::C; 3]>), axis: usize, sign: f64, out: &mut Vec<SurfacePoint>) {
    let g = &e.0;
    for k in 0..g.z.len() {
        for j in 0..g.y.len() {
            for i in 0..g.x.len() {
                let p = i + g.x.len() * (j + g.y.len() * k);
                let w = [trapezoid(&g.x, i), trapezoid(&g.y, j), trapezoid(&g.z, k)];
                let area = (0..3).filter(|a| *a != axis).map(|a| w[a]).product::<f64>();
                let jv = cross_n(axis, sign, h.1[p].map(cx));
                let mv = cross_n(axis, sign, e.1[p].map(cx)).map(|c| -c);
                out.push(([g.x[i], g.y[j], g.z[k]], jv, mv, area));
            }
        }
    }
}

/// openEMS dumps H as zero on a PEC boundary, where tangential H is really at its largest,
/// and that row stands for the band of field nearest the ground: a monopole lost 4 % of
/// its radiated power to it. Tangential H is even about the plane, so the row comes from
/// the parabola through the next two; normal H is zero there.
fn restore_ground_row(h: &mut (Grid, Vec<[post::C; 3]>)) {
    let (g, v) = h;
    if g.z.len() < 3 || g.z[0].abs() > 1e-12 {
        return;
    }
    let (z1, z2) = (g.z[1], g.z[2]);
    let lift = z1 * z1 / (z2 * z2 - z1 * z1);
    let (nx, ny) = (g.x.len(), g.y.len());
    for p in 0..nx * ny {
        let (near, far) = (v[p + nx * ny], v[p + 2 * nx * ny]);
        v[p] = [near[0] + (near[0] - far[0]).scale(lift), near[1] + (near[1] - far[1]).scale(lift), post::C::ZERO];
    }
}

/// The equivalent currents on the closed box of the plan. Over a ground plane the
/// image currents complete the box: J′ = (−Jx, −Jy, Jz), M′ = (Mx, My, −Mz).
fn box_points(dir: &Path, plan: &Plan, f: f64) -> Result<Vec<SurfacePoint>> {
    let mut pts: Vec<SurfacePoint> = Vec::new();
    for (face, axis, sign) in plan.faces() {
        let e = post::read_fd_field(dir, &format!("nf2ff_E_{face}"), f)?;
        let mut h = post::read_fd_field(dir, &format!("nf2ff_H_{face}"), f)?;
        if plan.ground {
            restore_ground_row(&mut h);
        }
        face_points(&e, &h, axis, sign, &mut pts);
    }
    if plan.ground {
        let img: Vec<SurfacePoint> = pts.iter().map(|(p, j, m, a)| ([p[0], p[1], -p[2]], [-j[0], -j[1], j[2]], [m[0], m[1], -m[2]], *a)).collect();
        pts.extend(img);
    }
    Ok(pts)
}

/// Near field → far field on the closed box of the plan.
fn far_field(dir: &Path, plan: &Plan, f: f64, step: f64, scale: f64) -> Result<FarField> {
    let pts = box_points(dir, plan, f)?;
    let k = 2.0 * std::f64::consts::PI * f / C0;
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
    Ok(ff)
}

/// A dumped plane with where it is, in the antenna's own coordinates.
fn read_plane(dir: &Path, name: &str, f: f64) -> Result<FieldPlane> {
    let abs = post::read_vtr(&post::fd_file(dir, name, f, "abs"))?;
    let axes: Vec<usize> = [abs.x.len(), abs.y.len(), abs.z.len()].iter().enumerate().filter(|(_, n)| **n > 1).map(|(k, _)| k).collect();
    if axes.len() != 2 {
        bail!("{name} is not a plane");
    }
    let fixed = 3 - axes[0] - axes[1];
    let coord = |k: usize| match k {
        0 => abs.x.clone(),
        1 => abs.y.clone(),
        _ => abs.z.clone(),
    };
    let (cu, cv, cf) = (coord(axes[0]), coord(axes[1]), coord(fixed));
    let mag = |g: &Grid| -> Vec<f32> { (0..g.len()).map(|p| g.at(p).iter().map(|v| v * v).sum::<f32>().sqrt()).collect() };
    let mut env = mag(&abs);
    let mut frames: Vec<Vec<f32>> = post::phase_frames(dir, name, f).iter().filter_map(|p| post::read_vtr(p).ok()).filter(|g| g.len() == abs.len()).map(|g| mag(&g)).collect();
    // The feed's peak would push everything else off the scale: 99th percentile.
    let mut sorted: Vec<f32> = env.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        bail!("{name} is empty");
    }
    let k = ((sorted.len() - 1) as f32 * 0.99) as usize;
    let reference = (*sorted.select_nth_unstable_by(k, f32::total_cmp).1).max(1e-30);
    env.iter_mut().for_each(|v| *v /= reference);
    frames.iter_mut().flatten().for_each(|v| *v /= reference);
    let mut corner = [0.0; 3];
    corner[axes[0]] = cu[0];
    corner[axes[1]] = cv[0];
    corner[fixed] = cf[0];
    let mut du = [0.0; 3];
    du[axes[0]] = cu[cu.len() - 1] - cu[0];
    let mut dv = [0.0; 3];
    dv[axes[1]] = cv[cv.len() - 1] - cv[0];
    Ok(FieldPlane { name: name.to_string(), nu: cu.len(), nv: cv.len(), u: (cu[0], cu[cu.len() - 1]), v: (cv[0], cv[cv.len() - 1]), corner, du, dv, mag: env, frames })
}

/// A readable name for a plane.
pub fn plane_title(name: &str) -> &'static str {
    match name {
        "E_xy" => "|E| in the xy plane",
        "E_xz" => "|E| in the xz plane",
        "E_yz" => "|E| in the yz plane",
        "H_top" => "|H| just above the metal (surface current)",
        _ => "Field",
    }
}

fn simulate_fdtd(project: &Project, geom: &Geometry, ctx: &JobCtx, opts: &RunOptions) -> Result<SimResult> {
    let Some(exe) = crate::rtls::fdtd::exe() else {
        bail!("The openEMS field solver is not installed yet. Use Install on the Setup page.");
    };
    let t0 = Instant::now();
    let setup = &project.setup;
    let design_f = project.f0();
    let plan = csx::plan(geom, setup, design_f)?;
    for w in &plan.warnings {
        ctx.log(w.clone());
    }
    let dir = run_dir(&project.name)?;
    let threads = if opts.threads > 0 {
        opts.threads
    } else if setup.threads > 0 {
        setup.threads
    } else {
        threads_for(plan.cells(), opts.share)
    };
    let n = geom.ports.len();
    let freqs = linspace(setup.f_lo, setup.f_hi, setup.points.max(2));
    let z0: Vec<f64> = geom.ports.iter().map(|p| p.z0).collect();
    let zero = C64::new(0.0, 0.0);
    let mut s = vec![vec![vec![zero; n]; n]; freqs.len()];
    let mut far: Vec<Vec<FarField>> = Vec::with_capacity(n);
    let mut planes = Vec::new();
    let mut warnings = plan.warnings.clone();
    ctx.log(format!("{}{:.2} M cells, smallest {:.3} mm, {threads} threads", opts.label, plan.cells() as f64 / 1e6, plan.min_cell * 1e3));
    for j in 0..n {
        ctx.check()?;
        let d = dir.join(format!("p{}", j + 1));
        std::fs::create_dir_all(&d)?;
        std::fs::write(d.join("sim.xml"), csx::to_xml(geom, &plan, j))?;
        let label = if n > 1 { format!("{}port {} of {n}: ", opts.label, j + 1) } else { opts.label.clone() };
        if run_openems(ctx, &exe, &d, plan.end_db, threads, &label, opts.report)? {
            warnings.push(format!("{} of {} before the energy decayed to -{:.0} dB: narrow-band results may be off. Raise the step limit.", antenna_core::project::STOPPED_SHORT, setup.max_steps, plan.end_db));
        }
        if opts.report {
            ctx.busy(format!("{label}S-parameters…"), -1.0);
        }
        check_probes(&d, n)?;
        let driven = post::port_fd(&d, (j + 1) as u32, z0[j], &freqs)?;
        let waves = |v: &[post::C]| -> Vec<C64> { v.iter().map(|x| cx(*x)).collect() };
        checks::check_port_waves(j, &freqs, &waves(&driven.inc), &waves(&driven.u), &waves(&driven.i))?;
        for i in 0..n {
            let at_i = if i == j { driven.clone() } else { post::port_fd(&d, (i + 1) as u32, z0[i], &freqs)? };
            for k in 0..freqs.len() {
                // Power waves: b_i/√Z_i over a_j/√Z_j, here with voltage waves.
                s[k][i][j] = cx(at_i.refl[k]) / cx(driven.inc[k]) * (z0[j] / z0[i]).sqrt();
            }
        }
        let mut per_f = Vec::with_capacity(plan.far_freqs.len());
        if !plan.far_freqs.is_empty() {
            let at_far = post::port_fd(&d, (j + 1) as u32, z0[j], &plan.far_freqs)?;
            for (k, f) in plan.far_freqs.iter().enumerate() {
                ctx.check()?;
                if opts.report {
                    ctx.busy(format!("{label}far field at {:.3} GHz…", f / 1e9), -1.0);
                }
                // Normalised to 1 W incident, as the MoM solvers report.
                let a = cx(at_far.inc[k]);
                let p_inc = (0.5 * a.norm_sqr() / z0[j]).max(1e-300);
                let (u, i) = (cx(at_far.u[k]), cx(at_far.i[k]));
                // Keeps its sign: negative means the port returned power, and has to show.
                let p_acc = 0.5 * (u * i.conj()).re;
                let mut ff = far_field(&d, &plan, *f, setup.far_step.max(1.0), 1.0 / p_inc.sqrt())?;
                if !ff.p_rad().is_finite() {
                    bail!("The near-field dump of port {} at {:.3} GHz holds invalid numbers: the run diverged. Try a finer mesh around small features.", j + 1, f / 1e9);
                }
                // The incident wave is the phase reference, as the method of moments reports:
                // only then do ports' patterns add with a state's weights. The raw phase is an
                // arbitrary delay of the time-domain pulse.
                let turn = a.conj() / a.norm().max(1e-300);
                for row in ff.e_theta.iter_mut().chain(ff.e_phi.iter_mut()) {
                    row.iter_mut().for_each(|v| *v *= turn);
                }
                ff.p_incident = 1.0;
                ff.p_accepted = p_acc / p_inc;
                per_f.push(ff);
            }
        }
        far.push(per_f);
        if j == 0 {
            for p in &plan.planes {
                match read_plane(&d, &p.name, design_f) {
                    Ok(v) => planes.push(v),
                    Err(e) => ctx.log(format!("No field plane {}: {e:#}", p.name)),
                }
            }
        }
    }
    let network = Network { f: freqs, z0, s };
    if !opts.keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let lossless = csx::models_no_loss(geom);
    // What the stop level leaves unresolved, in power, with a margin.
    warnings.extend(checks::passivity_warnings(&network, 4.0 * 10f64.powf(-plan.end_db / 20.0)));
    warnings.extend(checks::balance_warnings(&network, &far, lossless, "The far field isn't converged: refine the mesh or lower the stop level."));
    Ok(SimResult {
        label: project.name.clone(),
        solver: SolverKind::Fdtd.label().to_string(),
        network,
        far_f: plan.far_freqs.clone(),
        far,
        planes,
        currents: None,
        size: plan.cells(),
        seconds: t0.elapsed().as_secs_f64(),
        dir: dir.display().to_string(),
        vars: project.vars()?,
        warnings,
        density: setup.cells_per_lambda,
        density_unit: "cells per λ".to_string(),
        end_db: plan.end_db,
        metal: project.metal.clone(),
        lossless,
        convergence: None,
    })
}

/// Every port's probes have to hold numbers: a diverged run can leave NaN on disk, and the
/// probe reader skips what it can't parse, leaving a finite series that looks fine.
fn check_probes(dir: &Path, ports: usize) -> Result<()> {
    for p in 1..=ports {
        for name in [format!("port_ut{p}"), format!("port_it{p}")] {
            let path = dir.join(&name);
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            if let Some(what) = checks::non_finite_sample(&text) {
                bail!("The run diverged: probe {name} holds {what}. Try a finer mesh around small features.");
            }
        }
    }
    Ok(())
}

fn simulate_mom(project: &Project, geom: &Geometry, ctx: &JobCtx, opts: &RunOptions) -> Result<SimResult> {
    let t0 = Instant::now();
    let setup = &project.setup;
    let mom = MomOptions { segments_per_lambda: setup.segments_per_lambda, edges_per_lambda: setup.edges_per_lambda, theta_step: setup.far_step.max(1.0), phi_step: setup.far_step.max(1.0) };
    if opts.report {
        ctx.busy(format!("{}Meshing…", opts.label), -1.0);
    }
    let problem = MomProblem::new(geom, setup.f_hi, mom)?;
    let unknowns = problem.unknowns();
    ctx.log(format!("{}{unknowns} unknowns", opts.label));
    let samples = linspace(setup.f_lo, setup.f_hi, setup.mom_samples.max(3));
    let z0: Vec<f64> = geom.ports.iter().map(|p| p.z0).collect();
    let total = samples.len();
    let mut solved = 0;
    let sweep = sparam::refined_sweep(&samples, z0, total.max(8), |f| {
        ctx.check()?;
        solved += 1;
        if opts.report {
            ctx.busy(format!("{}MoM {:.3} GHz ({solved} solved)", opts.label, f / 1e9), (solved as f32 / total as f32).min(0.99));
        }
        Ok(problem.solve(f, false)?.z)
    })?;
    ctx.log(format!("{}{solved} frequencies solved, {} to check the sampling", opts.label, solved - total));
    let network = sweep.network.interpolate(&linspace(setup.f_lo, setup.f_hi, setup.points.max(2)));
    let n = geom.ports.len();
    let mut far: Vec<Vec<FarField>> = vec![Vec::new(); n];
    let mut current = None;
    for f in &setup.far_freqs {
        ctx.check()?;
        if opts.report {
            ctx.busy(format!("{}Far field at {:.3} GHz…", opts.label, f / 1e9), -1.0);
        }
        let pt = problem.solve(*f, true)?;
        for (p, ff) in far.iter_mut().zip(pt.far) {
            p.push(ff);
        }
        current.get_or_insert(pt.current);
    }
    let currents = current.map(|c| {
        let v = problem.view();
        CurrentView { verts: v.verts, tris: v.tris, segments: v.segments, current: c }
    });
    let (solver, density, density_unit) = match problem.formulation() {
        antenna_core::mom::Formulation::Wire => (SolverKind::MomWire, setup.segments_per_lambda, "segments per λ"),
        antenna_core::mom::Formulation::Surface => (SolverKind::MomSurface, setup.edges_per_lambda, "edges per λ"),
    };
    let lossless = antenna_core::mom::models_no_loss(geom);
    let mut warnings = problem.warnings();
    warnings.extend(sampling_warnings(&sweep, &network));
    warnings.extend(checks::passivity_warnings(&network, 2e-3));
    warnings.extend(checks::balance_warnings(&network, &far, lossless, "The solve or the far-field grid isn't converged: refine the mesh or the grid step."));
    Ok(SimResult {
        label: project.name.clone(),
        solver: solver.label().to_string(),
        network,
        far_f: setup.far_freqs.clone(),
        far,
        planes: Vec::new(),
        currents,
        size: unknowns as u64,
        seconds: t0.elapsed().as_secs_f64(),
        dir: String::new(),
        vars: project.vars()?,
        warnings,
        density,
        density_unit: density_unit.to_string(),
        end_db: 0.0,
        metal: project.metal.clone(),
        lossless,
        convergence: None,
    })
}

/// What the frequency sampling couldn't vouch for: a resonance the refinement ran out of
/// solves on, or one it found that the output sweep is too coarse to draw.
fn sampling_warnings(sweep: &sparam::RefinedSweep, network: &Network) -> Vec<String> {
    let mut out = Vec::new();
    if !sweep.unresolved.is_empty() {
        let at: Vec<String> = sweep.unresolved.iter().take(4).map(|f| format!("{:.4} GHz", f / 1e9)).collect();
        out.push(format!("A resonance near {} is narrower than the solved frequencies can resolve, so S11 there may be far off. Raise Solved frequencies or narrow the sweep.", at.join(", ")));
    }
    for port in 0..network.ports() {
        let (solved, shown) = (sweep.network.sij_db(port, port), network.sij_db(port, port));
        let best = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
        // A notch drawn at −48 dB instead of −58 dB is still a fine picture.
        if best(&shown) > -30.0 && best(&shown) > best(&solved) + 1.0 {
            out.push(format!("Port {}'s resonance is sharper than the output sweep shows: {:.1} dB solved, {:.1} dB drawn. Raise Output points.", port + 1, best(&solved), best(&shown)));
        }
    }
    out
}

/// One simulation with the solver the project asks for. With switches or varactors on the
/// model it comes back as the project's active state, feeds only, so a sweep, an optimiser or
/// a results page takes it like any other antenna.
pub fn simulate(project: &Project, ctx: &JobCtx, opts: &RunOptions) -> Result<SimResult> {
    let basis = simulate_ports(project, ctx, opts)?;
    let rc = &project.reconfig;
    if !rc.is_on() {
        return Ok(basis);
    }
    let geom = project.geometry()?;
    let names: Vec<String> = geom.ports.iter().map(|p| p.name.clone()).collect();
    let state = rc.active_state().cloned().unwrap_or_else(|| rc.blank_state("All off"));
    let solved = Solved::new(&basis, &names, rc)?;
    let mut r = solved.result(rc, &state)?;
    // Redrawing the currents costs a factorisation, which only a run someone looks at needs.
    if opts.keep
        && let Some(f) = basis.far_f.first()
        && project.solver_for(&geom) != SolverKind::Fdtd
    {
        r.currents = state_currents(project, &geom, &solved, &rc.settings_of(&state), *f).ok();
    }
    Ok(r)
}

/// Every port's network and far fields, with nothing connected to them yet.
pub fn simulate_ports(project: &Project, ctx: &JobCtx, opts: &RunOptions) -> Result<SimResult> {
    project.setup.check_band()?;
    let geom = project.geometry()?;
    match project.solver_for(&geom) {
        SolverKind::Fdtd => simulate_fdtd(project, &geom, ctx, opts),
        _ => simulate_mom(project, &geom, ctx, opts),
    }
}

/// The surface or wire currents of one reconfigurable state, with 1 W incident at the first
/// feed: which metal is really working is the first thing to look at. Method of moments only.
pub fn state_currents(project: &Project, geom: &Geometry, solved: &Solved, settings: &[Setting], f: f64) -> Result<CurrentView> {
    let setup = &project.setup;
    let opts = MomOptions { segments_per_lambda: setup.segments_per_lambda, edges_per_lambda: setup.edges_per_lambda, ..Default::default() };
    let problem = MomProblem::new(geom, setup.f_hi, opts)?;
    let waves = solved.waves(&project.reconfig, settings, f).context("those settings have no finite answer at that frequency")?;
    let current = problem.currents_for_waves(f, &waves)?;
    let v = problem.view();
    Ok(CurrentView { verts: v.verts, tris: v.tris, segments: v.segments, current })
}

/// The per-port fields through a gate's read zone, by whichever solver the model takes.
/// FDTD runs once per port, then its near-field box answers for points outside it,
/// so the grid doesn't have to reach the whole aisle.
pub fn gate_field(project: &Project, spec: &GateSpec, ctx: &JobCtx) -> Result<GateField> {
    let geom = project.geometry()?;
    let setup = &project.setup;
    let step = setup.far_step.max(1.0);
    let opts = MomOptions { segments_per_lambda: setup.segments_per_lambda, edges_per_lambda: setup.edges_per_lambda, theta_step: step, phi_step: step };
    // A gate is solved at one frequency, so it's the mesh there that decides, not the
    // mesh for the top of the sweep band that an ordinary run would need.
    let fit = geom.solver_fit();
    let use_mom = match setup.solver {
        SolverKind::Auto => (fit.wire_mom || fit.surface_mom) && MomProblem::new(&geom, spec.f, opts).is_ok_and(|m| m.unknowns() <= antenna_core::project::AUTO_MOM_LIMIT),
        SolverKind::Fdtd => false,
        _ => true,
    };
    if use_mom {
        ctx.busy("Solving the gate…", -1.0);
        return gate::solve_mom(&geom, spec, opts);
    }
    let t0 = Instant::now();
    let f = spec.f;
    let mut p = project.clone();
    p.setup.far_freqs = vec![f];
    p.setup.field_planes = false;
    p.setup.f_lo = p.setup.f_lo.min(f * 0.95);
    p.setup.f_hi = p.setup.f_hi.max(f * 1.05);
    let plan = csx::plan(&geom, &p.setup, f)?;
    // A sample spacing clear of the box, where its currents are smooth to sum.
    let clear = plan.max_cell;
    let keep_out = [(std::array::from_fn(|k| plan.nf_min[k] - clear), std::array::from_fn(|k| plan.nf_max[k] + clear))];
    let samples = gate::samples(&geom, spec, &keep_out)?;
    let r = simulate_fdtd(&p, &geom, ctx, &RunOptions::default())?;
    let k = 2.0 * std::f64::consts::PI * f / C0;
    let root = PathBuf::from(&r.dir);
    let mut e = Vec::with_capacity(geom.ports.len());
    let mut far = Vec::with_capacity(geom.ports.len());
    for (j, port) in geom.ports.iter().enumerate() {
        ctx.check()?;
        ctx.busy(format!("Fields through the zone, port {} of {}…", j + 1, geom.ports.len()), -1.0);
        let d = root.join(format!("p{}", j + 1));
        let at = post::port_fd(&d, (j + 1) as u32, port.z0, &[f])?;
        // 1 W incident with the incident wave as the phase reference, as the method of
        // moments reports: only then do the ports add with a state's phases. The raw
        // phase is an arbitrary delay of the time-domain pulse.
        let a = cx(at.inc[0]);
        let p_inc = (0.5 * a.norm_sqr() / port.z0).max(1e-300);
        let turn = a.conj() / a.norm().max(1e-300);
        let scale = turn / p_inc.sqrt();
        let currents: Vec<antenna_core::mom::Huygens> = box_points(&d, &plan, f)?.into_iter().map(|(pos, j, m, area)| antenna_core::mom::Huygens { pos, j, m, area }).collect();
        let field: Vec<[C64; 3]> = samples.points.par_iter().map(|q| antenna_core::mom::field_outside(&currents, k, *q).map(|v| v * scale)).collect();
        e.push(field);
        // `simulate_fdtd` has already referred these to the incident wave.
        if let Some(ff) = r.far.get(j).and_then(|per_f| per_f.first().cloned()) {
            far.push(ff);
        }
    }
    let network = r.network.interpolate(&[f]);
    Ok(GateField { f, solver: "FDTD (openEMS)".into(), size: r.size, seconds: t0.elapsed().as_secs_f64(), samples, e, network, far, ports: geom.ports.iter().map(|x| x.name.clone()).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> JobCtx {
        JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() }
    }

    fn project(id: &str, f0: &str, cells: f64) -> Project {
        let mut p = Project::from_template(id, Some(f0)).unwrap();
        p.setup.cells_per_lambda = cells;
        p.setup.field_planes = false;
        p
    }

    /// Pages from real solves: a wire with one port, a Yagi with several far fields, and a
    /// gate with five ports, its states and its gate test. Set `ANTENNA_REPORT_DIR` to keep them.
    #[test]
    fn antenna_reports_from_real_runs_hold_together() {
        use antenna_core::report::{Extras, GateSection, LinkInputs, html_with};
        let regions = crate::antenna::ui::solve::rfid_limits(&crate::rtls::region::RegionState { regions: crate::rtls::region::load(), active: None, base: Default::default() });
        assert!(regions.iter().any(|r| r.name.contains("Australia") && r.eirp_dbm == Some(36.0)), "{regions:?}");
        let link = Some(LinkInputs { tx_dbm: 30.0, rx_gain_dbi: 2.0, sensitivity_dbm: -18.0, margin_db: 3.0 });
        let keep = std::env::var("ANTENNA_REPORT_DIR").ok();
        let c = ctx();
        let dipole = project("dipole", "915MHz", 20.0);
        let mut yagi = project("yagi", "915MHz", 20.0);
        yagi.setup.far_freqs = vec![880e6, 915e6, 950e6];
        yagi.setup.mom_samples = 21;
        let mut panel = Project::from_template("rfid_gate", None).unwrap();
        panel.fit_band(902e6, 928e6);
        panel.states = gate::preset_states(gate::Preset::Sweep, 5);
        for (name, p) in [("dipole", dipole), ("yagi", yagi), ("rfid_gate", panel)] {
            let r = simulate(&p, &c, &RunOptions { keep: false, ..Default::default() }).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let gate_test = (name == "rfid_gate").then(|| {
                let geometry = p.geometry().unwrap();
                let mut spec = GateSpec::for_project(&p, &geometry);
                spec.spacing = 0.25;
                let field = gate_field(&p, &spec, &c).unwrap_or_else(|e| panic!("{e:#}"));
                let report = gate::evaluate(&field, &spec, &p.states);
                let (read, stray) = gate::predict(&report, &spec, 100);
                GateSection { read_range: (read.prediction.low, read.prediction.high), stray_range: (stray.prediction.low, stray.prediction.high), evidence: read.prediction.evidence.label().to_string(), assumptions: gate::assumptions(&field, &spec), report, spec }
            });
            let has_gate = gate_test.is_some();
            let page = html_with(&p, &r, &Extras { stamp: "Exported by a test".into(), link, regions: regions.clone(), mesh_study: Vec::new(), gate: gate_test });
            for needle in ["At an angle", "Front: x across", "Key frequencies", "Smith chart", "Peak realized gain", "Checks", "Design variables", "Download the S-parameters", "Current on the metal"] {
                assert!(page.contains(needle), "{name}: no {needle}");
            }
            for bad in ["NaN", ">inf", " inf ", "-inf"] {
                assert!(!page.contains(bad), "{name}: {bad} in the page");
            }
            assert_eq!(page.contains("Australia"), name == "rfid_gate", "{name}: RFID limits belong on RFID antennas");
            assert_eq!(page.contains("Between the ports"), name == "rfid_gate", "{name}");
            assert_eq!(page.contains("RFID gate test"), has_gate, "{name}");
            assert!(page.len() < 6_000_000, "{name}: {} bytes", page.len());
            eprintln!("{name}: {} kB, {} unknowns, {} warnings", page.len() / 1000, r.size, r.warnings.len());
            if let Some(dir) = &keep {
                std::fs::write(format!("{dir}/{name}.html"), &page).unwrap();
            }
        }
    }

    /// An FDTD run: dielectric in the drawing, field planes in the page. Needs openEMS.
    #[test]
    #[ignore]
    fn antenna_report_from_fdtd_run_holds_together() {
        let mut p = project("patch_probe", "2.45GHz", 20.0);
        p.setup.field_planes = true;
        let r = simulate(&p, &ctx(), &RunOptions { keep: false, ..Default::default() }).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(!r.planes.is_empty(), "planes were asked for");
        let page = antenna_core::report::html(&p, &r);
        for needle in ["Field planes", "data:image/png;base64,", "cells per λ", "FR-4", "Stopped at"] {
            assert!(page.contains(needle), "no {needle}");
        }
        assert!(!page.contains("NaN"));
        if let Ok(dir) = std::env::var("ANTENNA_REPORT_DIR") {
            std::fs::write(format!("{dir}/patch_probe.html"), &page).unwrap();
        }
    }

    /// The narrowest regional band still has to give a usable sweep: EU's is only 3 MHz wide.
    #[test]
    fn the_gate_solves_across_a_regional_rfid_band() {
        let mut p = Project::from_template("rfid_gate", None).unwrap();
        p.fit_band(865e6, 868e6);
        let c = ctx();
        let r = simulate(&p, &c, &RunOptions { keep: false, ..Default::default() }).unwrap_or_else(|e| panic!("{e:#}"));
        let (lo, hi) = (r.network.f[0], *r.network.f.last().unwrap());
        assert!((lo - 865e6).abs() < 1.0 && (hi - 868e6).abs() < 1.0, "{lo} to {hi}");
        assert_eq!(r.network.s[0].len(), 5, "five ports");
        assert!(r.network.sij_db(0, 0).iter().all(|db| db.is_finite()));
        assert!(r.metrics(0, 0).is_some(), "a far field at the band's centre");
    }

    fn fdtd(mut p: Project) -> SimResult {
        p.setup.solver = SolverKind::Fdtd;
        let c = ctx();
        let opts = RunOptions { keep: false, ..Default::default() };
        simulate(&p, &c, &opts).unwrap_or_else(|e| panic!("{e:#}\n{:?}", c.live.lock().unwrap().log))
    }

    fn at(r: &SimResult, f: f64) -> (f64, C64) {
        let k = r.network.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).unwrap().0;
        (r.network.sij_db(0, 0)[k], r.network.zin(0)[k])
    }

    /// What openEMS leaves on the PEC row is zero; the field there is the plane's maximum.
    #[test]
    fn antenna_ground_row_of_the_near_field_box_is_restored() {
        let k = 2.0 * std::f64::consts::PI / 0.3;
        let z = vec![0.0, 0.0079, 0.0164, 0.025];
        let g = Grid { x: vec![0.1], y: vec![0.0, 0.05], z: z.clone(), ncomp: 3, data: Vec::new() };
        let field = |zk: f64| [post::C::new((k * zk).cos(), 0.0), post::C::new(0.0, 2.0 * (k * zk).cos()), post::C::new(0.3 * (k * zk).sin(), 0.0)];
        let mut v: Vec<[post::C; 3]> = z.iter().flat_map(|zk| [field(*zk), field(*zk)]).collect();
        v[0] = [post::C::ZERO; 3];
        v[1] = [post::C::ZERO; 3];
        let mut h = (g, v);
        restore_ground_row(&mut h);
        for row in &h.1[..2] {
            assert!((row[0].re - 1.0).abs() < 2e-3, "{row:?}");
            assert!((row[1].im - 2.0).abs() < 4e-3, "{row:?}");
            assert_eq!(row[2], post::C::ZERO, "normal H is zero on PEC");
        }
        // A face that doesn't reach the plane is left alone, like the top of the box.
        let top = Grid { x: vec![0.1, 0.2], y: vec![0.1, 0.2], z: vec![0.145], ncomp: 3, data: Vec::new() };
        let mut h = (top, vec![field(0.1); 4]);
        restore_ground_row(&mut h);
        assert_eq!(h.1[0], field(0.1));
    }

    /// A diverged run can leave text the probe reader skips over, and the series it keeps looks fine.
    #[test]
    fn antenna_a_probe_with_a_nan_fails_the_run() {
        let dir = std::env::temp_dir().join(format!("sensesim_probe_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, last: &str| std::fs::write(dir.join(name), format!("% time/s\tvalue\n0\t0\n1e-12\t{last}\n")).unwrap();
        write("port_ut1", "2e-3");
        write("port_it1", "1e-5");
        assert!(check_probes(&dir, 1).is_ok());
        write("port_it1", "-nan(ind)");
        let e = check_probes(&dir, 1).unwrap_err().to_string();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(e.contains("diverged") && e.contains("port_it1"), "{e}");
    }

    /// A run cut short says so. openEMS tells stderr, which used to go unread.
    #[test]
    #[ignore]
    fn antenna_fdtd_step_limit_is_reported() {
        let mut p = project("strip_dipole", "1GHz", 12.0);
        p.setup.max_steps = 1500;
        let r = fdtd(p);
        assert!(r.warnings.iter().any(|w| w.contains("step limit")), "{:?}", r.warnings);
        assert!(r.stopped_short(), "recipes refuse a run on this: {:?}", r.warnings);
    }

    /// `cargo test --bin SenseSim --features antenna-studio antenna -- --ignored --nocapture`
    /// needs openEMS installed. The reference antenna: pattern, match, power balance.
    #[test]
    #[ignore]
    fn antenna_fdtd_dipole_matches_theory() {
        let r = fdtd(project("strip_dipole", "1GHz", 20.0));
        let m = r.metrics(0, 0).unwrap();
        let (s, z) = at(&r, 1e9);
        let x0 = zero_reactance(&r).unwrap();
        eprintln!("strip dipole FDTD: {} cells, {:.0} s, S11 {s:.1} dB, Z {z:.1}, X = 0 at {:.1} MHz, D {:.3}, eff {:.4}, peak θ {}", r.size, r.seconds, x0 / 1e6, m.peak_directivity_dbi, m.rad_efficiency, m.peak_theta);
        assert!((m.peak_directivity_dbi - 2.15).abs() < 0.05, "D {}", m.peak_directivity_dbi);
        assert!((m.peak_theta - 90.0).abs() <= 3.0, "beam at {}", m.peak_theta);
        assert!((m.rad_efficiency - 1.0).abs() < 0.01, "a lossless antenna radiates what it accepts: {}", m.rad_efficiency);
        // Over 24 to 81 cells per λ this settles near 1003.7 MHz, first-order in the cell.
        assert!((x0 / 1.0037e9 - 1.0).abs() < 0.017, "X = 0 at {x0}");
        assert!((z - C64::new(74.6, 4.3)).norm() < 2.0, "{z}");
    }

    /// Infinite ground: PEC boundary and image currents in the transform.
    #[test]
    #[ignore]
    fn antenna_fdtd_monopole_on_ground() {
        let r = fdtd(project("monopole", "1GHz", 24.0));
        let m = r.metrics(0, 0).unwrap();
        let (s, z) = at(&r, 1e9);
        eprintln!("monopole FDTD: {} cells, {:.0} s, S11 {s:.1} dB, Z {z:.1}, D {:.3}, eff {:.4}, peak θ {}", r.size, r.seconds, m.peak_directivity_dbi, m.rad_efficiency, m.peak_theta);
        assert!((m.peak_directivity_dbi - 5.15).abs() < 0.05, "D {}", m.peak_directivity_dbi);
        assert!(m.peak_theta >= 84.0 && m.peak_theta <= 90.0, "beam at {}", m.peak_theta);
        // This read 0.957 while openEMS's zero H on the ground row went into the transform.
        assert!((m.rad_efficiency - 1.0).abs() < 0.01, "{}", m.rad_efficiency);
        assert!((z - C64::new(38.3, 7.3)).norm() < 2.0, "{z}");
    }

    /// A lossy printed patch: resonance near design, beam up, FR-4 eats power.
    #[test]
    #[ignore]
    fn antenna_fdtd_patch_on_fr4() {
        let r = fdtd(project("patch_probe", "2.45GHz", 20.0));
        let m = r.metrics(0, 0).unwrap();
        let res = r.network.resonances(0);
        eprintln!("patch FDTD: {} cells, {:.0} s, resonances {res:?}, D {:.2}, G {:.2}, eff {:.3}, peak θ {}", r.size, r.seconds, m.peak_directivity_dbi, m.peak_gain_dbi, m.rad_efficiency, m.peak_theta);
        assert!(res.iter().any(|(f, db)| (f / 2.45e9 - 1.0).abs() < 0.01 && *db < -20.0), "{res:?}");
        assert!(m.peak_theta <= 3.0, "beam at {}", m.peak_theta);
        assert!((m.peak_directivity_dbi - 5.75).abs() < 0.2, "{}", m.peak_directivity_dbi);
        assert!((0.42..0.49).contains(&m.rad_efficiency), "FR-4 is lossy: {}", m.rad_efficiency);
    }

    /// Two ports: the full S-matrix is reciprocal and each port has its pattern.
    #[test]
    #[ignore]
    fn antenna_fdtd_two_ports_are_reciprocal() {
        let mut p = project("mimo_patch", "2.45GHz", 14.0);
        // A −22 dB coupling needs the ring-down far below that: stopping at −40 dB leaves
        // about 1 % of the amplitude uncounted, which is 13 % of this S21.
        p.setup.end_db = 60.0;
        p.setup.points = 101;
        let r = fdtd(p);
        assert_eq!(r.network.ports(), 2);
        assert_eq!(r.far.len(), 2);
        let a = r.network.sij_db(1, 0);
        let peak = a.iter().copied().fold(f64::MIN, f64::max);
        // Compare the complex values, near where there is coupling to measure: in a
        // deep null a tiny absolute error is a large number of dB.
        let mut worst: f64 = 0.0;
        for k in 0..a.len() {
            if a[k] > peak - 15.0 {
                let (x, y) = (r.network.s[k][1][0], r.network.s[k][0][1]);
                worst = worst.max((x - y).norm() / x.norm());
            }
        }
        let ecc = antenna_core::sparam::ecc_from_far(&r.far[0][0], &r.far[1][0]);
        eprintln!("two ports: worst relative |S21-S12| {:.2} % within 15 dB of the peak, peak S21 {peak:.1} dB, ECC {ecc:.3}", worst * 100.0);
        assert!(worst < 0.03, "S21 and S12 differ by {:.1} %", worst * 100.0);
        assert!(peak < -5.0);
    }

    /// Where every FDTD-only template really lands. A survey, not a pass/fail:
    /// `cargo test --bin SenseSim --features antenna-studio antenna_fdtd_template_audit -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn antenna_fdtd_template_audit() {
        // AUDIT="patch_probe:yf=0.2*L;patch_probe:yf=0.3*L,h=1mm" tries values without a rebuild.
        let list = std::env::var("AUDIT").unwrap_or_else(|_| "uwb_monopole;ifa_pcb;patch_circular;patch_inset;dra;vivaldi;horn;patch_probe".to_string());
        for case in list.split(';') {
            let (id, sets) = case.split_once(':').unwrap_or((case, ""));
            let mut p = Project::from_template(id, None).unwrap();
            for set in sets.split(',').filter(|s| !s.is_empty()) {
                let (k, v) = set.split_once('=').unwrap();
                p.params.iter_mut().find(|r| r.name == k).unwrap().expr = v.to_string();
            }
            let id = case;
            p.setup.cells_per_lambda = std::env::var("AUDIT_CELLS").ok().and_then(|v| v.parse().ok()).unwrap_or(14.0);
            p.setup.field_planes = false;
            p.setup.points = 201;
            let r = fdtd(p.clone());
            let f0 = p.f0();
            let (s, z) = at(&r, f0);
            let best = r.network.f.iter().zip(r.network.sij_db(0, 0)).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            let m = r.metrics(0, 0).unwrap();
            eprintln!(
                "AUDIT {id:16} {:7} cells {:5.0} s  Z {:7.1}{:+7.1}j  S11@f0 {s:6.1}  best {:6.1} at {:.3}·f0  RG {:5.2} dBi θ{:3.0} φ{:3.0}  eff {:.3}",
                r.size, r.seconds, z.re, z.im, best.1, best.0 / f0, m.peak_realized_dbi, m.peak_theta, m.peak_phi, m.rad_efficiency
            );
        }
    }

    /// Where X crosses zero going up, port 1: the resonance as Z sees it, free of the S11 reference.
    fn zero_reactance(r: &SimResult) -> Option<f64> {
        let z = r.network.zin(0);
        let f = &r.network.f;
        (1..z.len()).find(|k| z[*k - 1].im < 0.0 && z[*k].im >= 0.0).map(|k| f[k - 1] + (f[k] - f[k - 1]) * (-z[k - 1].im) / (z[k].im - z[k - 1].im))
    }

    /// A mesh and stop-level survey, not a pass/fail.
    /// `STUDY="fdtd:strip_dipole:20:50;mom:strip_dipole:48:0" cargo test --bin SenseSim antenna_study -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn antenna_study() {
        let list = std::env::var("STUDY").unwrap_or_else(|_| "fdtd:strip_dipole:20:30;fdtd:strip_dipole:20:60;mom:strip_dipole:12:0;mom:strip_dipole:48:0".to_string());
        for case in list.split(';') {
            let v: Vec<&str> = case.split(':').collect();
            let (solver, id, density, stop): (&str, &str, f64, f64) = (v[0], v[1], v[2].parse().unwrap(), v[3].parse().unwrap());
            let mut p = project(id, "1GHz", density);
            p.setup.points = 1201;
            // An optional fifth field sets a variable: `fdtd:strip_dipole:36:50:g=lambda0/240`.
            if let Some((k, x)) = v.get(4).and_then(|s| s.split_once('=')) {
                p.params.iter_mut().find(|r| r.name == k).unwrap().expr = x.to_string();
            }
            let r = if solver == "fdtd" {
                p.setup.end_db = stop;
                p.setup.solver = SolverKind::Fdtd;
                let c = ctx();
                simulate(&p, &c, &RunOptions { keep: true, ..Default::default() }).unwrap_or_else(|e| panic!("{e:#}\n{:?}", c.live.lock().unwrap().log))
            } else {
                p.setup.edges_per_lambda = density;
                p.setup.segments_per_lambda = density;
                p.setup.mom_samples = 31;
                let c = ctx();
                simulate(&p, &c, &RunOptions::default()).unwrap_or_else(|e| panic!("{e:#}"))
            };
            let m = r.metrics(0, 0).unwrap();
            let (s, z) = at(&r, 1e9);
            eprintln!(
                "STUDY {case}: size {} {:.0} s | res {:?} | X=0 at {:?} | S11(f0) {s:.2} dB Z {z:.2} | D {:.3} dBi | eff {:.4} | dir {}",
                r.size, r.seconds, r.network.resonances(0).first().map(|x| (x.0 / 1e6, x.1)), zero_reactance(&r).map(|f| f / 1e6), m.peak_directivity_dbi, m.rad_efficiency, r.dir
            );
        }
    }

    /// Power at each stage of a kept run, for a power-balance question: the feed's
    /// ½Re(UI*), the Poynting flux through the near-field box, the far-field integral.
    /// `STUDY_DIR=<run>/p1 STUDY_F=1e9 STUDY_Z0=50 STUDY_GROUND=1 cargo test --bin SenseSim antenna_power_stages -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn antenna_power_stages() {
        let dir = PathBuf::from(std::env::var("STUDY_DIR").unwrap());
        let env_num = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (f, z0) = (env_num("STUDY_F", 1e9), env_num("STUDY_Z0", 50.0));
        let ground = env_num("STUDY_GROUND", 0.0) != 0.0;
        let p = post::port_fd(&dir, 1, z0, &[f]).unwrap();
        let (u, i, a) = (cx(p.u[0]), cx(p.i[0]), cx(p.inc[0]));
        let p_acc = 0.5 * (u * i.conj()).re;
        let p_inc = 0.5 * a.norm_sqr() / z0;
        let (mut flux, mut pts): (f64, Vec<SurfacePoint>) = (0.0, Vec::new());
        for (face, axis, sign) in csx::FACES {
            if ground && face == "zn" {
                continue;
            }
            let e = post::read_fd_field(&dir, &format!("nf2ff_E_{face}"), f).unwrap();
            let mut h = post::read_fd_field(&dir, &format!("nf2ff_H_{face}"), f).unwrap();
            if ground {
                restore_ground_row(&mut h);
            }
            let before = pts.len();
            face_points(&e, &h, axis, sign, &mut pts);
            let mut here = 0.0;
            for (k, (_, _, _, area)) in pts[before..].iter().enumerate() {
                let (ev, hv) = (e.1[k].map(cx), h.1[k].map(cx));
                let c = [ev[1] * hv[2].conj() - ev[2] * hv[1].conj(), ev[2] * hv[0].conj() - ev[0] * hv[2].conj(), ev[0] * hv[1].conj() - ev[1] * hv[0].conj()];
                here += 0.5 * c[axis].re * sign * area;
            }
            eprintln!("face {face}: flux {:.5} of accepted, {} samples, z {:?}", here / p_acc, pts.len() - before, e.0.z.iter().take(6).collect::<Vec<_>>());
            flux += here;
            if face == "xp" {
                // The tangential field up the middle of the face.
                let g = &h.0;
                let j = g.y.len() / 2;
                for k in 0..g.z.len().min(8) {
                    let q = g.x.len() * (j + g.y.len() * k);
                    eprintln!("  z {:.5}: |H| {:?} |E| {:?}", g.z[k], h.1[q].map(|c| (cx(c).norm() * 1e3 * 1e12).round() / 1e3), e.1[q].map(|c| (cx(c).norm() * 1e12).round() / 1e3));
                }
            }
        }
        eprintln!("accepted {p_acc:.5e} of incident {p_inc:.5e} (S11 {:.2} dB); flux/accepted {:.5}", 10.0 * (1.0 - p_acc / p_inc).abs().log10(), flux / p_acc);
    }

    /// Two very different solvers must agree on the same metal antenna.
    #[test]
    #[ignore]
    fn antenna_fdtd_and_mom_agree() {
        // Resonance as X = 0, which doesn't depend on the S11 reference. Each pair of limits
        // is what this strip really holds: the two meshes err opposite ways, and even
        // converged about 1 % stays between FDTD's gap and MoM's delta gap.
        for (cells, edges, limit) in [(24.0, 12.0, 0.032), (36.0, 96.0, 0.02)] {
            let base = project("strip_dipole", "1GHz", cells);
            let mut mom = base.clone();
            mom.setup.solver = SolverKind::MomSurface;
            mom.setup.edges_per_lambda = edges;
            let c = ctx();
            let a = simulate(&mom, &c, &RunOptions::default()).unwrap();
            let b = fdtd(base);
            let (fa, fb) = (zero_reactance(&a).unwrap(), zero_reactance(&b).unwrap());
            let (ma, mb) = (a.metrics(0, 0).unwrap(), b.metrics(0, 0).unwrap());
            eprintln!("MoM {edges} edges/λ {} unknowns {:.1} s: X = 0 at {:.1} MHz, D {:.3}; FDTD {cells} cells/λ {} cells {:.0} s: X = 0 at {:.1} MHz, D {:.3}; {:.2} % apart", a.size, a.seconds, fa / 1e6, ma.peak_directivity_dbi, b.size, b.seconds, fb / 1e6, mb.peak_directivity_dbi, (fa / fb - 1.0) * 100.0);
            assert!((fa / fb - 1.0).abs() < limit, "resonances {fa} vs {fb}");
            assert!((ma.peak_directivity_dbi - mb.peak_directivity_dbi).abs() < 0.05, "{} {}", ma.peak_directivity_dbi, mb.peak_directivity_dbi);
        }
    }

    /// FDTD and MoM on the same gate, field by field through the aisle, and the read
    /// shares that come out. A smaller gate than the template, to keep it a few minutes:
    /// `cargo test --bin SenseSim antenna_fdtd_gate_agrees_with_mom -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn antenna_fdtd_gate_agrees_with_mom() {
        let mut p = Project::from_template("rfid_gate", None).unwrap();
        // Both vertical: their fields interfere, so a wrong phase between ports shows.
        for (name, v) in [("N", "2"), ("zc", "0.8"), ("dz", "0.26"), ("pol", "0")] {
            p.params.iter_mut().find(|r| r.name == name).unwrap().expr = v.to_string();
        }
        p.setup.cells_per_lambda = 24.0;
        p.setup.end_db = 50.0;
        p.setup.points = 51;
        let g = p.geometry().unwrap();
        let mut spec = GateSpec::for_project(&p, &g);
        spec.aisle.low = 0.4;
        spec.aisle.high = 1.2;
        spec.aisle.start = 0.0;
        spec.aisle.width = 1.0;
        spec.aisle.length = 0.8;
        spec.aisle.stray_from = 1.0;
        spec.aisle.stray_depth = 0.6;
        spec.spacing = 0.2;
        let states = gate::preset_states(gate::Preset::InPhase, 2);
        let c = ctx();
        let mut mom = p.clone();
        mom.setup.solver = SolverKind::MomSurface;
        mom.setup.edges_per_lambda = 20.0;
        let a = gate_field(&mom, &spec, &c).unwrap();
        let mut f = p.clone();
        f.setup.solver = SolverKind::Fdtd;
        let b = gate_field(&f, &spec, &c).unwrap_or_else(|e| panic!("{e:#}\n{:?}", c.live.lock().unwrap().log));
        eprintln!("MoM {} unknowns, {:.1} s; FDTD {} cells, {:.0} s", a.size, a.seconds, b.size, b.seconds);
        // FDTD leaves out what lies inside its near-field box, so compare where both have a value.
        let index: std::collections::HashMap<[u64; 3], usize> = a.samples.points.iter().enumerate().map(|(i, q)| (q.map(f64::to_bits), i)).collect();
        let (mut num, mut den, mut worst_db, mut count) = (0.0, 0.0, 0.0f64, 0);
        for port in 0..2 {
            for (ib, q) in b.samples.points.iter().enumerate() {
                let Some(&ia) = index.get(&q.map(f64::to_bits)) else { continue };
                let (u, v) = (a.e[port][ia], b.e[port][ib]);
                let sq = |w: [C64; 3]| w[0].norm_sqr() + w[1].norm_sqr() + w[2].norm_sqr();
                let d = [u[0] - v[0], u[1] - v[1], u[2] - v[2]];
                num += sq(d);
                den += sq(u);
                worst_db = worst_db.max((10.0 * (sq(u) / sq(v)).log10()).abs());
                count += 1;
            }
        }
        let rel = (num / den as f64).sqrt();
        eprintln!("{count} field samples: complex difference {:.1} %, worst |E| {worst_db:.2} dB apart", rel * 100.0);
        eprintln!("S11 MoM {:.1} dB, FDTD {:.1} dB; S21 MoM {:.1} dB, FDTD {:.1} dB", antenna_core::db20(a.network.s[0][0][0].norm()), antenna_core::db20(b.network.s[0][0][0].norm()), antenna_core::db20(a.network.s[0][1][0].norm()), antenna_core::db20(b.network.s[0][1][0].norm()));
        assert!(rel < 0.25, "fields differ by {:.1} %", rel * 100.0);
        // Under phases, and at a lower power where a share has room to differ.
        let low = GateSpec { tx_dbm: 10.0, ..spec };
        for step in [0, 90, 180] {
            let state = if step == 0 { states.clone() } else { gate::preset_states(gate::Preset::Progressive(step), 2) };
            let (ra, rb) = (gate::evaluate(&a, &low, &state), gate::evaluate(&b, &low, &state));
            // Both solvers' tables, lane by lane, where both have the whole lane.
            let mean_db = |t: &[f64]| t.iter().copied().filter(|v| v.is_finite()).sum::<f64>() / t.iter().filter(|v| v.is_finite()).count().max(1) as f64;
            let (ma, mb) = (mean_db(&ra.walk_table), mean_db(&rb.walk_table));
            let eirp = (ra.states[0].eirp_dbm.unwrap(), rb.states[0].eirp_dbm.unwrap());
            eprintln!("{step:3}° per element: read MoM {:.3}, FDTD {:.3}; stray MoM {:.3}, FDTD {:.3}; EIRP MoM {:.2}, FDTD {:.2} dBm; mean walk gain MoM {ma:.2}, FDTD {mb:.2} dB", ra.read, rb.read, ra.stray, rb.stray, eirp.0, eirp.1);
            assert!((eirp.0 - eirp.1).abs() < 1.0, "EIRP at {step}°: {eirp:?}");
        }
    }

    /// The checks have to stay quiet on sound models, or nobody will read them. Skips what
    /// is too large to sweep in a test.
    /// `cargo test --bin SenseSim antenna_mom_templates_are_quiet -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn antenna_mom_templates_are_quiet() {
        let mut noisy = Vec::new();
        for t in antenna_core::templates::all() {
            let Ok(mut p) = Project::from_template(t.id, None) else { continue };
            let Ok(g) = p.geometry() else { continue };
            let fit = g.solver_fit();
            if !fit.wire_mom && !fit.surface_mom {
                continue;
            }
            if MomProblem::new(&g, p.setup.f_hi, MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, ..Default::default() }).is_ok_and(|m| m.unknowns() > 4500) {
                continue;
            }
            p.setup.mom_samples = p.setup.mom_samples.min(21);
            p.setup.field_planes = false;
            let c = ctx();
            let r = simulate(&p, &c, &RunOptions::default()).unwrap_or_else(|e| panic!("{}: {e:#}", t.id));
            eprintln!("{:<18} {:.1} s, {:?}, {:?}", t.id, r.seconds, c.live.lock().unwrap().log.back(), r.warnings);
            // One short segment at the helix's feed is real and stated on purpose.
            if !r.warnings.is_empty() && t.id != "helix_axial" {
                noisy.push(format!("{}: {:?}", t.id, r.warnings));
            }
        }
        assert!(noisy.is_empty(), "{}", noisy.join("\n"));
    }

    /// What a run knows about itself reaches the result, and from there the report.
    #[test]
    fn antenna_mom_result_says_how_it_was_made() {
        let mut p = project("dipole", "1GHz", 20.0);
        p.setup.segments_per_lambda = 6.0;
        p.setup.mom_samples = 9;
        let c = ctx();
        let r = simulate(&p, &c, &RunOptions::default()).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(r.warnings.iter().any(|w| w.contains("per wavelength")), "{:?}", r.warnings);
        assert_eq!((r.density, r.density_unit.as_str(), r.end_db), (6.0, "segments per λ", 0.0));
        assert!(r.lossless && r.metal == "PEC", "{} {}", r.lossless, r.metal);
        assert!(r.convergence.is_none());
        let page = antenna_core::report::html(&p, &r);
        assert!(page.contains("6 segments per λ") && page.contains("per wavelength") && page.contains("Power balance"));
        p.metal = "Copper".into();
        p.setup.segments_per_lambda = 24.0;
        let lossy = simulate(&p, &c, &RunOptions::default()).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(!lossy.lossless && lossy.metal == "Copper");
        assert!(lossy.warnings.is_empty(), "{:?}", lossy.warnings);
    }

    /// Wire MoM through the app path: fast and with a far field per frequency.
    #[test]
    fn antenna_mom_runs_a_yagi() {
        let mut p = project("yagi", "915MHz", 20.0);
        p.setup.mom_samples = 11;
        p.setup.far_freqs = vec![915e6, 950e6];
        let c = ctx();
        let r = simulate(&p, &c, &RunOptions::default()).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(r.solver.contains("MoM"));
        assert_eq!(r.far_f.len(), 2);
        let m = r.metrics(0, 0).unwrap();
        eprintln!("Yagi: {} unknowns, {:.1} s, peak {:.2} dBi at θ {}, F/B {:?}", r.size, r.seconds, m.peak_directivity_dbi, m.peak_theta, m.front_to_back_db);
        assert!(m.peak_directivity_dbi > 7.0, "{}", m.peak_directivity_dbi);
        assert!(m.peak_theta < 20.0, "beam at {}", m.peak_theta);
        assert!(r.currents.as_ref().is_some_and(|v| !v.current.is_empty()));
        assert!(r.network.f.len() == p.setup.points, "interpolated to the output sweep");
    }
}
