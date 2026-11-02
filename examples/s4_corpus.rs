//! Spike S4: real CAD bodies through the whole FEA path. Each body of SolidCraft's test corpus goes
//! through tagged 3MF + STEP export, Gmsh, face classification, a CalculiX deck and a solve.
//!
//!     cargo run --release --example s4_corpus --features design -- CORPUS_DIR

use std::path::Path;

use sim_core::ccx::deck::{self, Condition, Groups, Material, Model, Procedure};
use sim_core::ccx::run;
use sim_core::geometry::{Limits, Partition, classify, read_tagged_3mf};
use sim_core::jobs::Cancel;
use sim_core::mesh::gmsh::{self, Options};
use sim_core::results::StepKind;
use solidcraft_engine::io::{Format, MeshQuality, export_with_parts};

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "third_party/solidcraft/apps/solidcraft-cli/tests/corpus".into());
    let mut files: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "solidcraft")).collect();
    files.sort();
    let work = std::env::temp_dir().join("sense-sim-s4");
    let _ = std::fs::remove_dir_all(&work);

    println!("{:<34} {:>6} {:>6} {:>5} {:>6} {:>6} {:>8} {:>7} {:>10}  result", "body", "tris", "tets", "faces", "whole", "jagged", "area%", "issues", "max disp m");
    let (mut clean, mut total, mut solved, mut blocked) = (0, 0, 0, 0);
    for file in &files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(file)?;
        let doc = match solidcraft_engine::io::read_design(&bytes) {
            Ok(d) => d,
            Err(e) => {
                println!("{stem}: could not open: {e}");
                continue;
            }
        };
        let session = solidcraft_engine::Session::opened_at(doc, None, Some(&file.to_string_lossy()));
        let state = session.doc.painted(&session.world_state());
        let names: Vec<String> = state.bodies.iter().map(|b| b.name.clone()).collect();
        for name in names {
            total += 1;
            let id = format!("{stem}/{name}");
            let dir = work.join(format!("{stem}-{total}"));
            std::fs::create_dir_all(&dir)?;
            let meta = solidcraft_engine::doc::Metadata::default();
            match one_body(&state, &name, &meta, &dir) {
                Ok(r) => {
                    let ok = r.issues.is_empty();
                    if ok {
                        clean += 1;
                    } else {
                        blocked += 1;
                    }
                    if r.solved.is_some() {
                        solved += 1;
                    }
                    println!(
                        "{:<34} {:>6} {:>6} {:>5} {:>6} {:>6} {:>8.3} {:>7} {:>10}  {}",
                        trim(&id),
                        r.tris,
                        r.tets,
                        r.faces,
                        r.whole,
                        r.jagged,
                        r.worst_area * 100.0,
                        r.issues.len(),
                        r.solved.map_or("-".to_string(), |d| format!("{d:.3e}")),
                        if ok { "clean".to_string() } else { format!("BLOCKED: {}", r.issues.first().cloned().unwrap_or_default()) }
                    );
                }
                Err(e) => {
                    blocked += 1;
                    println!("{:<34} failed: {}", trim(&id), format!("{e:#}").lines().next().unwrap_or(""));
                }
            }
        }
    }
    println!("\n{total} bodies: {clean} clean, {blocked} blocked or failed, {solved} solved by CalculiX");
    Ok(())
}

fn trim(s: &str) -> String {
    if s.len() > 34 { format!("...{}", &s[s.len() - 31..]) } else { s.to_string() }
}

struct Report {
    tris: usize,
    tets: usize,
    faces: u32,
    whole: usize,
    jagged: usize,
    worst_area: f64,
    issues: Vec<String>,
    solved: Option<f64>,
}

fn one_body(state: &solidcraft_engine::doc::ModelState, name: &str, meta: &solidcraft_engine::doc::Metadata, dir: &Path) -> anyhow::Result<Report> {
    let bodies = [name.to_string()];
    let tagged = export_with_parts(state, &bodies, &[], Format::ThreeMfTagged, "body", meta, MeshQuality::Fine).map_err(|e| anyhow::anyhow!("3MF: {e}"))?;
    let part: Partition = read_tagged_3mf(&tagged)?.remove(0);
    let step = export_with_parts(state, &bodies, &[], Format::Step, "body", meta, MeshQuality::Fine).map_err(|e| anyhow::anyhow!("STEP: {e}"))?;
    std::fs::write(dir.join("body.step"), step)?;

    // A mesh about 12 elements across the smallest side, in metres.
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diagonal = (size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt();
    let opts = Options { size: (diagonal / 25.0) * 1000.0, threads: 16 };
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\nGeometry.OCCTargetUnit = \"MM\";\nMesh.MeshSizeFromCurvature = 16;\nMerge \"body.step\";\n{}",
        gmsh::settings(&opts)
    );
    let mut mesh = gmsh::mesh_geo(&sim_core::tools::gmsh().ok_or_else(|| anyhow::anyhow!("no Gmsh"))?, dir, &geo, &Cancel::default(), |_| {})?;
    for n in &mut mesh.nodes {
        for v in n.iter_mut() {
            *v *= 1e-3;
        }
    }

    let a = classify(&mesh, &part, diagonal * 2e-3, Limits::default());
    // Negative control: the same mesh against the surface moved by 1% of the body's size, and with
    // two faces' tags swapped. Each must either block, or (for the swap) follow the tags; a clean
    // pass of the moved surface would mean the audit checks nothing.
    let mut moved = part.clone();
    for p in &mut moved.positions {
        p[0] += diagonal * 0.01;
        p[1] += diagonal * 0.01;
        p[2] += diagonal * 0.01;
    }
    let control = classify(&mesh, &moved, diagonal * 2e-3, Limits::default());
    assert!(!control.issues.is_empty(), "the audit passed a surface moved by 1%: it checks nothing");
    if part.faces >= 2 {
        let mut swapped = part.clone();
        for f in &mut swapped.tri_face {
            *f = match *f { 0 => 1, 1 => 0, other => other };
        }
        let c = classify(&mesh, &swapped, diagonal * 2e-3, Limits::default());
        for (entity, face) in &a.whole {
            let expected = match *face { 0 => 1, 1 => 0, other => other };
            assert_eq!(c.whole.get(entity), Some(&expected), "labels must follow the tags exactly");
        }
    }
    let areas = part.face_areas();
    let worst_area = areas.iter().zip(&a.meshed_area).filter(|(t, _)| **t > 0.0).map(|(t, m)| (m - t).abs() / t).fold(0.0, f64::max);
    let mut report = Report { tris: part.triangles.len(), tets: mesh.tets.len(), faces: part.faces, whole: a.whole.len(), jagged: a.jagged.len(), worst_area, issues: a.issues.iter().map(|i| i.to_string()).collect(), solved: None };
    if !report.issues.is_empty() {
        return Ok(report);
    }

    // A solve: the two largest whole faces, one held and one pulled.
    let mut by_area: Vec<(i32, f64)> = a.whole.iter().map(|(e, f)| (*e, areas[*f as usize])).collect();
    by_area.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    if by_area.len() < 2 {
        return Ok(report);
    }
    let groups = Groups(vec![("HELD".into(), vec![by_area[0].0]), ("PULLED".into(), vec![by_area[1].0])]);
    let model = Model {
        mesh: &mesh,
        material: Material { name: "steel".into(), young: 210e9, poisson: 0.3, density: 7850.0, conductivity: None, specific_heat: None, expansion: None, plastic: vec![] },
        groups,
        conditions: vec![Condition::Fixed { group: "HELD".into() }, Condition::Force { group: "PULLED".into(), force: [10.0, 10.0, 10.0] }],
        procedure: Procedure::Static { nlgeom: false, increment: 1.0 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let text = deck::write(&model)?;
    let r = run::solve(&sim_core::tools::ccx().ok_or_else(|| anyhow::anyhow!("no CalculiX"))?, dir, &text, StepKind::Time, 16, &Cancel::default(), |_| {})?;
    let last = r.frd.steps.last().ok_or_else(|| anyhow::anyhow!("no results"))?;
    let disp = last.field("DISP").ok_or_else(|| anyhow::anyhow!("no displacement"))?;
    report.solved = disp.range().map(|(_, max)| f64::from(max));
    Ok(report)
}
