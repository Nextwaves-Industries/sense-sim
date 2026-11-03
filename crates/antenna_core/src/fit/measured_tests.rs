//! Against antennas someone built and measured: the two line-fed patches in Vasylchenko, Schols,
//! De Raedt and Vandenbosch, "A benchmarking of six software packages for full-wave analysis of
//! microstrip antennas", EuCAP 2007, which also gives what HFSS, CST and four others predicted for
//! them. This solver hasn't been run on anything built here yet; this is the nearest thing.
//!
//! Sizes are from its Figure 1. The board's outline isn't given, beyond the line reaching its edge.
//! `cargo test -p antenna_core --features gpu --release --lib measured -- --ignored --nocapture --test-threads=1`

use std::sync::atomic::AtomicBool;

use super::*;
use crate::geometry::{MeshHint, PortDef, Solid};

const H: f64 = 1.524e-3;
const PATCH: f64 = 15.2e-3;
const LINE: f64 = 30.4e-3;
const WIDE: f64 = 3.8e-3;
const INSET: f64 = 5.7e-3;
/// From the patch's side to the slot's outer edge.
const SHOULDER: f64 = 5.07e-3;

/// Table 1 and Table 2: Fmin, Fmax and Fc at -10 dB, in GHz.
const MEASURED_PLAIN: (f64, f64, f64) = (5.115, 5.22, 5.168);
const MEASURED_INSERT: (f64, f64, f64) = (5.603, 5.758, 5.681);
const OTHERS_PLAIN: [(&str, f64); 6] = [("MAGMAS", 5.364), ("IE3D", 5.345), ("FEKO", 5.3), ("Momentum", 5.276), ("HFSS", 5.304), ("CST MWS", 5.27)];
const OTHERS_INSERT: [(&str, f64); 5] = [("MAGMAS", 5.615), ("IE3D", 5.541), ("FEKO", 5.38), ("HFSS", 5.539), ("CST MWS", 5.493)];

struct Build<'a> {
    substrate: &'a str,
    /// What's under the patch, if it isn't the substrate.
    insert: Option<&'a str>,
    /// Board edge to patch on the three sides without the line.
    margin: f64,
    metal: &'a str,
    wave: bool,
}

fn model(b: &Build) -> Geometry {
    let half = PATCH / 2.0;
    let line_half = WIDE / 2.0;
    let slot = half - SHOULDER;
    let outline = vec![
        [-LINE, -line_half],
        [INSET, -line_half],
        [INSET, -slot],
        [0.0, -slot],
        [0.0, -half],
        [PATCH, -half],
        [PATCH, half],
        [0.0, half],
        [0.0, slot],
        [INSET, slot],
        [INSET, line_half],
        [-LINE, line_half],
    ];
    let (x0, x1, y) = (-LINE, PATCH + b.margin, half + b.margin);
    let mut solids = vec![Solid { name: "board".into(), material: b.substrate.into(), shape: Shape::Box { min: [x0, -y, 0.0], max: [x1, y, H] } }];
    if let Some(m) = b.insert {
        solids.push(Solid { name: "insert".into(), material: m.into(), shape: Shape::Box { min: [0.0, -half, 0.0], max: [PATCH, half, H] } });
    }
    solids.push(Solid { name: "ground".into(), material: b.metal.into(), shape: Shape::Box { min: [x0, -y, 0.0], max: [x1, y, 0.0] } });
    solids.push(Solid { name: "patch".into(), material: b.metal.into(), shape: Shape::Polygon { axis: 2, at: H, pts: outline, thick: 0.0 } });
    let ports = vec![PortDef { name: "feed".into(), a: [-LINE, 0.0, 0.0], b: [-LINE, 0.0, H], width: WIDE, z0: 50.0 }];
    Geometry {
        solids,
        ports,
        hints: vec![MeshHint { min: [-LINE, -half, 0.0], max: [PATCH, half, H], cell: 0.5e-3 }],
        wave_ports: if b.wave { vec![0] } else { Vec::new() },
        ..Default::default()
    }
}

fn setup_for(cells: f64) -> Project {
    let mut p = Project::from_template("strip_dipole", Some("5.25GHz")).unwrap();
    p.setup.f_lo = 4.5e9;
    p.setup.f_hi = 6.5e9;
    p.setup.points = 801;
    p.setup.cells_per_lambda = cells;
    p.setup.end_db = 60.0;
    p.setup.max_steps = 3_000_000;
    p.setup.field_planes = false;
    p.setup.far_freqs = Vec::new();
    p
}

/// The -10 dB band around the deepest point, Fmin, Fmax, Fc and the deepest S11, in GHz and dB, as
/// the paper's tables give it. Referred to 50 ohms, as its analyser was.
fn band(r: &SimResult) -> Option<(f64, f64, f64, f64)> {
    let n = r.network.renormalized(vec![50.0]);
    let db = n.sij_db(0, 0);
    let low = db.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?.0;
    if db[low] > -10.0 {
        return None;
    }
    let cross = |k: usize, j: usize| n.f[k] + (n.f[j] - n.f[k]) * (-10.0 - db[k]) / (db[j] - db[k]);
    let down = (1..=low).rev().find(|k| db[*k - 1] > -10.0).map(|k| cross(k - 1, k))?;
    let up = (low..db.len() - 1).find(|k| db[*k + 1] > -10.0).map(|k| cross(k, k + 1))?;
    Some((down / 1e9, up / 1e9, (down + up) / 2e9, db[low]))
}

fn run(b: &Build, cells: f64, opts: &Options) -> (SimResult, Option<(f64, f64, f64, f64)>) {
    let p = setup_for(cells);
    let g = model(b);
    let cancel = AtomicBool::new(false);
    let (busy, log) = (|_: String, _: f32| {}, |s: String| println!("    {s}"));
    let r = simulate(&p, &g, opts, &Hooks { cancel: &cancel, busy: &busy, log: &log }).unwrap_or_else(|e| panic!("{e:#}"));
    let found = band(&r);
    (r, found)
}

fn show(what: &str, r: &SimResult, found: Option<(f64, f64, f64, f64)>, measured: f64) {
    match found {
        Some((lo, hi, fc, depth)) => println!("{what}: {lo:.3} to {hi:.3} GHz, Fc {fc:.3} ({:+.2} % from measured), BW {:.3}, deepest {depth:.1} dB, {:.2} M cells, {:.0} s", (fc / measured - 1.0) * 100.0, hi - lo, r.size as f64 / 1e6, r.seconds),
        None => println!("{what}: no -10 dB band, {:.2} M cells, {:.0} s", r.size as f64 / 1e6, r.seconds),
    }
    for w in &r.warnings {
        println!("    warning: {w}");
    }
}

const PROCESS: &str = "RO4003C (process Dk)";

fn plain(wave: bool) -> Build<'static> {
    Build { substrate: PROCESS, insert: None, margin: LINE, metal: "PEC", wave }
}

fn inserted() -> Build<'static> {
    Build { substrate: PROCESS, insert: Some("RO3003"), margin: LINE, metal: "PEC", wave: true }
}

/// The figures to read, and the sensitivities behind them.
#[test]
#[ignore]
fn study_measured_patches() {
    let opts = Options::default();
    println!("Measured: plain {MEASURED_PLAIN:?}, RO3003 under the patch {MEASURED_INSERT:?}");
    println!("Published, Fc in GHz: plain {OTHERS_PLAIN:?}, insert {OTHERS_INSERT:?}");
    for cells in [20.0, 30.0, 45.0] {
        let (r, f) = run(&plain(true), cells, &opts);
        show(&format!("plain, wave port, {cells}/λ"), &r, f, MEASURED_PLAIN.2);
    }
    let (r, f) = run(&plain(false), 30.0, &opts);
    show("plain, lumped feed, 30/λ", &r, f, MEASURED_PLAIN.2);
    let (r, f) = run(&inserted(), 30.0, &opts);
    show("insert, wave port, 30/λ", &r, f, MEASURED_INSERT.2);
    // What the paper doesn't say, and what it might have got wrong.
    for margin in [15e-3, 60e-3] {
        let (r, f) = run(&Build { margin, ..plain(true) }, 30.0, &opts);
        show(&format!("plain, board margin {:.0} mm", margin * 1e3), &r, f, MEASURED_PLAIN.2);
    }
    let (r, f) = run(&Build { metal: "Copper", ..plain(true) }, 30.0, &opts);
    show("plain, copper", &r, f, MEASURED_PLAIN.2);
    let (r, f) = run(&Build { substrate: "RO4003C", ..plain(true) }, 30.0, &opts);
    show("plain, Rogers' design Dk 3.55", &r, f, MEASURED_PLAIN.2);
}

/// A wave port's whole result: S from either engine, and a far field that accounts for the power.
#[test]
#[ignore]
fn a_wave_feed_gives_a_full_result_on_either_engine() {
    let mut p = setup_for(20.0);
    p.setup.far_freqs = vec![5.33e9];
    p.setup.far_step = 5.0;
    p.setup.points = 401;
    let g = model(&plain(true));
    let cancel = AtomicBool::new(false);
    let (busy, log) = (|_: String, _: f32| {}, |s: String| println!("    {s}"));
    let hooks = Hooks { cancel: &cancel, busy: &busy, log: &log };
    let cpu = simulate(&p, &g, &Options { backend: Backend::Cpu, ..Default::default() }, &hooks).unwrap();
    let m = cpu.metrics(0, 0).unwrap();
    println!("CPU: D {:.2} dBi, efficiency {:.3}, peak θ {}, warnings {:?}", m.peak_directivity_dbi, m.rad_efficiency, m.peak_theta, cpu.warnings);
    assert!((4.5..9.0).contains(&m.peak_directivity_dbi), "D {}", m.peak_directivity_dbi);
    // Lossless here, and the rest of the input leaves down the line.
    assert!((m.rad_efficiency - 1.0).abs() < 0.1, "power: {}", m.rad_efficiency);
    assert!(m.peak_theta < 40.0, "beam at {}", m.peak_theta);
    #[cfg(feature = "gpu")]
    {
        let gpu = simulate(&p, &g, &Options { backend: Backend::Gpu, ..Default::default() }, &hooks).unwrap();
        let d = crate::checks::delta_s(&cpu.network, &gpu.network);
        let n = gpu.metrics(0, 0).unwrap();
        println!("GPU: worst |dS11| {d:.2e}, D {:.2}, efficiency {:.3}", n.peak_directivity_dbi, n.rad_efficiency);
        assert!(d < 2e-3, "{d}");
        assert!((n.peak_directivity_dbi - m.peak_directivity_dbi).abs() < 0.02);
    }
}

/// Both solvers' published answers are about 3 % from the antenna that was built, on the plain board,
/// and the other way on the one with an insert. This is held to where they land.
#[test]
#[ignore]
fn the_built_antennas_land_among_the_published_solvers() {
    let opts = Options::default();
    let (r, f) = run(&plain(true), 30.0, &opts);
    let (_, _, plain_fc, _) = f.expect("a -10 dB band");
    show("plain", &r, f, MEASURED_PLAIN.2);
    let (r, f) = run(&inserted(), 30.0, &opts);
    let (_, _, insert_fc, _) = f.expect("a -10 dB band");
    show("insert", &r, f, MEASURED_INSERT.2);
    let range = |v: &[(&str, f64)]| v.iter().fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(x.1), b.max(x.1)));
    let (lo, hi) = range(&OTHERS_PLAIN);
    assert!(plain_fc > lo * 0.995 && plain_fc < hi * 1.005, "plain {plain_fc} against {lo} to {hi}");
    let (lo, hi) = range(&OTHERS_INSERT);
    assert!(insert_fc > lo * 0.995 && insert_fc < hi * 1.005, "insert {insert_fc} against {lo} to {hi}");
    // Within the paper's own bar for all of them, against the antennas themselves.
    assert!((plain_fc / MEASURED_PLAIN.2 - 1.0).abs() < 0.05, "{plain_fc}");
    assert!((insert_fc / MEASURED_INSERT.2 - 1.0).abs() < 0.05, "{insert_fc}");
}

/// A plain line between two wave ports, on the whole path a project takes: all of the wave goes
/// through, each way, delayed by the line between the two planes the feeds were drawn on.
#[test]
#[ignore]
fn two_wave_feeds_read_the_line_between_them() {
    let (length, half) = (40e-3, 20e-3);
    let mut p = setup_for(30.0);
    p.setup.f_lo = 2e9;
    p.setup.f_hi = 6e9;
    p.setup.points = 41;
    let strip = vec![[-length / 2.0, -WIDE / 2.0], [length / 2.0, -WIDE / 2.0], [length / 2.0, WIDE / 2.0], [-length / 2.0, WIDE / 2.0]];
    let g = Geometry {
        solids: vec![
            Solid { name: "board".into(), material: PROCESS.into(), shape: Shape::Box { min: [-length / 2.0, -half, 0.0], max: [length / 2.0, half, H] } },
            Solid { name: "ground".into(), material: "PEC".into(), shape: Shape::Box { min: [-length / 2.0, -half, 0.0], max: [length / 2.0, half, 0.0] } },
            Solid { name: "strip".into(), material: "PEC".into(), shape: Shape::Polygon { axis: 2, at: H, pts: strip, thick: 0.0 } },
        ],
        ports: vec![
            PortDef { name: "left".into(), a: [-length / 2.0, 0.0, 0.0], b: [-length / 2.0, 0.0, H], width: WIDE, z0: 50.0 },
            PortDef { name: "right".into(), a: [length / 2.0, 0.0, 0.0], b: [length / 2.0, 0.0, H], width: WIDE, z0: 50.0 },
        ],
        wave_ports: vec![0, 1],
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let (busy, log) = (|_: String, _: f32| {}, |s: String| println!("    {s}"));
    let r = simulate(&p, &g, &Options::default(), &Hooks { cancel: &cancel, busy: &busy, log: &log }).unwrap_or_else(|e| panic!("{e:#}"));
    // From the ports' own line.
    let eps_eff: f64 = 2.65;
    for f in [3e9, 4e9, 5e9] {
        let k = r.network.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).unwrap().0;
        let s = &r.network.s[k];
        let want = -2.0 * std::f64::consts::PI * r.network.f[k] * eps_eff.sqrt() / C0 * length;
        let wrap = |x: f64| (x + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI) - std::f64::consts::PI;
        println!("{:.1} GHz: |S11| {:.1} dB, |S21| {:.3} dB, S21 {:+.1} deg against {:+.1} for the line, S21 and S12 apart {:.1e}", f / 1e9, 20.0 * s[0][0].norm().log10(), 20.0 * s[1][0].norm().log10(), s[1][0].arg().to_degrees(), wrap(want).to_degrees(), (s[1][0] - s[0][1]).norm());
        assert!(s[0][0].norm() < 0.06, "S11 {}", s[0][0].norm());
        assert!((s[1][0].norm() - 1.0).abs() < 0.03, "S21 {}", s[1][0].norm());
        assert!((s[1][0] - s[0][1]).norm() < 0.01, "not reciprocal: {} {}", s[1][0], s[0][1]);
        assert!(wrap(s[1][0].arg() - want).abs() < 0.03 * want.abs(), "{} against {want}", s[1][0].arg());
    }
}
