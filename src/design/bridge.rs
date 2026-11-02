//! Between the design and the rest of Sense Sim: bodies out to the scene as USD, and out to the
//! solvers as a tagged surface and STEP.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use sim_core::ccx::study::FeaGeometry;
use sim_core::geometry::{Partition, read_tagged_3mf};
use sim_core::study::FaceRef;
use solidcraft_engine::io::{Format, MeshQuality, export_with_parts};
use solidcraft_engine::{Sel, Session};

/// The names of the design's bodies.
pub fn bodies(session: &Session) -> Vec<String> {
    session.world_state().bodies.iter().map(|b| b.name.clone()).collect()
}

/// A body's surface, each triangle knowing its face.
pub fn partition(session: &Session, body: &str, quality: MeshQuality) -> Result<Partition> {
    let state = session.doc.painted(&session.world_state());
    let names = [body.to_string()];
    let bytes = export_with_parts(&state, &names, &[], Format::ThreeMfTagged, "body", &session.doc.metadata, quality).map_err(|e| anyhow!("exporting {body}: {e}"))?;
    read_tagged_3mf(&bytes)?.into_iter().next().ok_or_else(|| anyhow!("{body} has no surface"))
}

/// What FEA needs of a body.
pub fn fea_geometry(session: &Session, body: &str) -> Result<FeaGeometry> {
    let state = session.doc.painted(&session.world_state());
    let names = [body.to_string()];
    let step = export_with_parts(&state, &names, &[], Format::Step, "body", &session.doc.metadata, MeshQuality::Fine).map_err(|e| anyhow!("exporting {body} as STEP: {e}"))?;
    Ok(FeaGeometry { partition: partition(session, body, MeshQuality::Fine)?, step, second: None })
}

/// The faces selected in the design, as study references: (body, face). Points come in the design's
/// millimetres and go out in metres, the unit of everything downstream.
pub fn selected_faces(selection: &[Sel]) -> Vec<(String, FaceRef)> {
    selection
        .iter()
        .filter_map(|s| match s {
            Sel::Face { body, index, point } => Some((body.clone(), FaceRef { face: *index as u32, point: [point.x * 1e-3, point.y * 1e-3, point.z * 1e-3], second: false })),
            _ => None,
        })
        .collect()
}

/// A body as a USDA file. Metres and Z up are stated in it, and the loader takes it from there.
pub fn usda_of(part: &Partition, name: &str) -> String {
    let prim: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let prim = if prim.is_empty() || prim.starts_with(|c: char| c.is_ascii_digit()) { format!("B{prim}") } else { prim };
    let mut s = format!("#usda 1.0\n(\n    defaultPrim = \"{prim}\"\n    metersPerUnit = 1\n    upAxis = \"Z\"\n)\n\ndef Xform \"{prim}\" (\n    kind = \"component\"\n)\n{{\n    def Mesh \"Mesh\"\n    {{\n        uniform token subdivisionScheme = \"none\"\n        int[] faceVertexCounts = [");
    let counts = vec!["3"; part.triangles.len()];
    s.push_str(&counts.join(", "));
    s.push_str("]\n        int[] faceVertexIndices = [");
    for (i, t) in part.triangles.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "{}, {}, {}", t[0], t[1], t[2]);
    }
    s.push_str("]\n        point3f[] points = [");
    for (i, p) in part.positions.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "({}, {}, {})", p[0] as f32, p[1] as f32, p[2] as f32);
    }
    s.push_str("]\n    }\n}\n");
    s
}

/// Writes a body's USD where generated assets live, under a name that changes with its shape, so
/// that the scene's undo and saved layouts keep pointing at the shape they had.
pub fn bake(session: &Session, body: &str) -> Result<PathBuf> {
    let part = partition(session, body, MeshQuality::Standard)?;
    let text = usda_of(&part, body);
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from).join("SenseSim").join("design");
    std::fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
    let safe: String = body.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let path = base.join(format!("{safe}_{:016x}.usda", h.finish()));
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
