//! The built-in solver against the method of moments, on the shapes that disagreed with openEMS.

use std::sync::atomic::AtomicBool;

use super::*;
use crate::mom::{MomOptions, MomProblem};
use crate::project::Project;
use crate::freeform;

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
    let z = problem.solve(f, false).unwrap().z;
    z[0][0]
}

/// The bare 30 x 20 mm loop with 2 mm traces: nothing near it, so only its own inductance and the
/// feed are in question. openEMS read 153 and 158 ohm at 400 MHz, with MoM and Wheeler near 170.
fn bare_loop() -> Project {
    let mut p = Project::from_template("custom", Some("915MHz")).unwrap();
    freeform::draw(&mut p, &serde_json::json!({
        "vars": { "A": "15mm", "B": "20mm", "w": "2mm", "gp": "1.5mm", "Z0": "200" },
        "parts": [{ "kind": "strip", "name": "loop", "path": [["gp/2", "0"], ["A", "0"], ["A", "B"], ["-A", "B"], ["-A", "0"], ["-gp/2", "0"]], "w": "w" }],
        "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
    })).unwrap();
    p.setup.f_lo = 300e6;
    p.setup.f_hi = 1200e6;
    p.setup.far_freqs = Vec::new();
    p.setup.field_planes = false;
    p.setup.points = 181;
    p.setup.max_steps = 3_000_000;
    p
}

/// A study to read, not a verdict: `cargo test -p antenna_core --features fit --release bare_loop -- --ignored --nocapture`
#[test]
#[ignore]
fn study_bare_loop() {
    let p = bare_loop();
    let g = p.geometry().unwrap();
    let freqs = [400e6, 600e6, 800e6, 915e6, 1100e6];
    for edges in [192.0, 384.0] {
        let line: Vec<String> = freqs.iter().map(|f| { let z = mom_z(&p, edges, *f); format!("{:.0} MHz {:.1} {:+.1}j", f / 1e6, z.re, z.im) }).collect();
        println!("MoM {edges}/λ: {}", line.join(" | "));
    }
    for (cells, subcell) in [(12.0, false), (24.0, false), (48.0, false), (12.0, true), (24.0, true), (48.0, true)] {
        let mut q = p.clone();
        q.setup.cells_per_lambda = cells;
        let opts = Options { subcell, threads: 16, ..Default::default() };
        let t0 = std::time::Instant::now();
        let r = quiet(|h| simulate(&q, &g, &opts, h)).unwrap();
        let line: Vec<String> = freqs.iter().map(|f| { let z = at(&r, *f); format!("{:.0} MHz {:.1} {:+.1}j", f / 1e6, z.re, z.im) }).collect();
        println!("FIT {cells}/λ subcell {subcell}: {:.2} M cells {:.1} s: {}", r.size as f64 / 1e6, t0.elapsed().as_secs_f64(), line.join(" | "));
    }
}

/// The same loop with a narrower gap at the feed. MoM bridges the gap with metal and drives a delta
/// gap across it, which is a far narrower gap than the 1.5 mm drawn: if that's the disagreement,
/// narrowing the real gap here walks towards MoM's reactance.
/// `cargo test -p antenna_core --features fit --release study_feed_gap -- --ignored --nocapture`
#[test]
#[ignore]
fn study_feed_gap() {
    let freqs = [400e6, 600e6, 800e6, 915e6, 1100e6];
    let make = |gp_mm: f64| {
        let mut p = Project::from_template("custom", Some("915MHz")).unwrap();
        freeform::draw(&mut p, &serde_json::json!({
            "vars": { "A": "15mm", "B": "20mm", "w": "2mm", "gp": format!("{gp_mm}mm"), "Z0": "200" },
            "parts": [{ "kind": "strip", "name": "loop", "path": [["gp/2", "0"], ["A", "0"], ["A", "B"], ["-A", "B"], ["-A", "0"], ["-gp/2", "0"]], "w": "w" }],
            "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
        })).unwrap();
        p.setup.f_lo = 300e6;
        p.setup.f_hi = 1200e6;
        p.setup.far_freqs = Vec::new();
        p.setup.field_planes = false;
        p.setup.points = 181;
        p.setup.max_steps = 3_000_000;
        p.setup.cells_per_lambda = 24.0;
        p
    };
    let fit_lc = |x400: f64, x915: f64| -> (f64, f64) {
        let w = 2.0 * std::f64::consts::PI;
        let (a, b) = (x400 / (w * 400e6), x915 / (w * 915e6));
        let ratio = b / a;
        let u = (ratio - 1.0) / (ratio * 915e6 * 915e6 - 400e6 * 400e6);
        let l = a * (1.0 - 400e6 * 400e6 * u);
        (l * 1e9, (1.0 / u).sqrt() / 1e6)
    };
    for gp in [1.5, 0.75, 0.375, 0.2, 0.1] {
        let p = make(gp);
        let g = p.geometry().unwrap();
        let opts = Options { threads: 24, ..Default::default() };
        let r = quiet(|h| simulate(&p, &g, &opts, h)).unwrap();
        let z: Vec<C64> = freqs.iter().map(|f| at(&r, *f)).collect();
        let line: Vec<String> = freqs.iter().zip(&z).map(|(f, z)| format!("{:.0} MHz {:+.1}j", f / 1e6, z.im)).collect();
        let (l, fr) = fit_lc(z[0].im, z[3].im);
        println!("FIT gap {gp} mm, {:.2} M cells, {:.0} s: {} | L {l:.1} nH, self-resonance {fr:.0} MHz", r.size as f64 / 1e6, r.seconds, line.join(" | "));
    }
    let p = make(1.5);
    for edges in [96.0, 192.0, 384.0, 768.0] {
        let z: Vec<C64> = freqs.iter().map(|f| mom_z(&p, edges, *f)).collect();
        let line: Vec<String> = freqs.iter().zip(&z).map(|(f, z)| format!("{:.0} MHz {:+.1}j", f / 1e6, z.im)).collect();
        let (l, fr) = fit_lc(z[0].im, z[3].im);
        println!("MoM {edges}/λ: {} | L {l:.1} nH, self-resonance {fr:.0} MHz", line.join(" | "));
    }
}
