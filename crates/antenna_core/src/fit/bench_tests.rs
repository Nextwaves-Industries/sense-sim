//! The benchmarks the openEMS path has to pass, with the same limits, and comparisons to read.

use std::sync::atomic::AtomicBool;

use super::*;
use crate::mom::{MomOptions, MomProblem};
use crate::project::Project;

fn quiet<T>(f: impl FnOnce(&Hooks) -> T) -> T {
    let cancel = AtomicBool::new(false);
    let busy = |_: String, _: f32| {};
    let log = |s: String| println!("  {s}");
    f(&Hooks { cancel: &cancel, busy: &busy, log: &log })
}

fn at(r: &SimResult, f: f64) -> C64 {
    let k = r.network.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).unwrap().0;
    r.network.zin(0)[k]
}

fn mom_z(p: &Project, edges: f64, f: f64) -> C64 {
    let g = p.geometry().unwrap();
    let opts = MomOptions { edges_per_lambda: edges, segments_per_lambda: p.setup.segments_per_lambda, ..Default::default() };
    let problem = MomProblem::new(&g, p.setup.f_hi, opts).unwrap();
    problem.solve(f, false).unwrap().z[0][0]
}

fn project(id: &str, f0: &str, cells: f64) -> Project {
    let mut p = Project::from_template(id, Some(f0)).unwrap();
    p.setup.cells_per_lambda = cells;
    p.setup.field_planes = false;
    p
}

fn fit(p: &Project) -> SimResult {
    let g = p.geometry().unwrap();
    quiet(|h| simulate(p, &g, &Options::default(), h)).unwrap_or_else(|e| panic!("{e:#}"))
}

/// Where X crosses zero going up, port 1: the resonance as Z sees it, free of the S11 reference.
fn zero_reactance(r: &SimResult) -> Option<f64> {
    let z = r.network.zin(0);
    let f = &r.network.f;
    (1..z.len()).find(|k| z[*k - 1].im < 0.0 && z[*k].im >= 0.0).map(|k| f[k - 1] + (f[k] - f[k - 1]) * (-z[k - 1].im) / (z[k].im - z[k - 1].im))
}

/// The same limits as `antenna_fdtd_dipole_matches_theory`.
#[test]
#[ignore]
fn fit_dipole_matches_theory() {
    let r = fit(&project("strip_dipole", "1GHz", 20.0));
    let m = r.metrics(0, 0).unwrap();
    let z = at(&r, 1e9);
    let x0 = zero_reactance(&r).unwrap();
    println!("strip dipole: {} cells, {:.1} s, Z {z:.1}, X = 0 at {:.1} MHz, D {:.3}, eff {:.4}, peak θ {}", r.size, r.seconds, x0 / 1e6, m.peak_directivity_dbi, m.rad_efficiency, m.peak_theta);
    assert!((m.peak_directivity_dbi - 2.15).abs() < 0.05, "D {}", m.peak_directivity_dbi);
    assert!((m.peak_theta - 90.0).abs() <= 3.0, "beam at {}", m.peak_theta);
    assert!((m.rad_efficiency - 1.0).abs() < 0.01, "a lossless antenna radiates what it accepts: {}", m.rad_efficiency);
    assert!((x0 / 1.0037e9 - 1.0).abs() < 0.017, "X = 0 at {x0}");
}

#[test]
#[ignore]
fn fit_monopole_on_ground() {
    let r = fit(&project("monopole", "1GHz", 24.0));
    let m = r.metrics(0, 0).unwrap();
    let z = at(&r, 1e9);
    println!("monopole: {} cells, {:.1} s, Z {z:.1}, D {:.3}, eff {:.4}, peak θ {}", r.size, r.seconds, m.peak_directivity_dbi, m.rad_efficiency, m.peak_theta);
    assert!((m.peak_directivity_dbi - 5.15).abs() < 0.05, "D {}", m.peak_directivity_dbi);
    assert!(m.peak_theta >= 84.0 && m.peak_theta <= 90.0, "beam at {}", m.peak_theta);
    assert!((m.rad_efficiency - 1.0).abs() < 0.01, "{}", m.rad_efficiency);
}

/// A lossy printed patch: resonance near design, beam up, FR-4 eats power.
#[test]
#[ignore]
fn fit_patch_on_fr4() {
    let r = fit(&project("patch_probe", "2.45GHz", 20.0));
    let m = r.metrics(0, 0).unwrap();
    let res = r.network.resonances(0);
    println!("patch: {} cells, {:.1} s, resonances {res:?}, D {:.2}, G {:.2}, eff {:.3}, peak θ {}", r.size, r.seconds, m.peak_directivity_dbi, m.peak_gain_dbi, m.rad_efficiency, m.peak_theta);
    assert!(res.iter().any(|(f, db)| (f / 2.45e9 - 1.0).abs() < 0.015 && *db < -15.0), "{res:?}");
    assert!(m.peak_theta <= 3.0, "beam at {}", m.peak_theta);
    assert!((m.peak_directivity_dbi - 5.75).abs() < 0.3, "{}", m.peak_directivity_dbi);
    assert!((0.40..0.52).contains(&m.rad_efficiency), "FR-4 is lossy: {}", m.rad_efficiency);
}

/// Two ports: the full S-matrix is reciprocal and each port has its pattern.
#[test]
#[ignore]
fn fit_two_ports_are_reciprocal() {
    let mut p = project("mimo_patch", "2.45GHz", 14.0);
    p.setup.end_db = 60.0;
    p.setup.points = 101;
    let r = fit(&p);
    assert_eq!(r.network.ports(), 2);
    let a = r.network.sij_db(1, 0);
    let peak = a.iter().copied().fold(f64::MIN, f64::max);
    let mut worst: f64 = 0.0;
    for k in 0..a.len() {
        if a[k] > peak - 15.0 {
            let (x, y) = (r.network.s[k][1][0], r.network.s[k][0][1]);
            worst = worst.max((x - y).norm() / x.norm());
        }
    }
    println!("two ports: {} cells, {:.1} s, worst relative |S21-S12| {:.2} % within 15 dB of the peak, peak S21 {peak:.1} dB", r.size, r.seconds, worst * 100.0);
    assert!(worst < 0.03, "S21 and S12 differ by {:.1} %", worst * 100.0);
    assert!(peak < -5.0);
}

/// The strip dipole at several densities against MoM: resonance and directivity.
#[test]
#[ignore]
fn study_strip_dipole_against_mom() {
    for cells in [12.0, 20.0, 36.0] {
        let base = project("strip_dipole", "1GHz", cells);
        let r = fit(&base);
        let m = r.metrics(0, 0).unwrap();
        let x0 = zero_reactance(&r).unwrap();
        println!("FIT {cells}/λ: {:.2} M cells {:.1} s: X = 0 at {:.1} MHz, R there {:.1}, D {:.3}", r.size as f64 / 1e6, r.seconds, x0 / 1e6, at(&r, x0).re, m.peak_directivity_dbi);
    }
    let base = project("strip_dipole", "1GHz", 20.0);
    for edges in [48.0, 96.0, 192.0, 384.0] {
        let mut crossing = None;
        let mut last: Option<(f64, C64)> = None;
        for k in 0..41 {
            let f = 0.9e9 + 0.2e9 * k as f64 / 40.0;
            let z = mom_z(&base, edges, f);
            if let Some((fp, zp)) = last
                && zp.im < 0.0
                && z.im >= 0.0
            {
                crossing = Some((fp + (f - fp) * (-zp.im) / (z.im - zp.im), z.re));
            }
            last = Some((f, z));
        }
        println!("MoM {edges}/λ: X = 0 at {:?}", crossing.map(|(f, r)| (f / 1e6, r)));
    }
}

/// A wire loop in both solvers. No sheet and no bridge strip, so if these agree where the flat
/// loop's solvers did not, the difference on the flat loop lies in how a flat feed is modelled.
#[test]
#[ignore]
fn study_wire_loop_in_both() {
    let make = |gap: f64| {
        let (a, b) = (15e-3, 20e-3);
        let mut p = Project::from_template("custom", Some("915MHz")).unwrap();
        p.setup.f_lo = 300e6;
        p.setup.f_hi = 1600e6;
        p.setup.far_freqs = Vec::new();
        p.setup.field_planes = false;
        p.setup.points = 261;
        p.setup.max_steps = 3_000_000;
        let g = Geometry {
            solids: vec![crate::geometry::Solid { name: "loop".into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[gap / 2.0, 0.0, 0.0], [a, 0.0, 0.0], [a, b, 0.0], [-a, b, 0.0], [-a, 0.0, 0.0], [-gap / 2.0, 0.0, 0.0]], r: 0.5e-3 } }],
            ports: vec![crate::geometry::PortDef { name: "feed".into(), a: [-gap / 2.0, 0.0, 0.0], b: [gap / 2.0, 0.0, 0.0], width: 0.0, z0: 200.0 }],
            hints: vec![crate::geometry::MeshHint { min: [-a, 0.0, 0.0], max: [a, b, 0.0], cell: 1e-3 }],
            ..Default::default()
        };
        (p, g)
    };
    let freqs = [400e6, 800e6, 915e6, 1100e6, 1300e6];
    for segs in [48.0, 96.0, 192.0, 384.0] {
        let (p, g) = make(2e-3);
        let opts = MomOptions { segments_per_lambda: segs, edges_per_lambda: 96.0, ..Default::default() };
        let _ = &opts;
        match MomProblem::new(&g, p.setup.f_hi, opts) {
            Ok(problem) => {
                let line: Vec<String> = freqs.iter().map(|f| { let z = problem.solve(*f, false).unwrap().z[0][0]; format!("{:.0} MHz {:.1} {:+.1}j", f / 1e6, z.re, z.im) }).collect();
                println!("wire MoM {segs}/λ: {}", line.join(" | "));
            }
            Err(e) => println!("wire MoM {segs}/λ: {e}"),
        }
    }
    for (gap, cells) in [(2e-3, 12.0), (2e-3, 24.0), (1e-3, 24.0)] {
        let (mut p, g) = make(gap);
        p.setup.cells_per_lambda = cells;
        let r = quiet(|h| simulate(&p, &g, &Options::default(), h)).unwrap_or_else(|e| panic!("{e:#}"));
        let line: Vec<String> = freqs.iter().map(|f| { let z = at(&r, *f); format!("{:.0} MHz {:.1} {:+.1}j", f / 1e6, z.re, z.im) }).collect();
        println!("wire FIT {cells}/λ, gap {gap}: {:.2} M cells {:.0} s: {}", r.size as f64 / 1e6, r.seconds, line.join(" | "));
    }
}

/// The same model on both engines, through the whole of a run: S-parameters and far field.
#[cfg(feature = "gpu")]
#[test]
#[ignore]
fn fit_gpu_and_cpu_give_the_same_antenna() {
    for (id, f0, cells) in [("strip_dipole", "1GHz", 20.0), ("patch_probe", "2.45GHz", 20.0)] {
        let p = project(id, f0, cells);
        let g = p.geometry().unwrap();
        let a = quiet(|h| simulate(&p, &g, &Options { backend: Backend::Cpu, ..Default::default() }, h)).unwrap();
        let b = quiet(|h| simulate(&p, &g, &Options { backend: Backend::Gpu, ..Default::default() }, h)).unwrap();
        let worst = a.network.s.iter().zip(&b.network.s).map(|(x, y)| (x[0][0] - y[0][0]).norm()).fold(0.0, f64::max);
        let (ma, mb) = (a.metrics(0, 0).unwrap(), b.metrics(0, 0).unwrap());
        println!("{id}: {} cells, CPU {:.1} s, GPU {:.1} s, worst |dS11| {worst:.2e}, D {:.3} and {:.3}, eff {:.4} and {:.4}", a.size, a.seconds, b.seconds, ma.peak_directivity_dbi, mb.peak_directivity_dbi, ma.rad_efficiency, mb.rad_efficiency);
        assert!(worst < 2e-3, "{worst}");
        assert!((ma.peak_directivity_dbi - mb.peak_directivity_dbi).abs() < 0.01);
    }
}

/// A dipole whose arms slant at `slope`, with a strip narrower than a cell on all but the finest mesh.
fn slanted_dipole(slope: f64, w: f64, reach: f64) -> Geometry {
    let gap = 2e-3;
    // The strip's width as a cut across y sees it.
    let cut = w * (1.0 + slope * slope).sqrt();
    let arm = |sign: f64| {
        let (x0, x1) = (sign * gap / 2.0, sign * (gap / 2.0 + reach));
        let (y1, half) = (sign * reach * slope, cut / 2.0);
        vec![[x0, -half], [x1, y1 - half], [x1, y1 + half], [x0, half]]
    };
    let sheet = |name: &str, pts: Vec<[f64; 2]>| crate::geometry::Solid { name: name.into(), material: "PEC".into(), shape: Shape::Polygon { axis: 2, at: 0.0, pts, thick: 0.0 } };
    Geometry {
        solids: vec![sheet("right", arm(1.0)), sheet("left", arm(-1.0))],
        ports: vec![crate::geometry::PortDef { name: "feed".into(), a: [-gap / 2.0, 0.0, 0.0], b: [gap / 2.0, 0.0, 0.0], width: cut, z0: 73.0 }],
        ..Default::default()
    }
}

/// A slanted strip narrower than the model's cell, left to the planner, against a fine method of
/// moments solve, which has no grid to step along. What the mesh around it does to the answer, from
/// 10 mm cells to 0.5 mm on a 1 mm strip at 45 degrees: -31.5, -22.2, -9.9, -12.5, -2.8, +0.6 %.
#[test]
#[ignore]
fn slanted_strips_read_true_when_left_to_the_planner() {
    for (slope, what) in [(1.0f64, "45 degrees"), (0.5, "27 degrees")] {
        let w = 2.0e-3;
        // About half a wavelength at 1 GHz along the slant.
        let reach = 0.072 / (1.0 + slope * slope).sqrt();
        let g = slanted_dipole(slope, w, reach);
        let opts = MomOptions { edges_per_lambda: 96.0, segments_per_lambda: 60.0, ..Default::default() };
        let problem = MomProblem::new(&g, 1.5e9, opts).unwrap();
        let fs: Vec<f64> = (0..21).map(|k| 0.7e9 + 0.04e9 * k as f64).collect();
        let xs: Vec<f64> = fs.iter().map(|f| problem.solve(*f, false).unwrap().z[0][0].im).collect();
        let mom = (1..fs.len()).find(|k| xs[*k - 1] < 0.0 && xs[*k] >= 0.0).map(|k| fs[k - 1] + (fs[k] - fs[k - 1]) * (-xs[k - 1]) / (xs[k] - xs[k - 1])).expect("the method of moments finds a resonance");
        let mut p = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        p.setup.f_lo = 0.6e9;
        p.setup.f_hi = 1.5e9;
        p.setup.points = 451;
        p.setup.cells_per_lambda = 20.0;
        p.setup.field_planes = false;
        p.setup.far_freqs = Vec::new();
        let r = quiet(|h| simulate(&p, &g, &Options::default(), h)).unwrap_or_else(|e| panic!("{e:#}"));
        let x0 = zero_reactance(&r).expect("a resonance: the arm's current has a path");
        println!("{what}: strip {:.1} mm: MoM {:.1} MHz, this {:.1} MHz ({:+.1} %), {:.2} M cells, {:.0} s, {:?}", w * 1e3, mom / 1e6, x0 / 1e6, (x0 / mom - 1.0) * 100.0, r.size as f64 / 1e6, r.seconds, r.warnings);
        assert!((x0 / mom - 1.0).abs() < 0.03, "{x0} against {mom}");
        assert!(!r.warnings.iter().any(|w| w.contains("runs in steps")), "{:?}", r.warnings);
    }
}
