//! Turns a surface into solid cells on a Cartesian grid. A closed surface is filled by counting
//! crossings along each column. An open or messy one (USD assets often are) is drawn as a shell of
//! cells instead, and only what the outside can't reach is filled.

use rayon::prelude::*;

use crate::geometry::Partition;

/// A grid in metres: `origin` is the corner of cell (0, 0, 0).
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub origin: [f64; 3],
    pub dx: f64,
    pub size: [u32; 3],
}

impl Grid {
    pub fn index(&self, x: u32, y: u32, z: u32) -> usize {
        x as usize + self.size[0] as usize * (y as usize + self.size[1] as usize * z as usize)
    }

    pub fn cells(&self) -> usize {
        self.size[0] as usize * self.size[1] as usize * self.size[2] as usize
    }

    /// The middle of a cell.
    pub fn centre(&self, x: u32, y: u32, z: u32) -> [f64; 3] {
        [self.origin[0] + (f64::from(x) + 0.5) * self.dx, self.origin[1] + (f64::from(y) + 0.5) * self.dx, self.origin[2] + (f64::from(z) + 0.5) * self.dx]
    }
}

/// Solid cells of a closed surface, by crossings along x through each cell centre in y and z. The
/// column lines are nudged off exact cell centres so that they don't run through triangle edges.
pub fn fill_closed(part: &Partition, grid: &Grid) -> Vec<bool> {
    let (nx, ny, nz) = (grid.size[0] as usize, grid.size[1] as usize, grid.size[2] as usize);
    // Triangles by the columns they can touch.
    let mut columns: Vec<Vec<u32>> = vec![Vec::new(); ny * nz];
    for i in 0..part.triangles.len() {
        let [a, b, c] = part.triangle(i);
        let yz = |p: [f64; 3]| ((p[1] - grid.origin[1]) / grid.dx - 0.5, (p[2] - grid.origin[2]) / grid.dx - 0.5);
        let (ya, za) = yz(a);
        let (yb, zb) = yz(b);
        let (yc, zc) = yz(c);
        let (y0, y1) = (ya.min(yb).min(yc).floor().max(0.0) as usize, (ya.max(yb).max(yc).ceil().max(0.0) as usize).min(ny.saturating_sub(1)));
        let (z0, z1) = (za.min(zb).min(zc).floor().max(0.0) as usize, (za.max(zb).max(zc).ceil().max(0.0) as usize).min(nz.saturating_sub(1)));
        for z in z0..=z1 {
            for y in y0..=y1 {
                columns[y + ny * z].push(i as u32);
            }
        }
    }
    let mut solid = vec![false; nx * ny * nz];
    solid.par_chunks_mut(nx * ny).enumerate().for_each(|(z, plane)| {
        let mut hits: Vec<f64> = Vec::new();
        for y in 0..ny {
            let py = grid.origin[1] + (y as f64 + 0.5 + 1.3e-4) * grid.dx;
            let pz = grid.origin[2] + (z as f64 + 0.5 + 2.7e-4) * grid.dx;
            hits.clear();
            for &t in &columns[y + ny * z] {
                let [a, b, c] = part.triangle(t as usize);
                // The crossing of the x-line at (py, pz), by barycentric coordinates in the yz plane.
                let d = (b[1] - a[1]) * (c[2] - a[2]) - (c[1] - a[1]) * (b[2] - a[2]);
                if d.abs() < 1e-30 {
                    continue;
                }
                let u = ((py - a[1]) * (c[2] - a[2]) - (c[1] - a[1]) * (pz - a[2])) / d;
                let v = ((b[1] - a[1]) * (pz - a[2]) - (py - a[1]) * (b[2] - a[2])) / d;
                if u >= 0.0 && v >= 0.0 && u + v <= 1.0 {
                    hits.push(a[0] + u * (b[0] - a[0]) + v * (c[0] - a[0]));
                }
            }
            hits.sort_by(f64::total_cmp);
            for pair in hits.chunks_exact(2) {
                let (x0, x1) = (((pair[0] - grid.origin[0]) / grid.dx - 0.5).ceil().max(0.0) as usize, ((pair[1] - grid.origin[0]) / grid.dx - 0.5).floor());
                if x1 < 0.0 {
                    continue;
                }
                let x1 = (x1 as usize).min(nx - 1);
                for x in x0..=x1 {
                    plane[x + nx * y] = true;
                }
            }
        }
    });
    solid
}

/// Solid cells of any surface: every cell a triangle touches, plus every cell the domain's outside
/// can't reach. Conservative: thin walls come out at least a cell thick.
pub fn fill_shell(part: &Partition, grid: &Grid) -> Vec<bool> {
    let (nx, ny, nz) = (grid.size[0] as i64, grid.size[1] as i64, grid.size[2] as i64);
    let mut solid = vec![false; grid.cells()];
    for i in 0..part.triangles.len() {
        let [a, b, c] = part.triangle(i);
        // Enough samples across the triangle that no cell in its path is skipped.
        let longest = [(a, b), (b, c), (c, a)].iter().map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()).fold(0.0, f64::max);
        let n = ((longest / grid.dx * 2.0).ceil() as usize).max(1);
        for s in 0..=n {
            for t in 0..=(n - s) {
                let (u, v) = (s as f64 / n as f64, t as f64 / n as f64);
                let w = 1.0 - u - v;
                let p = [w * a[0] + u * b[0] + v * c[0], w * a[1] + u * b[1] + v * c[1], w * a[2] + u * b[2] + v * c[2]];
                let cell = |k: usize| ((p[k] - grid.origin[k]) / grid.dx).floor() as i64;
                let (x, y, z) = (cell(0), cell(1), cell(2));
                if x >= 0 && y >= 0 && z >= 0 && x < nx && y < ny && z < nz {
                    solid[grid.index(x as u32, y as u32, z as u32)] = true;
                }
            }
        }
    }
    // Flood the outside in from the domain's faces; what stays unreached is inside.
    let mut outside = vec![false; grid.cells()];
    let mut stack: Vec<(i64, i64, i64)> = Vec::new();
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                if (x == 0 || y == 0 || z == 0 || x == nx - 1 || y == ny - 1 || z == nz - 1) && !solid[grid.index(x as u32, y as u32, z as u32)] {
                    outside[grid.index(x as u32, y as u32, z as u32)] = true;
                    stack.push((x, y, z));
                }
            }
        }
    }
    while let Some((x, y, z)) = stack.pop() {
        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
            let (a, b, c) = (x + dx, y + dy, z + dz);
            if a < 0 || b < 0 || c < 0 || a >= nx || b >= ny || c >= nz {
                continue;
            }
            let i = grid.index(a as u32, b as u32, c as u32);
            if !solid[i] && !outside[i] {
                outside[i] = true;
                stack.push((a, b, c));
            }
        }
    }
    outside.iter().map(|o| !o).collect()
}

/// Fills `part` by whichever way fits it.
pub fn fill(part: &Partition, grid: &Grid) -> Vec<bool> {
    if part.is_closed() { fill_closed(part, grid) } else { fill_shell(part, grid) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed box as a partition.
    fn cube(lo: [f64; 3], hi: [f64; 3]) -> Partition {
        let p = |x: usize, y: usize, z: usize| [if x == 0 { lo[0] } else { hi[0] }, if y == 0 { lo[1] } else { hi[1] }, if z == 0 { lo[2] } else { hi[2] }];
        let positions = vec![p(0, 0, 0), p(1, 0, 0), p(1, 1, 0), p(0, 1, 0), p(0, 0, 1), p(1, 0, 1), p(1, 1, 1), p(0, 1, 1)];
        let quads = [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]];
        let mut triangles = Vec::new();
        let mut tri_face = Vec::new();
        for (f, q) in quads.iter().enumerate() {
            triangles.push([q[0], q[1], q[2]]);
            triangles.push([q[0], q[2], q[3]]);
            tri_face.extend([f as u32, f as u32]);
        }
        Partition { name: "cube".into(), positions, triangles, tri_face, faces: 6 }
    }

    fn grid() -> Grid {
        Grid { origin: [0.0; 3], dx: 0.1, size: [20, 20, 20] }
    }

    #[test]
    fn a_closed_cube_fills_to_the_right_volume() {
        let part = cube([0.5, 0.5, 0.5], [1.5, 1.5, 1.5]);
        assert!(part.is_closed());
        let solid = fill(&part, &grid());
        assert_eq!(solid.iter().filter(|s| **s).count(), 1000, "10 x 10 x 10 cells");
        assert!(solid[grid().index(10, 10, 10)]);
        assert!(!solid[grid().index(2, 10, 10)]);
    }

    #[test]
    fn a_body_past_the_grid_is_clipped_without_harm() {
        let part = cube([-5.0, 0.5, 0.5], [0.95, 1.5, 1.5]);
        let solid = fill(&part, &grid());
        assert_eq!(solid.iter().filter(|s| **s).count(), 900, "x cells 0 to 8 of the 10 x 10 section");
    }

    #[test]
    fn an_open_surface_still_gets_a_solid_when_it_encloses_something() {
        let mut part = cube([0.55, 0.55, 0.55], [1.45, 1.45, 1.45]);
        // One triangle missing: not closed. The hole is under a cell's size here, so the shell seals it.
        part.triangles.pop();
        part.tri_face.pop();
        assert!(!part.is_closed());
        let solid = fill(&part, &Grid { origin: [0.0; 3], dx: 0.1, size: [20, 20, 20] });
        assert!(!solid[grid().index(2, 2, 2)], "the outside stays fluid");
        assert!(solid[grid().index(5, 5, 5)], "the shell is solid");
    }

    #[test]
    fn a_plate_comes_out_at_least_a_cell_thick() {
        // An open square sheet: no inside at all, but it must still block the flow.
        let part = Partition { name: "sheet".into(), positions: vec![[0.5, 0.5, 1.03], [1.5, 0.5, 1.03], [1.5, 1.5, 1.03], [0.5, 1.5, 1.03]], triangles: vec![[0, 1, 2], [0, 2, 3]], tri_face: vec![0, 0], faces: 1 };
        let solid = fill(&part, &grid());
        assert!(solid[grid().index(10, 10, 10)], "the sheet's cell");
        assert!(!solid[grid().index(10, 10, 12)], "above it stays fluid");
        assert!(!solid[grid().index(10, 10, 7)], "below it stays fluid");
    }
}
