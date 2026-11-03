//! From a problem to the two diagonal matrices and the sparse list of special edges.
//!
//! `me[a][n]` is `Mε` of the edge along axis `a` at node `n`: infinite for metal, which pins its
//! voltage at zero. `imu[a][n]` is `Mμ⁻¹` of the face normal to `a` with lower corner `n`.

use rayon::prelude::*;

use crate::edge::{self, Pair};
use crate::grid::Grid;
use crate::problem::*;
use crate::{EPS0, MU0};

/// A special edge: updated from its own list because something sits on it.
#[derive(Clone, Copy, Debug)]
pub struct RlEdge {
    pub axis: usize,
    pub node: usize,
    /// Series resistance and inductance along this edge.
    pub r: f64,
    pub l: f64,
}

/// A Debye branch on an edge: a series R and C across it.
#[derive(Clone, Copy, Debug)]
pub struct DebyeEdge {
    pub axis: usize,
    pub node: usize,
    pub r: f64,
    pub c: f64,
}

/// One lumped port's edges.
#[derive(Clone, Debug)]
pub struct PortEdges {
    pub axis: usize,
    /// `+1` if the gap's `a → b` runs along `+axis`.
    pub sign: f64,
    pub z0: f64,
    /// Every edge of the port, as `(node, resistance of this edge)`.
    pub edges: Vec<(usize, f64)>,
    /// Series edges of each column, for reading the voltage across the gap.
    pub columns: Vec<Vec<usize>>,
    /// The edges across the gap's middle, around which the current is read.
    pub mid: Vec<usize>,
}

pub struct Assembled {
    pub grid: Grid,
    pub walls: [Wall; 6],
    pub me: [Vec<f64>; 3],
    pub imu: [Vec<f64>; 3],
    /// Conductance across edges, from lossy material.
    pub g: [Option<Vec<f64>>; 3],
    pub rl: Vec<RlEdge>,
    pub debye: Vec<DebyeEdge>,
    pub ports: Vec<PortEdges>,
    /// Notes for the person running it.
    pub warnings: Vec<String>,
}

/// Cut cells that close: an edge this far covered counts as metal, and a face with less open than
/// this counts as shut. A sliver left open rings on its own at no useful frequency, and sets the
/// time step. Closing it moves the wall by at most this share of a cell.
const CLOSE_EDGE: f64 = 0.95;
const CLOSE_FACE: f64 = 0.05;

/// How much capacitance a real-metal edge carries, against the plain grid's for that edge.
const RL_EDGE_C: f64 = 16.0;

fn sub3(a: usize) -> (usize, usize) {
    ((a + 1) % 3, (a + 2) % 3)
}

/// Plain vacuum, from the grid alone.
fn vacuum(grid: &Grid) -> ([Vec<f64>; 3], [Vec<f64>; 3]) {
    let [nx, ny, nz] = grid.n();
    let mk = |a: usize, f: &(dyn Fn(usize, usize, usize) -> f64 + Sync)| -> Vec<f64> {
        let _ = a;
        let mut v = vec![0.0; nx * ny * nz];
        v.par_chunks_mut(nx * ny).enumerate().for_each(|(k, layer)| {
            for j in 0..ny {
                for i in 0..nx {
                    layer[i + nx * j] = f(i, j, k);
                }
            }
        });
        v
    };
    let me: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let (t1, t2) = sub3(a);
        mk(a, &|i, j, k| {
            let ix = [i, j, k];
            let l = grid.primal(a, ix[a]);
            if l <= 0.0 { f64::INFINITY } else { EPS0 * grid.dual(t1, ix[t1]) * grid.dual(t2, ix[t2]) / l }
        })
    });
    let imu: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let (t1, t2) = sub3(a);
        mk(a, &|i, j, k| {
            let ix = [i, j, k];
            let area = grid.primal(t1, ix[t1]) * grid.primal(t2, ix[t2]);
            if area <= 0.0 { 0.0 } else { grid.dual(a, ix[a]) / (MU0 * area) }
        })
    });
    (me, imu)
}

/// From a problem to what the engines step.
/// Fills everything behind a plane from what's on it. Geometry and materials are already in the
/// operator by now, so this copies them whatever made them, scaled for the cell lengths along the
/// axis: each coefficient depends on those in one known way, set by which way it points.
fn extrude(asm: &mut Assembled, x: Extrude) {
    let grid = asm.grid.clone();
    let a = x.axis;
    let n = grid.n();
    let strides = grid.strides();
    let up = x.direction > 0.0;
    let count = n[a];
    if x.plane >= count || (up && x.plane + 1 >= count) || (!up && x.plane == 0) {
        return;
    }
    // The first line and cell wholly inside the model. The plane itself won't do: a substrate ending
    // on it fills half of the dual cells there, and a copy of that is a different line. So the plane
    // is filled too, and the cross-section has to hold for a cell inside it.
    let cell_from = if up { x.plane } else { x.plane - 1 };
    let line_from = if up { x.plane + 1 } else { x.plane - 1 };
    let lines: Vec<usize> = if up { (0..=x.plane).collect() } else { (x.plane..count).collect() };
    let cells: Vec<usize> = if up { (0..x.plane).collect() } else { (x.plane..count - 1).collect() };
    let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
    let at = |layer: usize, i: usize, j: usize| layer * strides[a] + i * strides[t1] + j * strides[t2];
    // Everything on one transverse row of the plane, whichever axis it points along.
    let copy = |v: &mut Vec<f64>, to: usize, from: usize, scale: f64| {
        for j in 0..n[t2] {
            for i in 0..n[t1] {
                let (p, q) = (at(to, i, j), at(from, i, j));
                let c = v[q];
                v[p] = if c.is_finite() { c * scale } else { c };
            }
        }
    };
    let dual = |i: usize| grid.dual(a, i).max(1e-30);
    let primal = |i: usize| grid.primal(a, i).max(1e-30);
    for &l in &lines {
        for t in [t1, t2] {
            copy(&mut asm.me[t], l, line_from, dual(l) / dual(line_from));
            if let Some(g) = &mut asm.g[t] {
                copy(g, l, line_from, dual(l) / dual(line_from));
            }
        }
        copy(&mut asm.imu[a], l, line_from, dual(l) / dual(line_from));
    }
    for &c in &cells {
        for t in [t1, t2] {
            copy(&mut asm.imu[t], c, cell_from, primal(cell_from) / primal(c));
        }
        copy(&mut asm.me[a], c, cell_from, primal(cell_from) / primal(c));
        if let Some(g) = &mut asm.g[a] {
            copy(g, c, cell_from, primal(cell_from) / primal(c));
        }
    }
    let layer = |node: usize| node / strides[a] % count;
    let behind = |axis: usize, node: usize| -> bool {
        let l = layer(node);
        if axis == a {
            if up { l < x.plane } else { l >= x.plane && l + 1 < count }
        } else if up {
            l <= x.plane
        } else {
            l >= x.plane
        }
    };
    asm.rl.retain(|e| !behind(e.axis, e.node));
    asm.debye.retain(|e| !behind(e.axis, e.node));
    let (mut more_rl, mut more_debye) = (Vec::new(), Vec::new());
    let scale_of = |axis: usize, to: usize, from: usize| if axis == a { primal(from) / primal(to) } else { dual(to) / dual(from) };
    let moved = |node: usize, to: usize, from: usize| node + to * strides[a] - from * strides[a];
    for e in &asm.rl {
        let (src, targets) = if e.axis == a { (cell_from, &cells) } else { (line_from, &lines) };
        if layer(e.node) == src {
            for &to in targets {
                // Resistance and inductance go as one over what capacitance goes as.
                let k = 1.0 / scale_of(e.axis, to, src);
                more_rl.push(RlEdge { axis: e.axis, node: moved(e.node, to, src), r: e.r * k, l: e.l * k });
            }
        }
    }
    for e in &asm.debye {
        let (src, targets) = if e.axis == a { (cell_from, &cells) } else { (line_from, &lines) };
        if layer(e.node) == src {
            for &to in targets {
                let k = scale_of(e.axis, to, src);
                more_debye.push(DebyeEdge { axis: e.axis, node: moved(e.node, to, src), r: e.r / k, c: e.c * k });
            }
        }
    }
    asm.rl.extend(more_rl);
    asm.debye.extend(more_debye);
}

pub fn assemble(p: &Problem) -> Assembled {
    let grid = p.grid.clone();
    let (mut me, mut imu) = vacuum(&grid);
    let mut g: [Option<Vec<f64>>; 3] = [None, None, None];
    let mut rl = Vec::new();
    let mut debye = Vec::new();
    let mut warnings = Vec::new();
    bodies::apply(p, &grid, &mut me, &mut imu, &mut g, &mut rl, &mut debye);
    // A subcell correction changes the shape of a cell's field, and so its conductance and its
    // relaxation just as it changes its capacitance. They're scaled by the same factor below.
    let before = (g.iter().any(|x| x.is_some()) || !debye.is_empty()).then(|| me.clone());
    sheets::apply(p, &grid, &mut me, &mut imu, &mut rl, &mut warnings);
    wires::apply(p, &grid, &mut me, &mut imu, &mut rl, &mut warnings);
    if let Some(before) = &before {
        for a in 0..3 {
            if let Some(gv) = &mut g[a] {
                for (x, (now, was)) in gv.iter_mut().zip(me[a].iter().zip(&before[a])) {
                    if *x > 0.0 && now.is_finite() && was.is_finite() {
                        *x *= now / was;
                    }
                }
            }
        }
        for d in &mut debye {
            let (now, was) = (me[d.axis][d.node], before[d.axis][d.node]);
            if now.is_finite() && was.is_finite() {
                d.c *= now / was;
                d.r *= was / now;
            }
        }
    }
    walls(p, &grid, &mut me);
    let ports = ports_of(p, &grid, &mut me, &mut rl, &mut warnings);
    // Nothing lives on the outer walls: their edges are metal, and have no neighbours beyond.
    let on_wall = |axis: usize, node: usize| -> bool {
        let n = grid.n();
        let ix = [node % n[0], (node / n[0]) % n[1], node / (n[0] * n[1])];
        (0..3).any(|k| k != axis && (ix[k] == 0 || ix[k] + 1 == n[k]))
    };
    rl.retain(|e| !on_wall(e.axis, e.node));
    // An edge carries one thing, as the engines' update lists assume: a port over metal, metal over
    // a lossy material. A real-metal sheet on the face of a real-metal body is the one metal.
    let mut seen = std::collections::HashSet::new();
    rl.reverse();
    rl.retain(|e| seen.insert((e.axis, e.node)));
    rl.reverse();
    let mut taken: std::collections::HashSet<(usize, usize)> = rl.iter().map(|e| (e.axis, e.node)).collect();
    for port in &ports {
        taken.extend(port.edges.iter().map(|(n, _)| (port.axis, *n)));
    }
    debye.retain(|d| !on_wall(d.axis, d.node) && me[d.axis][d.node].is_finite() && !taken.contains(&(d.axis, d.node)));
    for &(a, node) in &taken {
        if let Some(gv) = &mut g[a] {
            gv[node] = 0.0;
        }
    }
    // Real metal keeps a voltage, so its edges are in the time stepping. A trapezoidal branch leaves
    // the grid's highest mode undamped, so such an edge needs enough capacitance of its own to meet
    // the time-step limit. The voltage is thousands of times below its neighbours, so this much
    // extra capacitance across a few milliohms changes nothing at radio frequency.
    for e in &rl {
        let ix = [e.node % grid.n()[0], (e.node / grid.n()[0]) % grid.n()[1], e.node / (grid.n()[0] * grid.n()[1])];
        let (t1, t2) = sub3(e.axis);
        me[e.axis][e.node] = RL_EDGE_C * EPS0 * grid.dual(t1, ix[t1]) * grid.dual(t2, ix[t2]) / grid.primal(e.axis, ix[e.axis]);
    }
    let mut asm = Assembled { grid, walls: p.walls, me, imu, g, rl, debye, ports, warnings };
    for x in &p.extrude {
        extrude(&mut asm, *x);
    }
    asm
}

/// Outer walls are metal: the absorber is backed by it too, and at a PEC wall that's the point.
fn walls(p: &Problem, grid: &Grid, me: &mut [Vec<f64>; 3]) {
    let _ = p;
    let n = grid.n();
    for a in 0..3 {
        let (t1, t2) = sub3(a);
        for (side, plane) in [(0, 0), (1, n[a] - 1)] {
            let _ = side;
            for i2 in 0..n[t2] {
                for i1 in 0..n[t1] {
                    let mut ix = [0; 3];
                    ix[a] = plane;
                    ix[t1] = i1;
                    ix[t2] = i2;
                    let node = grid.idx(ix[0], ix[1], ix[2]);
                    me[t1][node] = f64::INFINITY;
                    me[t2][node] = f64::INFINITY;
                }
            }
        }
    }
}

fn ports_of(p: &Problem, grid: &Grid, me: &mut [Vec<f64>; 3], rl: &mut Vec<RlEdge>, warnings: &mut Vec<String>) -> Vec<PortEdges> {
    let mut out = Vec::new();
    for (nr, port) in p.ports.iter().enumerate() {
        let a = port.axis;
        let (lo, hi) = (port.a[a].min(port.b[a]), port.a[a].max(port.b[a]));
        let (i0, i1) = (grid.nearest(a, lo), grid.nearest(a, hi));
        let i1 = i1.max(i0 + 1);
        // The columns across a wide port: every line inside its width, or the nearest to its centre.
        let centre: P3 = std::array::from_fn(|k| (port.a[k] + port.b[k]) * 0.5);
        let (t1, t2) = sub3(a);
        let mut lines: [Vec<usize>; 3] = Default::default();
        for t in [t1, t2] {
            lines[t] = vec![grid.nearest(t, centre[t])];
        }
        if let Some(w) = port.width_axis
            && port.width > 0.0
        {
            let (x, y) = (centre[w] - port.width / 2.0, centre[w] + port.width / 2.0);
            let (s, e) = grid.span(w, x - 1e-12, y + 1e-12);
            if e > s {
                lines[w] = (s..e).collect();
            }
        }
        if let Some((w, width)) = port.second {
            let (x, y) = (centre[w] - width / 2.0, centre[w] + width / 2.0);
            let (s, e) = grid.span(w, x - 1e-12, y + 1e-12);
            if e > s {
                lines[w] = (s..e).collect();
            }
        }
        let series = i1 - i0;
        let parallel = lines[t1].len() * lines[t2].len();
        let r_edge = port.z0 * parallel as f64 / series as f64;
        let mut edges = Vec::new();
        let mut columns = Vec::new();
        let mut mid = Vec::new();
        let mid_at = i0 + series / 2;
        for &c1 in &lines[t1] {
            for &c2 in &lines[t2] {
                let mut col = Vec::new();
                for at in i0..i1 {
                    let mut ix = [0; 3];
                    ix[a] = at;
                    ix[t1] = c1;
                    ix[t2] = c2;
                    let node = grid.idx(ix[0], ix[1], ix[2]);
                    // An edge lying on an outer wall has nothing beyond it to update from.
                    if [t1, t2].iter().any(|t| ix[*t] == 0 || ix[*t] + 1 == grid.n()[*t]) {
                        warnings.push(format!("Port {} lies on an outer wall of the model, so that part of it does nothing.", nr + 1));
                        continue;
                    }
                    // A port across metal would be shorted out: the port wins, and says so.
                    if me[a][node].is_infinite() {
                        warnings.push(format!("Port {} is drawn across metal: the metal under it is ignored.", nr + 1));
                        let (d1, d2) = (grid.dual(t1, ix[t1]), grid.dual(t2, ix[t2]));
                        me[a][node] = EPS0 * d1 * d2 / grid.primal(a, at);
                    }
                    edges.push((node, r_edge));
                    col.push(node);
                    if at == mid_at {
                        mid.push(node);
                    }
                }
                if !col.is_empty() {
                    columns.push(col);
                }
            }
        }
        // Nothing else may sit on a port edge.
        rl.retain(|e| !(e.axis == a && edges.iter().any(|(n, _)| *n == e.node)));
        let sign = if port.b[a] >= port.a[a] { 1.0 } else { -1.0 };
        out.push(PortEdges { axis: a, sign, z0: port.z0, edges, columns, mid });
    }
    out
}

mod bodies {
    use super::*;

    /// The materials along one column: `(length, material)` with air where nothing is.
    fn column(bodies: &[&Body], p: P3, axis: usize, lo: f64, hi: f64, out: &mut Vec<(f64, Option<Mat>)>, scratch: &mut Vec<(f64, f64)>) {
        out.clear();
        let mut cuts: Vec<f64> = vec![lo, hi];
        let mut chords: Vec<(usize, f64, f64)> = Vec::new();
        for (b, body) in bodies.iter().enumerate() {
            scratch.clear();
            body.solid.chord(p, axis, lo, hi, scratch);
            for &(x, y) in scratch.iter() {
                cuts.push(x);
                cuts.push(y);
                chords.push((b, x, y));
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        for w in cuts.windows(2) {
            let mid = (w[0] + w[1]) * 0.5;
            // Later bodies win.
            let mat = chords.iter().rev().find(|(_, x, y)| mid >= *x && mid <= *y).map(|(b, _, _)| bodies[*b].mat);
            out.push((w[1] - w[0], mat));
        }
    }

    /// The transverse sample points of a dual face or a face: breakpoints at every box plane
    /// within, so that a layer thinner than a cell isn't missed, and a few points in each piece
    /// if anything curved is around.
    fn samples(lo: f64, hi: f64, bodies: &[&Body], axis: usize, out: &mut Vec<(f64, f64)>) {
        out.clear();
        if hi <= lo {
            return;
        }
        let mut cuts = vec![lo, hi];
        let mut curved = false;
        for b in bodies {
            if !b.solid.is_box() {
                curved = true;
            }
            for x in b.solid.planes(axis) {
                if x > lo && x < hi {
                    cuts.push(x);
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        for w in cuts.windows(2) {
            let d = w[1] - w[0];
            if curved {
                // Midpoints of eight pieces: the fill fraction next to a curved wall is the
                // whole of the conformal correction, so its error is what limits the order.
                for s in 0..8 {
                    out.push((w[0] + d * (s as f64 + 0.5) / 8.0, d / 8.0));
                }
            } else {
                out.push(((w[0] + w[1]) * 0.5, d));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply(p: &Problem, grid: &Grid, me: &mut [Vec<f64>; 3], imu: &mut [Vec<f64>; 3], g: &mut [Option<Vec<f64>>; 3], rl: &mut Vec<RlEdge>, debye: &mut Vec<DebyeEdge>) {
        if p.bodies.is_empty() {
            return;
        }
        let [nx, ny, nz] = grid.n();
        let nodes = nx * ny * nz;
        // Only cells that a body's box reaches can change.
        let bounds: Vec<(P3, P3)> = p.bodies.iter().map(|b| b.solid.bounds()).collect();
        let index_range = |a: usize, lo: f64, hi: f64| -> (usize, usize) {
            let l = &grid.lines[a];
            let s = l.partition_point(|v| *v < lo).saturating_sub(2);
            let e = (l.partition_point(|v| *v <= hi) + 2).min(l.len());
            (s, e)
        };
        let mut touched = vec![false; nodes];
        for (lo, hi) in &bounds {
            let (i0, i1) = index_range(0, lo[0], hi[0]);
            let (j0, j1) = index_range(1, lo[1], hi[1]);
            let (k0, k1) = index_range(2, lo[2], hi[2]);
            for k in k0..k1 {
                for j in j0..j1 {
                    for i in i0..i1 {
                        touched[i + nx * (j + ny * k)] = true;
                    }
                }
            }
        }
        let any_lossy = p.bodies.iter().any(|b| matches!(b.mat, Mat::Dielectric { kappa, .. } if kappa > 0.0));
        if any_lossy {
            for a in 0..3 {
                g[a] = Some(vec![0.0; nodes]);
            }
        }
        // Each edge and face is worked out alone, so this splits by layer.
        type Out = (Vec<RlEdge>, Vec<DebyeEdge>);
        let results: Vec<Out> = (0..3)
            .map(|a| {
                let (t1, t2) = sub3(a);
                let me_a = &mut me[a];
                let g_a = g[a].as_mut().map(|v| v.as_mut_slice());
                let touched = &touched;
                let bodies = &p.bodies;
                let bounds = &bounds;
                let g_chunks: Vec<Option<&mut [f64]>> = match g_a {
                    Some(v) => v.chunks_mut(nx * ny).map(Some).collect(),
                    None => (0..nz).map(|_| None).collect(),
                };
                let parts: Vec<Out> = me_a
                    .par_chunks_mut(nx * ny)
                    .zip(g_chunks.into_par_iter())
                    .enumerate()
                    .map(|(k, (layer, mut g_layer))| {
                        let mut deb = Vec::new();
                        let mut skins = Vec::new();
                        let mut col = Vec::new();
                        let mut scratch = Vec::new();
                        let (mut s1, mut s2) = (Vec::new(), Vec::new());
                        let mut near: Vec<&Body> = Vec::new();
                        // A scene can hold thousands of bodies: only those at this layer and row are looked at.
                        let by_layer = |b: &(P3, P3), axis: usize, at: usize| -> bool {
                            let c = grid.lines[axis][at];
                            let (lo, hi) = grid.dual_parts(axis, at);
                            let reach = grid.primal(axis, at).max(hi);
                            b.0[axis] <= c + reach && b.1[axis] >= c - lo
                        };
                        let in_layer: Vec<usize> = (0..bounds.len()).filter(|b| by_layer(&bounds[*b], 2, k)).collect();
                        for j in 0..ny {
                            let in_row: Vec<usize> = in_layer.iter().copied().filter(|b| by_layer(&bounds[*b], 1, j)).collect();
                            if in_row.is_empty() {
                                continue;
                            }
                            for i in 0..nx {
                                let n2 = i + nx * j;
                                if !touched[n2 + nx * ny * k] {
                                    continue;
                                }
                                let ix = [i, j, k];
                                let len = grid.primal(a, ix[a]);
                                if len <= 0.0 {
                                    continue;
                                }
                                let (x0, x1) = (grid.lines[a][ix[a]], grid.lines[a][ix[a]] + len);
                                let (d1lo, d1hi) = grid.dual_parts(t1, ix[t1]);
                                let (d2lo, d2hi) = grid.dual_parts(t2, ix[t2]);
                                let (c1, c2) = (grid.lines[t1][ix[t1]], grid.lines[t2][ix[t2]]);
                                let (u0, u1, v0, v1) = (c1 - d1lo, c1 + d1hi, c2 - d2lo, c2 + d2hi);
                                near.clear();
                                for &b in &in_row {
                                    let (lo, hi) = &bounds[b];
                                    if lo[a] <= x1 && hi[a] >= x0 && lo[t1] <= u1 && hi[t1] >= u0 && lo[t2] <= v1 && hi[t2] >= v0 {
                                        near.push(&bodies[b]);
                                    }
                                }
                                if near.is_empty() {
                                    continue;
                                }
                                // The edge's own line: fully in metal means it's metal.
                                let mut here = [0.0; 3];
                                here[t1] = c1;
                                here[t2] = c2;
                                here[a] = x0;
                                column(&near, here, a, x0, x1, &mut col, &mut scratch);
                                let metal: f64 = col.iter().filter(|(_, m)| m.is_some_and(|m| m.is_metal())).map(|(l, _)| *l).sum();
                                if metal >= len * CLOSE_EDGE {
                                    layer[n2] = f64::INFINITY;
                                    // Real metal loses power on its skin: an edge with free space
                                    // on any side of it, its current spread over that boundary.
                                    let sigma = col.iter().rev().find_map(|(_, m)| match m {
                                        Some(Mat::Metal { sigma }) => Some(*sigma),
                                        _ => None,
                                    });
                                    if let Some(sigma) = sigma {
                                        let quad = |s1: f64, s2: f64, col: &mut Vec<(f64, Option<Mat>)>, scratch: &mut Vec<(f64, f64)>| -> bool {
                                            let (d1, d2) = (if s1 < 0.0 { d1lo } else { d1hi }, if s2 < 0.0 { d2lo } else { d2hi });
                                            let mut q = [0.0; 3];
                                            q[t1] = c1 + s1 * d1 * 0.5;
                                            q[t2] = c2 + s2 * d2 * 0.5;
                                            q[a] = x0;
                                            column(&near, q, a, x0, x1, col, scratch);
                                            let m: f64 = col.iter().filter(|(_, m)| m.is_some_and(|m| m.is_metal())).map(|(l, _)| *l).sum();
                                            m >= 0.5 * len
                                        };
                                        let q00 = quad(-1.0, -1.0, &mut col, &mut scratch);
                                        let q10 = quad(1.0, -1.0, &mut col, &mut scratch);
                                        let q01 = quad(-1.0, 1.0, &mut col, &mut scratch);
                                        let q11 = quad(1.0, 1.0, &mut col, &mut scratch);
                                        let mut width = 0.0;
                                        if q00 != q10 {
                                            width += d2lo;
                                        }
                                        if q01 != q11 {
                                            width += d2hi;
                                        }
                                        if q00 != q01 {
                                            width += d1lo;
                                        }
                                        if q10 != q11 {
                                            width += d1hi;
                                        }
                                        if width > 0.0 {
                                            let s = Surface::skin(sigma, p.design_f);
                                            skins.push(RlEdge { axis: a, node: n2 + nx * ny * k, r: s.r * len / width, l: s.l * len / width });
                                        }
                                    }
                                    continue;
                                }
                                let own = (len - metal).max(1e-3 * len);
                                // Columns across the dual face, in parallel; along each, in series.
                                samples(u0, u1, &near, t1, &mut s1);
                                samples(v0, v1, &near, t2, &mut s2);
                                let (mut sum_c, mut sum_g, mut area) = (0.0, 0.0, 0.0);
                                let (mut sum_deb_c, mut sum_tau_c) = (0.0, 0.0);
                                for &(u, du) in s1.iter() {
                                    for &(v, dv) in s2.iter() {
                                        let mut q = [0.0; 3];
                                        q[t1] = u;
                                        q[t2] = v;
                                        q[a] = x0;
                                        column(&near, q, a, x0, x1, &mut col, &mut scratch);
                                        let da = du * dv;
                                        area += da;
                                        let (mut ser, mut lk, mut free) = (0.0, 0.0, 0.0);
                                        let mut deb_w = 0.0;
                                        let mut tau = 0.0;
                                        for &(l, m) in col.iter() {
                                            let (er, k, d) = match m {
                                                None => (1.0, 0.0, None),
                                                Some(Mat::Dielectric { eps_r, kappa, debye, .. }) => (eps_r, kappa, debye),
                                                Some(_) => continue,
                                            };
                                            free += l;
                                            ser += l / er;
                                            lk += k * l / (er * er);
                                            if let Some(d) = d {
                                                deb_w += d.delta_eps * l / (er * er);
                                                tau = d.tau;
                                            }
                                        }
                                        if free > 0.0 && ser > 0.0 {
                                            // This column's permittivity, in series over what's free of metal. The
                                            // voltage is across the edge's own free length below: taking each column's
                                            // distance to a slanted wall instead lets the shortest ones dominate.
                                            let eps_col = free / ser;
                                            // A column cut short by a slanted wall only carries flux over part of the
                                            // edge. On a wall square to the edge every column has the same length, and
                                            // this is 1.
                                            let share = (free / own).min(1.0);
                                            sum_c += da * eps_col * share;
                                            // Loss goes with the square of the field in each part.
                                            sum_g += da * lk * eps_col * eps_col / free * share;
                                            sum_deb_c += da * deb_w * eps_col * eps_col / free * share;
                                            if deb_w > 0.0 {
                                                sum_tau_c = tau;
                                            }
                                        }
                                    }
                                }
                                if area <= 0.0 || sum_c <= 0.0 {
                                    continue;
                                }
                                layer[n2] = EPS0 * sum_c / own;
                                if let Some(gl) = g_layer.as_mut() {
                                    gl[n2] = sum_g / own;
                                }
                                if sum_deb_c > 0.0 {
                                    let c = EPS0 * sum_deb_c / own;
                                    deb.push(DebyeEdge { axis: a, node: n2 + nx * ny * k, r: sum_tau_c / c, c });
                                }
                            }
                        }
                        (skins, deb)
                    })
                    .collect();
                let mut out: Out = (Vec::new(), Vec::new());
                for (s, d) in parts {
                    out.0.extend(s);
                    out.1.extend(d);
                }
                out
            })
            .collect();
        for (s, d) in results {
            rl.extend(s);
            debye.extend(d);
        }
        // Faces: metal takes away area, and a magnetic material changes the reluctance.
        let any_mu = p.bodies.iter().any(|b| matches!(b.mat, Mat::Dielectric { mu_r, .. } if mu_r != 1.0));
        let any_metal = p.bodies.iter().any(|b| b.mat.is_metal());
        if !any_mu && !any_metal {
            return;
        }
        for a in 0..3 {
            let (t1, t2) = sub3(a);
            imu[a].par_chunks_mut(nx * ny).enumerate().for_each(|(k, layer)| {
                let mut col = Vec::new();
                let mut scratch = Vec::new();
                let mut near: Vec<&Body> = Vec::new();
                let by_layer = |b: &(P3, P3), axis: usize, at: usize| -> bool {
                    let c = grid.lines[axis][at];
                    let (lo, hi) = grid.dual_parts(axis, at);
                    let reach = grid.primal(axis, at).max(hi);
                    b.0[axis] <= c + reach && b.1[axis] >= c - lo
                };
                let in_layer: Vec<usize> = (0..bounds.len()).filter(|b| by_layer(&bounds[*b], 2, k)).collect();
                for j in 0..ny {
                    let in_row: Vec<usize> = in_layer.iter().copied().filter(|b| by_layer(&bounds[*b], 1, j)).collect();
                    if in_row.is_empty() {
                        continue;
                    }
                    for i in 0..nx {
                        let n2 = i + nx * j;
                        if !touched[n2 + nx * ny * k] {
                            continue;
                        }
                        let ix = [i, j, k];
                        let (l1, l2) = (grid.primal(t1, ix[t1]), grid.primal(t2, ix[t2]));
                        if l1 <= 0.0 || l2 <= 0.0 {
                            continue;
                        }
                        let (dlo, dhi) = grid.dual_parts(a, ix[a]);
                        let (c, c1, c2) = (grid.lines[a][ix[a]], grid.lines[t1][ix[t1]], grid.lines[t2][ix[t2]]);
                        near.clear();
                        for &b in &in_row {
                            let (lo, hi) = &bounds[b];
                            if lo[a] <= c + dhi && hi[a] >= c - dlo && lo[t1] <= c1 + l1 && hi[t1] >= c1 && lo[t2] <= c2 + l2 && hi[t2] >= c2 {
                                near.push(&p.bodies[b]);
                            }
                        }
                        if near.is_empty() {
                            continue;
                        }
                        // The free share of the face, from rows across it.
                        const ROWS: usize = 8;
                        let mut free = 0.0;
                        for r in 0..ROWS {
                            let v = c2 + l2 * (r as f64 + 0.5) / ROWS as f64;
                            let mut q = [0.0; 3];
                            q[a] = c;
                            q[t2] = v;
                            q[t1] = c1;
                            column(&near, q, t1, c1, c1 + l1, &mut col, &mut scratch);
                            let open: f64 = col.iter().filter(|(_, m)| !m.is_some_and(|m| m.is_metal())).map(|(l, _)| *l).sum();
                            free += open / l1 / ROWS as f64;
                        }
                        if free < CLOSE_FACE {
                            layer[n2] = 0.0;
                            continue;
                        }
                        // Along the dual edge H only lives in what is free of metal, and in series
                        // through any magnetic material. Averaged over the open part of the face: its
                        // middle may be under metal when the rest of it is not, and a face taken from
                        // there alone would stop responding at all.
                        let (mut rel_sum, mut rel_n) = (0.0, 0usize);
                        for sa in 0..4 {
                            for sb in 0..4 {
                                let mut q = [0.0; 3];
                                q[a] = c;
                                q[t1] = c1 + l1 * (sa as f64 + 0.5) / 4.0;
                                q[t2] = c2 + l2 * (sb as f64 + 0.5) / 4.0;
                                column(&near, q, a, c - dlo, c + dhi, &mut col, &mut scratch);
                                let mut rel = 0.0;
                                for &(l, m) in col.iter() {
                                    match m {
                                        None => rel += l,
                                        Some(Mat::Dielectric { mu_r, .. }) => rel += l / mu_r,
                                        Some(_) => {}
                                    }
                                }
                                if rel > 0.0 {
                                    rel_sum += rel;
                                    rel_n += 1;
                                }
                            }
                        }
                        // An open part narrower than the samples still has its dual edge: nothing it could
                        // cross is known to be in the way, and freezing the face would seal the gap.
                        let rel = if rel_n > 0 { rel_sum / rel_n as f64 } else { dlo + dhi };
                        // The relaxed part of conformal metal: a sliver of open face would set the
                        // whole run's time step, so its area counts as at least 3 %. Measured on a hollow sphere: this
                        // moves the frequency by 0.2 %, against 0.8 % for a floor of 30 %.
                        let area = free.max(0.03) * l1 * l2;
                        if rel > 0.0 {
                            layer[n2] = rel / (MU0 * area);
                        } else {
                            layer[n2] = 0.0;
                        }
                    }
                }
            });
        }
        if any_metal {
            seal_shut_faces(grid, &touched, me, imu);
        }
    }
}

/// A shut face with free edges around it would act as a magnetic wall: its H can't change, so
/// nothing holds those edges' voltages down. Shutting a face has to mean metal around it.
fn seal_shut_faces(grid: &Grid, touched: &[bool], me: &mut [Vec<f64>; 3], imu: &[Vec<f64>; 3]) {
    let [nx, ny, nz] = grid.n();
    let strides = grid.strides();
    for a in 0..3 {
        let (t1, t2) = sub3(a);
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let p = i + nx * (j + ny * k);
                    let ix = [i, j, k];
                    if !touched[p] || imu[a][p] != 0.0 || ix[t1] + 1 >= grid.n()[t1] || ix[t2] + 1 >= grid.n()[t2] {
                        continue;
                    }
                    me[t1][p] = f64::INFINITY;
                    me[t1][p + strides[t2]] = f64::INFINITY;
                    me[t2][p] = f64::INFINITY;
                    me[t2][p + strides[t1]] = f64::INFINITY;
                }
            }
        }
    }
}

mod sheets {
    use super::*;

    /// How many places along a cell decide whether metal runs the whole of an edge.
    const PROBES: usize = 4;

    /// All the metal in one plane.
    struct Plane<'a> {
        axis: usize,
        at: f64,
        polys: Vec<&'a [[f64; 2]]>,
        /// Polygon index → its surface.
        surface: Vec<Option<Surface>>,
    }

    impl Plane<'_> {
        /// Metal on the line of constant `v` (`along_u`) or `u`. A line exactly on an outline
        /// counts as on the metal: an edge sitting on a mesh line has metal nodes on it.
        fn line(&self, along_u: bool, fixed: f64, tol: f64) -> Vec<(f64, f64)> {
            let mut out = Vec::new();
            let f = if along_u { 1 } else { 0 };
            for pts in &self.polys {
                let on_vertex = pts.iter().any(|p| (p[f] - fixed).abs() <= tol);
                if on_vertex {
                    polygon_line(pts, along_u, fixed - tol, &mut out);
                    polygon_line(pts, along_u, fixed + tol, &mut out);
                } else {
                    polygon_line(pts, along_u, fixed, &mut out);
                }
            }
            merge_intervals(&mut out, tol);
            out
        }

        fn bounds(&self) -> ([f64; 2], [f64; 2]) {
            let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
            for pts in &self.polys {
                for p in pts.iter() {
                    for k in 0..2 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            (lo, hi)
        }

        fn contains(&self, u: f64, v: f64) -> bool {
            self.polys.iter().any(|pts| point_in_polygon(pts, [u, v]))
        }

        fn surface_at(&self, u: f64, v: f64) -> Option<Surface> {
            for (k, pts) in self.polys.iter().enumerate() {
                if point_in_polygon(pts, [u, v]) {
                    return self.surface[k];
                }
            }
            self.surface.iter().find_map(|s| *s)
        }
    }

    /// A strip that holds no line is centred on the nearest one, keeping its width: that's where
    /// its current will flow. Returns what moved, as `(line, original centre)`.
    fn snap(iv: &mut Vec<(f64, f64)>, lines: &[f64], moved: &mut Vec<(usize, f64)>) {
        for x in iv.iter_mut() {
            let s = lines.partition_point(|l| *l < x.0 - 1e-15);
            if s < lines.len() && lines[s] <= x.1 + 1e-15 {
                continue;
            }
            let c = (x.0 + x.1) * 0.5;
            let at = if s == 0 {
                0
            } else if s >= lines.len() {
                lines.len() - 1
            } else if c - lines[s - 1] <= lines[s] - c {
                s - 1
            } else {
                s
            };
            let half = (x.1 - x.0) * 0.5;
            *x = (lines[at] - half, lines[at] + half);
            moved.push((at, c));
        }
        merge_intervals(iv, 0.0);
    }

    /// The corrected-field factor for a segment on a line, from the boundary nearest it.
    fn line_factor(iv: &[(f64, f64)], mid: f64, cell: f64, pair: Pair) -> f64 {
        if iv.is_empty() {
            return 1.0;
        }
        // Nearest boundary to `mid`.
        let mut best = (f64::MAX, 0usize, false);
        for (n, &(p, q)) in iv.iter().enumerate() {
            let (dp, dq) = ((mid - p).abs(), (mid - q).abs());
            if dp < best.0 {
                best = (dp, n, false);
            }
            if dq < best.0 {
                best = (dq, n, true);
            }
        }
        // Well inside a wide piece of metal, or far outside, nothing needs correcting.
        if best.0 > 2.5 * cell {
            return 1.0;
        }
        edge::factor(edge::local_at(iv, best.1, best.2, cell), pair)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply(p: &Problem, grid: &Grid, me: &mut [Vec<f64>; 3], imu: &mut [Vec<f64>; 3], rl: &mut Vec<RlEdge>, warnings: &mut Vec<String>) {
        // Sheets on the same plane act together, or two overlapping ones would look like edges.
        let mut planes: Vec<Plane> = Vec::new();
        for s in &p.sheets {
            let plane = match planes.iter_mut().find(|q| q.axis == s.axis && (q.at - s.at).abs() < 1e-12) {
                Some(q) => q,
                None => {
                    planes.push(Plane { axis: s.axis, at: s.at, polys: Vec::new(), surface: Vec::new() });
                    planes.last_mut().unwrap()
                }
            };
            for poly in &s.polys {
                plane.polys.push(poly);
                plane.surface.push(s.surface);
            }
        }
        for plane in &planes {
            one_plane(p, grid, plane, me, imu, rl, warnings);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn one_plane(p: &Problem, grid: &Grid, plane: &Plane, me: &mut [Vec<f64>; 3], imu: &mut [Vec<f64>; 3], rl: &mut Vec<RlEdge>, warnings: &mut Vec<String>) {
        let a = plane.axis;
        let (u, v) = sub3(a);
        let k0 = grid.nearest(a, plane.at);
        if (grid.lines[a][k0] - plane.at).abs() > 0.05 * grid.dual(a, k0) {
            warnings.push(format!("A sheet at {:.3} mm isn't on a mesh line along axis {a}; it was moved to the nearest one.", plane.at * 1e3));
        }
        // Heights count from the line the metal is on: a sheet a hair off it is still in that plane,
        // and the correction treats the plane itself differently from a height above it.
        let plane_at = grid.lines[a][k0];
        let (lo, hi) = plane.bounds();
        let scale = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(grid.min_cell());
        let tol = 1e-9 * scale;
        let (lu, lv) = (&grid.lines[u], &grid.lines[v]);
        // Lines of the plane that could see this metal, with room for the correction's reach.
        let widen = |l: &[f64], a: f64, b: f64| -> (usize, usize) {
            let s = l.partition_point(|x| *x < a).saturating_sub(4);
            let e = (l.partition_point(|x| *x <= b) + 4).min(l.len());
            (s, e)
        };
        let (u0, u1) = widen(lu, lo[0], hi[0]);
        let (v0, v1) = widen(lv, lo[1], hi[1]);
        let cell_mid = |l: &[f64], i: usize| (l[i] + l[(i + 1).min(l.len() - 1)]) * 0.5;

        // First the cross-sections through each cell's probes, which find strips too narrow to
        // hold a line and say which edges their current runs on.
        let mut extra_rows: Vec<(usize, f64)> = Vec::new();
        let mut extra_cols: Vec<(usize, f64)> = Vec::new();
        for i in u0..u1.saturating_sub(1) {
            for s in 0..PROBES {
                let at = lu[i] + (lu[i + 1] - lu[i]) * (s as f64 + 0.5) / PROBES as f64;
                let mut iv = plane.line(false, at, tol);
                let mut moved = Vec::new();
                snap(&mut iv, lv, &mut moved);
                extra_rows.extend(moved);
            }
        }
        for j in v0..v1.saturating_sub(1) {
            for s in 0..PROBES {
                let at = lv[j] + (lv[j + 1] - lv[j]) * (s as f64 + 0.5) / PROBES as f64;
                let mut iv = plane.line(true, at, tol);
                let mut moved = Vec::new();
                snap(&mut iv, lu, &mut moved);
                extra_cols.extend(moved);
            }
        }
        // The metal on each line as the grid will hold it: true metal, plus strips moved onto it.
        let rows_build = |extra: &[(usize, f64)], j: usize, at: f64| -> Vec<(f64, f64)> {
            let mut iv = plane.line(true, at, tol);
            let mut uniq: Vec<f64> = extra.iter().filter(|e| e.0 == j).map(|e| e.1).collect();
            uniq.sort_by(f64::total_cmp);
            uniq.dedup_by(|a, b| (*a - *b).abs() < 1e-9 * scale);
            for c in uniq {
                iv.extend(plane.line(true, c, tol));
            }
            merge_intervals(&mut iv, tol);
            let mut moved = Vec::new();
            snap(&mut iv, lu, &mut moved);
            iv
        };
        let cols_build = |extra: &[(usize, f64)], i: usize, at: f64| -> Vec<(f64, f64)> {
            let mut iv = plane.line(false, at, tol);
            let mut uniq: Vec<f64> = extra.iter().filter(|e| e.0 == i).map(|e| e.1).collect();
            uniq.sort_by(f64::total_cmp);
            uniq.dedup_by(|a, b| (*a - *b).abs() < 1e-9 * scale);
            for c in uniq {
                iv.extend(plane.line(false, c, tol));
            }
            merge_intervals(&mut iv, tol);
            let mut moved = Vec::new();
            snap(&mut iv, lv, &mut moved);
            iv
        };
        let rows: Vec<Vec<(f64, f64)>> = (v0..v1).map(|j| rows_build(&extra_rows, j, lv[j])).collect();
        let cols: Vec<Vec<(f64, f64)>> = (u0..u1).map(|i| cols_build(&extra_cols, i, lu[i])).collect();
        // A face's cross-section runs through its middle, where a strip that hugs one line may
        // not be seen; the strip's own edges were corrected from its line.
        let rows_mid: Vec<Vec<(f64, f64)>> = (v0..v1).map(|j| { let mut iv = plane.line(true, cell_mid(lv, j), tol); let mut m = Vec::new(); snap(&mut iv, lu, &mut m); iv }).collect();
        let cols_mid: Vec<Vec<(f64, f64)>> = (u0..u1).map(|i| { let mut iv = plane.line(false, cell_mid(lu, i), tol); let mut m = Vec::new(); snap(&mut iv, lv, &mut m); iv }).collect();

        let node = |iu: usize, iv: usize, ia: usize| -> usize {
            let mut ix = [0; 3];
            ix[u] = iu;
            ix[v] = iv;
            ix[a] = ia;
            grid.idx(ix[0], ix[1], ix[2])
        };

        // Corrections on every edge and face close to the plane's metal.
        if p.subcell {
            let la = &grid.lines[a];
            let n_a = la.len();
            let span_a = |cell: f64| -> (usize, usize) {
                let s = la.partition_point(|x| *x < plane_at - 2.5 * cell - 1e-15);
                let e = la.partition_point(|x| *x <= plane_at + 2.5 * cell + 1e-15);
                (s, e)
            };
            for j in v0..v1 {
                for i in u0..u1 {
                    let row = &rows[j - v0];
                    let col = &cols[i - u0];
                    let row_mid = &rows_mid[j - v0];
                    let col_mid = &cols_mid[i - u0];
                    if row.is_empty() && col.is_empty() && row_mid.is_empty() && col_mid.is_empty() {
                        continue;
                    }
                    let cu = grid.primal(u, i).max(grid.dual(u, i));
                    let cv = grid.primal(v, j).max(grid.dual(v, j));
                    let (ka0, ka1) = span_a(cu.max(cv));
                    for ka in ka0..ka1.min(n_a) {
                        let eta = la[ka] - plane_at;
                        let (dn, up) = grid.dual_parts(a, ka);
                        let flip = eta < -1e-15;
                        let (down, upp) = if flip { (up, dn) } else { (dn, up) };
                        let ca = grid.primal(a, ka).max(dn + up);
                        // Segments along u and along v, at this height.
                        if i + 1 < lu.len() {
                            let (s0, s1) = (lu[i], lu[i + 1]);
                            let f_row = line_factor(row, (s0 + s1) * 0.5, (s1 - s0).max(ca), Pair::Along { s0, s1, eta, down, up: upp });
                            let n = node(i, j, ka);
                            if me[u][n].is_finite() {
                                me[u][n] *= f_row;
                            }
                            // The face above this segment takes the same factor, from its own line.
                            if j + 1 < lv.len() {
                                let f = line_factor(row_mid, (s0 + s1) * 0.5, (s1 - s0).max(ca), Pair::Along { s0, s1, eta, down, up: upp });
                                imu[a][n] *= f;
                            }
                        }
                        if j + 1 < lv.len() {
                            let (s0, s1) = (lv[j], lv[j + 1]);
                            let f_col = line_factor(col, (s0 + s1) * 0.5, (s1 - s0).max(ca), Pair::Along { s0, s1, eta, down, up: upp });
                            let n = node(i, j, ka);
                            if me[v][n].is_finite() {
                                me[v][n] *= f_col;
                            }
                            if i + 1 < lu.len() {
                                let f = line_factor(col_mid, (s0 + s1) * 0.5, (s1 - s0).max(ca), Pair::Along { s0, s1, eta, down, up: upp });
                                imu[a][n] *= f;
                            }
                        }
                        // Segments standing on the plane, which never reach across it.
                        if ka + 1 < n_a {
                            let (e0, e1) = (la[ka] - plane_at, la[ka + 1] - plane_at);
                            if e0.abs().min(e1.abs()) <= 2.5 * ca && e0 * e1 >= -1e-30 {
                                let (l, r) = grid.dual_parts(u, i);
                                let f_row = line_factor(row, lu[i], cu.max(ca), Pair::Across { s: lu[i], eta0: e0, eta1: e1, left: l, right: r });
                                let (l2, r2) = grid.dual_parts(v, j);
                                let f_col = line_factor(col, lv[j], cv.max(ca), Pair::Across { s: lv[j], eta0: e0, eta1: e1, left: l2, right: r2 });
                                let n = node(i, j, ka);
                                if me[a][n].is_finite() {
                                    me[a][n] *= f_row * f_col;
                                }
                                // Faces standing on the plane, on the segment through their middle.
                                // Normal to u: it spans v, so its cross-section (u, a) is through v's middle.
                                if j + 1 < lv.len() {
                                    let f = line_factor(row_mid, lu[i], cu.max(ca), Pair::Across { s: lu[i], eta0: e0, eta1: e1, left: l, right: r });
                                    imu[u][n] *= f;
                                }
                                if i + 1 < lu.len() {
                                    let f = line_factor(col_mid, lv[j], cv.max(ca), Pair::Across { s: lv[j], eta0: e0, eta1: e1, left: l2, right: r2 });
                                    imu[v][n] *= f;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Then the metal itself, and what a real metal adds to it.
        // An edge is metal only if metal covers all of it. Rounding up would push a tip out to the next
        // line while the correction above still assumes where it really is.
        let spans = |iv: &[(f64, f64)], a: f64, b: f64| iv.iter().any(|x| x.0 <= a + tol && x.1 >= b - tol);
        for j in v0..v1 {
            for i in u0..u1 {
                if i + 1 < lu.len() && spans(&rows[j - v0], lu[i], lu[i + 1]) {
                    let n = node(i, j, k0);
                    me[u][n] = f64::INFINITY;
                    let umid = cell_mid(lu, i);
                    if let Some(s) = plane.surface_at(umid, lv[j]) {
                        // Current spreads over the metal across this edge's dual width.
                        let band = cross_width(&cols_mid[i - u0], lv[j], grid.dual_parts(v, j));
                        let l = grid.primal(u, i);
                        rl.push(RlEdge { axis: u, node: n, r: s.r * l / band, l: s.l * l / band });
                    }
                }
                if j + 1 < lv.len() && spans(&cols[i - u0], lv[j], lv[j + 1]) {
                    let n = node(i, j, k0);
                    me[v][n] = f64::INFINITY;
                    let vmid = cell_mid(lv, j);
                    if let Some(s) = plane.surface_at(lu[i], vmid) {
                        let band = cross_width(&rows_mid[j - v0], lu[i], grid.dual_parts(u, i));
                        let l = grid.primal(v, j);
                        rl.push(RlEdge { axis: v, node: n, r: s.r * l / band, l: s.l * l / band });
                    }
                }
            }
        }
        // A slanting strip narrower than a cell may cover no edge from end to end, which would leave its
        // current nowhere to run. So its centre line goes down in steps along the mesh edges nearest it.
        let mut stepped = 0usize;
        {
            let mut put = |along_u: bool, iu: usize, iv: usize, at: (f64, f64), band: f64| {
                let n = node(iu, iv, k0);
                let (axis, l) = if along_u { (u, grid.primal(u, iu)) } else { (v, grid.primal(v, iv)) };
                if me[axis][n].is_finite() {
                    stepped += 1;
                }
                me[axis][n] = f64::INFINITY;
                if let Some(s) = plane.surface_at(at.0, at.1) {
                    let band = band.max(1e-3 * l);
                    rl.push(RlEdge { axis, node: n, r: s.r * l / band, l: s.l * l / band });
                }
            };
            // Cuts at each line of `a`, across `b`: a strip running mostly along `a` shows up narrow in them.
            for by_u in [true, false] {
                let (la, lb, axis_a, axis_b) = if by_u { (lu, lv, u, v) } else { (lv, lu, v, u) };
                let (r0, r1) = if by_u { (u0, u1) } else { (v0, v1) };
                // Centre and width of each narrow piece at the last probe.
                let mut last: Vec<(f64, f64, f64)> = Vec::new();
                for i in r0..r1.saturating_sub(1) {
                    for s in 0..PROBES {
                        let at = la[i] + (la[i + 1] - la[i]) * (s as f64 + 0.5) / PROBES as f64;
                        let mut now = Vec::new();
                        for x in plane.line(!by_u, at, tol) {
                            let c = (x.0 + x.1) * 0.5;
                            let near = lb.partition_point(|y| *y < c).min(lb.len() - 1);
                            let cell = (lb[near] - lb[near.saturating_sub(1)]).max(1e-30);
                            let w = x.1 - x.0;
                            // Only from two cells across is a full edge sure to lie inside the strip at every line,
                            // close enough to the next line's for the two to join up.
                            if w >= 2.0 * cell {
                                continue;
                            }
                            now.push((at, c, w));
                            for &(pa, pc, pw) in &last {
                                let (da, dc) = ((at - pa).abs(), (c - pc).abs());
                                let (ma, mb) = ((pa + at) * 0.5, (pc + c) * 0.5);
                                let mid = if by_u { (ma, mb) } else { (mb, ma) };
                                // The same strip one probe on: metal between the two, which on a coarse mesh may not
                                // overlap, and about one width, so not a corner's tip. Slanting no steeper than this
                                // cut can follow.
                                let same = w.min(pw) > 0.8 * w.max(pw) && plane.contains(mid.0, mid.1);
                                let can_follow = if by_u { dc <= da * (1.0 + 1e-6) } else { dc < da * (1.0 - 1e-6) };
                                if !same || !can_follow || dc <= 1e-6 * cell {
                                    continue;
                                }
                                let (fa, fb) = (grid.nearest(axis_a, pa), grid.nearest(axis_b, pc));
                                let (ta, tb) = (grid.nearest(axis_a, at), grid.nearest(axis_b, c));
                                if (fa, fb) == (ta, tb) {
                                    continue;
                                }
                                let band = w.min(pw);
                                let mut run = |along_a: bool, fixed: usize, q0: usize, q1: usize| {
                                    for q in q0.min(q1)..q0.max(q1) {
                                        if along_a == by_u {
                                            put(true, q, fixed, mid, band);
                                        } else {
                                            put(false, fixed, q, mid, band);
                                        }
                                    }
                                };
                                // A diagonal step goes round the corner nearer the strip.
                                let dist = |qa: f64, qb: f64| (qa - ma).powi(2) + (qb - mb).powi(2);
                                if dist(la[ta], lb[fb]) <= dist(la[fa], lb[tb]) {
                                    run(true, fb, fa, ta);
                                    run(false, ta, fb, tb);
                                } else {
                                    run(false, fa, fb, tb);
                                    run(true, tb, fa, ta);
                                }
                            }
                        }
                        last = now;
                    }
                }
            }
        }
        if stepped > 0 {
            warnings.push(format!("A slanting trace too narrow for this mesh runs in steps along {stepped} mesh edges. Its current takes that longer path, so its resonance can read far low: over 30 % on a 45 degree trace 10 times narrower than a cell. Refine the mesh around it until it's about two cells wide."));
        }
    }

    /// How wide the metal is across an edge's dual band, at least a thousandth of the band.
    fn cross_width(iv: &[(f64, f64)], at: f64, parts: (f64, f64)) -> f64 {
        let (lo, hi) = (at - parts.0, at + parts.1);
        let w: f64 = iv.iter().map(|x| (x.1.min(hi) - x.0.max(lo)).max(0.0)).sum();
        w.max((hi - lo) * 1e-3)
    }
}

mod wires {
    use super::*;

    /// Steps along grid lines from one node to another, one axis at a time.
    fn walk(grid: &Grid, from: [usize; 3], to: [usize; 3], mut each: impl FnMut(usize, [usize; 3])) {
        let mut at = from;
        for a in 0..3 {
            while at[a] != to[a] {
                let step_up = to[a] > at[a];
                let lower = if step_up { at[a] } else { at[a] - 1 };
                let mut ix = at;
                ix[a] = lower;
                each(a, ix);
                at[a] = if step_up { at[a] + 1 } else { at[a] - 1 };
            }
        }
        let _ = grid;
    }

    /// The flux and voltage of a thin wire's field: `w = ln(ζ/r)`, centred on the wire.
    fn f_wire(r: f64, p0: [f64; 2], p1: [f64; 2], d0: [f64; 2], d1: [f64; 2]) -> f64 {
        let w = |q: [f64; 2]| -> num_complex::Complex64 {
            let rho = (q[0] * q[0] + q[1] * q[1]).sqrt().max(r);
            num_complex::Complex64::new((rho / r).ln(), q[1].atan2(q[0]))
        };
        let along = (w(p1).re - w(p0).re).abs();
        // Angles wrap, so take the short way between the ends of the dual segment.
        let mut dth = (w(d1).im - w(d0).im).abs();
        if dth > std::f64::consts::PI {
            dth = 2.0 * std::f64::consts::PI - dth;
        }
        let p_len = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt();
        let d_len = ((d1[0] - d0[0]).powi(2) + (d1[1] - d0[1]).powi(2)).sqrt();
        let sideways = {
            let mut x = (w(p1).im - w(p0).im).abs();
            if x > std::f64::consts::PI {
                x = 2.0 * std::f64::consts::PI - x;
            }
            x
        };
        if along <= 0.0 || dth <= 0.0 || along < 0.02 * sideways || p_len <= 0.0 || d_len <= 0.0 {
            return 1.0;
        }
        ((dth / d_len) / (along / p_len)).clamp(edge::F_MIN, edge::F_MAX)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply(p: &Problem, grid: &Grid, me: &mut [Vec<f64>; 3], imu: &mut [Vec<f64>; 3], rl: &mut Vec<RlEdge>, warnings: &mut Vec<String>) {
        for wire in &p.wires {
            let mut stepped = false;
            for seg in wire.pts.windows(2) {
                let from: [usize; 3] = std::array::from_fn(|k| grid.nearest(k, seg[0][k]));
                let to: [usize; 3] = std::array::from_fn(|k| grid.nearest(k, seg[1][k]));
                if from.iter().zip(&to).filter(|(x, y)| x != y).count() > 1 {
                    stepped = true;
                }
                let mut cells: Vec<(usize, [usize; 3])> = Vec::new();
                walk(grid, from, to, |a, ix| cells.push((a, ix)));
                for (a, ix) in cells {
                    let node = grid.idx(ix[0], ix[1], ix[2]);
                    me[a][node] = f64::INFINITY;
                    let l = grid.primal(a, ix[a]);
                    let perim = 2.0 * std::f64::consts::PI * wire.r;
                    if let Some(s) = wire.surface {
                        rl.push(RlEdge { axis: a, node, r: s.r * l / perim, l: s.l * l / perim });
                    }
                    // No radius given: a plain line of metal edges, as openEMS draws a curve.
                    if p.subcell && wire.r > 0.0 {
                        correct_around(grid, a, ix, wire.r, me, imu);
                    }
                }
            }
            if stepped {
                warnings.push("A wire that doesn't follow the mesh lines is stepped along them, which makes it longer than it is: its resonance reads low.".into());
            }
        }
    }

    /// Thin-wire correction on the rings of cells around one edge of wire.
    fn correct_around(grid: &Grid, a: usize, ix: [usize; 3], r: f64, me: &mut [Vec<f64>; 3], imu: &mut [Vec<f64>; 3]) {
        let (p, q) = sub3(a);
        let (lp, lq) = (&grid.lines[p], &grid.lines[q]);
        let (ip, iq) = (ix[p], ix[q]);
        let (c0, c1) = (lp[ip], lq[iq]);
        const RING: isize = 2;
        let at = |i: isize, l: &[f64]| -> Option<usize> { (i >= 0 && (i as usize) < l.len()).then_some(i as usize) };
        for dq in -RING..=RING {
            for dp in -RING..=RING {
                let (Some(np), Some(nq)) = (at(ip as isize + dp, lp), at(iq as isize + dq, lq)) else { continue };
                let x = lp[np] - c0;
                let y = lq[nq] - c1;
                let mut idx = ix;
                idx[p] = np;
                idx[q] = nq;
                let node = grid.idx(idx[0], idx[1], idx[2]);
                // Segment along p from this node, crossed by the dual segment along q.
                if np + 1 < lp.len() {
                    let x1 = lp[np + 1] - c0;
                    let (dl, dh) = grid.dual_parts(q, nq);
                    let xm = (x + x1) * 0.5;
                    let f = f_wire(r, [x, y], [x1, y], [xm, y - dl], [xm, y + dh]);
                    if me[p][node].is_finite() {
                        me[p][node] *= f;
                    }
                    // Its face stands over the same segment and runs along the wire.
                    imu[q][node] *= f;
                }
                if nq + 1 < lq.len() {
                    let y1 = lq[nq + 1] - c1;
                    let (dl, dh) = grid.dual_parts(p, np);
                    let ym = (y + y1) * 0.5;
                    let f = f_wire(r, [x, y], [x, y1], [x - dl, ym], [x + dh, ym]);
                    if me[q][node].is_finite() {
                        me[q][node] *= f;
                    }
                    imu[p][node] *= f;
                }
            }
        }
    }
}
