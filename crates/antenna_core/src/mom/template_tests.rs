//! Every catalogue template the method of moments claims to handle has to mesh
//! and solve, at the density and top frequency its project asks for.

use std::time::Instant;

use crate::farfield::FarField;
use crate::geometry::Geometry;
use crate::project::{AUTO_MOM_LIMIT, Project, SolverKind};
use crate::{C64, csx, templates};

use super::{MAX_UNKNOWNS, MomOptions, MomProblem, estimate_unknowns};

struct Outcome {
    unknowns: usize,
    z: C64,
    far: FarField,
    /// Radiated over the power that neither reflected nor left through another port.
    balance: f64,
    warnings: Vec<String>,
    seconds: f64,
}

fn run(p: &Project, g: &Geometry) -> anyhow::Result<Outcome> {
    let t = Instant::now();
    let opts = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, ..Default::default() };
    let m = MomProblem::new(g, p.setup.f_hi, opts)?;
    let mut point = m.solve(p.f0(), true)?;
    let far = point.far.swap_remove(0);
    let z0: Vec<f64> = g.ports.iter().map(|q| q.z0).collect();
    let net = crate::sparam::Network::from_z(vec![p.f0()], std::slice::from_ref(&point.z), z0);
    let leaves: f64 = net.s[0].iter().map(|row| row[0].norm_sqr()).sum();
    let balance = far.p_rad() / (1.0 - leaves);
    Ok(Outcome { unknowns: m.unknowns(), z: point.z[0][0], far, balance, warnings: m.warnings(), seconds: t.elapsed().as_secs_f64() })
}

fn summary(id: &str, kind: &str, o: &Outcome) {
    let m = o.far.metrics();
    println!("{id:<18} {kind:<8} {:>6} unknowns  {:>6.2} s  Z = {:>8.1} {:>+8.1}j  D {:>5.2} dBi at θ {:>3.0} φ {:>3.0}  rad_eff {:.3}  balance {:.3}  warnings {:?}", o.unknowns, o.seconds, o.z.re, o.z.im, m.peak_directivity_dbi, m.peak_theta, m.peak_phi, m.rad_efficiency, o.balance, o.warnings);
}

#[test]
fn every_mom_template_meshes_and_solves() {
    let mut failures = Vec::new();
    let mut solved = 0;
    for t in templates::all() {
        if t.id == "custom" {
            continue;
        }
        let p = Project::from_template(t.id, None).unwrap();
        let g = p.geometry().unwrap();
        let fit = g.solver_fit();
        if !fit.wire_mom && !fit.surface_mom {
            println!("{:<18} FDTD only", t.id);
            continue;
        }
        match run(&p, &g) {
            Ok(o) => {
                summary(t.id, "default", &o);
                solved += 1;
                // PEC has no loss to find: this only checks the far-field integral.
                if (o.balance - 1.0).abs() > crate::checks::BALANCE_LIMIT {
                    failures.push(format!("{}: power balance {}", t.id, o.balance));
                }
            }
            Err(e) => failures.push(format!("{}: {e:#}", t.id)),
        }
    }
    assert!(solved >= 15, "only {solved} templates solved");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn options(p: &Project) -> MomOptions {
    MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, ..Default::default() }
}

/// The estimate decides whether meshing happens at all, and `solver_for` skips the mesh
/// at four times its limit, so it can't be off by anything like that.
#[test]
fn the_unknowns_estimate_tracks_the_real_count() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for t in templates::all() {
        if t.id == "custom" {
            continue;
        }
        let p = Project::from_template(t.id, None).unwrap();
        let g = p.geometry().unwrap();
        let fit = g.solver_fit();
        if !fit.wire_mom && !fit.surface_mom {
            continue;
        }
        for scale in [1.0, 2.5] {
            let f = p.setup.f_hi * scale;
            let Ok(m) = MomProblem::new(&g, f, options(&p)) else { continue };
            let (real, est) = (m.unknowns() as f64, estimate_unknowns(&g, f, &options(&p)));
            println!("{:<18} ×{scale}: {real:>6} unknowns, estimated {est:>7.0} ({:.2})", t.id, est / real);
            checked += 1;
            // Small models round badly and can't get near a limit anyway.
            if real > 100.0 && !(0.4..=2.5).contains(&(est / real)) {
                failures.push(format!("{} ×{scale}: {real} unknowns, estimated {est:.0}", t.id));
            }
        }
    }
    assert!(checked >= 30, "only {checked} checked");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// 860 typed into a field that's in GHz, on the way to 860 MHz. All of this runs on the
/// UI thread each time the project changes, and it used to mesh for many minutes.
#[test]
fn a_mistyped_frequency_is_refused_at_once() {
    let t = Instant::now();
    for (lo, hi) in [(860e9, 96e9), (860e9, 960e9), (0.86e9, 96e9), (86e9, 96e9), (1e4, 1000e9)] {
        let mut p = Project::from_template("rfid_gate", None).unwrap();
        (p.setup.f_lo, p.setup.f_hi) = (lo, hi);
        let g = p.geometry().unwrap();
        assert_eq!(p.solver_for(&g), SolverKind::Fdtd, "{lo} to {hi}");
        assert!(csx::plan(&g, &p.setup, p.f0()).is_err(), "{lo} to {hi}");
        let refused = MomProblem::new(&g, hi, options(&p)).err().expect("too big to mesh").to_string();
        assert!(refused.contains("unknowns"), "{refused}");
    }
    assert!(t.elapsed().as_secs_f64() < 5.0, "took {:?}", t.elapsed());
}

/// The same slip by way of the variable table: typing 2.4GHz into f0 passes through 2,
/// which is 2 Hz, and the template scales itself to a panel some 200 000 km wide.
#[test]
fn a_half_typed_design_frequency_is_refused_at_once() {
    let t = Instant::now();
    let p = Project::from_template("rfid_gate", None).unwrap().with_values(&[("f0".to_string(), 2.0)]);
    let g = p.geometry().unwrap();
    assert_eq!(p.solver_for(&g), SolverKind::Fdtd);
    assert!(csx::plan(&g, &p.setup, p.f0()).is_err());
    assert!(MomProblem::new(&g, p.setup.f_hi, options(&p)).is_err());
    assert!(t.elapsed().as_secs_f64() < 5.0, "took {:?}", t.elapsed());
}

/// The guard is for slips. Everything a design might really ask for still goes through.
#[test]
fn real_designs_are_not_refused() {
    // The gate as the template has it: a surface model well under the limit.
    let p = Project::from_template("rfid_gate", None).unwrap();
    let g = p.geometry().unwrap();
    assert_eq!(p.solver_for(&g), SolverKind::MomSurface);
    assert!(MomProblem::new(&g, p.setup.f_hi, options(&p)).unwrap().unknowns() < AUTO_MOM_LIMIT);
    assert!(csx::plan(&g, &p.setup, p.f0()).is_ok());

    // A wide band, up to 9 GHz: too much for MoM on Auto, an honest big FDTD run.
    let mut wide = p.clone();
    wide.setup.f_hi = 9e9;
    assert_eq!(wide.solver_for(&g), SolverKind::Fdtd);
    let plan = csx::plan(&g, &wide.setup, wide.f0()).unwrap();
    assert!(plan.cells() as f64 > 1e7, "{}", plan.cells());

    // 86 GHz is no slip when the antenna is sized for it: the guard judges the model's
    // size in wavelengths, and isn't a ceiling on frequency.
    let mm = Project::from_template("rfid_gate", Some("86GHz")).unwrap();
    let mg = mm.geometry().unwrap();
    assert_eq!(mm.solver_for(&mg), SolverKind::MomSurface);
    assert!(MomProblem::new(&mg, mm.setup.f_hi, options(&mm)).is_ok());
    assert!(csx::plan(&mg, &mm.setup, mm.f0()).is_ok());

    // Just under the hard limit, on purpose, with the solver picked by hand.
    let mut dense = options(&p);
    dense.edges_per_lambda *= (0.8 * MAX_UNKNOWNS as f64 / estimate_unknowns(&g, p.setup.f_hi, &dense)).sqrt();
    if let Err(e) = MomProblem::new(&g, p.setup.f_hi, dense) {
        panic!("{} edges per wavelength: {e:#}", dense.edges_per_lambda);
    }
}

#[test]
fn a_backwards_sweep_says_so() {
    let mut p = Project::from_template("rfid_gate", None).unwrap();
    p.setup.f_lo = 860e9;
    let e = p.setup.check_band().unwrap_err().to_string();
    assert!(e.contains("860.000 GHz"), "{e}");
    let g = p.geometry().unwrap();
    assert_eq!(csx::plan(&g, &p.setup, p.f0()).unwrap_err().to_string(), e);
    p.setup.f_lo = p.setup.f_hi;
    assert!(p.setup.check_band().is_err());
    p.setup.f_lo = 0.0;
    assert!(p.setup.check_band().is_err());
    p.setup.f_lo = p.setup.f_hi * 0.9;
    assert!(p.setup.check_band().is_ok());
}

/// Printed templates turn into sheet antennas with no substrate. Whether their
/// variable table still evaluates with εr = 1 is the table's business, so a
/// geometry error is only reported; a meshing or solving error fails the test.
#[test]
fn printed_templates_in_air_mesh_and_solve() {
    let mut failures = Vec::new();
    for t in templates::all() {
        if t.substrate == "Air" {
            continue;
        }
        let Ok(mut p) = Project::from_template(t.id, None) else { continue };
        p.substrate = "Air".into();
        // Its edge-resistance formula divides by εr - 1, so the row needs a number.
        if t.id == "patch_inset" {
            p = p.with_values(&[("Redge".to_string(), 200.0)]);
        }
        let g = match p.geometry() {
            Ok(g) => g,
            Err(e) => {
                println!("{:<18} in air: no geometry ({e:#})", t.id);
                continue;
            }
        };
        let fit = g.solver_fit();
        if !fit.wire_mom && !fit.surface_mom {
            println!("{:<18} in air: not a MoM model", t.id);
            continue;
        }
        match run(&p, &g) {
            Ok(o) => summary(t.id, "air", &o),
            Err(e) => failures.push(format!("{}: {e:#}", t.id)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
