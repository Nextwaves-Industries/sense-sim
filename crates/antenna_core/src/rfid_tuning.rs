//! Finding dimensions for the RFID references, against the method of moments. Ignored: these search,
//! and print what they find. The results go into `samples.rs`, where tests hold them to it.
//!
//! `cargo test -p antenna_core rfid_tune -- --ignored --nocapture --test-threads=1`

use crate::mom::{MomOptions, MomProblem};
use crate::project::Project;
use crate::tag::power_transfer;
use crate::C64;

fn project(template: &str, f0: &str, sets: &[(&str, String)]) -> Project {
    let mut p = Project::from_template(template, Some(f0)).unwrap();
    for (name, value) in sets {
        p.params.iter_mut().find(|r| r.name == *name).unwrap_or_else(|| panic!("no variable {name}")).expr = value.clone();
    }
    p
}

fn opts(edges: f64) -> MomOptions {
    MomOptions { edges_per_lambda: edges, ..Default::default() }
}

/// Input impedance at each frequency.
fn impedance(p: &Project, freqs: &[f64], edges: f64) -> anyhow::Result<(Vec<C64>, usize)> {
    let g = p.geometry()?;
    let f_max = freqs.iter().cloned().fold(0.0, f64::max);
    let m = MomProblem::new(&g, f_max, opts(edges))?;
    let mut out = Vec::new();
    for f in freqs {
        out.push(m.solve(*f, false)?.z[0][0]);
    }
    Ok((out, m.unknowns()))
}

fn mm(v: f64) -> String {
    format!("{v}mm")
}

/// A label-sized inlay. How the reactance goes with the T and the length, against a 15-j231 chip.
#[test]
#[ignore]
fn rfid_tune_tag() {
    let zc = C64::new(15.0, -231.0);
    let freqs = [902e6, 915e6, 928e6];
    println!("{:>5} {:>5} {:>5} {:>2} | {:>18} | {:>5} {:>5} {:>5} | unknowns", "Lx", "a", "b", "n", "Z at 915", "t902", "t915", "t928");
    for lx in [95.0, 85.0, 75.0] {
        for a in [10.0, 14.0, 18.0, 24.0] {
            for b in [3.0, 4.5, 6.0] {
                for n in [0.0, 2.0, 4.0] {
                    let sets = [("Lx", mm(lx)), ("w", mm(1.0)), ("a", mm(a)), ("b", mm(b)), ("g", mm(1.0)), ("n", n.to_string()), ("Hm", mm(3.0)), ("s", mm(3.0)), ("x0", mm(a / 2.0 + 3.0))];
                    let p = project("rfid_tag", "915MHz", &sets);
                    match impedance(&p, &freqs, 24.0) {
                        Ok((z, unknowns)) => {
                            let t: Vec<f64> = z.iter().map(|z| power_transfer(*z, zc)).collect();
                            println!("{lx:>5} {a:>5} {b:>5} {n:>2} | {:>8.1} {:>+8.1}j | {:>5.2} {:>5.2} {:>5.2} | {unknowns}", z[1].re, z[1].im, t[0], t[1], t[2]);
                        }
                        Err(e) => println!("{lx:>5} {a:>5} {b:>5} {n:>2} | {e}"),
                    }
                }
            }
        }
    }
}

/// The tag again, held to a real label's footprint: 95 x 8 mm. Sorted by the worst of the band.
#[test]
#[ignore]
fn rfid_fine_tag() {
    let zc = C64::new(15.0, -231.0);
    let freqs = [902e6, 915e6, 928e6];
    let mut rows = Vec::new();
    for w in [0.6, 1.0, 1.5] {
        for a in [20.0, 24.0, 28.0, 32.0, 36.0, 44.0] {
            for b in [1.8, 2.2, 2.6, 3.0, 3.5] {
                for n in [0.0, 2.0, 3.0, 4.0, 5.0, 6.0] {
                    for s in [2.5, 3.5] {
                        let sets = [("Lx", mm(95.0)), ("w", mm(w)), ("a", mm(a)), ("b", mm(b)), ("g", mm(1.0)), ("n", n.to_string()), ("Hm", mm(3.5)), ("s", mm(s)), ("x0", mm(a / 2.0 + 2.0))];
                        let p = project("rfid_tag", "915MHz", &sets);
                        if let Ok((z, _)) = impedance(&p, &freqs, 24.0) {
                            let t: Vec<f64> = z.iter().map(|z| power_transfer(*z, zc)).collect();
                            let worst = t.iter().cloned().fold(1.0, f64::min);
                            rows.push((worst, format!("w {w} a {a} b {b} n {n} s {s}: Z {:.1} {:+.1}j, t {:.2} {:.2} {:.2}", z[1].re, z[1].im, t[0], t[1], t[2])));
                        }
                    }
                }
            }
        }
    }
    rows.sort_by(|x, y| y.0.total_cmp(&x.0));
    println!("{} designs fit", rows.len());
    for (_, r) in rows.iter().take(16) {
        println!("{r}");
    }
}

/// One feed: the axial ratio is there, so this looks for the match too.
#[test]
#[ignore]
fn rfid_fine_reader_cp() {
    let mut rows = Vec::new();
    for h in [0.025, 0.03, 0.035, 0.04] {
        for l in [0.44, 0.445, 0.45, 0.455, 0.46, 0.465, 0.47] {
            for t in [0.06, 0.07, 0.075, 0.08, 0.085, 0.09, 0.1] {
                for xf in [0.06, 0.08, 0.1, 0.12, 0.14] {
                    let sets = [("h", format!("{h}*lambda0")), ("L", format!("{l}*lambda0")), ("t", format!("{t}*lambda0")), ("xf", format!("{xf}*lambda0"))];
                    let p = project("rfid_reader_cp", "866.5MHz", &sets);
                    let g = p.geometry().unwrap();
                    let m = MomProblem::new(&g, 868e6, opts(12.0)).unwrap();
                    let mut worst = f64::NEG_INFINITY;
                    for f in [865e6, 868e6] {
                        let z = m.solve(f, false).unwrap().z[0][0];
                        worst = worst.max(20.0 * ((z - C64::new(50.0, 0.0)) / (z + C64::new(50.0, 0.0))).norm().log10());
                    }
                    // The pattern costs more, so only for those that could qualify.
                    if worst > -8.0 {
                        continue;
                    }
                    let far = m.solve(866.5e6, true).unwrap().far[0].metrics();
                    rows.push((worst.max(-10.0) + far.axial_ratio_db.max(2.0), format!("h {h} L {l} t {t} xf {xf}: S11 {worst:.1}, D {:.1}, AR {:.1}, {}", far.peak_directivity_dbi, far.axial_ratio_db, if far.rhcp_at_peak { "RHCP" } else { "LHCP" })));
                }
            }
        }
    }
    rows.sort_by(|x, y| x.0.total_cmp(&y.0));
    println!("{} matched designs", rows.len());
    for (_, r) in rows.iter().take(14) {
        println!("{r}");
    }
}

/// A shorted patch on a metal plate, close in to the short where R is low enough for a chip.
#[test]
#[ignore]
fn rfid_fine_onmetal() {
    let zc = C64::new(15.0, -231.0);
    let mut rows = Vec::new();
    for l in [50.0, 55.0, 60.0, 63.0, 66.0, 69.0, 72.0, 75.0, 78.0] {
        for w in [15.0, 20.0, 25.0, 30.0] {
            for h in [2.0, 3.0, 4.0, 5.0] {
                for xf in [1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0] {
                    let sets = [("L", mm(l)), ("W", mm(w)), ("h", mm(h)), ("xf", mm(xf)), ("Gx", mm(200.0)), ("Gy", mm(200.0)), ("wp", mm(1.0))];
                    let p = project("pifa", "915MHz", &sets);
                    if let Ok((z, _)) = impedance(&p, &[902e6, 915e6, 928e6], 16.0) {
                        let t: Vec<f64> = z.iter().map(|z| power_transfer(*z, zc)).collect();
                        let worst = t.iter().cloned().fold(1.0, f64::min);
                        rows.push((t[1], format!("L {l} W {w} h {h} xf {xf}: Z {:.1} {:+.1}j, t {:.2} {:.2} {:.2}, worst {worst:.2}", z[1].re, z[1].im, t[0], t[1], t[2])));
                    }
                }
            }
        }
    }
    rows.sort_by(|x, y| y.0.total_cmp(&x.0));
    println!("{} designs", rows.len());
    for (_, r) in rows.iter().take(14) {
        println!("{r}");
    }
}

/// Whether the mesh is fine enough to believe: the same design at rising density.
#[test]
#[ignore]
fn rfid_tune_tag_mesh() {
    let sets = [("Lx", mm(95.0)), ("w", mm(1.0)), ("a", mm(18.0)), ("b", mm(4.5)), ("g", mm(1.0)), ("n", "2".to_string()), ("Hm", mm(3.0)), ("s", mm(3.0)), ("x0", mm(12.0))];
    let p = project("rfid_tag", "915MHz", &sets);
    for edges in [12.0, 24.0, 40.0, 60.0] {
        let t = std::time::Instant::now();
        let (z, unknowns) = impedance(&p, &[915e6], edges).unwrap();
        println!("edges/lambda {edges:>4}: {unknowns:>5} unknowns, Z = {:.1} {:+.1}j, {:.1} s", z[0].re, z[0].im, t.elapsed().as_secs_f64());
    }
}

/// One feed, cut corners, for the narrow EU band.
#[test]
#[ignore]
fn rfid_tune_reader_cp() {
    let lambda: f64 = 299_792_458.0 / 866.5e6;
    println!("{:>5} {:>6} {:>6} {:>6} | {:>16} | {:>6} {:>6} {:>5} {:>5}", "h", "L", "t", "xf", "Z", "S11", "gain", "AR", "hand");
    for h in [0.04] {
        for l in [0.43, 0.44, 0.45, 0.46] {
            for t in [0.05, 0.065, 0.08] {
                for xf in [0.08, 0.11] {
                    let sets = [("h", format!("{}*lambda0", h)), ("L", format!("{l}*lambda0")), ("t", format!("{t}*lambda0")), ("xf", format!("{xf}*lambda0"))];
                    let p = project("rfid_reader_cp", "866.5MHz", &sets);
                    let g = p.geometry().unwrap();
                    let m = MomProblem::new(&g, 868e6, opts(12.0)).unwrap();
                    let point = m.solve(866.5e6, true).unwrap();
                    let z = point.z[0][0];
                    let s = (z - C64::new(50.0, 0.0)) / (z + C64::new(50.0, 0.0));
                    let far = point.far[0].metrics();
                    println!("{h:>5} {l:>6} {t:>6} {xf:>6} | {:>7.1} {:>+7.1}j | {:>6.1} {:>6.1} {:>5.1} {:>5}", z.re, z.im, 20.0 * s.norm().log10(), far.peak_directivity_dbi, far.axial_ratio_db, if far.rhcp_at_peak { "RHCP" } else { "LHCP" });
                }
            }
        }
    }
    println!("lambda {lambda:.3} m");
}

/// Two feeds in quadrature, for the wide US band.
#[test]
#[ignore]
fn rfid_tune_reader_dual() {
    use crate::farfield::FarField;
    let weights = [C64::from_polar(0.5f64.sqrt(), 0.0), C64::from_polar(0.5f64.sqrt(), (-90f64).to_radians())];
    println!("{:>5} {:>6} {:>6} | {:>14} {:>14} {:>14} | {:>6} {:>5} {:>5}", "h", "L", "xf", "active S 902", "915", "928", "gain", "AR", "hand");
    for h in [0.04, 0.05, 0.06] {
        for l in [0.41, 0.43, 0.45] {
            for xf in [0.07, 0.09, 0.11] {
                let sets = [("h", format!("{h}*lambda0")), ("L", format!("{l}*lambda0")), ("xf", format!("{xf}*lambda0"))];
                let p = project("rfid_reader_dual", "915MHz", &sets);
                let g = p.geometry().unwrap();
                let m = MomProblem::new(&g, 928e6, opts(12.0)).unwrap();
                let mut cells = Vec::new();
                for f in [902e6, 915e6, 928e6] {
                    let pt = m.solve(f, false).unwrap();
                    let net = crate::sparam::Network::from_z(vec![f], std::slice::from_ref(&pt.z), vec![50.0, 50.0]);
                    let active = net.active_reflection(0, &weights)[0];
                    cells.push(20.0 * active.norm().log10());
                }
                let pt = m.solve(915e6, true).unwrap();
                let list: Vec<&FarField> = pt.far.iter().collect();
                let far = crate::project::combine(&list, &weights).unwrap().metrics();
                println!("{h:>5} {l:>6} {xf:>6} | {:>14.1} {:>14.1} {:>14.1} | {:>6.1} {:>5.1} {:>5}", cells[0], cells[1], cells[2], far.peak_directivity_dbi, far.axial_ratio_db, if far.rhcp_at_peak { "RHCP" } else { "LHCP" });
            }
        }
    }
}

/// A shorted patch on metal: where it's inductive enough for a chip.
#[test]
#[ignore]
fn rfid_tune_onmetal() {
    let zc = C64::new(15.0, -231.0);
    println!("{:>5} {:>5} {:>5} {:>5} | {:>18} | {:>5} {:>5} {:>5}", "L", "W", "h", "xf", "Z at 915", "t902", "t915", "t928");
    for l in [45.0, 55.0, 65.0, 72.0, 78.0] {
        for w in [20.0, 30.0] {
            for h in [3.0, 5.0] {
                for xf in [5.0, 10.0, 15.0] {
                    let sets = [("L", mm(l)), ("W", mm(w)), ("h", mm(h)), ("xf", mm(xf)), ("Gx", mm(200.0)), ("Gy", mm(200.0)), ("wp", mm(2.0))];
                    let p = project("pifa", "915MHz", &sets);
                    match impedance(&p, &[902e6, 915e6, 928e6], 16.0) {
                        Ok((z, _)) => {
                            let t: Vec<f64> = z.iter().map(|z| power_transfer(*z, zc)).collect();
                            println!("{l:>5} {w:>5} {h:>5} {xf:>5} | {:>8.1} {:>+8.1}j | {:>5.2} {:>5.2} {:>5.2}", z[1].re, z[1].im, t[0], t[1], t[2]);
                        }
                        Err(e) => println!("{l:>5} {w:>5} {h:>5} {xf:>5} | {e}"),
                    }
                }
            }
        }
    }
}
