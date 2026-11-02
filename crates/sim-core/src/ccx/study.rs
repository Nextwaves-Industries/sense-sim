//! Takes a [`Study`] all the way through CalculiX: mesh, face assignment and its audit, deck, solve,
//! and results in the shared model.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};

use super::deck::{self, Groups, Material, Model, Procedure};
use super::run::{self, Run};
use crate::catalog::AnalysisId;
use crate::geometry::classify::{Assignment, Limits, classify};
use crate::geometry::partition::Partition;
use crate::jobs::JobCtx;
use crate::mesh::gmsh::{self, Options};
use crate::mesh::msh::{SurfaceEntity, TetMesh};
use crate::results::{Field, Location, ResultMesh, ResultSet, ResultStep, StepKind};
use crate::study::{BcKind, Study};

/// A body as FEA needs it: the tagged surface, and the same body as STEP for Gmsh. A body that only
/// exists as a mesh, such as a USD asset, has no STEP: Gmsh then remeshes its surface instead.
#[derive(Debug, Clone)]
pub struct FeaGeometry {
    pub partition: Partition,
    pub step: Vec<u8>,
    /// A second body, in contact with this one. Meshed apart from it: where they touch, two faces lie
    /// on top of each other, and matching faces to one mesh of both could mix them up.
    pub second: Option<Box<FeaGeometry>>,
}

impl FeaGeometry {
    /// The Gmsh script for this body in millimetres, and the file it reads.
    pub fn mesh_script(&self, opts: &Options) -> (String, &'static str, Vec<u8>) {
        if self.step.is_empty() {
            // Split the surface along the same creases its faces came from, so that each Gmsh surface
            // is one face. Second-order points stay on straight edges: the facets are all there is
            // of the shape, and there is no true curve for them to follow.
            let script = format!(
                "Merge \"body.stl\";\nClassifySurfaces{{{} * Pi / 180, 1, 1, Pi}};\nCreateGeometry;\nSurface Loop(1) = Surface{{:}};\nVolume(1) = {{1}};\n{}Mesh.SecondOrderLinear = 1;\nMesh.HighOrderOptimize = 0;\n",
                crate::geometry::regions::CREASE_DEGREES,
                gmsh::settings(opts)
            );
            (script, "body.stl", crate::geometry::regions::to_stl(&self.partition, 1000.0))
        } else {
            (format!("SetFactory(\"OpenCASCADE\");\nGeometry.OCCTargetUnit = \"MM\";\nMesh.MeshSizeFromCurvature = 16;\nMerge \"body.step\";\n{}", gmsh::settings(opts)), "body.step", self.step.clone())
        }
    }
}

pub use crate::check::faces as check_faces;
use crate::check::faces_on as check_faces_on;

/// One body's mesh in metres, and each of its faces' surfaces in that mesh.
fn mesh_body(geometry: &FeaGeometry, study: &Study, dir: &std::path::Path, second: bool, threads: usize, ctx: &JobCtx) -> Result<(TetMesh, HashMap<u32, Vec<i32>>)> {
    let gmsh_exe = crate::tools::gmsh().ok_or_else(|| anyhow!("Gmsh is not installed"))?;
    let part = &geometry.partition;
    let diagonal = diagonal_of(part);
    check_faces_on(study, part, second, diagonal * 1e-3)?;
    std::fs::create_dir_all(dir)?;
    // STEP is in millimetres, and Gmsh works in those; the mesh comes back in metres below.
    let opts = Options { size: (diagonal * study.settings.element_size * 1000.0).max(1e-6), threads };
    let (geo, file, bytes) = geometry.mesh_script(&opts);
    std::fs::write(dir.join(file), &bytes).context("writing the geometry")?;
    let mut mesh = gmsh::mesh_geo(&gmsh_exe, dir, &geo, &ctx.cancel, |l| ctx.log(l))?;
    for n in &mut mesh.nodes {
        for v in n.iter_mut() {
            *v *= 1e-3;
        }
    }
    ctx.log(&format!("{} nodes, {} elements", mesh.nodes.len(), mesh.tets.len()));

    ctx.set("Matching faces to the mesh", 0.2);
    let assignment = classify(&mesh, part, diagonal * 2e-3, Limits::default());
    if !assignment.issues.is_empty() {
        let list: Vec<String> = assignment.issues.iter().take(5).map(|i| i.to_string()).collect();
        bail!("The mesh doesn't match the design's faces, so loads can't be placed reliably:\n{}", list.join("\n"));
    }
    for c in &study.conditions {
        for f in c.faces.iter().filter(|f| f.second == second) {
            if assignment.approximate.contains(&f.face) {
                ctx.log(&format!("{}: face {} shares a surface with its neighbour in the mesh, so its edge follows the elements.", c.kind.label(), f.face));
            }
        }
    }
    let entities = face_entities(&assignment, &mut mesh);
    Ok((mesh, entities))
}

/// Runs `study` on `geometry`, reporting to `ctx`.
pub fn run_study(study: &Study, geometry: &FeaGeometry, ctx: &JobCtx) -> Result<ResultSet> {
    let ccx_exe = crate::tools::ccx().ok_or_else(|| anyhow!("CalculiX is not installed"))?;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 32);

    ctx.set("Meshing", 0.05);
    let (mut mesh, first) = mesh_body(geometry, study, &ctx.dir, false, threads, ctx)?;
    let mut second = HashMap::new();
    if let Some(other) = &geometry.second {
        ctx.set("Meshing the second body", 0.12);
        let (other_mesh, faces) = mesh_body(other, study, &ctx.dir.join("second"), true, threads, ctx)?;
        let offset = mesh.absorb(other_mesh);
        second = faces.into_iter().map(|(f, e)| (f, e.into_iter().map(|t| t + offset).collect())).collect();
    } else if study.conditions.iter().any(|c| c.faces.iter().any(|f| f.second)) {
        bail!("Some faces are on a second body, but the study has none.");
    }

    ctx.set("Solving", 0.25);
    let groups = build_groups(study, &first, &second)?;
    let conditions = conditions_of(study, diagonal_of(&geometry.partition));
    let model = Model { mesh: &mesh, material: material_of(study), groups, conditions, procedure: procedure_of(study), initial_temperature: initial_temperature_of(study), prescribed_temperature: None };
    let text = deck::write(&model)?;
    let kind = match study.analysis {
        AnalysisId::Frequency => StepKind::Mode,
        AnalysisId::Harmonic => StepKind::Frequency,
        _ => StepKind::Time,
    };
    let done = run::solve(&ccx_exe, &ctx.dir, &text, kind, threads, &ctx.cancel, |l| ctx.log(l))?;

    ctx.set("Reading results", 0.95);
    let mut set = to_results(&mesh, done)?;
    name_reactions(&mut set, study);
    Ok(set)
}

/// CalculiX totals a held group's reaction under the group's name, like `C2.fz`. This says what it is.
fn name_reactions(set: &mut ResultSet, study: &Study) {
    for p in &mut set.plots {
        let Some((group, axis)) = p.name.split_once('.') else { continue };
        let Some(i) = group.strip_prefix('C').and_then(|n| n.parse::<usize>().ok()) else { continue };
        let Some(c) = study.conditions.get(i) else { continue };
        let direction = match axis {
            "fx" => "x",
            "fy" => "y",
            "fz" => "z",
            other => other,
        };
        p.name = format!("{} {}, reaction along {direction}", c.kind.label(), i + 1);
    }
}
pub fn diagonal_of(part: &Partition) -> f64 {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt()
}

/// Which mesh surfaces make up each face. A face that is a whole mesh surface uses it; one that shares
/// a surface gets a surface of its own made of its facets.
fn face_entities(a: &Assignment, mesh: &mut TetMesh) -> HashMap<u32, Vec<i32>> {
    let mut face_entities: HashMap<u32, Vec<i32>> = HashMap::new();
    for (entity, face) in &a.whole {
        face_entities.entry(*face).or_default().push(*entity);
    }
    let mut extra: HashMap<u32, Vec<[u32; 6]>> = HashMap::new();
    for (s, facets) in mesh.surfaces.iter().zip(&a.facets) {
        if a.jagged.contains(&s.tag) {
            for (t, f) in s.tris.iter().zip(facets) {
                if let Some(f) = f {
                    extra.entry(*f).or_default().push(*t);
                }
            }
        }
    }
    let mut next = mesh.surfaces.iter().map(|s| s.tag).max().unwrap_or(0) + 1;
    for (face, tris) in extra {
        mesh.surfaces.push(SurfaceEntity { tag: next, tris });
        face_entities.entry(face).or_default().push(next);
        next += 1;
    }
    face_entities
}

/// A face group per condition, named `C0`, `C1`... A contact has two, `C0A` and `C0B`: its faces on
/// each body.
fn build_groups(study: &Study, first: &HashMap<u32, Vec<i32>>, second: &HashMap<u32, Vec<i32>>) -> Result<Groups> {
    let mut groups = Vec::new();
    for (i, c) in study.conditions.iter().enumerate() {
        let entities_of = |on_second: bool| -> Result<Vec<i32>> {
            let mut out = Vec::new();
            for f in c.faces.iter().filter(|f| f.second == on_second) {
                let map = if on_second { second } else { first };
                let list = map.get(&f.face).ok_or_else(|| anyhow!("{}: face {} has no mesh on it", c.kind.label(), f.face))?;
                out.extend(list.iter().copied());
            }
            Ok(out)
        };
        if matches!(c.kind, BcKind::Contact { .. }) {
            groups.push((format!("C{i}A"), entities_of(false)?));
            groups.push((format!("C{i}B"), entities_of(true)?));
        } else {
            let mut all = entities_of(false)?;
            all.extend(entities_of(true)?);
            groups.push((format!("C{i}"), all));
        }
    }
    Ok(Groups(groups))
}

fn conditions_of(study: &Study, diagonal: f64) -> Vec<deck::Condition> {
    let mut out = Vec::new();
    for (i, c) in study.conditions.iter().enumerate() {
        let group = format!("C{i}");
        match &c.kind {
            BcKind::Fixed => out.push(deck::Condition::Fixed { group }),
            BcKind::Force { force } => out.push(deck::Condition::Force { group, force: *force }),
            BcKind::Pressure { pressure } => out.push(deck::Condition::Pressure { group, pressure: *pressure }),
            BcKind::Temperature { value } => out.push(deck::Condition::Temperature { group, value: *value }),
            BcKind::HeatFlux { flux } => out.push(deck::Condition::HeatFlux { group, flux: *flux }),
            BcKind::Convection { coefficient, ambient } => out.push(deck::Condition::Convection { group, coefficient: *coefficient, ambient: *ambient }),
            BcKind::Displacement { displacement } => {
                for (axis, value) in displacement.iter().enumerate() {
                    out.push(deck::Condition::Displacement { group: group.clone(), axis, value: *value });
                }
            }
            BcKind::Contact { friction } => {
                // Ten times stiffer than an element of the size asked for: the benchmark's choice.
                let element = (diagonal * study.settings.element_size).max(1e-9);
                out.push(deck::Condition::Contact { slave: format!("{group}A"), master: format!("{group}B"), slope: 10.0 * study.material.young / element, friction: (*friction > 0.0).then_some(*friction) });
            }
            BcKind::VelocityInlet { .. } | BcKind::PressureOutlet { .. } | BcKind::Wall | BcKind::Potential { .. } => {}
        }
    }
    out
}
fn material_of(study: &Study) -> Material {
    let s = &study.material;
    let thermal = matches!(study.analysis, AnalysisId::HeatTransfer | AnalysisId::Thermomechanical);
    // Plasticity only where it was asked for: an elastic-perfectly-plastic curve from the yield stress.
    let plastic = if study.analysis == AnalysisId::NonlinearMechanical && s.yield_stress > 0.0 { vec![(s.yield_stress, 0.0)] } else { vec![] };
    Material {
        name: s.name.clone(),
        young: s.young,
        poisson: s.poisson,
        density: s.density,
        conductivity: thermal.then_some(s.conductivity),
        specific_heat: thermal.then_some(s.specific_heat),
        expansion: (study.analysis == AnalysisId::Thermomechanical).then_some(s.expansion),
        plastic,
    }
}

fn procedure_of(study: &Study) -> Procedure {
    let t = &study.settings;
    let transient = t.transient.then_some((t.dt, t.duration));
    match study.analysis {
        AnalysisId::Frequency => Procedure::Frequency { modes: t.modes },
        AnalysisId::Dynamic => Procedure::Dynamic { dt: t.dt, duration: t.duration },
        AnalysisId::Harmonic => Procedure::Harmonic { modes: t.modes, f_min: t.f_min, f_max: t.f_max, points: t.points, damping: t.damping },
        AnalysisId::HeatTransfer => Procedure::Heat { transient },
        AnalysisId::Thermomechanical => Procedure::Thermomechanical { transient, reference: t.reference_temperature },
        AnalysisId::NonlinearMechanical => Procedure::Static { nlgeom: true, increment: t.load_increment },
        _ => Procedure::Static { nlgeom: false, increment: 1.0 },
    }
}

fn initial_temperature_of(study: &Study) -> Option<f64> {
    matches!(study.analysis, AnalysisId::HeatTransfer | AnalysisId::Thermomechanical).then_some(study.settings.initial_temperature)
}

/// Turns what CalculiX wrote into the shared model. Second-order facets become four straight
/// triangles, so that drawing uses the values at their mid-side nodes.
pub fn to_results(mesh: &TetMesh, run: Run) -> Result<ResultSet> {
    // Node ids in the deck are one above the mesh's indices, and every node that is on an element
    // comes back; the map also survives a solver leaving out unused ones.
    let at: HashMap<u32, u32> = run.frd.node_ids.iter().enumerate().map(|(i, id)| (*id, i as u32)).collect();
    let map = |n: u32| at.get(&(n + 1)).copied();
    let mut surface = Vec::new();
    for s in &mesh.surfaces {
        for t in &s.tris {
            let Some(v) = t.iter().map(|n| map(*n)).collect::<Option<Vec<u32>>>() else { continue };
            for tri in [[v[0], v[3], v[5]], [v[3], v[1], v[4]], [v[5], v[4], v[2]], [v[3], v[4], v[5]]] {
                surface.push(tri);
            }
        }
    }
    if surface.is_empty() {
        bail!("the results have none of the mesh's surface nodes");
    }
    let tets = mesh.tets.iter().filter_map(|t| Some([map(t[0])?, map(t[1])?, map(t[2])?, map(t[3])?])).collect();
    let mut steps = run.frd.steps;
    for s in &mut steps {
        add_amplitude(s);
    }
    Ok(ResultSet { mesh: ResultMesh { nodes: run.frd.nodes, tets, surface, cut_start: None }, steps, plots: run.totals })
}

/// A harmonic step's real and imaginary parts as one amplitude, the thing to look at.
fn add_amplitude(step: &mut ResultStep) {
    let (Some(re), Some(im)) = (step.field("DISP"), step.field("DISPI")) else { return };
    let (re, im) = (re.clone(), im.clone());
    if re.data.len() != im.data.len() {
        return;
    }
    let data: Vec<f32> = re.data.iter().zip(im.data.iter()).map(|(a, b)| (a * a + b * b).sqrt()).collect();
    step.fields.push(Field { name: "AMPLITUDE".into(), unit: re.unit.clone(), location: Location::Node, components: re.components.clone(), data: Arc::from(data) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::study::Condition;

    fn square() -> Partition {
        Partition { name: "s".into(), positions: vec![[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]], triangles: vec![[0, 1, 2], [0, 2, 3]], tri_face: vec![0, 1], faces: 2 }
    }

    #[test]
    fn plasticity_is_only_for_the_nonlinear_analysis() {
        let mut s = Study::new("t".into(), AnalysisId::Static);
        assert!(material_of(&s).plastic.is_empty());
        s.analysis = AnalysisId::NonlinearMechanical;
        assert_eq!(material_of(&s).plastic, vec![(s.material.yield_stress, 0.0)]);
    }

    #[test]
    fn harmonic_amplitude_comes_from_both_parts() {
        let f = |n: &str, d: &[f32]| Field { name: n.into(), unit: "m".into(), location: Location::Node, components: vec!["D1".into()], data: Arc::from(d) };
        let mut s = ResultStep { kind: StepKind::Frequency, value: 10.0, fields: vec![f("DISP", &[3.0]), f("DISPI", &[4.0])] };
        add_amplitude(&mut s);
        assert_eq!(s.field("AMPLITUDE").unwrap().at(0), &[5.0]);
    }
}
