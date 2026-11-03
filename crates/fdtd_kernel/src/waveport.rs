//! Wave ports for quasi-TEM lines: microstrip, stripline, coax.
//!
//! The line's cross-section is solved statically on the plane the port sits in, using the same
//! `Mε` and `Mμ` as the 3D run, so the mode is what this grid really supports and its impedance
//! comes with the subcell metal included. From the two solves come the capacitance and inductance
//! per metre, so `Z0 = √(L′/C′)` and the phase velocity `1/√(L′C′)`.
//!
//! The wave goes in one way only, by a total-field/scattered-field boundary on that plane, and
//! comes back out as voltage and current projected on the mode, so anything not in the mode is
//! rejected. Moving the result to a reference plane away from the port is then a phase shift.
//!
//! This holds for a line whose fields are close to TEM. A waveguide or a line well above its first
//! higher mode isn't one, and needs a full modal solve this doesn't have.

use crate::assemble::Assembled;
use crate::grid::Grid;

/// Where a wave port sits: a rectangle in the plane normal to `axis`, at line `plane`, whose rim
/// counts as the line's outer conductor.
#[derive(Clone, Debug)]
pub struct WavePortDef {
    pub axis: usize,
    /// The line of `axis` the injection plane is on.
    pub plane: usize,
    /// Line ranges along `axis + 1` and `axis + 2`, inclusive.
    pub r1: (usize, usize),
    pub r2: (usize, usize),
    /// `+1.0` if the wave going into the model runs towards `+axis`.
    pub direction: f64,
    /// A point on the signal conductor, as `(axis + 1, axis + 2)` line indices.
    pub signal: (usize, usize),
    /// A point on the return conductor, if it needn't reach the rim, as a coax's shield doesn't.
    pub ground: Option<(usize, usize)>,
}

/// The line's static solution.
#[derive(Clone, Debug)]
pub struct Mode {
    pub z0: f64,
    pub eps_eff: f64,
    /// Capacitance and inductance per metre.
    pub c: f64,
    pub l: f64,
    /// `(comp, offset within the plane)`: edge voltages for 1 V on the signal conductor.
    pub e: Vec<(usize, usize, f64)>,
    /// `(comp, node offset, value)`: dual-edge voltages for 1 A on the signal conductor.
    pub h: Vec<(usize, usize, f64)>,
}

/// Conjugate gradients for a graph Laplacian with some nodes held. `x` holds the fixed values.
fn solve_laplace(n: usize, edges: &[(u32, u32, f64)], fixed: &[bool], x: &mut [f64]) {
    let apply = |v: &[f64], out: &mut [f64]| {
        out.iter_mut().for_each(|o| *o = 0.0);
        for &(a, b, w) in edges {
            let d = w * (v[a as usize] - v[b as usize]);
            out[a as usize] += d;
            out[b as usize] -= d;
        }
    };
    let mut r = vec![0.0; n];
    apply(x, &mut r);
    for i in 0..n {
        r[i] = if fixed[i] { 0.0 } else { -r[i] };
    }
    // Diagonal scaling: weights vary a great deal near the metal and across graded cells.
    let mut diag = vec![0.0f64; n];
    for &(a, b, w) in edges {
        diag[a as usize] += w;
        diag[b as usize] += w;
    }
    diag.iter_mut().for_each(|d| *d = if *d > 0.0 { 1.0 / *d } else { 1.0 });
    let mut z: Vec<f64> = r.iter().zip(&diag).map(|(a, b)| a * b).collect();
    let mut p = z.clone();
    let mut ap = vec![0.0; n];
    let mut rz: f64 = r.iter().zip(&z).map(|(a, b)| a * b).sum();
    let start = rz.abs().max(1e-300);
    for _ in 0..(n * 4).max(2000) {
        if rz.abs() < 1e-24 * start {
            break;
        }
        apply(&p, &mut ap);
        for i in 0..n {
            if fixed[i] {
                ap[i] = 0.0;
            }
        }
        let denom: f64 = p.iter().zip(&ap).map(|(a, b)| a * b).sum();
        if denom.abs() < 1e-300 {
            break;
        }
        let alpha = rz / denom;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        for i in 0..n {
            z[i] = r[i] * diag[i];
        }
        let next: f64 = r.iter().zip(&z).map(|(a, b)| a * b).sum();
        let beta = next / rz;
        rz = next;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
}

/// Solves the cross-section. Fails if no conductor is found at `signal`, or it touches the rim.
pub fn solve_mode(asm: &Assembled, def: &WavePortDef) -> Result<Mode, String> {
    let grid: &Grid = &asm.grid;
    let a = def.axis;
    let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
    let (n1, n2) = (def.r1.1 - def.r1.0 + 1, def.r2.1 - def.r2.0 + 1);
    let id = |i: usize, j: usize| i + n1 * j;
    let node3 = |i: usize, j: usize, plane: usize| -> usize {
        let mut ix = [0; 3];
        ix[a] = plane;
        ix[t1] = def.r1.0 + i;
        ix[t2] = def.r2.0 + j;
        grid.idx(ix[0], ix[1], ix[2])
    };
    // The conductors, from the metal edges: nodes joined by metal are at one potential.
    let mut parent: Vec<u32> = (0..(n1 * n2) as u32).collect();
    fn find(p: &mut [u32], x: u32) -> u32 {
        let mut r = x;
        while p[r as usize] != r {
            r = p[r as usize];
        }
        let mut c = x;
        while p[c as usize] != r {
            let n = p[c as usize];
            p[c as usize] = r;
            c = n;
        }
        r
    }
    let is_metal = |m: f64| !m.is_finite();
    for j in 0..n2 {
        for i in 0..n1 {
            let n = node3(i, j, def.plane);
            if i + 1 < n1 && is_metal(asm.me[t1][n]) {
                let (x, y) = (find(&mut parent, id(i, j) as u32), find(&mut parent, id(i + 1, j) as u32));
                parent[x as usize] = y;
            }
            if j + 1 < n2 && is_metal(asm.me[t2][n]) {
                let (x, y) = (find(&mut parent, id(i, j) as u32), find(&mut parent, id(i, j + 1) as u32));
                parent[x as usize] = y;
            }
        }
    }
    // Real metal keeps a finite coefficient in `me`; its edges are on the RL list.
    for e in &asm.rl {
        if e.axis != t1 && e.axis != t2 {
            continue;
        }
        for j in 0..n2 {
            for i in 0..n1 {
                if node3(i, j, def.plane) == e.node {
                    let other = if e.axis == t1 { (i + 1, j) } else { (i, j + 1) };
                    if other.0 < n1 && other.1 < n2 {
                        let (x, y) = (find(&mut parent, id(i, j) as u32), find(&mut parent, id(other.0, other.1) as u32));
                        parent[x as usize] = y;
                    }
                }
            }
        }
    }
    let (si, sj) = (def.signal.0.wrapping_sub(def.r1.0), def.signal.1.wrapping_sub(def.r2.0));
    if si >= n1 || sj >= n2 {
        return Err("the signal conductor is outside the port".into());
    }
    let signal = find(&mut parent, id(si, sj) as u32);
    let rim = |i: usize, j: usize| i == 0 || j == 0 || i + 1 == n1 || j + 1 == n2;
    let mut held_rim = vec![false; n1 * n2];
    for j in 0..n2 {
        for i in 0..n1 {
            if rim(i, j) {
                let r = find(&mut parent, id(i, j) as u32);
                if r == signal {
                    return Err("the signal conductor reaches the port's rim".into());
                }
                held_rim[id(i, j)] = true;
            }
        }
    }
    // Every other conductor joined to the rim counts as the rim. Floating ones stay free.
    let mut outer: Vec<u32> = Vec::new();
    for j in 0..n2 {
        for i in 0..n1 {
            if rim(i, j) {
                outer.push(find(&mut parent, id(i, j) as u32));
            }
        }
    }
    if let Some(g) = def.ground {
        let (gi, gj) = (g.0.wrapping_sub(def.r1.0), g.1.wrapping_sub(def.r2.0));
        if gi >= n1 || gj >= n2 {
            return Err("the return conductor is outside the port".into());
        }
        let r = find(&mut parent, id(gi, gj) as u32);
        if r == signal {
            return Err("the signal and return conductors are joined at the port".into());
        }
        outer.push(r);
    }
    outer.sort_unstable();
    outer.dedup();
    let mut fixed = vec![false; n1 * n2];
    let mut phi = vec![0.0; n1 * n2];
    for j in 0..n2 {
        for i in 0..n1 {
            let r = find(&mut parent, id(i, j) as u32);
            if r == signal {
                fixed[id(i, j)] = true;
                phi[id(i, j)] = 1.0;
            } else if outer.binary_search(&r).is_ok() {
                fixed[id(i, j)] = true;
            }
        }
    }
    // In-plane edges. Mε includes the dual length along the line, which per metre comes out.
    let along = grid.dual(a, def.plane).max(1e-30);
    let mut edges_c: Vec<(u32, u32, f64)> = Vec::new();
    let mut edges_c0: Vec<(u32, u32, f64)> = Vec::new();
    let mut edges_l: Vec<(u32, u32, f64)> = Vec::new();
    let lp = grid.primal(a, def.plane).max(1e-30);
    let mut mode_e_edges = Vec::new();
    for j in 0..n2 {
        for i in 0..n1 {
            let n = node3(i, j, def.plane);
            for (k, (t, other)) in [(t1, (i + 1, j)), (t2, (i, j + 1))].into_iter().enumerate() {
                if other.0 >= n1 || other.1 >= n2 {
                    continue;
                }
                let u = id(i, j) as u32;
                let v = id(other.0, other.1) as u32;
                let m = asm.me[t][n];
                if m.is_finite() && m > 0.0 {
                    edges_c.push((u, v, m / along));
                    // Vacuum: the plain grid's capacitance, without the material's permittivity.
                    let plain = plain_me(grid, t, n);
                    edges_c0.push((u, v, (m / along).min(plain / along)));
                    mode_e_edges.push((t, n, u, v, m));
                }
                // The face over this segment has its normal along the other in-plane axis, and
                // its flux per metre is the difference of A between the segment's two ends.
                let nf = if k == 0 { t2 } else { t1 };
                let imu = asm.imu[nf][n];
                if imu > 0.0 {
                    edges_l.push((u, v, imu * lp));
                }
            }
        }
    }
    let total = n1 * n2;
    // Nodes joined by metal are one node, or a floating conductor wouldn't be equipotential.
    let roots: Vec<u32> = (0..total as u32).map(|x| find(&mut parent, x)).collect();
    let merge = |edges: &mut Vec<(u32, u32, f64)>| {
        for e in edges.iter_mut() {
            e.0 = roots[e.0 as usize];
            e.1 = roots[e.1 as usize];
        }
        edges.retain(|e| e.0 != e.1);
    };
    merge(&mut edges_c);
    merge(&mut edges_c0);
    merge(&mut edges_l);
    let mut phi0 = phi.clone();
    let mut a_sol = phi.clone();
    solve_laplace(total, &edges_c, &fixed, &mut phi);
    solve_laplace(total, &edges_c0, &fixed, &mut phi0);
    solve_laplace(total, &edges_l, &fixed, &mut a_sol);
    let energy = |edges: &[(u32, u32, f64)], x: &[f64]| -> f64 { edges.iter().map(|&(u, v, w)| { let d = x[u as usize] - x[v as usize]; w * d * d }).sum() };
    let c = energy(&edges_c, &phi);
    let c0 = energy(&edges_c0, &phi0);
    let inv_l = energy(&edges_l, &a_sol);
    if !(c > 0.0 && inv_l > 0.0) {
        return Err("the line has no capacitance or inductance: is the signal conductor shorted?".into());
    }
    // A = 1 on the conductor, so the flux per metre is 1 and I = 1/L′.
    let l = 1.0 / inv_l;
    let z0 = (l / c).sqrt();
    let eps_eff = (crate::C0 * crate::C0 * l * c).max(1.0);
    let at = |x: &[f64], i: u32| x[roots[i as usize] as usize];
    let _ = c0;
    // The mode's fields for 1 V and 1 A.
    let mut e = Vec::new();
    for &(t, n, u, v, _) in &mode_e_edges {
        // The edge's voltage runs from its low node to its high one: E·dl = φ(low) - φ(high).
        e.push((t, n, at(&phi, u) - at(&phi, v)));
    }
    let mut h = Vec::new();
    let current_scale = l; // A was solved for flux 1 per metre, giving 1/L′ amps; this makes it 1 A.
    for j in 0..n2 {
        for i in 0..n1 {
            let n = node3(i, j, def.plane);
            for (k, (_, other)) in [(t1, (i + 1, j)), (t2, (i, j + 1))].into_iter().enumerate() {
                if other.0 >= n1 || other.1 >= n2 {
                    continue;
                }
                let nf = if k == 0 { t2 } else { t1 };
                let imu = asm.imu[nf][n];
                if imu <= 0.0 {
                    continue;
                }
                let d = at(&a_sol, id(i, j) as u32) - at(&a_sol, id(other.0, other.1) as u32);
                // Flux through the face is d per metre over this cell's length; h̃ = Mμ⁻¹·b. For a
                // wave along +axis H = â×E/Z, and â×t1 = t2, â×t2 = -t1: the second keeps its sign.
                let sign = if k == 0 { 1.0 } else { -1.0 };
                h.push((nf, n, sign * d * lp * imu * current_scale));
            }
        }
    }
    Ok(Mode { z0, eps_eff, c, l, e, h })
}

fn plain_me(grid: &Grid, t: usize, n: usize) -> f64 {
    let [nx, ny, _] = grid.n();
    let ix = [n % nx, (n / nx) % ny, n / (nx * ny)];
    let (a, b) = ((t + 1) % 3, (t + 2) % 3);
    let l = grid.primal(t, ix[t]);
    if l <= 0.0 { f64::INFINITY } else { crate::EPS0 * grid.dual(a, ix[a]) * grid.dual(b, ix[b]) / l }
}

/// The port in the time stepping: `apply_*` add the incident wave across its plane, and `read`
/// takes the voltage and current of the mode one cell inside.
pub struct WavePortRun {
    pub def: WavePortDef,
    pub mode: Mode,
    /// Where it reads, in cells inward of the injection plane.
    read_offset: usize,
    /// Plane spacing used for the half-cell delay.
    dx: f64,
    /// For the two projections' denominators.
    pub read_e: Vec<(usize, usize, f64)>,
    pub read_h: Vec<(usize, usize, f64)>,
    pub u: Vec<f64>,
    pub i: Vec<f64>,
    /// Stride along the port's axis.
    stride: usize,
}

impl WavePortRun {
    pub fn new(asm: &Assembled, def: WavePortDef) -> Result<Self, String> {
        let mode = solve_mode(asm, &def)?;
        let n = asm.grid.n();
        let strides = [1, n[0], n[0] * n[1]];
        let stride = strides[def.axis];
        let read_offset = 2usize;
        // The cell the wave goes into, which is below the plane for one going down.
        let dx = asm.grid.primal(def.axis, if def.direction > 0.0 { def.plane } else { def.plane - 1 }).max(1e-30);
        // Weights so a pure mode reads exactly its amplitude, whatever the energy norm gives.
        let shift = |p: usize| -> usize {
            if def.direction > 0.0 { p + stride * read_offset } else { p - stride * read_offset }
        };
        let e_den: f64 = mode.e.iter().map(|(t, q, v)| { let m = asm.me[*t][*q]; if m.is_finite() { m * v * v } else { 0.0 } }).sum();
        let read_e = mode
            .e
            .iter()
            .filter(|(t, q, _)| asm.me[*t][*q].is_finite())
            .map(|(t, q, v)| (*t, shift(*q), asm.me[*t][*q] * v / e_den))
            .collect();
        let h_den: f64 = mode.h.iter().map(|(t, q, v)| { let im = asm.imu[*t][*q]; if im > 0.0 { v * v / im } else { 0.0 } }).sum();
        // E is read on a plane and H half a cell either side of it. Averaging the two planes puts
        // the current where the voltage is, which no single phase shift could do for both waves.
        let mut read_h = Vec::with_capacity(mode.h.len() * 2);
        for (t, q, v) in mode.h.iter().filter(|(t, q, _)| asm.imu[*t][*q] > 0.0) {
            let w = v / asm.imu[*t][*q] / h_den * 0.5;
            let at = shift(*q);
            read_h.push((*t, at, w));
            read_h.push((*t, at - stride, w));
        }
        Ok(Self { def, mode, read_offset, dx, read_e, read_h, u: Vec::new(), i: Vec::new(), stride })
    }

    /// Time for the wave to cross half a cell: the delay between E on the plane and H beside it.
    pub fn half_cell_delay(&self) -> f64 {
        0.5 * self.dx * self.mode.eps_eff.sqrt() / crate::C0
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    /// How far inward the voltage is read, metres of line from the injection plane.
    pub fn read_distance(&self, grid: &Grid) -> f64 {
        let l = &grid.lines[self.def.axis];
        if self.def.direction > 0.0 { l[self.def.plane + self.read_offset] - l[self.def.plane] } else { l[self.def.plane] - l[self.def.plane - self.read_offset] }
    }
}

/// A wave port's reading at one frequency: forward and backward waves at its read plane, with the
/// forward direction being into the model.
pub fn waves_at(run: &WavePortRun, dt: f64, f: f64) -> (num_complex::Complex64, num_complex::Complex64) {
    let u = crate::engine::dft(&run.u, dt, 1.0, f);
    let i = crate::engine::dft(&run.i, dt, 0.5, f) * run.def.direction;
    let z = run.mode.z0;
    ((u + i * z) * 0.5, (u - i * z) * 0.5)
}

/// Phase constant of the line at `f`.
pub fn beta(mode: &Mode, f: f64) -> f64 {
    2.0 * std::f64::consts::PI * f * mode.eps_eff.sqrt() / crate::C0
}
