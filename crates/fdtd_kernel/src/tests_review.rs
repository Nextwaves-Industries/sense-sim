//! What an independent read of this code found, each pinned so it stays fixed.

use std::collections::HashSet;

use crate::assemble::assemble;
use crate::engine::{Engine, Stop, stable_dt};
use crate::grid::{Grid, even};
use crate::problem::*;

fn cube(half: f64, n: usize) -> Grid {
    let l = even(-half, half, n);
    Grid::new([l.clone(), l.clone(), l])
}

/// An edge takes one update. Two would integrate it twice a step, with a port reading about half
/// its impedance, or NaN where a Debye branch meets perfect metal.
#[test]
fn every_edge_is_on_one_update_list_only() {
    let g = cube(0.02, 41);
    let mut p = Problem::new(g);
    // Water around all of it, a copper sheet through it, a port across a gap in the sheet, and a
    // copper body with a copper sheet on its face.
    p.bodies.push(Body { solid: Solid::Box { min: [-0.01; 3], max: [0.01; 3] }, mat: Mat::Dielectric { eps_r: 5.2, mu_r: 1.0, kappa: 0.1, debye: Some(Debye { delta_eps: 73.0, tau: 8e-12 }) } });
    p.bodies.push(Body { solid: Solid::Box { min: [0.012, -0.004, -0.004], max: [0.016, 0.004, 0.004] }, mat: Mat::Metal { sigma: 5.8e7 } });
    let copper = Some(Surface::sheet(5.8e7, 35e-6, 1e9));
    let w = 0.004;
    p.sheets.push(Sheet { axis: 2, at: 0.0, polys: vec![vec![[-0.008, -w], [-0.001, -w], [-0.001, w], [-0.008, w]], vec![[0.001, -w], [0.008, -w], [0.008, w], [0.001, w]]], surface: copper });
    p.sheets.push(Sheet { axis: 0, at: 0.012, polys: vec![vec![[-0.004, -0.004], [0.004, -0.004], [0.004, 0.004], [-0.004, 0.004]]], surface: copper });
    // A perfect sheet inside the water as well: its edges can't carry a branch at all.
    p.sheets.push(Sheet { axis: 1, at: 0.006, polys: vec![vec![[-0.004, -0.004], [0.004, -0.004], [0.004, 0.004], [-0.004, 0.004]]], surface: None });
    p.ports.push(Port { a: [-0.001, 0.0, 0.0], b: [0.001, 0.0, 0.0], axis: 0, width_axis: Some(1), width: 2.0 * w, second: None, z0: 50.0 });
    let asm = assemble(&p);
    assert!(!asm.rl.is_empty() && !asm.debye.is_empty());
    let mut seen = HashSet::new();
    for e in &asm.rl {
        assert!(seen.insert((e.axis, e.node)), "metal edge listed twice: {e:?}");
    }
    for port in &asm.ports {
        for (n, _) in &port.edges {
            assert!(seen.insert((port.axis, *n)), "port edge {n} also carries metal");
        }
    }
    // Plain loss has no place on metal or a port. It can share an edge with a Debye branch: a material
    // can have both, and the two are applied one after the other.
    for &(a, n) in &seen {
        if let Some(gv) = &asm.g[a] {
            assert_eq!(gv[n], 0.0, "plain loss on a metal or port edge");
        }
    }
    for d in &asm.debye {
        assert!(seen.insert((d.axis, d.node)), "Debye branch on a taken edge: {d:?}");
        assert!(asm.me[d.axis][d.node].is_finite(), "Debye branch on perfect metal: {d:?}");
    }
    // And it runs.
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 8);
    eng.drive(0, Pulse { f0: 3e9, fc: 2e9 });
    eng.advance_many(800);
    assert!(eng.energy().is_finite());
}

/// A subcell factor changes a cell's field shape, which sets its conductance as well as its
/// capacitance: tan δ has to stay what the material says.
#[test]
fn a_subcell_correction_keeps_the_loss_tangent() {
    let g = cube(0.02, 41);
    let mut p = Problem::new(g.clone());
    let (er, kappa) = (4.0, 0.2);
    p.bodies.push(Body { solid: Solid::Box { min: [-0.015; 3], max: [0.015; 3] }, mat: Mat::Dielectric { eps_r: er, mu_r: 1.0, kappa, debye: None } });
    let plain = assemble(&p);
    // A strip across the middle, edge between lines, inside the material.
    p.sheets.push(Sheet { axis: 2, at: 0.0, polys: vec![vec![[-0.0123, -0.0013], [0.0123, -0.0013], [0.0123, 0.0013], [-0.0123, 0.0013]]], surface: None });
    let cut = assemble(&p);
    let gc = cut.g[0].as_ref().unwrap();
    let mut corrected = 0;
    for n in 0..g.nodes() {
        let (m0, m1) = (plain.me[0][n], cut.me[0][n]);
        if m0.is_finite() && m1.is_finite() && (m1 / m0 - 1.0).abs() > 0.02 && gc[n] > 0.0 {
            corrected += 1;
            // Inside uniform material, G/C is κ/ε whatever the cell's shape.
            let ratio = gc[n] / m1;
            let want = kappa / (er * crate::EPS0);
            assert!((ratio / want - 1.0).abs() < 1e-6, "{ratio} against {want}");
        }
    }
    assert!(corrected > 0, "the strip corrected nothing");
}

/// A sheet a hair off its line is still in that plane. It used to count as above it, which drops
/// the lower half of every dual face next to it, and halves their capacitance.
#[test]
fn a_sheet_slightly_off_its_line_gets_the_same_correction() {
    let g = cube(0.02, 41);
    let strip = vec![vec![[-0.0123, -0.0013], [0.0123, -0.0013], [0.0123, 0.0013], [-0.0123, 0.0013]]];
    let build = |at: f64| {
        let mut p = Problem::new(g.clone());
        p.sheets.push(Sheet { axis: 2, at, polys: strip.clone(), surface: None });
        assemble(&p)
    };
    let (exact, off) = (build(0.0), build(1e-7));
    for a in 0..3 {
        for n in 0..g.nodes() {
            let (x, y) = (exact.me[a][n], off.me[a][n]);
            if x.is_finite() || y.is_finite() {
                assert!((x / y - 1.0).abs() < 1e-9, "axis {a} node {n}: {x} against {y}");
            }
        }
    }
}

/// The test for a run blowing up: too long a time step has to be caught, not reported as a run that
/// only needs more steps.
#[test]
fn an_unstable_run_is_reported_as_diverged() {
    let g = cube(0.02, 31);
    let mut p = Problem::new(g);
    p.walls = [Wall::Pec; 6];
    p.ports.push(Port { a: [0.0, 0.0, 0.0], b: [0.0, 0.0, 40e-3 / 30.0], axis: 2, width_axis: None, width: 0.0, second: None, z0: 50.0 });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt * 1.1, 4);
    eng.drive(0, Pulse { f0: 5e9, fc: 3e9 });
    let stop = eng.run(60.0, 200_000, |_, _| true);
    assert_eq!(stop, Stop::Diverged);
}

/// Slow growth is the hard case: the old test compared the energy with a peak that had just been
/// raised to it. Here a stable run has to end normally, at the time step the estimate gives.
#[test]
fn a_run_at_the_measured_step_stays_stable_with_a_hot_spot() {
    let g = cube(0.02, 31);
    let mut p = Problem::new(g.clone());
    p.walls = [Wall::Pec; 6];
    p.ports.push(Port { a: [0.0, 0.0, 0.0], b: [0.0, 0.0, 40e-3 / 30.0], axis: 2, width_axis: None, width: 0.0, second: None, z0: 50.0 });
    let mut asm = assemble(&p);
    // A few faces well above the rest: what a sliver of open metal looks like to the time step.
    let [nx, ny, nz] = g.n();
    let at = g.idx(nx / 2 + 3, ny / 2 + 3, nz / 2 + 3);
    asm.imu[1][at] *= 1.9;
    asm.imu[2][at + 1] *= 1.9;
    let dt = stable_dt(&asm);
    let courant = (40e-3 / 30.0) / (crate::C0 * 3f64.sqrt());
    println!("time step {:.3} of Courant", dt / courant);
    let mut eng = Engine::<f64>::new(&asm, dt, 4);
    eng.drive(0, Pulse { f0: 5e9, fc: 3e9 });
    eng.advance_many(600);
    let w0 = eng.energy();
    eng.advance_many(20_000);
    let w1 = eng.energy();
    println!("energy {w0:e} then {w1:e}");
    assert!(w1.is_finite() && w1 < w0 * 1.5, "{w0} {w1}");
}

/// A port lying on an outer wall has no neighbours to update from.
#[test]
fn a_port_on_an_outer_wall_warns_and_does_not_read_out_of_bounds() {
    let z = even(0.0, 0.02, 21);
    let x = even(-0.02, 0.02, 41);
    let g = Grid::new([x.clone(), x, z]);
    let mut p = Problem::new(g);
    p.walls[4] = Wall::Pec;
    p.ports.push(Port { a: [-0.001, 0.0, 0.0], b: [0.001, 0.0, 0.0], axis: 0, width_axis: None, width: 0.0, second: None, z0: 50.0 });
    let asm = assemble(&p);
    assert!(asm.warnings.iter().any(|w| w.contains("outer wall")), "{:?}", asm.warnings);
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 2);
    eng.drive(0, Pulse { f0: 5e9, fc: 3e9 });
    eng.advance_many(50);
}

/// A slanting strip has to conduct end to end whatever the mesh does to it: nodes joined by metal
/// from one end to the other, at sizes from far narrower than a cell to about two cells across.
#[test]
fn a_slanting_strip_is_joined_up_at_any_width() {
    let g = cube(0.05, 51);
    for (slope, reach) in [(1.0f64, 0.03f64), (0.5, 0.03), (0.2, 0.03)] {
        for w in [0.1e-3, 0.5e-3, 1.0e-3, 1.5e-3, 2.0e-3, 2.6e-3, 3.5e-3] {
            let cut = w * (1.0 + slope * slope).sqrt();
            let mut p = Problem::new(g.clone());
            let strip = vec![[-reach, -reach * slope - cut / 2.0], [reach, reach * slope - cut / 2.0], [reach, reach * slope + cut / 2.0], [-reach, -reach * slope + cut / 2.0]];
            p.sheets.push(Sheet { axis: 2, at: 0.0, polys: vec![strip], surface: None });
            let asm = assemble(&p);
            let [nx, ny, _] = g.n();
            let k = g.nearest(2, 0.0);
            let id = |i: usize, j: usize| i + nx * j;
            let mut parent: Vec<usize> = (0..nx * ny).collect();
            fn find(p: &mut [usize], x: usize) -> usize {
                let mut r = x;
                while p[r] != r {
                    p[r] = p[p[r]];
                    r = p[r];
                }
                r
            }
            for j in 0..ny {
                for i in 0..nx {
                    let n = g.idx(i, j, k);
                    if i + 1 < nx && asm.me[0][n].is_infinite() {
                        let (a, b) = (find(&mut parent, id(i, j)), find(&mut parent, id(i + 1, j)));
                        parent[a] = b;
                    }
                    if j + 1 < ny && asm.me[1][n].is_infinite() {
                        let (a, b) = (find(&mut parent, id(i, j)), find(&mut parent, id(i, j + 1)));
                        parent[a] = b;
                    }
                }
            }
            // A little inside each end, on the nearest metal.
            let on_metal = |x: f64, y: f64| {
                let (i0, j0) = (g.nearest(0, x), g.nearest(1, y));
                let mut near = None;
                for j in j0.saturating_sub(2)..(j0 + 3).min(ny - 1) {
                    for i in i0.saturating_sub(2)..(i0 + 3).min(nx - 1) {
                        let n = g.idx(i, j, k);
                        if asm.me[0][n].is_infinite() || asm.me[1][n].is_infinite() {
                            near = Some(id(i, j));
                        }
                    }
                }
                near
            };
            let d = reach * 0.9;
            let (a, b) = (on_metal(-d, -d * slope), on_metal(d, d * slope));
            let (a, b) = (a.unwrap_or_else(|| panic!("no metal at one end: slope {slope}, width {w}")), b.unwrap_or_else(|| panic!("no metal at the other end: slope {slope}, width {w}")));
            assert_eq!(find(&mut parent, a), find(&mut parent, b), "not joined up: slope {slope}, width {w}");
        }
    }
}
