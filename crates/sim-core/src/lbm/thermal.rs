//! Steady heat transfer on the voxel grid, in solids and fluid together. Runs on the CPU, between
//! bursts of the GPU flow solver.
//!
//! Direct coupling in the LBM can't work here: heat crosses solid parts thousands of times slower
//! than air crosses the domain, so a transient thermal LBM would need days to settle. So the flow
//! is frozen, this solves the steady energy balance on it, and for natural convection the resulting
//! buoyancy goes back to the flow. Steady state only, by construction.

use rayon::prelude::*;

/// A cell's thermal role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    /// Solved for, with conductivity and any heat source (W/m3, in the units of the rest).
    Free { conductivity: f64, source: f64 },
    /// Held at a temperature, with its boundary halfway to its neighbours' centres.
    Fixed(f64),
    /// No heat passes: the outside of the domain, or insulated solid.
    Insulated,
    /// A boundary the flow crosses: what goes out carries its own temperature, and anything coming
    /// back in has this one. There is no conduction across it.
    Open(f64),
}

pub struct Problem<'a> {
    pub size: [usize; 3],
    pub dx: f64,
    pub cells: &'a [Cell],
    /// Velocity per cell. Zero in solids.
    pub velocity: &'a [[f64; 3]],
    /// Density times specific heat, per cell.
    pub heat_capacity: &'a [f64],
    /// Periodic along these axes.
    pub periodic: [bool; 3],
}

/// Result of a solve.
#[derive(Debug, Clone, Copy)]
pub struct Report {
    pub sweeps: usize,
    pub change: f64,
}

/// Solves to `tolerance` (the residual as a share of the right-hand side), from and into `t`, by
/// BiCGStab. Relaxation sweeps can't do this: a part conducts thousands of times better than the
/// air around it, and it would warm up only a sliver per sweep. Differences are
/// central where the cell Peclet number allows and upwind where it doesn't (the hybrid scheme,
/// which stays bounded at any Peclet number).
pub fn solve(p: &Problem, t: &mut [f64], tolerance: f64, max_sweeps: usize) -> Report {
    let [nx, ny, nz] = p.size;
    let n = nx * ny * nz;
    assert_eq!(t.len(), n);
    let index = |x: usize, y: usize, z: usize| x + nx * (y + ny * z);
    let dx = p.dx;
    // Coefficients, precomputed: for each cell, six neighbours' (index, conductance) and its source.
    struct Stencil {
        neighbour: [u32; 6],
        a: [f64; 6],
        ap: f64,
        b: f64,
    }
    let stencils: Vec<Option<Stencil>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let Cell::Free { conductivity: k, source } = p.cells[i] else { return None };
            let (z, rem) = (i / (nx * ny), i % (nx * ny));
            let (y, x) = (rem / nx, rem % nx);
            let mut s = Stencil { neighbour: [0; 6], a: [0.0; 6], ap: 0.0, b: source * dx * dx * dx };
            let area = dx * dx;
            for (d, (axis, step)) in [(0usize, 1i64), (0, -1), (1, 1), (1, -1), (2, 1), (2, -1)].into_iter().enumerate() {
                let coord = [x as i64, y as i64, z as i64];
                let size = [nx as i64, ny as i64, nz as i64];
                let mut c = coord;
                c[axis] += step;
                if c[axis] < 0 || c[axis] >= size[axis] {
                    if !p.periodic[axis] {
                        continue;
                    }
                    c[axis] = c[axis].rem_euclid(size[axis]);
                }
                let j = index(c[0] as usize, c[1] as usize, c[2] as usize);
                let (conductance, value) = match p.cells[j] {
                    Cell::Insulated => continue,
                    // A wall halfway between the centres: half the distance.
                    Cell::Fixed(v) => (k * area / (0.5 * dx), Some(v)),
                    Cell::Open(v) => (0.0, Some(v)),
                    // Two materials in series, each over half the distance.
                    Cell::Free { conductivity: other, .. } => (2.0 * k * other / (k + other).max(1e-300) * area / dx, None),
                };
                // Flow out through this face: its velocity is the two cells' mean. Nothing flows through
                // the face of a solid, whatever the fluid cell beside it is doing: averaging with the
                // solid's zero would invent a flow into the wall, and heat with it.
                let still = |v: [f64; 3]| v[0] == 0.0 && v[1] == 0.0 && v[2] == 0.0;
                let face_velocity = if still(p.velocity[i]) || still(p.velocity[j]) { 0.0 } else { 0.5 * (p.velocity[i][axis] + p.velocity[j][axis]) * step as f64 };
                let flow = 0.5 * (p.heat_capacity[i] + p.heat_capacity[j]) * face_velocity * area;
                // Hybrid: central while diffusion dominates, otherwise upwind.
                let coefficient = (-flow).max(conductance - 0.5 * flow).max(0.0);
                // Conservative: what leaves one cell through a face is exactly what enters the next, so
                // the heat out of the domain equals the heat put in, whatever small divergence a
                // lattice velocity field has. (Solving for the rise above ambient keeps that divergence
                // from acting as a source of its own.)
                s.ap += coefficient + flow;
                match value {
                    Some(v) => s.b += coefficient * v,
                    None => {
                        s.neighbour[d] = j as u32;
                        s.a[d] = coefficient;
                    }
                }
            }
            s.ap = s.ap.max(1e-300);
            Some(s)
        })
        .collect();
    for (i, c) in p.cells.iter().enumerate() {
        if let Cell::Fixed(v) = c {
            t[i] = *v;
        }
    }
    // Everything not solved for keeps its value: an identity row.
    let keep: Vec<f64> = t.to_vec();
    let matrix = |x: &[f64], y: &mut [f64]| {
        y.par_iter_mut().enumerate().for_each(|(i, out)| {
            *out = match &stencils[i] {
                Some(s) => {
                    let mut v = s.ap * x[i];
                    for d in 0..6 {
                        if s.a[d] != 0.0 {
                            v -= s.a[d] * x[s.neighbour[d] as usize];
                        }
                    }
                    v
                }
                None => x[i],
            };
        });
    };
    // Jacobi: scales away the huge differences of conductance between metal and air.
    let precondition = |x: &[f64], y: &mut [f64]| {
        y.par_iter_mut().enumerate().for_each(|(i, out)| {
            *out = match &stencils[i] {
                Some(s) => x[i] / s.ap,
                None => x[i],
            };
        });
    };
    let right: Vec<f64> = (0..n).into_par_iter().map(|i| stencils[i].as_ref().map_or(keep[i], |s| s.b)).collect();
    let dot = |a: &[f64], b: &[f64]| a.par_iter().zip(b.par_iter()).map(|(u, v)| u * v).sum::<f64>();
    let norm_b = dot(&right, &right).sqrt().max(1e-300);

    // BiCGStab (van der Vorst, 1992): the system isn't symmetric once there's flow.
    let mut r = vec![0.0; n];
    matrix(t, &mut r);
    r.par_iter_mut().zip(right.par_iter()).for_each(|(ri, bi)| *ri = bi - *ri);
    // The shadow vector can't simply be the first residual: with pure advection the residual moves
    // along the flow, and soon has nothing in common with where it began, which stops the method
    // dead. A fixed scatter of values doesn't have that problem.
    let mut r0: Vec<f64> = (0..n).map(|i| ((i as f64 + 1.0) * 0.618_033_988_75).fract() - 0.5).collect();
    let (mut rho, mut alpha, mut omega) = (1.0f64, 1.0f64, 1.0f64);
    let (mut v, mut pv, mut phat, mut s, mut shat, mut tv) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let mut change = dot(&r, &r).sqrt() / norm_b;
    let mut sweeps = 0;
    while sweeps < max_sweeps && change > tolerance {
        let mut rho_new = dot(&r0, &r);
        if rho_new.abs() < 1e-14 * (dot(&r0, &r0) * dot(&r, &r)).sqrt() {
            // Broken down anyway: start the method again from where the answer has got to.
            r0.clone_from(&r);
            rho_new = dot(&r0, &r);
            if rho_new <= 0.0 {
                break;
            }
            rho = 1.0;
            alpha = 1.0;
            omega = 1.0;
            pv.par_iter_mut().for_each(|x| *x = 0.0);
            v.par_iter_mut().for_each(|x| *x = 0.0);
        }
        let beta = (rho_new / rho) * (alpha / omega);
        pv.par_iter_mut().zip(r.par_iter()).zip(v.par_iter()).for_each(|((p_i, r_i), v_i)| *p_i = r_i + beta * (*p_i - omega * v_i));
        precondition(&pv, &mut phat);
        matrix(&phat, &mut v);
        alpha = rho_new / dot(&r0, &v);
        s.par_iter_mut().zip(r.par_iter()).zip(v.par_iter()).for_each(|((s_i, r_i), v_i)| *s_i = r_i - alpha * v_i);
        precondition(&s, &mut shat);
        matrix(&shat, &mut tv);
        let tt = dot(&tv, &tv);
        omega = if tt > 0.0 { dot(&tv, &s) / tt } else { 0.0 };
        t.par_iter_mut().zip(phat.par_iter()).zip(shat.par_iter()).for_each(|((x, a), b)| *x += alpha * a + omega * b);
        r.par_iter_mut().zip(s.par_iter()).zip(tv.par_iter()).for_each(|((r_i, s_i), t_i)| *r_i = s_i - omega * t_i);
        rho = rho_new;
        change = dot(&r, &r).sqrt() / norm_b;
        sweeps += 1;
        if omega == 0.0 {
            break;
        }
    }
    Report { sweeps, change }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn line(cells: Vec<Cell>, velocity: f64) -> (Vec<f64>, Report) {
        let n = cells.len();
        let u = vec![[velocity, 0.0, 0.0]; n];
        let c = vec![1.0; n];
        let mut t = vec![0.0; n];
        let p = Problem { size: [n, 1, 1], dx: 1.0, cells: &cells, velocity: &u, heat_capacity: &c, periodic: [false, true, true] };
        let r = solve(&p, &mut t, 1e-11, 20_000);
        (t, r)
    }

    #[test]
    fn conduction_between_two_temperatures_is_linear() {
        let mut cells = vec![Cell::Free { conductivity: 1.0, source: 0.0 }; 11];
        cells[0] = Cell::Fixed(0.0);
        cells[10] = Cell::Fixed(10.0);
        let (t, r) = line(cells, 0.0);
        assert!(r.change < 1e-9, "{r:?}");
        assert!(r.sweeps > 0);
        // Walls halfway: 9 free cells across a width of 9.
        for i in 1..10 {
            let want = 10.0 * (i as f64 - 0.5) / 9.0;
            assert!((t[i] - want).abs() < 1e-6, "cell {i}: {} against {want}", t[i]);
        }
    }

    #[test]
    fn two_materials_in_series_share_the_drop_by_their_resistance() {
        // k = 1 for 5 cells then k = 9 for 5: the interface temperature is exact in series.
        let mut cells = vec![Cell::Free { conductivity: 1.0, source: 0.0 }; 12];
        for c in cells.iter_mut().take(11).skip(6) {
            *c = Cell::Free { conductivity: 9.0, source: 0.0 };
        }
        cells[0] = Cell::Fixed(0.0);
        cells[11] = Cell::Fixed(100.0);
        let (t, _) = line(cells, 0.0);
        // Thermal resistances 5/1 and 5/9, in series.
        let q = 100.0 / (5.0 + 5.0 / 9.0);
        assert!((t[1] - q * 0.5).abs() < 1e-5, "{}", t[1]);
        assert!((t[10] - (100.0 - q * 0.5 / 9.0)).abs() < 1e-5, "{}", t[10]);
    }

    #[test]
    fn a_heat_source_gives_the_parabola() {
        let mut cells = vec![Cell::Free { conductivity: 2.0, source: 1.0 }; 22];
        cells[0] = Cell::Fixed(0.0);
        cells[21] = Cell::Fixed(0.0);
        let (t, _) = line(cells, 0.0);
        // Width 20, T(x) = q x (L - x) / 2k.
        let x = 9.5;
        // Plus the second-order offset of a wall half a cell from the first centre: q dx^2 / 8k.
        let want = x * (20.0 - x) / 4.0 + 1.0 / 16.0;
        assert!((t[10] - want).abs() < 1e-3, "{} against {want}", t[10]);
    }

    #[test]
    fn heat_put_in_leaves_with_the_flow() {
        // Flow along a line from a fixed inlet out through an open end, with a source in between:
        // the outlet must be warmer by exactly source / (capacity x velocity).
        let mut cells = vec![Cell::Free { conductivity: 1e-3, source: 0.0 }; 22];
        cells[0] = Cell::Fixed(0.0);
        cells[21] = Cell::Open(0.0);
        cells[10] = Cell::Free { conductivity: 1e-3, source: 0.5 };
        let (t, _) = line(cells, 2.0);
        assert!((t[20] - 0.25).abs() < 1e-3, "{}", t[20]);
    }

    #[test]
    fn a_small_source_is_still_solved_properly() {
        // Temperatures of order 1e-7: an absolute tolerance would call this done at once.
        let mut cells = vec![Cell::Free { conductivity: 1.0, source: 1e-9 }; 22];
        cells[0] = Cell::Fixed(0.0);
        cells[21] = Cell::Fixed(0.0);
        let n = cells.len();
        let (u, c) = (vec![[0.0; 3]; n], vec![1.0; n]);
        let mut t = vec![0.0; n];
        let p = Problem { size: [n, 1, 1], dx: 1.0, cells: &cells, velocity: &u, heat_capacity: &c, periodic: [false, true, true] };
        solve(&p, &mut t, 1e-10, 20_000);
        let want = 1e-9 * 9.5 * 10.5 / 2.0 + 1e-9 / 8.0;
        assert!((t[10] - want).abs() / want < 1e-3, "{} against {want}", t[10]);
    }

    #[test]
    fn heat_from_a_block_in_a_stream_all_leaves_with_the_stream() {
        // A hot metal block in still air, with streams flowing past on either side: a flow that truly
        // has no divergence, in which all the heat must cross by conduction and then leave with the
        // stream. Energy must be conserved.
        let (nx, ny) = (60usize, 21usize);
        let n = nx * ny;
        let index = |x: usize, y: usize| x + nx * y;
        let mut cells = vec![Cell::Free { conductivity: 0.025, source: 0.0 }; n];
        let mut velocity = vec![[1.0, 0.0, 0.0]; n];
        let capacity = vec![1200.0; n];
        let mut power = 0.0;
        for y in 0..ny {
            cells[index(0, y)] = Cell::Fixed(0.0);
            cells[index(nx - 1, y)] = Cell::Open(0.0);
        }
        for y in 7..14 {
            for x in 0..nx {
                velocity[index(x, y)] = [0.0; 3];
            }
        }
        for y in 8..13 {
            for x in 20..30 {
                cells[index(x, y)] = Cell::Free { conductivity: 50.0, source: 2.0 };
                power += 2.0;
            }
        }
        let mut t = vec![0.0; n];
        let p = Problem { size: [nx, ny, 1], dx: 1.0, cells: &cells, velocity: &velocity, heat_capacity: &capacity, periodic: [false, true, true] };
        let r = solve(&p, &mut t, 1e-9, 20_000);
        assert!(r.change < 1e-8, "{r:?}");
        let carried: f64 = (0..ny).map(|y| 1200.0 * velocity[index(nx - 2, y)][0] * t[index(nx - 2, y)]).sum();
        let off = (carried - power).abs() / power;
        assert!(off < 0.02, "{power} went in and {carried} came out");
        assert!(t[index(25, 10)] > t[index(50, 10)], "the block must be the hottest place");
    }

    #[test]
    fn a_metal_block_in_air_reaches_its_steady_temperature() {
        // Conductivity ratio 2000: the case relaxation sweeps can't do. Heat from the block must all
        // cross the air on either side, which fixes the block's temperature exactly.
        let (air, metal) = (0.025, 50.0);
        let mut cells = vec![Cell::Free { conductivity: air, source: 0.0 }; 42];
        for c in cells.iter_mut().take(26).skip(16) {
            *c = Cell::Free { conductivity: metal, source: 1.0 };
        }
        cells[0] = Cell::Fixed(0.0);
        cells[41] = Cell::Fixed(0.0);
        let (t, r) = line(cells, 0.0);
        assert!(r.sweeps < 500, "took {} iterations", r.sweeps);
        // 10 W per unit area, half each way, across the 15 cells of air between wall and block.
        let surface = 5.0 * 15.0 / air;
        assert!((t[20] - surface).abs() / surface < 0.002, "{} against about {surface}", t[20]);
    }

    #[test]
    fn strong_flow_stays_bounded_and_carries_the_inlet_along() {
        let mut cells = vec![Cell::Free { conductivity: 1e-3, source: 0.0 }; 12];
        cells[0] = Cell::Fixed(1.0);
        cells[11] = Cell::Fixed(0.0);
        let (t, _) = line(cells, 1.0);
        for v in &t {
            assert!(*v >= -1e-9 && *v <= 1.0 + 1e-9, "unbounded: {t:?}");
        }
        assert!((t[5] - 1.0).abs() < 1e-3, "{t:?}");
    }
}
