//! Antenna Studio: a workspace for designing one antenna at a time, with the
//! workflow of HFSS, CST and FEKO: parametric model, field solvers (openEMS FDTD
//! and method of moments), S-parameters, far fields, arrays, matching, sweeps,
//! optimisation and yield. The kernel is the `antenna_core` crate; this module
//! adds the background jobs, the UI and the assistant's tools.

pub mod assist;
pub mod gate;
pub mod reconfig;
pub mod run;
pub mod tools;
pub mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use antenna_core::checks;
use antenna_core::optimize::{self, Method};
use antenna_core::project::{Goal, Project, SimResult, SolverKind, total_penalty};
use anyhow::Result;
use bevy::prelude::*;

pub use run::Status;
use run::{JobCtx, Live, RunOptions};

/// One point of a parametric sweep.
#[derive(Clone)]
pub struct SweepPoint {
    pub values: Vec<(String, f64)>,
    pub result: Arc<SimResult>,
}

#[derive(Clone, Default)]
pub struct SweepResult {
    pub var: String,
    pub var2: Option<String>,
    pub points: Vec<SweepPoint>,
    pub total: usize,
}

#[derive(Clone, Default)]
pub struct OptState {
    pub names: Vec<String>,
    /// (evaluation, best penalty) so far.
    pub history: Vec<(usize, f64)>,
    pub best_x: Vec<f64>,
    pub best_value: f64,
    pub evals: usize,
    pub finished: bool,
    pub method: String,
    pub best: Option<Arc<SimResult>>,
}

/// One step of a mesh convergence study.
#[derive(Clone, Debug)]
pub struct ConvergeStep {
    pub density: f64,
    pub size: u64,
    pub seconds: f64,
    pub f_res: f64,
    pub peak_dbi: f64,
    /// ΔS against the previous step: the largest complex change of S11 over the band
    /// (the convergence figure HFSS reports). Over every S entry, not only S11.
    pub delta_s: f64,
    /// What that says is still left, for a first-order error.
    pub remaining: f64,
    /// The largest change of peak realized gain against the previous step, dB.
    pub gain_change_db: f64,
    pub result: Arc<SimResult>,
}

#[derive(Clone, Default)]
pub struct ConvergeState {
    pub steps: Vec<ConvergeStep>,
    pub converged: bool,
    pub limit: f64,
    pub finished: bool,
    /// Why the study stopped early, if it did.
    pub note: Option<String>,
}

#[derive(Clone, Default)]
pub struct YieldState {
    pub names: Vec<String>,
    pub samples: usize,
    pub done: usize,
    pub limit_db: f64,
    pub outcome: Option<YieldSummary>,
}

#[derive(Clone, Default)]
pub struct YieldSummary {
    pub fraction: f64,
    pub ci95: (f64, f64),
    pub mean: f64,
    pub std: f64,
    pub values: Vec<f64>,
    pub sensitivities: Vec<f64>,
    pub passed: usize,
    pub samples: usize,
    /// Samples that would not solve. Yield and its interval are of the rest.
    pub failed: usize,
    /// Why the first of them failed.
    pub first_failure: Option<String>,
    /// The interval's lower bound clears the target and every sample solved.
    pub meets_target: bool,
}

/// The yield a design has to show, judged on the lower bound of its interval.
pub const YIELD_TARGET: f64 = 0.9;

#[derive(Default)]
struct Shared {
    latest: Option<Arc<SimResult>>,
    /// Finished runs of this session, newest last.
    history: Vec<(Project, Arc<SimResult>)>,
    sweep: Option<SweepResult>,
    opt: Option<OptState>,
    converge: Option<ConvergeState>,
    yield_: Option<YieldState>,
    rcs: Option<RcsResult>,
    gate: Option<gate::GateRun>,
    gate_found: Option<Vec<antenna_core::project::DriveState>>,
    reconfig: Option<reconfig::ReconfigRun>,
    reconfig_found: Option<Vec<antenna_core::reconfig::Found>>,
}

/// Radar cross section against angle, from a plane-wave MoM solve.
#[derive(Clone, Default)]
pub struct RcsResult {
    pub f: f64,
    pub monostatic: bool,
    pub phi: f64,
    /// (θ°, RCS in dBsm).
    pub curve: Vec<(f64, f64)>,
    pub unknowns: usize,
    pub seconds: f64,
}

/// The Studio: the project being edited, its job, and the UI state.
#[derive(Resource)]
pub struct Studio {
    pub project: Project,
    live: Arc<Mutex<Live>>,
    cancel: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<(u64, std::process::Child)>>>,
    shared: Arc<Mutex<Shared>>,
    pub ui: ui::StudioUi,
}

impl Default for Studio {
    fn default() -> Self {
        Self {
            project: Project::from_template("patch_probe", None).expect("the built-in template is valid"),
            live: Arc::new(Mutex::new(Live { status: Some(Status::Idle), speed_mcs: 120.0, ..Default::default() })),
            cancel: Arc::new(AtomicBool::new(false)),
            children: Arc::new(Mutex::new(Vec::new())),
            shared: Arc::new(Mutex::new(Shared::default())),
            ui: ui::StudioUi::default(),
        }
    }
}

impl Drop for Studio {
    fn drop(&mut self) {
        for (_, c) in self.children.lock().unwrap().iter_mut() {
            let _ = c.kill();
        }
    }
}

fn ctx_of(s: &Studio) -> JobCtx {
    JobCtx { live: s.live.clone(), cancel: s.cancel.clone(), children: s.children.clone() }
}

/// Keeps a result small enough to hold many of them: no field planes.
fn slim(mut r: SimResult) -> SimResult {
    r.planes.clear();
    r
}

impl Studio {
    pub fn status(&self) -> Status {
        self.live.lock().unwrap().status.clone().unwrap_or(Status::Idle)
    }
    pub fn busy(&self) -> bool {
        matches!(self.status(), Status::Busy(..))
    }
    pub fn speed_mcs(&self) -> f64 {
        self.live.lock().unwrap().speed_mcs
    }
    pub fn log_tail(&self, n: usize) -> Vec<String> {
        let l = self.live.lock().unwrap();
        l.log.iter().rev().take(n).rev().cloned().collect()
    }
    pub fn latest(&self) -> Option<Arc<SimResult>> {
        self.shared.lock().unwrap().latest.clone()
    }
    pub fn history(&self) -> Vec<(Project, Arc<SimResult>)> {
        self.shared.lock().unwrap().history.clone()
    }
    /// Shows an earlier run again.
    pub fn show_result(&self, r: Arc<SimResult>) {
        self.shared.lock().unwrap().latest = Some(r);
    }
    pub fn sweep(&self) -> Option<SweepResult> {
        self.shared.lock().unwrap().sweep.clone()
    }
    pub fn opt(&self) -> Option<OptState> {
        self.shared.lock().unwrap().opt.clone()
    }
    pub fn converge(&self) -> Option<ConvergeState> {
        self.shared.lock().unwrap().converge.clone()
    }
    pub fn rcs(&self) -> Option<RcsResult> {
        self.shared.lock().unwrap().rcs.clone()
    }

    /// Radar cross section of the metal model, ports shorted. Monostatic sweeps the
    /// incidence over θ in the plane at `phi`; bistatic keeps the wave at
    /// `theta_inc` and sweeps the observer.
    pub fn run_rcs(&mut self, f: f64, phi: f64, pol_theta: bool, monostatic: bool, theta_inc: f64, step: f64) {
        let base = self.project.clone();
        self.spawn(move |ctx, shared| {
            let t0 = std::time::Instant::now();
            let geom = base.scatterer()?;
            let opts = antenna_core::mom::MomOptions { segments_per_lambda: base.setup.segments_per_lambda, edges_per_lambda: base.setup.edges_per_lambda, ..Default::default() };
            ctx.busy("Meshing…", -1.0);
            let problem = antenna_core::mom::MomProblem::new(&geom, f, opts)?;
            let top = if geom.ground { 90.0 } else { 180.0 };
            let angles = antenna_core::linspace(0.0, top, (top / step.max(0.25)).round() as usize + 1);
            let unit = antenna_core::C64::new(1.0, 0.0);
            let zero = antenna_core::C64::new(0.0, 0.0);
            let (pt, pp) = if pol_theta { (unit, zero) } else { (zero, unit) };
            let mut curve = Vec::with_capacity(angles.len());
            if monostatic {
                for (k, th) in angles.iter().enumerate() {
                    ctx.check_cancel()?;
                    ctx.busy(format!("RCS at θ = {th:.0}°"), k as f32 / angles.len() as f32);
                    let v = problem.rcs(f, *th, phi, pt, pp, &[(*th, phi)])?;
                    curve.push((*th, antenna_core::db10(v[0])));
                }
            } else {
                ctx.busy("Solving the plane wave…", -1.0);
                let observe: Vec<(f64, f64)> = angles.iter().map(|th| (*th, phi)).collect();
                let v = problem.rcs(f, theta_inc, phi, pt, pp, &observe)?;
                curve = angles.iter().zip(v).map(|(th, s)| (*th, antenna_core::db10(s))).collect();
            }
            shared.lock().unwrap().rcs = Some(RcsResult { f, monostatic, phi, curve, unknowns: problem.unknowns(), seconds: t0.elapsed().as_secs_f64() });
            Ok(())
        });
    }

    pub fn yield_state(&self) -> Option<YieldState> {
        self.shared.lock().unwrap().yield_.clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        for (_, c) in self.children.lock().unwrap().iter_mut() {
            let _ = c.kill();
        }
    }

    fn spawn(&mut self, job: impl FnOnce(&JobCtx, &Arc<Mutex<Shared>>) -> Result<()> + Send + 'static) {
        if self.busy() {
            return;
        }
        self.cancel.store(false, Ordering::SeqCst);
        let ctx = ctx_of(self);
        let shared = self.shared.clone();
        ctx.busy("Starting…", -1.0);
        std::thread::spawn(move || {
            let r = job(&ctx, &shared);
            let status = match r {
                Ok(()) => Status::Idle,
                Err(_) if ctx.cancelled() => Status::Failed("Cancelled".into()),
                Err(e) => Status::Failed(format!("{e:#}")),
            };
            ctx.status(status);
        });
    }

    /// One simulation of the current project.
    pub fn run(&mut self) {
        let project = self.project.clone();
        self.spawn(move |ctx, shared| {
            let r = Arc::new(run::simulate(&project, ctx, &RunOptions::default())?);
            let mut s = shared.lock().unwrap();
            s.latest = Some(r.clone());
            s.history.push((project, r));
            if s.history.len() > 30 {
                s.history.remove(0);
            }
            Ok(())
        });
    }

    /// Parametric sweep over one variable or a grid of two.
    pub fn run_sweep(&mut self, var: (String, Vec<f64>), var2: Option<(String, Vec<f64>)>, parallel: usize) {
        let base = self.project.clone();
        self.spawn(move |ctx, shared| {
            let mut cases: Vec<Vec<(String, f64)>> = Vec::new();
            for v in &var.1 {
                match &var2 {
                    Some((n2, vs2)) => {
                        for w in vs2 {
                            cases.push(vec![(var.0.clone(), *v), (n2.clone(), *w)]);
                        }
                    }
                    None => cases.push(vec![(var.0.clone(), *v)]),
                }
            }
            let total = cases.len();
            shared.lock().unwrap().sweep = Some(SweepResult { var: var.0.clone(), var2: var2.as_ref().map(|v| v.0.clone()), points: Vec::new(), total });
            let parallel = parallel.clamp(1, total.max(1));
            let next = std::sync::atomic::AtomicUsize::new(0);
            let error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
            std::thread::scope(|sc| {
                for _ in 0..parallel {
                    sc.spawn(|| {
                        loop {
                            let k = next.fetch_add(1, Ordering::SeqCst);
                            let Some(values) = cases.get(k) else { break };
                            if ctx.cancelled() || error.lock().unwrap().is_some() {
                                break;
                            }
                            let mut p = base.with_values(values);
                            p.setup.field_planes = false;
                            let opts = RunOptions { label: String::new(), keep: false, threads: 0, share: parallel, report: false };
                            match run::simulate(&p, ctx, &opts) {
                                Ok(r) => {
                                    let mut s = shared.lock().unwrap();
                                    if let Some(sw) = s.sweep.as_mut() {
                                        sw.points.push(SweepPoint { values: values.clone(), result: Arc::new(slim(r)) });
                                        let done = sw.points.len();
                                        ctx.busy(format!("Sweep: {done} of {total} done"), done as f32 / total as f32);
                                    }
                                }
                                Err(e) => {
                                    *error.lock().unwrap() = Some(e);
                                    break;
                                }
                            }
                        }
                    });
                }
            });
            if let Some(e) = error.into_inner().unwrap() {
                return Err(e);
            }
            if let Some(sw) = shared.lock().unwrap().sweep.as_mut() {
                // Finished out of order with parallel runs; order for the plots.
                sw.points.sort_by(|a, b| a.values.iter().map(|v| v.1).partial_cmp(b.values.iter().map(|v| v.1)).unwrap_or(std::cmp::Ordering::Equal));
            }
            Ok(())
        });
    }

    /// Optimises the named variables against the goals.
    pub fn run_optimize(&mut self, vars: Vec<(String, f64, f64)>, goals: Vec<(Goal, f64)>, method: Method, max_evals: usize, parallel: usize) {
        let base = self.project.clone();
        let names: Vec<String> = vars.iter().map(|v| v.0.clone()).collect();
        let bounds: Vec<(f64, f64)> = vars.iter().map(|v| (v.1, v.2)).collect();
        let start_vals = base.vars().unwrap_or_default();
        let x0: Vec<f64> = vars.iter().map(|v| start_vals.get(&v.0).copied().unwrap_or((v.1 + v.2) / 2.0).clamp(v.1, v.2)).collect();
        self.spawn(move |ctx, shared| {
            shared.lock().unwrap().opt = Some(OptState { names: names.clone(), best_value: f64::MAX, method: format!("{method:?}"), ..Default::default() });
            let parallel = parallel.max(1);
            let evals = std::sync::atomic::AtomicUsize::new(0);
            let first_error: Mutex<Option<String>> = Mutex::new(None);
            let objective = |x: &[f64]| -> f64 {
                if ctx.cancelled() {
                    return 1e6;
                }
                let values: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
                let mut p = base.with_values(&values);
                p.setup.field_planes = false;
                let opts = RunOptions { label: String::new(), keep: false, threads: 0, share: parallel, report: false };
                let n = evals.fetch_add(1, Ordering::SeqCst) + 1;
                match run::simulate(&p, ctx, &opts) {
                    Ok(r) => {
                        let value = total_penalty(&goals, &r);
                        let mut s = shared.lock().unwrap();
                        if let Some(o) = s.opt.as_mut() {
                            o.evals = n;
                            if value < o.best_value {
                                o.best_value = value;
                                o.best_x = x.to_vec();
                                o.best = Some(Arc::new(slim(r)));
                                o.history.push((n, value));
                            }
                        }
                        ctx.busy(format!("Optimising: {n} of {max_evals} runs, best penalty {value:.2}"), n as f32 / max_evals.max(1) as f32);
                        value
                    }
                    Err(e) => {
                        first_error.lock().unwrap().get_or_insert_with(|| format!("{e:#}"));
                        1e6
                    }
                }
            };
            let options = optimize::Options { method, max_evals, seed: 1, population: None, tol: 1e-3, parallel };
            let outcome = optimize::minimize(objective, &x0, &bounds, &options, |t| {
                let _ = t;
                !ctx.cancelled()
            });
            if outcome.value >= 1e6
                && let Some(e) = first_error.into_inner().unwrap()
            {
                anyhow::bail!(e);
            }
            if let Some(o) = shared.lock().unwrap().opt.as_mut() {
                o.finished = true;
                o.evals = outcome.evals;
                if o.best_x.is_empty() {
                    o.best_x = outcome.x;
                    o.best_value = outcome.value;
                }
            }
            Ok(())
        });
    }

    /// Refines the mesh from the project's own density until the results stop changing:
    /// the counterpart of the adaptive passes of a frequency-domain solver. Converged
    /// takes two passes running with the error left under `limit`, over every S entry,
    /// and with the peak gain settled.
    pub fn run_converge(&mut self, limit: f64, max_steps: usize) {
        let base = self.project.clone();
        self.spawn(move |ctx, shared| {
            shared.lock().unwrap().converge = Some(ConvergeState { limit, ..Default::default() });
            let geom = base.geometry()?;
            let solver = base.solver_for(&geom);
            // A wire pass is cheap, so it can double. On FDTD or a surface 1.5 already
            // costs 5 and 11 times the pass before, and the error left is weighed instead.
            let (start, ratio) = match solver {
                SolverKind::Fdtd => (base.setup.cells_per_lambda, 1.5),
                SolverKind::MomWire => (base.setup.segments_per_lambda, 2.0),
                _ => (base.setup.edges_per_lambda, 1.5),
            };
            let densities = checks::study_densities(start, ratio, max_steps.max(3));
            let mut previous: Option<Arc<SimResult>> = None;
            let mut changes = Vec::new();
            for (i, density) in densities.iter().copied().enumerate() {
                ctx.check_cancel()?;
                let mut p = base.clone();
                p.setup.field_planes = false;
                match solver {
                    SolverKind::Fdtd => p.setup.cells_per_lambda = density,
                    SolverKind::MomWire => p.setup.segments_per_lambda = density,
                    _ => p.setup.edges_per_lambda = density,
                }
                let label = format!("Pass {} of up to {}, density {density:.0}: ", i + 1, densities.len());
                let opts = RunOptions { label, keep: false, ..Default::default() };
                let mut r = match run::simulate(&p, ctx, &opts) {
                    Ok(r) => slim(r),
                    // A later pass can pass what the solver can model, such as a wire's
                    // radius. The passes so far stand, and they haven't converged.
                    Err(e) if i > 0 && !ctx.cancelled() => {
                        if let Some(st) = shared.lock().unwrap().converge.as_mut() {
                            st.note = Some(format!("Stopped before density {density:.0}: {e:#}"));
                            st.finished = true;
                        }
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                let (mut delta, mut remaining, mut gain_change) = (f64::INFINITY, f64::INFINITY, f64::INFINITY);
                if let Some(q) = &previous {
                    delta = checks::delta_s(&r.network, &q.network);
                    remaining = checks::remaining_error(delta, ratio);
                    gain_change = checks::gain_change_db(&r, q);
                    changes.push(checks::PassChange { delta_s: delta, gain_db: gain_change });
                }
                let converged = checks::study_converged(&changes, ratio, limit);
                if delta.is_finite() {
                    r.convergence = Some(antenna_core::project::MeshCheck { converged, passes: i + 1, delta_s: delta, remaining });
                }
                let r = Arc::new(r);
                let s_db = r.network.sij_db(0, 0);
                let fres = r.network.f.iter().zip(&s_db).min_by(|a, b| a.1.total_cmp(b.1)).map_or(0.0, |x| *x.0);
                let peak = r.metrics(0, 0).map_or(f64::NAN, |m| m.peak_realized_dbi);
                let step = ConvergeStep { density, size: r.size, seconds: r.seconds, f_res: fres, peak_dbi: peak, delta_s: delta, remaining, gain_change_db: gain_change, result: r.clone() };
                let mut s = shared.lock().unwrap();
                let st = s.converge.get_or_insert_with(Default::default);
                st.steps.push(step);
                if converged {
                    st.converged = true;
                    st.finished = true;
                    return Ok(());
                }
                drop(s);
                previous = Some(r);
            }
            if let Some(st) = shared.lock().unwrap().converge.as_mut() {
                st.finished = true;
            }
            Ok(())
        });
    }

    /// Monte-Carlo yield of "worst S11 in the band ≤ limit" under manufacturing tolerances
    /// on the named variables, as `(name, relative, absolute)`, both 1σ. Etching and
    /// machining errors are absolute: they don't scale with the dimension.
    pub fn run_yield(&mut self, vars: Vec<(String, f64, f64)>, n: usize, f_lo: f64, f_hi: f64, limit_db: f64, parallel: usize) {
        let base = self.project.clone();
        self.spawn(move |ctx, shared| {
            let names: Vec<String> = vars.iter().map(|v| v.0.clone()).collect();
            shared.lock().unwrap().yield_ = Some(YieldState { names: names.clone(), samples: n, done: 0, limit_db, outcome: None });
            let values = base.vars()?;
            let x0: Vec<f64> = names.iter().map(|k| values.get(k).copied().unwrap_or(0.0)).collect();
            let tol: Vec<optimize::Tolerance> = vars.iter().map(|v| optimize::Tolerance { rel: v.1, abs: v.2, gaussian: true }).collect();
            let parallel = parallel.clamp(1, n.max(1));
            let first_failure: Mutex<Option<String>> = Mutex::new(None);
            let f = |x: &[f64]| -> f64 {
                if ctx.cancelled() {
                    return f64::NAN;
                }
                let vals: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
                let mut p = base.with_values(&vals);
                p.setup.field_planes = false;
                p.setup.far_freqs.clear();
                let opts = RunOptions { label: String::new(), keep: false, threads: 0, share: parallel, report: false };
                // A sample that won't solve says nothing about the design: NaN, not a 0 dB failure.
                let v = match run::simulate(&p, ctx, &opts) {
                    Ok(r) => antenna_core::project::band_worst(&r.network, 0, 0, f_lo, f_hi).unwrap_or(f64::NAN),
                    Err(e) => {
                        first_failure.lock().unwrap().get_or_insert_with(|| format!("{e:#}"));
                        f64::NAN
                    }
                };
                let mut s = shared.lock().unwrap();
                if let Some(y) = s.yield_.as_mut() {
                    y.done += 1;
                    let done = y.done;
                    ctx.busy(format!("Yield: {done} of {n} samples"), done as f32 / n as f32);
                }
                v
            };
            let y = optimize::monte_carlo(f, &x0, &tol, n, 7, parallel, |v| v <= limit_db);
            ctx.check_cancel()?;
            if let Some(st) = shared.lock().unwrap().yield_.as_mut() {
                let meets_target = y.meets(YIELD_TARGET);
                let failure = first_failure.into_inner().unwrap();
                st.outcome = Some(YieldSummary { fraction: y.fraction, ci95: y.ci95, mean: y.mean, std: y.std, values: y.values, sensitivities: y.sensitivities, passed: y.passed, samples: y.samples, failed: y.failed, first_failure: failure, meets_target });
            }
            Ok(())
        });
    }
}

impl JobCtx {
    fn check_cancel(&self) -> Result<()> {
        if self.cancelled() {
            anyhow::bail!("cancelled");
        }
        Ok(())
    }
}


pub struct AntennaPlugin;

impl Plugin for AntennaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Studio>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use antenna_core::project::Goal;

    /// Waits for the job; a failure fails the test with its message.
    fn finish(s: &Studio) {
        let t0 = std::time::Instant::now();
        while s.busy() {
            assert!(t0.elapsed().as_secs() < 300, "job still running: {:?}", s.status());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if let Status::Failed(e) = s.status() {
            panic!("job failed: {e}\n{:?}", s.log_tail(10));
        }
    }

    fn dipole() -> Studio {
        let mut s = Studio::default();
        s.project = Project::from_template("dipole", Some("1GHz")).unwrap();
        s.project.setup.mom_samples = 9;
        s
    }

    #[test]
    fn a_run_lands_in_the_history() {
        let mut s = dipole();
        s.run();
        finish(&s);
        let r = s.latest().expect("a result");
        assert!(r.solver.contains("MoM"), "{}", r.solver);
        assert_eq!(s.history().len(), 1);
    }

    #[test]
    fn a_sweep_returns_every_value_in_order() {
        let mut s = dipole();
        s.run_sweep(("L".into(), vec![0.13, 0.14, 0.15, 0.16]), None, 2);
        finish(&s);
        let sw = s.sweep().unwrap();
        assert_eq!(sw.points.len(), 4);
        let ls: Vec<f64> = sw.points.iter().map(|p| p.values[0].1).collect();
        assert!(ls.windows(2).all(|w| w[0] < w[1]), "{ls:?}");
        // A longer dipole resonates lower.
        let res: Vec<f64> = sw.points.iter().map(|p| p.result.network.resonances(0)[0].0).collect();
        assert!(res.windows(2).all(|w| w[0] > w[1]), "{res:?}");
    }

    #[test]
    fn a_two_variable_sweep_is_a_grid() {
        let mut s = dipole();
        s.run_sweep(("L".into(), vec![0.13, 0.15]), Some(("a".into(), vec![0.0003, 0.0006, 0.001])), 3);
        finish(&s);
        assert_eq!(s.sweep().unwrap().points.len(), 6);
    }

    /// The whole loop: a detuned dipole, a goal, and the optimiser brings it back.
    #[test]
    fn the_optimizer_tunes_a_detuned_dipole() {
        let mut s = dipole();
        s.project.params.iter_mut().find(|p| p.name == "L").unwrap().expr = "0.19".into();
        s.project.setup.far_freqs.clear();
        let goals = vec![(Goal::MatchBelow { port: 0, f_lo: 0.98e9, f_hi: 1.02e9, db: -15.0 }, 1.0)];
        s.run_optimize(vec![("L".into(), 0.1, 0.2)], goals, Method::NelderMead, 30, 1);
        finish(&s);
        let o = s.opt().unwrap();
        assert!(o.finished);
        assert!(o.best_value < 1.0, "penalty {} after {} runs at L = {:?}", o.best_value, o.evals, o.best_x);
        assert!((0.13..0.15).contains(&o.best_x[0]), "L = {}", o.best_x[0]);
    }

    #[test]
    fn the_mesh_study_converges_on_a_wire() {
        let mut s = dipole();
        s.run_converge(0.03, 8);
        finish(&s);
        let c = s.converge().unwrap();
        assert!(c.finished);
        assert!(c.converged, "{:?}", c.steps.iter().map(|x| x.delta_s).collect::<Vec<_>>());
        assert!(c.steps.len() >= 2);
    }

    /// The audit's dipole read "converged at density 16" with 0.042 of error left, coarser
    /// than the project's own 24, and that was the result it then showed.
    #[test]
    fn the_mesh_study_starts_from_the_project_and_takes_two_passes() {
        let mut s = dipole();
        s.run_converge(0.02, 6);
        finish(&s);
        let c = s.converge().unwrap();
        assert!(c.finished);
        let densities: Vec<f64> = c.steps.iter().map(|x| x.density).collect();
        let deltas: Vec<f64> = c.steps.iter().map(|x| x.delta_s).collect();
        println!("densities {densities:?}, ΔS {deltas:?}, converged {}, note {:?}", c.converged, c.note);
        assert_eq!(c.steps[0].density, 24.0, "the project's own mesh comes first");
        assert!(c.steps.len() >= 3 || !c.converged, "two passes running take three runs: {densities:?}");
        if c.converged {
            let last = &c.steps[c.steps.len() - 1];
            assert!(last.remaining < 0.02 && c.steps[c.steps.len() - 2].remaining < 0.02, "{deltas:?}");
            assert!(last.result.convergence.is_some_and(|m| m.converged));
        }
        // The first pass has nothing to be compared with.
        assert!(!c.steps[0].delta_s.is_finite());
        assert!(c.steps[0].result.convergence.is_none());
    }

    /// Past the wire's limit the study ends with what it has, not an error.
    #[test]
    fn the_mesh_study_stops_at_the_thin_wire_limit() {
        let mut s = dipole();
        // Δ/a falls below 2 on the third pass.
        s.project.params.iter_mut().find(|p| p.name == "a").unwrap().expr = "0.005*lambda0".into();
        s.run_converge(0.0001, 8);
        finish(&s);
        let c = s.converge().unwrap();
        assert!(c.finished && !c.converged);
        assert!(c.note.as_ref().is_some_and(|n| n.contains("radius")), "{:?}", c.note);
        assert!(c.steps.len() >= 2, "{}", c.steps.len());
        assert!(c.steps.last().unwrap().result.convergence.is_some_and(|m| !m.converged));
    }

    /// A crashed sample used to count as a failed one, at 0 dB, in the mean and the spread.
    #[test]
    fn yield_counts_samples_that_would_not_solve() {
        let mut s = dipole();
        s.project.setup.far_freqs.clear();
        // A 1σ of 100 % takes the radius through zero about one time in six, which no solver accepts.
        s.run_yield(vec![("a".into(), 1.0, 0.0)], 40, 0.98e9, 1.02e9, -3.0, 4);
        finish(&s);
        let o = s.yield_state().unwrap().outcome.unwrap();
        println!("failed {}, passed {} of {}, fraction {}, mean {}", o.failed, o.passed, o.samples, o.fraction, o.mean);
        assert!(o.failed > 0 && o.failed < 40, "{}", o.failed);
        assert!(o.values.iter().filter(|v| v.is_nan()).count() == o.failed);
        assert!(o.first_failure.is_some());
        assert!((o.fraction - o.passed as f64 / (40 - o.failed) as f64).abs() < 1e-12);
        assert!(o.mean < -3.0, "failures left out of the mean: {}", o.mean);
        assert!(!o.meets_target, "with samples missing the verdict can't pass");
    }

    /// Absolute tolerances move a dimension by the same amount whatever its size.
    #[test]
    fn yield_takes_absolute_tolerances() {
        let mut s = dipole();
        s.project.setup.far_freqs.clear();
        let f = |s: &mut Studio, tol: (f64, f64)| {
            s.run_yield(vec![("L".into(), tol.0, tol.1)], 12, 0.98e9, 1.02e9, -10.0, 2);
            finish(s);
            s.yield_state().unwrap().outcome.unwrap()
        };
        let none = f(&mut s, (0.0, 0.0));
        let some = f(&mut s, (0.0, 0.002));
        assert!(none.std < 1e-9 && some.std > 0.01, "{} {}", none.std, some.std);
    }

    #[test]
    fn yield_falls_with_a_tighter_limit() {
        let run = |limit: f64| {
            let mut s = dipole();
            s.project.setup.far_freqs.clear();
            s.run_yield(vec![("L".into(), 0.03, 0.0)], 12, 0.98e9, 1.02e9, limit, 2);
            finish(&s);
            s.yield_state().unwrap().outcome.expect("an outcome")
        };
        let easy = run(-3.0);
        let hard = run(-30.0);
        assert!(easy.fraction > hard.fraction, "{} vs {}", easy.fraction, hard.fraction);
        assert_eq!(easy.samples, 12);
    }

    /// Against physical optics: a 2 λ plate head on, 4πA²/λ².
    #[test]
    fn the_rcs_job_matches_a_plate() {
        let mut s = Studio::default();
        let f = 1e9;
        let lambda = antenna_core::C0 / f;
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        let side = format!("{}", lambda);
        p.extra.push(antenna_core::project::ExtraSolid {
            name: "plate".into(),
            material: "PEC".into(),
            prim: antenna_core::project::ExtraPrim::Box { min: [format!("-{side}"), format!("-{side}"), "0".into()], max: [side.clone(), side.clone(), "0".into()] },
        });
        s.project = p;
        s.run_rcs(f, 0.0, true, true, 0.0, 30.0);
        let t0 = std::time::Instant::now();
        while s.busy() {
            assert!(t0.elapsed().as_secs() < 300);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match s.status() {
            Status::Failed(e) => panic!("{e}"),
            _ => {
                let r = s.rcs().unwrap();
                let area = (2.0 * lambda) * (2.0 * lambda);
                let want = antenna_core::db10(4.0 * std::f64::consts::PI * area * area / (lambda * lambda));
                let head_on = r.curve[0].1;
                assert!((head_on - want).abs() < 1.5, "{head_on} vs {want} dBsm");
            }
        }
    }
}
