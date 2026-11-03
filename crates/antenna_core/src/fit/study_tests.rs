//! Studies to read, not verdicts: adaptive passes, the three solvers side by side, a real tag.

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

/// Adaptive passes against the same model at uniform densities.
#[test]
#[ignore]
fn study_adaptive_passes() {
    let mut p = project("patch_probe", "2.45GHz", 10.0);
    p.setup.points = 201;
    let g = p.geometry().unwrap();
    let (r, passes) = quiet(|h| simulate_adaptive(&p, &g, &Options::default(), h, Adaptive::default())).unwrap();
    for (n, q) in passes.iter().enumerate() {
        println!("pass {}: {:.2} M cells, {:.1} s, change {:?}", n + 1, q.cells as f64 / 1e6, q.seconds, q.change);
    }
    let conv = r.convergence.clone().unwrap();
    println!("converged {} after {} passes, delta {:.4}", conv.converged, conv.passes, conv.delta_s);
    for cells in [20.0, 30.0, 45.0] {
        let mut u = p.clone();
        u.setup.cells_per_lambda = cells;
        let f = fit(&u);
        let d = checks::delta_s(&r.network, &f.network);
        println!("uniform {cells}/λ: {:.2} M cells, {:.1} s, differs from the adaptive result by {d:.4}", f.size as f64 / 1e6, f.seconds);
    }
}

/// Reads an openEMS probe file: time and value columns.
fn probe(path: &std::path::Path) -> (Vec<f64>, Vec<f64>) {
    let text = std::fs::read_to_string(path).unwrap();
    let (mut t, mut v) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        let mut it = line.split_whitespace().filter_map(|x| x.parse::<f64>().ok());
        if let (Some(a), Some(b)) = (it.next(), it.next()) {
            t.push(a);
            v.push(b);
        }
    }
    (t, v)
}

/// One run of the installed openEMS on the same project, for comparison. `None` if not installed.
fn openems_z(p: &Project, freqs: &[f64]) -> Option<(Vec<C64>, u64, f64)> {
    openems_with(p, freqs, None, "multithreaded")
}

/// As `openems_z`, with another build of openEMS or another engine.
fn openems_with(p: &Project, freqs: &[f64], exe: Option<&str>, engine: &str) -> Option<(Vec<C64>, u64, f64)> {
    let exe = match exe {
        Some(x) => std::path::PathBuf::from(x),
        None => std::path::PathBuf::from(std::env::var("LOCALAPPDATA").ok()?).join("SenseSim/openems-fdtd/openEMS/openEMS.exe"),
    };
    if !exe.exists() {
        return None;
    }
    let g = p.geometry().unwrap();
    let plan = crate::csx::plan(&g, &p.setup, p.f0()).unwrap();
    let dir = std::env::temp_dir().join(format!("fit_oems_{}_{}_{engine}", std::process::id(), plan.cells()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sim.xml"), crate::csx::to_xml(&g, &plan, 0)).unwrap();
    let t0 = std::time::Instant::now();
    let threads = (plan.cells() / 80_000).clamp(2, 24).to_string();
    let out = std::process::Command::new(&exe).arg("sim.xml").arg(format!("--engine={engine}")).arg(format!("--numThreads={threads}")).current_dir(&dir).output().unwrap();
    if !out.status.success() {
        println!("openEMS {engine} failed: {}", String::from_utf8_lossy(&out.stderr).lines().rev().take(4).collect::<Vec<_>>().join(" | "));
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if let Some(line) = text.lines().filter(|l| l.contains("Speed:") || l.to_lowercase().contains("gpu") || l.contains("backend")).take(3).map(|l| l.trim().to_string()).reduce(|a, b| format!("{a} | {b}")) {
        println!("  openEMS {engine}: {}", line.chars().take(220).collect::<String>());
    }
    let secs = t0.elapsed().as_secs_f64();
    let (tu, u) = probe(&dir.join("port_ut1"));
    let (ti, i) = probe(&dir.join("port_it1"));
    let z = freqs
        .iter()
        .map(|f| {
            let w = 2.0 * std::f64::consts::PI * f;
            let dft = |t: &[f64], v: &[f64]| t.iter().zip(v).map(|(t, v)| C64::from_polar(*v, -w * t)).fold(C64::new(0.0, 0.0), |a, b| a + b);
            dft(&tu, &u) / dft(&ti, &i)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    Some((z, plan.cells(), secs))
}

/// The three solvers side by side, with times, on the same models.
/// `cargo test -p antenna_core --features gpu --release study_three_solvers -- --ignored --nocapture`
#[test]
#[ignore]
fn study_three_solvers() {
    let freqs = [800e6, 915e6, 1000e6];
    let row = |z: &[C64]| -> String { z.iter().zip(&freqs).map(|(z, f)| format!("{:.0} MHz {:.1}{:+.1}j", f / 1e6, z.re, z.im)).collect::<Vec<_>>().join(" | ") };
    let mut base = project("strip_dipole", "1GHz", 20.0);
    base.setup.far_freqs = Vec::new();
    for cells in [12.0, 20.0, 36.0] {
        let mut p = base.clone();
        p.setup.cells_per_lambda = cells;
        let r = fit(&p);
        let z: Vec<C64> = freqs.iter().map(|f| at(&r, *f)).collect();
        println!("dipole {cells}/λ FIT: {:.2} M cells, {:.1} s: {}", r.size as f64 / 1e6, r.seconds, row(&z));
        if let Some((z, n, s)) = openems_z(&p, &freqs) {
            println!("dipole {cells}/λ openEMS: {:.2} M cells, {s:.1} s: {}", n as f64 / 1e6, row(&z));
        }
    }
    let z: Vec<C64> = freqs.iter().map(|f| mom_z(&base, 192.0, *f)).collect();
    println!("dipole MoM 192/λ: {}", row(&z));
    let mut pl = Project::from_template("custom", Some("915MHz")).unwrap();
    crate::freeform::draw(&mut pl, &serde_json::json!({
        "vars": { "A": "15mm", "B": "20mm", "w": "2mm", "gp": "1.5mm", "Z0": "200" },
        "parts": [{ "kind": "strip", "name": "loop", "path": [["gp/2", "0"], ["A", "0"], ["A", "B"], ["-A", "B"], ["-A", "0"], ["-gp/2", "0"]], "w": "w" }],
        "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
    }))
    .unwrap();
    pl.setup.f_lo = 300e6;
    pl.setup.f_hi = 1200e6;
    pl.setup.far_freqs = Vec::new();
    pl.setup.field_planes = false;
    pl.setup.points = 181;
    pl.setup.max_steps = 3_000_000;
    for cells in [24.0, 48.0] {
        let mut p = pl.clone();
        p.setup.cells_per_lambda = cells;
        let r = fit(&p);
        let z: Vec<C64> = freqs.iter().map(|f| at(&r, *f)).collect();
        println!("loop {cells}/λ FIT: {:.2} M cells, {:.1} s: {}", r.size as f64 / 1e6, r.seconds, row(&z));
        if let Some((z, n, s)) = openems_z(&p, &freqs) {
            println!("loop {cells}/λ openEMS: {:.2} M cells, {s:.1} s: {}", n as f64 / 1e6, row(&z));
        }
    }
    let z: Vec<C64> = freqs.iter().map(|f| mom_z(&pl, 384.0, *f)).collect();
    println!("loop MoM 384/λ: {}", row(&z));
}

/// The searched thin-trace tag from the app, which openEMS took about 46 minutes on, against MoM.
#[test]
#[ignore]
fn study_searched_tag() {
    let mut base = Project::from_template("tag_loop", Some("915MHz")).unwrap();
    for (name, mm) in [("Lx", 93.356), ("w", 0.567), ("a", 16.338), ("b", 14.906), ("wl", 1.867), ("gc", 1.817), ("g", 0.763), ("Hm", 3.474), ("s", 2.655), ("x0", 17.502), ("Lp", 17.954), ("Hp", 13.055)] {
        base.params.iter_mut().find(|r| r.name == name).unwrap().expr = format!("{mm}mm");
    }
    base.params.iter_mut().find(|r| r.name == "n").unwrap().expr = "2".into();
    base.setup.f_lo = 800e6;
    base.setup.f_hi = 1030e6;
    base.setup.far_freqs = vec![915e6];
    base.setup.field_planes = false;
    base.setup.max_steps = 3_000_000;
    let freqs = [860e6, 902e6, 915e6, 928e6, 980e6];
    let z: Vec<C64> = freqs.iter().map(|f| mom_z(&base, 192.0, *f)).collect();
    println!("MoM 192/λ: {}", z.iter().zip(&freqs).map(|(z, f)| format!("{:.0} MHz {:.1}{:+.1}j", f / 1e6, z.re, z.im)).collect::<Vec<_>>().join(" | "));
    for cells in [20.0, 40.0, 80.0] {
        let mut p = base.clone();
        p.setup.cells_per_lambda = cells;
        let r = fit(&p);
        let line: Vec<String> = freqs.iter().map(|f| { let z = at(&r, *f); format!("{:.0} MHz {:.1}{:+.1}j", f / 1e6, z.re, z.im) }).collect();
        let m = r.metrics(0, 0).unwrap();
        println!("FIT {cells}/λ: {:.2} M cells, {:.1} s, warnings {:?}: {} | gain {:.2} dBi, eff {:.3}", r.size as f64 / 1e6, r.seconds, r.warnings, line.join(" | "), m.peak_gain_dbi, m.rad_efficiency);
    }
}

fn drawn_loop(gap_mm: f64) -> Project {
    let mut p = Project::from_template("custom", Some("915MHz")).unwrap();
    crate::freeform::draw(&mut p, &serde_json::json!({
        "vars": { "A": "15mm", "B": "20mm", "w": "2mm", "gp": format!("{gap_mm}mm"), "Z0": "50" },
        "parts": [{ "kind": "strip", "name": "loop", "path": [["gp/2", "0"], ["A", "0"], ["A", "B"], ["-A", "B"], ["-A", "0"], ["-gp/2", "0"]], "w": "w" }],
        "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
    }))
    .unwrap();
    p
}

/// Writes the outlines of the tags in `docs/fit/measure-tags.md`, ready to have made.
#[test]
#[ignore]
fn fit_write_measurement_kit() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/measurement-kit");
    std::fs::create_dir_all(&dir).unwrap();
    let mut searched = Project::from_template("tag_loop", Some("915MHz")).unwrap();
    for (name, mm) in [("Lx", 93.356), ("w", 0.567), ("a", 16.338), ("b", 14.906), ("wl", 1.867), ("gc", 1.817), ("g", 0.763), ("Hm", 3.474), ("s", 2.655), ("x0", 17.502), ("Lp", 17.954), ("Hp", 13.055)] {
        searched.params.iter_mut().find(|r| r.name == name).unwrap().expr = format!("{mm}mm");
    }
    searched.params.iter_mut().find(|r| r.name == "n").unwrap().expr = "2".into();
    for (name, p) in [("loop_2mm", drawn_loop(1.5)), ("loop_2mm_narrow", drawn_loop(0.2)), ("tag_thin", searched)] {
        let g = p.geometry().unwrap();
        for (axis, at, _) in g.flat_layers() {
            std::fs::write(dir.join(format!("{name}.gbr")), g.to_gerber(axis, at)).unwrap();
        }
        std::fs::write(dir.join(format!("{name}.dxf")), g.to_dxf()).unwrap();
        println!("{name}: wrote {}", dir.join(name).display());
    }
}

/// The comparison reads zero against itself, and finds what was changed.
#[test]
fn a_measurement_comparison_finds_a_known_difference() {
    use crate::sparam::Network;
    let f: Vec<f64> = (0..101).map(|k| 800e6 + 4e6 * k as f64).collect();
    let net = |shift: f64, z0: f64| -> Network {
        let s = f
            .iter()
            .map(|f| {
                // A series RLC: resonance at 1 GHz, moved by `shift`.
                let w = 2.0 * std::f64::consts::PI * f;
                let (r, l) = (50.0, 30e-9);
                let w0 = 2.0 * std::f64::consts::PI * (1e9 + shift);
                let c = 1.0 / (w0 * w0 * l);
                let z = C64::new(r, w * l - 1.0 / (w * c));
                vec![vec![(z - z0) / (z + z0)]]
            })
            .collect();
        Network { f: f.clone(), z0: vec![z0], s }
    };
    let same = compare_measured(&net(0.0, 50.0), &net(0.0, 50.0), 800e6, 1.2e9);
    assert!(same.worst_s < 1e-9, "{same:?}");
    assert!(same.worst_x < 1e-6, "{same:?}");
    assert_eq!(same.resonance_meas, same.resonance_sim);
    let moved = compare_measured(&net(0.0, 50.0), &net(20e6, 50.0), 800e6, 1.2e9);
    let (a, b) = (moved.resonance_meas.unwrap(), moved.resonance_sim.unwrap());
    assert!(((b - a) - 20e6).abs() < 4.1e6, "{moved:?}");
    assert!(moved.worst_s > 0.05, "{moved:?}");
    // A simulation against another reference impedance compares the same.
    let other = compare_measured(&net(0.0, 50.0), &net(0.0, 200.0), 800e6, 1.2e9);
    assert!(other.worst_s < 1e-9, "{other:?}");
}

/// Planner faults an independent read found, pinned.
fn sheet_project(solids: Vec<crate::geometry::Solid>, ports: Vec<crate::geometry::PortDef>) -> (Project, Geometry) {
    let mut p = Project::from_template("custom", Some("915MHz")).unwrap();
    p.setup.f_lo = 800e6;
    p.setup.f_hi = 1000e6;
    p.setup.far_freqs = Vec::new();
    p.setup.field_planes = false;
    let g = Geometry { solids, ports, hints: vec![crate::geometry::MeshHint { min: [-0.06, -0.03, 0.0], max: [0.06, 0.03, 0.0], cell: 2e-3 }], ..Default::default() };
    (p, g)
}

fn sheet(name: &str, min: P3, max: P3) -> crate::geometry::Solid {
    crate::geometry::Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min, max } }
}

fn feed(a: P3, b: P3) -> crate::geometry::PortDef {
    crate::geometry::PortDef { name: "feed".into(), a, b, width: 0.0, z0: 50.0 }
}

/// A strip narrower than a cell gets a line through its middle, across its width. On the wrong axis
/// the strip would be moved by up to half a cell, and two neighbours could land on one line.
#[test]
fn a_narrow_strip_gets_its_line_across_its_width() {
    let (y0, y1) = (0.0102, 0.0107);
    let (p, g) = sheet_project(vec![sheet("strip", [-0.05, y0, 0.0], [0.05, y1, 0.0]), sheet("feeder", [-0.05, -0.003, 0.0], [-0.04, 0.003, 0.0])], vec![feed([-0.052, 0.0, 0.0], [-0.05, 0.0, 0.0])]);
    let plan = plan(&g, &p.setup, p.f0(), true, &Default::default()).unwrap();
    let centre = (y0 + y1) * 0.5;
    let nearest = plan.problem.grid.lines[1].iter().map(|y| (y - centre).abs()).fold(f64::MAX, f64::min);
    assert!(nearest < 1e-5, "nearest y line is {} mm from the strip's middle", nearest * 1e3);
}

/// Two sheets closer than the merge distance stay on two lines: on one they are a short.
#[test]
fn close_parallel_sheets_keep_separate_lines() {
    let h = 0.2e-3;
    let (p, g) = sheet_project(vec![sheet("ground", [-0.05, -0.02, 0.0], [0.05, 0.02, 0.0]), sheet("strip", [-0.04, -0.001, h], [0.04, 0.001, h])], vec![feed([-0.04, 0.0, 0.0], [-0.04, 0.0, h])]);
    let plan = plan(&g, &p.setup, p.f0(), true, &Default::default()).unwrap();
    let z = &plan.problem.grid.lines[2];
    for at in [0.0, h] {
        assert!(z.iter().any(|x| (x - at).abs() < 1e-9), "no line at {at}");
    }
    assert!(plan.problem.grid.min_cell() > 1e-5, "{}", plan.problem.grid.min_cell());
}

/// The same plane reached two ways differs in its last digits, and must not become a cell a few
/// atoms thick, whose time step could never get a pulse through.
#[test]
fn a_plane_reached_two_ways_is_one_line() {
    let at = 0.1e-3 + 0.2e-3;
    let port_end = 0.3e-3;
    assert_ne!(at, port_end, "these have to differ for the test to mean anything");
    let (p, g) = sheet_project(vec![sheet("ground", [-0.05, -0.02, 0.0], [0.05, 0.02, 0.0]), sheet("strip", [-0.04, -0.001, at], [0.04, 0.001, at])], vec![feed([-0.04, 0.0, 0.0], [-0.04, 0.0, port_end])]);
    let plan = plan(&g, &p.setup, p.f0(), true, &Default::default()).unwrap();
    assert!(plan.problem.grid.min_cell() > 1e-6, "smallest cell {} m", plan.problem.grid.min_cell());
}

/// Metal thinner than a pair of lines could hold counts as a sheet, and doesn't vanish.
#[test]
fn thin_metal_with_thickness_is_still_metal() {
    let t = 35e-6;
    let (p, g) = sheet_project(vec![sheet("foil", [-0.05, -0.01, 0.0], [0.05, 0.01, t])], vec![feed([-0.052, 0.0, 0.0], [-0.05, 0.0, 0.0])]);
    let plan = plan(&g, &p.setup, p.f0(), true, &Default::default()).unwrap();
    assert_eq!(plan.problem.sheets.len(), 1, "it became a sheet");
    let asm = assemble(&plan.problem);
    let metal = asm.me[0].iter().filter(|m| m.is_infinite()).count();
    let walls = assemble(&Problem { sheets: Vec::new(), ports: Vec::new(), ..plan.problem.clone() }).me[0].iter().filter(|m| m.is_infinite()).count();
    assert!(metal > walls + 100, "{metal} metal edges along x, {walls} of them walls");
}

/// A feed flat on an infinite ground has nothing to drive it, and says so.
#[test]
fn a_feed_flat_on_the_ground_is_refused() {
    let (p, mut g) = sheet_project(vec![sheet("strip", [-0.04, -0.001, 0.002], [0.04, 0.001, 0.002])], vec![feed([-0.001, 0.0, 0.0], [0.001, 0.0, 0.0])]);
    g.ground = true;
    let e = plan(&g, &p.setup, p.f0(), true, &Default::default()).unwrap_err().to_string();
    assert!(e.contains("ground plane"), "{e}");
}

/// openEMS's own GPU engine, from its project's CI build, beside its CPU engine and this solver.
/// `OPENEMS_CI=<path to openEMS.exe> cargo test -p antenna_core --features gpu --release study_openems_gpu -- --ignored --nocapture`
#[test]
#[ignore]
fn study_openems_gpu() {
    let ci = std::env::var("OPENEMS_CI").expect("OPENEMS_CI");
    let freqs = [915e6];
    let mut dipole = project("strip_dipole", "1GHz", 36.0);
    dipole.setup.far_freqs = Vec::new();
    let mut pl = drawn_loop(1.5);
    pl.setup.f_lo = 300e6;
    pl.setup.f_hi = 1200e6;
    pl.setup.far_freqs = Vec::new();
    pl.setup.field_planes = false;
    pl.setup.points = 181;
    pl.setup.max_steps = 3_000_000;
    pl.setup.cells_per_lambda = 24.0;
    for (name, p) in [("dipole 36/λ", &dipole), ("loop 24/λ", &pl)] {
        let r = fit(p);
        println!("{name}: built-in {:.2} M cells {:.1} s, Z {:.2}", r.size as f64 / 1e6, r.seconds, at(&r, 915e6));
        for engine in ["multithreaded", "gpu"] {
            match openems_with(p, &freqs, Some(&ci), engine) {
                Some((z, n, s)) => println!("{name}: openEMS CI {engine}: {:.2} M cells {s:.1} s, Z {:.2}", n as f64 / 1e6, z[0]),
                None => println!("{name}: openEMS CI {engine}: no result"),
            }
        }
    }
}

/// What the adaptive mesh saves, if anything: on each model, the cheapest uniform mesh that lands as
/// close to the finest uniform mesh as the adaptive result does.
#[test]
#[ignore]
fn study_adaptive_savings() {
    let quiet_project = |mut p: Project| {
        p.setup.far_freqs = Vec::new();
        p.setup.field_planes = false;
        p.setup.points = 201;
        p.setup.max_steps = 3_000_000;
        p
    };
    let mut tag = Project::from_template("tag_loop", Some("915MHz")).unwrap();
    for (name, mm) in [("Lx", 93.356), ("w", 0.567), ("a", 16.338), ("b", 14.906), ("wl", 1.867), ("gc", 1.817), ("g", 0.763), ("Hm", 3.474), ("s", 2.655)] {
        tag.params.iter_mut().find(|r| r.name == name).unwrap().expr = format!("{mm}mm");
    }
    tag.setup.f_lo = 800e6;
    tag.setup.f_hi = 1030e6;
    let models = vec![("dipole", project("strip_dipole", "1GHz", 20.0)), ("loop", drawn_loop(2.0)), ("patch", project("patch_probe", "2.45GHz", 20.0)), ("tag", tag)];
    for (name, base) in models {
        let base = quiet_project(base);
        let g = base.geometry().unwrap();
        let mut uniform = Vec::new();
        for cells in [12.0, 20.0, 30.0, 45.0, 60.0] {
            let mut p = base.clone();
            p.setup.cells_per_lambda = cells;
            uniform.push((cells, fit(&p)));
        }
        let reference = uniform.last().unwrap().1.network.clone();
        let mut start = base.clone();
        start.setup.cells_per_lambda = 12.0;
        let (r, passes) = quiet(|h| simulate_adaptive(&start, &g, &Options::default(), h, Adaptive::default())).unwrap();
        let total: f64 = passes.iter().map(|q| q.seconds).sum();
        let error = checks::delta_s(&r.network, &reference);
        println!("{name}: adaptive {} passes, {:.2} M cells at the end, {total:.0} s in all, error against 60/λ {error:.4}, converged {}", passes.len(), r.size as f64 / 1e6, r.convergence.as_ref().is_some_and(|c| c.converged));
        for q in &passes {
            println!("    pass: {:.2} M cells, {:.1} s, change {:?}", q.cells as f64 / 1e6, q.seconds, q.change);
        }
        let mut matched = None;
        for (cells, u) in &uniform {
            let e = checks::delta_s(&u.network, &reference);
            println!("    uniform {cells}/λ: {:.2} M cells, {:.1} s, error {e:.4}", u.size as f64 / 1e6, u.seconds);
            if matched.is_none() && e <= error && *cells < 60.0 {
                matched = Some((*cells, u.size, u.seconds));
            }
        }
        match matched {
            Some((cells, size, seconds)) => println!("    cheapest uniform as close: {cells}/λ, {:.2} M cells, {seconds:.0} s. Adaptive uses {:.0} % of its cells and {:.0} % of its time.", size as f64 / 1e6, r.size as f64 / size as f64 * 100.0, total / seconds * 100.0),
            None => println!("    no coarser uniform mesh gets as close."),
        }
    }
}
