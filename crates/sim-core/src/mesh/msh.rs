//! Reader for Gmsh's `.msh` 4.1 ASCII format: what the FEA backends need (nodes, second-order
//! tetrahedra, and the second-order triangles of each CAD surface entity).

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};

/// Gmsh element types this reader keeps.
const TRI6: i32 = 9;
const TET10: i32 = 11;
const TRI3: i32 = 2;
const TET4: i32 = 4;

/// A surface of the mesh: a CAD entity, or a discrete surface per tag (Tier B).
#[derive(Debug, Clone, Default)]
pub struct SurfaceEntity {
    pub tag: i32,
    pub tris: Vec<[u32; 6]>,
}

#[derive(Debug, Clone, Default)]
pub struct TetMesh {
    /// Node positions, in the file's units.
    pub nodes: Vec<[f64; 3]>,
    /// In **Gmsh order**: corners 0-3, then edges 01, 12, 20, 30, 23, 31.
    pub tets: Vec<[u32; 10]>,
    /// The volume entity each tetrahedron belongs to.
    pub tet_volume: Vec<i32>,
    pub surfaces: Vec<SurfaceEntity>,
    /// Physical groups: (dimension, tag) to name.
    pub physical_names: HashMap<(i32, i32), String>,
    /// Entities in each physical group: (dimension, physical tag) to entity tags.
    pub physical_entities: HashMap<(i32, i32), Vec<i32>>,
}

impl TetMesh {
    /// Adds another mesh's nodes, elements and surfaces to this one, as a separate body. Its surfaces'
    /// tags go up by the number returned, so that they stay different.
    pub fn absorb(&mut self, other: TetMesh) -> i32 {
        let nodes = self.nodes.len() as u32;
        let tags = self.surfaces.iter().map(|s| s.tag).max().unwrap_or(0);
        let volumes = self.tet_volume.iter().copied().max().unwrap_or(0);
        self.nodes.extend(other.nodes);
        self.tets.extend(other.tets.iter().map(|t| t.map(|n| n + nodes)));
        self.tet_volume.extend(other.tet_volume.iter().map(|v| v + volumes));
        self.surfaces.extend(other.surfaces.into_iter().map(|s| SurfaceEntity { tag: s.tag + tags, tris: s.tris.iter().map(|t| t.map(|n| n + nodes)).collect() }));
        tags
    }

    /// The midpoint of edge `(a, b)`, for tests of node order.
    pub fn midpoint(&self, a: u32, b: u32) -> [f64; 3] {
        let (p, q) = (self.nodes[a as usize], self.nodes[b as usize]);
        [(p[0] + q[0]) / 2.0, (p[1] + q[1]) / 2.0, (p[2] + q[2]) / 2.0]
    }

    /// How many tetrahedra have a Jacobian at or below zero anywhere the solver will evaluate it
    /// (the integration points of a 10-node element and its corners), or that are flat. A solver
    /// stops on these with little explanation, so they're found here first.
    pub fn inverted_tets(&self) -> usize {
        const A: f64 = 0.138196601125011;
        const B: f64 = 0.585410196624969;
        let points: [[f64; 3]; 8] = [[A, A, A], [B, A, A], [A, B, A], [A, A, B], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        self.tets.iter().filter(|t| points.iter().any(|p| self.jacobian(t, *p) <= 0.0)).count()
    }

    /// The Jacobian determinant of a tetrahedron at a natural coordinate, in Gmsh node order.
    fn jacobian(&self, t: &[u32; 10], [r, s, u]: [f64; 3]) -> f64 {
        let w = 1.0 - r - s - u;
        // Derivatives of the shape functions, one row per node in Gmsh's order:
        // corners 0-3, then edges 01, 12, 20, 30, 23, 31.
        let d: [[f64; 3]; 10] = [
            [1.0 - 4.0 * w, 1.0 - 4.0 * w, 1.0 - 4.0 * w],
            [4.0 * r - 1.0, 0.0, 0.0],
            [0.0, 4.0 * s - 1.0, 0.0],
            [0.0, 0.0, 4.0 * u - 1.0],
            [4.0 * (w - r), -4.0 * r, -4.0 * r],
            [4.0 * s, 4.0 * r, 0.0],
            [-4.0 * s, 4.0 * (w - s), -4.0 * s],
            [-4.0 * u, -4.0 * u, 4.0 * (w - u)],
            [0.0, 4.0 * u, 4.0 * s],
            [4.0 * u, 0.0, 4.0 * r],
        ];
        let mut j = [[0.0; 3]; 3];
        for (node, grad) in t.iter().zip(&d) {
            let p = self.nodes[*node as usize];
            for a in 0..3 {
                for b in 0..3 {
                    j[a][b] += p[a] * grad[b];
                }
            }
        }
        j[0][0] * (j[1][1] * j[2][2] - j[1][2] * j[2][1]) - j[0][1] * (j[1][0] * j[2][2] - j[1][2] * j[2][0]) + j[0][2] * (j[1][0] * j[2][1] - j[1][1] * j[2][0])
    }

    /// The bounding box of the nodes.
    pub fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for n in &self.nodes {
            for i in 0..3 {
                lo[i] = lo[i].min(n[i]);
                hi[i] = hi[i].max(n[i]);
            }
        }
        (lo, hi)
    }
}

/// Lines of a section, with the reader's position, so errors can name a line.
struct Lines<'a> {
    it: std::iter::Enumerate<std::str::Lines<'a>>,
    at: usize,
}

impl<'a> Lines<'a> {
    fn next(&mut self) -> Result<&'a str> {
        let (i, l) = self.it.next().ok_or_else(|| anyhow!("unexpected end of file after line {}", self.at))?;
        self.at = i + 1;
        Ok(l.trim())
    }
    fn ints(&mut self) -> Result<Vec<i64>> {
        let at = self.at + 1;
        self.next()?.split_ascii_whitespace().map(|t| t.parse::<i64>().with_context(|| format!("line {at}: bad integer {t:?}"))).collect()
    }
}

/// Parses a `.msh` 4.1 ASCII file's text.
pub fn parse(text: &str) -> Result<TetMesh> {
    let mut l = Lines { it: text.lines().enumerate(), at: 0 };
    let mut mesh = TetMesh::default();
    let mut node_index: HashMap<i64, u32> = HashMap::new();
    let mut saw_format = false;
    let mut surfaces: HashMap<i32, usize> = HashMap::new();
    while let Ok(line) = l.next() {
        match line {
            "$MeshFormat" => {
                let head = l.next()?;
                let mut f = head.split_ascii_whitespace();
                let version = f.next().unwrap_or("");
                if !version.starts_with("4.") {
                    bail!("expected .msh format 4.x, found {version}");
                }
                if f.next() != Some("0") {
                    bail!("expected ASCII .msh, found binary");
                }
                saw_format = true;
            }
            "$PhysicalNames" => {
                let n: usize = l.next()?.parse().context("physical name count")?;
                for _ in 0..n {
                    let row = l.next()?;
                    let mut f = row.splitn(3, ' ');
                    let dim: i32 = f.next().unwrap_or("").parse().context("physical dimension")?;
                    let tag: i32 = f.next().unwrap_or("").parse().context("physical tag")?;
                    let name = f.next().unwrap_or("").trim().trim_matches('"').to_string();
                    mesh.physical_names.insert((dim, tag), name);
                }
            }
            "$Entities" => read_entities(&mut l, &mut mesh)?,
            "$Nodes" => read_nodes(&mut l, &mut mesh, &mut node_index)?,
            "$Elements" => read_elements(&mut l, &mut mesh, &node_index, &mut surfaces)?,
            _ => {}
        }
    }
    if !saw_format {
        bail!("no $MeshFormat section: not a .msh file");
    }
    Ok(mesh)
}

fn read_entities(l: &mut Lines, mesh: &mut TetMesh) -> Result<()> {
    let counts = l.ints()?;
    if counts.len() != 4 {
        bail!("line {}: bad $Entities counts", l.at);
    }
    // Points: tag x y z nPhysical physical...
    for _ in 0..counts[0] {
        l.next()?;
    }
    for (dim, count) in [(1, counts[1]), (2, counts[2]), (3, counts[3])] {
        for _ in 0..count {
            // tag minx miny minz maxx maxy maxz nPhysical physical... nBounding bounding...
            let row = l.next()?;
            let f: Vec<&str> = row.split_ascii_whitespace().collect();
            if f.len() < 8 {
                bail!("line {}: short entity row", l.at);
            }
            let tag: i32 = f[0].parse().context("entity tag")?;
            let n: usize = f[7].parse().context("physical count")?;
            for p in f.iter().skip(8).take(n) {
                let p: i32 = p.parse().context("physical id")?;
                mesh.physical_entities.entry((dim, p.abs())).or_default().push(tag);
            }
        }
    }
    Ok(())
}

fn read_nodes(l: &mut Lines, mesh: &mut TetMesh, index: &mut HashMap<i64, u32>) -> Result<()> {
    let head = l.ints()?;
    if head.len() != 4 {
        bail!("line {}: bad $Nodes header", l.at);
    }
    let (blocks, total) = (head[0], head[1] as usize);
    mesh.nodes.reserve(total);
    index.reserve(total);
    for _ in 0..blocks {
        let block = l.ints()?;
        if block.len() != 4 {
            bail!("line {}: bad node block header", l.at);
        }
        let n = block[3] as usize;
        let mut tags = Vec::with_capacity(n);
        for _ in 0..n {
            tags.push(l.next()?.parse::<i64>().context("node tag")?);
        }
        for tag in tags {
            let row = l.next()?;
            let mut f = row.split_ascii_whitespace().map(|t| t.parse::<f64>());
            let mut next = || f.next().ok_or_else(|| anyhow!("short node row")).and_then(|v| v.context("bad coordinate"));
            let p = [next()?, next()?, next()?];
            index.insert(tag, mesh.nodes.len() as u32);
            mesh.nodes.push(p);
        }
    }
    Ok(())
}

fn read_elements(l: &mut Lines, mesh: &mut TetMesh, index: &HashMap<i64, u32>, surfaces: &mut HashMap<i32, usize>) -> Result<()> {
    let head = l.ints()?;
    if head.len() != 4 {
        bail!("line {}: bad $Elements header", l.at);
    }
    let node = |tag: i64| index.get(&tag).copied().ok_or_else(|| anyhow!("element refers to unknown node {tag}"));
    for _ in 0..head[0] {
        let block = l.ints()?;
        if block.len() != 4 {
            bail!("line {}: bad element block header", l.at);
        }
        let (entity, kind, n) = (block[1] as i32, block[2] as i32, block[3] as usize);
        if matches!(kind, TRI3 | TET4) {
            bail!("the mesh is first order; the FEA backends need second-order elements (Mesh.ElementOrder = 2)");
        }
        for _ in 0..n {
            let row = l.ints()?;
            match kind {
                TET10 => {
                    if row.len() != 11 {
                        bail!("line {}: a 10-node tetrahedron needs 11 values, found {}", l.at, row.len());
                    }
                    let mut t = [0u32; 10];
                    for (d, s) in t.iter_mut().zip(&row[1..]) {
                        *d = node(*s)?;
                    }
                    mesh.tets.push(t);
                    mesh.tet_volume.push(entity);
                }
                TRI6 => {
                    if row.len() != 7 {
                        bail!("line {}: a 6-node triangle needs 7 values, found {}", l.at, row.len());
                    }
                    let mut t = [0u32; 6];
                    for (d, s) in t.iter_mut().zip(&row[1..]) {
                        *d = node(*s)?;
                    }
                    let at = *surfaces.entry(entity).or_insert_with(|| {
                        mesh.surfaces.push(SurfaceEntity { tag: entity, tris: Vec::new() });
                        mesh.surfaces.len() - 1
                    });
                    mesh.surfaces[at].tris.push(t);
                }
                // Lines and points of the CAD edges and vertices.
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absorbing_a_mesh_keeps_its_nodes_and_surfaces_apart() {
        let tet = |shift: f64, tag: i32| TetMesh {
            nodes: (0..10).map(|i| [f64::from(i) + shift, 0.0, 0.0]).collect(),
            tets: vec![[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]],
            tet_volume: vec![1],
            surfaces: vec![SurfaceEntity { tag, tris: vec![[0, 1, 2, 4, 5, 6]] }],
            ..Default::default()
        };
        let mut a = tet(0.0, 7);
        let offset = a.absorb(tet(100.0, 7));
        assert_eq!(offset, 7);
        assert_eq!(a.nodes.len(), 20);
        assert_eq!(a.tets[1][0], 10, "element nodes follow their own nodes");
        assert_eq!(a.nodes[a.tets[1][0] as usize][0], 100.0);
        assert_eq!(a.surfaces[1].tag, 14);
        assert_eq!(a.surfaces[1].tris[0][0], 10);
        assert_eq!(a.tet_volume, [1, 2]);
    }

    const ONE_TET: &str = "$MeshFormat\n4.1 0 8\n$EndMeshFormat\n$PhysicalNames\n1\n2 7 \"LOAD FACE\"\n$EndPhysicalNames\n$Entities\n0 0 1 1\n5 0 0 0 1 1 0 1 7 0\n1 0 0 0 1 1 1 0 0\n$EndEntities\n$Nodes\n1 10 1 10\n3 1 0 10\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n0 0 0\n1 0 0\n0 1 0\n0 0 1\n0.5 0 0\n0.5 0.5 0\n0 0.5 0\n0 0 0.5\n0 0.5 0.5\n0.5 0 0.5\n$EndNodes\n$Elements\n2 2 1 2\n2 5 9 1\n1 1 2 3 5 6 7\n3 1 11 1\n2 1 2 3 4 5 6 7 8 9 10\n$EndElements\n";

    #[test]
    fn reads_nodes_elements_and_groups() {
        let m = parse(ONE_TET).unwrap();
        assert_eq!(m.nodes.len(), 10);
        assert_eq!(m.tets.len(), 1);
        assert_eq!(m.surfaces.len(), 1);
        assert_eq!(m.surfaces[0].tag, 5);
        assert_eq!(m.physical_names[&(2, 7)], "LOAD FACE");
        assert_eq!(m.physical_entities[&(2, 7)], vec![5]);
        assert_eq!(m.nodes[4], [0.5, 0.0, 0.0]);
    }

    #[test]
    fn a_sound_element_is_not_inverted_and_a_folded_one_is() {
        let mut m = parse(ONE_TET).unwrap();
        // Gmsh order: the element in the fixture lists its mid-side nodes by its own numbering.
        m.tets[0] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(m.inverted_tets(), 0, "a straight unit tetrahedron");
        // Drag a mid-side node past the far corner: the element folds over itself.
        m.nodes[4] = [1.6, 0.0, 0.0];
        assert_eq!(m.inverted_tets(), 1);
    }

    #[test]
    fn refuses_what_it_cannot_use() {
        assert!(parse("not a mesh").is_err());
        assert!(parse("$MeshFormat\n2.2 0 8\n$EndMeshFormat\n").is_err());
        assert!(parse("$MeshFormat\n4.1 1 8\n$EndMeshFormat\n").is_err());
        let first_order = "$MeshFormat\n4.1 0 8\n$EndMeshFormat\n$Elements\n1 1 1 1\n3 1 4 1\n1 1 2 3 4\n$EndElements\n";
        let e = parse(first_order).unwrap_err().to_string();
        assert!(e.contains("second-order"), "{e}");
    }

    #[test]
    fn unknown_nodes_are_an_error_not_a_panic() {
        let bad = "$MeshFormat\n4.1 0 8\n$EndMeshFormat\n$Elements\n1 1 1 1\n3 1 11 1\n1 1 2 3 4 5 6 7 8 9 10\n$EndElements\n";
        assert!(parse(bad).unwrap_err().to_string().contains("unknown node"));
    }
}
