//! Designing a UHF tag from what it has to do, rather than adjusting a reference: every structure this
//! knows, and any the designer draws, searched against the chip, the band, the footprint and what the
//! tag will really be stuck in, judged by read range. Several different answers come back, each with
//! what it's good for, and what it does when its surroundings change.
//!
//! Everything solved here is a model: results are Simulated, never Measured.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::mom::{MomOptions, MomProblem};
use crate::optimize::{self, Method, Rng};
use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Param, Project};
use crate::stack::Stack;
use crate::tag::{chip_at, power_transfer, read_range_m};
use crate::{C0, C64};

/// What a tag has to do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagGoal {
    pub f_lo: f64,
    pub f_hi: f64,
    /// Series impedance at 915 MHz.
    pub chip_z: (f64, f64),
    /// Read sensitivity, dBm.
    pub read_dbm: f64,
    /// The reader's power and gain, dBm.
    pub eirp_dbm: f64,
    /// The largest it may be, metres. `None` is no limit.
    pub max_x: Option<f64>,
    pub max_y: Option<f64>,
    /// The thinnest trace and narrowest gap the process can make, metres.
    pub min_feature: f64,
    /// What it's in.
    pub stack: Stack,
    pub metal: String,
}

impl TagGoal {
    pub fn fc(&self) -> f64 {
        (self.f_lo + self.f_hi) / 2.0
    }

    fn lambda(&self) -> f64 {
        C0 / self.fc()
    }

    /// Enough frequencies to see the band, and no more: each is a whole matrix.
    fn freqs(&self) -> Vec<f64> {
        let wide = (self.f_hi - self.f_lo) / self.fc() > 0.04;
        crate::linspace(self.f_lo, self.f_hi, if wide { 5 } else { 3 })
    }

    fn range(&self, f: f64, gain_dbi: f64, t: f64) -> f64 {
        read_range_m(f, self.eirp_dbm, gain_dbi, t, self.read_dbm, 1.0)
    }
}

/// What a design does against a goal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TagScore {
    /// Forward-link read range at the band's worst frequency, aligned, in the stack, metres.
    pub worst_range_m: f64,
    pub center_range_m: f64,
    pub worst_transfer: f64,
    pub center_transfer: f64,
    pub gain_dbi: f64,
    pub efficiency: f64,
    pub size_x: f64,
    pub size_y: f64,
    /// Antenna impedance at the band's centre.
    pub z_center: (f64, f64),
    pub unknowns: usize,
}

/// What a study asks of each solve. Coarse while searching, finer for what's reported.
#[derive(Clone, Copy, Debug)]
pub struct Mesh {
    pub edges_per_lambda: f64,
    pub far_step: f64,
    /// The most unknowns a model may come to. Several run at once and a dense matrix goes as the
    /// square: a fine fractal meshed past this took 118 GB, on the way to a design nobody could use.
    pub limit: usize,
}

/// A plate needs this: at 48 a slotted plate read 206+538j against the 159+461j it converges to,
/// and at 96 it's within 2 %. See `tag_study_mesh`.
pub const SEARCH_MESH: Mesh = Mesh { edges_per_lambda: 96.0, far_step: 15.0, limit: 2500 };
pub const REPORT_MESH: Mesh = Mesh { edges_per_lambda: 192.0, far_step: 5.0, limit: 7000 };

/// A project on this goal's metal, stack and band.
pub fn fitted(p: &Project, goal: &TagGoal) -> Project {
    let mut q = p.clone();
    q.stack = goal.stack.clone();
    q.metal = goal.metal.clone();
    q.setup.f_lo = goal.f_lo;
    q.setup.f_hi = goal.f_hi;
    q.setup.far_freqs = vec![goal.fc()];
    q
}

pub fn evaluate(p: &Project, goal: &TagGoal, mesh: Mesh) -> Result<TagScore> {
    let g = p.geometry()?;
    let fs = goal.freqs();
    let top = *fs.last().unwrap_or(&goal.f_hi);
    let problem = MomProblem::new(&g, top, MomOptions { edges_per_lambda: mesh.edges_per_lambda, theta_step: mesh.far_step, phi_step: mesh.far_step, ..Default::default() })?;
    if problem.unknowns() > mesh.limit {
        bail!("This design meshes to {} unknowns, past the {} a search allows: its traces are too fine for its area.", problem.unknowns(), mesh.limit);
    }
    let fc = goal.fc();
    let centre = problem.solve(fc, true)?;
    let m = centre.far[0].metrics();
    let zc = centre.z[0][0];
    let gain = m.peak_gain_dbi;
    let (mut worst_range, mut worst_t) = (f64::INFINITY, 1.0f64);
    for f in &fs {
        // The centre is already solved, and each frequency is a whole matrix.
        let z = if *f == fc { zc } else { problem.solve(*f, false)?.z[0][0] };
        let t = power_transfer(z, chip_at(goal.chip_z, *f));
        worst_t = worst_t.min(t);
        worst_range = worst_range.min(goal.range(*f, gain, t));
    }
    let tc = power_transfer(zc, chip_at(goal.chip_z, fc));
    let (lo, hi) = g.bounds();
    Ok(TagScore {
        worst_range_m: worst_range,
        center_range_m: goal.range(fc, gain, tc),
        worst_transfer: worst_t,
        center_transfer: tc,
        gain_dbi: gain,
        efficiency: m.rad_efficiency,
        size_x: hi[0] - lo[0],
        size_y: hi[1] - lo[1],
        z_center: (zc.re, zc.im),
        unknowns: problem.unknowns(),
    })
}

/// The thing to minimise. Range, with a little of the centre so a flat stretch still has a slope,
/// and a steep charge for leaving the footprint so the search knows which way is back.
fn penalty(s: &TagScore, goal: &TagGoal) -> f64 {
    let over = |size: f64, limit: Option<f64>| limit.map_or(0.0, |m| (size - m).max(0.0) / goal.lambda());
    // A millimetre over costs about 6 m at 915 MHz: more than any gain from leaving the label.
    -(s.worst_range_m + 0.1 * s.center_range_m) + 2000.0 * (over(s.size_x, goal.max_x) + over(s.size_y, goal.max_y))
}

/// A range to search one variable over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bound {
    /// A length, as fractions of the wavelength.
    Lambda(f64, f64),
    /// A count, or a plain number.
    Plain(f64, f64),
    /// A trace or gap: from the process's limit to this fraction of the wavelength.
    Fine(f64),
}

/// A kind of structure, and how far to search it.
pub struct Family {
    pub template: &'static str,
    pub title: &'static str,
    /// What makes it different, in terms that help someone choose.
    pub idea: &'static str,
    pub good_for: &'static str,
    pub bounds: &'static [(&'static str, Bound)],
}

use Bound::{Fine, Lambda, Plain};

pub const FAMILIES: [Family; 9] = [
    Family {
        template: "rfid_tag",
        title: "T-match meander dipole",
        idea: "A meandered dipole with an inductive T-match, the textbook inlay.",
        good_for: "A long narrow label, with a familiar way to tune it",
        bounds: &[("Lx", Lambda(0.15, 0.45)), ("w", Fine(0.012)), ("a", Lambda(0.02, 0.16)), ("b", Lambda(0.006, 0.05)), ("g", Fine(0.006)), ("n", Plain(0.0, 6.0)), ("Hm", Lambda(0.006, 0.04)), ("s", Lambda(0.005, 0.02)), ("x0", Lambda(0.02, 0.1))],
    },
    Family {
        template: "tag_loop",
        title: "Coupled loop and radiator",
        idea: "The chip's own small loop, coupled through a gap to a separate radiator, so resistance and reactance tune nearly apart.",
        good_for: "The strongest match with the most freedom to retune for a material",
        bounds: &[("Lx", Lambda(0.15, 0.45)), ("w", Fine(0.012)), ("a", Lambda(0.02, 0.12)), ("b", Lambda(0.01, 0.06)), ("wl", Fine(0.012)), ("gc", Lambda(0.0015, 0.02)), ("g", Fine(0.006)), ("n", Plain(0.0, 5.0)), ("Hm", Lambda(0.006, 0.04)), ("s", Lambda(0.005, 0.02)), ("x0", Lambda(0.03, 0.1)), ("Lp", Lambda(0.0, 0.06)), ("Hp", Lambda(0.01, 0.08))],
    },
    Family {
        template: "tag_dual",
        title: "Loop and two radiators",
        idea: "One loop coupled to a long and a short radiator: two resonances from one chip.",
        good_for: "Covering two regions or a wide band, such as 865 and 915 MHz with one tag",
        bounds: &[("L1", Lambda(0.15, 0.5)), ("L2", Lambda(0.12, 0.45)), ("w", Fine(0.012)), ("a", Lambda(0.02, 0.12)), ("b", Lambda(0.01, 0.06)), ("wl", Fine(0.012)), ("g1", Lambda(0.0015, 0.02)), ("g2", Lambda(0.0015, 0.02)), ("g", Fine(0.006)), ("t1", Lambda(0.0, 0.05)), ("t2", Lambda(0.0, 0.05)), ("n1", Plain(0.0, 4.0)), ("n2", Plain(0.0, 4.0)), ("Hm", Lambda(0.004, 0.03)), ("s", Lambda(0.005, 0.02)), ("x0", Lambda(0.03, 0.1))],
    },
    Family {
        template: "tag_dogbone",
        title: "Dogbone",
        idea: "Wide arms and heavy end plates with a T-match: much aperture and low Q for its length.",
        good_for: "The most gain and range when there's height to spare",
        bounds: &[("Lx", Lambda(0.15, 0.45)), ("wa", Lambda(0.006, 0.06)), ("Lp", Lambda(0.01, 0.1)), ("Hp", Lambda(0.02, 0.15)), ("a", Lambda(0.02, 0.12)), ("b", Lambda(0.01, 0.06)), ("wt", Fine(0.012)), ("g", Fine(0.006))],
    },
    Family {
        template: "tag_bowtie",
        title: "Bow-tie with a hairpin",
        idea: "A flared flat dipole with an inductive hairpin across the chip.",
        good_for: "The widest band, and the least bothered by changing surroundings",
        bounds: &[("Lx", Lambda(0.15, 0.45)), ("Hb", Lambda(0.015, 0.15)), ("wn", Lambda(0.004, 0.02)), ("x0", Lambda(0.015, 0.08)), ("xs", Lambda(0.006, 0.08)), ("hs", Lambda(0.006, 0.06)), ("wt", Fine(0.012)), ("g", Fine(0.006))],
    },
    Family {
        template: "tag_slot",
        title: "Slotted plate",
        idea: "A solid plate, an inductive window and a slit holding the chip: no thin traces at all.",
        good_for: "Low loss and easy printing, and a spacer over metal",
        bounds: &[("Lx", Lambda(0.15, 0.5)), ("Ly", Lambda(0.03, 0.2)), ("a", Lambda(0.01, 0.2)), ("b", Lambda(0.005, 0.15)), ("g", Fine(0.006)), ("wp", Fine(0.012)), ("ys", Lambda(0.002, 0.03))],
    },
    Family {
        template: "tag_spiral",
        title: "Square spiral",
        idea: "Two arms wound into a square, with a hairpin across the chip.",
        good_for: "A square label with no length to spare, and a rounder pattern",
        bounds: &[("r0", Lambda(0.015, 0.05)), ("pitch", Lambda(0.006, 0.025)), ("turns", Plain(3.0, 16.0)), ("last", Plain(0.1, 1.0)), ("xs", Lambda(0.005, 0.04)), ("hs", Lambda(0.005, 0.04)), ("w", Fine(0.008)), ("g", Fine(0.006))],
    },
    Family {
        template: "tag_fractal",
        title: "Fractal arms",
        idea: "Each arm fills a block with a Hilbert curve, with a T-match.",
        good_for: "The most wire in the least area, at a cost in loss",
        bounds: &[("Ax", Lambda(0.04, 0.2)), ("Ay", Lambda(0.03, 0.15)), ("order", Plain(1.0, 3.0)), ("x0", Lambda(0.02, 0.08)), ("a", Lambda(0.02, 0.1)), ("b", Lambda(0.008, 0.05)), ("w", Fine(0.006)), ("g", Fine(0.006))],
    },
    Family {
        template: "tag_bent",
        title: "Bent arms",
        idea: "Arms turned at right angles, so current runs along both axes.",
        good_for: "Reading at more orientations, with fewer deep nulls",
        bounds: &[("Xa", Lambda(0.06, 0.25)), ("Ya", Lambda(0.02, 0.15)), ("w", Fine(0.02)), ("a", Lambda(0.02, 0.1)), ("b", Lambda(0.008, 0.05)), ("wt", Fine(0.012)), ("g", Fine(0.006))],
    },
];

pub fn family(template: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.template == template)
}

/// How long to search. Roughly solves per structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Quick,
    Normal,
    Deep,
    Exhaustive,
}

impl Effort {
    pub fn solves(self) -> usize {
        match self {
            Effort::Quick => 60,
            Effort::Normal => 240,
            Effort::Deep => 900,
            Effort::Exhaustive => 3000,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

/// A design someone could choose.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TagOption {
    pub id: String,
    /// The structure, in words.
    pub title: String,
    pub idea: String,
    pub good_for: String,
    pub project: Project,
    /// Its dimensions, in millimetres where they're lengths.
    pub values: Vec<(String, f64)>,
    pub score: TagScore,
    /// How far the range moves between two fine meshes, as a fraction: what the mesh still leaves open.
    pub mesh_shift: f64,
    /// What it does as its surroundings change: `(what changed, worst range in metres)`.
    pub if_changed: Vec<(String, f64)>,
    /// Why this one is on the list.
    pub stands_out: Vec<String>,
    /// What to bear in mind.
    pub warnings: Vec<String>,
    /// Bandwidth over which the range stays above 70 % of its best, from a wide sweep, Hz.
    #[serde(default)]
    pub band: Option<(f64, f64)>,
}

/// What's been found so far, to read while a search runs.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub message: String,
    pub done: usize,
    pub total: usize,
    pub best_range_m: f64,
}

/// A way for a long search to report, and to be told to stop.
pub struct Control<'a> {
    pub cancel: &'a AtomicBool,
    pub report: &'a (dyn Fn(Progress) + Sync),
    /// How many solves may run at once.
    pub parallel: usize,
}

/// Something to search: a project, and which of its variables may move and how far.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub title: String,
    pub idea: String,
    pub good_for: String,
    pub project: Project,
    pub vars: Vec<(String, f64, f64)>,
}

fn is_length_bound(b: Bound) -> bool {
    !matches!(b, Bound::Plain(..))
}

/// A structure ready to search against a goal: bands sized, footprint and process limits applied.
pub fn candidate(f: &Family, goal: &TagGoal) -> Result<Candidate> {
    let l = goal.lambda();
    let mut p = Project::from_template(f.template, Some(&format!("{}MHz", goal.fc() / 1e6)))?;
    p.fit_band(goal.f_lo, goal.f_hi);
    let p = fitted(&p, goal);
    let mut vars = Vec::with_capacity(f.bounds.len());
    for (name, bound) in f.bounds {
        let (lo, hi) = match *bound {
            Lambda(a, b) => (a * l, b * l),
            Plain(a, b) => (a, b),
            Fine(b) => (goal.min_feature, (b * l).max(goal.min_feature * 1.5)),
        };
        if !p.params.iter().any(|r| r.name == *name) {
            bail!("{}: no variable {name}", f.template);
        }
        // Nothing overall can start past the footprint, so don't look there.
        let mut hi = hi;
        if is_length_bound(*bound) {
            if let Some(m) = goal.max_x.filter(|_| matches!(*name, "Lx" | "L1" | "L2")) {
                hi = hi.min(m);
            }
            if let Some(m) = goal.max_y.filter(|_| matches!(*name, "Ly")) {
                hi = hi.min(m);
            }
        }
        vars.push(((*name).to_string(), lo, hi.max(lo * 1.001)));
    }
    Ok(Candidate { title: f.title.to_string(), idea: f.idea.to_string(), good_for: f.good_for.to_string(), project: p, vars })
}

const NO_GEOMETRY: f64 = 1e4;

/// Searches one candidate: broadly at first, then close in around the best, all at the search's mesh.
/// Returns the best values and how it scored, or nothing if no design fitted.
pub fn search_one(c: &Candidate, goal: &TagGoal, solves: usize, ctl: &Control, seed: u64, on_best: &(dyn Fn(usize, f64) + Sync)) -> Option<(Vec<(String, f64)>, TagScore)> {
    let names: Vec<String> = c.vars.iter().map(|v| v.0.clone()).collect();
    let bounds: Vec<(f64, f64)> = c.vars.iter().map(|v| (v.1, v.2)).collect();
    let start = c.project.vars().unwrap_or_default();
    let x0: Vec<f64> = c.vars.iter().map(|v| start.get(&v.0).copied().unwrap_or((v.1 + v.2) / 2.0).clamp(v.1, v.2)).collect();
    let best: Mutex<Option<(f64, Vec<f64>, TagScore)>> = Mutex::new(None);
    let done = AtomicUsize::new(0);
    let f = |x: &[f64]| -> f64 {
        if ctl.cancel.load(Ordering::Relaxed) {
            return NO_GEOMETRY;
        }
        let values: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
        let p = c.project.with_values(&values);
        // Lots of the box doesn't build at all: a cheap way to see that, and no solve.
        let v = match evaluate(&p, goal, SEARCH_MESH) {
            Ok(s) => {
                let v = penalty(&s, goal);
                let mut b = best.lock().unwrap();
                if b.as_ref().is_none_or(|q| v < q.0) {
                    *b = Some((v, x.to_vec(), s));
                }
                v
            }
            Err(_) => NO_GEOMETRY,
        };
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        on_best(n, best.lock().unwrap().as_ref().map_or(0.0, |q| q.2.worst_range_m));
        v
    };
    let wide = solves * 7 / 10;
    let population = (6 * names.len()).clamp(16, 80).min(wide.max(4));
    let opts = optimize::Options { method: Method::DifferentialEvolution, max_evals: wide.max(1), seed, population: Some(population), tol: 1e-3, parallel: ctl.parallel.max(1) };
    let _ = optimize::minimize(&f, &x0, &bounds, &opts, |_| !ctl.cancel.load(Ordering::Relaxed));
    let found = best.lock().unwrap().as_ref().map(|q| q.1.clone());
    if let Some(from) = found {
        let close: Vec<(f64, f64)> = from.iter().zip(&bounds).map(|(v, b)| {
            let reach = 0.2 * (b.1 - b.0);
            ((v - reach).max(b.0), (v + reach).min(b.1))
        }).collect();
        let opts = optimize::Options { method: Method::NelderMead, max_evals: (solves - wide).max(1), seed, population: None, tol: 1e-3, parallel: ctl.parallel.max(1) };
        let _ = optimize::minimize(&f, &from, &close, &opts, |_| !ctl.cancel.load(Ordering::Relaxed));
    }
    let out = best.into_inner().unwrap()?;
    Some((names.into_iter().zip(out.1).collect(), out.2))
}

/// The nearest things that could change around a design: more or less of what's touching it, and
/// each layer away. For a design that has to survive more than the exact stack it was tuned for.
pub fn neighbours(stack: &Stack) -> Vec<(String, Stack)> {
    let mut out = Vec::new();
    let mut scaled = |name: String, k: f64| {
        let mut s = stack.clone();
        for l in s.above.iter_mut().chain(&mut s.below) {
            l.thickness *= k;
        }
        (name, s)
    };
    if !stack.is_empty() {
        out.push(scaled("All layers half as thick".into(), 0.5));
        out.push(scaled("All layers 1.5 times as thick".into(), 1.5));
    }
    if !stack.above.is_empty() {
        let mut s = stack.clone();
        s.above.clear();
        out.push(("Nothing over it".to_string(), s));
    }
    if !stack.is_empty() {
        out.push(("Bare, in free space".to_string(), Stack::default()));
    }
    out
}

/// The range over a wide sweep, for the band it really covers.
fn band_of(p: &Project, goal: &TagGoal, gain: f64) -> Result<Option<(f64, f64)>> {
    let g = p.geometry()?;
    let fc = goal.fc();
    let lo = fc * 0.8;
    let hi = fc * 1.2;
    let problem = MomProblem::new(&g, hi, MomOptions { edges_per_lambda: REPORT_MESH.edges_per_lambda, theta_step: 30.0, phi_step: 30.0, ..Default::default() })?;
    let fs = crate::linspace(lo, hi, 17);
    let mut ranges = Vec::with_capacity(fs.len());
    for f in &fs {
        let z = problem.solve(*f, false)?.z[0][0];
        ranges.push(goal.range(*f, gain, power_transfer(z, chip_at(goal.chip_z, *f))));
    }
    let top = ranges.iter().cloned().fold(0.0, f64::max);
    let peak = ranges.iter().position(|r| *r == top).unwrap_or(0);
    let (mut a, mut b) = (peak, peak);
    while a > 0 && ranges[a - 1] >= 0.7 * top {
        a -= 1;
    }
    while b + 1 < fs.len() && ranges[b + 1] >= 0.7 * top {
        b += 1;
    }
    Ok((top > 0.0).then_some((fs[a], fs[b])))
}

/// Variables that count something: they round, and aren't lengths.
fn counts(name: &str) -> bool {
    matches!(name, "n" | "n1" | "n2" | "turns" | "order")
}

/// A design as someone could draw it and read it: a micrometre at most in a length, whole counts. The
/// variable table then says `94.732mm` and not `9.4732e-2`.
pub fn tidied(p: &Project, values: &[(String, f64)]) -> Project {
    let mut q = p.clone();
    for (name, v) in values {
        let Some(row) = q.params.iter_mut().find(|r| &r.name == name) else { continue };
        row.expr = if counts(name) {
            format!("{}", v.round())
        } else if name == "last" {
            format!("{}", (v * 1e3).round() / 1e3)
        } else if v.abs() < 2.0 {
            format!("{}mm", (v * 1e6).round() / 1e3)
        } else {
            format!("{v}")
        };
    }
    q
}

/// The dimensions someone reads: lengths in mm.
fn readable(c: &Candidate, values: &[(String, f64)]) -> Vec<(String, f64)> {
    values.iter().map(|(n, v)| {
        if counts(n) {
            return (n.clone(), v.round());
        }
        let length = c.vars.iter().find(|x| &x.0 == n).is_some_and(|x| x.2 < 2.0 && !matches!(n.as_str(), "last"));
        (if length { format!("{n}_mm") } else { n.clone() }, if length { (v * 1e6).round() / 1e3 } else { (v * 100.0).round() / 100.0 })
    }).collect()
}

/// A search on one mesh finds that mesh's best, and a plate's resonance moves as the mesh refines: a
/// slotted plate lost 42 % of its range going from the search's mesh to a finer one, while traces
/// barely moved. So the last steps are taken on the mesh that's reported. Keeps what it started with
/// unless it finds better.
pub fn polish(c: &Candidate, values: Vec<(String, f64)>, goal: &TagGoal, solves: usize, ctl: &Control) -> Vec<(String, f64)> {
    polish_on(c, values, goal, solves, ctl, REPORT_MESH)
}

fn polish_on(c: &Candidate, values: Vec<(String, f64)>, goal: &TagGoal, solves: usize, ctl: &Control, on: Mesh) -> Vec<(String, f64)> {
    if values.is_empty() || ctl.cancel.load(Ordering::Relaxed) {
        return values;
    }
    let mesh = Mesh { far_step: SEARCH_MESH.far_step, ..on };
    let names: Vec<String> = values.iter().map(|v| v.0.clone()).collect();
    let x0: Vec<f64> = values.iter().map(|v| v.1).collect();
    let bounds: Vec<(f64, f64)> = c.vars.iter().zip(&x0).map(|(v, x)| {
        let reach = 0.06 * (v.2 - v.1);
        ((x - reach).max(v.1), (x + reach).min(v.2))
    }).collect();
    let f = |x: &[f64]| -> f64 {
        if ctl.cancel.load(Ordering::Relaxed) {
            return NO_GEOMETRY;
        }
        let vals: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
        evaluate(&c.project.with_values(&vals), goal, mesh).map_or(NO_GEOMETRY, |s| penalty(&s, goal))
    };
    let start = f(&x0);
    // Too big for this mesh: what it has stands.
    if start >= NO_GEOMETRY {
        return values;
    }
    let opts = optimize::Options { method: Method::NelderMead, max_evals: solves, seed: 11, population: None, tol: 1e-3, parallel: ctl.parallel.max(1) };
    let out = optimize::minimize(f, &x0, &bounds, &opts, |_| !ctl.cancel.load(Ordering::Relaxed));
    if out.value < start { names.into_iter().zip(out.x).collect() } else { values }
}

/// Fine enough to cross a trace. Against a mesh well under the trace's width, a loop and radiator
/// read 212 ohm of reactance at 96 edges a wavelength, 212 at 192, 206 at 384 and 203 at 768, and a
/// 10 ohm error matters against a chip's 15 ohm. See `tag_study_trace_mesh`. Solid plates needn't go
/// this far, and past `limit` they don't: 192 has converged for them.
pub const FINAL_MESH: Mesh = Mesh { edges_per_lambda: 384.0, far_step: 5.0, limit: 7000 };
/// How many options get it, and how many solves each. They're seconds each, not fractions.
const FINAL_OPTIONS: usize = 4;
const FINAL_SOLVES: usize = 16;

/// The density to run an opened design at: as fine as the search finished on, if it isn't too big.
pub fn clean_mesh(p: &Project) -> f64 {
    let Ok(g) = p.geometry() else { return REPORT_MESH.edges_per_lambda };
    for edges in [FINAL_MESH.edges_per_lambda, 288.0] {
        if MomProblem::new(&g, p.setup.f_hi, MomOptions { edges_per_lambda: edges, ..Default::default() }).is_ok_and(|m| m.unknowns() <= FINAL_MESH.limit) {
            return edges;
        }
    }
    REPORT_MESH.edges_per_lambda
}

/// What a density is checked against, close enough to it to cost little.
fn check_of(mesh: Mesh) -> Mesh {
    Mesh { edges_per_lambda: mesh.edges_per_lambda * 0.75, far_step: 15.0, limit: mesh.limit }
}

/// A design at the reporting mesh, with what it's for and what surrounds it.
pub fn finish(c: &Candidate, values: Vec<(String, f64)>, goal: &TagGoal, id: String) -> Result<TagOption> {
    let project = tidied(&c.project, &values);
    let mut warnings = Vec::new();
    let score = match evaluate(&project, goal, REPORT_MESH) {
        Ok(s) => s,
        // Too big for the finer mesh isn't a reason to lose it, but it has to be said.
        Err(e) => {
            warnings.push(format!("Not checked on a finer mesh: {e:#}"));
            evaluate(&project, goal, Mesh { far_step: REPORT_MESH.far_step, ..SEARCH_MESH })?
        }
    };
    surround(c, values, project, score, goal, id, warnings, REPORT_MESH)
}

/// A design's last steps, on a mesh fine enough to cross its traces. `None` if it's too big for it,
/// in which case what it has stands.
fn finish_final(c: &Candidate, o: &TagOption, goal: &TagGoal, ctl: &Control) -> Option<TagOption> {
    let vars = o.project.vars().ok()?;
    let values: Vec<(String, f64)> = c.vars.iter().filter_map(|v| vars.get(&v.0).map(|x| (v.0.clone(), *x))).collect();
    let values = polish_on(c, values, goal, FINAL_SOLVES, ctl, FINAL_MESH);
    let project = tidied(&c.project, &values);
    let score = evaluate(&project, goal, FINAL_MESH).ok()?;
    surround(c, values, project, score, goal, o.id.clone(), Vec::new(), FINAL_MESH).ok()
}

/// What goes with a design's score: how settled its mesh is, what it does if its surroundings change,
/// and the band it covers.
#[allow(clippy::too_many_arguments)]
fn surround(c: &Candidate, values: Vec<(String, f64)>, project: Project, score: TagScore, goal: &TagGoal, id: String, mut warnings: Vec<String>, mesh: Mesh) -> Result<TagOption> {
    let check = check_of(mesh);
    let mesh_shift = evaluate(&project, goal, check).map_or(0.0, |other| if score.worst_range_m > 0.0 { (other.worst_range_m - score.worst_range_m).abs() / score.worst_range_m } else { 1.0 });
    if mesh_shift > 0.05 {
        warnings.push(format!("Range moves by {:.0} % between a mesh of {:.0} and {:.0} edges a wavelength. Run a mesh study before believing the figure.", mesh_shift * 100.0, check.edges_per_lambda, mesh.edges_per_lambda));
    }
    if score.efficiency < 0.5 {
        warnings.push(format!("Only {:.0} % of the power it accepts is radiated: the rest goes in its surroundings and metal.", score.efficiency * 100.0));
    }
    if goal.max_x.is_some_and(|m| score.size_x > m * 1.001) || goal.max_y.is_some_and(|m| score.size_y > m * 1.001) {
        warnings.push(format!("At {:.1} x {:.1} mm it doesn't fit the footprint.", score.size_x * 1e3, score.size_y * 1e3));
    }
    let mut if_changed = Vec::new();
    for (name, s) in neighbours(&goal.stack) {
        let other = TagGoal { stack: s, ..goal.clone() };
        let q = fitted(&project, &other);
        if let Ok(r) = evaluate(&q, &other, Mesh { far_step: SEARCH_MESH.far_step, ..REPORT_MESH }) {
            if_changed.push((name, r.worst_range_m));
        }
    }
    let band = band_of(&project, goal, score.gain_dbi).ok().flatten();
    Ok(TagOption { id, title: c.title.clone(), idea: c.idea.clone(), good_for: c.good_for.clone(), values: readable(c, &values), project, score, mesh_shift, if_changed, stands_out: Vec::new(), warnings, band })
}

/// Says what each option does best, so a list of options is a choice.
pub fn mark_out(options: &mut [TagOption], goal: &TagGoal) {
    if options.is_empty() {
        return;
    }
    // It can be called again as options are added.
    for o in options.iter_mut() {
        o.stands_out.clear();
        o.warnings.retain(|w| !w.starts_with("Far behind"));
    }
    // Smallest means nothing for a tag that hardly reads, so only real contenders can stand out.
    let longest = options.iter().map(|o| o.score.worst_range_m).fold(0.0, f64::max);
    for o in options.iter_mut().filter(|o| o.score.worst_range_m < 0.5 * longest) {
        o.warnings.push(format!("Far behind the best here, at {:.1} m against {longest:.1} m. It may suit another footprint or band, or need a deeper search.", o.score.worst_range_m));
    }
    let best_by = |list: &[TagOption], key: &dyn Fn(&TagOption) -> f64| list.iter().enumerate().filter(|(_, o)| o.score.worst_range_m >= 0.5 * longest).max_by(|a, b| key(a.1).total_cmp(&key(b.1))).map(|x| x.0);
    let area = |o: &TagOption| o.score.size_x * o.score.size_y;
    let survives = |o: &TagOption| o.if_changed.iter().map(|c| c.1).fold(o.score.worst_range_m, f64::min);
    let band = |o: &TagOption| o.band.map_or(0.0, |b| b.1 - b.0);
    for (i, words) in [
        (best_by(options, &|o| o.score.worst_range_m), format!("Longest read range across {:.0}-{:.0} MHz", goal.f_lo / 1e6, goal.f_hi / 1e6)),
        (best_by(options, &|o| -area(o)), "Smallest".to_string()),
        (best_by(options, &band), "Widest usable band".to_string()),
        (best_by(options, &survives), "Least bothered by changes around it".to_string()),
        (best_by(options, &|o| o.score.efficiency), "Radiates the most of what it accepts".to_string()),
    ] {
        if let Some(i) = i {
            options[i].stands_out.push(words);
        }
    }
}

/// Every structure asked for, or all of them, plus any the designer drew. Best first.
pub fn explore(goal: &TagGoal, templates: &[String], extra: Vec<Candidate>, effort: Effort, ctl: &Control) -> Result<Vec<TagOption>> {
    let mut list = Vec::new();
    for f in FAMILIES.iter().filter(|f| templates.is_empty() || templates.iter().any(|t| t == f.template)) {
        list.push(candidate(f, goal)?);
    }
    list.extend(extra);
    if list.is_empty() {
        bail!("Nothing to search: no structure called {}.", templates.join(", "));
    }
    // More dimensions need more solves: 240 was two generations for a structure with 16 of them.
    let budget = |c: &Candidate| (effort.solves() as f64 * (c.vars.len() as f64 / 8.0).max(1.0)) as usize;
    let total: usize = list.iter().map(budget).sum();
    let spent = AtomicUsize::new(0);
    let mut options = Vec::new();
    for (k, c) in list.iter().enumerate() {
        if ctl.cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }
        let solves = budget(c);
        let before = spent.load(Ordering::Relaxed);
        let on_best = |n: usize, best: f64| {
            let so_far = before + n;
            spent.fetch_max(so_far, Ordering::Relaxed);
            (ctl.report)(Progress { message: format!("{} of {}: {}", k + 1, list.len(), c.title), done: so_far.min(total), total, best_range_m: best });
        };
        let Some((values, coarse)) = search_one(c, goal, solves, ctl, 17 + k as u64, &on_best) else { continue };
        (ctl.report)(Progress { message: format!("Finishing {} on a finer mesh", c.title), done: before + solves, total, best_range_m: coarse.worst_range_m });
        spent.store(before + solves, Ordering::Relaxed);
        let values = polish(c, values, goal, (solves / 6).clamp(10, 60), ctl);
        if let Ok(o) = finish(c, values, goal, format!("option_{}", k + 1)) {
            options.push(o);
        }
    }
    options.sort_by(|a, b| b.score.worst_range_m.total_cmp(&a.score.worst_range_m));
    // The ones in contention get their last steps where a trace is actually resolved. A quick look
    // doesn't wait for that, and it says so.
    if effort == Effort::Quick {
        for o in &mut options {
            o.warnings.push(format!("A quick search stops at {:.0} edges a wavelength, where a trace's reactance still reads a few percent high: a normal search finishes finer.", REPORT_MESH.edges_per_lambda));
        }
    } else {
        for o in options.iter_mut().take(FINAL_OPTIONS) {
            if ctl.cancel.load(Ordering::Relaxed) {
                break;
            }
            let Some(c) = list.iter().find(|c| c.title == o.title) else { continue };
            (ctl.report)(Progress { message: format!("Final tuning of {}", c.title), done: total, total, best_range_m: o.score.worst_range_m });
            if let Some(n) = finish_final(c, o, goal, ctl) {
                *o = n;
            }
        }
        options.sort_by(|a, b| b.score.worst_range_m.total_cmp(&a.score.worst_range_m));
    }
    for (k, o) in options.iter_mut().enumerate() {
        o.id = format!("option_{}", k + 1);
    }
    mark_out(&mut options, goal);
    Ok(options)
}

/// What a change of surroundings does to a design, and what a small retune gets back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Retune {
    pub before: TagScore,
    /// The same dimensions in the new surroundings.
    pub detuned: TagScore,
    /// After the small changes.
    pub after: TagScore,
    pub project: Project,
    /// `(variable, from, to)`.
    pub changes: Vec<(String, f64, f64)>,
    /// The resonance's frequency against what the chip needs, before and in the new stack, Hz.
    pub note: String,
}

/// The variables a retune may touch: this design's own lengths, not its band or the process's limits.
fn tunable(p: &Project, reach: f64) -> Vec<(String, f64, f64)> {
    let Ok(v) = p.vars() else { return Vec::new() };
    let skip = |n: &str| matches!(n, "f0" | "lambda0" | "Z0" | "n" | "turns" | "order" | "g" | "w" | "wl" | "wt");
    p.params.iter().filter(|r| !r.derived && !skip(&r.name)).filter_map(|r| {
        let x = *v.get(&r.name)?;
        (x > 0.0 && x < 2.0).then(|| (r.name.clone(), x * (1.0 - reach), x * (1.0 + reach)))
    }).collect()
}

/// Puts a design in new surroundings, and tunes it back with changes of at most `reach` of each
/// dimension: "it had to modify a bit". It's the same structure afterwards.
pub fn retune(p: &Project, goal: &TagGoal, old_stack: &Stack, reach: f64, solves: usize, ctl: &Control) -> Result<Retune> {
    // Its numbers are compared with each other, on a mesh that resolves the change: paper moves a tag
    // by about what the coarser one gets wrong.
    const FINE: Mesh = Mesh { far_step: SEARCH_MESH.far_step, ..REPORT_MESH };
    let was = TagGoal { stack: old_stack.clone(), ..goal.clone() };
    let before = evaluate(&fitted(p, &was), &was, FINE)?;
    let moved = fitted(p, goal);
    let detuned = evaluate(&moved, goal, FINE)?;
    let vars = tunable(&moved, reach);
    if vars.is_empty() {
        bail!("This design has no dimensions to tune.");
    }
    let names: Vec<String> = vars.iter().map(|v| v.0.clone()).collect();
    let bounds: Vec<(f64, f64)> = vars.iter().map(|v| (v.1, v.2)).collect();
    let v0 = moved.vars()?;
    let x0: Vec<f64> = names.iter().map(|n| v0[n]).collect();
    let done = AtomicUsize::new(0);
    let f = |x: &[f64]| -> f64 {
        if ctl.cancel.load(Ordering::Relaxed) {
            return NO_GEOMETRY;
        }
        let values: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        (ctl.report)(Progress { message: "Retuning".into(), done: n.min(solves), total: solves, best_range_m: 0.0 });
        evaluate(&moved.with_values(&values), goal, FINE).map_or(NO_GEOMETRY, |s| penalty(&s, goal))
    };
    let opts = optimize::Options { method: Method::Surrogate, max_evals: solves, seed: 5, population: None, tol: 1e-3, parallel: ctl.parallel.max(1) };
    let out = optimize::minimize(f, &x0, &bounds, &opts, |_| !ctl.cancel.load(Ordering::Relaxed));
    let values: Vec<(String, f64)> = names.iter().cloned().zip(out.x.iter().copied()).collect();
    let project = tidied(&moved, &values);
    let after = evaluate(&project, goal, FINE)?;
    let changes: Vec<(String, f64, f64)> = values.iter().zip(&x0).filter(|((_, to), from)| (to - **from).abs() > 1e-6 * from.abs().max(1e-6)).map(|((n, to), from)| (n.clone(), *from, *to)).collect();
    let note = format!("Worst-case range {:.1} m as designed, {:.1} m in the new surroundings, {:.1} m retuned", before.worst_range_m, detuned.worst_range_m, after.worst_range_m);
    Ok(Retune { before, detuned, after, project, changes, note })
}

// ---------------------------------------------------------------------------
// Free-form: metal laid out cell by cell, and evolved

/// A grid of metal over one half of the footprint, mirrored in x, with the chip's feed loop near the
/// bottom edge and the only thing assumed. Metal in the first column joins its mirror image across the
/// centre, so a radiator can pass over the loop and couple to it, however else it wanders. The loop's
/// size evolves with the rest, since it's what sets the reactance.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub nx: usize,
    pub ny: usize,
    pub cell_x: f64,
    pub cell_y: f64,
    /// `nx × ny`, row by row.
    pub on: Vec<bool>,
    pub loop_a: f64,
    pub loop_b: f64,
    pub trace: f64,
    pub gap: f64,
}

impl Layout {
    fn at(&self, x: usize, y: usize) -> bool {
        self.on[y * self.nx + x]
    }

    fn bottom(&self) -> f64 {
        -(self.ny as f64) * self.cell_y / 2.0
    }

    /// Where the loop's top line runs.
    fn loop_top(&self) -> f64 {
        self.bottom() + self.trace / 2.0 + self.loop_b
    }

    /// A cell on the loop or within a trace's width of it can't have metal.
    fn clear(&self, x: usize, y: usize) -> bool {
        let reach = 1.5 * self.trace;
        !(((x as f64) * self.cell_x) < self.loop_a / 2.0 + reach && self.bottom() + (y as f64) * self.cell_y < self.loop_top() + reach)
    }

    /// The first row a radiator can run along over the loop.
    fn first_row(&self) -> usize {
        (0..self.ny).find(|y| self.clear(0, *y)).unwrap_or(self.ny - 1)
    }

    /// As an ordinary project: parts you could edit, and a chip feed.
    pub fn project(&self, goal: &TagGoal) -> Result<Project> {
        let mut p = Project::from_template("custom", Some(&format!("{}MHz", goal.fc() / 1e6)))?;
        p.name = "Evolved tag".into();
        p.fit_band(goal.f_lo, goal.f_hi);
        p = fitted(&p, goal);
        if !p.params.iter().any(|r| r.name == "Z0") {
            p.params.push(Param::new("Z0", "50", "Port impedance. It doesn't matter here: a tag is judged against its chip"));
        }
        let num = |v: f64| format!("{v:e}");
        let mut block = |name: String, x0: f64, y0: f64, x1: f64, y1: f64| {
            let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].map(|q| [num(q[0]), num(q[1])]).to_vec();
            p.extra.push(ExtraSolid { name, material: goal.metal.clone(), prim: ExtraPrim::Polygon { axis: 2, at: "0".into(), pts: corners, thick: "0".into() } });
        };
        let bottom = self.bottom();
        // Runs along each row, so there are fewer pieces to mesh, on both sides.
        let mut k = 0;
        for y in 0..self.ny {
            let mut x = 0;
            while x < self.nx {
                if !self.at(x, y) || !self.clear(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.nx && self.at(x, y) && self.clear(x, y) {
                    x += 1;
                }
                let (a, b) = (start as f64 * self.cell_x, x as f64 * self.cell_x);
                let (c, d) = (bottom + y as f64 * self.cell_y, bottom + (y + 1) as f64 * self.cell_y);
                if start == 0 {
                    block(format!("metal {k}"), -b, c, b, d);
                } else {
                    block(format!("metal {k} +"), a, c, b, d);
                    block(format!("metal {k} -"), -b, c, -a, d);
                }
                k += 1;
            }
        }
        // The loop, with the chip in its gap: the one part that's always there.
        let (a, t, g) = (self.loop_a / 2.0, self.trace / 2.0, self.gap / 2.0);
        let (foot, top) = (bottom + t, self.loop_top());
        block("loop top".into(), -a - t, top - t, a + t, top + t);
        block("loop side +".into(), a - t, foot - t, a + t, top + t);
        block("loop side -".into(), -a - t, foot - t, -a + t, top + t);
        block("loop foot +".into(), g, foot - t, a + t, foot + t);
        block("loop foot -".into(), -a - t, foot - t, -g, foot + t);
        p.extra_ports.push(ExtraPort { name: "chip".into(), a: [num(-g), num(foot), "0".into()], b: [num(g), num(foot), "0".into()], width: num(self.trace), z0: "Z0".into() });
        Ok(p)
    }

    /// Metal not joined to the centre column carries no current to speak of, costs unknowns and is
    /// hard to print: dropped.
    fn prune(&mut self) {
        let (nx, ny) = (self.nx, self.ny);
        let mut keep = vec![false; self.on.len()];
        let usable = |l: &Layout, x: usize, y: usize| l.at(x, y) && l.clear(x, y);
        let mut stack: Vec<(usize, usize)> = (0..ny).filter(|y| usable(self, 0, *y)).map(|y| (0, y)).collect();
        while let Some((x, y)) = stack.pop() {
            if keep[y * nx + x] {
                continue;
            }
            keep[y * nx + x] = true;
            if x > 0 && usable(self, x - 1, y) {
                stack.push((x - 1, y));
            }
            if x + 1 < nx && usable(self, x + 1, y) {
                stack.push((x + 1, y));
            }
            if y > 0 && usable(self, x, y - 1) {
                stack.push((x, y - 1));
            }
            if y + 1 < ny && usable(self, x, y + 1) {
                stack.push((x, y + 1));
            }
        }
        self.on = keep;
    }
}

fn mutate(l: &Layout, rng: &mut Rng) -> Layout {
    let mut c = l.clone();
    let (nx, ny) = (c.nx, c.ny);
    let height = ny as f64 * c.cell_y;
    let width = 2.0 * nx as f64 * c.cell_x;
    match rng.below(7) {
        0 => {
            // A few cells.
            for _ in 0..(1 + rng.below(4)) {
                let i = rng.below(c.on.len());
                c.on[i] = !c.on[i];
            }
        }
        1 => {
            // A block on or off.
            let (w, h) = (1 + rng.below(nx.div_ceil(3).max(1)), 1 + rng.below(ny.div_ceil(2).max(1)));
            let (x0, y0) = (rng.below(nx), rng.below(ny));
            let on = rng.uniform() < 0.5;
            for y in y0..(y0 + h).min(ny) {
                for x in x0..(x0 + w).min(nx) {
                    c.on[y * nx + x] = on;
                }
            }
        }
        2 => {
            // A line along x.
            let (y, x0) = (rng.below(ny), rng.below(nx));
            let on = rng.uniform() < 0.6;
            for x in x0..(x0 + 1 + rng.below(nx)).min(nx) {
                c.on[y * nx + x] = on;
            }
        }
        3 => {
            // A line along y.
            let (x, y0) = (rng.below(nx), rng.below(ny));
            let on = rng.uniform() < 0.6;
            for y in y0..(y0 + 1 + rng.below(ny)).min(ny) {
                c.on[y * nx + x] = on;
            }
        }
        4 => {
            // Slide a column, so a meander's position can move without being rebuilt.
            let x = rng.below(nx);
            let col: Vec<bool> = (0..ny).map(|y| c.at(x, y)).collect();
            for (y, v) in col.iter().enumerate() {
                c.on[((y + 1) % ny) * nx + x] = *v;
            }
        }
        // The loop, which sets the reactance.
        _ => {
            c.loop_a = (c.loop_a * (1.0 + 0.1 * rng.gauss())).clamp(6.0 * c.trace, 0.6 * width);
            c.loop_b = (c.loop_b * (1.0 + 0.1 * rng.gauss())).clamp(4.0 * c.trace, 0.75 * height);
        }
    }
    c
}

/// One child from two parents: columns from one, the rest from the other, the loop from either.
fn cross(a: &Layout, b: &Layout, rng: &mut Rng) -> Layout {
    let cut = rng.below(a.nx);
    let mut c = a.clone();
    for y in 0..a.ny {
        for x in cut..a.nx {
            c.on[y * a.nx + x] = b.on[y * a.nx + x];
        }
    }
    if rng.uniform() < 0.5 {
        c.loop_a = b.loop_a;
        c.loop_b = b.loop_b;
    }
    c
}

/// A starting layout: a line over the loop out to some length, then wandering, sometimes with a plate
/// at its end. Whatever else it is, it's a radiator that couples to the loop.
fn seed_layout(base: &Layout, rng: &mut Rng) -> Layout {
    let mut c = base.clone();
    c.on.iter_mut().for_each(|v| *v = false);
    let (nx, ny) = (c.nx, c.ny);
    let mut y = c.first_row();
    let straight = nx / 3 + rng.below(nx - nx / 3);
    for x in 0..nx {
        c.on[y * nx + x] = true;
        if x >= straight && rng.uniform() < 0.5 {
            let to = c.first_row() + rng.below(ny - c.first_row());
            let (a, b) = (y.min(to), y.max(to));
            for row in a..=b {
                c.on[row * nx + x] = true;
            }
            y = to;
        }
    }
    if rng.uniform() < 0.5 {
        let from = nx - 1 - rng.below(nx / 3 + 1);
        for row in c.first_row()..ny {
            for x in from..nx {
                c.on[row * nx + x] = true;
            }
        }
    }
    c
}

/// Evolves a metal layout, with no idea of what shape it ought to be beyond a loop for the chip.
/// Results print on any process that can make a cell, but they aren't guaranteed to beat a good
/// structure, and more solves is what improves them.
pub fn evolve(goal: &TagGoal, nx: usize, ny: usize, solves: usize, ctl: &Control) -> Result<TagOption> {
    if nx < 3 || ny < 2 {
        bail!("A grid needs at least 3 by 2 cells.");
    }
    let lambda = goal.lambda();
    let width = goal.max_x.unwrap_or(0.3 * lambda).max(0.05 * lambda);
    let height = goal.max_y.unwrap_or(0.08 * lambda).max(0.02 * lambda);
    let trace = goal.min_feature.max(lambda / 400.0);
    let base = Layout { nx, ny, cell_x: width / 2.0 / nx as f64, cell_y: height / ny as f64, on: vec![false; nx * ny], loop_a: (0.2 * width).max(6.0 * trace), loop_b: (0.45 * height).max(4.0 * trace), trace, gap: trace };
    if !(base.cell_x >= goal.min_feature && base.cell_y >= goal.min_feature) {
        bail!("This grid's cells are smaller than the process can make: use fewer cells.");
    }
    let mut rng = Rng::new(31);
    const POP: usize = 24;
    let score_of = |l: &Layout| -> f64 {
        if ctl.cancel.load(Ordering::Relaxed) {
            return NO_GEOMETRY;
        }
        let mut l = l.clone();
        l.prune();
        l.project(goal).and_then(|p| evaluate(&p, goal, SEARCH_MESH)).map_or(NO_GEOMETRY, |s| penalty(&s, goal))
    };
    let scoring = |list: Vec<Layout>| -> Vec<(f64, Layout)> {
        let next = AtomicUsize::new(0);
        let results: Vec<Mutex<Option<(f64, Layout)>>> = list.iter().map(|_| Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..ctl.parallel.max(1).min(list.len().max(1)) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(l) = list.get(i) else { break };
                        *results[i].lock().unwrap() = Some((score_of(l), l.clone()));
                    }
                });
            }
        });
        results.into_iter().filter_map(|m| m.into_inner().unwrap()).collect()
    };
    let mut pop = scoring((0..POP).map(|_| seed_layout(&base, &mut rng)).collect());
    pop.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut spent = POP;
    while spent < solves && !ctl.cancel.load(Ordering::Relaxed) {
        let parents = POP / 3;
        let mut children = Vec::with_capacity(POP);
        for _ in 0..POP {
            let a = &pop[rng.below(parents)].1;
            let b = &pop[rng.below(pop.len())].1;
            let child = if rng.uniform() < 0.3 { cross(a, b, &mut rng) } else { a.clone() };
            children.push(mutate(&child, &mut rng));
        }
        spent += children.len();
        pop.extend(scoring(children));
        pop.sort_by(|a, b| a.0.total_cmp(&b.0));
        pop.truncate(POP);
        (ctl.report)(Progress { message: "Evolving a free-form layout".into(), done: spent.min(solves), total: solves, best_range_m: -pop[0].0 / 1.1 });
    }
    let mut best = pop.swap_remove(0).1;
    best.prune();
    let project = best.project(goal)?;
    let c = Candidate { title: "Evolved free-form layout".into(), idea: format!("Metal laid out cell by cell on a {nx} by {ny} grid and evolved over {spent} solves, with only a loop for the chip assumed."), good_for: "A shape nobody would have drawn, where the area is odd or a known structure doesn't fit".into(), project, vars: Vec::new() };
    finish(&c, Vec::new(), goal, "evolved".into())
}

/// A sensible goal for a band and chip, for tests and defaults.
pub fn goal_for(f_lo: f64, f_hi: f64, chip_z: (f64, f64), read_dbm: f64) -> TagGoal {
    TagGoal { f_lo, f_hi, chip_z, read_dbm, eirp_dbm: 36.0, max_x: None, max_y: None, min_feature: 200e-6, stack: Stack::default(), metal: "Aluminium".into() }
}

/// The antenna's impedance over a sweep, for a report.
pub fn impedances(p: &Project, freqs: &[f64], edges: f64) -> Result<Vec<C64>> {
    let g = p.geometry()?;
    let top = freqs.iter().cloned().fold(0.0, f64::max);
    let m = MomProblem::new(&g, top, MomOptions { edges_per_lambda: edges, ..Default::default() })?;
    freqs.iter().map(|f| Ok(m.solve(*f, false)?.z[0][0])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack::preset;

    fn ucode9(max_x: f64, max_y: f64) -> TagGoal {
        TagGoal { max_x: Some(max_x), max_y: Some(max_y), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) }
    }

    fn quiet() -> (AtomicBool, impl Fn(Progress) + Sync) {
        (AtomicBool::new(false), |_p: Progress| {})
    }

    #[test]
    fn every_family_can_start_at_its_template_with_its_variables() {
        let goal = ucode9(0.1, 0.03);
        for f in &FAMILIES {
            let c = candidate(f, &goal).unwrap_or_else(|e| panic!("{}: {e:#}", f.template));
            assert!(c.vars.iter().all(|v| v.2 > v.1), "{}: {:?}", f.template, c.vars);
            assert_eq!(c.project.stack, goal.stack);
        }
    }

    #[test]
    fn footprint_counts_and_the_process_limit_binds_the_thinnest_trace() {
        let goal = TagGoal { min_feature: 500e-6, ..ucode9(0.05, 0.02) };
        let c = candidate(family("rfid_tag").unwrap(), &goal).unwrap();
        let lx = c.vars.iter().find(|v| v.0 == "Lx").unwrap();
        assert!(lx.2 <= 0.05 + 1e-9, "{lx:?}");
        let w = c.vars.iter().find(|v| v.0 == "w").unwrap();
        assert!((w.1 - 500e-6).abs() < 1e-12, "{w:?}");
        let s = TagScore { size_x: 0.1, size_y: 0.01, worst_range_m: 5.0, ..Default::default() };
        let ok = TagScore { size_x: 0.05, ..s.clone() };
        assert!(penalty(&s, &goal) > penalty(&ok, &goal) + 10.0, "leaving the footprint has to cost more than it gains");
    }

    #[test]
    fn neighbours_go_both_ways_and_end_in_free_space() {
        let s = (preset("paper_label").unwrap().stack)();
        let n = neighbours(&s);
        assert_eq!(n.len(), 4);
        assert!(n.iter().any(|(_, x)| x.above.is_empty() && !x.below.is_empty()));
        assert!(n.iter().any(|(_, x)| x.is_empty()));
        assert!(neighbours(&Stack::default()).is_empty());
    }

    #[test]
    fn a_found_design_reads_like_a_drawing_and_stays_the_same_design() {
        let goal = ucode9(0.095, 0.03);
        let c = candidate(family("tag_loop").unwrap(), &goal).unwrap();
        let values = vec![("Lx".to_string(), 0.09473216308), ("n".to_string(), 2.0034), ("gc".to_string(), 0.00203817)];
        let raw = c.project.with_values(&values);
        let tidy = tidied(&c.project, &values);
        let row = |p: &Project, n: &str| p.params.iter().find(|r| r.name == n).unwrap().expr.clone();
        assert_eq!(row(&tidy, "Lx"), "94.732mm");
        assert_eq!(row(&tidy, "n"), "2");
        assert_eq!(row(&tidy, "gc"), "2.038mm");
        let (a, b) = (evaluate(&raw, &goal, SEARCH_MESH).unwrap(), evaluate(&tidy, &goal, SEARCH_MESH).unwrap());
        assert!((a.center_range_m - b.center_range_m).abs() < 0.02 * a.center_range_m, "{a:?} {b:?}");
    }

    #[test]
    fn effort_reads_from_a_word() {
        assert_eq!(Effort::parse("deep"), Some(Effort::Deep));
        assert_eq!(Effort::parse("nope"), None);
        assert!(Effort::Quick.solves() < Effort::Exhaustive.solves());
    }

    #[test]
    fn a_layout_builds_a_project_with_its_feed_and_mirrored_metal() {
        let goal = ucode9(0.1, 0.02);
        let mut l = Layout { nx: 8, ny: 4, cell_x: 0.004, cell_y: 0.005, on: vec![false; 32], loop_a: 0.012, loop_b: 0.006, trace: 0.001, gap: 0.001 };
        // A row right across, clear of the loop.
        for x in 0..8 {
            l.on[2 * 8 + x] = true;
        }
        l.on[0] = true; // On the loop, so dropped.
        l.on[7] = true; // Joined to nothing, so dropped.
        l.prune();
        assert!(!l.on[7] && !l.on[0], "{:?}", l.on);
        assert!((0..8).all(|x| l.on[2 * 8 + x]), "{:?}", l.on);
        assert_eq!(l.first_row(), 2);
        let p = l.project(&goal).unwrap();
        let g = p.geometry().unwrap();
        assert_eq!(g.ports.len(), 1);
        let (lo, hi) = g.bounds();
        assert!((lo[0] + hi[0]).abs() < 1e-9, "mirrored in x: {lo:?} {hi:?}");
        // It runs right over the centre, which is what lets current through to both sides.
        let across = g.solids.iter().any(|s| matches!(&s.shape, crate::geometry::Shape::Polygon { pts, .. } if pts.iter().any(|q| q[0] < -0.01) && pts.iter().any(|q| q[0] > 0.01)));
        assert!(across, "{:?}", g.solids.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
        let s = evaluate(&p, &goal, Mesh { edges_per_lambda: 16.0, far_step: 30.0, limit: 2500 }).unwrap();
        assert!(s.worst_range_m.is_finite() && s.unknowns > 10, "{s:?}");
    }

    #[test]
    fn evolution_keeps_every_child_on_the_grid() {
        let base = Layout { nx: 6, ny: 3, cell_x: 0.004, cell_y: 0.005, on: vec![false; 18], loop_a: 0.012, loop_b: 0.008, trace: 0.001, gap: 0.001 };
        let mut rng = Rng::new(3);
        let mut c = seed_layout(&base, &mut rng);
        assert!(c.on.iter().any(|v| *v));
        for _ in 0..200 {
            c = mutate(&c, &mut rng);
            assert_eq!(c.on.len(), 18);
            let d = cross(&c, &base, &mut rng);
            assert_eq!(d.on.len(), 18);
        }
    }

    /// A short real search at a coarse budget: it has to beat where it started, on the band, and stay
    /// in the footprint. Ten solves a family would be noise; this one only needs to go the right way.
    #[test]
    fn a_short_search_improves_on_its_starting_point() {
        let goal = ucode9(0.095, 0.03);
        let c = candidate(family("tag_loop").unwrap(), &goal).unwrap();
        let start = evaluate(&c.project, &goal, SEARCH_MESH).unwrap();
        let (cancel, report) = quiet();
        let ctl = Control { cancel: &cancel, report: &report, parallel: 4 };
        let (values, best) = search_one(&c, &goal, 48, &ctl, 3, &|_, _| {}).expect("something fits");
        println!("start {:.1} m, found {:.1} m, {:?}", start.worst_range_m, best.worst_range_m, values);
        assert!(best.worst_range_m > start.worst_range_m, "{best:?} vs {start:?}");
        assert!(best.size_x <= 0.095 * 1.02, "{best:?}");
    }
}

/// Real searches, long enough to see what the structures can do. `cargo test -p antenna_core --release
/// tag_study -- --ignored --nocapture --test-threads=1`
#[cfg(test)]
mod studies {
    use super::*;
    use crate::stack::preset;

    fn goal() -> TagGoal {
        TagGoal { max_x: Some(0.095), max_y: Some(0.025), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) }
    }

    fn solve_rate(goal: &TagGoal) {
        for f in &FAMILIES {
            let c = candidate(f, goal).unwrap();
            let t = std::time::Instant::now();
            match evaluate(&c.project, goal, SEARCH_MESH) {
                Ok(s) => println!("{:<14} {:>6} unknowns, {:.2} s, {:.1} m worst, Z {:.1} {:+.1}j, t {:.2}, {:.1} x {:.1} mm", f.template, s.unknowns, t.elapsed().as_secs_f64(), s.worst_range_m, s.z_center.0, s.z_center.1, s.worst_transfer, s.size_x * 1e3, s.size_y * 1e3),
                Err(e) => println!("{:<14} {e:#}", f.template),
            }
        }
    }

    #[test]
    #[ignore]
    fn tag_study_solve_rate() {
        solve_rate(&goal());
        solve_rate(&TagGoal { stack: (preset("paper_label").unwrap().stack)(), ..goal() });
    }

    /// How fine a mesh each structure needs, under paper: impedance and range against density.
    #[test]
    #[ignore]
    fn tag_study_mesh() {
        let goal = TagGoal { stack: (preset("paper_label").unwrap().stack)(), max_x: None, max_y: None, ..goal() };
        for f in &FAMILIES {
            let c = candidate(f, &goal).unwrap();
            for edges in [24.0, 48.0, 96.0, 192.0, 384.0] {
                let t = std::time::Instant::now();
                match evaluate(&c.project, &goal, Mesh { edges_per_lambda: edges, far_step: 15.0, limit: 20000 }) {
                    Ok(s) => println!("{:<14} {edges:>4}/λ {:>5} unk {:>6.2} s  Z {:>6.1} {:>+7.1}j  t {:.2}  gain {:>5.2} eff {:.2}  range {:>5.1} m", f.template, s.unknowns, t.elapsed().as_secs_f64(), s.z_center.0, s.z_center.1, s.center_transfer, s.gain_dbi, s.efficiency, s.center_range_m),
                    Err(e) => println!("{:<14} {edges:>4}/λ {e:#}", f.template),
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn tag_study_explore() {
        let goal = TagGoal { stack: (preset("paper_label").unwrap().stack)(), ..goal() };
        let cancel = AtomicBool::new(false);
        let report = |p: Progress| {
            if p.done % 40 == 0 {
                println!("{} {}/{} best {:.1} m", p.message, p.done, p.total, p.best_range_m);
            }
        };
        let ctl = Control { cancel: &cancel, report: &report, parallel: 8 };
        let t = std::time::Instant::now();
        let list = explore(&goal, &[], Vec::new(), Effort::Normal, &ctl).unwrap();
        println!("{:.0} s", t.elapsed().as_secs_f64());
        for o in &list {
            println!("{} {:<24} worst {:.1} m, centre {:.1} m, t {:.2}, {:.2} dBi, eff {:.2}, {:.1} x {:.1} mm, Z {:.1} {:+.1}j, band {:?}, mesh shift {:.2}, {:?}, {:?}, {:?}", o.id, o.title, o.score.worst_range_m, o.score.center_range_m, o.score.worst_transfer, o.score.gain_dbi, o.score.efficiency, o.score.size_x * 1e3, o.score.size_y * 1e3, o.score.z_center.0, o.score.z_center.1, o.band.map(|b| (b.0 / 1e6, b.1 / 1e6)), o.mesh_shift, o.stands_out, o.if_changed, o.warnings);
            println!("    {:?}", o.values);
        }
    }

    /// The user's own example: a design tuned bare, then put under paper.
    #[test]
    #[ignore]
    fn tag_study_under_paper() {
        let bare = TagGoal { stack: (preset("free_space").unwrap().stack)(), ..goal() };
        let cancel = AtomicBool::new(false);
        let report = |_p: Progress| {};
        let ctl = Control { cancel: &cancel, report: &report, parallel: 8 };
        let c = candidate(family("tag_loop").unwrap(), &bare).unwrap();
        let (values, _) = search_one(&c, &bare, 200, &ctl, 7, &|_, _| {}).unwrap();
        let tuned = c.project.with_values(&values);
        for id in ["paper_label", "thick_paper_label", "paper_on_carton", "paper_on_glass", "paper_on_plastic", "on_water"] {
            let goal = TagGoal { stack: (preset(id).unwrap().stack)(), ..bare.clone() };
            let r = retune(&tuned, &goal, &bare.stack, 0.2, 100, &ctl).unwrap();
            println!("{id:<20} {} | Z {:.1} {:+.1}j -> {:.1} {:+.1}j | {:?}", r.note, r.detuned.z_center.0, r.detuned.z_center.1, r.after.z_center.0, r.after.z_center.1, r.changes.iter().map(|c| format!("{} {:+.1} mm", c.0, (c.2 - c.1) * 1e3)).collect::<Vec<_>>());
        }
    }

    /// Different jobs should come out differently: wide bands, a square label, glass, metal, water.
    #[test]
    #[ignore]
    fn tag_study_scenarios() {
        let cases: [(&str, f64, f64, f64, f64, &str); 6] = [
            ("global band, long label", 860e6, 960e6, 0.095, 0.03, "paper_label"),
            ("EU band, long label", 865e6, 868e6, 0.095, 0.025, "paper_label"),
            ("square label", 902e6, 928e6, 0.05, 0.05, "paper_label"),
            ("on glass", 902e6, 928e6, 0.095, 0.025, "paper_on_glass"),
            ("on foam over metal", 902e6, 928e6, 0.095, 0.05, "foam_on_metal"),
            ("on a bottle of water", 902e6, 928e6, 0.095, 0.05, "on_water"),
        ];
        let cancel = AtomicBool::new(false);
        let report = |_p: Progress| {};
        let ctl = Control { cancel: &cancel, report: &report, parallel: 8 };
        for (name, lo, hi, x, y, stack) in cases {
            let goal = TagGoal { max_x: Some(x), max_y: Some(y), stack: (preset(stack).unwrap().stack)(), ..goal_for(lo, hi, (15.0, -231.0), -24.0) };
            let t = std::time::Instant::now();
            let list = explore(&goal, &[], Vec::new(), Effort::Normal, &ctl).unwrap();
            println!("== {name}: {:.0} s", t.elapsed().as_secs_f64());
            for o in list.iter().take(4) {
                println!("{:<26} worst {:>5.1} m, centre {:>5.1} m, t {:.2}, {:>5.2} dBi, eff {:.2}, {:>5.1} x {:>4.1} mm, Z {:.1} {:+.1}j {:?} {:?}", o.title, o.score.worst_range_m, o.score.center_range_m, o.score.worst_transfer, o.score.gain_dbi, o.score.efficiency, o.score.size_x * 1e3, o.score.size_y * 1e3, o.score.z_center.0, o.score.z_center.1, o.stands_out, o.warnings);
            }
        }
    }

    #[test]
    #[ignore]
    fn tag_study_evolve() {
        let goal = TagGoal { stack: (preset("paper_label").unwrap().stack)(), ..goal() };
        let cancel = AtomicBool::new(false);
        let report = |p: Progress| println!("{} {}/{} best {:.1} m", p.message, p.done, p.total, p.best_range_m);
        let ctl = Control { cancel: &cancel, report: &report, parallel: 8 };
        let o = evolve(&goal, 12, 6, 480, &ctl).unwrap();
        println!("{:?} {:?} {:?}", o.score, o.if_changed, o.warnings);
    }
}

/// Finds what makes a structure's mesh run away. Prints each try before it starts, so the last line
/// is the culprit. `cargo test -p antenna_core --release tag_study_runaway -- --ignored --nocapture`
#[cfg(test)]
mod runaway {
    use super::*;
    use crate::stack::preset;

    #[test]
    #[ignore]
    fn tag_study_runaway() {
        use std::io::Write;
        let goal = TagGoal { max_x: Some(0.095), max_y: Some(0.025), stack: (preset("paper_label").unwrap().stack)(), ..goal_for(902e6, 928e6, (15.0, -231.0), -24.0) };
        let id = std::env::var("FAMILY").unwrap_or_else(|_| "tag_fractal".into());
        let c = candidate(family(&id).unwrap(), &goal).unwrap();
        let mut rng = Rng::new(99);
        for k in 0..400 {
            let values: Vec<(String, f64)> = c.vars.iter().map(|v| (v.0.clone(), v.1 + rng.uniform() * (v.2 - v.1))).collect();
            let p = c.project.with_values(&values);
            print!("{k} {:?} ", values.iter().map(|v| format!("{}={:.5}", v.0, v.1)).collect::<Vec<_>>());
            std::io::stdout().flush().unwrap();
            let t = std::time::Instant::now();
            let r = p.geometry().and_then(|g| MomProblem::new(&g, 928e6, MomOptions { edges_per_lambda: SEARCH_MESH.edges_per_lambda, ..Default::default() }).map(|m| m.unknowns()));
            println!("{:.2} s {:?}", t.elapsed().as_secs_f64(), r.map_err(|e| format!("{e:#}").chars().take(60).collect::<String>()));
        }
    }
}

/// Whether a thin trace's reactance has really converged, with the mesh going well under the trace's
/// width, where the current can crowd to its edges as it does in metal. `cargo test -p antenna_core
/// --release tag_study_trace_mesh -- --ignored --nocapture --test-threads=1`
#[cfg(test)]
mod trace_mesh {
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::Project;

    fn z_at(p: &Project, edges: f64) -> Option<(usize, crate::C64, f64)> {
        let g = p.geometry().ok()?;
        let t = std::time::Instant::now();
        let m = MomProblem::new(&g, 915e6, MomOptions { edges_per_lambda: edges, ..Default::default() }).ok()?;
        if m.unknowns() > 26000 {
            return Some((m.unknowns(), crate::C64::new(f64::NAN, f64::NAN), 0.0));
        }
        let z = m.solve(915e6, false).ok()?.z[0][0];
        Some((m.unknowns(), z, t.elapsed().as_secs_f64()))
    }

    /// A rectangular strip loop against the textbook: Wheeler's rectangular loop with a flat strip's
    /// equivalent radius of a quarter of its width.
    #[test]
    #[ignore]
    fn tag_study_trace_mesh() {
        use std::f64::consts::PI;
        for (a, b, w) in [(0.03f64, 0.02f64, 0.001f64), (0.016, 0.015, 0.0019), (0.03, 0.02, 0.0003)] {
            let mut p = Project::from_template("custom", Some("915MHz")).unwrap();
            let ws = format!("{}mm", w * 1e3);
            crate::freeform::draw(&mut p, &serde_json::json!({
                "vars": { "A": format!("{}mm", a * 500.0), "B": format!("{}mm", b * 1e3), "w": ws, "gp": "1mm" },
                "parts": [{ "kind": "strip", "name": "loop", "path": [["gp/2", "0"], ["A", "0"], ["A", "B"], ["-A", "B"], ["-A", "0"], ["-gp/2", "0"]], "w": "w" }],
                "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
            })).unwrap();
            let r = w / 4.0;
            let d = (a * a + b * b).sqrt();
            let l = crate::MU0 / PI * (a * (2.0 * a * b / (r * (a + d))).ln() + b * (2.0 * a * b / (r * (b + d))).ln() + 2.0 * d - 2.0 * (a + b));
            println!("loop {:.0} x {:.0} mm, {:.2} mm strip: low-frequency inductance from the formula {:.1} nH, X at 915 MHz about {:.1} ohm before self-resonance", a * 1e3, b * 1e3, w * 1e3, l * 1e9, 2.0 * PI * 915e6 * l);
            for edges in [96.0, 192.0, 384.0, 768.0, 1536.0, 3072.0] {
                if let Some((n, z, s)) = z_at(&p, edges) {
                    println!("  {edges:>5}/λ, edge {:.2} mm, {n:>6} unknowns, {s:>6.1} s: Z {:.2} {:+.2}j", crate::C0 / 915e6 / edges * 1e3, z.re, z.im);
                }
            }
        }
        // The design the search ranked first.
        let mut t = Project::from_template("tag_loop", Some("915MHz")).unwrap();
        for (name, mm) in [("Lx", 93.356), ("w", 0.567), ("a", 16.338), ("b", 14.906), ("wl", 1.867), ("gc", 1.817), ("g", 0.763), ("Hm", 3.474), ("s", 2.655), ("x0", 17.502), ("Lp", 17.954), ("Hp", 13.055)] {
            t.params.iter_mut().find(|r| r.name == name).unwrap().expr = format!("{mm}mm");
        }
        t.params.iter_mut().find(|r| r.name == "n").unwrap().expr = "2".into();
        println!("the searched tag, free space, PEC:");
        for edges in [96.0, 192.0, 384.0, 768.0, 1536.0] {
            if let Some((n, z, s)) = z_at(&t, edges) {
                println!("  {edges:>5}/λ, edge {:.2} mm, {n:>6} unknowns, {s:>6.1} s: Z {:.2} {:+.2}j", crate::C0 / 915e6 / edges * 1e3, z.re, z.im);
            }
        }
    }
}

/// Who's right about a loop's reactance: the surface solver, a separate thin-wire solver, and the
/// textbook inductance, on a loop nothing else is near. `cargo test -p antenna_core --release
/// tag_study_three_referees -- --ignored --nocapture`
#[cfg(test)]
mod referees {
    use crate::geometry::{Geometry, PortDef, Shape, Solid};
    use crate::mom::{MomOptions, MomProblem};
    use std::f64::consts::PI;

    #[test]
    #[ignore]
    fn tag_study_three_referees() {
        let (a, b, w, gap) = (0.03f64, 0.02f64, 0.002f64, 0.0015f64);
        let r = w / 4.0;
        let d = (a * a + b * b).sqrt();
        let l = crate::MU0 / PI * (a * (2.0 * a * b / (r * (a + d))).ln() + b * (2.0 * a * b / (r * (b + d))).ln() + 2.0 * d - 2.0 * (a + b));
        println!("{:.0} x {:.0} mm loop, {:.1} mm strip or {:.2} mm radius wire. Wheeler: {:.1} nH", a * 1e3, b * 1e3, w * 1e3, r * 1e3, l * 1e9);
        let h = |x: f64, y: f64| [x, y, 0.0];
        let (ha, hg) = (a / 2.0, gap / 2.0);
        let wire = Geometry {
            solids: vec![Solid { name: "loop".into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![h(hg, 0.0), h(ha, 0.0), h(ha, b), h(-ha, b), h(-ha, 0.0), h(-hg, 0.0)], r } }],
            ports: vec![PortDef { name: "feed".into(), a: h(-hg, 0.0), b: h(hg, 0.0), width: 0.0, z0: 200.0 }],
            ..Default::default()
        };
        let mut p = crate::project::Project::from_template("custom", Some("915MHz")).unwrap();
        crate::freeform::draw(&mut p, &serde_json::json!({
            "vars": { "A": format!("{}mm", ha * 1e3), "B": format!("{}mm", b * 1e3), "w": format!("{}mm", w * 1e3), "gp": format!("{}mm", gap * 1e3) },
            "parts": [{ "kind": "strip", "name": "loop", "path": [["gp/2", "0"], ["A", "0"], ["A", "B"], ["-A", "B"], ["-A", "0"], ["-gp/2", "0"]], "w": "w" }],
            "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
        })).unwrap();
        let strip = p.geometry().unwrap();
        let top = 1.5e9;
        let wires: Vec<(f64, MomProblem)> = [40.0, 160.0, 640.0].iter().filter_map(|s| MomProblem::new(&wire, top, MomOptions { segments_per_lambda: *s, ..Default::default() }).ok().map(|m| (*s, m))).collect();
        let surfaces: Vec<(f64, MomProblem)> = [192.0, 768.0].iter().filter_map(|e| MomProblem::new(&strip, top, MomOptions { edges_per_lambda: *e, ..Default::default() }).ok().map(|m| (*e, m))).collect();
        for (s, m) in &wires {
            println!("wire  {s:>4}/λ: {} unknowns", m.unknowns());
        }
        for (e, m) in &surfaces {
            println!("sheet {e:>4}/λ: {} unknowns", m.unknowns());
        }
        for f in [100e6, 200e6, 400e6, 600e6, 800e6, 915e6, 1100e6] {
            let formula = 2.0 * PI * f * l;
            let mut row = format!("{:>5.0} MHz: ωL {formula:>6.1}", f / 1e6);
            for (s, m) in &wires {
                let z = m.solve(f, false).unwrap().z[0][0];
                row += &format!(" | wire{s:.0} {:.2} {:+.1}j", z.re, z.im);
            }
            for (e, m) in &surfaces {
                let z = m.solve(f, false).unwrap().z[0][0];
                row += &format!(" | sheet{e:.0} {:.2} {:+.1}j", z.re, z.im);
            }
            println!("{row}");
        }
    }
}
