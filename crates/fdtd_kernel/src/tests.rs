//! Canonical answers, each with a known exact value.

use num_complex::Complex64 as C;

use crate::assemble::assemble;
use crate::engine::{Engine, Stop, dft, stable_dt, waves};
use crate::grid::{Grid, even};
use crate::problem::*;
use crate::C0;

/// The strongest frequency in a ringing trace between two limits, by a fine search of its spectrum.
fn peak_between(trace: &[f64], dt: f64, f_lo: f64, f_hi: f64) -> f64 {
    // An impulse leaves a static field behind, and a window's sidelobes of that would swamp the
    // mode: take the mean out and taper the ends.
    let mean = trace.iter().sum::<f64>() / trace.len() as f64;
    let m = trace.len() as f64;
    let tapered: Vec<f64> = trace.iter().enumerate().map(|(k, v)| (v - mean) * (std::f64::consts::PI * k as f64 / m).sin().powi(2)).collect();
    let n = 2001;
    let spectrum: Vec<(f64, f64)> = (0..n)
        .map(|k| {
            let f = f_lo + (f_hi - f_lo) * k as f64 / (n - 1) as f64;
            (f, dft(&tapered, dt, 0.0, f).norm())
        })
        .collect();
    let k = (1..n - 1).max_by(|a, b| spectrum[*a].1.total_cmp(&spectrum[*b].1)).unwrap();
    // A parabola through the three samples around the peak, on the log scale.
    let (a, b, c) = (spectrum[k - 1].1.ln(), spectrum[k].1.ln(), spectrum[k + 1].1.ln());
    let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
    spectrum[k].0 + shift * (spectrum[k + 1].0 - spectrum[k].0)
}

/// Lines at spacing `h` symmetric about zero, with lines at `±h/2` and none at zero: what a
/// one-cell port gap at the origin needs.
fn lines_about_gap(half: f64, h: f64) -> Vec<f64> {
    let n = (half / h).round() as i64;
    (-n..n).map(|k| (k as f64 + 0.5) * h).collect()
}

/// The time step of an empty uniform grid is the textbook Courant limit.
#[test]
fn the_measured_time_step_is_the_courant_limit() {
    let h = 1e-3;
    let g = Grid::new([even(0.0, 0.03, 31), even(0.0, 0.03, 31), even(0.0, 0.03, 31)]);
    let mut p = Problem::new(g);
    p.walls = [Wall::Pec; 6];
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let courant = h / (C0 * 3f64.sqrt());
    println!("dt {dt:e}, Courant {courant:e}, ratio {:.3}", dt / courant);
    // 0.95 is the margin; a PEC box's top mode is just under the free grid's.
    assert!(dt <= courant * 1.02 && dt >= courant * 0.9, "{}", dt / courant);
}

/// A rectangular metal cavity: the TE101 mode's frequency is known exactly, and the error of a
/// Yee grid falls as the square of the cell. Exercises both updates and the metal walls.
#[test]
fn a_cavity_rings_at_its_exact_frequency() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let exact = C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    let mut errors = Vec::new();
    for cells in [15usize, 30] {
        let h = a / cells as f64;
        let ny = (b / h).round() as usize + 1;
        let nz = (d / h).round() as usize + 1;
        let g = Grid::new([even(0.0, a, cells + 1), even(0.0, b, ny), even(0.0, d, nz)]);
        let mut p = Problem::new(g.clone());
        p.walls = [Wall::Pec; 6];
        let asm = assemble(&p);
        let dt = stable_dt(&asm);
        let mut eng = Engine::<f64>::new(&asm, dt, 8);
        // Start from something with every mode in it, off every symmetry plane.
        let at = g.idx(cells / 3, ny / 2, nz / 3);
        eng.e[1][at] = 1.0;
        let steps = (40.0 / (exact * dt)) as u64;
        let mut trace = Vec::with_capacity(steps as usize);
        for _ in 0..steps {
            eng.advance();
            trace.push(eng.e[1][at]);
        }
        let f = peak_between(&trace, dt, exact * 0.9, exact * 1.1);
        let err = f / exact - 1.0;
        println!("{cells} cells across: {:.4} GHz against {:.4}, {:+.3} %", f / 1e9, exact / 1e9, err * 100.0);
        errors.push(err.abs());
    }
    assert!(errors[1] < 0.003, "{errors:?}");
    assert!(errors[0] / errors[1] > 2.5, "second order means four times better: {errors:?}");
}

/// A pulse from a point source, read just inside the absorber, against the same run in a domain so
/// big that nothing can come back during the comparison. The difference is the reflection.
#[test]
fn the_absorber_reflects_almost_nothing() {
    let h = 1e-3;
    let pml = 8usize;
    let source = |eng: &mut Engine<f32>, at: usize, t: f64, pulse: &Pulse| eng.e[2][at] += pulse.at(t) as f32;
    let run = |half_cells: usize, steps: u64, dt: Option<f64>| -> (Vec<f64>, f64) {
        let side = half_cells as f64 * h;
        let n = 2 * half_cells + 1;
        let g = Grid::new([even(-side, side, n), even(-side, side, n), even(-side, side, n)]);
        let p = Problem::new(g.clone());
        let asm = assemble(&p);
        let dt = dt.unwrap_or_else(|| stable_dt(&asm));
        let mut eng = Engine::<f32>::new(&asm, dt, 16);
        let pulse = Pulse { f0: 15e9, fc: 10e9 };
        let c = half_cells;
        let at = g.idx(c, c, c);
        // Just inside the small domain's absorber, off the axes where the field has nulls.
        let probe_at = 16 - pml - 2;
        let probe = g.idx(c + probe_at, c + 2, c + 2);
        let mut trace = Vec::with_capacity(steps as usize);
        for s in 0..steps {
            source(&mut eng, at, s as f64 * dt, &pulse);
            eng.advance();
            trace.push(eng.e[2][probe].f());
        }
        (trace, dt)
    };
    use crate::real::Real;
    // The big domain's reflection has 2·50 mm to travel, far past the end of this run.
    let steps = 500;
    let (near, dt) = run(16, steps, None);
    let (far, _) = run(90, steps, Some(dt));
    let peak = far.iter().map(|v| v.abs()).fold(0.0, f64::max);
    let worst = near.iter().zip(&far).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    let db = 20.0 * (worst / peak).log10();
    println!("peak {peak:e}, worst difference {worst:e}: {db:.1} dB");
    assert!(db < -40.0, "{db}");
}

/// A half-wave dipole of thin wire in free space: its impedance is a textbook value. The first
/// full test of absorber, port and subcell wire together.
#[test]
fn a_thin_wire_dipole_has_a_textbook_impedance() {
    let lambda = C0 / 1e9;
    let (h, half, r) = (5e-3, 0.2, 0.5e-3);
    let lines = lines_about_gap(half, h);
    let g = Grid::new([even(-half, half, (2.0 * half / h) as usize + 1), even(-half, half, (2.0 * half / h) as usize + 1), lines]);
    let mut p = Problem::new(g);
    let gap = h;
    let arm = ((0.48 * lambda - gap) / 2.0 / h).round() * h;
    p.wires.push(Wire { pts: vec![[0.0, 0.0, gap / 2.0], [0.0, 0.0, gap / 2.0 + arm]], r, surface: None });
    p.wires.push(Wire { pts: vec![[0.0, 0.0, -gap / 2.0], [0.0, 0.0, -gap / 2.0 - arm]], r, surface: None });
    p.ports.push(Port { a: [0.0, 0.0, -gap / 2.0], b: [0.0, 0.0, gap / 2.0], axis: 2, width_axis: None, width: 0.0, second: None, z0: 73.0 });
    let asm = assemble(&p);
    assert!(asm.warnings.is_empty(), "{:?}", asm.warnings);
    let dt = stable_dt(&asm);
    let cells = asm.grid.cells();
    let mut eng = Engine::<f32>::new(&asm, dt, 16);
    eng.drive(0, Pulse { f0: 1e9, fc: 0.5e9 });
    let t0 = std::time::Instant::now();
    let stop = eng.run(40.0, 200_000, |_, _| true);
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(stop, Stop::Decayed);
    let total = (arm * 2.0 + gap) / lambda;
    let rate = cells as f64 * eng.step as f64 / secs / 1e6;
    println!("{cells} cells, dt {dt:e}, {} steps, {secs:.1} s, {rate:.0} MC/s, length {total:.3} λ at 1 GHz", eng.step);
    let mut crossing = None;
    let mut last: Option<(f64, C)> = None;
    for f_mhz in (800..=1200).step_by(10) {
        let f = f_mhz as f64 * 1e6;
        let w = waves(&eng.traces[0], dt, f);
        let z = w.u / w.i;
        if let Some((fp, zp)) = last
            && zp.im < 0.0
            && z.im >= 0.0
        {
            crossing = Some((fp + (f - fp) * (-zp.im) / (z.im - zp.im), z.re));
        }
        last = Some((f, z));
        if f_mhz % 50 == 0 {
            println!("{f_mhz} MHz: Z = {:.1} {:+.1}j", z.re, z.im);
        }
    }
    let (f_res, r_res) = crossing.expect("the dipole crosses resonance in the sweep");
    println!("resonance {:.1} MHz, R {:.1} ohm, length there {:.3} λ", f_res / 1e6, r_res, (arm * 2.0 + gap) * f_res / C0);
    // A thin half-wave dipole resonates at about 0.47 to 0.49 λ, with R near 65 to 73 Ω.
    let electrical = (arm * 2.0 + gap) * f_res / C0;
    assert!((0.45..0.50).contains(&electrical), "{electrical}");
    assert!((55.0..80.0).contains(&r_res), "{r_res}");
}

/// A small wire loop reads as an inductor. Its inductance has a textbook value to compare.
#[test]
fn a_small_loop_is_an_inductor_of_the_right_size() {
    let h = 1e-3;
    let lines = even(-0.05, 0.05, 101);
    let z = lines_about_gap(0.05, h);
    let g = Grid::new([lines.clone(), lines, z]);
    let mut p = Problem::new(g);
    let (top, bottom, width, r) = (0.0105, -0.0105, 0.02, 0.2e-3);
    let gap = h / 2.0;
    p.wires.push(Wire { pts: vec![[0.0, 0.0, gap], [0.0, 0.0, top], [width, 0.0, top], [width, 0.0, bottom], [0.0, 0.0, bottom], [0.0, 0.0, -gap]], r, surface: None });
    p.ports.push(Port { a: [0.0, 0.0, -gap], b: [0.0, 0.0, gap], axis: 2, width_axis: None, width: 0.0, second: None, z0: 50.0 });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f32>::new(&asm, dt, 16);
    eng.drive(0, Pulse { f0: 0.6e9, fc: 0.5e9 });
    let stop = eng.run(40.0, 300_000, |_, _| true);
    assert_ne!(stop, Stop::Diverged);
    // Square-ish loop of round wire: L ≈ (μ0/π)·(a·ln(2a/r)... ) with sides a and b.
    let (a, b): (f64, f64) = (width, top - bottom);
    let d = (a * a + b * b).sqrt();
    let textbook = crate::MU0 / std::f64::consts::PI
        * (a * (2.0 * a / r).ln() + b * (2.0 * b / r).ln() + 2.0 * d - a * ((a + d) / b).asinh().min(((a + d) / b).ln()) - b * ((b + d) / a).ln() - 2.0 * (a + b) + (a + b) / 4.0);
    println!("textbook {:.2} nH", textbook * 1e9);
    for f in [0.2e9, 0.4e9] {
        let w = waves(&eng.traces[0], dt, f);
        let z = w.u / w.i;
        let l = z.im / (2.0 * std::f64::consts::PI * f);
        println!("{:.1} GHz: Z = {:.2} {:+.2}j, {:.2} nH", f / 1e9, z.re, z.im, l * 1e9);
        assert!(z.im > 0.0, "a small loop is an inductor: {z}");
        assert!(z.re.abs() < z.im.abs(), "{z}");
        assert!((l / textbook - 1.0).abs() < 0.2, "{} against {}", l * 1e9, textbook * 1e9);
    }
}

/// What the core does per cell on this machine. Not a pass or fail: load from other work moves it.
#[test]
#[ignore]
fn throughput() {
    for (cells, threads) in [(100usize, 4usize), (100, 16), (100, 32), (200, 16), (200, 32), (200, 56)] {
        let lines = even(0.0, cells as f64 * 1e-3, cells + 1);
        let g = Grid::new([lines.clone(), lines.clone(), lines]);
        let p = Problem::new(g);
        let asm = assemble(&p);
        let dt = 1.8e-12;
        let mut eng = Engine::<f32>::new(&asm, dt, threads);
        let steps = 200;
        let t0 = std::time::Instant::now();
        eng.advance_many(steps);
        let s = t0.elapsed().as_secs_f64();
        println!("{cells}^3 on {threads} threads: {:.0} MC/s", (cells * cells * cells) as f64 * steps as f64 / s / 1e6);
    }
}

/// The GPU engine against the CPU engine, number for number, on a model with an absorber, a port
/// and a sheet: `cargo test -p fdtd_kernel --features gpu --release gpu_matches -- --ignored --nocapture`
#[cfg(feature = "gpu")]
#[test]
#[ignore]
fn gpu_matches_the_cpu() {
    use crate::gpu::GpuEngine;
    let h = 1e-3;
    let x = even(-0.05, 0.05, 101);
    let z = lines_about_gap(0.05, h);
    let g = Grid::new([x.clone(), x, z]);
    let mut p = Problem::new(g);
    let w = 0.002;
    let square = |a: f64, b: f64| vec![[a, -w / 2.0], [b, -w / 2.0], [b, w / 2.0], [a, w / 2.0]];
    let _ = square;
    // Two arms of a flat dipole in the plane x = 0 along z, with real metal for the loss branch.
    let arm = |z0: f64, z1: f64| vec![[-w / 2.0, z0], [w / 2.0, z0], [w / 2.0, z1], [-w / 2.0, z1]];
    let surface = if std::env::var("NO_SURFACE").is_ok() { None } else { Some(Surface::sheet(3.5e7, 18e-6, 2e9)) };
    p.sheets.push(Sheet { axis: 0, at: 0.0, polys: vec![arm(h / 2.0, 0.035), arm(-0.035, -h / 2.0)], surface });
    p.ports.push(Port { a: [0.0, 0.0, -h / 2.0], b: [0.0, 0.0, h / 2.0], axis: 2, width_axis: Some(1), width: w, second: None, z0: 100.0 });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let pulse = Pulse { f0: 2e9, fc: 1e9 };
    let mut cpu = Engine::<f32>::new(&asm, dt, 16);
    cpu.drive(0, pulse);
    let mut gpu = GpuEngine::new(&asm, dt, 8).expect("a GPU");
    println!("GPU: {}, batch of steps", gpu.adapter_name);
    gpu.drive(0, pulse);
    let t0 = std::time::Instant::now();
    for _ in 0..50 {
        gpu.advance_batch().unwrap();
    }
    let secs = t0.elapsed().as_secs_f64();
    let steps = gpu.step;
    println!("{} cells, {steps} steps in {secs:.2} s: {:.0} MC/s", asm.grid.cells(), asm.grid.cells() as f64 * steps as f64 / secs / 1e6);
    let t0 = std::time::Instant::now();
    cpu.advance_many(steps);
    println!("CPU: {:.0} MC/s", asm.grid.cells() as f64 * steps as f64 / t0.elapsed().as_secs_f64() / 1e6);
    let (a, b) = (&cpu.traces[0], &gpu.traces[0]);
    assert_eq!(a.u.len(), b.u.len());
    for k in [0usize, 1, 2, 5, 10, 20, 50, 100, 200, 400, 800, 1600, 3199] {
        println!("step {k}: U cpu {:.6e} gpu {:.6e}; I cpu {:.6e} gpu {:.6e}", a.u[k], b.u[k], a.i[k], b.i[k]);
    }
    let peak = a.u.iter().map(|v| v.abs()).fold(0.0, f64::max);
    let worst = a.u.iter().zip(&b.u).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
    let peak_i = a.i.iter().map(|v| v.abs()).fold(0.0, f64::max);
    let worst_i = a.i.iter().zip(&b.i).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
    println!("voltage: peak {peak:e}, worst difference {worst:e}; current: peak {peak_i:e}, worst difference {worst_i:e}");
    let e_cpu = cpu.energy();
    let e_gpu = gpu.energy().unwrap();
    println!("energy: CPU {e_cpu:e}, GPU {e_gpu:e}");
    assert!(worst < peak * 2e-3, "{worst} of {peak}");
    assert!(worst_i < peak_i * 2e-3, "{worst_i} of {peak_i}");
    assert!((e_cpu / e_gpu - 1.0).abs() < 2e-2, "{e_cpu} {e_gpu}");
}

/// What the GPU does per cell at sizes where it matters, with the CPU beside it. Not a pass or
/// fail, since other work on the machine moves both:
/// `cargo test -p fdtd_kernel --features gpu --release gpu_throughput -- --ignored --nocapture`
#[cfg(feature = "gpu")]
#[test]
#[ignore]
fn gpu_throughput() {
    use crate::gpu::GpuEngine;
    for cells in [100usize, 200, 300, 400] {
        let lines = even(0.0, cells as f64 * 1e-3, cells + 1);
        let g = Grid::new([lines.clone(), lines.clone(), lines]);
        let mut p = Problem::new(g);
        p.ports.push(Port { a: [0.05, 0.05, 0.05], b: [0.05, 0.05, 0.051], axis: 2, width_axis: None, width: 0.0, second: None, z0: 50.0 });
        let asm = assemble(&p);
        let dt = 1.8e-12;
        let total = (cells * cells * cells) as f64;
        let bytes = total * 60.0 / 1e9;
        match GpuEngine::new(&asm, dt, 64) {
            Ok(mut gpu) => {
                gpu.drive(0, Pulse { f0: 5e9, fc: 3e9 });
                let batches = if cells <= 200 { 8 } else { 3 };
                let t0 = std::time::Instant::now();
                for _ in 0..batches {
                    gpu.advance_batch().unwrap();
                }
                let s = t0.elapsed().as_secs_f64();
                println!("{cells}^3 ({:.0} M cells, about {bytes:.1} GB) on {}: {:.0} MC/s", total / 1e6, gpu.adapter_name, total * gpu.step as f64 / s / 1e6);
            }
            Err(e) => println!("{cells}^3 ({:.0} M cells): no GPU run: {e:#}", total / 1e6),
        }
        if cells <= 300 {
            let mut cpu = Engine::<f32>::new(&asm, dt, 56);
            cpu.drive(0, Pulse { f0: 5e9, fc: 3e9 });
            let steps = 64;
            let t0 = std::time::Instant::now();
            cpu.advance_many(steps);
            println!("{cells}^3 on 56 CPU threads: {:.0} MC/s", total * steps as f64 / t0.elapsed().as_secs_f64() / 1e6);
        }
    }
}
