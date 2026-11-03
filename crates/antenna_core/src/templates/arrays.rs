//! Arrays as real models, one port per element, so the solve holds their coupling. The Phased
//! array and Direction finding pages read them with no further solve.

use std::f64::consts::TAU;

use anyhow::{Result, bail};

use super::{Ctx, base, bx, hint, p, port, wire};
use crate::geometry::Geometry;
use crate::project::Param;

pub fn dipole_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("nx", "4", "Elements along x"),
        p("ny", "4", "Elements along y"),
        p("dx", "0.5*lambda0", "Spacing along x"),
        p("dy", "0.5*lambda0", "Spacing along y"),
        p("L", "0.44*lambda0", "Dipole length, along y"),
        p("h", "0.25*lambda0", "Height above the ground plane"),
        p("a", "0.003*lambda0", "Wire radius"),
        p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("Z0", "100", "Port impedance"),
    ]);
    v
}

pub fn dipole(c: &Ctx) -> Result<Geometry> {
    let (nx, ny) = (c.count("nx", 1, 16)?, c.count("ny", 1, 16)?);
    let (dx, dy, l, h, a, g, z0) = (c.get("dx")?, c.get("dy")?, c.get("L")?, c.get("h")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    if ny > 1 && dy <= l {
        bail!("Dipoles along y need a spacing dy above their length L, or their ends touch.");
    }
    if nx > 1 && dx <= 4.0 * a {
        bail!("The spacing dx is too small for this wire radius.");
    }
    let (mut solids, mut ports) = (Vec::new(), Vec::new());
    for iy in 0..ny {
        for ix in 0..nx {
            let x = (ix as f64 - (nx as f64 - 1.0) / 2.0) * dx;
            let y = (iy as f64 - (ny as f64 - 1.0) / 2.0) * dy;
            let name = format!("E{}", ports.len() + 1);
            solids.push(wire(&name, c.metal, vec![[x, y - l / 2.0, h], [x, y + l / 2.0, h]], a));
            ports.push(port(&name, [x, y - g / 2.0, h], [x, y + g / 2.0, h], 0.0, z0));
        }
    }
    let (xe, ye) = ((nx as f64 - 1.0) / 2.0 * dx, (ny as f64 - 1.0) / 2.0 * dy + l / 2.0);
    Ok(Geometry { solids, ports, ground: true, axis_forward: true, hints: vec![hint([-xe, -ye, 0.0], [xe, ye, h], l / 20.0)], ..Default::default() })
}

pub fn ring_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("n", "8", "Elements round the ring"),
        p("R", "0.5*lambda0", "Ring radius"),
        p("hm", "0.235*lambda0", "Monopole height"),
        p("a", "0.003*lambda0", "Wire radius"),
        p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}

pub fn ring(c: &Ctx) -> Result<Geometry> {
    let n = c.count("n", 3, 32)?;
    let (r, hm, a, g, z0) = (c.get("R")?, c.get("hm")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    if 2.0 * r * (TAU / 2.0 / n as f64).sin() <= 4.0 * a {
        bail!("The ring is too tight for this many elements of this wire radius.");
    }
    let (mut solids, mut ports) = (Vec::new(), Vec::new());
    for k in 0..n {
        let t = TAU * k as f64 / n as f64;
        let (x, y) = (r * t.cos(), r * t.sin());
        let name = format!("E{}", k + 1);
        solids.push(wire(&name, c.metal, vec![[x, y, 0.0], [x, y, hm]], a));
        ports.push(port(&name, [x, y, 0.0], [x, y, g], 0.0, z0));
    }
    Ok(Geometry { solids, ports, ground: true, hints: vec![hint([-r, -r, 0.0], [r, r, hm], hm / 20.0)], ..Default::default() })
}

pub fn patch_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("nx", "3", "Elements along x"),
        p("ny", "3", "Elements along y"),
        p("dx", "0.6*lambda0", "Spacing along x"),
        p("dy", "0.6*lambda0", "Spacing along y"),
        p("L", "0.45*lambda0", "Patch length, along y"),
        p("W", "0.5*lambda0", "Patch width, along x"),
        p("h", "0.025*lambda0", "Air gap above the ground plane"),
        p("yf", "0.14*lambda0", "Probe offset from the patch centre"),
        p("wp", "lambda0/60", "Probe width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}

pub fn patch(c: &Ctx) -> Result<Geometry> {
    let (nx, ny) = (c.count("nx", 1, 8)?, c.count("ny", 1, 8)?);
    let (dx, dy, l, w, h, yf, wp, z0) = (c.get("dx")?, c.get("dy")?, c.get("L")?, c.get("W")?, c.get("h")?, c.get("yf")?, c.get("wp")?, c.get("Z0")?);
    if (nx > 1 && dx <= w) || (ny > 1 && dy <= l) {
        bail!("The spacing has to clear the patch, or neighbours touch.");
    }
    let (mut solids, mut ports) = (Vec::new(), Vec::new());
    for iy in 0..ny {
        for ix in 0..nx {
            let x = (ix as f64 - (nx as f64 - 1.0) / 2.0) * dx;
            let y = (iy as f64 - (ny as f64 - 1.0) / 2.0) * dy;
            let name = format!("E{}", ports.len() + 1);
            solids.push(bx(&name, c.metal, [x - w / 2.0, y - l / 2.0, h], [x + w / 2.0, y + l / 2.0, h]));
            ports.push(port(&name, [x, y + yf, 0.0], [x, y + yf, h], wp, z0));
        }
    }
    let (xe, ye) = ((nx as f64 - 1.0) / 2.0 * dx + w / 2.0, (ny as f64 - 1.0) / 2.0 * dy + l / 2.0);
    Ok(Geometry { solids, ports, ground: true, axis_forward: true, hints: vec![hint([-xe, -ye, 0.0], [xe, ye, h], l / 20.0)], ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::super::by_id;
    use crate::project::{Project, SolverKind};
    use crate::sparam::Network;
    use crate::{C64, phased};
    use phased::solved::SolvedArray;

    fn solved(id: &str, sets: &[(&str, &str)]) -> (Project, SolvedArray) {
        let mut p = Project::from_template(id, None).unwrap();
        for (name, value) in sets {
            p.params.iter_mut().find(|r| r.name == *name).unwrap().expr = value.to_string();
        }
        let g = p.geometry().unwrap();
        let f = p.f0();
        let opts = crate::mom::MomOptions { theta_step: 5.0, phi_step: 5.0, segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, ..Default::default() };
        let m = crate::mom::MomProblem::new(&g, f, opts).unwrap();
        let t = std::time::Instant::now();
        let pt = m.solve(f, true).unwrap();
        println!("{id}: {} unknowns, {} ports, {:.1} s", m.unknowns(), g.ports.len(), t.elapsed().as_secs_f64());
        let z0: Vec<f64> = g.ports.iter().map(|q| q.z0).collect();
        let net = Network::from_z(vec![f], std::slice::from_ref(&pt.z), z0.clone());
        let pos = g.ports.iter().map(|q| q.center()).collect();
        let names = g.ports.iter().map(|q| q.name.clone()).collect();
        (p.clone(), SolvedArray::new(f, pt.far, net.s[0].clone(), z0, pos, names).unwrap())
    }

    #[test]
    fn the_array_templates_have_a_port_per_element_and_are_listed() {
        for (id, ports) in [("array_dipole", 16), ("array_ring", 8), ("array_patch", 9)] {
            assert!(by_id(id).is_some(), "{id}");
            let p = Project::from_template(id, None).unwrap();
            let g = p.geometry().unwrap();
            assert_eq!(g.ports.len(), ports, "{id}");
            assert_ne!(p.solver_for(&g), SolverKind::Fdtd, "{id} should solve by moments in Auto");
        }
    }

    #[test]
    fn bad_spacings_are_refused_in_words() {
        let mut p = Project::from_template("array_dipole", None).unwrap();
        p.params.iter_mut().find(|r| r.name == "dy").unwrap().expr = "0.3*lambda0".into();
        let e = p.geometry().unwrap_err().to_string();
        assert!(e.contains("touch"), "{e}");
    }

    #[test]
    fn the_dipole_array_is_matched_while_scanned_and_forms_a_beam() {
        let (_, a) = solved("array_dipole", &[]);
        let w: Vec<C64> = a.textbook_weights(0.0, 0.0);
        let row = a.scan_row(0.0, 0.0, &w);
        println!("broadside {row:?}");
        assert!(row.pointing_error_deg < 3.0, "{row:?}");
        assert!(row.tarc_db < -8.0, "active match at broadside: {row:?}");
        assert!(row.realized_dbi > 12.0, "{row:?}");
        let b = a.radiation_matrix();
        let (radiated, accepted) = a.power_balance(&w, &b);
        assert!((radiated / accepted - 1.0).abs() < 0.05, "{radiated} vs {accepted}");
    }

    #[test]
    fn the_ring_has_a_ring_and_the_full_circle_is_read_back() {
        use phased::doa::{self, DfOptions, FromSolved, Impair, Method, Scan, Source, Table};
        let (_, a) = solved("array_ring", &[]);
        let array = phased::df_array_of_ports(a.pos.clone(), a.f);
        assert!(array.ring, "{:?}", a.pos);
        // A monopole on a ground plate has E along θ̂ at the horizon.
        let pol = (C64::new(1.0, 0.0), C64::new(0.0, 0.0));
        let resp = FromSolved::new(&a, Some(pol));
        let table = Table::new(&resp, Scan::Azimuth { theta: 80.0 }, 1.0);
        for phi in [17.0, 123.0, 271.0] {
            let truth = [Source { theta: 80.0, phi, ..Default::default() }];
            let mut rng = proof::Rng::new(phi as u64);
            let x = doa::snapshots(&resp, &truth, 200, 0.01, &Impair::default(), &mut rng);
            let r = doa::estimate(Method::Music, &x, &array, &table, &resp, &DfOptions { count: Some(1), ..Default::default() }).unwrap();
            let e = phased::aperture::angle_between((80.0, phi), r.dirs[0]);
            assert!(e < 0.5, "φ {phi}: {:?}, off by {e}", r.dirs);
        }
    }

    #[test]
    fn the_patch_array_solves_and_scans() {
        let (_, a) = solved("array_patch", &[]);
        let w = a.textbook_weights(0.0, 0.0);
        let row = a.scan_row(0.0, 0.0, &w);
        println!("patch broadside {row:?}");
        assert!(row.pointing_error_deg < 5.0, "{row:?}");
        assert!(row.realized_dbi > 11.0, "{row:?}");
    }
}

