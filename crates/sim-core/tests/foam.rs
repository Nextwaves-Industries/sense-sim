//! The verification benchmarks of the OpenFOAM analyses. They need the solver environment.
//!
//!     cargo test --release --test foam -- --ignored --nocapture --test-threads=1

use std::path::PathBuf;

use sim_core::foam::cases::{self, Control, dim};
use sim_core::foam::{self, Case};
use sim_core::jobs::Cancel;

fn work(name: &str) -> PathBuf {
    std::env::temp_dir().join("sense-sim-verify-foam").join(name)
}

fn assert_close(what: &str, got: f64, want: f64, tol: f64) {
    let err = (got - want).abs() / want.abs();
    println!("{what}: got {got:.6e}, expected {want:.6e}, error {:.3}% (allowed {:.1}%)", err * 100.0, tol * 100.0);
    assert!(err <= tol, "{what}: {got} is {:.2}% from {want}, allowed {:.1}%", err * 100.0, tol * 100.0);
}

/// Second-order steady schemes for laminar incompressible runs.
fn simple_numerics(case: &mut Case, velocity_relax: f64) {
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(phi,U) bounded Gauss linearUpwind grad(U); div((nuEff*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\nwallDist { method meshWave; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        &format!(
            "solvers {{ p {{ solver GAMG; smoother GaussSeidel; tolerance 1e-10; relTol 0.01; }} U {{ solver smoothSolver; smoother GaussSeidel; tolerance 1e-10; relTol 0.05; }} }}\nSIMPLE {{ nNonOrthogonalCorrectors 0; consistent yes; pRefCell 0; pRefValue 0; residualControl {{ p 1e-7; U 1e-7; }} }}\nrelaxationFactors {{ equations {{ U {velocity_relax}; }} }}\n"
        ),
    );
}

/// Lid-driven cavity at Re 1000, against Ghia, Ghia and Shin (1982): the centreline's minimum u.
#[test]
#[ignore = "needs the solver environment"]
fn sim_incompressible_cavity() {
    let mut case = Case::new("cavity");
    let n = 129;
    cases::block_mesh(&mut case, [0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [n, n, 1], [1.0, 1.0, 1.0], [("walls", "wall"), ("walls", "wall"), ("walls", "wall"), ("lid", "wall"), ("sides", "empty"), ("sides", "empty")]);
    cases::control_dict(&mut case, &Control { application: "simpleFoam", end_time: 6000.0, dt: 1.0, write_interval: 6000.0, adjustable: None, extra: "" });
    cases::transport(&mut case, 0.001);
    cases::laminar(&mut case);
    simple_numerics(&mut case, 0.9);
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("lid", "type fixedValue; value uniform (1 0 0);"), ("walls", "type noSlip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p", "volScalarField", dim::KINEMATIC_PRESSURE, "0", &[("lid", "type zeroGradient;"), ("walls", "type zeroGradient;"), ("sides", "type empty;")]);
    let t0 = std::time::Instant::now();
    let mut last = String::new();
    let run = foam::run(&case, &work("cavity"), &["blockMesh > log.blockMesh", "simpleFoam"], &Cancel::default(), |l| {
        if l.contains("SIMPLE solution converged") || l.starts_with("Time = ") {
            last = l.to_string();
        }
    })
    .unwrap();
    println!("{:.1} s; last: {last}", t0.elapsed().as_secs_f64());
    let (_, grid) = run.latest(None).unwrap();
    let u = grid.cell_field("U").expect("no U");
    // The vertical centreline: the cells just either side of x = 0.5 average to it.
    let centres = grid.cell_centres();
    let dx = 1.0 / n as f64;
    let mut line: Vec<(f64, f64)> = centres.iter().enumerate().filter(|(_, c)| (c[0] - 0.5).abs() < dx * 0.51).map(|(i, c)| (c[1], f64::from(u.at(i)[0]))).collect();
    line.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (y_min, u_min) = line.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
    println!("u_min {u_min:.4} at y {y_min:.4}");
    assert_close("u_min on the centreline", u_min, -0.3829, 0.02);
    assert_close("its height", y_min, 0.1719, 0.05);
}

/// Plane Poiseuille flow: pressure drop between plates, exact on a fine enough grid.
#[test]
#[ignore = "needs the solver environment"]
fn sim_incompressible_poiseuille() {
    let mut case = Case::new("poiseuille");
    let (length, h, mean, nu) = (1.0, 0.05, 0.1, 1e-4);
    cases::block_mesh(&mut case, [0.0, 0.0, 0.0], [length, h, 0.005], [100, 40, 1], [1.0, 1.0, 1.0], [("inlet", "patch"), ("outlet", "patch"), ("walls", "wall"), ("walls", "wall"), ("sides", "empty"), ("sides", "empty")]);
    cases::control_dict(&mut case, &Control { application: "simpleFoam", end_time: 6000.0, dt: 1.0, write_interval: 6000.0, adjustable: None, extra: "" });
    cases::transport(&mut case, nu);
    cases::laminar(&mut case);
    simple_numerics(&mut case, 0.9);
    // A fully developed parabola coming in, so the drop has no entrance length.
    let parabola = format!("type codedFixedValue; value uniform (0 0 0); name poiseuilleInlet; code #{{ const vectorField& Cf = patch().Cf(); vectorField& f = *this; forAll(Cf, i) {{ const scalar y = Cf[i].y(); f[i] = vector(6.0*{mean}*y*({h}-y)/({h}*{h}), 0, 0); }} #}};");
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, &format!("({mean} 0 0)"), &[("inlet", &parabola), ("outlet", "type zeroGradient;"), ("walls", "type noSlip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p", "volScalarField", dim::KINEMATIC_PRESSURE, "0", &[("inlet", "type zeroGradient;"), ("outlet", "type fixedValue; value uniform 0;"), ("walls", "type zeroGradient;"), ("sides", "type empty;")]);
    let run = foam::run(&case, &work("poiseuille"), &["blockMesh > log.blockMesh", "simpleFoam"], &Cancel::default(), |_| {}).unwrap();
    let (_, grid) = run.latest(None).unwrap();
    let (p, centres) = (grid.cell_field("p").expect("no p"), grid.cell_centres());
    // Gradient between two stations in the fully developed middle, kinematic.
    let station = |x: f64| {
        let v: Vec<f64> = centres.iter().enumerate().filter(|(_, c)| (c[0] - x).abs() < 0.0051).map(|(i, _)| f64::from(p.at(i)[0])).collect();
        v.iter().sum::<f64>() / v.len() as f64
    };
    let gradient = (station(0.2) - station(0.8)) / 0.6;
    assert_close("pressure gradient (12 nu U / h^2)", gradient, 12.0 * nu * mean / (h * h), 0.005);
}

/// `thermophysicalProperties` of an ideal gas with the given molecular weight and gamma = 1.4.
fn ideal_gas(case: &mut Case, mu: f64, region: &str) {
    let molw = 28.96;
    let r = 8314.46 / molw;
    let cp = r * 1.4 / 0.4;
    case.dict(
        &format!("constant{region}"),
        "thermophysicalProperties",
        "dictionary",
        &format!("thermoType {{ type hePsiThermo; mixture pureMixture; transport const; thermo hConst; equationOfState perfectGas; specie specie; energy sensibleInternalEnergy; }}\nmixture {{ specie {{ molWeight {molw}; }} thermodynamics {{ Cp {cp}; Hf 0; }} transport {{ mu {mu}; Pr 0.71; }} }}\n"),
    );
}

fn central_numerics(case: &mut Case) {
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "fluxScheme Kurganov;\nddtSchemes { default Euler; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(tauMC) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; reconstruct(rho) vanLeer; reconstruct(U) vanLeerV; reconstruct(T) vanLeer; }\nsnGradSchemes { default corrected; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        "solvers { \"(rho|rhoU|rhoE)\" { solver diagonal; } U { solver smoothSolver; smoother GaussSeidel; nSweeps 2; tolerance 1e-10; relTol 0; } h { $U; tolerance 1e-10; relTol 0; } e { $U; tolerance 1e-10; relTol 0; } }\n",
    );
}

/// The Sod shock tube, against the exact Riemann solution of its middle state.
#[test]
#[ignore = "needs the solver environment"]
fn sim_compressible_sod() {
    let mut case = Case::new("sod");
    let r = 8314.46 / 28.96;
    let (p_left, rho_left, p_right, rho_right) = (1e5, 1.0, 1e4, 0.125);
    let (t_left, t_right) = (p_left / (rho_left * r), p_right / (rho_right * r));
    // Dimensionless time 0.2, with the scale sqrt(rho/p) of the left state on a tube of length 1.
    let t_end = 0.2 * (rho_left / p_left as f64).sqrt();
    cases::block_mesh(&mut case, [0.0, 0.0, 0.0], [1.0, 0.01, 0.01], [1000, 1, 1], [1.0, 1.0, 1.0], [("ends", "patch"), ("ends", "patch"), ("sides", "empty"), ("sides", "empty"), ("sides", "empty"), ("sides", "empty")]);
    cases::control_dict(&mut case, &Control { application: "rhoCentralFoam", end_time: t_end, dt: 1e-8, write_interval: t_end, adjustable: Some((0.2, 1.0)), extra: "" });
    ideal_gas(&mut case, 0.0, "");
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    central_numerics(&mut case);
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("ends", "type zeroGradient;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p", "volScalarField", dim::PRESSURE, &p_right.to_string(), &[("ends", "type zeroGradient;"), ("sides", "type empty;")]);
    cases::field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t_right.to_string(), &[("ends", "type zeroGradient;"), ("sides", "type empty;")]);
    case.dict("system", "setFieldsDict", "dictionary", &format!("defaultFieldValues (volScalarFieldValue T {t_right} volScalarFieldValue p {p_right});\nregions ( boxToCell {{ box (0 -1 -1) (0.5 1 1); fieldValues (volScalarFieldValue T {t_left} volScalarFieldValue p {p_left}); }} );\n"));
    let run = foam::run(&case, &work("sod"), &["blockMesh > log.blockMesh", "setFields > log.setFields", "rhoCentralFoam"], &Cancel::default(), |_| {}).unwrap();
    let (_, grid) = run.latest(None).unwrap();
    let (p, u, centres) = (grid.cell_field("p").expect("no p"), grid.cell_field("U").expect("no U"), grid.cell_centres());
    // The plateau between the contact and the shock is around x = 0.75.
    let around = |x: f64, f: &dyn Fn(usize) -> f64| {
        let v: Vec<f64> = centres.iter().enumerate().filter(|(_, c)| (c[0] - x).abs() < 0.01).map(|(i, _)| f(i)).collect();
        v.iter().sum::<f64>() / v.len() as f64
    };
    assert_close("p* / p_left", around(0.75, &|i| f64::from(p.at(i)[0])) / p_left, 0.30313, 0.01);
    assert_close("u* (dimensionless)", around(0.75, &|i| f64::from(u.at(i)[0])) / (p_left / rho_left as f64).sqrt(), 0.92745, 0.01);
    // The shock: where pressure falls halfway from the plateau to the right state.
    let mut line: Vec<(f64, f64)> = centres.iter().enumerate().map(|(i, c)| (c[0], f64::from(p.at(i)[0]))).collect();
    line.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = 0.5 * (0.30313 * p_left + p_right);
    let shock = line.windows(2).find(|w| w[0].0 > 0.7 && w[0].1 >= half && w[1].1 < half).expect("no shock")[0].0;
    assert_close("shock position", shock, 0.850, 0.01);
}

/// Supersonic flow over a 15 degree wedge at Mach 2.5: oblique shock theory.
#[test]
#[ignore = "needs the solver environment"]
fn sim_compressible_wedge() {
    let mut case = Case::new("wedge");
    let tan = 15f64.to_radians().tan();
    // A block whose floor rises at the wedge angle from x = 0.
    let (nx, ny) = (160, 120);
    let (l, h) = (1.0, 1.0);
    let body = format!(
        "scale 1;\nvertices\n(\n (0 0 0) ({l} {y} 0) ({l} {h} 0) (0 {h} 0)\n (0 0 0.02) ({l} {y} 0.02) ({l} {h} 0.02) (0 {h} 0.02)\n);\nblocks ( hex (0 1 2 3 4 5 6 7) ({nx} {ny} 1) simpleGrading (1 1 1) );\nedges ();\nboundary\n(\n inlet {{ type patch; faces ((0 4 7 3)); }}\n outlet {{ type patch; faces ((1 2 6 5)); }}\n top {{ type patch; faces ((3 7 6 2)); }}\n wedge {{ type wall; faces ((0 1 5 4)); }}\n sides {{ type empty; faces ((0 3 2 1) (4 5 6 7)); }}\n);\nmergePatchPairs ();\n",
        y = l * tan
    );
    case.dict("system", "blockMeshDict", "dictionary", &body);
    let (t, p) = (300.0, 1e5);
    let a = (1.4 * 8314.46 / 28.96 * t as f64).sqrt();
    let inflow = 2.5 * a;
    let t_end = 6.0 * l / inflow;
    cases::control_dict(&mut case, &Control { application: "rhoCentralFoam", end_time: t_end, dt: 1e-8, write_interval: t_end, adjustable: Some((0.3, 1.0)), extra: "" });
    ideal_gas(&mut case, 0.0, "");
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    central_numerics(&mut case);
    let outflow = "type zeroGradient;";
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, &format!("({inflow} 0 0)"), &[("inlet", &format!("type fixedValue; value uniform ({inflow} 0 0);")), ("outlet", outflow), ("top", outflow), ("wedge", "type slip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p", "volScalarField", dim::PRESSURE, &p.to_string(), &[("inlet", &format!("type fixedValue; value uniform {p};")), ("outlet", outflow), ("top", outflow), ("wedge", "type zeroGradient;"), ("sides", "type empty;")]);
    cases::field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t.to_string(), &[("inlet", &format!("type fixedValue; value uniform {t};")), ("outlet", outflow), ("top", outflow), ("wedge", "type zeroGradient;"), ("sides", "type empty;")]);
    let run = foam::run(&case, &work("wedge"), &["blockMesh > log.blockMesh", "rhoCentralFoam"], &Cancel::default(), |_| {}).unwrap();
    let (_, grid) = run.latest(None).unwrap();
    let (pf, centres) = (grid.cell_field("p").expect("no p"), grid.cell_centres());
    // Behind the shock, near the wall in the wedge's middle.
    let v: Vec<f64> = centres.iter().enumerate().filter(|(_, c)| (c[0] - 0.6).abs() < 0.03 && c[1] - c[0] * tan < 0.04 && c[1] - c[0] * tan > 0.0).map(|(i, _)| f64::from(pf.at(i)[0])).collect();
    assert!(!v.is_empty(), "no cells behind the shock");
    assert_close("p2 / p1", v.iter().sum::<f64>() / v.len() as f64 / p, 2.467, 0.01);
    // The shock angle: at x = 0.6, the height where pressure crosses halfway.
    let mut column: Vec<(f64, f64)> = centres.iter().enumerate().filter(|(_, c)| (c[0] - 0.6).abs() < 0.004).map(|(i, c)| (c[1], f64::from(pf.at(i)[0]))).collect();
    column.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = p * (1.0 + 2.467) / 2.0;
    let y = column.windows(2).find(|w| w[0].1 >= half && w[1].1 < half).expect("no shock in the column")[0].0;
    assert_close("shock angle (deg)", y.atan2(0.6).to_degrees(), 36.9, 0.01);
}

/// Natural convection in a square cavity, against de Vahl Davis (1983): Nu at Ra 1e5.
#[test]
#[ignore = "needs the solver environment"]
fn sim_convective_de_vahl_davis() {
    de_vahl_davis("cavity_hot", 1e5, 4.519, 100, 8000.0);
}

/// The same at Ra 1e6, with its thinner boundary layers.
#[test]
#[ignore = "needs the solver environment"]
fn sim_convective_ra_1e6() {
    de_vahl_davis("cavity_hot_1e6", 1e6, 8.800, 160, 30000.0);
}

fn de_vahl_davis(name: &str, ra: f64, nusselt: f64, n: usize, end_time: f64) {
    let mut case = Case::new(name);
    cases::block_mesh(&mut case, [0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [n, n, 1], [1.0, 1.0, 1.0], [("hot", "wall"), ("cold", "wall"), ("insulated", "wall"), ("insulated", "wall"), ("sides", "empty"), ("sides", "empty")]);
    let (t0, dt_wall, beta, g, pr) = (300.0f64, 2.0f64, 1.0 / 300.0, 9.81f64, 0.71f64);
    let nu = (g * beta * dt_wall * pr / ra).sqrt();
    let rho0 = 1.0;
    let cp = 1000.0;
    cases::control_dict(&mut case, &Control { application: "buoyantSimpleFoam", end_time, dt: 1.0, write_interval: end_time, adjustable: None, extra: "" });
    case.dict("constant", "g", "uniformDimensionedVectorField", &format!("dimensions [0 1 -2 0 0 0 0];\nvalue (0 {} 0);\n", -g));
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    case.dict(
        "constant",
        "thermophysicalProperties",
        "dictionary",
        &format!("thermoType {{ type heRhoThermo; mixture pureMixture; transport const; thermo hConst; equationOfState Boussinesq; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight 28.96; }} equationOfState {{ rho0 {rho0}; T0 {t0}; beta {beta}; }} thermodynamics {{ Cp {cp}; Hf 0; }} transport {{ mu {}; Pr {pr}; }} }}\n", nu * rho0),
    );
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(phi,U) bounded Gauss linearUpwind grad(U); div(phi,h) bounded Gauss linearUpwind default; div(phi,K) bounded Gauss linear; div(phi,Ekp) bounded Gauss linear; div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        "solvers { p_rgh { solver GAMG; smoother DIC; tolerance 1e-9; relTol 0.01; } \"(U|h)\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-9; relTol 0.05; } }\nSIMPLE { nNonOrthogonalCorrectors 0; pRefCell 0; pRefValue 1e5; residualControl { p_rgh 1e-6; U 1e-6; h 1e-6; } }\nrelaxationFactors { fields { rho 1.0; p_rgh 0.4; } equations { U 0.6; h 0.6; } }\n",
    );
    let (hot, cold) = (t0 + dt_wall / 2.0, t0 - dt_wall / 2.0);
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("hot", "type noSlip;"), ("cold", "type noSlip;"), ("insulated", "type noSlip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t0.to_string(), &[("hot", &format!("type fixedValue; value uniform {hot};")), ("cold", &format!("type fixedValue; value uniform {cold};")), ("insulated", "type zeroGradient;"), ("sides", "type empty;")]);
    let fb = "type fixedFluxPressure; value uniform 1e5;";
    cases::field(&mut case, "p_rgh", "volScalarField", dim::PRESSURE, "1e5", &[("hot", fb), ("cold", fb), ("insulated", fb), ("sides", "type empty;")]);
    let calc = "type calculated; value uniform 1e5;";
    cases::field(&mut case, "p", "volScalarField", dim::PRESSURE, "1e5", &[("hot", calc), ("cold", calc), ("insulated", calc), ("sides", "type empty;")]);
    let mut last = String::new();
    let run = foam::run(&case, &work(name), &["blockMesh > log.blockMesh", "buoyantSimpleFoam"], &Cancel::default(), |l| {
        if l.contains("SIMPLE solution converged") {
            last = l.to_string();
        }
    })
    .unwrap();
    println!("{last}");
    let (_, grid) = run.latest(None).unwrap();
    let (tf, centres) = (grid.cell_field("T").expect("no T"), grid.cell_centres());
    // Mean wall gradient at x = 0, second order from the two nearest cell rows.
    let dx = 1.0 / n as f64;
    let row = |x: f64| {
        let v: Vec<f64> = centres.iter().enumerate().filter(|(_, c)| (c[0] - x).abs() < dx * 0.1).map(|(i, _)| f64::from(tf.at(i)[0])).collect();
        v.iter().sum::<f64>() / v.len() as f64
    };
    let (t1, t2) = (row(dx * 0.5), row(dx * 1.5));
    let gradient = (9.0 * (t1 - hot) - (t2 - hot)) / (3.0 * dx);
    assert_close(&format!("Nu at Ra {ra:e}"), -gradient / dt_wall, nusselt, 0.02);
}

/// How hard the interface is kept sharp: what the study uses, or `FOAM_CALPHA`. A sharper interface
/// gives surface tension fewer cells to act through. Measured on the droplet at 40 cells across, the
/// pressure error at cAlpha 1, 0.5, 0.25, 0 was 12.1, 6.5, 4.9, 3.4%. At 1, a finer mesh didn't help (13.2%
/// at 80 cells across); at 0.25 it does (0.75%).
fn alpha_controls(text: &str) -> String {
    let c = std::env::var("FOAM_CALPHA").unwrap_or_else(|_| "0.25".into());
    text.replace("cAlpha 1;", &format!("cAlpha {c};"))
}

/// A rising bubble, against Hysing et al. (2009), test case 1.
#[test]
#[ignore = "needs the solver environment"]
fn sim_multiphase_rising_bubble() {
    let mut case = Case::new("bubble");
    let h: usize = std::env::var("BUBBLE_CELLS").ok().and_then(|v| v.parse().ok()).unwrap_or(80);
    let co: f64 = std::env::var("BUBBLE_CO").ok().and_then(|v| v.parse().ok()).unwrap_or(0.3);
    let max_dt = 0.002 * (80.0 / h as f64).powf(1.5);
    cases::block_mesh(&mut case, [0.0, 0.0, 0.0], [1.0, 2.0, 0.0125], [h, 2 * h, 1], [1.0, 1.0, 1.0], [("slipWalls", "wall"), ("slipWalls", "wall"), ("noSlipWalls", "wall"), ("noSlipWalls", "wall"), ("sides", "empty"), ("sides", "empty")]);
    // Courant numbers don't protect a sharp interface: capillary waves limit the time step to
    // sqrt((rho1 + rho2) h^3 / (4 pi sigma)) = 0.0026 here (Brackbill). Above it, the bubble rises
    // too slowly with no other sign of trouble.
    let extra = format!("maxDeltaT {max_dt};\nfunctions {{ rise {{ type volFieldValue; libs (fieldFunctionObjects); fields (U); operation weightedVolAverage; weightField alpha.light; regionType all; writeControl timeStep; writeInterval 10; writeFields false; log false; }} }}\n");
    cases::control_dict(&mut case, &Control { application: "interFoam", end_time: 3.0, dt: 1e-4, write_interval: 3.0, adjustable: Some((co, co)), extra: &extra });
    case.dict("constant", "g", "uniformDimensionedVectorField", "dimensions [0 1 -2 0 0 0 0];\nvalue (0 -0.98 0);\n");
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    // The bubble is the first phase, so that its alpha is 1 inside the cylinder setAlphaField draws.
    case.dict("constant", "transportProperties", "dictionary", "phases (light heavy);\nlight { transportModel Newtonian; nu 0.01; rho 100; }\nheavy { transportModel Newtonian; nu 0.01; rho 1000; }\nsigma 24.5;\n");
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "ddtSchemes { default Euler; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(rhoPhi,U) Gauss linearUpwind grad(U); div(phi,alpha) Gauss vanLeer; div(phirb,alpha) Gauss linear; div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        &alpha_controls("solvers { \"alpha.light.*\" { nAlphaCorr 2; nAlphaSubCycles 2; cAlpha 1; MULESCorr yes; nLimiterIter 5; solver smoothSolver; smoother symGaussSeidel; tolerance 1e-8; relTol 0; } \"pcorr.*\" { solver PCG; preconditioner DIC; tolerance 1e-5; relTol 0; } p_rgh { solver PCG; preconditioner DIC; tolerance 1e-8; relTol 0.01; } p_rghFinal { $p_rgh; relTol 0; } U { solver smoothSolver; smoother symGaussSeidel; tolerance 1e-7; relTol 0; } }\nPIMPLE { momentumPredictor no; nOuterCorrectors 1; nCorrectors 3; nNonOrthogonalCorrectors 0; pRefCell 0; pRefValue 0; }\nrelaxationFactors { equations { \".*\" 1; } }\n"),
    );
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("slipWalls", "type slip;"), ("noSlipWalls", "type noSlip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p_rgh", "volScalarField", dim::PRESSURE, "0", &[("slipWalls", "type fixedFluxPressure; value uniform 0;"), ("noSlipWalls", "type fixedFluxPressure; value uniform 0;"), ("sides", "type empty;")]);
    cases::field(&mut case, "alpha.light", "volScalarField", dim::NONE, "0", &[("slipWalls", "type zeroGradient;"), ("noSlipWalls", "type zeroGradient;"), ("sides", "type empty;")]);
    case.dict("system", "setAlphaFieldDict", "dictionary", "field alpha.light;\ntype cylinder;\nradius 0.25;\norigin (0.5 0.5 0);\ndirection (0 0 1);\n");
    let run = foam::run(&case, &work("bubble"), &["blockMesh > log.blockMesh", "setAlphaField > log.setAlphaField", "interFoam"], &Cancel::default(), |_| {}).unwrap();
    let (time, grid) = run.latest(None).unwrap();
    println!("reached t = {time}");
    let (alpha, centres) = (grid.cell_field("alpha.light").expect("no alpha"), grid.cell_centres());
    // Centroid of the light fluid.
    let (mut sum, mut mass) = (0.0, 0.0);
    for (i, c) in centres.iter().enumerate() {
        let light = f64::from(alpha.at(i)[0]);
        sum += light * c[1];
        mass += light;
    }
    let area = mass * (1.0 / h as f64) * (1.0 / h as f64);
    assert_close("bubble area is conserved", area, std::f64::consts::PI * 0.0625, 0.01);
    assert_close("centroid height at t = 3", sum / mass, 1.0813, 0.03);
    // The fastest it rose, from the mean velocity of the light fluid over the run.
    let history = run.read_remote("postProcessing/rise/0/volFieldValue.dat").unwrap();
    let peak = history
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.replace(['(', ')'], " ").split_ascii_whitespace().filter_map(|v| v.parse::<f64>().ok()).nth(2))
        .fold(f64::NEG_INFINITY, f64::max);
    assert_close("peak rise speed", peak, 0.2417, 0.03);
}

/// Rotating inner cylinder in an annulus, in a rotating frame: exact torque (Taylor-Couette).
#[test]
#[ignore = "needs the solver environment"]
fn sim_multipurpose_mrf_taylor_couette() {
    let mut case = Case::new("couette");
    let (r1, r2, omega, nu, rho) = (0.05f64, 0.1f64, 2.0f64, 1e-3f64, 1.2f64);
    // A 90-degree sector with cyclic ends would need more set-up; four blocks make the whole annulus.
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let (a, b) = (r1 * s, r2 * s);
    let body = format!(
        "scale 1;\nvertices\n(\n ({a} -{a} 0) ({b} -{b} 0) ({b} {b} 0) ({a} {a} 0) (-{a} {a} 0) (-{b} {b} 0) (-{b} -{b} 0) (-{a} -{a} 0)\n ({a} -{a} 0.01) ({b} -{b} 0.01) ({b} {b} 0.01) ({a} {a} 0.01) (-{a} {a} 0.01) (-{b} {b} 0.01) (-{b} -{b} 0.01) (-{a} -{a} 0.01)\n);\nblocks\n(\n hex (0 1 2 3 8 9 10 11) (40 60 1) simpleGrading (1 1 1)\n hex (3 2 5 4 11 10 13 12) (40 60 1) simpleGrading (1 1 1)\n hex (4 5 6 7 12 13 14 15) (40 60 1) simpleGrading (1 1 1)\n hex (7 6 1 0 15 14 9 8) (40 60 1) simpleGrading (1 1 1)\n);\nedges\n(\n arc 0 3 ({r1} 0 0) arc 1 2 ({r2} 0 0) arc 3 4 (0 {r1} 0) arc 2 5 (0 {r2} 0) arc 4 7 (-{r1} 0 0) arc 5 6 (-{r2} 0 0) arc 7 0 (0 -{r1} 0) arc 6 1 (0 -{r2} 0)\n arc 8 11 ({r1} 0 0.01) arc 9 10 ({r2} 0 0.01) arc 11 12 (0 {r1} 0.01) arc 10 13 (0 {r2} 0.01) arc 12 15 (-{r1} 0 0.01) arc 13 14 (-{r2} 0 0.01) arc 15 8 (0 -{r1} 0.01) arc 14 9 (0 -{r2} 0.01)\n);\nboundary\n(\n inner {{ type wall; faces ((0 8 11 3) (3 11 12 4) (4 12 15 7) (7 15 8 0)); }}\n outer {{ type wall; faces ((1 2 10 9) (2 5 13 10) (5 6 14 13) (6 1 9 14)); }}\n sides {{ type empty; faces ((0 3 2 1) (3 4 5 2) (4 7 6 5) (7 0 1 6) (8 9 10 11) (11 10 13 12) (12 13 14 15) (15 14 9 8)); }}\n);\nmergePatchPairs ();\n"
    );
    case.dict("system", "blockMeshDict", "dictionary", &body);
    let forces = "functions { torque { type forces; libs (forces); patches (inner); rho rhoInf; rhoInf 1.2; CofR (0 0 0); writeControl timeStep; writeInterval 500; log false; } }\n";
    cases::control_dict(&mut case, &Control { application: "rhoSimpleFoam", end_time: 4000.0, dt: 1.0, write_interval: 4000.0, adjustable: None, extra: forces });
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    let molw = 28.96;
    let gas = 8314.46 / molw;
    let cp = gas * 3.5;
    let t = 300.0;
    let p = rho * gas * t;
    case.dict(
        "constant",
        "thermophysicalProperties",
        "dictionary",
        &format!("thermoType {{ type hePsiThermo; mixture pureMixture; transport const; thermo hConst; equationOfState perfectGas; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight {molw}; }} thermodynamics {{ Cp {cp}; Hf 0; }} transport {{ mu {}; Pr 0.71; }} }}\n", nu * rho),
    );
    case.dict("constant", "MRFProperties", "dictionary", &format!("zone {{ cellZone all; active yes; nonRotatingPatches (outer); origin (0 0 0); axis (0 0 1); omega {omega}; }}\n"));
    case.dict("system", "topoSetDict", "dictionary", "actions ( { name all; type cellZoneSet; action new; source boxToCell; box (-1 -1 -1) (1 1 1); } );\n");
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(phi,U) bounded Gauss linearUpwind grad(U); div(phi,h) bounded Gauss upwind; div(phi,K) bounded Gauss linear; div(phid,p) Gauss upwind; div(phiMRF,p) Gauss upwind; div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        &format!("solvers {{ p {{ solver GAMG; smoother GaussSeidel; tolerance 1e-9; relTol 0.01; }} \"(U|h)\" {{ solver PBiCGStab; preconditioner DILU; tolerance 1e-9; relTol 0.05; }} }}\nSIMPLE {{ nNonOrthogonalCorrectors 1; pMinFactor 0.5; pMaxFactor 2; pRefCell 0; pRefValue {p}; residualControl {{ p 1e-7; U 1e-7; h 1e-7; }} }}\nrelaxationFactors {{ fields {{ p 0.3; rho 0.1; }} equations {{ U 0.7; h 0.5; }} }}\n"),
    );
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("inner", &format!("type rotatingWallVelocity; origin (0 0 0); axis (0 0 1); omega {omega}; value uniform (0 0 0);")), ("outer", "type noSlip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p", "volScalarField", dim::PRESSURE, &p.to_string(), &[("inner", "type zeroGradient;"), ("outer", "type zeroGradient;"), ("sides", "type empty;")]);
    cases::field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t.to_string(), &[("inner", "type zeroGradient;"), ("outer", "type zeroGradient;"), ("sides", "type empty;")]);
    let mut last = String::new();
    let run = foam::run(&case, &work("couette"), &["blockMesh > log.blockMesh", "topoSet > log.topoSet", "rhoSimpleFoam"], &Cancel::default(), |l| {
        if l.contains("SIMPLE solution converged") {
            last = l.to_string();
        }
    })
    .unwrap();
    println!("{last}");
    let log = run.read_remote("postProcessing/torque/0/moment.dat").unwrap();
    let line = log.lines().rev().find(|l| !l.starts_with('#') && !l.trim().is_empty()).expect("no torque written");
    let cols: Vec<f64> = line.replace(['(', ')'], " ").split_ascii_whitespace().filter_map(|v| v.parse().ok()).collect();
    println!("moment row: {cols:?}");
    // Time, then total moment x, y, z.
    let torque_z = cols[3];
    let mu = nu * rho;
    let exact = 4.0 * std::f64::consts::PI * mu * omega * r1 * r1 * r2 * r2 / (r2 * r2 - r1 * r1);
    assert_close("torque per metre", -torque_z / 0.01, exact, 0.01);
}
/// Air between plates heated through a solid wall, against the isoflux channel's exact result:
/// the outer surface sits q (t / k_s + D_h / (8.235 k_f)) above the air's mean temperature.
#[test]
#[ignore = "needs the solver environment"]
fn sim_cht_channel() {
    let mut case = Case::new("cht");
    // Half a channel, symmetric at y = 0: air to h, then the solid to h + t.
    let (length, h, t, depth) = (0.3f64, 0.01f64, 0.002f64, 0.001f64);
    let (ny_f, ny_s, nx) = (20usize, 4usize, 75usize);
    let (rho, mu, cp, pr, k_s, q) = (1.2f64, 1.8e-5f64, 1000.0f64, 0.71f64, 1.0f64, 100.0f64);
    let nu = mu / rho;
    let k_f = mu * cp / pr;
    let d_h = 4.0 * h;
    let u = 100.0 * nu / d_h;
    let body = format!(
        "scale 1;\nvertices\n(\n (0 0 0) ({length} 0 0) ({length} {h} 0) (0 {h} 0) (0 0 {depth}) ({length} 0 {depth}) ({length} {h} {depth}) (0 {h} {depth})\n (0 {top} 0) ({length} {top} 0) ({length} {top} {depth}) (0 {top} {depth})\n);\nblocks\n(\n hex (0 1 2 3 4 5 6 7) air ({nx} {ny_f} 1) simpleGrading (1 1 1)\n hex (3 2 9 8 7 6 10 11) solid ({nx} {ny_s} 1) simpleGrading (1 1 1)\n);\nedges ();\nboundary\n(\n inlet {{ type patch; faces ((0 4 7 3)); }}\n outlet {{ type patch; faces ((1 2 6 5)); }}\n centre {{ type symmetryPlane; faces ((0 1 5 4)); }}\n heated {{ type wall; faces ((8 11 10 9)); }}\n solidEnds {{ type wall; faces ((3 7 11 8) (2 9 10 6)); }}\n sides {{ type empty; faces ((0 3 2 1) (4 5 6 7) (3 8 9 2) (7 6 10 11)); }}\n);\nmergePatchPairs ();\n",
        top = h + t
    );
    case.dict("system", "blockMeshDict", "dictionary", &body);
    cases::control_dict(&mut case, &Control { application: "chtMultiRegionSimpleFoam", end_time: 60000.0, dt: 1.0, write_interval: 60000.0, adjustable: None, extra: "" });
    case.dict("system", "fvSchemes", "dictionary", "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n");
    case.dict("system", "fvSolution", "dictionary", "");
    case.dict("constant", "regionProperties", "dictionary", "regions ( fluid (air) solid (solid) );\n");
    case.dict("constant/air", "g", "uniformDimensionedVectorField", "dimensions [0 1 -2 0 0 0 0];\nvalue (0 0 0);\n");
    case.dict("constant", "g", "uniformDimensionedVectorField", "dimensions [0 1 -2 0 0 0 0];\nvalue (0 0 0);\n");
    case.dict("constant/air", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    case.dict("constant/air", "thermophysicalProperties", "dictionary", &format!("thermoType {{ type heRhoThermo; mixture pureMixture; transport const; thermo hConst; equationOfState rhoConst; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight 28.96; }} equationOfState {{ rho {rho}; }} thermodynamics {{ Cp {cp}; Hf 0; }} transport {{ mu {mu}; Pr {pr}; }} }}\n"));
    case.dict("constant/solid", "thermophysicalProperties", "dictionary", &format!("thermoType {{ type heSolidThermo; mixture pureMixture; transport constIso; thermo hConst; equationOfState rhoConst; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight 50; }} transport {{ kappa {k_s}; }} thermodynamics {{ Hf 0; Cp 500; }} equationOfState {{ rho 2000; }} }}\n"));
    case.dict("system/air", "fvSchemes", "dictionary", "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(phi,U) bounded Gauss linearUpwind grad(U); div(phi,h) bounded Gauss linearUpwind grad(h); div(phi,K) bounded Gauss linear; div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n");
    case.dict("system/air", "fvSolution", "dictionary", "solvers { rho { solver diagonal; } rhoFinal { $rho; } p_rgh { solver GAMG; smoother DIC; tolerance 1e-9; relTol 0.01; } \"(U|h)\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-9; relTol 0.05; } }\nSIMPLE { nNonOrthogonalCorrectors 0; pRefCell 0; pRefValue 1e5; rhoMin 0.5; rhoMax 2.0; residualControl { p_rgh 1e-6; U 1e-6; h 1e-8; } }\nrelaxationFactors { fields { rho 1.0; p_rgh 0.5; } equations { U 0.7; h 0.95; } }\n");
    case.dict("system/solid", "fvSchemes", "dictionary", "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n");
    case.dict("system/solid", "fvSolution", "dictionary", "solvers { h { solver PCG; preconditioner DIC; tolerance 1e-9; relTol 0.01; } }\nSIMPLE { nNonOrthogonalCorrectors 0; residualControl { h 1e-8; } }\nrelaxationFactors { equations { h 1.0; } }\n");
    let t0 = 300.0;
    let coupled = |kappa: &str| format!("type compressible::turbulentTemperatureCoupledBaffleMixed; Tnbr T; kappaMethod {kappa}; value uniform {t0};");
    let e = "type empty;";
    let fb = "type fixedFluxPressure; value uniform 1e5;";
    let calc = "type calculated; value uniform 1e5;";
    let write = |case: &mut Case, region: &str, name: &str, class: &str, d: &str, internal: &str, patches: &[(&str, &str)]| {
        let mut body = format!("dimensions {d};\ninternalField uniform {internal};\nboundaryField\n{{\n");
        for (p, text) in patches {
            body.push_str(&format!(" {p} {{ {text} }}\n"));
        }
        body.push_str("}\n");
        case.dict(&format!("0/{region}"), name, class, &body);
    };
    write(&mut case, "air", "U", "volVectorField", dim::VELOCITY, &format!("({u} 0 0)"), &[("inlet", &format!("type fixedValue; value uniform ({u} 0 0);")), ("outlet", "type zeroGradient;"), ("centre", "type symmetryPlane;"), ("air_to_solid", "type noSlip;"), ("sides", e)]);
    write(&mut case, "air", "T", "volScalarField", dim::TEMPERATURE, &t0.to_string(), &[("inlet", &format!("type fixedValue; value uniform {t0};")), ("outlet", "type zeroGradient;"), ("centre", "type symmetryPlane;"), ("air_to_solid", &coupled("fluidThermo")), ("sides", e)]);
    write(&mut case, "air", "p_rgh", "volScalarField", dim::PRESSURE, "1e5", &[("inlet", fb), ("outlet", "type fixedValue; value uniform 1e5;"), ("centre", "type symmetryPlane;"), ("air_to_solid", fb), ("sides", e)]);
    write(&mut case, "air", "p", "volScalarField", dim::PRESSURE, "1e5", &[("inlet", calc), ("outlet", calc), ("centre", "type symmetryPlane;"), ("air_to_solid", calc), ("sides", e)]);
    write(&mut case, "solid", "T", "volScalarField", dim::TEMPERATURE, &t0.to_string(), &[("heated", &format!("type externalWallHeatFluxTemperature; mode flux; q uniform {q}; kappaMethod solidThermo; value uniform {t0};")), ("solidEnds", "type zeroGradient;"), ("solid_to_air", &coupled("solidThermo")), ("sides", e)]);
    write(&mut case, "solid", "p", "volScalarField", dim::PRESSURE, "1e5", &[("heated", calc), ("solidEnds", calc), ("solid_to_air", calc), ("sides", e)]);
    // A finished case takes hours: CHT_REUSE reads the one that's there.
    let run = if std::env::var_os("CHT_REUSE").is_some() {
        foam::Run { distro: sim_core::provision::distro(), remote: "/home/solver/runs/cht".into(), local: work("cht") }
    } else {
        foam::run(&case, &work("cht"), &["mv 0 0.keep", "blockMesh > log.blockMesh", "splitMeshRegions -cellZones -overwrite > log.split", "rm -rf 0", "mv 0.keep 0", "chtMultiRegionSimpleFoam"], &Cancel::default(), |_| {}).unwrap()
    };
    let (_, air) = run.latest(Some("air")).unwrap();
    let (_, solid) = run.latest(Some("solid")).unwrap();
    let dx = length / nx as f64;
    let (tf, uf, ca) = (air.cell_field("T").expect("no T in the air"), air.cell_field("U").expect("no U"), air.cell_centres());
    let (ts, cs) = (solid.cell_field("T").expect("no T in the solid"), solid.cell_centres());
    let dy_s = t / ny_s as f64;
    let exact_gap = q * (t / k_s + d_h / (8.235 * k_f));
    let measure = |station: f64| -> (f64, f64) {
        let (mut sum_ut, mut sum_u) = (0.0, 0.0);
        for (i, c) in ca.iter().enumerate() {
            if (c[0] - station).abs() < dx * 0.51 {
                let ux = f64::from(uf.at(i)[0]);
                sum_ut += ux * f64::from(tf.at(i)[0]);
                sum_u += ux;
            }
        }
        let top_cell: Vec<f64> = cs.iter().enumerate().filter(|(_, c)| (c[0] - station).abs() < dx * 0.51 && (c[1] - (h + t - dy_s / 2.0)).abs() < dy_s * 0.1).map(|(i, _)| f64::from(ts.at(i)[0])).collect();
        assert!(!top_cell.is_empty(), "no cells at the heated surface");
        (sum_ut / sum_u, top_cell.iter().sum::<f64>() / top_cell.len() as f64 + q * (dy_s / 2.0) / k_s)
    };
    for s in [0.06, 0.12, 0.15, 0.18, 0.2, 0.22, 0.24, 0.26, 0.27, 0.28, 0.29] {
        let (bulk, surface) = measure(s);
        println!("x = {s}: bulk rise {:.2} K against {:.2}, surface above bulk {:.3} K, {:+.2}% from developed", bulk - t0, q * s / (rho * cp * u * h), surface - bulk, (surface - bulk - exact_gap) / exact_gap * 100.0);
    }
    // In the developed middle. The exact result holds past the entry length (0.14 m here) and away from
    // the far end, where the wall's conduction and the insulated end bend the heat flow. Measured along
    // this run, from x = 0.12 to 0.22 the gap is within 0.11 to 0.36% of exact; it's 3.9% low by 0.27,
    // only 30 mm from the end, and 10.5% low at 0.29.
    let station = 0.18;
    let (bulk, surface) = measure(station);
    println!("bulk {bulk:.3} K, outer surface {surface:.3} K");
    // The heat must all be there first: otherwise this case hasn't finished converging.
    let expected_bulk = t0 + q * station / (rho * cp * u * h);
    assert_close("bulk temperature rise, from the energy balance", bulk - t0, expected_bulk - t0, 0.02);
    assert_close("surface minus bulk", surface - bulk, exact_gap, 0.02);
}

/// A still droplet held in shape by surface tension: in two dimensions, the pressure inside is
/// sigma / R above the outside (Laplace).
#[test]
#[ignore = "needs the solver environment"]
fn sim_multiphase_droplet_pressure() {
    let mut case = Case::new("droplet");
    let (side, radius, sigma) = (0.04f64, 0.01f64, 0.07f64);
    let n: usize = std::env::var("DROPLET_CELLS").ok().and_then(|v| v.parse().ok()).unwrap_or(160);
    cases::block_mesh(&mut case, [0.0, 0.0, 0.0], [side, side, side / n as f64], [n, n, 1], [1.0, 1.0, 1.0], [("walls", "wall"), ("walls", "wall"), ("walls", "wall"), ("walls", "wall"), ("sides", "empty"), ("sides", "empty")]);
    cases::control_dict(&mut case, &Control { application: "interFoam", end_time: 0.02, dt: 1e-5, write_interval: 0.02, adjustable: Some((0.3, 0.3)), extra: "maxDeltaT 1e-4;\n" });
    case.dict("constant", "g", "uniformDimensionedVectorField", "dimensions [0 1 -2 0 0 0 0];\nvalue (0 0 0);\n");
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    case.dict("constant", "transportProperties", "dictionary", &format!("phases (drop air);\ndrop {{ transportModel Newtonian; nu 1e-6; rho 1000; }}\nair {{ transportModel Newtonian; nu 1.48e-5; rho 1; }}\nsigma {sigma};\n"));
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "ddtSchemes { default Euler; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(rhoPhi,U) Gauss linearUpwind grad(U); div(phi,alpha) Gauss vanLeer; div(phirb,alpha) Gauss linear; div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        &alpha_controls("solvers { \"alpha.drop.*\" { nAlphaCorr 2; nAlphaSubCycles 1; cAlpha 1; MULESCorr yes; nLimiterIter 5; solver smoothSolver; smoother symGaussSeidel; tolerance 1e-8; relTol 0; } \"pcorr.*\" { solver PCG; preconditioner DIC; tolerance 1e-5; relTol 0; } p_rgh { solver PCG; preconditioner DIC; tolerance 1e-8; relTol 0.01; } p_rghFinal { $p_rgh; relTol 0; } U { solver smoothSolver; smoother symGaussSeidel; tolerance 1e-7; relTol 0; } }\nPIMPLE { momentumPredictor no; nOuterCorrectors 1; nCorrectors 3; nNonOrthogonalCorrectors 0; pRefCell 0; pRefValue 0; }\nrelaxationFactors { equations { \".*\" 1; } }\n"),
    );
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("walls", "type noSlip;"), ("sides", "type empty;")]);
    cases::field(&mut case, "p_rgh", "volScalarField", dim::PRESSURE, "0", &[("walls", "type fixedFluxPressure; value uniform 0;"), ("sides", "type empty;")]);
    cases::field(&mut case, "alpha.drop", "volScalarField", dim::NONE, "0", &[("walls", "type zeroGradient;"), ("sides", "type empty;")]);
    case.dict("system", "setAlphaFieldDict", "dictionary", &format!("field alpha.drop;\ntype cylinder;\nradius {radius};\norigin ({} {} 0);\ndirection (0 0 1);\n", side / 2.0, side / 2.0));
    let run = foam::run(&case, &work("droplet"), &["blockMesh > log.blockMesh", "setAlphaField > log.setAlphaField", "interFoam"], &Cancel::default(), |_| {}).unwrap();
    let (time, grid) = run.latest(None).unwrap();
    println!("reached t = {time}");
    let (p, centres) = (grid.cell_field("p_rgh").expect("no p_rgh"), grid.cell_centres());
    let (mut inside, mut outside, mut ni, mut no) = (0.0, 0.0, 0.0, 0.0);
    for (i, c) in centres.iter().enumerate() {
        let r = ((c[0] - side / 2.0).powi(2) + (c[1] - side / 2.0).powi(2)).sqrt();
        if r < 0.5 * radius {
            inside += f64::from(p.at(i)[0]);
            ni += 1.0;
        } else if r > 1.5 * radius {
            outside += f64::from(p.at(i)[0]);
            no += 1.0;
        }
    }
    println!("{n} cells across, {} across the droplet", (2.0 * radius / (side / n as f64)) as u32);
    assert_close("pressure jump", inside / ni - outside / no, sigma / radius, 0.05);
}

/// A choked converging-diverging nozzle: the flow rate is set by the throat alone,
/// m = sqrt(gamma / R) (2 / (gamma + 1))^((gamma + 1) / (2 (gamma - 1))) A p0 / sqrt(T0). A planar
/// half nozzle with slip walls, so that no boundary layer narrows the throat.
#[test]
#[ignore = "needs the solver environment"]
fn sim_multipurpose_choked_nozzle() {
    let mut case = Case::new("nozzle");
    let (half_length, throat, depth) = (0.2f64, 0.02f64, 0.01f64);
    let height = |x: f64| throat * (1.0 + (x / half_length).powi(2));
    let curve = |z: f64| -> String { (1..80).map(|i| { let x = -half_length + 2.0 * half_length * f64::from(i) / 80.0; format!("({x} {} {z})", height(x)) }).collect::<Vec<_>>().join(" ") };
    let (top, l, d) = (height(half_length), half_length, depth);
    let body = format!(
        "scale 1;\nvertices\n(\n (-{l} 0 0) ({l} 0 0) ({l} {top} 0) (-{l} {top} 0)\n (-{l} 0 {d}) ({l} 0 {d}) ({l} {top} {d}) (-{l} {top} {d})\n);\nblocks\n(\n hex (0 1 2 3 4 5 6 7) (240 48 1) simpleGrading (1 1 1)\n);\nedges\n(\n polyLine 3 2 ({})\n polyLine 7 6 ({})\n);\nboundary\n(\n inlet {{ type patch; faces ((0 4 7 3)); }}\n outlet {{ type patch; faces ((1 2 6 5)); }}\n top {{ type wall; faces ((3 7 6 2)); }}\n axis {{ type symmetryPlane; faces ((0 1 5 4)); }}\n sides {{ type empty; faces ((0 3 2 1) (4 5 6 7)); }}\n);\nmergePatchPairs ();\n",
        curve(0.0),
        curve(d)
    );
    case.dict("system", "blockMeshDict", "dictionary", &body);
    let (p0, t0, gamma, molw) = (1e5f64, 300.0f64, 1.4f64, 28.96f64);
    let gas = 8314.46 / molw;
    let cp = gas * gamma / (gamma - 1.0);
    let extra = "maxDeltaT 1e-5;\nfunctions { flow { type surfaceFieldValue; libs (fieldFunctionObjects); regionType patch; name outlet; operation sum; fields (phi); writeControl timeStep; writeInterval 50; writeFields false; log false; } }\n";
    cases::control_dict(&mut case, &Control { application: "rhoPimpleFoam", end_time: 0.03, dt: 1e-7, write_interval: 0.03, adjustable: Some((0.4, 0.4)), extra });
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
    case.dict(
        "constant",
        "thermophysicalProperties",
        "dictionary",
        &format!("thermoType {{ type hePsiThermo; mixture pureMixture; transport const; thermo hConst; equationOfState perfectGas; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight {molw}; }} thermodynamics {{ Cp {cp}; Hf 0; }} transport {{ mu 1e-7; Pr 0.71; }} }}\n"),
    );
    case.dict(
        "system",
        "fvSchemes",
        "dictionary",
        "ddtSchemes { default Euler; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(phi,U) Gauss limitedLinearV 1; div(phi,h) Gauss limitedLinear 1; div(phi,K) Gauss limitedLinear 1; div(phid,p) Gauss limitedLinear 1; div(phi,Ekp) Gauss limitedLinear 1; div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n",
    );
    case.dict(
        "system",
        "fvSolution",
        "dictionary",
        "solvers { \"rho.*\" { solver diagonal; } p { solver PBiCGStab; preconditioner DILU; tolerance 1e-8; relTol 0.01; } pFinal { $p; relTol 0; } \"(U|h)\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-8; relTol 0.01; } \"(U|h)Final\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-8; relTol 0; } }\nPIMPLE { momentumPredictor yes; transonic yes; nOuterCorrectors 2; nCorrectors 2; nNonOrthogonalCorrectors 0; pMinFactor 0.01; pMaxFactor 2; }\nrelaxationFactors { equations { \".*\" 1; } }\n",
    );
    let sym = ("axis", "type symmetryPlane;");
    let sides = ("sides", "type empty;");
    cases::field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", &[("inlet", "type pressureInletOutletVelocity; value uniform (0 0 0);"), ("outlet", "type zeroGradient;"), ("top", "type slip;"), sym, sides]);
    cases::field(&mut case, "p", "volScalarField", dim::PRESSURE, &(0.5 * p0).to_string(), &[("inlet", &format!("type totalPressure; p0 uniform {p0}; U U; phi phi; rho none; psi thermo:psi; gamma {gamma}; value uniform {p0};")), ("outlet", &format!("type waveTransmissive; field p; psi thermo:psi; gamma {gamma}; fieldInf {}; lInf 0.4; value uniform {};", 0.09 * p0, 0.5 * p0)), ("top", "type zeroGradient;"), sym, sides]);
    cases::field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t0.to_string(), &[("inlet", &format!("type totalTemperature; T0 uniform {t0}; U U; phi phi; psi thermo:psi; gamma {gamma}; value uniform {t0};")), ("outlet", "type zeroGradient;"), ("top", "type zeroGradient;"), sym, sides]);
    let run = foam::run(&case, &work("nozzle"), &["blockMesh > log.blockMesh", "rhoPimpleFoam"], &Cancel::default(), |_| {}).unwrap();
    let log = run.read_remote("postProcessing/flow/0/surfaceFieldValue.dat").unwrap();
    let rows: Vec<f64> = log.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split_ascii_whitespace().nth(1).and_then(|v| v.parse().ok())).collect();
    assert!(rows.len() > 4, "no flow rate written");
    let history: Vec<String> = (1..=10).map(|i| format!("{:.5}", rows[rows.len() * i / 10 - 1])).collect();
    println!("flow rate through the run: {}", history.join(" "));
    // Settled: the mean of the last part.
    let tail = &rows[rows.len() - rows.len() / 5..];
    let flow = tail.iter().sum::<f64>() / tail.len() as f64;
    let exact = (gamma / gas).sqrt() * (2.0 / (gamma + 1.0)).powf((gamma + 1.0) / (2.0 * (gamma - 1.0))) * throat * depth * p0 / t0.sqrt();
    println!("the flow rate over the last part ran from {:.6} to {:.6}", tail.iter().copied().fold(f64::INFINITY, f64::min), tail.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    assert_close("choked flow rate", flow, exact, 0.01);
}