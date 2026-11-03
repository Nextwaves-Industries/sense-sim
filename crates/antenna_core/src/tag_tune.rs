//! Tuning a tag by the two things a designer sets by hand: the loop, whose length and width match the
//! antenna to the chip's impedance, and the long line outside it, whose length puts the tag on its
//! frequency. This reports those two as the tag's metrics, moves them until the antenna is the
//! conjugate of the chip at the frequency asked for, and says what a millimetre of each is worth.
//!
//! A conjugate match at a frequency also makes that frequency the peak of the power into the chip,
//! since the transfer can't exceed one, so the two aims come to one complex equation at `f0`. The loop
//! has two sizes and the line one, which is enough unknowns for it wherever the footprint allows.
//!
//! Everything solved here is a model: results are Simulated, never Measured.

use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::mom::{MomOptions, MomProblem};
use crate::project::Project;
use crate::tag::{chip_at, power_transfer, read_range_m};
use crate::tag_design::{Control, Mesh, Progress, REPORT_MESH, SEARCH_MESH, TagGoal, TagScore, evaluate, fitted};
use crate::{C0, C64};

/// The variables that make up the loop and the line.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Knobs {
    /// Its length and width, and for a coupled loop how far it sits from the radiator.
    pub loop_vars: Vec<String>,
    /// The one that lengthens the line.
    pub arm_var: String,
}

impl Knobs {
    fn all(&self) -> Vec<String> {
        let mut v = self.loop_vars.clone();
        v.push(self.arm_var.clone());
        v
    }
}

/// Which variables they are in a structure this knows. Two radiators have two frequencies, so a
/// `tag_dual` has no one line to set, and a drawing has whatever its designer named.
pub fn knobs_for(p: &Project) -> Option<Knobs> {
    let meandered = p.vars().ok()?.get("n").is_some_and(|n| n.round() >= 1.0);
    let line = if meandered { "Hm" } else { "Lx" };
    let (looped, arm): (&[&str], &str) = match p.template.as_str() {
        "rfid_tag" => (&["a", "b"], line),
        "tag_loop" => (&["a", "b", "gc"], line),
        "tag_dogbone" | "tag_slot" => (&["a", "b"], "Lx"),
        "tag_bowtie" => (&["xs", "hs"], "Lx"),
        "tag_spiral" => (&["xs", "hs"], "pitch"),
        "tag_fractal" => (&["a", "b"], "Ax"),
        "tag_bent" => (&["a", "b"], "Xa"),
        _ => return None,
    };
    Some(Knobs { loop_vars: looped.iter().map(|s| (*s).to_string()).collect(), arm_var: arm.to_string() })
}

/// One arm's length along its metal, from the centre to the tip, where the structure makes that a
/// plain sum. A meander's sideways runs add `4·Hm` a period and its steps add nothing, since they're
/// already part of how far along the axis it goes.
pub fn arm_wire(p: &Project) -> Option<f64> {
    let v = p.vars().ok()?;
    let g = |k: &str| v.get(k).copied();
    let meander = || Some(4.0 * g("n")?.round().max(0.0) * g("Hm")?);
    match p.template.as_str() {
        "rfid_tag" => Some(g("Lx")? / 2.0 + meander()?),
        "tag_loop" => Some(g("Lx")? / 2.0 - g("Lp")?.max(0.0) + meander()?),
        "tag_dogbone" | "tag_bowtie" | "tag_slot" => Some(g("Lx")? / 2.0),
        "tag_bent" => Some(g("Xa")? + g("Ya")?),
        _ => None,
    }
}

/// Both metrics for one design at one frequency.
#[derive(Clone, Debug, Serialize)]
pub struct Reading {
    pub f0: f64,
    /// The antenna, in series form, at `f0`.
    pub z: (f64, f64),
    /// What it would have to be: the chip's conjugate at `f0`.
    pub needed: (f64, f64),
    /// Share of the collected power that reaches the chip at `f0`.
    pub transfer: f64,
    /// Where that share is highest, within a fifth either side of `f0`. `None` if it's still climbing
    /// at the edge, so the peak is further away than that.
    pub peak_hz: Option<f64>,
    pub peak_transfer: f64,
    /// Forward-link read range at `f0`, aligned, metres.
    pub range_m: f64,
    pub gain_dbi: f64,
    pub efficiency: f64,
    pub size_x: f64,
    pub size_y: f64,
    /// The loop's variables as they stand, metres.
    pub loop_dims: Vec<(String, f64)>,
    /// The line's variable as it stands, metres.
    pub arm_var: (String, f64),
    /// One arm along its metal, metres, where the structure gives a plain figure.
    pub arm_wire: Option<f64>,
    /// A quarter wavelength in air at `f0`, the usual place to start the line, metres.
    pub quarter_wave: f64,
    pub unknowns: usize,
}

impl Reading {
    /// Whether the antenna is close enough to the chip's conjugate at the target.
    pub fn matched(&self) -> bool {
        self.transfer >= MATCHED
    }

    /// Whether the power into the chip peaks at the target.
    pub fn on_frequency(&self) -> bool {
        self.peak_hz.is_some_and(|f| (f - self.f0).abs() / self.f0 <= ON_FREQUENCY)
    }
}

/// A count rather than a size, so never shown in millimetres.
pub fn is_count(name: &str) -> bool {
    matches!(name, "n" | "turns" | "order")
}

/// What a millimetre of one variable is worth, at the tuned design.
#[derive(Clone, Debug, Serialize)]
pub struct Worth {
    pub var: String,
    /// Part of the line, rather than of the loop.
    pub is_line: bool,
    pub r_per_mm: f64,
    pub x_per_mm: f64,
    /// How far the peak moves, from the slopes of reactance in size and in frequency. `None` where
    /// reactance doesn't climb with frequency here, so there's no such figure.
    pub mhz_per_mm: Option<f64>,
}

/// What a tune did.
#[derive(Clone, Debug, Serialize)]
pub struct Tune {
    pub f0: f64,
    pub knobs: Knobs,
    pub before: Reading,
    pub after: Reading,
    pub project: Project,
    /// `(variable, from, to)`, every variable that moved, metres or a plain count.
    pub changes: Vec<(String, f64, f64)>,
    pub worth: Vec<Worth>,
    /// Over the whole band, on the reporting mesh, by the same measure the design search uses.
    pub score: TagScore,
    pub notes: Vec<String>,
    pub solves: usize,
}

/// Far enough either side of `f0` to find a peak that's a long way off. A tag's Q is about 15.
const SPAN: f64 = 0.2;
/// Above this share of the power there's nothing left worth a solve: 0.1 dB.
const GOOD_ENOUGH: f64 = 0.977;
/// How much of the power counts as matched in what's said to a designer: 0.5 dB.
const MATCHED: f64 = 0.89;
/// How far from its target the peak may be and still count as on frequency.
const ON_FREQUENCY: f64 = 0.01;

/// Meshed for the top of the sweep, at the density `mesh` means at `f0`, so a sweep and a single
/// frequency see the same model.
fn problem(p: &Project, f0: f64, mesh: Mesh, far_step: Option<f64>) -> Result<MomProblem> {
    let g = p.geometry()?;
    let step = far_step.unwrap_or(45.0);
    MomProblem::new(&g, f0 * (1.0 + SPAN), MomOptions { edges_per_lambda: mesh.edges_per_lambda / (1.0 + SPAN), theta_step: step, phi_step: step, ..Default::default() })
}

fn z_of(p: &Project, f0: f64, mesh: Mesh) -> Result<C64> {
    Ok(problem(p, f0, mesh, None)?.solve(f0, false)?.z[0][0])
}

/// Strictly: the assessment holds a design to exactly what was asked, so a tag 16 µm over is over.
fn fits(p: &Project, goal: &TagGoal) -> Result<bool> {
    let (lo, hi) = p.geometry()?.bounds();
    Ok(goal.max_x.is_none_or(|m| hi[0] - lo[0] <= m) && goal.max_y.is_none_or(|m| hi[1] - lo[1] <= m))
}

/// Runs `job` over `items`, at most `parallel` at a time, and keeps their order.
fn each<T: Sync, R: Send>(items: &[T], parallel: usize, job: &(dyn Fn(&T) -> R + Sync)) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> = items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..parallel.clamp(1, items.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    *slots[i].lock().unwrap() = Some(job(item));
                }
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().expect("every slot was filled")).collect()
}

/// The top of a parabola through three evenly spaced points, as a shift from the middle one in steps.
fn vertex(a: f64, b: f64, c: f64) -> f64 {
    let curve = a - 2.0 * b + c;
    if curve >= 0.0 { 0.0 } else { (0.5 * (a - c) / curve).clamp(-1.0, 1.0) }
}

/// Reads both metrics. Impedance at `f0` comes from the reporting mesh, and the same difference is
/// applied across the sweep, which uses the search's mesh to stay quick.
pub fn read(p: &Project, goal: &TagGoal, f0: f64, knobs: &Knobs, ctl: &Control) -> Result<Reading> {
    let zc = chip_at(goal.chip_z, f0);
    let m = problem(p, f0, SEARCH_MESH, Some(SEARCH_MESH.far_step))?;
    let centre = m.solve(f0, true)?;
    let metrics = centre.far[0].metrics();
    let shift = z_of(p, f0, REPORT_MESH)? - centre.z[0][0];
    let z = centre.z[0][0] + shift;
    let tau = |f: f64| -> Result<f64> { Ok(power_transfer(m.solve(f, false)?.z[0][0] + shift, chip_at(goal.chip_z, f))) };

    // A step of a fiftieth of the span keeps a parabola through the top three good to under a megahertz.
    let sweep: Vec<f64> = crate::linspace(f0 * (1.0 - SPAN), f0 * (1.0 + SPAN), 41);
    let taus: Vec<f64> = each(&sweep, ctl.parallel, &|f: &f64| tau(*f)).into_iter().collect::<Result<_>>()?;
    let top = taus.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |x| x.0);
    let peak_hz = (top > 0 && top + 1 < sweep.len()).then(|| sweep[top] + (sweep[1] - sweep[0]) * vertex(taus[top - 1], taus[top], taus[top + 1]));
    let peak_transfer = taus[top];

    let values = p.vars()?;
    let get = |n: &String| (n.clone(), values.get(n).copied().unwrap_or(f64::NAN));
    let (lo, hi) = p.geometry()?.bounds();
    let transfer = power_transfer(z, zc);
    Ok(Reading {
        f0,
        z: (z.re, z.im),
        needed: (zc.re, -zc.im),
        transfer,
        peak_hz,
        peak_transfer,
        range_m: read_range_m(f0, goal.eirp_dbm, metrics.peak_gain_dbi, transfer, goal.read_dbm, 1.0),
        gain_dbi: metrics.peak_gain_dbi,
        efficiency: metrics.rad_efficiency,
        size_x: hi[0] - lo[0],
        size_y: hi[1] - lo[1],
        loop_dims: knobs.loop_vars.iter().map(get).collect(),
        arm_var: get(&knobs.arm_var),
        arm_wire: arm_wire(p),
        quarter_wave: C0 / f0 / 4.0,
        unknowns: m.unknowns(),
    })
}

/// How far an impedance is from the chip's conjugate. Resistance counts by ratio, as it ranges over
/// orders of magnitude, and reactance against the resistance of the whole circuit, which is what it
/// costs: either term at one is about 1 dB of power into the chip.
fn residual(z: C64, zc: C64) -> [f64; 2] {
    [(z.re.max(1e-3) / zc.re).ln(), (z.im + zc.im) / (2.0 * zc.re)]
}

fn size(r: &[f64; 2]) -> f64 {
    r[0] * r[0] + r[1] * r[1]
}

/// Solves `(JᵀJ + μI) d = -Jᵀr` by elimination: the damped least-squares step, which with more
/// unknowns than equations stays close to where it started.
fn damped_step(jac: &[[f64; 2]], r: &[f64; 2], mu: f64) -> Vec<f64> {
    let n = jac.len();
    let mut a = vec![vec![0.0; n + 1]; n];
    for i in 0..n {
        for j in 0..n {
            a[i][j] = jac[i][0] * jac[j][0] + jac[i][1] * jac[j][1] + if i == j { mu } else { 0.0 };
        }
        a[i][n] = -(jac[i][0] * r[0] + jac[i][1] * r[1]);
    }
    for c in 0..n {
        let pivot = (c..n).max_by(|x, y| a[*x][c].abs().total_cmp(&a[*y][c].abs())).unwrap_or(c);
        a.swap(c, pivot);
        if a[c][c].abs() < 1e-300 {
            continue;
        }
        for row in c + 1..n {
            let k = a[row][c] / a[c][c];
            for col in c..=n {
                a[row][col] -= k * a[c][col];
            }
        }
    }
    let mut d = vec![0.0; n];
    for c in (0..n).rev() {
        let done: f64 = (c + 1..n).map(|k| a[c][k] * d[k]).sum();
        d[c] = if a[c][c].abs() < 1e-300 { 0.0 } else { (a[c][n] - done) / a[c][c] };
    }
    d
}

/// One step of the sizes, as fractions of each. The line's resonance is narrow: a millimetre or so of a
/// meander's height takes the resistance from the chip's to five times that, so any more and the
/// slopes the step is based on stop applying.
const MOST_STEP: f64 = 0.1;
/// A probe has to move a variable further than a mesh's noise and less than the resonance's width.
const PROBE: f64 = 0.015;
const SMALLEST_PROBE: f64 = 20e-6;
/// How many solves a tune may spend. Tags solve in tens of milliseconds, so this is well under a minute.
const MOST_SOLVES: usize = 1500;
/// And a limit on time, as a solid plate has many more unknowns than a trace.
const MOST_SECONDS: u64 = 90;
/// A look along the whole of the line's range. Fine enough not to step over its resonance.
const SCAN_POINTS: usize = 48;

/// A model of how sizes give an impedance: real solves in use, a formula in the tests. `None` for sizes
/// that don't build or don't fit.
type Solver<'a> = dyn Fn(&[f64]) -> Option<C64> + Sync + 'a;

struct Search<'a> {
    solver: &'a Solver<'a>,
    zc: C64,
    /// Sizes may not leave this box.
    bounds: Vec<(f64, f64)>,
    parallel: usize,
    max_rounds: usize,
    keep_going: &'a (dyn Fn() -> bool + Sync),
}

/// The slope of the impedance against each size, per fractional change of it. A size that can't move
/// either way, as it's at the edge of what builds, gets no slope and the others do the work.
fn slopes(s: &Search, x: &[f64], z: C64) -> Vec<C64> {
    let probes: Vec<usize> = (0..x.len()).collect();
    each(&probes, s.parallel, &|k: &usize| {
        let h = (x[*k] * PROBE).max(SMALLEST_PROBE);
        for sign in [1.0, -1.0] {
            let mut y = x.to_vec();
            y[*k] += sign * h;
            if y[*k] < s.bounds[*k].0 || y[*k] > s.bounds[*k].1 {
                continue;
            }
            if let Some(w) = (s.solver)(&y) {
                return (w - z) / (sign * h / x[*k]);
            }
        }
        C64::new(0.0, 0.0)
    })
}

/// In terms of the residual: `dr/dz` is `1/R` for resistance and a constant for reactance.
fn residual_slopes(raw: &[C64], z: C64, zc: C64) -> Vec<[f64; 2]> {
    raw.iter().map(|d| [d.re / z.re.max(1e-3), d.im / (2.0 * zc.re)]).collect()
}

/// Damped Gauss-Newton on the sizes. Returns the best it reached and its impedance. It needs starting
/// near the resonance, where the slopes point at the answer: see [`Job::match_loop`].
fn descend(s: &Search, start: &[f64]) -> Option<(Vec<f64>, C64)> {
    let mut x = start.to_vec();
    let mut z = (s.solver)(&x)?;
    let mut mu = 1e-3;
    for _ in 0..s.max_rounds {
        if !(s.keep_going)() || power_transfer(z, s.zc) >= GOOD_ENOUGH {
            break;
        }
        let r = residual(z, s.zc);
        let jac = residual_slopes(&slopes(s, &x, z), z, s.zc);
        let mut moved = false;
        for _ in 0..6 {
            let mut d = damped_step(&jac, &r, mu);
            let longest = d.iter().fold(0.0f64, |m, v| m.max(v.abs()));
            if longest > MOST_STEP {
                d.iter_mut().for_each(|v| *v *= MOST_STEP / longest);
            }
            let y: Vec<f64> = x.iter().zip(&d).zip(&s.bounds).map(|((v, dv), b)| (v * (1.0 + dv)).clamp(b.0, b.1)).collect();
            if let Some(w) = (s.solver)(&y)
                && size(&residual(w, s.zc)) < size(&r)
            {
                x = y;
                z = w;
                mu = (mu * 0.3).max(1e-6);
                moved = true;
                break;
            }
            mu *= 8.0;
        }
        if !moved {
            break;
        }
    }
    Some((x, z))
}

fn named(names: &[String], x: &[f64]) -> Vec<(String, f64)> {
    names.iter().cloned().zip(x.iter().copied()).collect()
}

/// Sizes applied to a design, with what has to follow them: a meander or a flare starts outside the
/// loop, so it moves out as the loop widens.
fn with_sizes(p: &Project, sizes: &[(String, f64)]) -> Project {
    let q = p.with_values(sizes);
    let Ok(v) = q.vars() else { return q };
    let need = match q.template.as_str() {
        "rfid_tag" | "tag_loop" | "tag_fractal" => v.get("a").zip(v.get("w")).map(|(a, w)| a / 2.0 + 1.5 * w),
        "tag_bowtie" => v.get("xs").copied(),
        _ => None,
    };
    match (need, v.get("x0")) {
        (Some(n), Some(x0)) if *x0 < n => q.with_values(&[("x0".to_string(), n)]),
        _ => q,
    }
}

/// A design that doesn't fit the footprint can't be tuned where it stands. This brings it in. A meander
/// takes what the overall length loses in more periods, if it has to; anything else scales by the
/// variables that set its width and height. The tune then finds its resonance again from there.
fn into_footprint(p: &Project, goal: &TagGoal) -> Result<Project> {
    if fits(p, goal)? {
        return Ok(p.clone());
    }
    let mut q = p.clone();
    let v = p.vars()?;
    let get = |k: &str| v.get(k).copied();
    if let (true, Some(lx), Some(hm), Some(s), Some(x0), Some(w)) = (matches!(p.template.as_str(), "rfid_tag" | "tag_loop"), get("Lx"), get("Hm"), get("s"), get("x0"), get("w")) {
        let lx_new = goal.max_x.map_or(lx, |m| lx.min(m * 0.995));
        let wire = lx / 2.0 + 4.0 * get("n").unwrap_or(0.0).round().max(0.0) * hm;
        let room_y = goal.max_y.map_or(hm, |m| (m - 2.0 * w) / 2.0);
        q = p.with_values(&[("Lx".to_string(), lx_new)]);
        for n in 1..=12usize {
            let hm_new = ((wire - lx_new / 2.0) / (4.0 * n as f64)).min(room_y);
            let s_new = s.min((lx_new / 2.0 - x0 - 2.0 * w) / (2.0 * n as f64));
            if !(hm_new > w && s_new > 2.0 * w) {
                continue;
            }
            let c = p.with_values(&[("Lx".to_string(), lx_new), ("n".to_string(), n as f64), ("Hm".to_string(), hm_new), ("s".to_string(), s_new)]);
            if c.geometry().is_ok() && fits(&c, goal)? {
                q = c;
                if lx_new / 2.0 + 4.0 * n as f64 * hm_new >= wire * 0.98 {
                    break;
                }
            }
        }
    }
    let wide = match p.template.as_str() {
        "tag_bent" => Some("Xa"),
        "tag_fractal" => Some("Ax"),
        "tag_spiral" => Some("pitch"),
        _ => get("Lx").map(|_| "Lx"),
    };
    let tall: &[&str] = match p.template.as_str() {
        "tag_dogbone" => &["Hp", "b"],
        "tag_bowtie" => &["Hb", "hs"],
        "tag_slot" => &["Ly", "b"],
        "tag_bent" => &["Ya", "b"],
        "tag_fractal" => &["Ay", "b"],
        "tag_spiral" => &["pitch"],
        _ => &["b"],
    };
    for _ in 0..5 {
        if q.geometry().is_ok() && fits(&q, goal)? {
            return Ok(q);
        }
        let Ok(g) = q.geometry() else { break };
        let (lo, hi) = g.bounds();
        let (kx, ky) = (goal.max_x.map_or(1.0, |m| (m * 0.99 / (hi[0] - lo[0])).min(1.0)), goal.max_y.map_or(1.0, |m| (m * 0.98 / (hi[1] - lo[1])).min(1.0)));
        let now = q.vars()?;
        let mut sizes: Vec<(String, f64)> = Vec::new();
        if let (true, Some(n)) = (kx < 1.0, wide) {
            sizes.extend(now.get(n).map(|x| (n.to_string(), x * kx)));
        }
        if ky < 1.0 {
            sizes.extend(tall.iter().filter_map(|n| now.get(*n).map(|x| ((*n).to_string(), x * ky))));
        }
        if sizes.is_empty() {
            break;
        }
        q = q.with_values(&sizes);
    }
    if q.geometry().is_err() || !fits(&q, goal)? {
        bail!("It doesn't fit the footprint even brought in. Allow more room, or run the design search.");
    }
    Ok(q)
}

/// A way of making the line longer: the design set up for it, and how far its variable can go.
#[derive(Clone, Debug)]
struct Line {
    project: Project,
    arm: String,
    lo: f64,
    hi: f64,
}

/// The ways to try, best first. A meander can be taller or have more periods, and height is what's
/// continuous, so each number of periods is one line: the design's own first, then more of them, which
/// is what a low footprint needs. Anything else has the one variable it was given.
fn lines_for(base: &Project, goal: &TagGoal, knobs: &Knobs, f0: f64, structure_own: bool) -> Result<Vec<Line>> {
    let v = base.vars()?;
    let get = |k: &str| v.get(k).copied();
    let quarter = C0 / f0 / 4.0;
    let single = |arm: &str| -> Result<Line> {
        let Some(now) = get(arm).filter(|x| *x > 0.0) else { bail!("'{arm}' has to be a positive size in this design to be tuned.") };
        let hi = if arm == "Lx" { goal.max_x.map_or(now * 3.0, |m| m * 0.995).max(now) } else { now * 3.0 };
        Ok(Line { project: base.clone(), arm: arm.to_string(), lo: (now * 0.3).max(goal.min_feature), hi })
    };
    let meander = structure_own && matches!(base.template.as_str(), "rfid_tag" | "tag_loop");
    if !meander {
        return Ok(vec![single(&knobs.arm_var)?]);
    }
    let (Some(lx), Some(w), Some(s), Some(x0)) = (get("Lx"), get("w"), get("s"), get("x0")) else { bail!("This meander is missing a size.") };
    let tip = lx / 2.0 - get("Lp").unwrap_or(0.0).max(0.0);
    let top = goal.max_y.map_or(quarter / 2.0, |m| (m - 2.0 * w) / 2.0);
    let own = get("n").unwrap_or(0.0).round().max(0.0) as usize;
    let mut out = Vec::new();
    if own == 0 {
        out.push(single("Lx")?);
    }
    let first = own.max(1);
    for n in (first..=12).chain((1..first).rev()) {
        let step = s.min((tip - x0 - 2.0 * w) / (2.0 * n as f64));
        if step <= 2.0 * w || top <= 1.2 * w {
            continue;
        }
        let project = base.with_values(&[("n".to_string(), n as f64), ("s".to_string(), step)]);
        out.push(Line { project, arm: "Hm".to_string(), lo: 1.2 * w, hi: top });
    }
    if out.is_empty() {
        bail!("There's no room for a meander in this length: bring the loop in or allow more room.");
    }
    Ok(out)
}

/// Which side of the line's resonance to sit on. Short is the smaller, and it needs less of the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Short,
    Long,
}

fn strongest(samples: &[(f64, C64)]) -> Option<usize> {
    samples.iter().enumerate().max_by(|a, b| a.1.1.re.total_cmp(&b.1.1.re)).map(|x| x.0)
}

/// Whether the resistance really turns over within these samples, rather than still climbing at an end.
fn has_resonance(samples: &[(f64, C64)]) -> bool {
    strongest(samples).is_some_and(|k| k > 0 && k + 1 < samples.len())
}

/// Two neighbouring lengths the resistance `r` lies between, on one side of the resonance.
fn flank(samples: &[(f64, C64)], r: f64, side: Side) -> Option<(f64, f64)> {
    let peak = strongest(samples)?;
    let pairs: Vec<&[(f64, C64)]> = match side {
        Side::Short => samples[..=peak].windows(2).collect(),
        Side::Long => samples[peak..].windows(2).collect(),
    };
    pairs.into_iter().find(|w| (w[0].1.re < r) != (w[1].1.re < r)).map(|w| (w[0].0, w[1].0))
}

/// What a tune is working with: a design, the names of its sizes, loop first and the line last.
struct Job<'a> {
    base: &'a Project,
    names: Vec<String>,
    goal: &'a TagGoal,
    f0: f64,
    zc: C64,
    solves: &'a AtomicUsize,
    keep_going: &'a (dyn Fn() -> bool + Sync),
    parallel: usize,
}

impl Job<'_> {
    fn project(&self, x: &[f64]) -> Project {
        with_sizes(self.base, &named(&self.names, x))
    }

    fn z(&self, x: &[f64], shift: C64) -> Option<C64> {
        if !(self.keep_going)() || self.solves.load(Ordering::Relaxed) > MOST_SOLVES {
            return None;
        }
        let q = self.project(x);
        if !fits(&q, self.goal).ok()? {
            return None;
        }
        self.solves.fetch_add(1, Ordering::Relaxed);
        z_of(&q, self.f0, SEARCH_MESH).ok().map(|z| z + shift)
    }

    /// The impedance along the line's length, with the loop held. What doesn't build is left out.
    fn along(&self, loop_x: &[f64], lo: f64, hi: f64, points: usize, shift: C64) -> Vec<(f64, C64)> {
        let lengths = crate::linspace(lo, hi, points);
        let solved = each(&lengths, self.parallel, &|arm: &f64| {
            let mut x = loop_x.to_vec();
            x.push(*arm);
            self.z(&x, shift).map(|z| (*arm, z))
        });
        solved.into_iter().flatten().collect()
    }

    /// Sets the line for this loop: where the resistance equals the chip's on the side asked for, or at
    /// its strongest if it never gets that high. `near` is where to look first.
    fn set_line(&self, loop_x: &[f64], line: &Line, side: Side, near: Option<f64>, shift: C64) -> Option<(f64, C64)> {
        let at = |arm: f64| {
            let mut x = loop_x.to_vec();
            x.push(arm);
            self.z(&x, shift)
        };
        let rc = self.zc.re;
        let refine = |bracket: (f64, f64)| -> Option<(f64, C64)> {
            let (mut a, mut b) = bracket;
            let mut last = None;
            // Whichever side of the chip's resistance the first end is on, the crossing stays between them.
            let a_below = at(a).is_some_and(|z| z.re < rc);
            for _ in 0..9 {
                let m = (a + b) / 2.0;
                let z = at(m)?;
                if (z.re < rc) == a_below {
                    a = m;
                } else {
                    b = m;
                }
                last = Some((m, z));
            }
            last
        };
        if let Some(g) = near {
            let samples = self.along(loop_x, (g * 0.85).max(line.lo), (g * 1.15).min(line.hi), 12, shift);
            if let Some(found) = flank(&samples, rc, side).and_then(refine) {
                return Some(found);
            }
        }
        let samples = self.along(loop_x, line.lo, line.hi, SCAN_POINTS, shift);
        if let Some(found) = flank(&samples, rc, side).and_then(refine) {
            return Some(found);
        }
        strongest(&samples).map(|k| samples[k])
    }

    /// Scales the loop until its reactance is what the chip needs, with the line reset for each size.
    /// `dir` says which of the loop's sizes scale. Reactance climbs steadily with the loop, so this is
    /// a root to bracket, and what doesn't build or fit marks how far it can go.
    fn match_loop(&self, line: &Line, start: &[f64], dir: &[f64], side: Side, shift: C64) -> Option<Vec<f64>> {
        let needed = -self.zc.im;
        let scaled = |s: f64| -> Vec<f64> { start.iter().zip(dir).map(|(v, d)| if *d > 0.0 { v * s } else { *v }).collect() };
        let (mut below, mut above): (Option<(f64, f64)>, Option<(f64, f64)>) = (None, None);
        let (mut floor, mut ceil) = (0.2f64, 5.0f64);
        let mut good: Option<f64> = None;
        let mut near = None;
        let mut best: Option<(f64, Vec<f64>)> = None;
        let mut s = 1.0f64;
        for _ in 0..12 {
            let loop_x = scaled(s);
            let Some((arm, z)) = self.set_line(&loop_x, line, side, near, shift) else {
                if s >= good.unwrap_or(1.0) {
                    ceil = ceil.min(s);
                } else {
                    floor = floor.max(s);
                }
                let back = good.unwrap_or(if s >= 1.0 { floor } else { ceil });
                let next = if good.is_some() { (s + back) / 2.0 } else { s * if s >= 1.0 { 0.75 } else { 1.3 } };
                if (next - s).abs() < 1e-3 * s {
                    break;
                }
                s = next;
                continue;
            };
            near = Some(arm);
            good = Some(s);
            let score = size(&residual(z, self.zc));
            let mut x = loop_x;
            x.push(arm);
            if best.as_ref().is_none_or(|b| score < b.0) {
                best = Some((score, x));
            }
            let e = z.im - needed;
            if e.abs() <= 0.25 * self.zc.re {
                break;
            }
            if e < 0.0 {
                below = Some((s, e));
            } else {
                above = Some((s, e));
            }
            let next = match (below, above) {
                (Some((a, ea)), Some((b, eb))) => {
                    let (lo, hi) = (a.min(b), a.max(b));
                    let margin = (hi - lo) * 0.1;
                    (a + (b - a) * (-ea) / (eb - ea)).clamp(lo + margin, hi - margin)
                }
                _ => s * (needed / z.im.max(1.0)).clamp(0.5, 2.0),
            }
            .clamp(floor, ceil);
            if (next - s).abs() < 1e-3 * s {
                break;
            }
            s = next;
        }
        best.map(|b| b.1)
    }

    /// Closes the match from near it, and holds it to the finer mesh: a difference of 2 % in a reactance
    /// of 230 Ω is a large part of the match, so what that mesh adds is fed back to the search.
    fn polish(&self, from: &[f64], bounds: &[(f64, f64)], ctl: &Control) -> Option<(Vec<f64>, f64)> {
        let mut shift = C64::new(0.0, 0.0);
        let mut x = from.to_vec();
        let mut best: Option<(Vec<f64>, f64)> = None;
        for _ in 0..3 {
            let bias = shift;
            let solver = |y: &[f64]| self.z(y, bias);
            let search = Search { solver: &solver, zc: self.zc, bounds: bounds.to_vec(), parallel: ctl.parallel, max_rounds: 14, keep_going: self.keep_going };
            let Some((found, _)) = descend(&search, &x) else { break };
            x = found;
            let q = self.project(&x);
            let fine = z_of(&q, self.f0, REPORT_MESH).ok()?;
            let t = power_transfer(fine, self.zc);
            if best.as_ref().is_none_or(|b| t > b.1) {
                best = Some((x.clone(), t));
            }
            if t >= GOOD_ENOUGH {
                break;
            }
            shift = fine - z_of(&q, self.f0, SEARCH_MESH).ok()?;
        }
        best
    }
}

/// What each size is worth at the tuned design.
fn worth_of(p: &Project, goal: &TagGoal, f0: f64, knobs: &Knobs, ctl: &Control) -> Result<Vec<Worth>> {
    let names = knobs.all();
    let v = p.vars()?;
    let x: Vec<f64> = names.iter().filter_map(|n| v.get(n).copied()).collect();
    if x.len() != names.len() {
        bail!("A variable to tune is missing from this design.");
    }
    let m = problem(p, f0, SEARCH_MESH, None)?;
    let z = m.solve(f0, false)?.z[0][0];
    // How fast the whole circuit's reactance climbs with frequency, chip included.
    let df = f0 * 0.01;
    let (below, above) = (m.solve(f0 - df, false)?.z[0][0], m.solve(f0 + df, false)?.z[0][0]);
    let total = |f: f64, z: C64| z.im + chip_at(goal.chip_z, f).im;
    let climb = (total(f0 + df, above) - total(f0 - df, below)) / (2.0 * df);
    let solver = |y: &[f64]| -> Option<C64> {
        let q = with_sizes(p, &named(&names, y));
        z_of(&q, f0, SEARCH_MESH).ok()
    };
    let probes: Vec<usize> = (0..x.len()).collect();
    let rows = each(&probes, ctl.parallel, &|k: &usize| -> Option<(f64, C64)> {
        let h = (x[*k] * PROBE).max(SMALLEST_PROBE);
        [1.0, -1.0].iter().find_map(|sign: &f64| {
            let mut y = x.clone();
            y[*k] += sign * h;
            solver(&y).map(|w| (sign * h, w))
        })
    });
    Ok(rows
        .into_iter()
        .enumerate()
        .filter_map(|(k, row)| {
            let (h, w) = row?;
            let mm = h * 1e3;
            let (r, xr) = ((w.re - z.re) / mm, (w.im - z.im) / mm);
            let moves = (climb > 0.0).then(|| -xr / climb / 1e6);
            Some(Worth { var: names[k].clone(), is_line: names[k] == knobs.arm_var, r_per_mm: r, x_per_mm: xr, mhz_per_mm: moves })
        })
        .collect())
}

/// Says what limits a design that didn't get there, in terms of what could be changed.
fn notes_on(after: &Reading, f0: f64, moved: &[(String, f64, f64)], knobs: &Knobs, goal: &TagGoal, in_reach: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mhz = |f: f64| f / 1e6;
    if !in_reach && after.transfer < MATCHED {
        out.push(format!("The line can't reach resonance at {:.1} MHz in this footprint with the folds tried: allow more room, or use a structure with more aperture.", mhz(f0)));
    }
    if after.transfer >= MATCHED {
        out.push(format!("Matched at {:.1} MHz: {:.0} % of the collected power reaches the chip.", mhz(f0), after.transfer * 100.0));
    } else {
        let (r, x) = after.z;
        let (rn, xn) = after.needed;
        if (x - xn).abs() > 2.0 * rn {
            let which = if x < xn { "too little" } else { "too much" };
            out.push(format!("The antenna has {which} inductance: {x:.0} Ω of reactance against the {xn:.0} Ω the chip needs. The loop can't correct that within this footprint and these limits."));
        }
        if r < rn * 0.5 {
            out.push(format!("Its resistance is {r:.1} Ω against the chip's {rn:.1} Ω: too small an aperture to collect power, whatever the loop does. A longer or wider radiator is the only real cure."));
        } else if r > rn * 2.0 {
            out.push(format!("Its resistance is {r:.1} Ω against the chip's {rn:.1} Ω: the loop is coupling too strongly. A smaller loop, or one further from the radiator, lowers it."));
        }
        if out.is_empty() {
            out.push(format!("Only {:.0} % of the power reaches the chip at {:.1} MHz.", after.transfer * 100.0, mhz(f0)));
        }
    }
    match after.peak_hz {
        Some(f) if (f - f0).abs() / f0 <= ON_FREQUENCY => {}
        Some(f) => out.push(format!("The peak is at {:.1} MHz, {:+.1} MHz from the target.", mhz(f), mhz(f - f0))),
        None => out.push(format!("The peak is more than {:.0} % from {:.1} MHz.", SPAN * 100.0, mhz(f0))),
    }
    if let Some(w) = after.arm_wire {
        out.push(format!("One arm is {:.1} mm along its metal, {:.2} times a quarter wavelength in air ({:.1} mm). A quarter wave is where to start, not the answer: folding back and forth makes an arm need more, and end plates, film and paper make it need less.", w * 1e3, w / after.quarter_wave, after.quarter_wave * 1e3));
    }
    if moved.iter().all(|c| !knobs.all().contains(&c.0)) {
        out.push("No dimension changed: there was nothing to improve within the limits.".to_string());
    }
    if !goal.stack.is_empty() {
        out.push(format!("Tuned in these surroundings: {}. Layers are as wide as the world in this solver.", goal.stack.describe()));
    }
    out
}

/// Sets a tuned design up so that running it shows what the tune found: this mesh, since a coarser one
/// reads a different reactance, and a sweep that shows the peak and the whole band.
fn ready_to_run(p: &mut Project, goal: &TagGoal, f0: f64) {
    p.setup.f_lo = (f0 * 0.9).min(goal.f_lo);
    p.setup.f_hi = (f0 * 1.1).max(goal.f_hi);
    p.setup.far_freqs = vec![f0];
    p.setup.edges_per_lambda = SEARCH_MESH.edges_per_lambda;
    p.setup.mom_samples = 21;
}

/// Moves the loop and the line until the antenna is the chip's conjugate at `f0`, the way it's done by
/// hand. Find where the line resonates; sit where its resistance equals the chip's; size the loop for
/// the reactance, resetting the line as that moves it; then close what's left with all of them together.
/// Reports both metrics before and after.
pub fn tune(p: &Project, goal: &TagGoal, f0: f64, knobs: &Knobs, ctl: &Control) -> Result<Tune> {
    if !(f0 > 0.0) {
        bail!("The target frequency has to be above zero.");
    }
    let start = fitted(p, goal);
    let solves = AtomicUsize::new(0);
    let expect = 400;
    let step = |what: &str| {
        let n = solves.load(Ordering::Relaxed);
        (ctl.report)(Progress { message: what.to_string(), done: n.min(expect), total: expect, best_range_m: 0.0, found: None });
    };
    step("Reading the design as it stands");
    let before = read(&start, goal, f0, knobs, ctl)?;
    let cancelled = || ctl.cancel.load(Ordering::Relaxed);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(MOST_SECONDS);
    let keep_going = || !cancelled() && std::time::Instant::now() < deadline;
    if cancelled() {
        bail!("Cancelled");
    }

    let base = into_footprint(&start, goal)?;
    let structure_own = knobs_for(&start).is_some_and(|k| k.loop_vars == knobs.loop_vars);
    let lines = lines_for(&base, goal, knobs, f0, structure_own)?;
    let values = base.vars()?;
    let mut loop_x = Vec::with_capacity(knobs.loop_vars.len());
    for n in &knobs.loop_vars {
        let Some(v) = values.get(n).copied().filter(|v| *v > 0.0) else {
            bail!("'{n}' has to be a positive size in this design to be tuned.");
        };
        loop_x.push(v);
    }
    let zc = chip_at(goal.chip_z, f0);
    let job_on = |line: &'_ Line| -> (Vec<String>, Vec<(f64, f64)>) {
        let mut names = knobs.loop_vars.clone();
        names.push(line.arm.clone());
        let mut bounds: Vec<(f64, f64)> = loop_x.iter().map(|v| ((v * 0.15).max(goal.min_feature), v * 6.0)).collect();
        bounds.push((line.lo, line.hi));
        (names, bounds)
    };

    // Where can the line reach its resonance at all? Each way of lengthening it gets one look.
    step("Finding where the line resonates");
    let mut reachable: Vec<&Line> = Vec::new();
    let mut closest: Option<(f64, &Line)> = None;
    for line in &lines {
        let (names, _) = job_on(line);
        let job = Job { base: &line.project, names, goal, f0, zc, solves: &solves, keep_going: &keep_going, parallel: ctl.parallel };
        let samples = job.along(&loop_x, line.lo, line.hi, SCAN_POINTS, C64::new(0.0, 0.0));
        if has_resonance(&samples) {
            reachable.push(line);
            if reachable.len() == 4 {
                break;
            }
        } else if let Some(k) = strongest(&samples)
            && closest.as_ref().is_none_or(|c| samples[k].1.re > c.0)
        {
            closest = Some((samples[k].1.re, line));
        }
    }
    if cancelled() {
        bail!("Cancelled");
    }
    let in_reach = !reachable.is_empty();
    if !in_reach {
        match closest {
            Some((_, line)) => reachable.push(line),
            None => bail!("Nothing near this design builds and fits the footprint."),
        }
    }

    // The loop's own sizes scale together first, then one at a time: what's holding one back, such as
    // the footprint's height, needn't hold the others. A coupling gap isn't a size of the loop.
    let scales: Vec<f64> = knobs.loop_vars.iter().map(|n| if n == "gc" { 0.0 } else { 1.0 }).collect();
    let mut dirs = vec![scales.clone()];
    for k in 0..scales.len() {
        if scales[k] > 0.0 && scales.iter().sum::<f64>() > 1.0 {
            let mut d = vec![0.0; scales.len()];
            d[k] = 1.0;
            dirs.push(d);
        }
    }

    step("Tuning the loop and the line");
    let mut best: Option<(f64, Project, String)> = None;
    'all: for line in &reachable {
        let (names, bounds) = job_on(line);
        let job = Job { base: &line.project, names, goal, f0, zc, solves: &solves, keep_going: &keep_going, parallel: ctl.parallel };
        for side in [Side::Short, Side::Long] {
            for dir in &dirs {
                if !keep_going() || solves.load(Ordering::Relaxed) > MOST_SOLVES {
                    break 'all;
                }
                let Some(near) = job.match_loop(line, &loop_x, dir, side, C64::new(0.0, 0.0)) else { continue };
                step("Closing the match");
                let Some((x, t)) = job.polish(&near, &bounds, ctl) else { continue };
                if best.as_ref().is_none_or(|b| t > b.0) {
                    best = Some((t, job.project(&x), line.arm.clone()));
                }
                if t >= GOOD_ENOUGH {
                    break 'all;
                }
            }
            // The other side of the resonance is larger: it's only worth trying if this one fell short.
            if best.as_ref().is_some_and(|b| b.0 >= MATCHED) {
                break 'all;
            }
        }
    }
    if cancelled() {
        bail!("Cancelled");
    }
    let Some((_, mut project, arm_var)) = best else { bail!("Nothing near this design builds and fits the footprint.") };
    ready_to_run(&mut project, goal, f0);
    let knobs = Knobs { loop_vars: knobs.loop_vars.clone(), arm_var };

    step("Reading the tuned design");
    let after = read(&project, goal, f0, &knobs, ctl)?;
    let worth = worth_of(&project, goal, f0, &knobs, ctl)?;
    let score = evaluate(&project, goal, REPORT_MESH)?;

    let (was, now) = (start.vars()?, project.vars()?);
    let changes: Vec<(String, f64, f64)> = project
        .params
        .iter()
        .filter(|r| !r.derived)
        .filter_map(|r| {
            let (a, b) = (*was.get(&r.name)?, *now.get(&r.name)?);
            ((a - b).abs() > 1e-9 * a.abs().max(1e-9)).then(|| (r.name.clone(), a, b))
        })
        .collect();
    let mut notes = notes_on(&after, f0, &changes, &knobs, goal, in_reach);
    if after.transfer < MATCHED && (std::time::Instant::now() >= deadline || solves.load(Ordering::Relaxed) > MOST_SOLVES) {
        notes.push("The search ran out of its allowance before it matched, so a better answer may exist. The design search goes much further.".to_string());
    }
    // Landing on one frequency says nothing about the rest of the band, and a tag is narrow.
    if goal.f_hi > goal.f_lo && after.matched() && score.worst_transfer < MATCHED {
        let centre = (goal.f_lo + goal.f_hi) / 2.0;
        let cure = if (f0 - centre).abs() > (goal.f_hi - goal.f_lo) * 0.1 { format!("Tuning to the middle of the band, {:.1} MHz, shares that out.", centre / 1e6) } else { "One tune can't hold a band this wide with this structure: the design search looks for wider ones.".to_string() };
        notes.push(format!("Across {:.0}-{:.0} MHz the worst is {:.0} % of the power to the chip, against {:.0} % at {:.1} MHz. {cure}", goal.f_lo / 1e6, goal.f_hi / 1e6, score.worst_transfer * 100.0, after.transfer * 100.0, f0 / 1e6));
    }
    Ok(Tune { f0, knobs, before, after, project, changes, worth, score, notes, solves: solves.load(Ordering::Relaxed) })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::tag_design::{FAMILIES, candidate, goal_for};

    fn ucode9() -> TagGoal {
        TagGoal { max_x: Some(0.095), max_y: Some(0.03), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) }
    }

    #[test]
    fn every_structure_with_one_line_has_knobs_it_really_owns() {
        let goal = ucode9();
        for f in &FAMILIES {
            let c = candidate(f, &goal).unwrap();
            let Some(k) = knobs_for(&c.project) else {
                assert_eq!(f.template, "tag_dual", "only two radiators has no one line to set");
                continue;
            };
            let v = c.project.vars().unwrap();
            for n in k.all() {
                assert!(v.get(&n).is_some_and(|x| *x > 0.0), "{}: {n}", f.template);
            }
        }
        let mut drawn = Project::from_template("rfid_tag", Some("915MHz")).unwrap();
        drawn.template = "custom".into();
        assert!(knobs_for(&drawn).is_none(), "a drawing has whatever its designer named");
    }

    /// The T-match meander's arm walked by hand: 47.5 mm along the axis, and four sideways runs of 3 mm
    /// in each of two periods.
    #[test]
    fn an_arm_is_measured_along_its_metal() {
        let mut p = Project::from_template("rfid_tag", Some("915MHz")).unwrap();
        for (n, v) in [("Lx", "95mm"), ("n", "2"), ("Hm", "3mm")] {
            p.params.iter_mut().find(|r| r.name == n).unwrap().expr = v.into();
        }
        assert!((arm_wire(&p).unwrap() - (0.0475 + 8.0 * 0.003)).abs() < 1e-12);
    }

    #[test]
    fn the_residual_is_zero_only_at_the_conjugate_and_a_step_finds_it() {
        let zc = C64::new(15.0, -231.0);
        assert_eq!(size(&residual(C64::new(15.0, 231.0), zc)), 0.0);
        assert!(size(&residual(C64::new(15.0, 200.0), zc)) > 0.5);
        // With more sizes than equations, the damped step meets both and hardly moves the third.
        let jac = [[1.0, 0.0], [0.0, 2.0], [0.0, 0.0]];
        let d = damped_step(&jac, &[0.5, -1.0], 1e-9);
        assert!((d[0] + 0.5).abs() < 1e-6 && (d[1] - 0.5).abs() < 1e-6 && d[2].abs() < 1e-6, "{d:?}");
    }

    #[test]
    fn a_parabola_through_three_points_tops_out_between_them() {
        assert!((vertex(0.0, 1.0, 0.0)).abs() < 1e-12);
        assert!(vertex(0.5, 1.0, 0.9) > 0.0, "nearer the higher neighbour");
        assert_eq!(vertex(1.0, 0.5, 1.0), 0.0, "a dip has no top");
    }

    /// A stand-in tag with a known answer, solved in no time: loop size sets reactance, line length sets
    /// resistance and adds a little reactance, as the real one does.
    fn stand_in(x: &[f64]) -> Option<C64> {
        let (a, line) = (x[0], x[1]);
        Some(C64::new(15.0 * (line / 0.09).powi(4), 8000.0 * a + 900.0 * (line - 0.09)))
    }

    fn search_of<'a>(solver: &'a Solver<'a>, keep_going: &'a (dyn Fn() -> bool + Sync)) -> Search<'a> {
        Search { solver, zc: C64::new(15.0, -231.0), bounds: vec![(0.001, 0.2), (0.01, 0.3)], parallel: 2, max_rounds: 20, keep_going }
    }

    #[test]
    fn descent_reaches_a_conjugate_match_from_a_poor_start() {
        let keep_going = || true;
        let s = search_of(&stand_in, &keep_going);
        let (x, z) = descend(&s, &[0.05, 0.06]).unwrap();
        assert!(power_transfer(z, s.zc) > GOOD_ENOUGH, "{x:?} {z}");
    }

    #[test]
    fn descent_stops_when_told_and_refuses_what_doesnt_build() {
        let stop = || false;
        let s = search_of(&stand_in, &stop);
        let (x, _) = descend(&s, &[0.05, 0.06]).unwrap();
        assert_eq!(x, vec![0.05, 0.06], "told to stop before its first step");
        let never = |_: &[f64]| None;
        let go = || true;
        assert!(descend(&search_of(&never, &go), &[0.05, 0.06]).is_none());
    }

    /// Past the footprint's edge the search keeps to what fits, and gets as near as that allows.
    #[test]
    fn descent_stays_inside_what_fits() {
        let walled = |x: &[f64]| if x[1] > 0.07 { None } else { stand_in(x) };
        let go = || true;
        let s = search_of(&walled, &go);
        let (x, z) = descend(&s, &[0.03, 0.06]).unwrap();
        assert!(x[1] <= 0.07, "{x:?}");
        assert!((z.im - 231.0).abs() < 25.0, "the loop still sets the reactance: {z}");
    }

    #[test]
    fn a_design_too_long_for_the_footprint_is_brought_in_with_its_wire_kept() {
        let goal = TagGoal { max_x: Some(0.07), max_y: Some(0.03), ..ucode9() };
        let p = fitted(&Project::from_template("rfid_tag", Some("915MHz")).unwrap(), &goal);
        assert!(!fits(&p, &goal).unwrap());
        let q = into_footprint(&p, &goal).unwrap();
        assert!(fits(&q, &goal).unwrap());
        let (was, now) = (arm_wire(&p).unwrap(), arm_wire(&q).unwrap());
        assert!((now - was).abs() / was < 0.05, "{was} {now}");
    }

    /// The template at 915 MHz is 95.016 mm long, which a real model was told fitted 95 mm.
    #[test]
    fn a_design_a_hair_over_its_footprint_is_over_and_comes_in() {
        let goal = ucode9();
        let p = fitted(&Project::from_template("rfid_tag", Some("915MHz")).unwrap(), &goal);
        assert!(!fits(&p, &goal).unwrap());
        let (lo, hi) = into_footprint(&p, &goal).unwrap().geometry().unwrap().bounds();
        assert!(hi[0] - lo[0] <= 0.095, "{}", hi[0] - lo[0]);
    }

    #[test]
    fn every_structure_with_a_line_can_be_brought_into_a_label() {
        let goal = TagGoal { max_x: Some(0.08), max_y: Some(0.025), ..ucode9() };
        for f in &FAMILIES {
            if f.template == "tag_dual" {
                continue;
            }
            let p = fitted(&Project::from_template(f.template, Some("902MHz")).unwrap(), &goal);
            if f.template == "tag_spiral" {
                let e = into_footprint(&p, &goal).unwrap_err();
                assert!(format!("{e:#}").contains("footprint"), "a spiral is for a square label: {e:#}");
                continue;
            }
            let q = into_footprint(&p, &goal).unwrap_or_else(|e| panic!("{}: {e:#}", f.template));
            assert!(fits(&q, &goal).unwrap(), "{}", f.template);
        }
    }

    fn at(arm: f64, r: f64) -> (f64, C64) {
        (arm, C64::new(r, 0.0))
    }

    #[test]
    fn a_flank_is_found_on_the_side_asked_for_and_only_past_a_real_turn() {
        let curve = [at(1.0, 2.0), at(2.0, 8.0), at(3.0, 70.0), at(4.0, 30.0), at(5.0, 6.0)];
        assert!(has_resonance(&curve));
        assert_eq!(flank(&curve, 15.0, Side::Short), Some((2.0, 3.0)));
        assert_eq!(flank(&curve, 15.0, Side::Long), Some((4.0, 5.0)));
        assert_eq!(flank(&curve, 90.0, Side::Short), None, "never that high");
        assert!(!has_resonance(&[at(1.0, 2.0), at(2.0, 5.0), at(3.0, 9.0)]), "still climbing at the end of its range");
    }

    #[test]
    fn a_low_footprint_gets_more_periods_to_try_and_a_drawing_gets_its_own_variable() {
        let goal = ucode9();
        let p = fitted(&Project::from_template("rfid_tag", Some("915MHz")).unwrap(), &goal);
        let knobs = knobs_for(&p).unwrap();
        let lines = lines_for(&p, &goal, &knobs, 902e6, true).unwrap();
        let n_of = |l: &Line| l.project.vars().unwrap()["n"];
        assert_eq!(n_of(&lines[0]), 2.0, "the design's own count comes first");
        assert!(lines.iter().any(|l| n_of(l) > 2.0));
        assert!(lines.iter().all(|l| l.arm == "Hm" && l.hi <= 0.015));
        let own = lines_for(&p, &goal, &Knobs { loop_vars: vec!["a".into()], arm_var: "Lx".into() }, 902e6, false).unwrap();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].arm, "Lx");
        assert!(lines_for(&p, &goal, &Knobs { loop_vars: vec!["a".into()], arm_var: "nope".into() }, 902e6, false).is_err());
    }

    #[test]
    fn a_wider_loop_pushes_the_meander_out_so_it_still_builds() {
        let goal = ucode9();
        let p = fitted(&Project::from_template("rfid_tag", Some("915MHz")).unwrap(), &goal);
        let q = with_sizes(&p, &[("a".to_string(), 0.03)]);
        q.geometry().unwrap();
        assert!(q.vars().unwrap()["x0"] > 0.015);
    }

    fn quiet() -> (AtomicBool, impl Fn(Progress) + Sync) {
        (AtomicBool::new(false), |_p: Progress| {})
    }

    /// The whole method against the real solver, for the example asked for: a T-match meander, a
    /// UCODE 9 and 902 MHz. Both metrics have to land.
    #[test]
    fn the_loop_and_line_put_a_real_tag_on_902_mhz_and_matched() {
        let goal = ucode9();
        let f0 = 902e6;
        let p = Project::from_template("rfid_tag", Some("915MHz")).unwrap();
        let knobs = knobs_for(&p).unwrap();
        let (cancel, report) = quiet();
        let ctl = Control { cancel: &cancel, report: &report, parallel: 4 };
        let t = tune(&p, &goal, f0, &knobs, &ctl).unwrap();
        println!("{t:#?}");
        assert!(t.after.transfer >= MATCHED, "{:?}", t.after);
        let peak = t.after.peak_hz.expect("a peak in the sweep");
        assert!((peak - f0).abs() / f0 <= ON_FREQUENCY, "peak {peak}");
        assert!(t.after.transfer > t.before.transfer, "{} {}", t.before.transfer, t.after.transfer);
        assert!(t.after.size_x <= 0.095 && t.after.size_y <= 0.03, "{:?}", t.after);
        assert!(t.score.center_range_m > 0.0);
        assert_eq!(t.project.setup.edges_per_lambda, SEARCH_MESH.edges_per_lambda, "a run has to see what the tune saw");
        assert!(t.project.setup.f_lo < f0 && t.project.setup.f_hi >= 928e6, "and the peak and the band");
        assert!(t.notes.iter().any(|n| n.contains("902-928") && n.contains("915.0")), "tuned to the band's edge, it says what that costs: {:?}", t.notes);
        // The same answer by a path that doesn't share the tuner's mesh.
        let z = evaluate(&t.project, &TagGoal { f_lo: f0, f_hi: f0, ..goal.clone() }, REPORT_MESH).unwrap();
        assert!(z.center_transfer >= MATCHED * 0.95, "{z:?}");
    }
}

/// Whether the rule of thumb holds, and how the tuner does across jobs. `cargo test -p antenna_core
/// --release tag_tune_study -- --ignored --nocapture --test-threads=1`
#[cfg(test)]
mod studies {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::stack::preset;
    use crate::tag_design::goal_for;

    fn say(t: &Tune, seconds: f64) {
        let a = &t.after;
        println!("  {:.0} s, {} solves: t {:.2} to {:.2}, Z {:.1}{:+.1}j (needs {:.1}{:+.1}j), peak {:?} MHz, arm {:?} mm ({:.2} x lambda/4), {:.1} x {:.1} mm, {:.1} dBi, eff {:.2}, range {:.1} m", seconds, t.solves, t.before.transfer, a.transfer, a.z.0, a.z.1, a.needed.0, a.needed.1, a.peak_hz.map(|f| (f / 1e4).round() / 1e2), a.arm_wire.map(|w| (w * 1e4).round() / 10.0), a.arm_wire.unwrap_or(f64::NAN) / a.quarter_wave, a.size_x * 1e3, a.size_y * 1e3, a.gain_dbi, a.efficiency, a.range_m);
        println!("  moved {:?}", t.changes.iter().map(|c| if c.0 == "n" { format!("n {} to {}", c.1, c.2) } else { format!("{} {:.2} to {:.2}", c.0, c.1 * 1e3, c.2 * 1e3) }).collect::<Vec<_>>());
        for w in &t.worth {
            println!("  {:<6} per mm: R {:+.2}, X {:+.2}, {:+.2?} MHz{}", w.var, w.r_per_mm, w.x_per_mm, w.mhz_per_mm, if w.is_line { " (line)" } else { "" });
        }
        for n in &t.notes {
            println!("  {n}");
        }
    }

    #[test]
    #[ignore]
    fn tag_tune_study_across_jobs() {
        let cancel = AtomicBool::new(false);
        let report = |_p: Progress| {};
        let ctl = Control { cancel: &cancel, report: &report, parallel: 4 };
        let free = TagGoal { max_x: Some(0.095), max_y: Some(0.03), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) };
        let paper = TagGoal { stack: (preset("paper_label").unwrap().stack)(), ..free.clone() };
        let slit = TagGoal { chip_z: (10.0, -191.0), ..free.clone() };
        let eu = TagGoal { f_lo: 865e6, f_hi: 868e6, ..free.clone() };
        let small = TagGoal { max_x: Some(0.05), max_y: Some(0.02), ..free.clone() };
        for (what, template, goal, f0) in [
            ("T-match, 902 MHz, bare", "rfid_tag", &free, 902e6),
            ("T-match, 915 MHz, bare", "rfid_tag", &free, 915e6),
            ("T-match, 928 MHz, bare", "rfid_tag", &free, 928e6),
            ("T-match, 866.5 MHz, bare", "rfid_tag", &eu, 866.5e6),
            ("T-match, 902 MHz, slit chip", "rfid_tag", &slit, 902e6),
            ("T-match, 902 MHz, under paper", "rfid_tag", &paper, 902e6),
            ("T-match, 902 MHz, 50 x 20 mm", "rfid_tag", &small, 902e6),
            ("Coupled loop, 902 MHz, bare", "tag_loop", &free, 902e6),
            ("Coupled loop, 902 MHz, under paper", "tag_loop", &paper, 902e6),
            ("Dogbone, 902 MHz, bare", "tag_dogbone", &free, 902e6),
            ("Bow-tie, 902 MHz, bare", "tag_bowtie", &free, 902e6),
            ("Slotted plate, 902 MHz, bare", "tag_slot", &free, 902e6),
            ("Bent arms, 902 MHz, bare", "tag_bent", &free, 902e6),
        ] {
            println!("{what}");
            let p = Project::from_template(template, Some(&format!("{}MHz", f0 / 1e6))).unwrap();
            let knobs = knobs_for(&p).unwrap();
            let t0 = std::time::Instant::now();
            match tune(&p, goal, f0, &knobs, &ctl) {
                Ok(t) => say(&t, t0.elapsed().as_secs_f64()),
                Err(e) => println!("  {e:#}"),
            }
        }
    }

    /// How smooth is what the tuner walks on? One size at a time, in fine steps, on both meshes.
    #[test]
    #[ignore]
    fn tag_tune_study_surface() {
        let goal = TagGoal { max_x: Some(0.095), max_y: Some(0.03), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) };
        let f0 = 902e6;
        let zc = chip_at(goal.chip_z, f0);
        let p = fitted(&Project::from_template("rfid_tag", Some("902MHz")).unwrap(), &goal);
        let v = p.vars().unwrap();
        println!("{:?}", ["Lx", "w", "a", "b", "g", "n", "Hm", "s", "x0"].map(|k| (k, (v[k] * 1e5).round() / 1e2)));
        for (name, lo, hi, step) in [("Hm", 2.0, 14.0, 0.5), ("a", 8.0, 40.0, 2.0), ("b", 3.0, 14.0, 0.5)] {
            let mut x = lo;
            while x <= hi {
                let q = p.with_values(&[(name.to_string(), x * 1e-3)]);
                let t = std::time::Instant::now();
                match (z_of(&q, f0, SEARCH_MESH), z_of(&q, f0, REPORT_MESH)) {
                    (Ok(a), Ok(b)) => println!("{name} {x:>5.2}: search {:>7.1}{:>+8.1}j t {:.2} | report {:>7.1}{:>+8.1}j t {:.2} | fits {} | {:.0} ms", a.re, a.im, power_transfer(a, zc), b.re, b.im, power_transfer(b, zc), fits(&q, &goal).unwrap_or(false), t.elapsed().as_secs_f64() * 1e3),
                    (a, b) => println!("{name} {x:>5.2}: {:?} {:?}", a.err().map(|e| e.to_string()), b.err().map(|e| e.to_string())),
                }
                x += step;
            }
        }
    }

    /// Is it true that the line sets the frequency and the loop the match? One at a time from a tuned
    /// tag: where the peak goes, and what the reactance and resistance do.
    #[test]
    #[ignore]
    fn tag_tune_study_of_the_two_roles() {
        let cancel = AtomicBool::new(false);
        let report = |_p: Progress| {};
        let ctl = Control { cancel: &cancel, report: &report, parallel: 4 };
        let goal = TagGoal { max_x: Some(0.095), max_y: Some(0.03), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) };
        let f0 = 902e6;
        for template in ["rfid_tag", "tag_loop"] {
            let p = Project::from_template(template, Some("902MHz")).unwrap();
            let knobs = knobs_for(&p).unwrap();
            let t = tune(&p, &goal, f0, &knobs, &ctl).unwrap();
            println!("{template}");
            say(&t, 0.0);
            let v = t.project.vars().unwrap();
            for name in knobs.all() {
                for k in [0.85, 0.95, 1.05, 1.15] {
                    let q = t.project.with_values(&[(name.clone(), v[&name] * k)]);
                    if !fits(&q, &goal).unwrap_or(false) {
                        println!("  {name} x {k}: doesn't fit");
                        continue;
                    }
                    match read(&q, &goal, f0, &knobs, &ctl) {
                        Ok(r) => println!("  {name} x {k}: Z {:.1}{:+.1}j, t {:.2}, peak {:?} MHz at t {:.2}", r.z.0, r.z.1, r.transfer, r.peak_hz.map(|f| (f / 1e4).round() / 1e2), r.peak_transfer),
                        Err(e) => println!("  {name} x {k}: {e:#}"),
                    }
                }
            }
        }
    }
}
