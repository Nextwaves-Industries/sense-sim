//! Frequency-domain fields recorded during a run: the faces of the near-field box, for the far
//! field, and field planes to look at.
//!
//! Fields are collected at the centres of a plane's cells, where the four tangential components
//! can be averaged to one place. H lags E by half a step, which each sample's phase accounts for,
//! so the box's Poynting flux matches the port's accepted power.

use num_complex::Complex64 as C;

use crate::grid::Grid;
use crate::real::Real;
use crate::team::Team;

/// A rectangle of cells in a plane of constant `axis`, at line `plane`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub axis: usize,
    pub plane: usize,
    /// Cell ranges along `axis + 1` and `axis + 2`.
    pub r1: (usize, usize),
    pub r2: (usize, usize),
}

/// A window's transforms at one frequency, per cell of the window: E along `axis + 1`, E along
/// `axis + 2`, the same for H, then E along `axis` itself, for looking at.
#[derive(Clone, Debug)]
pub struct Plane {
    pub window: Window,
    pub f: f64,
    pub data: Vec<[C; 5]>,
}

impl Plane {
    pub fn dims(&self) -> (usize, usize) {
        (self.window.r1.1 - self.window.r1.0, self.window.r2.1 - self.window.r2.0)
    }
}

struct Series {
    window: Window,
    /// `[freq][cell]`, so a frequency can be taken away at the end.
    data: Vec<Vec<[C; 5]>>,
    // Per axis: lengths used to turn voltages back into field values.
    l1: Vec<f64>,
    l2: Vec<f64>,
    d1: Vec<f64>,
    d2: Vec<f64>,
    // Along the normal: the edge above the plane and the one below.
    up: f64,
    down: f64,
}

/// Records windows at a set of frequencies, every `every` steps.
pub struct Recorder {
    freqs: Vec<f64>,
    every: u64,
    n: [usize; 3],
    series: Vec<Series>,
}

impl Recorder {
    pub fn new(grid: &Grid, windows: &[Window], freqs: &[f64], every: u64) -> Self {
        let series = windows
            .iter()
            .map(|w| {
                let (t1, t2) = ((w.axis + 1) % 3, (w.axis + 2) % 3);
                let cells = (w.r1.1 - w.r1.0) * (w.r2.1 - w.r2.0);
                let lens = |t: usize, r: (usize, usize), prim: bool| -> Vec<f64> {
                    // One extra entry: H needs the dual length at both ends of each cell.
                    (r.0..=r.1).map(|i| if prim { grid.primal(t, i) } else { grid.dual(t, i) }).collect()
                };
                Series { window: *w, data: vec![vec![[C::new(0.0, 0.0); 5]; cells]; freqs.len()], l1: lens(t1, w.r1, true), l2: lens(t2, w.r2, true), d1: lens(t1, w.r1, false), d2: lens(t2, w.r2, false), up: grid.primal(w.axis, w.plane), down: grid.primal(w.axis, w.plane - 1) }
            })
            .collect();
        Self { freqs: freqs.to_vec(), every: every.max(1), n: grid.n(), series }
    }

    /// Adds the fields at the end of step `step`: E at `step·dt`, H half a step before.
    pub fn sample<R: Real>(&mut self, step: u64, dt: f64, e: &[Vec<R>; 3], h: &[Vec<R>; 3], team: &Team) {
        if step % self.every != 0 {
            return;
        }
        let n = self.n;
        let strides = [1, n[0], n[0] * n[1]];
        let weight = self.every as f64 * dt;
        for (fi, f) in self.freqs.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * f;
            let ph_e = C::from_polar(weight, -w * step as f64 * dt);
            let ph_h = C::from_polar(weight, -w * (step as f64 - 0.5) * dt);
            for s in &mut self.series {
                let win = s.window;
                let (t1, t2) = ((win.axis + 1) % 3, (win.axis + 2) % 3);
                let (st1, st2, sa) = (strides[t1], strides[t2], strides[win.axis]);
                let (m1, m2) = (win.r1.1 - win.r1.0, win.r2.1 - win.r2.0);
                let (e1, e2, h1, h2, ea) = (&e[t1], &e[t2], &h[t1], &h[t2], &e[win.axis]);
                let (l1, l2, d1, d2, up, down) = (&s.l1, &s.l2, &s.d1, &s.d2, s.up, s.down);
                let parts = m2.min(team.threads() * 4).max(1);
                let out = Shared(s.data[fi].as_mut_ptr());
                team.run(parts, &|p| {
                    for b in p * m2 / parts..(p + 1) * m2 / parts {
                        for a in 0..m1 {
                            let q = win.plane * sa + (win.r1.0 + a) * st1 + (win.r2.0 + b) * st2;
                            // E along t1 sits on the cell's edges at both ends of t2, and so on.
                            let ev1 = (e1[q].f() + e1[q + st2].f()) * 0.5 / l1[a];
                            let ev2 = (e2[q].f() + e2[q + st1].f()) * 0.5 / l2[b];
                            // H along t1 sits at the plane ± half a cell, on lines of t1.
                            let hv1 = (h1[q].f() / d1[a] + h1[q + st1].f() / d1[a + 1] + h1[q - sa].f() / d1[a] + h1[q - sa + st1].f() / d1[a + 1]) * 0.25;
                            let hv2 = (h2[q].f() / d2[b] + h2[q + st2].f() / d2[b + 1] + h2[q - sa].f() / d2[b] + h2[q - sa + st2].f() / d2[b + 1]) * 0.25;
                            let corners = |p: usize| ea[p].f() + ea[p + st1].f() + ea[p + st2].f() + ea[p + st1 + st2].f();
                            let en = (corners(q) / up + corners(q - sa) / down) * 0.125;
                            // SAFETY: each cell of a frequency's series is written by one part.
                            unsafe {
                                let cell = &mut *out.get().add(a + m1 * b);
                                cell[0] += ph_e * ev1;
                                cell[1] += ph_e * ev2;
                                cell[2] += ph_h * hv1;
                                cell[3] += ph_h * hv2;
                                cell[4] += ph_e * en;
                            }
                        }
                    }
                });
            }
        }
    }

    /// Adds one sample of window `win` from values already at the cell centres, as `(E1, E2, H1, H2, En)`
    /// per cell: what the GPU engine hands back.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub fn add_cells(&mut self, step: u64, dt: f64, win: usize, values: &[f32]) {
        let weight = self.every as f64 * dt;
        for (fi, f) in self.freqs.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * f;
            let ph_e = C::from_polar(weight, -w * step as f64 * dt);
            let ph_h = C::from_polar(weight, -w * (step as f64 - 0.5) * dt);
            let cells = &mut self.series[win].data[fi];
            for (cell, v) in cells.iter_mut().zip(values.chunks_exact(5)) {
                cell[0] += ph_e * v[0] as f64;
                cell[1] += ph_e * v[1] as f64;
                cell[2] += ph_h * v[2] as f64;
                cell[3] += ph_h * v[3] as f64;
                cell[4] += ph_e * v[4] as f64;
            }
        }
    }

    pub fn clear(&mut self) {
        for s in &mut self.series {
            for f in &mut s.data {
                f.iter_mut().for_each(|c| *c = [C::new(0.0, 0.0); 5]);
            }
        }
    }

    /// What was recorded, one entry per window and frequency.
    pub fn planes(&self) -> Vec<Plane> {
        let mut out = Vec::new();
        for s in &self.series {
            for (fi, f) in self.freqs.iter().enumerate() {
                out.push(Plane { window: s.window, f: *f, data: s.data[fi].clone() });
            }
        }
        out
    }
}


/// Fields at a tensor of grid nodes through a run, at some frequencies: what a dump to a file asks
/// for. E is averaged from the two edges meeting at a node and H from the four faces around its
/// line, then divided by their lengths to give field values.
#[derive(Clone, Debug)]
pub struct NodeDump {
    pub name: String,
    pub magnetic: bool,
    pub freqs: Vec<f64>,
    /// Line indices along each axis.
    pub lines: [Vec<usize>; 3],
}

/// One dump's result at one frequency, in the first axis fastest.
#[derive(Clone, Debug)]
pub struct NodePlane {
    pub name: String,
    pub f: f64,
    pub lines: [Vec<usize>; 3],
    pub values: Vec<[C; 3]>,
}

/// Per node: its own index and the index one line lower along each axis, the node itself at the low
/// end of the grid, so nothing reads below the arrays. With the weights that turn voltages into field
/// values.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeEntry {
    pub p: u32,
    pub lo: [u32; 3],
    /// E: weights of the edge below and the edge above, per axis. H: the weight per axis, twice.
    pub w: [[f32; 2]; 3],
}

/// Records one dump. The transform is in double precision on the CPU, whichever engine stepped.
pub struct NodeRecorder {
    pub dump: NodeDump,
    pub table: Vec<NodeEntry>,
    every: u64,
    data: Vec<Vec<[C; 3]>>,
    /// As openEMS's transform, so a dump's scale matches its probes: twice the sum.
    weight: f64,
}

/// The node table of a dump on a grid.
pub fn node_table(grid: &Grid, dump: &NodeDump) -> Vec<NodeEntry> {
    let [sx, sy, sz] = grid.strides();
    let strides = [sx, sy, sz];
    let mut out = Vec::with_capacity(dump.lines.iter().map(|l| l.len()).product());
    for &k in &dump.lines[2] {
        for &j in &dump.lines[1] {
            for &i in &dump.lines[0] {
                let ix = [i, j, k];
                let p = grid.idx(i, j, k);
                let mut e = NodeEntry { p: p as u32, ..Default::default() };
                for a in 0..3 {
                    e.lo[a] = if ix[a] > 0 { (p - strides[a]) as u32 } else { p as u32 };
                    if dump.magnetic {
                        let d = grid.dual(a, ix[a]);
                        let w = if d > 0.0 { (1.0 / d) as f32 } else { 0.0 };
                        e.w[a] = [w, w];
                    } else {
                        let lo = if ix[a] > 0 { grid.primal(a, ix[a] - 1) } else { 0.0 };
                        let hi = grid.primal(a, ix[a]);
                        // Half of each where both exist, all of the one at the end of the grid.
                        let share = if lo > 0.0 && hi > 0.0 { 0.5 } else { 1.0 };
                        e.w[a] = [if lo > 0.0 { (share / lo) as f32 } else { 0.0 }, if hi > 0.0 { (share / hi) as f32 } else { 0.0 }];
                    }
                }
                out.push(e);
            }
        }
    }
    out
}

/// The field vector at one node.
#[inline(always)]
pub fn node_value<R: Real>(n: &NodeEntry, magnetic: bool, f: &[Vec<R>; 3]) -> [f64; 3] {
    let p = n.p as usize;
    if magnetic {
        // H along a sits on the four faces around this node's line, in the other two axes.
        std::array::from_fn(|a| {
            let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
            let (l1, l2) = (n.lo[t1] as usize, n.lo[t2] as usize);
            let diag = l1 + l2 - p;
            (f[a][p].f() + f[a][l1].f() + f[a][l2].f() + f[a][diag].f()) * 0.25 * n.w[a][0] as f64
        })
    } else {
        std::array::from_fn(|a| f[a][n.lo[a] as usize].f() * n.w[a][0] as f64 + f[a][p].f() * n.w[a][1] as f64)
    }
}

impl NodeRecorder {
    pub fn new(grid: &Grid, dump: NodeDump, every: u64) -> Self {
        let table = node_table(grid, &dump);
        let data = vec![vec![[C::new(0.0, 0.0); 3]; table.len()]; dump.freqs.len()];
        Self { dump, table, every: every.max(1), data, weight: 2.0 }
    }

    /// Adds one sample of the fields at the end of `step`.
    pub fn add(&mut self, step: u64, dt: f64, values: &[f32]) {
        // E is at `step`, H half a step before it.
        let t = if self.dump.magnetic { step as f64 - 0.5 } else { step as f64 } * dt;
        let w = self.weight * self.every as f64 * dt;
        for (fi, f) in self.dump.freqs.iter().enumerate() {
            let ph = C::from_polar(w, -2.0 * std::f64::consts::PI * f * t);
            for (cell, v) in self.data[fi].iter_mut().zip(values.chunks_exact(3)) {
                cell[0] += ph * v[0] as f64;
                cell[1] += ph * v[1] as f64;
                cell[2] += ph * v[2] as f64;
            }
        }
    }

    /// Samples from CPU arrays.
    pub fn sample<R: Real>(&mut self, step: u64, dt: f64, e: &[Vec<R>; 3], h: &[Vec<R>; 3], team: &Team) {
        if step % self.every != 0 {
            return;
        }
        let f = if self.dump.magnetic { h } else { e };
        let magnetic = self.dump.magnetic;
        let total = self.table.len();
        let mut out = vec![0f32; total * 3];
        let parts = (team.threads() * 4).min(total.max(1));
        let share = Shared(out.as_mut_ptr());
        let table = &self.table;
        team.run(parts, &|s| {
            for q in s * total / parts..(s + 1) * total / parts {
                let v = node_value(&table[q], magnetic, f);
                // SAFETY: each part writes its own range of nodes.
                unsafe {
                    *share.get().add(q * 3) = v[0] as f32;
                    *share.get().add(q * 3 + 1) = v[1] as f32;
                    *share.get().add(q * 3 + 2) = v[2] as f32;
                }
            }
        });
        self.add(step, dt, &out);
    }

    pub fn clear(&mut self) {
        for f in &mut self.data {
            f.iter_mut().for_each(|c| *c = [C::new(0.0, 0.0); 3]);
        }
    }

    pub fn planes(&self) -> Vec<NodePlane> {
        self.dump.freqs.iter().enumerate().map(|(fi, f)| NodePlane { name: self.dump.name.clone(), f: *f, lines: self.dump.lines.clone(), values: self.data[fi].clone() }).collect()
    }
}

struct Shared<T>(*mut T);
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Shared<T> {}
// SAFETY: writes through it are to disjoint cells.
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}
impl<T> Shared<T> {
    #[inline(always)]
    fn get(self) -> *mut T {
        self.0
    }
}
