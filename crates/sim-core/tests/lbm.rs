//! Verification of the GPU LBM solver against analytic flows, plus its measured speed.
//!
//!     cargo test --release --test lbm -- --ignored --nocapture --test-threads=1

use std::f64::consts::PI;
use std::time::Instant;

use sim_core::jobs::Cancel;
use sim_core::lbm::{Lbm, Setup, WALL};

fn setup(size: [u32; 3], viscosity: f32) -> Setup {
    Setup { size, viscosity, smagorinsky: 0.0, force: [0.0; 3], wall_velocity: [0.0; 3], inlet_velocity: [0.0; 3], inlet_z0: 0.0, inlet_reference_height: 1.0, inlet_channel: 0.0, rough: 0.0, mixing_max: 0.0 }
}

/// A decaying Taylor-Green vortex on `n` x `n`, returns the relative L2 error of velocity at the
/// time the amplitude has fallen by about e^-1.
fn taylor_green_error(n: u32) -> f64 {
    // Diffusive scaling: velocity halves as the grid doubles, viscosity stays.
    let nu = 0.02f64;
    let u0 = 0.08 * 32.0 / f64::from(n);
    let k = 2.0 * PI / f64::from(n);
    let steps = (1.0 / (2.0 * nu * k * k)).round() as u64;
    let nz = 4;
    let cells = (n * n * nz) as usize;
    let mut drho = vec![0.0f32; cells];
    let mut u = vec![[0.0f32; 3]; cells];
    for z in 0..nz {
        for y in 0..n {
            for x in 0..n {
                let i = (x + n * (y + n * z)) as usize;
                let (fx, fy) = (k * f64::from(x), k * f64::from(y));
                u[i] = [(-u0 * fx.cos() * fy.sin()) as f32, (u0 * fx.sin() * fy.cos()) as f32, 0.0];
                // The vortex's pressure field, p = rho c^2 ... with c^2 = 1/3.
                drho[i] = (-3.0 * u0 * u0 / 4.0 * ((2.0 * fx).cos() + (2.0 * fy).cos())) as f32;
            }
        }
    }
    let mut lbm = Lbm::new(&setup([n, n, nz], nu as f32), &[]).unwrap();
    lbm.initialise(&drho, &u).unwrap();
    lbm.run(steps, &Cancel::default(), |_| {}).unwrap();
    let m = lbm.read_macros().unwrap();
    let decay = (-2.0 * nu * k * k * lbm.steps() as f64).exp();
    let (mut error, mut norm) = (0.0, 0.0);
    for z in 0..nz {
        for y in 0..n {
            for x in 0..n {
                let i = (x + n * (y + n * z)) as usize;
                let (fx, fy) = (k * f64::from(x), k * f64::from(y));
                let (ax, ay) = (-u0 * decay * fx.cos() * fy.sin(), u0 * decay * fx.sin() * fy.cos());
                error += (f64::from(m.ux[i]) - ax).powi(2) + (f64::from(m.uy[i]) - ay).powi(2);
                norm += ax * ax + ay * ay;
            }
        }
    }
    // Signed: the amplitude against the exact one, and any uniform drift of the whole flow.
    let (mut dot, mut self_dot, mut mean) = (0.0, 0.0, 0.0);
    for z in 0..nz {
        for y in 0..n {
            for x in 0..n {
                let i = (x + n * (y + n * z)) as usize;
                let (fx, fy) = (k * f64::from(x), k * f64::from(y));
                let (ax, ay) = (-u0 * decay * fx.cos() * fy.sin(), u0 * decay * fx.sin() * fy.cos());
                dot += f64::from(m.ux[i]) * ax + f64::from(m.uy[i]) * ay;
                self_dot += ax * ax + ay * ay;
                mean += f64::from(m.ux[i]);
            }
        }
    }
    println!("N {n}: u0 {u0:.5}, {} steps, L2 {:.3e}, amplitude ratio - 1 = {:+.3e}, mean ux {:+.2e} ({:+.2e} per step)", lbm.steps(), (error / norm).sqrt(), dot / self_dot - 1.0, mean / cells as f64, mean / cells as f64 / lbm.steps() as f64);
    (error / norm).sqrt()
}

#[test]
#[ignore = "needs a GPU"]
fn sim_lbm_taylor_green_second_order() {
    // Lattice velocities stay at 0.02 and above, clear of the single-precision floor (see `lbm`).
    let grids = [16u32, 32, 64, 128, 256];
    let e: Vec<f64> = grids.iter().map(|n| taylor_green_error(*n)).collect();
    println!("L2 errors at {grids:?}: {e:?}");
    let orders: Vec<f64> = e.windows(2).map(|w| (w[0] / w[1]).log2()).collect();
    println!("observed order: {orders:?}");
    assert!(e[3] < 2e-3, "error at 128 is {}", e[3]);
    // The asymptotic order: the finest pair.
    for o in &orders[orders.len() - 1..] {
        assert!((*o - 2.0).abs() <= 0.2, "order {o} is not 2.0 +/- 0.2");
    }
}

/// Plane Poiseuille flow between bounce-back walls, driven by a body force: the walls must sit
/// exactly halfway between nodes, which TRT's magic combination guarantees.
#[test]
#[ignore = "needs a GPU"]
fn sim_lbm_poiseuille_walls_are_halfway() {
    for nu in [0.02f32, 0.1, 0.3] {
        let (nx, ny, nz) = (4u32, 34u32, 4u32);
        let g = 2e-6f32;
        let mut flags = vec![0u32; (nx * ny * nz) as usize];
        for z in 0..nz {
            for x in 0..nx {
                flags[(x + nx * (0 + ny * z)) as usize] = WALL;
                flags[(x + nx * ((ny - 1) + ny * z)) as usize] = WALL;
            }
        }
        let mut s = setup([nx, ny, nz], nu);
        s.force = [g, 0.0, 0.0];
        let mut lbm = Lbm::new(&s, &flags).unwrap();
        // Enough diffusion times to settle.
        let h = f64::from(ny - 2);
        let steps = (4.0 * h * h / f64::from(nu)) as u64;
        lbm.run(steps, &Cancel::default(), |_| {}).unwrap();
        let m = lbm.read_macros().unwrap();
        let mut worst: f64 = 0.0;
        for y in 1..ny - 1 {
            let i = (1 + nx * (y + ny)) as usize;
            // Walls halfway between y = 0 and 1, and between ny - 2 and ny - 1.
            let d = f64::from(y) - 0.5;
            let exact = f64::from(g) / (2.0 * f64::from(nu)) * d * (h - d);
            worst = worst.max((f64::from(m.ux[i]) - exact).abs() / (f64::from(g) * h * h / (8.0 * f64::from(nu))));
        }
        println!("nu {nu}: worst velocity error {:.4}% of peak", worst * 100.0);
        assert!(worst < 2e-3, "viscosity {nu}: error {worst} of peak");
    }
}

#[test]
#[ignore = "needs a GPU"]
fn sim_lbm_speed() {
    let n = 216u32; // About 10 million cells.
    let s = setup([n, n, n], 0.02);
    let mut lbm = Lbm::new(&s, &[]).unwrap();
    println!("{} cells on {}", lbm.cells(), lbm.adapter_name());
    let drho = vec![0.0f32; lbm.cells() as usize];
    let u = vec![[0.05f32, 0.0, 0.0]; lbm.cells() as usize];
    lbm.initialise(&drho, &u).unwrap();
    // Warm up, then measure.
    lbm.run(40, &Cancel::default(), |_| {}).unwrap();
    let t0 = Instant::now();
    let steps = 1000;
    lbm.run(steps, &Cancel::default(), |_| {}).unwrap();
    let mlups = steps as f64 * f64::from(lbm.cells()) / t0.elapsed().as_secs_f64() / 1e6;
    println!("speed: {mlups:.0} MLUPs");
    let m = lbm.read_macros().unwrap();
    let drift = m.ux.iter().map(|v| (f64::from(*v) - 0.05).abs()).fold(0.0, f64::max);
    println!("velocity drift in a uniform stream: {drift:.2e}");
    assert!(drift < 1e-4, "a uniform stream must stay uniform");
    assert!(mlups >= 250.0, "{mlups} MLUPs is under the kill line of 250");
}

/// Flow past a cylinder in a channel, against Schaefer and Turek (1996), case 2D-1 (Re 20):
/// drag coefficient 5.57 to 5.59. Walls here follow the voxels, so this measures how far that goes.
#[test]
#[ignore = "needs a GPU"]
fn sim_lbm_cylinder_schaefer_turek() {
    use sim_core::lbm::{INLET, OUTLET};
    // Measured at peak lattice speed 0.05: C_D 5.49, 5.41, 5.40 at D = 20, 40, 80, so a finer grid
    // doesn't close the gap. At D = 40: 5.409, 5.501, 5.551 at speeds 0.05, 0.025, 0.0125. The gap to
    // 5.58 halves with the speed, and those three extrapolate to 5.60. LBM_CYLINDER_MAX and
    // LBM_CYLINDER_U repeat them.
    let finest: u32 = std::env::var("LBM_CYLINDER_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    for d in [20u32, 40, 80].into_iter().filter(|d| *d <= finest) {
        // Lengths of the benchmark in units of the diameter (0.1): 2.2 long, 0.41 high.
        let scale = f64::from(d) / 0.1;
        let height = (0.41 * scale).round() as u32;
        let (nx, ny, nz) = ((2.2 * scale) as u32, height + 2, 3u32);
        let (cx, cy) = (0.2 * scale, 0.2 * scale + 0.5);
        let radius = f64::from(d) / 2.0;
        let mut flags = vec![0u32; (nx * ny * nz) as usize];
        let at = |x: u32, y: u32, z: u32| (x + nx * (y + ny * z)) as usize;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let f = &mut flags[at(x, y, z)];
                    if y == 0 || y == ny - 1 {
                        *f = WALL;
                    } else if x == 0 {
                        *f = INLET;
                    } else if x == nx - 1 {
                        *f = OUTLET;
                    } else if (f64::from(x) - cx).powi(2) + (f64::from(y) - cy).powi(2) <= radius * radius {
                        *f = WALL;
                    }
                }
            }
        }
        // Peak 0.05 in lattice units; mean velocity at the benchmark's ratio, 2/3 of the peak.
        let u_max: f32 = std::env::var("LBM_CYLINDER_U").ok().and_then(|v| v.parse().ok()).unwrap_or(0.025);
        let u_mean = f64::from(u_max) * 2.0 / 3.0;
        let nu = u_mean * f64::from(d) / 20.0;
        let mut s = setup([nx, ny, nz], nu as f32);
        s.inlet_velocity = [u_max, 0.0, 0.0];
        s.inlet_channel = height as f32;
        let mut lbm = Lbm::new(&s, &flags).unwrap();
        // Long enough for the flow through the channel, several times.
        let steps = (8.0 * f64::from(nx) / u_mean) as u64;
        lbm.run(steps, &Cancel::default(), |_| {}).unwrap();
        // Only the cylinder, not the channel walls.
        let on_cylinder = |cell: usize| {
            let y = (cell / nx as usize) % ny as usize;
            y != 0 && y != ny as usize - 1
        };
        let f = lbm.wall_force(&flags, on_cylinder).unwrap();
        let drag = f[0] / f64::from(nz);
        let cd = 2.0 * drag / (u_mean * u_mean * f64::from(d));
        println!("D = {d} cells, {} cells in all, {steps} steps: C_D = {cd:.3}", lbm.cells());
        if d == finest {
            assert!((cd - 5.58).abs() / 5.58 <= 0.03, "C_D {cd} against 5.58");
        }
    }
}
/// Natural convection in a square cavity, against de Vahl Davis (1983): the GPU flow solver and the
/// CPU energy solver, passing buoyancy and velocity back and forth until they agree.
#[test]
#[ignore = "needs a GPU"]
fn sim_cht_voxel_de_vahl_davis() {
    use sim_core::lbm::thermal::{self, Cell};
    let n = 130u32;
    let (nx, ny, nz) = (n, 3u32, n);
    let width = f64::from(n - 2);
    let (ra, pr) = (1e5f64, 0.71f64);
    // Free-fall velocity 0.05: then g beta dT = u^2 / L, and the viscosity follows from Ra and Pr.
    let u_ff = 0.05f64;
    let g_beta_dt = u_ff * u_ff / width;
    let nu = u_ff * width * (pr / ra).sqrt();
    let alpha = nu / pr;
    let count = (nx * ny * nz) as usize;
    let at = |x: u32, y: u32, z: u32| (x + nx * (y + ny * z)) as usize;
    let mut flags = vec![0u32; count];
    let mut cells = vec![Cell::Free { conductivity: alpha, source: 0.0 }; count];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = at(x, y, z);
                if x == 0 {
                    flags[i] = WALL;
                    cells[i] = Cell::Fixed(0.5);
                } else if x == nx - 1 {
                    flags[i] = WALL;
                    cells[i] = Cell::Fixed(-0.5);
                } else if z == 0 || z == nz - 1 {
                    flags[i] = WALL;
                    cells[i] = Cell::Insulated;
                }
            }
        }
    }
    let mut lbm = Lbm::new(&setup([nx, ny, nz], nu as f32), &flags).unwrap();
    let mut temperature = vec![0.0f64; count];
    let capacity = vec![1.0f64; count];
    let t0 = Instant::now();
    let mut last_nu = 0.0;
    for outer in 0..120 {
        let force: Vec<f32> = temperature.iter().zip(&flags).map(|(t, f)| if *f == WALL { 0.0 } else { (g_beta_dt * t) as f32 }).collect();
        lbm.set_cell_force(&force);
        lbm.run(4000, &Cancel::default(), |_| {}).unwrap();
        let m = lbm.read_macros().unwrap();
        let velocity: Vec<[f64; 3]> = (0..count).map(|i| if flags[i] == WALL { [0.0; 3] } else { [f64::from(m.ux[i]), f64::from(m.uy[i]), f64::from(m.uz[i])] }).collect();
        let problem = thermal::Problem { size: [nx as usize, ny as usize, nz as usize], dx: 1.0, cells: &cells, velocity: &velocity, heat_capacity: &capacity, periodic: [false, true, false] };
        thermal::solve(&problem, &mut temperature, 1e-8, 4000);
        // Nusselt number at the hot wall, second order from the two nearest columns.
        let mean = |x: u32| (1..nz - 1).map(|z| temperature[at(x, 1, z)]).sum::<f64>() / f64::from(nz - 2);
        let (t1, t2) = (mean(1), mean(2));
        let gradient = (9.0 * (t1 - 0.5) - (t2 - 0.5)) / 3.0;
        let nusselt = -gradient * width;
        println!("outer {outer}: Nu {nusselt:.4} at {:.0} s", t0.elapsed().as_secs_f64());
        if outer > 10 && (nusselt - last_nu).abs() < 1e-3 * nusselt {
            last_nu = nusselt;
            break;
        }
        last_nu = nusselt;
    }
    let err = (last_nu - 4.519).abs() / 4.519;
    println!("Nu {last_nu:.4} against 4.519: {:.2}% off", err * 100.0);
    assert!(err <= 0.03, "Nu {last_nu} is {:.2}% from 4.519", err * 100.0);
}
/// Air between plates heated through solid walls: the flow from the GPU solver, the heat from the
/// energy solver, against the isoflux channel's exact result. In the developed part, a point in the
/// solid sits q (d / k_s + D_h / (8.235 k_f)) above the air's mean temperature, with d its distance
/// from the air.
#[test]
#[ignore = "needs a GPU"]
fn sim_cht_voxel_channel() {
    use sim_core::lbm::thermal::{self, Cell};
    let (gap, wall) = (40usize, 8usize);
    let ny = gap + 2 * wall;
    // The flow, developed: a short periodic stretch driven by a body force.
    let (nx_flow, nz_flow) = (4u32, 3u32);
    let nu = 0.1f64;
    let g = 8.0 * nu * 0.02 / (gap as f64 * gap as f64);
    let solid_row = |y: usize| y < wall || y >= wall + gap;
    let mut flags = vec![0u32; nx_flow as usize * ny * nz_flow as usize];
    for z in 0..nz_flow as usize {
        for y in 0..ny {
            for x in 0..nx_flow as usize {
                if solid_row(y) {
                    flags[x + nx_flow as usize * (y + ny * z)] = WALL;
                }
            }
        }
    }
    let mut s = setup([nx_flow, ny as u32, nz_flow], nu as f32);
    s.force = [g as f32, 0.0, 0.0];
    let mut lbm = Lbm::new(&s, &flags).unwrap();
    lbm.run((4.0 * (gap * gap) as f64 / nu) as u64, &Cancel::default(), |_| {}).unwrap();
    let m = lbm.read_macros().unwrap();
    let profile: Vec<f64> = (0..ny).map(|y| if solid_row(y) { 0.0 } else { f64::from(m.ux[1 + nx_flow as usize * (y + ny)]) }).collect();
    let mean = profile.iter().sum::<f64>() / gap as f64;
    println!("mean speed {mean:.5}, peak {:.5}", profile.iter().copied().fold(0.0, f64::max));
    // The heat, on a longer stretch of the same channel.
    let d_h = 2.0 * gap as f64;
    let k_f = mean * d_h / 20.0;
    let k_s = 5.0 * k_f;
    let q = 1.0;
    let nx = 400usize;
    let at = |x: usize, y: usize| x + nx * y;
    let (mut cells, mut velocity) = (vec![Cell::Free { conductivity: k_f, source: 0.0 }; nx * ny], vec![[0.0; 3]; nx * ny]);
    for y in 0..ny {
        for x in 0..nx {
            let i = at(x, y);
            velocity[i] = [profile[y], 0.0, 0.0];
            if solid_row(y) {
                // The heat comes in through the outermost row of each wall.
                let source = if (y == 0 || y == ny - 1) && x > 0 && x < nx - 1 { q } else { 0.0 };
                cells[i] = if x == 0 || x == nx - 1 { Cell::Insulated } else { Cell::Free { conductivity: k_s, source } };
            } else if x == 0 {
                cells[i] = Cell::Fixed(0.0);
            } else if x == nx - 1 {
                cells[i] = Cell::Open(0.0);
            }
        }
    }
    let capacity = vec![1.0; nx * ny];
    let mut temperature = vec![0.0; nx * ny];
    let problem = thermal::Problem { size: [nx, ny, 1], dx: 1.0, cells: &cells, velocity: &velocity, heat_capacity: &capacity, periodic: [false, false, true] };
    let report = thermal::solve(&problem, &mut temperature, 1e-9, 60_000);
    println!("{} iterations, residual {:.2e}", report.sweeps, report.change);
    let bulk = |x: usize| (0..ny).map(|y| profile[y] * temperature[at(x, y)]).sum::<f64>() / (mean * gap as f64);
    // All the heat leaves: with the air through the outlet, and at this low speed, a little by
    // conduction back into the cold inlet.
    let power = 2.0 * q * (nx - 2) as f64;
    let carried = (0..ny).map(|y| profile[y] * temperature[at(nx - 2, y)]).sum::<f64>();
    let back: f64 = (wall..wall + gap).map(|y| k_f * temperature[at(1, y)] / 0.5).sum();
    println!("{power:.2} in, {carried:.2} carried out, {back:.2} conducted back");
    assert!((carried + back - power).abs() / power < 0.005, "{} against {power}", carried + back);
    // Developed well before here: 0.05 Pe D_h is 80 cells.
    let station = 320;
    let in_wall = 6.5;
    let got = temperature[at(station, 1)] - bulk(station);
    let exact = q * (in_wall / k_s + d_h / (8.235 * k_f));
    let err = (got - exact).abs() / exact;
    println!("wall above the air's mean: {got:.3} against {exact:.3}, {:.2}% off", err * 100.0);
    assert!(err <= 0.03, "{got} against {exact}");
}
/// A log-law wind blowing over a bare floor must keep its profile from inlet to outlet: if it
/// doesn't, wind studies drift before the wind ever reaches the buildings.
#[test]
#[ignore = "needs a GPU"]
fn sim_comfort_empty_domain_log_law() {
    use sim_core::lbm::{FLUID, INLET, OUTLET, ROUGH_LAYER, SLIP_FLOOR};
    let (nx, ny, nz) = (240u32, 8u32, 80u32);
    let at = |x: u32, y: u32, z: u32| (x + nx * (y + ny * z)) as usize;
    let mut flags = vec![0u32; (nx * ny * nz) as usize];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                flags[at(x, y, z)] = if z == 0 {
                    SLIP_FLOOR
                } else if x == 0 || z == nz - 1 {
                    INLET
                } else if x == nx - 1 {
                    OUTLET
                } else if z == 1 {
                    FLUID | ROUGH_LAYER
                } else {
                    FLUID
                };
            }
        }
    }
    let (u_ref, z0, z_ref) = (0.05f32, 0.1f32, 60.0f32);
    let mut s = setup([nx, ny, nz], 1e-4);
    s.smagorinsky = 0.12;
    s.inlet_velocity = [u_ref, 0.0, 0.0];
    s.inlet_z0 = z0;
    s.inlet_reference_height = z_ref;
    s.rough = (0.41 / ((0.5 + z0) / z0).ln()).powi(2);
    s.mixing_max = 8.0;
    let mut lbm = Lbm::new(&s, &flags).unwrap();
    let cells = lbm.cells() as usize;
    let u0: Vec<[f32; 3]> = (0..cells).map(|i| {
        let z = ((i / (nx * ny) as usize) as f32 - 0.5).max(0.0);
        [u_ref * ((z + z0) / z0).ln() / ((z_ref + z0) / z0).ln(), 0.0, 0.0]
    }).collect();
    lbm.initialise(&vec![0.0; cells], &u0).unwrap();
    lbm.run((4.0 * f64::from(nx) / f64::from(u_ref)) as u64, &Cancel::default(), |_| {}).unwrap();
    let m = lbm.read_macros().unwrap();
    let mean_at = |x: u32, z: u32| (0..ny).map(|y| f64::from(m.ux[at(x, y, z)])).sum::<f64>() / f64::from(ny);
    let (mut worst, mut at_z) = (0.0f64, 0);
    for z in [3u32, 6, 12, 25, 50] {
        let exact = f64::from(u_ref) * f64::from((z as f32 - 0.5 + z0) / z0).ln() / f64::from((z_ref + z0) / z0).ln();
        let got = mean_at(nx * 3 / 4, z);
        let e = (got - exact).abs() / exact;
        println!("z {z}: {got:.5} against {exact:.5}, {:.1}% off", e * 100.0);
        if e > worst {
            worst = e;
            at_z = z;
        }
    }
    println!("worst {:.1}% at height {at_z}", worst * 100.0);
    // Measured: 6.9% at 2.5 cells above the floor, 3.4% at 5.5, under 2% above that. Results within
    // a couple of cells of the ground are less sure, which is one reason comfort studies stay Beta.
    let near = (mean_at(nx * 3 / 4, 3) / (f64::from(u_ref) * f64::from((2.5 + z0) / z0).ln() / f64::from((z_ref + z0) / z0).ln()) - 1.0).abs();
    assert!(near <= 0.10, "{:.1}% at 2.5 cells above the floor", near * 100.0);
    for z in [6u32, 12, 25, 50] {
        let exact = f64::from(u_ref) * f64::from((z as f32 - 0.5 + z0) / z0).ln() / f64::from((z_ref + z0) / z0).ln();
        assert!((mean_at(nx * 3 / 4, z) - exact).abs() / exact <= 0.05, "height {z}");
    }
}
/// The collision used at high Reynolds number must survive what a real run throws at it: an
/// impulsive start past a solid block at a very low lattice viscosity.
#[test]
#[ignore = "needs a GPU"]
fn sim_lbm_stays_stable_at_low_viscosity() {
    use sim_core::lbm::{INLET, OUTLET};
    let (nx, ny, nz) = (160u32, 64u32, 64u32);
    let at = |x: u32, y: u32, z: u32| (x + nx * (y + ny * z)) as usize;
    let mut flags = vec![0u32; (nx * ny * nz) as usize];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                flags[at(x, y, z)] = if x == 0 {
                    INLET
                } else if x == nx - 1 {
                    OUTLET
                } else if (40..56).contains(&x) && (24..40).contains(&y) && (24..40).contains(&z) {
                    WALL
                } else {
                    0
                };
            }
        }
    }
    let mut s = setup([nx, ny, nz], 5e-4);
    s.smagorinsky = 0.16;
    s.inlet_velocity = [0.05, 0.0, 0.0];
    let mut lbm = Lbm::new(&s, &flags).unwrap();
    let n = lbm.cells() as usize;
    let u0: Vec<[f32; 3]> = flags.iter().map(|f| if *f == WALL { [0.0; 3] } else { [0.05, 0.0, 0.0] }).collect();
    lbm.initialise(&vec![0.0; n], &u0).unwrap();
    for pass in 1..=4 {
        lbm.run(4000, &Cancel::default(), |_| {}).unwrap();
        let m = lbm.read_macros().unwrap();
        let top = m.ux.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let bad = m.ux.iter().filter(|v| !v.is_finite()).count();
        println!("after {} steps: fastest {top:.4}, non-finite {bad}", pass * 4000);
        assert_eq!(bad, 0, "the solution blew up");
        assert!(top < 0.3, "lattice speed {top} has left the valid range");
    }
}