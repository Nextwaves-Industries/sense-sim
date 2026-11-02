//! Optimisers for design variables and tolerance analysis. Objectives may be slow
//! field solves: evaluations run on at most `parallel` threads at once, and the
//! budget in `max_evals` is never exceeded. Everything is deterministic for a seed.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

/// xoshiro256** seeded through splitmix64.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        Self { s: [next(), next(), next(), next()] }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)`.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        ((self.uniform() * n as f64) as usize).min(n.saturating_sub(1))
    }

    /// Standard normal, Box-Muller.
    pub fn gauss(&mut self) -> f64 {
        let a = 1.0 - self.uniform();
        let b = self.uniform();
        (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Method {
    NelderMead,
    ParticleSwarm,
    DifferentialEvolution,
    /// Coarse global search for expensive solvers: a Latin hypercube, then a cubic
    /// RBF surrogate with a linear tail picks infill points, then a local refine.
    Surrogate,
}

/// `tol` is the spread of objective values that counts as converged; the spread
/// of the points, in units of the bounds' width, must also fall below `√tol`.
/// `population` is the swarm or population size, or for `Surrogate` the size of the
/// initial Latin hypercube.
#[derive(Clone, Debug)]
pub struct Options {
    pub method: Method,
    pub max_evals: usize,
    pub seed: u64,
    pub population: Option<usize>,
    pub tol: f64,
    /// How many evaluations may run at once.
    pub parallel: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { method: Method::NelderMead, max_evals: 500, seed: 1, population: None, tol: 1e-6, parallel: 1 }
    }
}

#[derive(Clone, Debug)]
pub struct Trace {
    pub evals: usize,
    pub best: f64,
    pub x: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub x: Vec<f64>,
    pub value: f64,
    pub evals: usize,
    pub history: Vec<Trace>,
}

/// Minimises `f` within `bounds`. An infinite bound becomes ten times `max(|x0|, 1)`
/// away from `x0`; a NaN from `f` counts as +∞. `progress` sees each new best and
/// returns false to stop.
///
/// Nelder-Mead evaluates one point at a time except for its starting simplex and
/// shrink steps, so it gains little from `parallel`. The swarm and evolution
/// methods evaluate whole generations at once and give the same result for any
/// `parallel`. The surrogate takes `parallel` infill points per round, so its path
/// depends on `parallel` as well as the seed.
pub fn minimize<F, P>(f: F, x0: &[f64], bounds: &[(f64, f64)], options: &Options, mut progress: P) -> Outcome
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    let d = x0.len();
    let mut lo = Vec::with_capacity(d);
    let mut hi = Vec::with_capacity(d);
    for (i, x) in x0.iter().enumerate() {
        let (a, b) = bounds.get(i).copied().unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
        let reach = 10.0 * x.abs().max(1.0);
        lo.push(if a.is_finite() { a } else { x - reach });
        hi.push(if b.is_finite() { b } else { x + reach });
    }
    let start: Vec<f64> = (0..d)
        .map(|i| {
            let width = hi[i] - lo[i];
            if width > 0.0 { ((x0[i] - lo[i]) / width).clamp(0.0, 1.0) } else { 0.0 }
        })
        .collect();

    let mut run = Run {
        f: &f,
        progress: &mut progress,
        lo,
        hi,
        parallel: options.parallel.max(1),
        max_evals: options.max_evals.max(1),
        tol: options.tol.max(0.0),
        evals: 0,
        best_u: start.clone(),
        best: f64::INFINITY,
        history: Vec::new(),
        stopped: false,
    };
    let mut rng = Rng::new(options.seed);

    if d == 0 {
        run.eval(&[start]);
    } else {
        match options.method {
            Method::NelderMead => {
                nelder_mead(&mut run, &start, 0.1);
                polish(&mut run, 0.05);
            }
            Method::ParticleSwarm => particle_swarm(&mut run, &mut rng, &start, options.population),
            Method::DifferentialEvolution => differential_evolution(&mut run, &mut rng, &start, options.population),
            Method::Surrogate => surrogate(&mut run, &mut rng, &start, options.population),
        }
    }

    Outcome { x: run.real(&run.best_u), value: run.best, evals: run.evals, history: run.history }
}

/// Evaluation bookkeeping. Optimisers work in the unit box; `real` maps back.
struct Run<'a, F, P> {
    f: &'a F,
    progress: &'a mut P,
    lo: Vec<f64>,
    hi: Vec<f64>,
    parallel: usize,
    max_evals: usize,
    tol: f64,
    evals: usize,
    best_u: Vec<f64>,
    best: f64,
    history: Vec<Trace>,
    stopped: bool,
}

impl<F, P> Run<'_, F, P>
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    fn real(&self, u: &[f64]) -> Vec<f64> {
        u.iter().enumerate().map(|(i, v)| (self.lo[i] + v * (self.hi[i] - self.lo[i])).clamp(self.lo[i], self.hi[i])).collect()
    }

    fn done(&self) -> bool {
        self.stopped || self.evals >= self.max_evals
    }

    /// Values for `us`; points past the budget or after a stop read +∞.
    fn eval(&mut self, us: &[Vec<f64>]) -> Vec<f64> {
        let room = if self.stopped { 0 } else { self.max_evals.saturating_sub(self.evals) };
        let count = us.len().min(room);
        let xs: Vec<Vec<f64>> = us[..count].iter().map(|u| self.real(u)).collect();
        let values = par_map(self.f, &xs, self.parallel);
        let mut out = vec![f64::INFINITY; us.len()];
        for i in 0..count {
            let v = if values[i].is_nan() { f64::INFINITY } else { values[i] };
            out[i] = v;
            self.evals += 1;
            // Once stopped, the rest of a running batch is counted but not reported.
            if v < self.best && !self.stopped {
                self.best = v;
                self.best_u = us[i].clone();
                let trace = Trace { evals: self.evals, best: v, x: xs[i].clone() };
                if !(self.progress)(&trace) {
                    self.stopped = true;
                }
                self.history.push(trace);
            }
        }
        out
    }

    fn eval_one(&mut self, u: &[f64]) -> f64 {
        self.eval(std::slice::from_ref(&u.to_vec()))[0]
    }
}

/// Evaluates all points on at most `parallel` threads.
fn par_map<F>(f: &F, points: &[Vec<f64>], parallel: usize) -> Vec<f64>
where
    F: Fn(&[f64]) -> f64 + Sync,
{
    let workers = parallel.max(1).min(points.len());
    if workers <= 1 {
        return points.iter().map(|p| f(p)).collect();
    }
    let next = AtomicUsize::new(0);
    let mut out = vec![f64::INFINITY; points.len()];
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut finished = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(point) = points.get(i) else {
                            return finished;
                        };
                        finished.push((i, f(point)));
                    }
                })
            })
            .collect();
        for handle in handles {
            for (i, v) in handle.join().expect("an objective evaluation panicked") {
                out[i] = v;
            }
        }
    });
    out
}

fn clamp_unit(u: &mut [f64]) {
    u.iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
}

fn latin_hypercube(rng: &mut Rng, n: usize, d: usize) -> Vec<Vec<f64>> {
    let mut points = vec![vec![0.0; d]; n];
    for j in 0..d {
        let mut order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            let other = rng.below(i + 1);
            order.swap(i, other);
        }
        for (point, stratum) in points.iter_mut().zip(&order) {
            point[j] = (*stratum as f64 + rng.uniform()) / n as f64;
        }
    }
    points
}

/// Largest per-axis distance between any point and the first one.
fn spatial_spread(points: &[Vec<f64>]) -> f64 {
    let mut spread: f64 = 0.0;
    for p in &points[1..] {
        for (a, b) in p.iter().zip(&points[0]) {
            spread = spread.max((a - b).abs());
        }
    }
    spread
}

fn has_converged(values: &[f64], points: &[Vec<f64>], tol: f64) -> bool {
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    high - low <= tol && spatial_spread(points) <= tol.sqrt().max(1e-12)
}

/// Bound-clamped Nelder-Mead from `start`, returning at convergence or budget.
fn nelder_mead<F, P>(run: &mut Run<'_, F, P>, start: &[f64], step: f64)
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    let d = start.len();
    let mut points = vec![start.to_vec()];
    for i in 0..d {
        let mut p = start.to_vec();
        p[i] += if start[i] + step <= 1.0 { step } else { -step };
        clamp_unit(&mut p);
        points.push(p);
    }
    let mut values = run.eval(&points);

    // Gao-Han coefficients keep large simplexes from collapsing early.
    let n = d as f64;
    let (expand, contract, shrink) = if d > 2 { (1.0 + 2.0 / n, 0.75 - 0.5 / n, 1.0 - 1.0 / n) } else { (2.0, 0.5, 0.5) };

    while !run.done() {
        let mut order: Vec<usize> = (0..=d).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        points = order.iter().map(|i| points[*i].clone()).collect();
        values = order.iter().map(|i| values[*i]).collect();
        if has_converged(&values, &points, run.tol) || spatial_spread(&points) < 1e-13 {
            return;
        }

        let mut centroid = vec![0.0; d];
        for p in &points[..d] {
            for (c, v) in centroid.iter_mut().zip(p) {
                *c += v / n;
            }
        }
        let worst = points[d].clone();
        let along = |t: f64| -> Vec<f64> {
            let mut p: Vec<f64> = centroid.iter().zip(&worst).map(|(c, w)| c + t * (c - w)).collect();
            clamp_unit(&mut p);
            p
        };

        let reflected = along(1.0);
        let fr = run.eval_one(&reflected);
        if fr < values[0] {
            let expanded = along(expand);
            let fe = run.eval_one(&expanded);
            if fe < fr {
                points[d] = expanded;
                values[d] = fe;
            } else {
                points[d] = reflected;
                values[d] = fr;
            }
            continue;
        }
        if fr < values[d - 1] {
            points[d] = reflected;
            values[d] = fr;
            continue;
        }
        let (candidate, limit) = if fr < values[d] { (along(contract), fr) } else { (along(-contract), values[d]) };
        let fc = run.eval_one(&candidate);
        if fc < limit {
            points[d] = candidate;
            values[d] = fc;
            continue;
        }
        let best = points[0].clone();
        let shrunk: Vec<Vec<f64>> =
            points[1..].iter().map(|p| p.iter().zip(&best).map(|(v, b)| b + shrink * (v - b)).collect()).collect();
        let fresh = run.eval(&shrunk);
        for (i, (p, v)) in shrunk.into_iter().zip(fresh).enumerate() {
            points[i + 1] = p;
            values[i + 1] = v;
        }
    }
}

/// Restarts Nelder-Mead around the best point until a restart no longer helps. A
/// fresh simplex recovers from a collapse onto a bound or a false convergence.
fn polish<F, P>(run: &mut Run<'_, F, P>, step: f64)
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    let mut step = step;
    while !run.done() {
        let before = run.best;
        let start = run.best_u.clone();
        nelder_mead(run, &start, step);
        if before - run.best <= run.tol.max(1e-14) {
            return;
        }
        step = (step * 0.5).max(1e-4);
    }
}

fn starting_population(rng: &mut Rng, start: &[f64], size: usize) -> Vec<Vec<f64>> {
    let mut points = latin_hypercube(rng, size, start.len());
    points[0] = start.to_vec();
    points
}

/// The share of the budget the global phases may use before the local polish.
fn global_budget(max_evals: usize) -> usize {
    max_evals * 85 / 100
}

/// Synchronous constriction-factor swarm.
fn particle_swarm<F, P>(run: &mut Run<'_, F, P>, rng: &mut Rng, start: &[f64], population: Option<usize>)
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    const INERTIA: f64 = 0.7298;
    const PULL: f64 = 1.49618;
    const MAX_SPEED: f64 = 0.25;

    let d = start.len();
    let size = population.unwrap_or((10.0 + 10.0 * (d as f64).sqrt()) as usize).max(2);
    let limit = global_budget(run.max_evals);
    let mut points = starting_population(rng, start, size);
    let mut speeds: Vec<Vec<f64>> = (0..size).map(|_| (0..d).map(|_| 0.2 * (rng.uniform() - 0.5)).collect()).collect();
    let mut own_value = run.eval(&points);
    let mut own_best = points.clone();

    while !run.done() && run.evals < limit && !has_converged(&own_value, &own_best, run.tol) {
        let leader = run.best_u.clone();
        for i in 0..size {
            for j in 0..d {
                let (a, b) = (rng.uniform(), rng.uniform());
                let v = INERTIA * speeds[i][j] + PULL * a * (own_best[i][j] - points[i][j]) + PULL * b * (leader[j] - points[i][j]);
                let v = v.clamp(-MAX_SPEED, MAX_SPEED);
                let next = points[i][j] + v;
                points[i][j] = next.clamp(0.0, 1.0);
                speeds[i][j] = if (0.0..=1.0).contains(&next) { v } else { 0.0 };
            }
        }
        let values = run.eval(&points);
        for i in 0..size {
            if values[i] < own_value[i] {
                own_value[i] = values[i];
                own_best[i] = points[i].clone();
            }
        }
    }
    let spread = spatial_spread(&own_best);
    polish(run, spread.clamp(1e-3, 0.05));
}

/// DE/rand/1/bin mixed with DE/best/1/bin, dithered F, generational selection.
fn differential_evolution<F, P>(run: &mut Run<'_, F, P>, rng: &mut Rng, start: &[f64], population: Option<usize>)
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    const CROSSOVER: f64 = 0.9;

    let d = start.len();
    let size = population.unwrap_or((10 * d).clamp(15, 60)).max(4);
    let limit = global_budget(run.max_evals);
    let mut points = starting_population(rng, start, size);
    let mut values = run.eval(&points);

    while !run.done() && run.evals < limit && !has_converged(&values, &points, run.tol) {
        let weight = 0.5 + 0.5 * rng.uniform();
        let best = run.best_u.clone();
        let trials: Vec<Vec<f64>> = (0..size)
            .map(|i| {
                let mut pick = |taken: &[usize]| loop {
                    let k = rng.below(size);
                    if !taken.contains(&k) {
                        return k;
                    }
                };
                let a = pick(&[i]);
                let b = pick(&[i, a]);
                let c = pick(&[i, a, b]);
                let use_best = rng.uniform() < 0.5;
                let forced = rng.below(d);
                (0..d)
                    .map(|j| {
                        if j != forced && rng.uniform() >= CROSSOVER {
                            return points[i][j];
                        }
                        let base = if use_best { best[j] } else { points[a][j] };
                        let v = base + weight * (points[b][j] - points[c][j]);
                        if v < 0.0 {
                            0.5 * points[i][j]
                        } else if v > 1.0 {
                            0.5 * (points[i][j] + 1.0)
                        } else {
                            v
                        }
                    })
                    .collect()
            })
            .collect();
        let scores = run.eval(&trials);
        for (i, trial) in trials.into_iter().enumerate() {
            if scores[i] <= values[i] {
                points[i] = trial;
                values[i] = scores[i];
            }
        }
    }
    let spread = spatial_spread(&points);
    polish(run, spread.clamp(1e-3, 0.05));
}

/// Stochastic RBF search (Regis and Shoemaker): each round scores a pool of
/// candidates on predicted value and on distance from what has been evaluated.
fn surrogate<F, P>(run: &mut Run<'_, F, P>, rng: &mut Rng, start: &[f64], population: Option<usize>)
where
    F: Fn(&[f64]) -> f64 + Sync,
    P: FnMut(&Trace) -> bool,
{
    const VALUE_WEIGHTS: [f64; 4] = [0.3, 0.5, 0.8, 0.95];
    const SCALES: [f64; 4] = [0.2, 0.05, 0.01, 0.002];

    let d = start.len();
    let initial = population.unwrap_or(4 * d + 8).max(d + 2).min((run.max_evals / 2).max(1));
    let limit = run.max_evals * 7 / 10;
    let stall_limit = 8 + 4 * d;
    let pool_size = (200 * d).clamp(400, 2000);

    let mut xs = starting_population(rng, start, initial);
    let mut ys = run.eval(&xs);
    let mut stalled = 0;
    let mut round = 0;

    while !run.done() && run.evals < limit && stalled < stall_limit {
        let model = Rbf::fit(&xs, &ys);
        let best = run.best_u.clone();
        let pool: Vec<Vec<f64>> = (0..pool_size)
            .map(|i| {
                if i % 4 == 3 {
                    (0..d).map(|_| rng.uniform()).collect()
                } else {
                    let scale = SCALES[rng.below(SCALES.len())];
                    let mut p: Vec<f64> = best.iter().map(|b| b + scale * rng.gauss()).collect();
                    clamp_unit(&mut p);
                    p
                }
            })
            .collect();

        let batch = run.parallel.min(limit.saturating_sub(run.evals).max(1));
        let predicted: Vec<f64> = pool.iter().map(|p| model.as_ref().map_or(0.0, |m| m.predict(p))).collect();
        let mut nearest: Vec<f64> = pool.iter().map(|p| xs.iter().map(|q| distance(p, q)).fold(f64::INFINITY, f64::min)).collect();
        let (v_lo, v_hi) = min_max(&predicted);
        let mut chosen: Vec<Vec<f64>> = Vec::with_capacity(batch);
        for _ in 0..batch {
            let weight = VALUE_WEIGHTS[round % VALUE_WEIGHTS.len()];
            round += 1;
            let (d_lo, d_hi) = min_max(&nearest);
            let mut pick = None;
            let mut least = f64::INFINITY;
            for k in 0..pool.len() {
                if nearest[k] < 1e-7 {
                    continue;
                }
                let value_score = if v_hi > v_lo { (predicted[k] - v_lo) / (v_hi - v_lo) } else { 1.0 };
                let distance_score = if d_hi > d_lo { (d_hi - nearest[k]) / (d_hi - d_lo) } else { 1.0 };
                let score = weight * value_score + (1.0 - weight) * distance_score;
                if score < least {
                    least = score;
                    pick = Some(k);
                }
            }
            let Some(k) = pick else {
                break;
            };
            let point = pool[k].clone();
            for (p, near) in pool.iter().zip(nearest.iter_mut()) {
                *near = near.min(distance(p, &point));
            }
            chosen.push(point);
        }
        if chosen.is_empty() {
            break;
        }
        let before = run.best;
        let values = run.eval(&chosen);
        stalled = if run.best < before { 0 } else { stalled + chosen.len() };
        xs.extend(chosen);
        ys.extend(values);
    }
    polish(run, 0.05);
}

fn distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt()
}

fn min_max(values: &[f64]) -> (f64, f64) {
    values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)))
}

/// Cubic radial basis functions with a linear tail, fitted to values clipped at
/// their median so huge values far from the optimum do not swamp the local shape.
struct Rbf {
    centers: Vec<Vec<f64>>,
    coefficients: Vec<f64>,
    tail: Vec<f64>,
}

impl Rbf {
    const MAX_CENTERS: usize = 150;

    fn fit(xs: &[Vec<f64>], ys: &[f64]) -> Option<Self> {
        let mut order: Vec<usize> = (0..xs.len()).filter(|i| ys[*i].is_finite()).collect();
        order.sort_by(|a, b| ys[*a].total_cmp(&ys[*b]));
        order.truncate(Self::MAX_CENTERS);
        let d = xs.first()?.len();
        let n = order.len();
        if n < d + 2 {
            return None;
        }
        let median = ys[order[n / 2]];
        let size = n + d + 1;
        let mut a = vec![vec![0.0; size]; size];
        let mut b = vec![0.0; size];
        for (row, i) in order.iter().enumerate() {
            b[row] = ys[*i].min(median);
            for (col, j) in order.iter().enumerate() {
                a[row][col] = distance(&xs[*i], &xs[*j]).powi(3);
            }
            a[row][n] = 1.0;
            a[n][row] = 1.0;
            for k in 0..d {
                a[row][n + 1 + k] = xs[*i][k];
                a[n + 1 + k][row] = xs[*i][k];
            }
        }
        if !solve_dense(&mut a, &mut b) {
            return None;
        }
        let tail = b.split_off(n);
        Some(Self { centers: order.iter().map(|i| xs[*i].clone()).collect(), coefficients: b, tail })
    }

    fn predict(&self, x: &[f64]) -> f64 {
        let radial: f64 = self.centers.iter().zip(&self.coefficients).map(|(c, w)| w * distance(x, c).powi(3)).sum();
        radial + self.tail[0] + x.iter().zip(&self.tail[1..]).map(|(v, t)| v * t).sum::<f64>()
    }
}

/// Gaussian elimination with partial pivoting, `b` becomes the solution. False
/// when the matrix is numerically singular.
fn solve_dense(a: &mut [Vec<f64>], b: &mut [f64]) -> bool {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|x, y| a[*x][col].abs().total_cmp(&a[*y][col].abs())).unwrap_or(col);
        if a[pivot][col].abs() < 1e-14 {
            return false;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let factor = a[row][col] / a[col][col];
            if factor != 0.0 {
                let (top, bottom) = a.split_at_mut(row);
                for (x, y) in bottom[0][col..].iter_mut().zip(&top[col][col..]) {
                    *x -= factor * y;
                }
                b[row] -= factor * b[col];
            }
        }
    }
    for col in (0..n).rev() {
        let tail: f64 = (col + 1..n).map(|j| a[col][j] * b[j]).sum();
        b[col] = (b[col] - tail) / a[col][col];
    }
    true
}

/// `rel·|x| + abs` is the half-width of a uniform spread, or one sigma when `gaussian`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tolerance {
    pub rel: f64,
    pub abs: f64,
    pub gaussian: bool,
}

#[derive(Clone, Debug)]
pub struct Yield {
    pub samples: usize,
    pub passed: usize,
    pub fraction: f64,
    /// Wilson score interval on `fraction`.
    pub ci95: (f64, f64),
    pub values: Vec<f64>,
    pub mean: f64,
    pub std: f64,
    /// Per-variable |correlation| with the value.
    pub sensitivities: Vec<f64>,
    /// Samples that returned NaN: the solve failed, so they say nothing about the design.
    pub failed: usize,
}

impl Yield {
    /// Whether the data supports a yield of at least `target`: the interval's lower bound
    /// clears it, and every sample solved. 27 of 30 reads 90 % and only supports 74 %.
    pub fn meets(&self, target: f64) -> bool {
        self.failed == 0 && self.samples > 0 && self.ci95.0 >= target
    }
}

/// Monte-Carlo yield: perturbs `x0` with the tolerances (a variable without one stays
/// put), `f` returns the figure of merit, `pass` decides. A NaN value means the sample
/// could not be solved: it is counted in `failed`, and yield, interval and statistics are
/// of the samples that did solve.
pub fn monte_carlo<F>(f: F, x0: &[f64], tol: &[Tolerance], n: usize, seed: u64, parallel: usize, pass: impl Fn(f64) -> bool) -> Yield
where
    F: Fn(&[f64]) -> f64 + Sync,
{
    let mut rng = Rng::new(seed);
    let samples: Vec<Vec<f64>> = (0..n)
        .map(|_| {
            x0.iter()
                .enumerate()
                .map(|(i, x)| {
                    let t = tol.get(i).copied().unwrap_or_default();
                    let width = t.rel * x.abs() + t.abs;
                    let draw = if t.gaussian { rng.gauss() } else { 2.0 * rng.uniform() - 1.0 };
                    x + width * draw
                })
                .collect()
        })
        .collect();
    let values = par_map(&f, &samples, parallel);

    let passed = values.iter().filter(|v| !v.is_nan() && pass(**v)).count();
    let finite: Vec<usize> = (0..n).filter(|i| !values[*i].is_nan()).collect();
    let solved = finite.len();
    let fraction = if solved > 0 { passed as f64 / solved as f64 } else { 0.0 };
    let picked: Vec<f64> = finite.iter().map(|i| values[*i]).collect();
    let (mean, std) = mean_std(&picked);
    let sensitivities = (0..x0.len())
        .map(|j| {
            let column: Vec<f64> = finite.iter().map(|i| samples[*i][j]).collect();
            correlation(&column, &picked).abs()
        })
        .collect();
    Yield { samples: n, passed, fraction, ci95: wilson_interval(passed, solved), values, mean, std, sensitivities, failed: n - solved }
}

/// One-at-a-time local sensitivity `d(value)/d(x)` by central differences, with a
/// step of `step_rel·|x|`, or `step_rel` itself for a variable at zero.
pub fn sensitivity<F>(f: F, x0: &[f64], step_rel: f64, parallel: usize) -> Vec<f64>
where
    F: Fn(&[f64]) -> f64 + Sync,
{
    let steps: Vec<f64> = x0.iter().map(|x| if *x == 0.0 { step_rel } else { step_rel * x.abs() }).collect();
    let mut points = Vec::with_capacity(2 * x0.len());
    for (i, h) in steps.iter().enumerate() {
        for sign in [1.0, -1.0] {
            let mut p = x0.to_vec();
            p[i] += sign * h;
            points.push(p);
        }
    }
    let values = par_map(&f, &points, parallel);
    steps.iter().enumerate().map(|(i, h)| (values[2 * i] - values[2 * i + 1]) / (2.0 * h)).collect()
}

fn mean_std(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    if values.len() < 2 {
        return (mean, 0.0);
    }
    let variance = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / (n - 1.0);
    (mean, variance.sqrt())
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let (ma, _) = mean_std(a);
    let (mb, _) = mean_std(b);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma) * (x - ma);
        bb += (y - mb) * (y - mb);
    }
    if aa <= 0.0 || bb <= 0.0 { 0.0 } else { ab / (aa * bb).sqrt() }
}

fn wilson_interval(passed: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let z = 1.959_963_985;
    let n = n as f64;
    let p = passed as f64 / n;
    let z2 = z * z;
    let denominator = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denominator;
    let half = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denominator;
    ((center - half).max(0.0), (center + half).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    const ALL: [Method; 4] = [Method::NelderMead, Method::ParticleSwarm, Method::DifferentialEvolution, Method::Surrogate];

    fn rosenbrock(x: &[f64]) -> f64 {
        (1.0 - x[0]).powi(2) + 100.0 * (x[1] - x[0] * x[0]).powi(2)
    }

    fn sphere(x: &[f64]) -> f64 {
        x.iter().map(|v| (v - 0.3) * (v - 0.3)).sum()
    }

    fn options(method: Method, max_evals: usize) -> Options {
        Options { method, max_evals, seed: 42, population: None, tol: 1e-10, parallel: 1 }
    }

    /// Stated budgets: what each method needs on 2-D Rosenbrock from (−1.2, 1).
    fn rosenbrock_budget(method: Method) -> usize {
        match method {
            Method::NelderMead => 1000,
            Method::ParticleSwarm => 6000,
            Method::DifferentialEvolution => 6000,
            Method::Surrogate => 400,
        }
    }

    #[test]
    fn rosenbrock_minimum_is_found_by_every_method() {
        for method in ALL {
            let budget = rosenbrock_budget(method);
            let out = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0), (-2.0, 2.0)], &options(method, budget), |_| true);
            let error = ((out.x[0] - 1.0).powi(2) + (out.x[1] - 1.0).powi(2)).sqrt();
            println!("{method:?}: x = {:?}, f = {:.3e}, error = {error:.2e}, evals = {}", out.x, out.value, out.evals);
            assert!(error < 1e-2, "{method:?}: {:?} after {} evals", out.x, out.evals);
            assert!(out.evals <= budget);
            assert_eq!(out.value, out.history.last().unwrap().best);
        }
    }

    #[test]
    fn five_dimensional_sphere_is_found_by_every_method() {
        for method in ALL {
            let budget = match method {
                Method::NelderMead => 3000,
                Method::Surrogate => 300,
                _ => 8000,
            };
            let out = minimize(sphere, &[-1.0, 1.5, 0.5, -0.5, 2.0], &[(-3.0, 3.0); 5], &options(method, budget), |_| true);
            let error = out.x.iter().map(|v| (v - 0.3).abs()).fold(0.0, f64::max);
            println!("{method:?} sphere: error = {error:.2e}, evals = {}", out.evals);
            assert!(error < 1e-2, "{method:?}: {:?}", out.x);
            assert!(out.evals <= budget);
        }
    }

    #[test]
    fn bounds_are_respected_on_a_constrained_problem() {
        let outside = AtomicBool::new(false);
        let bounded = |x: &[f64]| {
            if !(0.0..=1.0).contains(&x[0]) || !(0.25..=0.5).contains(&x[1]) {
                outside.store(true, Ordering::Relaxed);
            }
            (x[0] - 3.0).powi(2) + (x[1] + 2.0).powi(2)
        };
        for method in ALL {
            let out = minimize(bounded, &[0.5, 0.3], &[(0.0, 1.0), (0.25, 0.5)], &options(method, 800), |_| true);
            assert!(!outside.load(Ordering::Relaxed), "{method:?} stepped out of bounds");
            assert!((out.x[0] - 1.0).abs() < 1e-3 && (out.x[1] - 0.25).abs() < 1e-3, "{method:?}: {:?}", out.x);
        }
    }

    #[test]
    fn a_start_outside_the_bounds_is_clamped() {
        let out = minimize(|x: &[f64]| x[0] * x[0], &[5.0], &[(1.0, 2.0)], &options(Method::NelderMead, 100), |_| true);
        assert!((out.x[0] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn parallel_limit_is_never_exceeded() {
        for method in ALL {
            for limit in [1usize, 3] {
                let running = AtomicUsize::new(0);
                let peak = AtomicUsize::new(0);
                let counted = |x: &[f64]| {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_micros(300));
                    running.fetch_sub(1, Ordering::SeqCst);
                    sphere(x)
                };
                let mut opts = options(method, 120);
                opts.parallel = limit;
                let out = minimize(counted, &[1.0, 1.0], &[(-2.0, 2.0); 2], &opts, |_| true);
                let seen = peak.load(Ordering::SeqCst);
                assert!(seen <= limit, "{method:?}: {seen} calls at once with a limit of {limit}");
                assert!(out.evals <= 120);
                if limit > 1 {
                    assert!(seen >= 2, "{method:?} never ran in parallel");
                }
            }
        }
    }

    #[test]
    fn same_seed_gives_the_same_result() {
        for method in ALL {
            let opts = options(method, 300);
            let first = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &opts, |_| true);
            let second = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &opts, |_| true);
            assert_eq!(first.x, second.x, "{method:?}");
            assert_eq!(first.evals, second.evals);
            assert_eq!(first.history.len(), second.history.len());
            if method != Method::NelderMead {
                let mut other = opts.clone();
                other.seed = 7;
                let third = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &other, |_| true);
                assert_ne!(first.x, third.x, "{method:?} ignored its seed");
            }
        }
    }

    #[test]
    fn generational_methods_do_not_depend_on_thread_count() {
        for method in [Method::NelderMead, Method::ParticleSwarm, Method::DifferentialEvolution] {
            let serial = options(method, 300);
            let mut threaded = serial.clone();
            threaded.parallel = 4;
            let a = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &serial, |_| true);
            let b = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &threaded, |_| true);
            assert_eq!(a.x, b.x, "{method:?}");
            assert_eq!(a.evals, b.evals);
        }
    }

    #[test]
    fn surrogate_is_deterministic_in_parallel() {
        let mut opts = options(Method::Surrogate, 200);
        opts.parallel = 3;
        let a = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &opts, |_| true);
        let b = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &opts, |_| true);
        assert_eq!(a.x, b.x);
    }

    #[test]
    fn progress_reports_improvements_and_can_stop() {
        let mut seen = Vec::new();
        let out = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &options(Method::NelderMead, 500), |t| {
            seen.push(t.best);
            true
        });
        assert_eq!(seen.len(), out.history.len());
        assert!(seen.windows(2).all(|w| w[1] < w[0]), "only new bests are reported");

        for method in ALL {
            let mut calls = 0;
            let out = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &options(method, 5000), |_| {
                calls += 1;
                calls < 3
            });
            assert_eq!(calls, 3, "{method:?}");
            assert_eq!(out.history.len(), 3);
            assert!(out.evals < 200, "{method:?} kept going: {}", out.evals);
        }
    }

    #[test]
    fn budget_is_exact_and_nan_is_harmless() {
        for method in ALL {
            let mut opts = options(method, 37);
            opts.tol = 0.0;
            let out = minimize(rosenbrock, &[-1.2, 1.0], &[(-2.0, 2.0); 2], &opts, |_| true);
            assert!(out.evals <= 37, "{method:?}: {}", out.evals);
            let nan_half = |x: &[f64]| if x[0] < 0.0 { f64::NAN } else { (x[0] - 0.5).powi(2) };
            let out = minimize(nan_half, &[1.0], &[(-1.0, 2.0)], &options(method, 400), |_| true);
            assert!((out.x[0] - 0.5).abs() < 1e-3, "{method:?}: {:?}", out.x);
        }
        let empty = minimize(|_: &[f64]| 3.0, &[], &[], &options(Method::ParticleSwarm, 50), |_| true);
        assert_eq!((empty.value, empty.evals), (3.0, 1));
    }

    #[test]
    fn monte_carlo_matches_a_linear_function() {
        // v = 2a + 3b + 1 with independent Gaussians: σ = √((2·0.1)² + (3·0.2)²).
        let linear = |x: &[f64]| 2.0 * x[0] + 3.0 * x[1] + 1.0;
        let tol = [Tolerance { rel: 0.1, abs: 0.0, gaussian: true }, Tolerance { rel: 0.0, abs: 0.2, gaussian: true }];
        let n = 20_000;
        let threshold = 6.0 + 0.5;
        let out = monte_carlo(linear, &[1.0, 1.0], &tol, n, 11, 4, |v| v < threshold);
        let sigma = (0.04f64 + 0.36).sqrt();
        let standard_error = sigma / (n as f64).sqrt();
        assert_eq!(out.samples, n);
        assert_eq!(out.values.len(), n);
        assert!((out.mean - 6.0).abs() < 4.0 * standard_error, "mean {}", out.mean);
        assert!((out.std - sigma).abs() < 4.0 * sigma / (2.0 * n as f64).sqrt(), "std {}", out.std);

        let want = normal_cdf(0.5 / sigma);
        let error = (want * (1.0 - want) / n as f64).sqrt();
        assert!((out.fraction - want).abs() < 4.0 * error, "yield {} vs {want}", out.fraction);
        assert!(out.ci95.0 < out.fraction && out.fraction < out.ci95.1);
        assert!(out.ci95.0 <= want && want <= out.ci95.1 || (out.fraction - want).abs() < 4.0 * error);

        let r_a = 0.2 / sigma;
        let r_b = 0.6 / sigma;
        assert!((out.sensitivities[0] - r_a).abs() < 0.03, "{}", out.sensitivities[0]);
        assert!((out.sensitivities[1] - r_b).abs() < 0.03, "{}", out.sensitivities[1]);
    }

    fn normal_cdf(x: f64) -> f64 {
        // Abramowitz and Stegun 26.2.17.
        let t = 1.0 / (1.0 + 0.231_641_9 * x.abs());
        let poly = t * (0.319_381_530 + t * (-0.356_563_782 + t * (1.781_477_937 + t * (-1.821_255_978 + t * 1.330_274_429))));
        let tail = (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt() * poly;
        if x >= 0.0 { 1.0 - tail } else { tail }
    }

    #[test]
    fn monte_carlo_uniform_spread_stays_in_range_and_is_seeded() {
        let tol = [Tolerance { rel: 0.5, abs: 1.0, gaussian: false }];
        let identity = |x: &[f64]| x[0];
        let out = monte_carlo(identity, &[4.0], &tol, 5000, 3, 1, |_| true);
        assert!(out.values.iter().all(|v| (1.0..=7.0).contains(v)));
        assert!((out.std - 3.0 / 3f64.sqrt()).abs() < 0.1, "{}", out.std);
        assert_eq!(out.fraction, 1.0);
        assert!(out.ci95.1 == 1.0 && out.ci95.0 > 0.99);
        let again = monte_carlo(identity, &[4.0], &tol, 5000, 3, 4, |_| true);
        assert_eq!(out.values, again.values, "same seed, any thread count");
    }

    /// A sample that would not solve says nothing about the hardware: it is counted
    /// apart, and the yield is of what did solve.
    #[test]
    fn monte_carlo_counts_samples_that_would_not_solve() {
        let next = AtomicUsize::new(0);
        let some_crash = |_: &[f64]| if next.fetch_add(1, Ordering::SeqCst) % 3 == 0 { f64::NAN } else { -20.0 };
        let out = monte_carlo(some_crash, &[1.0], &[Tolerance { rel: 0.1, abs: 0.0, gaussian: true }], 30, 1, 1, |v| v <= -10.0);
        assert_eq!(out.failed, 10);
        assert_eq!(out.passed, 20);
        assert_eq!(out.samples, 30);
        assert!((out.fraction - 1.0).abs() < 1e-12, "of the 20 that solved: {}", out.fraction);
        // The interval is of the 20 that solved, not of 30.
        let (lo, _) = wilson_interval(20, 20);
        assert!((out.ci95.0 - lo).abs() < 1e-12, "{:?}", out.ci95);
        assert!(!out.meets(0.5), "failed samples keep the verdict from passing");
        assert_eq!(out.values.iter().filter(|v| v.is_nan()).count(), 10);
        assert!((out.mean + 20.0).abs() < 1e-12, "the mean leaves them out: {}", out.mean);
    }

    /// 27 of 30 is 90 % read off, but the data only supports about 74 %.
    #[test]
    fn a_yield_is_judged_on_its_lower_bound() {
        let judge = |passed: usize, n: usize| {
            let next = AtomicUsize::new(0);
            let f = |_: &[f64]| if next.fetch_add(1, Ordering::SeqCst) < passed { -20.0 } else { 0.0 };
            monte_carlo(f, &[1.0], &[Tolerance::default()], n, 1, 1, |v| v <= -10.0)
        };
        let marginal = judge(27, 30);
        assert!((marginal.fraction - 0.9).abs() < 1e-12);
        assert!((marginal.ci95.0 - 0.743).abs() < 0.01, "{:?}", marginal.ci95);
        assert!(!marginal.meets(0.9));
        // Enough clean samples do clear it; 30 of 30 still does not.
        assert!(!judge(30, 30).meets(0.9));
        assert!(judge(60, 60).meets(0.9));
    }

    #[test]
    fn monte_carlo_respects_parallel() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let counted = |x: &[f64]| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_micros(200));
            running.fetch_sub(1, Ordering::SeqCst);
            x[0]
        };
        monte_carlo(counted, &[1.0], &[Tolerance::default()], 40, 1, 2, |_| true);
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn sensitivity_of_a_quadratic() {
        // d/dx(x² + 3xy) = 2x + 3y, d/dy = 3x.
        let q = |v: &[f64]| v[0] * v[0] + 3.0 * v[0] * v[1];
        let d = sensitivity(q, &[2.0, -1.0], 1e-3, 2);
        assert!((d[0] - 1.0).abs() < 1e-6, "{}", d[0]);
        assert!((d[1] - 6.0).abs() < 1e-6, "{}", d[1]);
        let at_zero = sensitivity(q, &[0.0, 0.0], 1e-3, 1);
        assert!(at_zero[0].abs() < 1e-6 && at_zero[1].abs() < 1e-6);
    }

    #[test]
    fn rng_is_reproducible_and_well_spread() {
        let mut a = Rng::new(5);
        let mut b = Rng::new(5);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let n = 50_000;
        let (mut sum_u, mut sum_g, mut sum_g2) = (0.0, 0.0, 0.0);
        for _ in 0..n {
            sum_u += a.uniform();
            let g = a.gauss();
            sum_g += g;
            sum_g2 += g * g;
        }
        assert!((sum_u / n as f64 - 0.5).abs() < 0.01);
        assert!((sum_g / n as f64).abs() < 0.02);
        assert!((sum_g2 / n as f64 - 1.0).abs() < 0.03);
    }
}
