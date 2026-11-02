//! Reader for legacy ASCII VTK unstructured grids, as `foamToVTK -legacy -ascii` writes them.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};

use crate::results::{Field, Location, ResultMesh};

/// One file: points, cells as (VTK type, nodes), and the fields on them.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    pub points: Vec<[f64; 3]>,
    pub cells: Vec<(u8, Vec<u32>)>,
    /// The simulation time, from the dataset's own `TimeValue`.
    pub time: Option<f64>,
    pub point_fields: Vec<Field>,
    pub cell_fields: Vec<Field>,
}

struct Tokens<'a> {
    it: std::str::SplitAsciiWhitespace<'a>,
}

impl<'a> Tokens<'a> {
    fn next(&mut self) -> Result<&'a str> {
        self.it.next().ok_or_else(|| anyhow!("the file ends early"))
    }
    fn usize(&mut self) -> Result<usize> {
        let t = self.next()?;
        t.parse().with_context(|| format!("expected a count, found {t:?}"))
    }
    fn f32(&mut self) -> Result<f32> {
        let t = self.next()?;
        // Some values come as "nan" or with a Fortran-free exponent; a bad one is an error.
        t.parse().with_context(|| format!("expected a number, found {t:?}"))
    }
}

fn unit_of(name: &str) -> &'static str {
    match name {
        "U" => "m/s",
        "p" | "p_rgh" => "Pa",
        "T" => "K",
        "k" => "m²/s²",
        "epsilon" => "m²/s³",
        "omega" => "1/s",
        "nut" | "nuTilda" => "m²/s",
        "rho" => "kg/m³",
        _ => "",
    }
}

fn labels(name: &str, n: usize) -> Vec<String> {
    match n {
        1 => vec![name.to_string()],
        3 => ["x", "y", "z"].iter().map(|c| format!("{name} {c}")).collect(),
        _ => (0..n).map(|i| format!("{name} {i}")).collect(),
    }
}

/// Reads a `FIELD` block's arrays, or a `SCALARS` / `VECTORS` block.
fn read_arrays(t: &mut Tokens, first: &str, location: Location, out: &mut Vec<Field>, count: usize) -> Result<()> {
    let mut push = |name: &str, comps: usize, data: Vec<f32>| {
        out.push(Field { name: name.to_string(), unit: unit_of(name).to_string(), location, components: labels(name, comps), data: Arc::from(data) });
    };
    match first {
        "FIELD" => {
            let _attrs_name = t.next()?;
            let arrays = t.usize()?;
            for _ in 0..arrays {
                let name = t.next()?.to_string();
                let comps = t.usize()?;
                let tuples = t.usize()?;
                let _ty = t.next()?;
                let mut data = Vec::with_capacity(comps * tuples);
                for _ in 0..comps * tuples {
                    data.push(t.f32()?);
                }
                push(&name, comps, data);
            }
        }
        "SCALARS" => {
            let name = t.next()?.to_string();
            let _ty = t.next()?;
            let mut next = t.next()?;
            let mut comps = 1;
            if next != "LOOKUP_TABLE" {
                comps = next.parse().context("scalar components")?;
                next = t.next()?;
            }
            if next != "LOOKUP_TABLE" {
                bail!("expected LOOKUP_TABLE, found {next}");
            }
            let _table = t.next()?;
            let mut data = Vec::with_capacity(count * comps);
            for _ in 0..count * comps {
                data.push(t.f32()?);
            }
            push(&name, comps, data);
        }
        "VECTORS" => {
            let name = t.next()?.to_string();
            let _ty = t.next()?;
            let mut data = Vec::with_capacity(count * 3);
            for _ in 0..count * 3 {
                data.push(t.f32()?);
            }
            push(&name, 3, data);
        }
        other => bail!("unexpected VTK section {other:?}"),
    }
    Ok(())
}

pub fn parse(text: &str) -> Result<Grid> {
    let mut lines = text.splitn(5, '\n');
    let version = lines.next().unwrap_or("");
    if !version.starts_with("# vtk DataFile") {
        bail!("not a legacy VTK file");
    }
    let _title = lines.next();
    if lines.next().map(str::trim) != Some("ASCII") {
        bail!("only ASCII VTK is read");
    }
    let dataset = lines.next().unwrap_or("").trim();
    if !dataset.ends_with("UNSTRUCTURED_GRID") {
        bail!("expected an unstructured grid, found {dataset:?}");
    }
    let mut t = Tokens { it: lines.next().unwrap_or("").split_ascii_whitespace() };
    let mut grid = Grid::default();
    let mut location = None;
    let mut offsets: Vec<usize> = Vec::new();
    while let Some(word) = t.it.next() {
        match word {
            "POINTS" => {
                let n = t.usize()?;
                let _ty = t.next()?;
                grid.points.reserve(n);
                for _ in 0..n {
                    let p = [t.f32()? as f64, t.f32()? as f64, t.f32()? as f64];
                    grid.points.push(p);
                }
            }
            "METADATA" => {
                // Skipped up to a blank; rarely written.
                while let Some(w) = t.it.next() {
                    if w == "INFORMATION" {
                        break;
                    }
                }
            }
            "CELLS" => {
                let first = t.usize()?;
                let second = t.usize()?;
                // Newer files give OFFSETS and CONNECTIVITY; older, count-prefixed cells.
                let peek = t.next()?;
                if peek == "OFFSETS" {
                    let _ty = t.next()?;
                    offsets = (0..first).map(|_| t.usize()).collect::<Result<_>>()?;
                    let word = t.next()?;
                    if word != "CONNECTIVITY" {
                        bail!("expected CONNECTIVITY, found {word}");
                    }
                    let _ty = t.next()?;
                    let conn: Vec<u32> = (0..second).map(|_| t.usize().map(|v| v as u32)).collect::<Result<_>>()?;
                    for w in offsets.windows(2) {
                        grid.cells.push((0, conn[w[0]..w[1]].to_vec()));
                    }
                } else {
                    let mut size = peek.parse::<usize>().context("a cell's size")?;
                    for i in 0..first {
                        if i > 0 {
                            size = t.usize()?;
                        }
                        let nodes = (0..size).map(|_| t.usize().map(|v| v as u32)).collect::<Result<_>>()?;
                        grid.cells.push((0, nodes));
                    }
                }
            }
            "CELL_TYPES" => {
                let n = t.usize()?;
                if n != grid.cells.len() {
                    bail!("{n} cell types for {} cells", grid.cells.len());
                }
                for c in &mut grid.cells {
                    c.0 = t.usize()? as u8;
                }
            }
            "POINT_DATA" => {
                let _n = t.usize()?;
                location = Some(Location::Node);
            }
            "CELL_DATA" => {
                let _n = t.usize()?;
                location = Some(Location::Cell);
            }
            // Field data before the geometry belongs to the dataset as a whole.
            "FIELD" if location.is_none() => {
                let mut whole = Vec::new();
                read_arrays(&mut t, word, Location::Node, &mut whole, 0)?;
                grid.time = whole.iter().find(|f| f.name == "TimeValue").and_then(|f| f.data.first()).map(|v| f64::from(*v));
            }
            "FIELD" | "SCALARS" | "VECTORS" => {
                let loc = location.ok_or_else(|| anyhow!("field data before POINT_DATA or CELL_DATA"))?;
                let count = if loc == Location::Node { grid.points.len() } else { grid.cells.len() };
                let target = if loc == Location::Node { &mut grid.point_fields } else { &mut grid.cell_fields };
                read_arrays(&mut t, word, loc, target, count)?;
            }
            other => bail!("unexpected VTK section {other:?}"),
        }
    }
    if grid.points.is_empty() {
        bail!("the file has no points");
    }
    let _ = offsets;
    Ok(grid)
}

/// Elmer writes a vector as three scalars, `Magnetic.flux.density.1` to `.3`, and again as `.e.1` to
/// `.e.3` for its per-element copy. This gives one vector per quantity, with a readable lower-case name,
/// and leaves other fields as they are, with their names tidied the same way.
pub fn join_components(fields: &[Field]) -> Vec<Field> {
    let tidy = |name: &str| name.replace('.', " ").to_lowercase();
    let base_of = |name: &str| name.strip_suffix(".1").map(str::to_string);
    let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    for f in fields {
        if used.contains(&f.name) {
            continue;
        }
        if let Some(base) = base_of(&f.name) {
            let second = fields.iter().find(|o| o.name == format!("{base}.2"));
            let third = fields.iter().find(|o| o.name == format!("{base}.3"));
            if let (Some(y), Some(z)) = (second, third)
                && y.len() == f.len()
                && z.len() == f.len()
                && f.stride() == 1
            {
                used.insert(format!("{base}.2"));
                used.insert(format!("{base}.3"));
                // The per-element copy adds nothing to what's drawn at the nodes.
                if base.ends_with(".e") && fields.iter().any(|o| o.name == format!("{}.1", base.trim_end_matches(".e"))) {
                    continue;
                }
                let mut data = Vec::with_capacity(f.len() * 3);
                for i in 0..f.len() {
                    data.extend_from_slice(&[f.data[i], y.data[i], z.data[i]]);
                }
                let name = tidy(&base);
                out.push(Field { components: vec![format!("{name} x"), format!("{name} y"), format!("{name} z")], name, unit: f.unit.clone(), location: f.location, data: std::sync::Arc::from(data) });
                continue;
            }
        }
        out.push(Field { name: tidy(&f.name), ..f.clone() });
    }
    out
}

impl Grid {
    pub fn point_field(&self, name: &str) -> Option<&Field> {
        self.point_fields.iter().find(|f| f.name == name)
    }

    pub fn cell_field(&self, name: &str) -> Option<&Field> {
        self.cell_fields.iter().find(|f| f.name == name)
    }

    /// The middle of each cell.
    pub fn cell_centres(&self) -> Vec<[f64; 3]> {
        self.cells
            .iter()
            .map(|(_, nodes)| {
                let mut c = [0.0; 3];
                for n in nodes {
                    let p = self.points[*n as usize];
                    for a in 0..3 {
                        c[a] += p[a];
                    }
                }
                c.map(|v| v / nodes.len().max(1) as f64)
            })
            .collect()
    }

    /// The grid as linear tetrahedra and its outer surface, for drawing: faces that belong to only
    /// one cell.
    pub fn to_result_mesh(&self) -> ResultMesh {
        // Each cell's corner tetrahedra and faces, by VTK type.
        let mut tets = Vec::new();
        let mut faces: std::collections::HashMap<Vec<u32>, (Vec<u32>, u32)> = std::collections::HashMap::new();
        let mut add_face = |f: &[u32]| {
            let mut key = f.to_vec();
            key.sort_unstable();
            faces.entry(key).or_insert_with(|| (f.to_vec(), 0)).1 += 1;
        };
        for (kind, n) in &self.cells {
            match (*kind, n.len()) {
                (10, 4) => {
                    tets.push([n[0], n[1], n[2], n[3]]);
                    for f in [[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]] {
                        add_face(&[n[f[0]], n[f[1]], n[f[2]]]);
                    }
                }
                (14, 5) => {
                    tets.push([n[0], n[1], n[2], n[4]]);
                    tets.push([n[0], n[2], n[3], n[4]]);
                    add_face(&[n[0], n[3], n[2], n[1]]);
                    for f in [[0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]] {
                        add_face(&[n[f[0]], n[f[1]], n[f[2]]]);
                    }
                }
                (13, 6) => {
                    tets.push([n[0], n[1], n[2], n[3]]);
                    tets.push([n[1], n[2], n[3], n[4]]);
                    tets.push([n[2], n[3], n[4], n[5]]);
                    add_face(&[n[0], n[2], n[1]]);
                    add_face(&[n[3], n[4], n[5]]);
                    for f in [[0, 1, 4, 3], [1, 2, 5, 4], [2, 0, 3, 5]] {
                        add_face(&[n[f[0]], n[f[1]], n[f[2]], n[f[3]]]);
                    }
                }
                (12, 8) => {
                    for t in [[0, 1, 3, 4], [1, 2, 3, 6], [1, 3, 4, 6], [1, 4, 5, 6], [3, 4, 6, 7]] {
                        tets.push([n[t[0]], n[t[1]], n[t[2]], n[t[3]]]);
                    }
                    for f in [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]] {
                        add_face(&[n[f[0]], n[f[1]], n[f[2]], n[f[3]]]);
                    }
                }
                // Anything else (polyhedra foamToVTK couldn't decompose) is left out of the drawing.
                _ => {}
            }
        }
        let mut surface = Vec::new();
        for (f, count) in faces.values() {
            if *count == 1 {
                for i in 1..f.len() - 1 {
                    surface.push([f[0], f[i], f[i + 1]]);
                }
            }
        }
        ResultMesh { nodes: self.points.clone(), tets, surface, cut_start: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# vtk DataFile Version 3.0\ncase\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 5 float\n0 0 0 1 0 0 0 1 0 0 0 1 1 1 1\nCELLS 2 10\n4 0 1 2 3\n4 1 2 3 4\nCELL_TYPES 2\n10 10\nCELL_DATA 2\nFIELD attributes 1\np 1 2 float\n1.5 -2.5\nPOINT_DATA 5\nFIELD attributes 1\nU 3 5 float\n1 0 0 2 0 0 3 0 0 4 0 0 5 0 0\n";

    #[test]
    fn reads_points_cells_and_fields() {
        let g = parse(SAMPLE).unwrap();
        assert_eq!(g.points.len(), 5);
        assert_eq!(g.cells, vec![(10, vec![0, 1, 2, 3]), (10, vec![1, 2, 3, 4])]);
        let p = g.cell_field("p").unwrap();
        assert_eq!(&*p.data, &[1.5, -2.5]);
        assert_eq!(p.unit, "Pa");
        assert_eq!(g.point_field("U").unwrap().at(2), &[3.0, 0.0, 0.0]);
    }

    #[test]
    fn a_dataset_level_time_is_read_as_the_time() {
        // As foamToVTK v2512 writes it, ahead of the points.
        let text = SAMPLE.replace("POINTS 5 float", "FIELD FieldData 1\nTimeValue 1 1 float\n1474\n\nPOINTS 5 float");
        let g = parse(&text).unwrap();
        assert_eq!(g.time, Some(1474.0));
        assert_eq!(g.points.len(), 5);
        assert!(g.cell_field("p").is_some());
    }

    #[test]
    fn the_outer_surface_excludes_shared_faces() {
        let m = parse(SAMPLE).unwrap().to_result_mesh();
        assert_eq!(m.tets.len(), 2);
        // Two tetrahedra sharing a face: 8 faces less the shared one, counted on both.
        assert_eq!(m.surface.len(), 6);
    }

    #[test]
    fn a_hex_is_five_tets_and_six_quads() {
        let text = "# vtk DataFile Version 3.0\nc\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 8 float\n0 0 0 1 0 0 1 1 0 0 1 0 0 0 1 1 0 1 1 1 1 0 1 1\nCELLS 1 9\n8 0 1 2 3 4 5 6 7\nCELL_TYPES 1\n12\n";
        let g = parse(text).unwrap();
        let m = g.to_result_mesh();
        assert_eq!(m.tets.len(), 5);
        assert_eq!(m.surface.len(), 12);
        // The tetrahedra fill the cube exactly.
        let volume: f64 = m.tets.iter().map(|t| {
            let (a, b, c, d) = (g.points[t[0] as usize], g.points[t[1] as usize], g.points[t[2] as usize], g.points[t[3] as usize]);
            let (u, v, w) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]], [d[0] - a[0], d[1] - a[1], d[2] - a[2]]);
            (u[0] * (v[1] * w[2] - v[2] * w[1]) - u[1] * (v[0] * w[2] - v[2] * w[0]) + u[2] * (v[0] * w[1] - v[1] * w[0])).abs() / 6.0
        }).sum();
        assert!((volume - 1.0).abs() < 1e-12, "{volume}");
    }

    #[test]
    fn elmers_separate_components_join_into_vectors() {
        let scalar = |name: &str, v: f32| Field { name: name.into(), unit: String::new(), location: Location::Node, components: vec![name.into()], data: std::sync::Arc::from(vec![v, v + 1.0]) };
        let joined = join_components(&[
            scalar("Potential", 7.0),
            scalar("Magnetic.flux.density.1", 1.0),
            scalar("Magnetic.flux.density.2", 2.0),
            scalar("Magnetic.flux.density.3", 3.0),
            scalar("Magnetic.flux.density.e.1", 9.0),
            scalar("Magnetic.flux.density.e.2", 9.0),
            scalar("Magnetic.flux.density.e.3", 9.0),
            scalar("Lonely.1", 4.0),
        ]);
        let names: Vec<&str> = joined.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["potential", "magnetic flux density", "lonely 1"]);
        let b = &joined[1];
        assert_eq!(b.stride(), 3);
        assert_eq!(b.at(0), &[1.0, 2.0, 3.0]);
        assert_eq!(b.at(1), &[2.0, 3.0, 4.0]);
        // The other ones keep their single values.
        assert_eq!(joined[0].stride(), 1);
        assert_eq!(joined[2].len(), 2);
    }

    #[test]
    fn broken_files_are_errors() {
        assert!(parse("hello").is_err());
        assert!(parse(&SAMPLE.replace("ASCII", "BINARY")).is_err());
        assert!(parse(&SAMPLE.replace("5 float\n0 0 0 1 0 0", "5 float\n0 0")).is_err());
    }
}
