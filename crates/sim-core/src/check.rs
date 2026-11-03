//! What a study's picks really land on, the same for every solver. A condition on the wrong face gives
//! a plausible, wrong answer with no error, so every route checks them before it starts, and the user
//! can see them, coloured and measured, before running at all.

use anyhow::{Result, bail};

use crate::geometry::classify::distance_to_triangle;
use crate::geometry::partition::{Partition, tri_area};
use crate::results::{Field, Location, ResultMesh, ResultSet, ResultStep, StepKind};
use crate::study::Study;

/// How far a picked point may lie from its face, as a share of the body's size: curved faces are
/// facets here, and a point picked on the true surface sits a little off them.
const POINT_TOLERANCE: f64 = 1e-3;

/// The name of the field the check draws.
pub const FIELD: &str = "Condition";

fn diagonal(part: &Partition) -> f64 {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt()
}

/// Is each picked face on this body still where it was picked? Editing a design can renumber faces,
/// and a load on the wrong face would look like any other result. This stops that, with no guessing.
pub fn faces_on(study: &Study, part: &Partition, second: bool, tolerance: f64) -> Result<()> {
    for c in &study.conditions {
        for f in c.faces.iter().filter(|f| f.second == second) {
            let mut nearest = f64::INFINITY;
            for (i, tag) in part.tri_face.iter().enumerate() {
                if *tag == f.face {
                    let [a, b, d] = part.triangle(i);
                    nearest = nearest.min(distance_to_triangle(f.point, a, b, d));
                }
            }
            if nearest > tolerance {
                bail!("{}: a picked face has moved or changed since it was picked. The object was changed or moved: pick the face again.", c.kind.label());
            }
        }
    }
    Ok(())
}

/// [`faces_on`] for a study's first body.
pub fn faces(study: &Study, part: &Partition, tolerance: f64) -> Result<()> {
    faces_on(study, part, false, tolerance)
}

/// [`faces`] at the tolerance every solver route uses.
pub fn before_run(study: &Study, part: &Partition) -> Result<()> {
    faces(study, part, diagonal(part) * POINT_TOLERANCE)
}

/// What one condition covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub label: String,
    pub faces: usize,
    /// m2
    pub area: f64,
    /// Its value in [`FIELD`]: what picks its colour.
    pub value: f32,
}

/// The study's body, coloured by condition, with what each one covers. Stops if a pick has gone stale.
/// A face under two conditions is drawn with the later one and counted in both.
pub fn preview(study: &Study, part: &Partition, second: Option<&Partition>) -> Result<(ResultSet, Vec<Coverage>)> {
    before_run(study, part)?;
    if let Some(other) = second {
        faces_on(study, other, true, diagonal(other) * POINT_TOLERANCE)?;
    }
    let mut rows: Vec<Coverage> = study.conditions.iter().enumerate().map(|(i, c)| Coverage { label: c.kind.label().to_string(), faces: c.faces.len(), area: 0.0, value: (i + 1) as f32 }).collect();
    let mut rest = Coverage { label: "Other faces".to_string(), faces: 0, area: 0.0, value: 0.0 };
    let (mut nodes, mut surface, mut values) = (Vec::new(), Vec::new(), Vec::new());
    for (body, on_second) in std::iter::once((part, false)).chain(second.map(|o| (o, true))) {
        let mut value_of = vec![0.0f32; body.faces as usize];
        let mut picked = vec![false; body.faces as usize];
        for (i, c) in study.conditions.iter().enumerate() {
            for f in c.faces.iter().filter(|f| f.second == on_second) {
                if let Some(slot) = value_of.get_mut(f.face as usize) {
                    *slot = (i + 1) as f32;
                    picked[f.face as usize] = true;
                }
            }
        }
        rest.faces += picked.iter().filter(|p| !**p).count();
        for (t, face) in body.tri_face.iter().enumerate() {
            let [a, b, c] = body.triangle(t);
            let area = tri_area(a, b, c);
            for (row, condition) in rows.iter_mut().zip(&study.conditions) {
                if condition.faces.iter().any(|f| f.second == on_second && f.face == *face) {
                    row.area += area;
                }
            }
            if !picked.get(*face as usize).copied().unwrap_or(false) {
                rest.area += area;
            }
            // Three nodes of its own: a face's colour stops at its edge instead of blending across.
            let first = nodes.len() as u32;
            nodes.extend([a, b, c]);
            surface.push([first, first + 1, first + 2]);
            let v = value_of.get(*face as usize).copied().unwrap_or(0.0);
            values.extend([v, v, v]);
        }
    }
    rows.push(rest);
    let field = Field { name: FIELD.to_string(), unit: String::new(), location: Location::Node, components: vec![FIELD.to_string()], data: values.into() };
    let set = ResultSet { mesh: ResultMesh { nodes, tets: Vec::new(), surface, cut_start: None }, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields: vec![field] }], plots: Vec::new() };
    Ok((set, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AnalysisId;
    use crate::study::{BcKind, Condition, FaceRef};

    /// A unit square split into two faces, one triangle each, of half a square metre.
    fn square() -> Partition {
        Partition { name: "s".into(), positions: vec![[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]], triangles: vec![[0, 1, 2], [0, 2, 3]], tri_face: vec![0, 1], faces: 2 }
    }

    fn on(face: u32, point: [f64; 3]) -> FaceRef {
        FaceRef { face, point, second: false }
    }

    #[test]
    fn a_picked_face_that_moved_stops_it() {
        let mut study = Study::new("t".into(), AnalysisId::Static);
        study.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![on(0, [0.75, 0.25, 0.0])] });
        assert!(before_run(&study, &square()).is_ok());
        // The same number, now somewhere else.
        study.conditions[0].faces[0].point = [0.25, 0.75, 0.0];
        let e = before_run(&study, &square()).unwrap_err().to_string();
        assert!(e.contains("pick the face again"), "{e}");
        // A number that no longer exists.
        study.conditions[0].faces[0] = on(9, [0.5, 0.5, 0.0]);
        assert!(before_run(&study, &square()).is_err());
    }

    #[test]
    fn the_check_colours_and_measures_each_condition() {
        let mut study = Study::new("t".into(), AnalysisId::Static);
        study.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![on(1, [0.25, 0.75, 0.0])] });
        let (set, rows) = preview(&study, &square(), None).unwrap();
        assert_eq!(rows[0], Coverage { label: "Fixed support".into(), faces: 1, area: 0.5, value: 1.0 });
        assert_eq!(rows[1], Coverage { label: "Other faces".into(), faces: 1, area: 0.5, value: 0.0 });
        let field = set.steps[0].field(FIELD).unwrap();
        assert_eq!(set.mesh.nodes.len(), 6, "faces don't share nodes, so colours don't blend");
        assert_eq!(field.len(), 6);
        assert_eq!(&field.data[..], &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0], "face 0 is the first triangle, and has no condition");
    }

    #[test]
    fn the_check_refuses_a_stale_pick() {
        let mut study = Study::new("t".into(), AnalysisId::Static);
        study.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![on(1, [0.75, 0.25, 0.0])] });
        assert!(preview(&study, &square(), None).is_err());
    }

    #[test]
    fn the_second_body_has_its_own_faces() {
        let mut study = Study::new("t".into(), AnalysisId::NonlinearMechanical);
        study.conditions.push(Condition { kind: BcKind::Contact { friction: 0.0 }, faces: vec![on(0, [0.75, 0.25, 0.0]), FaceRef { face: 1, point: [0.25, 0.75, 0.0], second: true }] });
        let (set, rows) = preview(&study, &square(), Some(&square())).unwrap();
        assert!((rows[0].area - 1.0).abs() < 1e-12, "half a square metre on each body: {rows:?}");
        assert!((rows[1].area - 1.0).abs() < 1e-12, "and the same left over: {rows:?}");
        assert_eq!(&set.steps[0].field(FIELD).unwrap().data[..], &[1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
    }
}
