//! The CPU time stepper.
//!
//! Each half step is the same small loop six times over, on whole rows so it vectorises. The
//! absorber and anything on a single edge come afterwards as corrections, which keeps the main
//! loop free of branches. An edge with something on it has a zero coefficient in the main loop
//! and is updated from its own list.

use rayon::prelude::*;

use crate::assemble::Assembled;
use crate::problem::{Pulse, Wall};
use crate::real::Real;
use crate::record::{NodeDump, NodePlane, NodeRecorder, Recorder};
use crate::team::Team;

/// `c·((a - b) - (d - f))` added over a row.
#[inline(always)]
fn row_body<R: Real>(dst: &mut [R], c: &[R], a: &[R], b: &[R], d: &[R], f: &[R]) {
    let n = dst.len();
    let (c, a, b, d, f) = (&c[..n], &a[..n], &b[..n], &d[..n], &f[..n]);
    for i in 0..n {
        dst[i] += c[i] * ((a[i] - b[i]) - (d[i] - f[i]));
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
fn row_avx512<R: Real>(dst: &mut [R], c: &[R], a: &[R], b: &[R], d: &[R], f: &[R]) {
    row_body(dst, c, a, b, d, f)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
fn row_avx2<R: Real>(dst: &mut [R], c: &[R], a: &[R], b: &[R], d: &[R], f: &[R]) {
    row_body(dst, c, a, b, d, f)
}

/// Which row loop this CPU gets. Checked once: the check is too slow for a loop this hot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Simd {
    Plain,
    Avx2,
    Avx512,
}

fn detect_simd() -> Simd {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx512f") {
            return Simd::Avx512;
        }
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            return Simd::Avx2;
        }
    }
    Simd::Plain
}

#[inline(always)]
fn row<R: Real>(simd: Simd, dst: &mut [R], c: &[R], a: &[R], b: &[R], d: &[R], f: &[R]) {
    match simd {
        // SAFETY: `detect_simd` found the feature on this CPU.
        #[cfg(target_arch = "x86_64")]
        Simd::Avx512 => unsafe { row_avx512(dst, c, a, b, d, f) },
        #[cfg(target_arch = "x86_64")]
        Simd::Avx2 => unsafe { row_avx2(dst, c, a, b, d, f) },
        _ => row_body(dst, c, a, b, d, f),
    }
}

/// A pointer that may cross threads, for writes the loop knows are disjoint.
struct Shared<T>(*mut T);
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Shared<T> {}
// SAFETY: every use writes indices no other thread touches in that loop.
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}
impl<T> Shared<T> {
    #[inline(always)]
    fn get(self) -> *mut T {
        self.0
    }
}

/// One absorbing layer's profile along its axis.
struct Layer<R> {
    /// The plane this layer's update applies to.
    plane: usize,
    a: R,
    b: R,
    kinv_m1: R,
}

/// One side of the absorber.
struct Zone<R> {
    axis: usize,
    e_layers: Vec<Layer<R>>,
    h_layers: Vec<Layer<R>>,
    // Memory variables, as [row][layer][i], one row per step of `t2`.
    psi_e1: Shared<R>,
    psi_e2: Shared<R>,
    psi_h1: Shared<R>,
    psi_h2: Shared<R>,
    store: [Vec<R>; 4],
}

fn pml_profile<R: Real>(depth: f64, cell: f64, dt: f64) -> (R, R, R) {
    // Polynomial grading, and the CFS shift that keeps low frequencies and grazing waves in check.
    const M: f64 = 3.0;
    let sigma_max = 0.8 * (M + 1.0) / (crate::ETA0 * cell);
    let kappa_max = 4.0;
    let alpha_max = 0.02;
    let g = depth.powf(M);
    let sigma = sigma_max * g;
    let kappa = 1.0 + (kappa_max - 1.0) * g;
    let alpha = alpha_max * (1.0 - depth);
    let e0 = crate::EPS0;
    let b = (-(sigma / kappa + alpha) * dt / e0).exp();
    let a = if sigma > 0.0 { sigma * (b - 1.0) / (sigma * kappa + kappa * kappa * alpha) } else { 0.0 };
    (R::of(a), R::of(b), R::of(1.0 / kappa - 1.0))
}

/// One side of the absorber as tables for the GPU: `(a, b, 1/κ - 1, 0)` per layer, and each set's first plane.
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
pub(crate) fn pml_tables(l: &[f64], side: usize, depth: usize, dt: f64) -> (Vec<[f32; 4]>, usize, Vec<[f32; 4]>, usize) {
    let (mut e, mut h) = (Vec::new(), Vec::new());
    let row = |depth_at: f64, cell: f64| {
        let (a, b, k) = pml_profile::<f32>(depth_at, cell, dt);
        [a, b, k, 0.0]
    };
    if side == 0 {
        let inner = l[depth];
        let thick = inner - l[0];
        for p in 1..=depth {
            e.push(row((inner - l[p]) / thick, (l[p + 1] - l[p - 1]) * 0.5));
        }
        for p in 0..depth {
            h.push(row((inner - (l[p] + l[p + 1]) * 0.5) / thick, l[p + 1] - l[p]));
        }
        (e, 1, h, 0)
    } else {
        let last = l.len() - 1;
        let inner = l[last - depth];
        let thick = l[last] - inner;
        for p in (last - depth)..last {
            e.push(row((l[p] - inner) / thick, (l[p + 1] - l[p - 1]) * 0.5));
            h.push(row(((l[p] + l[p + 1]) * 0.5 - inner) / thick, l[p + 1] - l[p]));
        }
        (e, last - depth, h, last - depth)
    }
}

fn make_zones<R: Real>(asm: &Assembled, dt: f64) -> Vec<Zone<R>> {
    let n = asm.grid.n();
    let mut zones = Vec::new();
    for axis in 0..3 {
        let l = &asm.grid.lines[axis];
        let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
        let (n1, n2) = (n[t1], n[t2]);
        for (side, wall) in [(0usize, asm.walls[axis * 2]), (1, asm.walls[axis * 2 + 1])] {
            let Wall::Pml(depth) = wall else { continue };
            if depth == 0 || depth + 2 >= l.len() {
                continue;
            }
            let (mut e_layers, mut h_layers) = (Vec::new(), Vec::new());
            if side == 0 {
                let inner = l[depth];
                let thick = inner - l[0];
                for p in 1..=depth {
                    let cell = ((l[p + 1] - l[p - 1]) * 0.5).max(1e-30);
                    let (a, b, k) = pml_profile::<R>((inner - l[p]) / thick, cell, dt);
                    e_layers.push(Layer { plane: p, a, b, kinv_m1: k });
                }
                for p in 0..depth {
                    let cell = l[p + 1] - l[p];
                    let (a, b, k) = pml_profile::<R>((inner - (l[p] + l[p + 1]) * 0.5) / thick, cell, dt);
                    h_layers.push(Layer { plane: p, a, b, kinv_m1: k });
                }
            } else {
                let last = l.len() - 1;
                let inner = l[last - depth];
                let thick = l[last] - inner;
                for p in (last - depth)..last {
                    let cell = ((l[p + 1] - l[p - 1]) * 0.5).max(1e-30);
                    let (a, b, k) = pml_profile::<R>((l[p] - inner) / thick, cell, dt);
                    e_layers.push(Layer { plane: p, a, b, kinv_m1: k });
                }
                for p in (last - depth)..last {
                    let cell = l[p + 1] - l[p];
                    let (a, b, k) = pml_profile::<R>(((l[p] + l[p + 1]) * 0.5 - inner) / thick, cell, dt);
                    h_layers.push(Layer { plane: p, a, b, kinv_m1: k });
                }
            }
            let size_e = n2 * e_layers.len() * n1;
            let size_h = n2 * h_layers.len() * n1;
            let mut store = [vec![R::default(); size_e], vec![R::default(); size_e], vec![R::default(); size_h], vec![R::default(); size_h]];
            let (pe1, pe2, ph1, ph2) = (Shared(store[0].as_mut_ptr()), Shared(store[1].as_mut_ptr()), Shared(store[2].as_mut_ptr()), Shared(store[3].as_mut_ptr()));
            zones.push(Zone { axis, e_layers, h_layers, psi_e1: pe1, psi_e2: pe2, psi_h1: ph1, psi_h2: ph2, store });
        }
    }
    zones
}

/// A series R and L on an edge, updated implicitly: a small inductance would otherwise need a
/// time step far below the grid's. Trapezoidal, so it can't add energy.
struct RlState {
    axis: usize,
    node: usize,
    m_dt: f64,
    alpha: f64,
    beta: f64,
    i: f64,
}

/// A series R and C across an edge, for Debye materials. On top of the edge's own coefficient,
/// so handled with the same implicit step.
struct DebyeState {
    axis: usize,
    node: usize,
    m_dt: f64,
    gd: f64,
    a: f64,
    v: f64,
}

struct PortEdge {
    node: usize,
    m_dt: f64,
    g: f64,
    /// Whether this edge is in the row across the middle of the gap.
    mid: bool,
}

struct PortState {
    axis: usize,
    sign: f64,
    edges: Vec<PortEdge>,
    series: f64,
    columns: Vec<Vec<usize>>,
}

/// Port readings in time: `u[n]` at `(n + 1)·dt`, `i[n]` at `(n + ½)·dt`.
#[derive(Clone, Debug, Default)]
pub struct PortTrace {
    pub u: Vec<f64>,
    pub i: Vec<f64>,
    pub z0: f64,
}

/// Why a run stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stop {
    /// Energy fell to the requested level.
    Decayed,
    /// Hit the step limit first.
    StepLimit,
    Cancelled,
    /// The fields blew up.
    Diverged,
}

pub struct Engine<R: Real> {
    pub n: [usize; 3],
    pub dt: f64,
    pub step: u64,
    pub e: [Vec<R>; 3],
    pub h: [Vec<R>; 3],
    ce: [Vec<R>; 3],
    ch: [Vec<R>; 3],
    // Loss, stored as the update's two factors on edges that have any.
    lossy: Vec<(usize, usize, f64, f64)>,
    zones: Vec<Zone<R>>,
    rl: Vec<RlState>,
    debye: Vec<DebyeState>,
    ports: Vec<PortState>,
    pub traces: Vec<PortTrace>,
    pulse: Option<Pulse>,
    driven: usize,
    team: Team,
    slabs: usize,
    recorders: Vec<Recorder>,
    node_recorders: Vec<NodeRecorder>,
    simd: Simd,
    pub peak_energy: f64,
    /// Wave ports on this run, and which is driven.
    pub wave_ports: Vec<crate::waveport::WavePortRun>,
    wave_driven: Option<usize>,
    /// Where the stored energy sits, summed over time per plane along each axis, if asked for.
    profile: Option<[Vec<f64>; 3]>,
}

/// `out = Mε⁻¹ C̃ Mμ⁻¹ C v`, the operator whose largest eigenvalue limits the time step.
fn operator(asm: &Assembled, v: &[Vec<f64>; 3], h: &mut [Vec<f64>; 3], out: &mut [Vec<f64>; 3]) {
    let strides = asm.grid.strides();
    let [nx, ny, nz] = asm.grid.n();
    for a in 0..3 {
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        let (s1, s2) = (strides[t1], strides[t2]);
        let (e1, e2) = (&v[t1], &v[t2]);
        let imu = &asm.imu[a];
        h[a].par_chunks_mut(nx * ny).enumerate().for_each(|(k, layer)| {
            if k + 1 >= nz + usize::from(a == 2) {
                return;
            }
            for j in 0..ny {
                if j + 1 >= ny + usize::from(a == 1) {
                    continue;
                }
                for i in 0..nx {
                    if i + 1 >= nx + usize::from(a == 0) {
                        continue;
                    }
                    let p = i + nx * (j + ny * k);
                    let c = (e1[p + s2] - e1[p]) - (e2[p + s1] - e2[p]);
                    layer[i + nx * j] = imu[p] * c;
                }
            }
        });
    }
    for a in 0..3 {
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        let (s1, s2) = (strides[t1], strides[t2]);
        let (h1, h2) = (&h[t1], &h[t2]);
        let me = &asm.me[a];
        out[a].par_chunks_mut(nx * ny).enumerate().for_each(|(k, layer)| {
            layer.iter_mut().for_each(|x| *x = 0.0);
            if k == 0 && a != 2 {
                return;
            }
            for j in 0..ny {
                if j == 0 && a != 1 {
                    continue;
                }
                for i in 0..nx {
                    if i == 0 && a != 0 {
                        continue;
                    }
                    let p = i + nx * (j + ny * k);
                    let m = me[p];
                    if !m.is_finite() || m <= 0.0 {
                        continue;
                    }
                    // The stencil as the time step has it is minus the operator.
                    layer[i + nx * j] = -((h2[p] - h2[p - s1]) - (h1[p] - h1[p - s2])) / m;
                }
            }
        });
    }
}

/// Largest eigenvalue of a symmetric tridiagonal matrix, by bisection on its Sturm sequence.
fn largest_tridiagonal(alpha: &[f64], beta: &[f64]) -> f64 {
    let n = alpha.len();
    let mut hi = 0.0f64;
    for i in 0..n {
        let b0 = if i > 0 { beta[i - 1].abs() } else { 0.0 };
        let b1 = if i < beta.len() { beta[i].abs() } else { 0.0 };
        hi = hi.max(alpha[i] + b0 + b1);
    }
    let mut lo = 0.0f64;
    // How many eigenvalues lie above x.
    let above = |x: f64| -> usize {
        let mut count = 0;
        let mut d = 1.0f64;
        for i in 0..n {
            let b2 = if i > 0 { beta[i - 1] * beta[i - 1] } else { 0.0 };
            d = alpha[i] - x - if i > 0 { b2 / if d == 0.0 { f64::MIN_POSITIVE } else { d } } else { 0.0 };
            if d > 0.0 {
                count += 1;
            }
        }
        count
    };
    for _ in 0..100 {
        let mid = (lo + hi) * 0.5;
        if above(mid) > 0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// The time step, from the operator itself: the largest eigenvalue of `Mε⁻¹ C̃ Mμ⁻¹ C`, measured by
/// Lanczos. Whatever the subcell metal did to the diagonals, this is what limits it. A few edges
/// standing a little above the rest are exactly what power iteration takes hundreds of steps to
/// find and Lanczos finds in dozens.
pub fn stable_dt(asm: &Assembled) -> f64 {
    // Its own pool, sized to the model: each iteration is a dozen short bursts, and on a pool of every
    // core those bursts spend longer waking threads than working. Measured at 6 to 9 s on 0.2 to 0.6 M
    // cells, more than the run itself.
    let threads = ((asm.grid.nodes() / 40_000).clamp(1, 24)).min(std::thread::available_parallelism().map_or(8, |n| n.get()));
    match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(|| lanczos_dt(asm)),
        Err(_) => lanczos_dt(asm),
    }
}

fn lanczos_dt(asm: &Assembled) -> f64 {
    let n = asm.grid.nodes();
    let mut v: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    for a in 0..3 {
        for p in 0..n {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            if asm.me[a][p].is_finite() && asm.me[a][p] > 0.0 {
                v[a][p] = (seed as f64 / u64::MAX as f64) - 0.5;
            }
        }
    }
    // The operator is symmetric in the Mε inner product.
    let inner = |x: &[Vec<f64>; 3], y: &[Vec<f64>; 3]| -> f64 {
        (0..3).map(|a| x[a].par_iter().zip(&y[a]).zip(&asm.me[a]).map(|((u, w), m)| if m.is_finite() { u * w * m } else { 0.0 }).sum::<f64>()).sum()
    };
    let norm = inner(&v, &v).max(1e-300).sqrt();
    v.iter_mut().for_each(|c| c.par_iter_mut().for_each(|x| *x /= norm));
    let mut prev: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
    let mut w: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
    let mut h: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
    let (mut alpha, mut beta) = (Vec::new(), Vec::new());
    let mut last_beta = 0.0;
    const ITERATIONS: usize = 80;
    for _ in 0..ITERATIONS {
        operator(asm, &v, &mut h, &mut w);
        let al = inner(&w, &v);
        alpha.push(al);
        for a in 0..3 {
            let (vv, pp) = (&v[a], &prev[a]);
            w[a].par_iter_mut().zip(vv.par_iter().zip(pp)).for_each(|(x, (y, z))| *x -= al * y + last_beta * z);
        }
        let b = inner(&w, &w).max(0.0).sqrt();
        if b < 1e-9 * al.abs().max(1e-300) {
            break;
        }
        beta.push(b);
        last_beta = b;
        for a in 0..3 {
            std::mem::swap(&mut prev[a], &mut v[a]);
            let (src, dst) = (&w[a], &mut v[a]);
            dst.par_iter_mut().zip(src).for_each(|(x, y)| *x = y / b);
        }
    }
    beta.truncate(alpha.len().saturating_sub(1));
    let lambda = largest_tridiagonal(&alpha, &beta);
    // Margin against the little Lanczos leaves, and the absorber's small perturbation.
    0.96 * 2.0 / lambda.max(1e-300).sqrt()
}

impl<R: Real> Engine<R> {
    pub fn new(asm: &Assembled, dt: f64, threads: usize) -> Self {
        let n = asm.grid.n();
        let nodes = asm.grid.nodes();
        let mut ce: [Vec<R>; 3] = std::array::from_fn(|a| asm.me[a].par_iter().map(|m| if m.is_finite() && *m > 0.0 { R::of(dt / m) } else { R::default() }).collect());
        let ch: [Vec<R>; 3] = std::array::from_fn(|a| asm.imu[a].par_iter().map(|m| R::of(dt * m)).collect());
        let mut lossy = Vec::new();
        for a in 0..3 {
            if let Some(g) = &asm.g[a] {
                for p in 0..nodes {
                    if g[p] > 0.0 && asm.me[a][p].is_finite() {
                        let s = g[p] * dt / (2.0 * asm.me[a][p]);
                        lossy.push((a, p, (1.0 - s) / (1.0 + s), 1.0 / (1.0 + s)));
                    }
                }
            }
        }
        // Edges with their own update get nothing from the main loop.
        let m_dt = |a: usize, p: usize| asm.me[a][p] / dt;
        let mut rl = Vec::with_capacity(asm.rl.len());
        for e in &asm.rl {
            let m = asm.me[e.axis][e.node];
            let den = e.l / dt + e.r / 2.0;
            let (alpha, beta) = ((e.l / dt - e.r / 2.0) / den, 0.5 / den);
            ce[e.axis][e.node] = R::default();
            rl.push(RlState { axis: e.axis, node: e.node, m_dt: m / dt, alpha, beta, i: 0.0 });
        }
        let mut debye = Vec::with_capacity(asm.debye.len());
        for d in &asm.debye {
            let a = dt / (2.0 * d.r * d.c);
            ce[d.axis][d.node] = R::default();
            debye.push(DebyeState { axis: d.axis, node: d.node, m_dt: m_dt(d.axis, d.node), gd: 1.0 / (d.r * (1.0 + a)), a, v: 0.0 });
        }
        let mut ports = Vec::with_capacity(asm.ports.len());
        for p in &asm.ports {
            let mut edges = Vec::with_capacity(p.edges.len());
            for &(node, r) in &p.edges {
                ce[p.axis][node] = R::default();
                edges.push(PortEdge { node, m_dt: m_dt(p.axis, node), g: 1.0 / r, mid: p.mid.contains(&node) });
            }
            let series = p.columns.first().map_or(1, |c| c.len()) as f64;
            ports.push(PortState { axis: p.axis, sign: p.sign, edges, series, columns: p.columns.clone() });
        }
        let traces = asm.ports.iter().map(|p| PortTrace { z0: p.z0, ..Default::default() }).collect();
        let threads = if threads == 0 { std::thread::available_parallelism().map_or(8, |v| v.get()) / 2 } else { threads };
        let threads = threads.max(1);
        let team = Team::new(threads);
        // More slabs than workers, so one that loses its core costs only its share.
        let slabs = (threads * 4).min(n[2]).max(1);
        Self {
            n,
            dt,
            step: 0,
            e: std::array::from_fn(|_| vec![R::default(); nodes]),
            h: std::array::from_fn(|_| vec![R::default(); nodes]),
            ce,
            ch,
            lossy,
            zones: make_zones(asm, dt),
            rl,
            debye,
            ports,
            traces,
            pulse: None,
            driven: 0,
            team,
            slabs,
            recorders: Vec::new(),
            node_recorders: Vec::new(),
            simd: detect_simd(),
            peak_energy: 0.0,
            wave_ports: Vec::new(),
            wave_driven: None,
            profile: None,
        }
    }

    /// Adds dumps of the field at grid nodes.
    pub fn record_nodes(&mut self, grid: &crate::grid::Grid, dumps: &[NodeDump], every: u64) {
        for d in dumps {
            self.node_recorders.push(NodeRecorder::new(grid, d.clone(), every));
        }
    }

    pub fn node_planes(&self) -> Vec<NodePlane> {
        self.node_recorders.iter().flat_map(|r| r.planes()).collect()
    }

    /// Adds a wave port. Solved once here; the same mode serves every run on this mesh.
    pub fn add_wave_port(&mut self, run: crate::waveport::WavePortRun) {
        self.wave_ports.push(run);
    }

    /// Drives a wave port with `pulse` in place of any lumped port.
    pub fn drive_wave(&mut self, port: usize, pulse: Pulse) {
        self.wave_driven = Some(port);
        self.pulse = Some(pulse);
        self.driven = usize::MAX;
    }

    /// Starts collecting where the energy sits, for choosing where to refine next.
    pub fn track_profile(&mut self) {
        self.profile = Some(std::array::from_fn(|a| vec![0.0; self.n[a]]));
    }

    pub fn profile(&self) -> Option<&[Vec<f64>; 3]> {
        self.profile.as_ref()
    }

    /// Adds the present field energy to the per-plane totals.
    fn sample_profile(&mut self) {
        let Some(mut prof) = self.profile.take() else { return };
        let [nx, ny, nz] = self.n;
        let slabs = self.slabs;
        let shared = std::sync::Mutex::new(&mut prof);
        let (e, h, ce, ch) = (&self.e, &self.h, &self.ce, &self.ch);
        self.team.run(slabs, &|s| {
            let mut local: [Vec<f64>; 3] = [vec![0.0; nx], vec![0.0; ny], vec![0.0; nz]];
            for k in s * nz / slabs..(s + 1) * nz / slabs {
                for j in 0..ny {
                    for i in 0..nx {
                        let p = i + nx * (j + ny * k);
                        let mut w = 0.0;
                        for a in 0..3 {
                            let (ke, kh) = (ce[a][p].f(), ch[a][p].f());
                            if ke > 0.0 {
                                let v = e[a][p].f();
                                w += v * v / ke;
                            }
                            if kh > 0.0 {
                                let v = h[a][p].f();
                                w += v * v / kh;
                            }
                        }
                        local[0][i] += w;
                        local[1][j] += w;
                        local[2][k] += w;
                    }
                }
            }
            let mut g = shared.lock().unwrap();
            for a in 0..3 {
                for (x, y) in g[a].iter_mut().zip(&local[a]) {
                    *x += y;
                }
            }
        });
        self.profile = Some(prof);
    }

    /// The driven wave port, H side: H next to the plane sees the incident E it would be missing.
    fn wave_h(&mut self) {
        let Some(pi) = self.wave_driven else { return };
        let Some(pulse) = self.pulse else { return };
        let n = self.n;
        let strides = [1, n[0], n[0] * n[1]];
        let t = self.step as f64 * self.dt;
        let run = &self.wave_ports[pi];
        let v = pulse.at(t);
        let a = run.def.axis;
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        let up = run.def.direction > 0.0;
        let sa = strides[a];
        for &(comp, node, val) in &run.mode.e {
            let e_inc = val * v;
            // For a wave along +axis, the scattered side is below the plane: H just above needs
            // the total E, and the same reasoning mirrored for a wave along -axis.
            let (target, sign) = if comp == t1 { (t2, if up { 1.0 } else { -1.0 }) } else { (t1, if up { -1.0 } else { 1.0 }) };
            let q = if up { node } else { node - sa };
            let c = self.ch[target][q].f();
            let cur = self.h[target][q].f();
            self.h[target][q] = R::of(cur + sign * c * e_inc);
        }
    }

    /// The driven wave port, E side: E on the plane sees H without the incident wave in it.
    fn wave_e(&mut self) {
        let Some(pi) = self.wave_driven else { return };
        let Some(pulse) = self.pulse else { return };
        let n = self.n;
        let strides = [1, n[0], n[0] * n[1]];
        let run = &self.wave_ports[pi];
        let t = (self.step as f64 + 0.5) * self.dt - run.half_cell_delay();
        let z0 = run.mode.z0;
        let a = run.def.axis;
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        let up = run.def.direction > 0.0;
        let _ = strides;
        // The incident current runs with the wave, so its sign along +axis is the wave direction.
        let i = pulse.at(t) / z0 * run.def.direction;
        for &(comp, node, val) in &run.mode.h {
            let h_inc = val * i;
            let (target, sign) = if comp == t2 { (t1, if up { 1.0 } else { -1.0 }) } else { (t2, if up { -1.0 } else { 1.0 }) };
            let c = self.ce[target][node].f();
            let cur = self.e[target][node].f();
            self.e[target][node] = R::of(cur + sign * c * h_inc);
        }
    }

    /// Reads every wave port: voltage after E, current as H was when E moved.
    fn wave_read(&mut self) {
        for run in &mut self.wave_ports {
            let u: f64 = run.read_e.iter().map(|&(c, q, w)| self.e[c][q].f() * w).sum();
            let i: f64 = run.read_h.iter().map(|&(c, q, w)| self.h[c][q].f() * w).sum();
            run.u.push(u);
            run.i.push(i);
        }
    }

    /// Adds a set of frequency-domain recordings to the run.
    pub fn record(&mut self, rec: Recorder) {
        self.recorders.push(rec);
    }

    pub fn recorders(&self) -> &[Recorder] {
        &self.recorders
    }

    /// Which port the pulse goes in at, or no pulse.
    pub fn drive(&mut self, port: usize, pulse: Pulse) {
        self.driven = port;
        self.wave_driven = None;
        self.pulse = Some(pulse);
    }

    /// Back to rest, for the next port's run on the same model.
    pub fn reset(&mut self) {
        for a in 0..3 {
            self.e[a].par_iter_mut().for_each(|v| *v = R::default());
            self.h[a].par_iter_mut().for_each(|v| *v = R::default());
        }
        for z in &mut self.zones {
            for v in &mut z.store {
                v.par_iter_mut().for_each(|x| *x = R::default());
            }
        }
        for r in &mut self.rl {
            r.i = 0.0;
        }
        for d in &mut self.debye {
            d.v = 0.0;
        }
        for r in &mut self.recorders {
            r.clear();
        }
        for r in &mut self.node_recorders {
            r.clear();
        }
        for t in &mut self.traces {
            t.u.clear();
            t.i.clear();
        }
        for w in &mut self.wave_ports {
            w.u.clear();
            w.i.clear();
        }
        self.step = 0;
        self.peak_energy = 0.0;
    }

    /// Rows of one z-layer of H, the absorber's part of it included.
    #[inline(always)]
    fn h_layer(ctx: &StepCtx<R>, k: usize) {
        let [nx, ny, nz] = ctx.n;
        let nl = nx * ny;
        let [ex, ey, ez] = ctx.e;
        let [cx, cy, cz] = ctx.ch;
        let simd = ctx.simd;
        // SAFETY for the slices below: a layer of a component is written by one worker.
        unsafe {
            if k + 1 < nz {
                // hx += c·((ey[k+1] - ey[k]) - (ez[j+1] - ez[j]))
                for j in 0..ny - 1 {
                    let r = nx * (j + ny * k);
                    let dst = std::slice::from_raw_parts_mut(ctx.hp[0].get().add(r), nx);
                    row(simd, dst, &cx[r..r + nx], &ey[r + nl..r + nl + nx], &ey[r..r + nx], &ez[r + nx..r + 2 * nx], &ez[r..r + nx]);
                }
                // hy += c·((ez[i+1] - ez[i]) - (ex[k+1] - ex[k]))
                for j in 0..ny {
                    let r = nx * (j + ny * k);
                    let m = nx - 1;
                    let dst = std::slice::from_raw_parts_mut(ctx.hp[1].get().add(r), m);
                    row(simd, dst, &cy[r..r + m], &ez[r + 1..r + 1 + m], &ez[r..r + m], &ex[r + nl..r + nl + m], &ex[r..r + m]);
                }
            }
            // hz += c·((ex[j+1] - ex[j]) - (ey[i+1] - ey[i]))
            for j in 0..ny - 1 {
                let r = nx * (j + ny * k);
                let m = nx - 1;
                let dst = std::slice::from_raw_parts_mut(ctx.hp[2].get().add(r), m);
                row(simd, dst, &cz[r..r + m], &ex[r + nx..r + nx + m], &ex[r..r + m], &ey[r + 1..r + 1 + m], &ey[r..r + m]);
            }
            for z in ctx.zones.iter() {
                z.h(ctx, k);
            }
        }
    }

    #[inline(always)]
    fn e_layer(ctx: &StepCtx<R>, k: usize) {
        let [nx, ny, nz] = ctx.n;
        let nl = nx * ny;
        let [hx, hy, hz] = ctx.h;
        let [cx, cy, cz] = ctx.ce;
        let simd = ctx.simd;
        // SAFETY for the slices below: a layer of a component is written by one worker.
        unsafe {
            if k >= 1 && k + 1 < nz {
                // ex += c·((hz[j] - hz[j-1]) - (hy[k] - hy[k-1]))
                for j in 1..ny - 1 {
                    let r = nx * (j + ny * k);
                    let m = nx - 1;
                    let dst = std::slice::from_raw_parts_mut(ctx.ep[0].get().add(r), m);
                    row(simd, dst, &cx[r..r + m], &hz[r..r + m], &hz[r - nx..r - nx + m], &hy[r..r + m], &hy[r - nl..r - nl + m]);
                }
                // ey += c·((hx[k] - hx[k-1]) - (hz[i] - hz[i-1]))
                for j in 0..ny - 1 {
                    let r = nx * (j + ny * k) + 1;
                    let m = nx - 2;
                    let dst = std::slice::from_raw_parts_mut(ctx.ep[1].get().add(r), m);
                    row(simd, dst, &cy[r..r + m], &hx[r..r + m], &hx[r - nl..r - nl + m], &hz[r..r + m], &hz[r - 1..r - 1 + m]);
                }
            }
            if k + 1 < nz {
                // ez += c·((hy[i] - hy[i-1]) - (hx[j] - hx[j-1]))
                for j in 1..ny - 1 {
                    let r = nx * (j + ny * k) + 1;
                    let m = nx - 2;
                    let dst = std::slice::from_raw_parts_mut(ctx.ep[2].get().add(r), m);
                    row(simd, dst, &cz[r..r + m], &hy[r..r + m], &hy[r - 1..r - 1 + m], &hx[r..r + m], &hx[r - nx..r - nx + m]);
                }
            }
            for z in ctx.zones.iter() {
                z.e(ctx, k);
            }
        }
    }

    /// `∮h` around an edge along `axis`: the current, conduction and displacement, through its
    /// dual face.
    #[inline(always)]
    fn curl_h(h: &[Vec<R>; 3], strides: [usize; 3], axis: usize, p: usize) -> f64 {
        let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
        let (s1, s2) = (strides[t1], strides[t2]);
        ((h[t2][p] - h[t2][p - s1]) - (h[t1][p] - h[t1][p - s2])).f()
    }

    fn specials(&mut self) {
        let n = self.n;
        let strides = [1, n[0], n[0] * n[1]];
        let ep: [Shared<R>; 3] = [Shared(self.e[0].as_mut_ptr()), Shared(self.e[1].as_mut_ptr()), Shared(self.e[2].as_mut_ptr())];
        {
            let h = &self.h;
            let ce = &self.ce;
            // Plain loss: the main loop already added c·curl with the unscaled c.
            if !self.lossy.is_empty() {
                let lossy = &self.lossy;
                let parts = lossy.len().div_ceil(4096);
                self.team.run(parts, &|s| {
                    for &(a, p, ca, cb) in &lossy[s * 4096..((s + 1) * 4096).min(lossy.len())] {
                        let c = ce[a][p].f();
                        let curl = Self::curl_h(h, strides, a, p);
                        // SAFETY: every edge appears once in this list.
                        unsafe {
                            let e = ep[a].get().add(p);
                            let old = (*e).f() - c * curl;
                            *e = R::of(ca * old + cb * c * curl);
                        }
                    }
                });
            }
            if !self.rl.is_empty() {
                let rp = Shared(self.rl.as_mut_ptr());
                let total = self.rl.len();
                let parts = total.div_ceil(4096);
                self.team.run(parts, &|s| {
                    for q in s * 4096..((s + 1) * 4096).min(total) {
                        // SAFETY: each state is in one part, and an edge carries one branch.
                        unsafe {
                            let st = &mut *rp.get().add(q);
                            let curl = Self::curl_h(h, strides, st.axis, st.node);
                            let e = ep[st.axis].get().add(st.node);
                            let old = (*e).f();
                            let next = (old * (st.m_dt - st.beta / 2.0) + curl - (1.0 + st.alpha) * st.i / 2.0) / (st.m_dt + st.beta / 2.0);
                            st.i = st.alpha * st.i + st.beta * (next + old);
                            *e = R::of(next);
                        }
                    }
                });
            }
            for st in self.debye.iter_mut() {
                let curl = Self::curl_h(h, strides, st.axis, st.node);
                // SAFETY: nothing else is running.
                unsafe {
                    let e = ep[st.axis].get().add(st.node);
                    let old = (*e).f();
                    let next = (old * (st.m_dt - st.gd / 2.0) + curl + st.gd * st.v) / (st.m_dt + st.gd / 2.0);
                    let mean = (next + old) * 0.5;
                    st.v = (st.v * (1.0 - st.a) + 2.0 * st.a * mean) / (1.0 + st.a);
                    *e = R::of(next);
                }
            }
        }
        // Ports: an ideal source behind the port's resistance, half a step on from the last E.
        let t = (self.step as f64 + 0.5) * self.dt;
        let v_s = self.pulse.map_or(0.0, |p| p.at(t));
        let h = &self.h;
        for (pi, port) in self.ports.iter().enumerate() {
            let a = port.axis;
            let src = if pi == self.driven && self.pulse.is_some() { -port.sign * v_s / port.series } else { 0.0 };
            // The current is the one through the port's own resistor, across the middle of the gap. The
            // loop of H around the gap would miss the displacement current of the gap's own cell, and so
            // leave part of the gap's capacitance out of the impedance. A real chip sits across all of it.
            let mut current = 0.0;
            for edge in &port.edges {
                let curl = Self::curl_h(h, strides, a, edge.node);
                // SAFETY: only this loop writes port edges.
                unsafe {
                    let e = ep[a].get().add(edge.node);
                    let old = (*e).f();
                    let next = (old * (edge.m_dt - edge.g / 2.0) + curl + edge.g * src) / (edge.m_dt + edge.g / 2.0);
                    *e = R::of(next);
                    if edge.mid {
                        current += edge.g * ((next + old) * 0.5 - src);
                    }
                }
            }
            let mut voltage = 0.0;
            for col in &port.columns {
                for &node in col {
                    // SAFETY: nothing else writes here now.
                    voltage += unsafe { (*ep[a].get().add(node)).f() };
                }
            }
            let trace = &mut self.traces[pi];
            trace.u.push(-port.sign * voltage / port.columns.len().max(1) as f64);
            trace.i.push(port.sign * current);
        }
    }

    /// One full step: H to `n + ½`, E to `n + 1`.
    pub fn advance(&mut self) {
        let nz = self.n[2];
        let slabs = self.slabs;
        let ep: [Shared<R>; 3] = [Shared(self.e[0].as_mut_ptr()), Shared(self.e[1].as_mut_ptr()), Shared(self.e[2].as_mut_ptr())];
        let hp: [Shared<R>; 3] = [Shared(self.h[0].as_mut_ptr()), Shared(self.h[1].as_mut_ptr()), Shared(self.h[2].as_mut_ptr())];
        {
            let ctx = StepCtx { n: self.n, e: &self.e, h: &self.h, ce: &self.ce, ch: &self.ch, ep, hp, zones: &self.zones, simd: self.simd };
            self.team.run(slabs, &|s| {
                for k in s * nz / slabs..(s + 1) * nz / slabs {
                    Self::h_layer(&ctx, k);
                }
            });
        }
        self.wave_h();
        {
            let ctx = StepCtx { n: self.n, e: &self.e, h: &self.h, ce: &self.ce, ch: &self.ch, ep, hp, zones: &self.zones, simd: self.simd };
            self.team.run(slabs, &|s| {
                for k in s * nz / slabs..(s + 1) * nz / slabs {
                    Self::e_layer(&ctx, k);
                }
            });
        }
        self.wave_e();
        self.specials();
        self.wave_read();
        for rec in &mut self.recorders {
            rec.sample(self.step + 1, self.dt, &self.e, &self.h, &self.team);
        }
        for rec in &mut self.node_recorders {
            rec.sample(self.step + 1, self.dt, &self.e, &self.h, &self.team);
        }
        self.step += 1;
    }

    /// `count` steps.
    pub fn advance_many(&mut self, count: u64) {
        for _ in 0..count {
            self.advance();
        }
    }

    /// Stored field energy.
    pub fn energy(&self) -> f64 {
        let nz = self.n[2];
        let nl = self.n[0] * self.n[1];
        let slabs = self.slabs;
        let total = std::sync::Mutex::new(0.0f64);
        self.team.run(slabs, &|s| {
            let mut sum = 0.0;
            let (from, to) = (s * nz / slabs * nl, (s + 1) * nz / slabs * nl);
            for a in 0..3 {
                for (v, k) in self.e[a][from..to].iter().zip(&self.ce[a][from..to]).chain(self.h[a][from..to].iter().zip(&self.ch[a][from..to])) {
                    let k = k.f();
                    if k > 0.0 {
                        let v = v.f();
                        sum += v * v / k;
                    }
                }
            }
            *total.lock().unwrap() += sum;
        });
        let w = *total.lock().unwrap();
        w * 0.5 * self.dt
    }

    /// Runs until the energy has fallen `end_db` below its peak, after the pulse has gone in.
    /// `tick` sees progress and may ask to stop by returning false.
    pub fn run(&mut self, end_db: f64, max_steps: u64, mut tick: impl FnMut(u64, f64) -> bool) -> Stop {
        let period = 64;
        let source_end = self.pulse.map_or(0.0, |p| 2.0 * p.delay());
        let limit = 10f64.powf(-end_db / 10.0);
        while self.step < max_steps {
            self.advance_many(period);
            let w = self.energy();
            if !w.is_finite() {
                return Stop::Diverged;
            }
            let t = self.step as f64 * self.dt;
            // Past the source nothing can add energy, so any real growth means the update is unstable.
            // The peak has to stop following it from there, or the test could never fire.
            if t > source_end && w > 2.0 * self.peak_energy.max(1e-300) {
                return Stop::Diverged;
            }
            if t <= source_end {
                self.peak_energy = self.peak_energy.max(w);
            }
            self.sample_profile();
            let db = if self.peak_energy > 0.0 { 10.0 * (w / self.peak_energy).max(1e-300).log10() } else { 0.0 };
            if !tick(self.step, db) {
                return Stop::Cancelled;
            }
            if t > source_end && w < self.peak_energy * limit {
                return Stop::Decayed;
            }
        }
        Stop::StepLimit
    }
}

/// What a half step's workers read and write.
struct StepCtx<'a, R: Real> {
    n: [usize; 3],
    e: &'a [Vec<R>; 3],
    h: &'a [Vec<R>; 3],
    ce: &'a [Vec<R>; 3],
    ch: &'a [Vec<R>; 3],
    ep: [Shared<R>; 3],
    hp: [Shared<R>; 3],
    zones: &'a [Zone<R>],
    simd: Simd,
}
// SAFETY: as for `Shared`: writes through its pointers are to disjoint layers.
unsafe impl<R: Real> Sync for StepCtx<'_, R> {}

impl<R: Real> Zone<R> {
    /// This zone's part of z-layer `k` of a half step. Each of the three axes meets the layers
    /// differently, which `visit` hides.
    #[inline(always)]
    fn visit(&self, n: [usize; 3], k: usize, layers: usize, plane_of: impl Fn(usize) -> usize, mut f: impl FnMut(usize, usize, usize)) {
        let (t1, t2) = ((self.axis + 1) % 3, (self.axis + 2) % 3);
        let (n1, n2) = (n[t1], n[t2]);
        match self.axis {
            // Rows of t2 = z: this layer is one value of t2.
            0 => {
                for li in 0..layers {
                    for i1 in 0..n1 {
                        f(li, i1, k);
                    }
                }
            }
            // t1 = z: this layer is one value of t1.
            1 => {
                for li in 0..layers {
                    for i2 in 0..n2 {
                        f(li, k, i2);
                    }
                }
            }
            // The absorber's planes are themselves z-layers.
            _ => {
                for li in 0..layers {
                    if plane_of(li) == k {
                        for i2 in 0..n2 {
                            for i1 in 0..n1 {
                                f(li, i1, i2);
                            }
                        }
                    }
                }
            }
        }
    }

    /// SAFETY: each `(layer, i1, i2)` of a zone maps to one z-layer, so workers on different
    /// z-layers touch different memory variables and field entries.
    #[inline(always)]
    unsafe fn h(&self, ctx: &StepCtx<R>, k: usize) {
        let n = ctx.n;
        let strides = [1, n[0], n[0] * n[1]];
        let a = self.axis;
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        let (s_a, s1, s2) = (strides[a], strides[t1], strides[t2]);
        let n1 = n[t1];
        let nl = self.h_layers.len();
        let (e1, e2) = (&ctx.e[t1], &ctx.e[t2]);
        let (c1, c2) = (&ctx.ch[t1], &ctx.ch[t2]);
        let (h1, h2) = (ctx.hp[t1].get(), ctx.hp[t2].get());
        let (p1, p2) = (self.psi_h1.get(), self.psi_h2.get());
        let minus = R::of(-1.0);
        self.visit(n, k, nl, |li| self.h_layers[li].plane, |li, i1, i2| unsafe {
            let lay = self.h_layers.get_unchecked(li);
            let q = lay.plane * s_a + i1 * s1 + i2 * s2;
            let w = i2 * nl * n1 + li * n1 + i1;
            // H_t1 gets +∂a E_t2, H_t2 gets -∂a E_t1.
            let d2 = *e2.get_unchecked(q + s_a) - *e2.get_unchecked(q);
            let psi = lay.b * *p1.add(w) + lay.a * d2;
            *p1.add(w) = psi;
            *h1.add(q) += *c1.get_unchecked(q) * (lay.kinv_m1 * d2 + psi);
            let d1 = *e1.get_unchecked(q + s_a) - *e1.get_unchecked(q);
            let psi = lay.b * *p2.add(w) + lay.a * d1;
            *p2.add(w) = psi;
            *h2.add(q) += minus * *c2.get_unchecked(q) * (lay.kinv_m1 * d1 + psi);
        });
    }

    /// SAFETY: as for `h`.
    #[inline(always)]
    unsafe fn e(&self, ctx: &StepCtx<R>, k: usize) {
        let n = ctx.n;
        let strides = [1, n[0], n[0] * n[1]];
        let a = self.axis;
        let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
        let (s_a, s1, s2) = (strides[a], strides[t1], strides[t2]);
        let n1 = n[t1];
        let nl = self.e_layers.len();
        let (h1, h2) = (&ctx.h[t1], &ctx.h[t2]);
        let (c1, c2) = (&ctx.ce[t1], &ctx.ce[t2]);
        let (e1, e2) = (ctx.ep[t1].get(), ctx.ep[t2].get());
        let (p1, p2) = (self.psi_e1.get(), self.psi_e2.get());
        let minus = R::of(-1.0);
        self.visit(n, k, nl, |li| self.e_layers[li].plane, |li, i1, i2| unsafe {
            let lay = self.e_layers.get_unchecked(li);
            let q = lay.plane * s_a + i1 * s1 + i2 * s2;
            let w = i2 * nl * n1 + li * n1 + i1;
            // E_t1 gets -∂a H_t2, E_t2 gets +∂a H_t1.
            let d2 = *h2.get_unchecked(q) - *h2.get_unchecked(q - s_a);
            let psi = lay.b * *p1.add(w) + lay.a * d2;
            *p1.add(w) = psi;
            *e1.add(q) += minus * *c1.get_unchecked(q) * (lay.kinv_m1 * d2 + psi);
            let d1 = *h1.get_unchecked(q) - *h1.get_unchecked(q - s_a);
            let psi = lay.b * *p2.add(w) + lay.a * d1;
            *p2.add(w) = psi;
            *e2.add(q) += *c2.get_unchecked(q) * (lay.kinv_m1 * d1 + psi);
        });
    }
}


/// Discrete Fourier transform of a port's trace, at time offset `offset` steps from step 0.
pub fn dft(trace: &[f64], dt: f64, offset: f64, f: f64) -> num_complex::Complex64 {
    let w = 2.0 * std::f64::consts::PI * f * dt;
    // Recurrence for the phasor: this runs for every frequency of the sweep.
    let step = num_complex::Complex64::from_polar(1.0, -w);
    let mut ph = num_complex::Complex64::from_polar(1.0, -w * offset);
    let mut s = num_complex::Complex64::new(0.0, 0.0);
    for (n, v) in trace.iter().enumerate() {
        s += ph * *v;
        ph *= step;
        if n & 0xFFF == 0xFFF {
            // Rounding builds up over a million multiplications.
            ph = num_complex::Complex64::from_polar(1.0, -w * (offset + (n + 1) as f64));
        }
    }
    s * dt
}

/// One port's voltage, current, incident and reflected waves at `f`.
#[derive(Clone, Copy, Debug)]
pub struct Waves {
    pub u: num_complex::Complex64,
    pub i: num_complex::Complex64,
    pub inc: num_complex::Complex64,
    pub refl: num_complex::Complex64,
}

pub fn waves(trace: &PortTrace, dt: f64, f: f64) -> Waves {
    let u = dft(&trace.u, dt, 1.0, f);
    let i = dft(&trace.i, dt, 0.5, f);
    let inc = (u + i * trace.z0) * 0.5;
    Waves { u, i, inc, refl: u - inc }
}
