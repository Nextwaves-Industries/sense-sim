//! Merges coincident points into one vertex id.

use std::collections::HashMap;

use crate::geometry::{P3, sub};

pub struct Welder {
    pub pts: Vec<P3>,
    tol: f64,
    cell: f64,
    cells: HashMap<[i64; 3], Vec<u32>>,
}

impl Welder {
    pub fn new(tol: f64) -> Self {
        Self { pts: Vec::new(), tol, cell: tol * 4.0, cells: HashMap::new() }
    }

    fn key(&self, p: P3) -> [i64; 3] {
        [(p[0] / self.cell).floor() as i64, (p[1] / self.cell).floor() as i64, (p[2] / self.cell).floor() as i64]
    }

    /// The id of an existing point within tolerance of `p`, if there is one.
    pub fn find(&self, p: P3) -> Option<u32> {
        let k = self.key(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(list) = self.cells.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) {
                        for &id in list {
                            let d = sub(self.pts[id as usize], p);
                            if d[0].abs() <= self.tol && d[1].abs() <= self.tol && d[2].abs() <= self.tol {
                                return Some(id);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// The id of `p`, added if it is new.
    pub fn id(&mut self, p: P3) -> u32 {
        if let Some(id) = self.find(p) {
            return id;
        }
        let id = self.pts.len() as u32;
        self.pts.push(p);
        let k = self.key(p);
        self.cells.entry(k).or_default().push(id);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_points_merge_and_far_points_do_not() {
        let mut w = Welder::new(1e-6);
        let a = w.id([1.0, 2.0, 3.0]);
        assert_eq!(w.id([1.0 + 5e-7, 2.0 - 5e-7, 3.0]), a);
        assert_ne!(w.id([1.0 + 5e-6, 2.0, 3.0]), a);
        assert_eq!(w.pts.len(), 2);
    }

    #[test]
    fn a_cell_boundary_does_not_split_a_point() {
        let mut w = Welder::new(1e-6);
        let a = w.id([0.0, 0.0, 0.0]);
        assert_eq!(w.id([-1e-9, 1e-9, -1e-9]), a);
    }
}
