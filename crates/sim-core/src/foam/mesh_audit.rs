//! Reads what a mesher actually made: each patch's area from the mesh's own files. A snappyHexMesh
//! run can leak into the wrong region or drop a patch and still report success, and a study would
//! then run on the wrong domain, so these areas are compared with the geometry before solving.

use anyhow::{Context, Result, anyhow};

/// A patch of the mesh, with its size.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub name: String,
    pub faces: usize,
    pub area: f64,
}

fn skip_header(text: &str) -> &str {
    // Past the FoamFile block, and any banner comment.
    let start = text.find("FoamFile").map_or(0, |i| text[i..].find('}').map_or(i, |j| i + j + 1));
    &text[start..]
}

/// `points` file: `n ( (x y z) ... )`.
pub fn parse_points(text: &str) -> Result<Vec<[f64; 3]>> {
    let body = skip_header(text);
    let open = body.find('(').ok_or_else(|| anyhow!("no list in the points file"))?;
    let inner = &body[open + 1..];
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(a) = rest.find('(') {
        let b = rest[a..].find(')').ok_or_else(|| anyhow!("unterminated point"))? + a;
        let mut it = rest[a + 1..b].split_ascii_whitespace().map(str::parse::<f64>);
        let mut next = || it.next().ok_or_else(|| anyhow!("a point has too few values")).and_then(|v| v.context("bad coordinate"));
        out.push([next()?, next()?, next()?]);
        rest = &rest[b + 1..];
    }
    Ok(out)
}

/// `faces` file: `n ( 4(a b c d) ... )`.
pub fn parse_faces(text: &str) -> Result<Vec<Vec<u32>>> {
    let body = skip_header(text);
    let open = body.find('(').ok_or_else(|| anyhow!("no list in the faces file"))?;
    let inner = &body[open + 1..];
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(a) = rest.find('(') {
        let b = rest[a..].find(')').ok_or_else(|| anyhow!("unterminated face"))? + a;
        let f: Vec<u32> = rest[a + 1..b].split_ascii_whitespace().map(|v| v.parse().map_err(|_| anyhow!("bad point index {v:?}"))).collect::<Result<_>>()?;
        out.push(f);
        rest = &rest[b + 1..];
    }
    Ok(out)
}

/// `boundary` file: patch names with their `nFaces` and `startFace`.
pub fn parse_boundary(text: &str) -> Result<Vec<(String, usize, usize)>> {
    let body = skip_header(text);
    let open = body.find('(').ok_or_else(|| anyhow!("no list in the boundary file"))?;
    let inner = &body[open + 1..];
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(brace) = rest.find('{') {
        let name = rest[..brace].split_ascii_whitespace().next_back().ok_or_else(|| anyhow!("a patch has no name"))?.to_string();
        let end = rest[brace..].find('}').ok_or_else(|| anyhow!("unterminated patch"))? + brace;
        let dict = &rest[brace + 1..end];
        let value = |key: &str| -> Result<usize> {
            let at = dict.find(key).ok_or_else(|| anyhow!("patch {name} has no {key}"))?;
            let tail = &dict[at + key.len()..];
            tail.split(';').next().unwrap_or("").trim().parse().map_err(|_| anyhow!("patch {name}: bad {key}"))
        };
        out.push((name.clone(), value("nFaces")?, value("startFace")?));
        rest = &rest[end + 1..];
    }
    Ok(out)
}

/// Every patch's area.
pub fn patches(points: &str, faces: &str, boundary: &str) -> Result<Vec<Patch>> {
    let points = parse_points(points).context("reading the mesh's points")?;
    let faces = parse_faces(faces).context("reading the mesh's faces")?;
    let mut out = Vec::new();
    for (name, count, start) in parse_boundary(boundary).context("reading the mesh's patches")? {
        let mut area = 0.0;
        for f in faces.iter().skip(start).take(count) {
            area += polygon_area(&points, f);
        }
        out.push(Patch { name, faces: count, area });
    }
    Ok(out)
}

fn polygon_area(points: &[[f64; 3]], face: &[u32]) -> f64 {
    let mut n = [0.0f64; 3];
    if face.len() < 3 {
        return 0.0;
    }
    let p0 = points[face[0] as usize];
    for w in face[1..].windows(2) {
        let (a, b) = (points[w[0] as usize], points[w[1] as usize]);
        let (u, v) = ([a[0] - p0[0], a[1] - p0[1], a[2] - p0[2]], [b[0] - p0[0], b[1] - p0[1], b[2] - p0[2]]);
        n[0] += u[1] * v[2] - u[2] * v[1];
        n[1] += u[2] * v[0] - u[0] * v[2];
        n[2] += u[0] * v[1] - u[1] * v[0];
    }
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

/// Compares the patches a mesh has with the areas expected of them, and looks for leftovers. Returns
/// what's wrong: empty means fine. `tolerance` is a share, since hex-dominant meshes round corners.
pub fn audit(patches: &[Patch], expected: &[(String, f64)], tolerance: f64) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(p) = patches.iter().find(|p| p.name == "defaultFaces" && p.faces > 0) {
        problems.push(format!("The mesh leaked: {} faces belong to no patch. Is the flow domain closed?", p.faces));
    }
    for (name, area) in expected {
        match patches.iter().find(|p| &p.name == name) {
            None => problems.push(format!("Patch {name} is missing from the mesh.")),
            Some(p) if p.faces == 0 => problems.push(format!("Patch {name} has no faces: the mesh didn't reach it.")),
            Some(p) if (p.area - area).abs() / area > tolerance => problems.push(format!("Patch {name} covers {:.4e} m² of its {area:.4e} m². The mesh may be too coarse for it, or the domain may have leaked.", p.area)),
            _ => {}
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTS: &str = "FoamFile { class vectorField; }\n4\n(\n(0 0 0)\n(1 0 0)\n(1 2 0)\n(0 2 0)\n)\n";
    const FACES: &str = "FoamFile { class faceList; }\n2\n(\n4(0 1 2 3)\n3(0 1 2)\n)\n";
    const BOUNDARY: &str = "FoamFile { class polyBoundaryMesh; }\n2\n(\n    inlet\n    {\n        type patch;\n        nFaces 1;\n        startFace 0;\n    }\n    walls\n    {\n        type wall;\n        inGroups 1(wall);\n        nFaces 1;\n        startFace 1;\n    }\n)\n";

    #[test]
    fn reads_patch_areas_from_the_mesh_files() {
        let p = patches(POINTS, FACES, BOUNDARY).unwrap();
        assert_eq!(p[0], Patch { name: "inlet".into(), faces: 1, area: 2.0 });
        assert_eq!(p[1].name, "walls");
        assert!((p[1].area - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_leak_or_a_lost_patch_is_caught() {
        let good = vec![Patch { name: "walls".into(), faces: 10, area: 2.0 }];
        assert!(audit(&good, &[("walls".into(), 2.02)], 0.05).is_empty());
        // Twice the expected area: the mesh has gone round the outside as well.
        assert!(audit(&good, &[("walls".into(), 1.0)], 0.05)[0].contains("covers"));
        assert!(audit(&good, &[("inlet".into(), 1.0)], 0.05)[0].contains("missing"));
        let leak = vec![Patch { name: "defaultFaces".into(), faces: 40, area: 3.0 }];
        assert!(audit(&leak, &[], 0.05)[0].contains("leaked"));
        let empty_default = vec![Patch { name: "defaultFaces".into(), faces: 0, area: 0.0 }];
        assert!(audit(&empty_default, &[], 0.05).is_empty(), "an empty leftover patch is normal");
        let empty = vec![Patch { name: "walls".into(), faces: 0, area: 0.0 }];
        assert!(audit(&empty, &[("walls".into(), 1.0)], 0.05)[0].contains("no faces"));
    }

    #[test]
    fn broken_files_are_errors() {
        assert!(parse_points("nothing").is_err());
        assert!(parse_faces("FoamFile { }\n1\n(\n4(0 1").is_err());
        assert!(parse_boundary("FoamFile { }\n(\n a { type patch; }\n)").is_err());
    }
}
