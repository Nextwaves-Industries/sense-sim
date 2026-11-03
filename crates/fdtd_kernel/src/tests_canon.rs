//! Exact answers for what the first tests leave out: curved metal, thin layers, material
//! dispersion, surface loss, numeric precision and the wave port.

use num_complex::Complex64 as C;

use crate::assemble::assemble;
use crate::engine::{Engine, Stop, dft, stable_dt, waves};
use crate::grid::{Grid, even};
use crate::problem::*;
use crate::static2d::lines_through;
use crate::waveport::{WavePortDef, WavePortRun, beta, waves_at};
use crate::{C0, EPS0};

fn tapered(trace: &[f64]) -> Vec<f64> {
    let mean = trace.iter().sum::<f64>() / trace.len() as f64;
    let m = trace.len() as f64;
    trace.iter().enumerate().map(|(k, v)| (v - mean) * (std::f64::consts::PI * k as f64 / m).sin().powi(2)).collect()
}

/// The strongest frequency of a ringing trace between two limits, on a taper.
fn peak(trace: &[f64], dt: f64, f_lo: f64, f_hi: f64) -> f64 {
    let t = tapered(trace);
    let n = 1501;
    let spectrum: Vec<(f64, f64)> = (0..n).map(|k| { let f = f_lo + (f_hi - f_lo) * k as f64 / (n - 1) as f64; (f, dft(&t, dt, 0.0, f).norm()) }).collect();
    let k = (1..n - 1).max_by(|a, b| spectrum[*a].1.total_cmp(&spectrum[*b].1)).unwrap();
    let (a, b, c) = (spectrum[k - 1].1.ln(), spectrum[k].1.ln(), spectrum[k + 1].1.ln());
    spectrum[k].0 + 0.5 * (a - c) / (a - 2.0 * b + c) * (spectrum[k + 1].0 - spectrum[k].0)
}

/// A hollow metal sphere has an exact lowest mode, TM011 at `ka = 2.7437`. There is no way to line
/// a grid up with a sphere, so this measures the conformal treatment of curved metal, on faces and
/// edges cut at every angle.
#[test]
#[ignore]
fn a_hollow_sphere_rings_at_its_exact_frequency() {
    let a = 0.05;
    let exact = 2.7437 * C0 / (2.0 * std::f64::consts::PI * a);
    let mut errors = Vec::new();
    for cells in [16usize, 24, 32] {
        let side = 1.1 * a;
        let n = (2.0 * side / (2.0 * a / cells as f64)).round() as usize + 1;
        let lines = even(-side, side, n);
        let g = Grid::new([lines.clone(), lines.clone(), lines]);
        let mut p = Problem::new(g.clone());
        p.walls = [Wall::Pec; 6];
        p.bodies.push(Body { solid: Solid::Outside(Box::new(Solid::Sphere { c: [0.0; 3], r: a })), mat: Mat::Pec });
        let asm = assemble(&p);
        let dt = stable_dt(&asm);
        let courant = (2.0 * side / (n - 1) as f64) / (C0 * 3f64.sqrt());
        let mut eng = Engine::<f64>::new(&asm, dt, 16);
        // On the axis, where TM011 has E along it, with a pulse narrow enough to ring only this mode.
        let c = n / 2;
        let at = g.idx(c, c, c);
        let pulse = Pulse { f0: exact, fc: 0.2 * exact };
        let source_steps = (2.0 * pulse.delay() / dt) as u64;
        let steps = source_steps + (60.0 / (exact * dt)) as u64;
        let mut trace = Vec::with_capacity(steps as usize);
        for s in 0..steps {
            if s < source_steps {
                eng.e[2][at] += pulse.at(s as f64 * dt) * 1e-3;
            }
            eng.advance();
            if s >= source_steps {
                trace.push(eng.e[2][at]);
            }
        }
        let f = peak(&trace, dt, exact * 0.85, exact * 1.15);
        let err = f / exact - 1.0;
        println!("{cells} cells across: {:.4} GHz against {:.4}, {:+.3} %, time step {:.2} of Courant", f / 1e9, exact / 1e9, err * 100.0, dt / courant);
        errors.push(err.abs());
    }
    // First order, not second: halving the cell halves this. The figures at 16, 24, 32 and 48 cells
    // across are -2.2, -1.5, -1.1 and -0.7 %, with no stray modes.
    assert!(errors[1] < 0.02, "{errors:?}");
    assert!(errors[2] < errors[1] && errors[1] < errors[0], "{errors:?}");
}

/// A layer far thinner than a cell, across the field: `TE101` of a box has no field variation along
/// `y`, so with a slab normal to `y` the displacement is uniform and the exact answer is a simple
/// series capacitance.
#[test]
fn a_thin_layer_across_the_field_is_exact() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let t: f64 = 0.3e-3;
    let er = 10.0;
    let f0 = C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    let eff = b / (b - t + t / er);
    let exact = f0 / eff.sqrt();
    let h = 1e-3;
    let g = Grid::new([even(0.0, a, 31), even(0.0, b, 21), even(0.0, d, 41)]);
    let mut p = Problem::new(g.clone());
    p.walls = [Wall::Pec; 6];
    // Well inside one cell: nothing lines up with it.
    p.bodies.push(Body { solid: Solid::Box { min: [0.0, 0.0103, 0.0], max: [a, 0.0103 + t, d] }, mat: Mat::Dielectric { eps_r: er, mu_r: 1.0, kappa: 0.0, debye: None } });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 8);
    let at = g.idx(10, 10, 13);
    eng.e[1][at] = 1.0;
    let steps = (80.0 / (exact * dt)) as u64;
    let mut trace = Vec::with_capacity(steps as usize);
    for _ in 0..steps {
        eng.advance();
        trace.push(eng.e[1][at]);
    }
    let f = peak(&trace, dt, exact * 0.9, exact * 1.1);
    println!("slab across the field: {:.4} GHz, exact {:.4} (empty {:.4}), {:+.3} %; layer is {:.2} of a cell", f / 1e9, exact / 1e9, f0 / 1e9, (f / exact - 1.0) * 100.0, t / h);
    let shift_exact = exact / f0 - 1.0;
    let shift_seen = f / f0 - 1.0;
    println!("shift {:.3} % of an exact {:.3} %", shift_seen * 100.0, shift_exact * 100.0);
    assert!((shift_seen / shift_exact - 1.0).abs() < 0.1, "{shift_seen} {shift_exact}");
}

/// The same layer along the field, with an exact answer from matching waves across it.
#[test]
fn a_thin_layer_along_the_field_is_exact() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let t: f64 = 0.3e-3;
    let er: f64 = 10.0;
    let z1 = 0.0213;
    let kx = std::f64::consts::PI / a;
    // Root of the transverse resonance: fields as sines from each wall, matched across the slab.
    let mismatch = |k: f64| -> f64 {
        let b1 = ((k * k - kx * kx).max(1e-9)).sqrt();
        let b2 = ((er * k * k - kx * kx).max(1e-9)).sqrt();
        // E and dE/dz from the wall at 0 up to the slab, through it, against the far wall.
        let (mut e, mut de) = ((b1 * z1).sin(), b1 * (b1 * z1).cos());
        let (c, s) = ((b2 * t).cos(), (b2 * t).sin());
        let (e2, de2) = (e * c + de * s / b2, -e * b2 * s + de * c);
        e = e2;
        de = de2;
        let rest = d - z1 - t;
        // Far wall: E(rest) = 0.
        e * (b1 * rest).cos() + de * (b1 * rest).sin() / b1
    };
    let k0 = ((std::f64::consts::PI / a).powi(2) + (std::f64::consts::PI / d).powi(2)).sqrt();
    let (mut lo, mut hi) = (k0 * 0.85, k0 * 1.0);
    for _ in 0..200 {
        let m = (lo + hi) * 0.5;
        if mismatch(lo) * mismatch(m) <= 0.0 {
            hi = m;
        } else {
            lo = m;
        }
    }
    let exact = (lo + hi) * 0.5 * C0 / (2.0 * std::f64::consts::PI);
    let f0 = k0 * C0 / (2.0 * std::f64::consts::PI);
    let g = Grid::new([even(0.0, a, 31), even(0.0, b, 21), even(0.0, d, 41)]);
    let mut p = Problem::new(g.clone());
    p.walls = [Wall::Pec; 6];
    p.bodies.push(Body { solid: Solid::Box { min: [0.0, 0.0, z1], max: [a, b, z1 + t] }, mat: Mat::Dielectric { eps_r: er, mu_r: 1.0, kappa: 0.0, debye: None } });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 8);
    let at = g.idx(10, 10, 13);
    eng.e[1][at] = 1.0;
    let steps = (80.0 / (exact * dt)) as u64;
    let mut trace = Vec::with_capacity(steps as usize);
    for _ in 0..steps {
        eng.advance();
        trace.push(eng.e[1][at]);
    }
    let f = peak(&trace, dt, exact * 0.9, exact * 1.1);
    let (shift_exact, shift_seen) = (exact / f0 - 1.0, f / f0 - 1.0);
    println!("slab along the field: {:.4} GHz, exact {:.4}, shift {:.3} % of an exact {:.3} %", f / 1e9, exact / 1e9, shift_seen * 100.0, shift_exact * 100.0);
    assert!((shift_seen / shift_exact - 1.0).abs() < 0.1, "{shift_seen} {shift_exact}");
}

/// Fits `s[n+3] = c0·s[n] + c1·s[n+1] + c2·s[n+2]` to a trace sampled about twelve times per period
/// and returns the frequency and decay of the oscillating root. With the box started in one mode's
/// exact shape nothing else rings, and a spectrum's peak would be biased by the decay itself.
fn damped_tone(trace: &[f64], dt: f64, f_guess: f64) -> (f64, f64) {
    let every = ((1.0 / (12.0 * f_guess * dt)).round() as usize).max(1);
    let s: Vec<f64> = trace.iter().step_by(every).copied().collect();
    let d = every as f64 * dt;
    let mut ata = [[0.0f64; 3]; 3];
    let mut atb = [0.0f64; 3];
    for w in s.windows(4) {
        for r in 0..3 {
            for c in 0..3 {
                ata[r][c] += w[r] * w[c];
            }
            atb[r] += w[r] * w[3];
        }
    }
    // Normal equations by elimination; small and well scaled.
    for p in 0..3 {
        let piv = (p..3).max_by(|x, y| ata[*x][p].abs().total_cmp(&ata[*y][p].abs())).unwrap();
        ata.swap(p, piv);
        atb.swap(p, piv);
        for r in p + 1..3 {
            let m = ata[r][p] / ata[p][p];
            for c in p..3 {
                ata[r][c] -= m * ata[p][c];
            }
            atb[r] -= m * atb[p];
        }
    }
    let mut x = [0.0f64; 3];
    for r in (0..3).rev() {
        let mut v = atb[r];
        for c in r + 1..3 {
            v -= ata[r][c] * x[c];
        }
        x[r] = v / ata[r][r];
    }
    // Roots of z³ - c2·z² - c1·z - c0 by Durand-Kerner.
    let poly = |z: C| z * z * z - z * z * x[2] - z * x[1] - x[0];
    let mut roots = [C::new(0.4, 0.9), C::new(0.4, 0.9).powi(2), C::new(0.4, 0.9).powi(3)];
    for _ in 0..200 {
        for k in 0..3 {
            let mut den = C::new(1.0, 0.0);
            for m in 0..3 {
                if m != k {
                    den *= roots[k] - roots[m];
                }
            }
            roots[k] -= poly(roots[k]) / den;
        }
    }
    let osc = roots.iter().max_by(|a, b| a.im.total_cmp(&b.im)).unwrap();
    (osc.arg() / (2.0 * std::f64::consts::PI * d), -osc.norm().ln() / d)
}

/// Sets every `Ey` of a box to the TE101 mode's shape.
fn start_in_te101<R: crate::real::Real>(eng: &mut Engine<R>, grid: &Grid, a: f64, d: f64) {
    let [nx, ny, nz] = grid.n();
    for k in 0..nz {
        for j in 0..ny - 1 {
            for i in 0..nx {
                let v = (std::f64::consts::PI * grid.lines[0][i] / a).sin() * (std::f64::consts::PI * grid.lines[2][k] / d).sin();
                eng.e[1][grid.idx(i, j, k)] = R::of(v);
            }
        }
    }
}

/// A box filled with a Debye material: `ω²ε(ω) = ω0²` has an exact complex root, whose real part
/// is the mode's frequency and imaginary part its decay.
#[test]
fn a_debye_material_has_the_exact_complex_resonance() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let (e_inf, delta, tau) = (2.0, 3.0, 40e-12);
    let w0 = 2.0 * std::f64::consts::PI * C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    // Newton on g(ω) = ω²·(ε∞ + Δ/(1 + jωτ)) - ω0², from the high-frequency value.
    let g = |w: C| w * w * (C::new(e_inf, 0.0) + C::new(delta, 0.0) / (C::new(1.0, 0.0) + C::new(0.0, tau) * w)) - C::new(w0 * w0, 0.0);
    let mut w = C::new(w0 / e_inf.sqrt(), w0 * 0.02);
    for _ in 0..200 {
        let h = w.norm() * 1e-7;
        let gp = (g(w + h) - g(w - h)) / (2.0 * h);
        w -= g(w) / gp;
    }
    let (f_exact, rate_exact) = (w.re / (2.0 * std::f64::consts::PI), w.im);
    let grid = Grid::new([even(0.0, a, 31), even(0.0, b, 21), even(0.0, d, 41)]);
    let mut p = Problem::new(grid.clone());
    p.walls = [Wall::Pec; 6];
    p.bodies.push(Body { solid: Solid::Box { min: [0.0; 3], max: [a, b, d] }, mat: Mat::Dielectric { eps_r: e_inf, mu_r: 1.0, kappa: 0.0, debye: Some(Debye { delta_eps: delta, tau }) } });
    let asm = assemble(&p);
    assert!(!asm.debye.is_empty());
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 8);
    start_in_te101(&mut eng, &grid, a, d);
    let at = grid.idx(15, 10, 20);
    let steps = (6.0 / (f_exact * dt)) as u64;
    let mut trace = Vec::with_capacity(steps as usize);
    for _ in 0..steps {
        eng.advance();
        trace.push(eng.e[1][at]);
    }
    let (f, rate) = damped_tone(&trace, dt, f_exact);
    println!("Debye: {:.4} GHz, exact {:.4}, {:+.2} %; decay {:.3e}/s, exact {:.3e}/s, {:+.1} %", f / 1e9, f_exact / 1e9, (f / f_exact - 1.0) * 100.0, rate, rate_exact, (rate / rate_exact - 1.0) * 100.0);
    assert!((f / f_exact - 1.0).abs() < 0.01, "{f} {f_exact}");
    assert!((rate / rate_exact - 1.0).abs() < 0.03, "{rate} {rate_exact}");
}

/// A box filled with a conducting dielectric: the same exercise for plain loss.
#[test]
fn a_lossy_dielectric_decays_at_the_exact_rate() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let (er, kappa) = (4.0, 0.05);
    let w0 = 2.0 * std::f64::consts::PI * C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    // Decay σ/(2ε), frequency reduced by the usual damped-oscillator root.
    let rate_exact = kappa / (2.0 * EPS0 * er);
    let f_exact = ((w0 * w0 / er - rate_exact * rate_exact).sqrt()) / (2.0 * std::f64::consts::PI);
    let grid = Grid::new([even(0.0, a, 31), even(0.0, b, 21), even(0.0, d, 41)]);
    let mut p = Problem::new(grid.clone());
    p.walls = [Wall::Pec; 6];
    p.bodies.push(Body { solid: Solid::Box { min: [0.0; 3], max: [a, b, d] }, mat: Mat::Dielectric { eps_r: er, mu_r: 1.0, kappa, debye: None } });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 8);
    start_in_te101(&mut eng, &grid, a, d);
    let at = grid.idx(15, 10, 20);
    let steps = (10.0 / (f_exact * dt)) as u64;
    let mut trace = Vec::with_capacity(steps as usize);
    for _ in 0..steps {
        eng.advance();
        trace.push(eng.e[1][at]);
    }
    let (f, rate) = damped_tone(&trace, dt, f_exact);
    println!("loss: {:.4} GHz, exact {:.4}, {:+.3} %; decay {:.3e}/s, exact {:.3e}/s, {:+.2} %", f / 1e9, f_exact / 1e9, (f / f_exact - 1.0) * 100.0, rate, rate_exact, (rate / rate_exact - 1.0) * 100.0);
    assert!((f / f_exact - 1.0).abs() < 0.005);
    assert!((rate / rate_exact - 1.0).abs() < 0.03, "{rate} {rate_exact}");
}

/// A long ring-down in single precision against double: rounding has to stay far below anything a
/// run reports. On a high-Q cavity with an energy check every so often, the way a run works.
#[test]
#[ignore]
fn single_precision_holds_a_long_ring_down() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let exact = C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    let grid = Grid::new([even(0.0, a, 31), even(0.0, b, 21), even(0.0, d, 41)]);
    let mut p = Problem::new(grid.clone());
    p.walls = [Wall::Pec; 6];
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let at = grid.idx(10, 10, 13);
    let steps = 400_000u64;
    let run_f32 = || {
        let mut e = Engine::<f32>::new(&asm, dt, 8);
        e.e[1][at] = 1.0;
        let mut t = Vec::with_capacity(steps as usize);
        for _ in 0..steps {
            e.advance();
            t.push(e.e[1][at] as f64);
        }
        (t, e.energy())
    };
    let run_f64 = || {
        let mut e = Engine::<f64>::new(&asm, dt, 8);
        e.e[1][at] = 1.0;
        let mut t = Vec::with_capacity(steps as usize);
        for _ in 0..steps {
            e.advance();
            t.push(e.e[1][at]);
        }
        (t, e.energy())
    };
    let (t32, w32) = run_f32();
    let (t64, w64) = run_f64();
    let (f32_, f64_) = (peak(&t32, dt, exact * 0.98, exact * 1.02), peak(&t64, dt, exact * 0.98, exact * 1.02));
    let tail: usize = t32.len() - 2000;
    let worst = t32[tail..].iter().zip(&t64[tail..]).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
    let peak_v = t64.iter().map(|v| v.abs()).fold(0.0, f64::max);
    println!("{steps} steps: f32 {:.6} GHz, f64 {:.6} GHz, {:.2e} apart; energy {:.6e} against {:.6e}; worst field difference at the end {:.2e} of the peak", f32_ / 1e9, f64_ / 1e9, (f32_ / f64_ - 1.0).abs(), w32, w64, worst / peak_v);
    assert!((f32_ / f64_ - 1.0).abs() < 1e-4);
    assert!((w32 / w64 - 1.0).abs() < 0.02, "{w32} {w64}");
}

/// A strip with real metal as a one-turn loop at low frequency: its resistance is the sheet's
/// resistance times its squares, with the current crowding to the edges only adding to it.
#[test]
#[ignore]
fn a_resistive_loop_reads_its_squares() {
    let h = 1e-3;
    let w = 4e-3;
    let lines = |half: f64| lines_through(-half, half, h, h / 2.0);
    let g = Grid::new([lines(0.05), lines(0.05), even(-0.03, 0.03, 61)]);
    let mut p = Problem::new(g);
    // A square ring in the plane z = 0 with a gap on one side: sides of 40 mm on the centre line.
    let side = 0.02;
    let (o, i) = (side + w / 2.0, side - w / 2.0);
    let gap = h;
    let ring = vec![[gap / 2.0, -o], [o, -o], [o, o], [-o, o], [-o, -o], [-gap / 2.0, -o], [-gap / 2.0, -i], [-i, -i], [-i, i], [i, i], [i, -i], [gap / 2.0, -i]];
    let rs = 0.5; // ohms per square, thin enough to be far below the skin depth
    let surface = Some(Surface { r: rs, l: 0.0 });
    p.sheets.push(Sheet { axis: 2, at: 0.0, polys: vec![ring], surface });
    p.ports.push(Port { a: [-gap / 2.0, -side, 0.0], b: [gap / 2.0, -side, 0.0], axis: 0, width_axis: Some(1), width: w, second: None, z0: 50.0 });
    let asm = assemble(&p);
    println!("warnings {:?}, {} loss edges", asm.warnings, asm.rl.len());
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f32>::new(&asm, dt, 16);
    eng.drive(0, Pulse { f0: 0.25e9, fc: 0.2e9 });
    let stop = eng.run(40.0, 400_000, |_, _| true);
    assert_ne!(stop, Stop::Diverged);
    // Squares along the centre line; corners count about 0.56 of a square each, not 1.
    let straight = 4.0 * (2.0 * side - w);
    let squares = straight / w + 4.0 * 0.56;
    let expect = rs * squares;
    for f in [0.05e9, 0.1e9, 0.2e9] {
        let wv = waves(&eng.traces[0], dt, f);
        let z = wv.u / wv.i;
        println!("{:.0} MHz: Z {:.2} {:+.2}j, sheet resistance times squares {expect:.2}", f / 1e6, z.re, z.im);
    }
    let wv = waves(&eng.traces[0], dt, 0.1e9);
    let z = wv.u / wv.i;
    assert!((z.re / expect - 1.0).abs() < 0.25, "{} against {expect}", z.re);
}

/// The wave port on a real line: a microstrip through a box. A straight line passes everything and
/// reflects nothing, its phase is known from its length, and its impedance has a closed form.
#[test]
#[ignore]
fn a_wave_port_measures_a_through_line() {
    let (h, w, er) = (1.6e-3, 3.0e-3, 4.4);
    let cell = 0.4e-3;
    let y = lines_through(-0.016, 0.016, cell, cell / 2.0);
    let z = {
        // Lines at the ground and the strip, and a few cells through the substrate.
        let mut v = even(0.0, h, 5);
        let mut at = h;
        let mut step = cell;
        while at < 0.016 {
            step = (step * 1.15).min(1.2e-3);
            at += step;
            v.push(at);
        }
        v
    };
    let x = even(-0.04, 0.04, 161);
    let grid = Grid::new([x, y, z]);
    let mut p = Problem::new(grid.clone());
    p.walls = [Wall::Pml(10), Wall::Pml(10), Wall::Pml(8), Wall::Pml(8), Wall::Pec, Wall::Pml(8)];
    p.bodies.push(Body { solid: Solid::Box { min: [-0.05, -0.05, 0.0], max: [0.05, 0.05, h] }, mat: Mat::Dielectric { eps_r: er, mu_r: 1.0, kappa: 0.0, debye: None } });
    let strip = vec![[-w / 2.0, -0.05], [w / 2.0, -0.05], [w / 2.0, 0.05], [-w / 2.0, 0.05]];
    // Sheet normal to z, outline in (x, y): swap so the strip runs along x.
    let strip: Vec<[f64; 2]> = strip.iter().map(|q| [q[1], q[0]]).collect();
    p.sheets.push(Sheet { axis: 2, at: h, polys: vec![strip], surface: None });
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let (ny, nz) = (grid.n()[1], grid.n()[2]);
    let j_mid = grid.nearest(1, 0.0);
    let k_strip = grid.nearest(2, h);
    let (j0, j1) = (grid.nearest(1, -0.012), grid.nearest(1, 0.012));
    let k1 = grid.nearest(2, 0.011);
    let _ = (ny, nz);
    let make = |plane: usize, direction: f64| WavePortDef { axis: 0, plane, r1: (j0, j1), r2: (0, k1), direction, signal: (j_mid, k_strip), ground: None };
    let (x1, x2) = (grid.nearest(0, -0.025), grid.nearest(0, 0.025));
    let port1 = WavePortRun::new(&asm, make(x1, 1.0)).expect("a mode at port 1");
    let port2 = WavePortRun::new(&asm, make(x2, -1.0)).expect("a mode at port 2");
    // Hammerstad and Jensen for zero thickness.
    let u = w / h;
    let aa = 1.0 + (1.0 / 49.0) * ((u.powi(4) + (u / 52.0).powi(2)) / (u.powi(4) + 0.432)).ln() + (1.0 / 18.7) * (1.0 + (u / 18.1).powi(3)).ln();
    let bb = 0.564 * ((er - 0.9) / (er + 3.0)).powf(0.053);
    let eps_eff: f64 = (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * (1.0 + 10.0 / u).powf(-aa * bb);
    let ff = 6.0 + (2.0 * std::f64::consts::PI - 6.0) * (-(30.666 / u).powf(0.7528)).exp();
    let z_hj = 60.0 / eps_eff.sqrt() * (ff / u + (1.0 + (2.0 / u).powi(2)).sqrt()).ln();
    println!("mode: Z0 {:.2} ohm (closed form {z_hj:.2}), eps_eff {:.3} (closed form {eps_eff:.3})", port1.mode.z0, port1.mode.eps_eff);
    let z0 = port1.mode.z0;
    let ee = port1.mode.eps_eff;
    let mode = port1.mode.clone();
    let mut eng = Engine::<f32>::new(&asm, dt, 24);
    eng.add_wave_port(port1);
    eng.add_wave_port(port2);
    eng.drive_wave(0, Pulse { f0: 3e9, fc: 2e9 });
    let stop = eng.run(40.0, 100_000, |_, _| true);
    assert_ne!(stop, Stop::Diverged);
    let length = grid.lines[0][x2] - grid.lines[0][x1] - eng.wave_ports[0].read_distance(&grid) - eng.wave_ports[1].read_distance(&grid);
    for f in [1.5e9, 3e9, 4.5e9] {
        let (a1, b1) = waves_at(&eng.wave_ports[0], dt, f);
        let (_a2, b2) = waves_at(&eng.wave_ports[1], dt, f);
        // Port 2's forward direction is into the model, so the wave arriving there is its `b`.
        let s11 = b1 / a1;
        let s21 = b2 / a1;
        let phase = s21.arg();
        let want = -beta(&mode, f) * length;
        let wrap = |p: f64| (p + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI) - std::f64::consts::PI;
        println!("{:.1} GHz: |S11| {:.1} dB, |S21| {:.3} dB, phase of S21 {:+.1} deg, expected {:+.1}", f / 1e9, 20.0 * s11.norm().log10(), 20.0 * s21.norm().log10(), phase.to_degrees(), wrap(want).to_degrees());
        assert!(s11.norm() < 0.08, "S11 {}", s11.norm());
        assert!((s21.norm() - 1.0).abs() < 0.03, "S21 {}", s21.norm());
        // Against the static line: a real microstrip slows a little with frequency, so this grows.
        assert!(wrap(phase - want).abs() < 0.03 * want.abs(), "{phase} against {want}");
    }
    assert!((z0 / z_hj - 1.0).abs() < 0.05, "{z0} {z_hj}");
    assert!((ee / eps_eff - 1.0).abs() < 0.05, "{ee} {eps_eff}");
}

/// A box cavity whose walls fall between mesh lines: flat metal, off the grid, exact answer. What
/// separates a fault in the conformal cells from anything to do with a sphere.
#[test]
fn walls_between_mesh_lines_give_the_exact_frequency() {
    let (a, b, d): (f64, f64, f64) = (0.0301, 0.0203, 0.0407);
    let exact = C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    let mut errors = Vec::new();
    for h in [2e-3, 1e-3, 0.5e-3] {
        let n = |len: f64| (len / h).ceil() as usize + 3;
        let lines = |len: f64| -> Vec<f64> { (0..n(len)).map(|k| (k as f64 - 1.0) * h).collect() };
        let g = Grid::new([lines(a), lines(b), lines(d)]);
        let mut p = Problem::new(g.clone());
        p.walls = [Wall::Pec; 6];
        p.bodies.push(Body { solid: Solid::Outside(Box::new(Solid::Box { min: [0.0, 0.0, 0.0], max: [a, b, d] })), mat: Mat::Pec });
        let asm = assemble(&p);
        let dt = stable_dt(&asm);
        let mut eng = Engine::<f64>::new(&asm, dt, 8);
        let at = g.idx(g.nearest(0, a * 0.33), g.nearest(1, b * 0.5), g.nearest(2, d * 0.33));
        // A pulse narrow enough to ring only this mode: an impulse puts more into the grid's own
        // high modes, which on a coarse grid outweigh it.
        let pulse = Pulse { f0: exact, fc: 0.2 * exact };
        let source_steps = (2.0 * pulse.delay() / dt) as u64;
        let steps = source_steps + (60.0 / (exact * dt)) as u64;
        let mut trace = Vec::with_capacity(steps as usize);
        for s in 0..steps {
            if s < source_steps {
                eng.e[1][at] += pulse.at(s as f64 * dt) * 1e-3;
            }
            eng.advance();
            if s >= source_steps {
                trace.push(eng.e[1][at]);
            }
        }
        let f = peak(&trace, dt, exact * 0.9, exact * 1.1);
        let err = f / exact - 1.0;
        println!("cell {:.1} mm: {:.4} GHz against {:.4}, {:+.3} %, time step {:.3} of Courant", h * 1e3, f / 1e9, exact / 1e9, err * 100.0, dt / (h / (C0 * 3f64.sqrt())));
        errors.push(err.abs());
    }
    assert!(errors.iter().all(|e| *e < 0.003), "{errors:?}");
}

/// A box with real metal walls: the wall loss of TE101 has a textbook Q. This is the skin impedance
/// on a metal body, which the openEMS path doesn't model on anything but flat sheets.
#[test]
fn real_metal_walls_give_the_textbook_q() {
    let (a, b, d): (f64, f64, f64) = (0.03, 0.02, 0.04);
    let sigma = 2e4; // poor metal, for a decay that a short run can read
    let h = 1e-3;
    let f0 = C0 / 2.0 * ((1.0 / a).powi(2) + (1.0 / d).powi(2)).sqrt();
    let w = 2.0 * std::f64::consts::PI * f0;
    let k = w / C0;
    let rs = (w * crate::MU0 / (2.0 * sigma)).sqrt();
    // Pozar's formula for TE101 in a rectangular cavity.
    let q = (k * a * d).powi(3) * b * crate::ETA0 / (2.0 * std::f64::consts::PI.powi(2) * rs) / (2.0 * a.powi(3) * b + 2.0 * b * d.powi(3) + a.powi(3) * d + a * d.powi(3));
    let rate_exact = w / (2.0 * q);
    let lines = |len: f64| -> Vec<f64> { let n = (len / h).round() as usize + 3; (0..n).map(|k| (k as f64 - 1.0) * h).collect() };
    let g = Grid::new([lines(a), lines(b), lines(d)]);
    let mut p = Problem::new(g.clone());
    p.walls = [Wall::Pec; 6];
    p.design_f = f0;
    p.bodies.push(Body { solid: Solid::Outside(Box::new(Solid::Box { min: [0.0; 3], max: [a, b, d] })), mat: Mat::Metal { sigma } });
    let asm = assemble(&p);
    assert!(!asm.rl.is_empty(), "the walls carry loss");
    let dt = stable_dt(&asm);
    let mut eng = Engine::<f64>::new(&asm, dt, 8);
    // In the mode's own shape, inside the box.
    let [nx, ny, nz] = g.n();
    for k in 0..nz {
        for j in 0..ny - 1 {
            for i in 0..nx {
                let (x, y, z) = (g.lines[0][i], g.lines[1][j], g.lines[2][k]);
                if x > 0.0 && x < a && y >= 0.0 && y < b && z > 0.0 && z < d {
                    eng.e[1][g.idx(i, j, k)] = (std::f64::consts::PI * x / a).sin() * (std::f64::consts::PI * z / d).sin();
                }
            }
        }
    }
    let at = g.idx(g.nearest(0, a / 2.0), g.nearest(1, b / 2.0), g.nearest(2, d / 2.0));
    let steps = (30.0 / (f0 * dt)) as u64;
    let mut trace = Vec::with_capacity(steps as usize);
    for _ in 0..steps {
        eng.advance();
        trace.push(eng.e[1][at]);
    }
    let (f, rate) = damped_tone(&trace, dt, f0);
    println!("real metal: {:.4} GHz (lossless {:.4}); Q {:.1} against a textbook {q:.1}; decay {:.3e}/s, exact {:.3e}/s, {:+.1} %", f / 1e9, f0 / 1e9, w / (2.0 * rate), rate, rate_exact, (rate / rate_exact - 1.0) * 100.0);
    assert!((rate / rate_exact - 1.0).abs() < 0.15, "{rate} {rate_exact}");
}

/// A strip and a substrate drawn only from port to port, on a grid that grows behind both planes.
/// Whatever comes back at either port comes from the extrusion or the absorber past it.
fn line_between_ports() -> (Problem, [crate::waveport::WavePortDef; 2]) {
    let (w, h, er): (f64, f64, f64) = (3.0e-3, 1.6e-3, 4.4);
    let (x1, x2) = (-0.025, 0.025);
    let grown = |mut at: f64, sign: f64, cell: f64, stop: f64| {
        let mut v = Vec::new();
        let mut step = cell;
        while at * sign < stop {
            step = (step * 1.2).min(1.5e-3);
            at += sign * step;
            v.push(at);
        }
        v
    };
    let cell = 0.5e-3;
    let mut x = grown(x1, -1.0, cell, 0.04);
    x.reverse();
    x.extend((0..=100).map(|i| x1 + (x2 - x1) * i as f64 / 100.0));
    x.extend(grown(x2, 1.0, cell, 0.04));
    let y = even(-0.02, 0.02, 81);
    let mut z: Vec<f64> = (0..=4).map(|i| h * i as f64 / 4.0).collect();
    let mut step = h / 4.0;
    while *z.last().unwrap() < 0.012 {
        step = (step * 1.15).min(1.2e-3);
        z.push(z.last().unwrap() + step);
    }
    let grid = Grid::new([x, y, z]);
    let mut p = Problem::new(grid.clone());
    p.walls = [Wall::Pml(8), Wall::Pml(8), Wall::Pml(8), Wall::Pml(8), Wall::Pec, Wall::Pml(8)];
    p.bodies.push(Body { solid: Solid::Box { min: [x1, -0.05, 0.0], max: [x2, 0.05, h] }, mat: Mat::Dielectric { eps_r: er, mu_r: 1.0, kappa: 0.0, debye: None } });
    p.sheets.push(Sheet { axis: 2, at: h, polys: vec![vec![[x1, -w / 2.0], [x2, -w / 2.0], [x2, w / 2.0], [x1, w / 2.0]]], surface: None });
    let (p1, p2) = (grid.nearest(0, x1), grid.nearest(0, x2));
    p.extrude.push(Extrude { axis: 0, plane: p1, direction: 1.0 });
    p.extrude.push(Extrude { axis: 0, plane: p2, direction: -1.0 });
    let def = |plane: usize, direction: f64| WavePortDef {
        axis: 0,
        plane,
        r1: (grid.nearest(1, -0.012), grid.nearest(1, 0.012)),
        r2: (0, grid.nearest(2, 0.011)),
        direction,
        signal: (grid.nearest(1, 0.0), grid.nearest(2, h)),
        ground: None,
    };
    (p, [def(p1, 1.0), def(p2, -1.0)])
}

/// Nothing drawn behind the ports, and the grid grading there as well: the operator is copied out to
/// the absorber, so a wave leaves as if the line went on for ever.
#[test]
fn a_line_extruded_behind_its_ports_reflects_nothing() {
    let (p, [d1, d2]) = line_between_ports();
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let (r1, r2) = (WavePortRun::new(&asm, d1).unwrap(), WavePortRun::new(&asm, d2).unwrap());
    println!("mode: Z0 {:.2} ohm, eps_eff {:.3}", r1.mode.z0, r1.mode.eps_eff);
    let mut eng = Engine::<f32>::new(&asm, dt, 24);
    eng.add_wave_port(r1);
    eng.add_wave_port(r2);
    eng.drive_wave(0, Pulse { f0: 3e9, fc: 2e9 });
    let stop = eng.run(40.0, 100_000, |_, _| true);
    assert_ne!(stop, Stop::Diverged);
    for f in [1.5e9, 3e9, 4.5e9] {
        let (a1, b1) = waves_at(&eng.wave_ports[0], dt, f);
        let (_, b2) = waves_at(&eng.wave_ports[1], dt, f);
        println!("{:.1} GHz: |S11| {:.1} dB, |S21| {:.3} dB", f / 1e9, 20.0 * (b1 / a1).norm().log10(), 20.0 * (b2 / a1).norm().log10());
        assert!((b1 / a1).norm() < 0.05, "S11 {}", (b1 / a1).norm());
        assert!(((b2 / a1).norm() - 1.0).abs() < 0.03, "S21 {}", (b2 / a1).norm());
    }
}

/// The same run on the GPU, to the digits single precision allows.
#[cfg(feature = "gpu")]
#[test]
fn wave_ports_on_the_gpu_match_the_cpu() {
    let (p, [d1, d2]) = line_between_ports();
    let asm = assemble(&p);
    let dt = stable_dt(&asm);
    let Ok(mut gpu) = crate::gpu::GpuEngine::new(&asm, dt, 16) else {
        println!("no GPU here");
        return;
    };
    let pulse = Pulse { f0: 3e9, fc: 2e9 };
    gpu.add_wave_port(WavePortRun::new(&asm, d1.clone()).unwrap());
    gpu.add_wave_port(WavePortRun::new(&asm, d2.clone()).unwrap());
    gpu.drive_wave(&asm, 0, pulse);
    while gpu.step < 4000 {
        gpu.advance_batch().unwrap();
    }
    let mut cpu = Engine::<f32>::new(&asm, dt, 24);
    cpu.add_wave_port(WavePortRun::new(&asm, d1).unwrap());
    cpu.add_wave_port(WavePortRun::new(&asm, d2).unwrap());
    cpu.drive_wave(0, pulse);
    cpu.advance_many(gpu.step);
    for f in [1.5e9, 3e9, 4.5e9] {
        for port in 0..2 {
            let (a, b) = (waves_at(&cpu.wave_ports[port], dt, f), waves_at(&gpu.wave_ports[port], dt, f));
            let scale = waves_at(&cpu.wave_ports[0], dt, f).0.norm();
            println!("{:.1} GHz port {port}: CPU {:.5} {:.5}, GPU {:.5} {:.5}", f / 1e9, a.0 / scale, a.1 / scale, b.0 / scale, b.1 / scale);
            assert!((a.0 - b.0).norm() / scale < 2e-3 && (a.1 - b.1).norm() / scale < 2e-3, "{a:?} {b:?}");
        }
    }
}
