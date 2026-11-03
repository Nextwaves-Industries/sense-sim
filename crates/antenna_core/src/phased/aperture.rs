//! Where an array's elements are and which way they face: lines, planar lattices, rings,
//! cylinders, multi-face panels and sparse arrays, in the frame the rest of the crate uses
//! (θ from +z, φ from +x towards +y, metres).

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

use crate::C0;
use crate::geometry::{Geometry, P3, cross, dot};

pub fn unit_dir(theta: f64, phi: f64) -> P3 {
    let (st, ct) = theta.to_radians().sin_cos();
    let (sp, cp) = phi.to_radians().sin_cos();
    [st * cp, st * sp, ct]
}

pub fn theta_hat(theta: f64, phi: f64) -> P3 {
    let (st, ct) = theta.to_radians().sin_cos();
    let (sp, cp) = phi.to_radians().sin_cos();
    [ct * cp, ct * sp, -st]
}

pub fn phi_hat(phi: f64) -> P3 {
    let (sp, cp) = phi.to_radians().sin_cos();
    [-sp, cp, 0.0]
}

/// θ in 0..=180°, φ in 0..360° of a direction.
pub fn angles_of(d: P3) -> (f64, f64) {
    let r = dot(d, d).sqrt().max(1e-300);
    let theta = (d[2] / r).clamp(-1.0, 1.0).acos().to_degrees();
    let phi = d[1].atan2(d[0]).to_degrees().rem_euclid(360.0);
    (theta, phi)
}

/// The angle between two directions given as (θ, φ), degrees.
pub fn angle_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    dot(unit_dir(a.0, a.1), unit_dir(b.0, b.1)).clamp(-1.0, 1.0).acos().to_degrees()
}

/// One radiator. `ix`/`nx` and `iy`/`ny` place it in its panel's grid for a taper; a layout
/// with no such grid gives 0 of 1, which a taper reads as uniform.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub pos: P3,
    /// Local z: where it faces.
    pub normal: P3,
    /// Local x: the polarisation it's referred to.
    pub x_axis: P3,
    pub panel: usize,
    pub ix: usize,
    pub iy: usize,
    pub nx: usize,
    pub ny: usize,
}

impl Element {
    pub fn y_axis(&self) -> P3 {
        cross(self.normal, self.x_axis)
    }
}

/// Sparse lines, whose difference coarray has far more lags than they have sensors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sparse {
    /// `n1` sensors at spacing 1, then `n2` at spacing `n1 + 1` (Pal and Vaidyanathan, 2010).
    Nested { n1: usize, n2: usize },
    /// Spacings `m` and `n`, coprime, one of them run twice as far (Vaidyanathan and Pal, 2011).
    Coprime { m: usize, n: usize },
    /// A restricted minimum-redundancy line (Moffet, 1968), 3 to 8 sensors.
    MinimumRedundancy { n: usize },
}

impl Sparse {
    /// Sensor positions in units of the base spacing, ascending, starting at 0.
    pub fn positions(self) -> Vec<i64> {
        let mut p: Vec<i64> = match self {
            Sparse::Nested { n1, n2 } => {
                let inner = (0..n1 as i64).collect::<Vec<_>>();
                let outer = (0..n2 as i64).map(|k| n1 as i64 + k * (n1 as i64 + 1));
                inner.into_iter().chain(outer).collect()
            }
            Sparse::Coprime { m, n } => {
                let a = (0..n as i64).map(|k| k * m as i64);
                let b = (0..2 * m as i64).map(|k| k * n as i64);
                a.chain(b).collect()
            }
            Sparse::MinimumRedundancy { n } => match n {
                0..=3 => vec![0, 1, 3],
                4 => vec![0, 1, 4, 6],
                5 => vec![0, 1, 4, 7, 9],
                6 => vec![0, 1, 6, 9, 11, 13],
                7 => vec![0, 1, 8, 11, 13, 15, 17],
                _ => vec![0, 1, 2, 11, 15, 18, 21, 23],
            },
        };
        p.sort_unstable();
        p.dedup();
        p
    }
}

/// How an array is laid out. Spacings and radii are in wavelengths at the design frequency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Layout {
    /// A line along x, facing +z.
    Line { n: usize, spacing: f64 },
    /// Rows along x, stacked along y, facing +z.
    Planar { nx: usize, ny: usize, dx: f64, dy: f64, triangular: bool },
    /// A ring in the xy plane. `radial`: elements face out; otherwise they all face +z, as
    /// monopoles on a ground plate do.
    Ring { n: usize, radius: f64, radial: bool },
    /// Concentric rings: (elements, radius).
    Rings { rings: Vec<(usize, f64)>, radial: bool },
    /// Rows around a cylinder on the z axis, facing out.
    Cylinder { around: usize, rows: usize, radius: f64, dz: f64 },
    /// Flat panels around the z axis, tilted up from the horizon: a multi-face radar or a
    /// base-station sector set.
    Panels { faces: usize, nx: usize, ny: usize, dx: f64, dy: f64, radius: f64, tilt_deg: f64 },
    /// A sparse line along x at a base spacing.
    SparseLine { kind: Sparse, spacing: f64 },
    /// Positions in metres, all facing +z.
    Custom { positions: Vec<P3> },
}

impl Default for Layout {
    fn default() -> Self {
        Layout::Planar { nx: 8, ny: 8, dx: 0.5, dy: 0.5, triangular: false }
    }
}

/// A uniform or sparse line's geometry, for methods that need one.
#[derive(Clone, Debug, PartialEq)]
pub struct LineInfo {
    /// Base spacing, metres.
    pub spacing: f64,
    /// Position of each element in units of the spacing, from 0.
    pub index: Vec<i64>,
}

impl LineInfo {
    pub fn is_uniform(&self) -> bool {
        self.index.windows(2).all(|w| w[1] - w[0] == 1)
    }
}

fn panel(out: &mut Vec<Element>, id: usize, grid: (usize, usize), step: (f64, f64), triangular: bool, frame: (P3, P3, P3), center: P3) {
    let (nx, ny) = grid;
    let (x_axis, y_axis, normal) = frame;
    let start = out.len();
    for iy in 0..ny {
        for ix in 0..nx {
            let shift = if triangular && nx > 1 && iy % 2 == 1 { 0.5 } else { 0.0 };
            let (a, b) = ((ix as f64 + shift) * step.0, iy as f64 * step.1);
            let pos = std::array::from_fn(|k| a * x_axis[k] + b * y_axis[k]);
            out.push(Element { pos, normal, x_axis, panel: id, ix, iy, nx, ny });
        }
    }
    // Centre it about `center`.
    let count = (out.len() - start).max(1) as f64;
    let mean: P3 = std::array::from_fn(|k| out[start..].iter().map(|e| e.pos[k]).sum::<f64>() / count);
    for e in &mut out[start..] {
        e.pos = std::array::from_fn(|k| e.pos[k] - mean[k] + center[k]);
    }
}

impl Layout {
    /// The elements, in metres, at a design frequency `f0`.
    pub fn elements(&self, f0: f64) -> Vec<Element> {
        let lambda = C0 / f0.max(1.0);
        let up = [0.0, 0.0, 1.0];
        let x = [1.0, 0.0, 0.0];
        let y = [0.0, 1.0, 0.0];
        let mut out = Vec::new();
        match self {
            Layout::Line { n, spacing } => panel(&mut out, 0, (*n, 1), (spacing * lambda, 0.0), false, (x, y, up), [0.0; 3]),
            Layout::Planar { nx, ny, dx, dy, triangular } => panel(&mut out, 0, (*nx, *ny), (dx * lambda, dy * lambda), *triangular, (x, y, up), [0.0; 3]),
            Layout::Ring { n, radius, radial } => ring(&mut out, 0, *n, radius * lambda, *radial),
            Layout::Rings { rings, radial } => {
                for (id, (n, r)) in rings.iter().enumerate() {
                    ring(&mut out, id, *n, r * lambda, *radial);
                }
            }
            Layout::Cylinder { around, rows, radius, dz } => {
                for iy in 0..*rows {
                    for k in 0..*around {
                        let a = TAU * k as f64 / *around as f64;
                        let n = [a.cos(), a.sin(), 0.0];
                        let z = (iy as f64 - (*rows as f64 - 1.0) / 2.0) * dz * lambda;
                        // ix stays 0 of 1: which elements light up follows the beam, not a fixed taper.
                        out.push(Element { pos: [radius * lambda * n[0], radius * lambda * n[1], z], normal: n, x_axis: [-a.sin(), a.cos(), 0.0], panel: k, ix: 0, iy, nx: 1, ny: *rows });
                    }
                }
            }
            Layout::Panels { faces, nx, ny, dx, dy, radius, tilt_deg } => {
                for face in 0..*faces {
                    let az = TAU * face as f64 / *faces as f64;
                    let tilt = tilt_deg.to_radians();
                    let normal = [az.cos() * tilt.cos(), az.sin() * tilt.cos(), tilt.sin()];
                    let across = [-az.sin(), az.cos(), 0.0];
                    let along = cross(normal, across);
                    let center = [radius * lambda * az.cos(), radius * lambda * az.sin(), 0.0];
                    panel(&mut out, face, (*nx, *ny), (dx * lambda, dy * lambda), false, (across, along, normal), center);
                }
            }
            Layout::SparseLine { kind, spacing } => {
                let d = spacing * lambda;
                let p = kind.positions();
                let mean = p.iter().sum::<i64>() as f64 / p.len().max(1) as f64;
                for k in &p {
                    out.push(Element { pos: [(*k as f64 - mean) * d, 0.0, 0.0], normal: up, x_axis: x, panel: 0, ix: 0, iy: 0, nx: 1, ny: 1 });
                }
            }
            Layout::Custom { positions } => {
                for p in positions {
                    out.push(Element { pos: *p, normal: up, x_axis: x, panel: 0, ix: 0, iy: 0, nx: 1, ny: 1 });
                }
            }
        }
        out
    }

    /// How many elements it has, without building them.
    pub fn count(&self) -> usize {
        match self {
            Layout::Line { n, .. } | Layout::Ring { n, .. } => *n,
            Layout::Planar { nx, ny, .. } => nx * ny,
            Layout::Rings { rings, .. } => rings.iter().map(|r| r.0).sum(),
            Layout::Cylinder { around, rows, .. } => around * rows,
            Layout::Panels { faces, nx, ny, .. } => faces * nx * ny,
            Layout::SparseLine { kind, .. } => kind.positions().len(),
            Layout::Custom { positions } => positions.len(),
        }
    }

    /// Set for a straight line along x: the methods that rely on shift invariance need it.
    pub fn line(&self, f0: f64) -> Option<LineInfo> {
        let lambda = C0 / f0.max(1.0);
        match self {
            Layout::Line { n, spacing } => Some(LineInfo { spacing: spacing * lambda, index: (0..*n as i64).collect() }),
            Layout::Planar { nx, ny: 1, dx, .. } => Some(LineInfo { spacing: dx * lambda, index: (0..*nx as i64).collect() }),
            Layout::SparseLine { kind, spacing } => Some(LineInfo { spacing: spacing * lambda, index: kind.positions() }),
            _ => None,
        }
    }

    /// Elements facing different ways need the beam to pick which of them light up.
    pub fn is_flat(&self) -> bool {
        !matches!(self, Layout::Cylinder { .. } | Layout::Panels { .. } | Layout::Ring { radial: true, .. } | Layout::Rings { radial: true, .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Layout::Line { n, spacing } => format!("Line of {n} at {spacing:.3} λ"),
            Layout::Planar { nx, ny, dx, dy, triangular } => format!("{nx} × {ny} {} at {dx:.3} × {dy:.3} λ", if *triangular { "triangular" } else { "rectangular" }),
            Layout::Ring { n, radius, .. } => format!("Ring of {n}, radius {radius:.3} λ"),
            Layout::Rings { rings, .. } => format!("{} rings, {} elements", rings.len(), self.count()),
            Layout::Cylinder { around, rows, radius, .. } => format!("Cylinder: {around} around × {rows} rows, radius {radius:.2} λ"),
            Layout::Panels { faces, nx, ny, tilt_deg, .. } => format!("{faces} faces of {nx} × {ny}, tilted {tilt_deg:.0}°"),
            Layout::SparseLine { kind, spacing } => match kind {
                Sparse::Nested { n1, n2 } => format!("Nested line {n1}+{n2} at {spacing:.3} λ"),
                Sparse::Coprime { m, n } => format!("Coprime line ({m}, {n}) at {spacing:.3} λ"),
                Sparse::MinimumRedundancy { n } => format!("Minimum-redundancy line of {n} at {spacing:.3} λ"),
            },
            Layout::Custom { positions } => format!("{} elements at given places", positions.len()),
        }
    }
}

fn ring(out: &mut Vec<Element>, id: usize, n: usize, r: f64, radial: bool) {
    for k in 0..n {
        let a = TAU * k as f64 / n.max(1) as f64;
        let (s, c) = a.sin_cos();
        let (normal, x_axis) = if radial { ([c, s, 0.0], [-s, c, 0.0]) } else { ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]) };
        out.push(Element { pos: [r * c, r * s, 0.0], normal, x_axis, panel: id, ix: 0, iy: 0, nx: 1, ny: 1 });
    }
}

/// Elements at the ports of a model, in port order, as a solved array has them.
pub fn elements_of_ports(geom: &Geometry) -> Vec<Element> {
    geom.ports.iter().map(|p| Element { pos: p.center(), normal: [0.0, 0.0, 1.0], x_axis: [1.0, 0.0, 0.0], panel: 0, ix: 0, iy: 0, nx: 1, ny: 1 }).collect()
}

/// The lags a sparse line's difference coarray has, each with how many pairs give it,
/// and the longest run of consecutive lags around zero: how many sources it can resolve.
pub fn coarray(index: &[i64]) -> (Vec<(i64, usize)>, i64) {
    let mut counts = std::collections::BTreeMap::new();
    for a in index {
        for b in index {
            *counts.entry(a - b).or_insert(0usize) += 1;
        }
    }
    let mut hole_free = 0;
    while counts.contains_key(&(hole_free + 1)) {
        hole_free += 1;
    }
    (counts.into_iter().collect(), hole_free)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: f64 = 3e9;

    fn lambda() -> f64 {
        C0 / F
    }

    #[test]
    fn directions_are_orthonormal_and_invert() {
        for (t, p) in [(10.0, 20.0), (90.0, 45.0), (135.0, 300.0)] {
            let (r, a, b) = (unit_dir(t, p), theta_hat(t, p), phi_hat(p));
            assert!((dot(r, r) - 1.0).abs() < 1e-12);
            assert!(dot(r, a).abs() < 1e-12 && dot(r, b).abs() < 1e-12 && dot(a, b).abs() < 1e-12);
            // (r̂, θ̂, φ̂) is right handed.
            let c = cross(a, b);
            assert!((dot(c, r) - 1.0).abs() < 1e-12);
            let (t2, p2) = angles_of(r);
            assert!((t2 - t).abs() < 1e-9 && (p2 - p).abs() < 1e-9, "{t2} {p2}");
        }
        assert!((angle_between((0.0, 0.0), (0.0, 123.0))).abs() < 1e-6, "the pole has one direction");
        assert!((angle_between((90.0, 0.0), (90.0, 90.0)) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn layouts_build_what_they_count() {
        for l in [
            Layout::Line { n: 8, spacing: 0.5 },
            Layout::Planar { nx: 4, ny: 3, dx: 0.5, dy: 0.5, triangular: true },
            Layout::Ring { n: 8, radius: 0.5, radial: false },
            Layout::Rings { rings: vec![(4, 0.25), (8, 0.5)], radial: false },
            Layout::Cylinder { around: 16, rows: 2, radius: 1.0, dz: 0.5 },
            Layout::Panels { faces: 4, nx: 4, ny: 4, dx: 0.5, dy: 0.5, radius: 1.5, tilt_deg: 10.0 },
            Layout::SparseLine { kind: Sparse::Nested { n1: 3, n2: 3 }, spacing: 0.5 },
            Layout::Custom { positions: vec![[0.0; 3], [0.1, 0.0, 0.0]] },
        ] {
            assert_eq!(l.elements(F).len(), l.count(), "{}", l.describe());
            for e in l.elements(F) {
                assert!((dot(e.normal, e.normal) - 1.0).abs() < 1e-9, "{}", l.describe());
                assert!(dot(e.normal, e.x_axis).abs() < 1e-9, "{}", l.describe());
            }
        }
    }

    #[test]
    fn a_planar_lattice_is_centred_and_a_triangular_one_is_shifted() {
        let e = Layout::Planar { nx: 3, ny: 2, dx: 0.5, dy: 0.25, triangular: false }.elements(F);
        let sum: P3 = std::array::from_fn(|k| e.iter().map(|x| x.pos[k]).sum());
        assert!(sum.iter().all(|v| v.abs() < 1e-12));
        assert!((e[1].pos[0] - e[0].pos[0] - 0.5 * lambda()).abs() < 1e-12);
        assert!((e[3].pos[1] - e[0].pos[1] - 0.25 * lambda()).abs() < 1e-12);
        let t = Layout::Planar { nx: 3, ny: 2, dx: 0.5, dy: 0.25, triangular: true }.elements(F);
        assert!((t[3].pos[0] - t[0].pos[0] - 0.25 * lambda()).abs() < 1e-12);
    }

    #[test]
    fn panels_face_their_own_way_and_sit_on_their_radius() {
        let l = Layout::Panels { faces: 4, nx: 3, ny: 3, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 };
        let e = l.elements(F);
        for f in 0..4 {
            let of: Vec<&Element> = e.iter().filter(|x| x.panel == f).collect();
            assert_eq!(of.len(), 9);
            let a = TAU * f as f64 / 4.0;
            let want = [a.cos(), a.sin(), 0.0];
            assert!((dot(of[0].normal, want) - 1.0).abs() < 1e-12);
            let mean: P3 = std::array::from_fn(|k| of.iter().map(|x| x.pos[k]).sum::<f64>() / 9.0);
            assert!((dot(mean, want) - 2.0 * lambda()).abs() < 1e-12);
            // Flat: every element in the plane through the panel's centre.
            for x in &of {
                let off: P3 = std::array::from_fn(|k| x.pos[k] - mean[k]);
                assert!(dot(off, want).abs() < 1e-12);
            }
        }
        // A tilt lifts the faces: the z of the normal is sin(tilt).
        let tilted = Layout::Panels { faces: 3, nx: 2, ny: 2, dx: 0.5, dy: 0.5, radius: 1.0, tilt_deg: 15.0 }.elements(F);
        assert!((tilted[0].normal[2] - 15f64.to_radians().sin()).abs() < 1e-12);
    }

    #[test]
    fn minimum_redundancy_tables_have_no_holes() {
        for (n, aperture) in [(3, 3), (4, 6), (5, 9), (6, 13), (7, 17), (8, 23)] {
            let p = Sparse::MinimumRedundancy { n }.positions();
            assert_eq!(p.len(), n);
            let (_, free) = coarray(&p);
            assert_eq!(free, aperture, "the lags of {p:?} run without a hole to the aperture");
        }
    }

    #[test]
    fn sparse_coarrays_reach_what_their_papers_say() {
        // Nested: 2·n2·(n1 + 1) − 1 consecutive lags in all.
        let (n1, n2) = (3, 3);
        let (_, free) = coarray(&Sparse::Nested { n1, n2 }.positions());
        assert_eq!(2 * free + 1, (2 * n2 * (n1 + 1) - 1) as i64);
        // Coprime with one spacing run twice as far: hole free up to at least m·n.
        let (m, n) = (3, 5);
        let positions = Sparse::Coprime { m, n }.positions();
        let (_, free) = coarray(&positions);
        assert!(free >= (m * n) as i64, "{free} from {positions:?}");
        assert_eq!(positions.len(), n + 2 * m - 1, "the two runs share only the sensor at 0");
    }

    #[test]
    fn a_line_says_it_is_one() {
        let l = Layout::Line { n: 5, spacing: 0.5 }.line(F).unwrap();
        assert!(l.is_uniform());
        assert!((l.spacing - 0.5 * lambda()).abs() < 1e-12);
        let s = Layout::SparseLine { kind: Sparse::Nested { n1: 2, n2: 2 }, spacing: 0.5 }.line(F).unwrap();
        assert!(!s.is_uniform());
        assert_eq!(s.index, vec![0, 1, 2, 5]);
        assert!(Layout::Ring { n: 4, radius: 0.3, radial: false }.line(F).is_none());
    }
}
