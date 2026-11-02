//! What any solver's run comes down to: a mesh, steps (times, frequencies or modes) and fields.
//! The viewer and the benchmarks only know this model.

use std::sync::Arc;

/// Where a field's values live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Node,
    Cell,
}

/// A named quantity with its unit, `components` values per point.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub unit: String,
    pub location: Location,
    /// Component labels, e.g. `["D1", "D2", "D3"]`. Its length is the stride of `data`.
    pub components: Vec<String>,
    pub data: Arc<[f32]>,
}

impl Field {
    pub fn stride(&self) -> usize {
        self.components.len().max(1)
    }

    /// Number of points.
    pub fn len(&self) -> usize {
        self.data.len() / self.stride()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// One point's values.
    pub fn at(&self, i: usize) -> &[f32] {
        let s = self.stride();
        &self.data[i * s..(i + 1) * s]
    }

    /// The magnitude of each point (the value itself for a scalar). Stress-like fields of six
    /// components give the von Mises value instead.
    pub fn scalar(&self) -> Vec<f32> {
        let s = self.stride();
        match s {
            1 => self.data.to_vec(),
            6 => self.data.chunks_exact(6).map(von_mises).collect(),
            _ => self.data.chunks_exact(s).map(|p| p.iter().map(|v| v * v).sum::<f32>().sqrt()).collect(),
        }
    }

    /// The smallest and largest of [`Field::scalar`], ignoring non-finite values.
    pub fn range(&self) -> Option<(f32, f32)> {
        let mut r: Option<(f32, f32)> = None;
        for v in self.scalar() {
            if v.is_finite() {
                r = Some(r.map_or((v, v), |(a, b)| (a.min(v), b.max(v))));
            }
        }
        r
    }
}

/// xx, yy, zz, xy, yz, zx.
pub fn von_mises(s: &[f32]) -> f32 {
    let [xx, yy, zz, xy, yz, zx] = [s[0] as f64, s[1] as f64, s[2] as f64, s[3] as f64, s[4] as f64, s[5] as f64];
    let v = 0.5 * ((xx - yy).powi(2) + (yy - zz).powi(2) + (zz - xx).powi(2)) + 3.0 * (xy * xy + yz * yz + zx * zx);
    v.max(0.0).sqrt() as f32
}

/// What a step's `value` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// Time, or a load factor in a static run.
    Time,
    /// A mode's natural frequency, in Hz.
    Mode,
    /// The excitation frequency of a harmonic run, in Hz.
    Frequency,
}

#[derive(Debug, Clone)]
pub struct ResultStep {
    pub kind: StepKind,
    pub value: f64,
    pub fields: Vec<Field>,
}

impl ResultStep {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name.eq_ignore_ascii_case(name))
    }
}

/// The mesh results are drawn on. Node order is the solver's.
#[derive(Debug, Clone, Default)]
pub struct ResultMesh {
    pub nodes: Vec<[f64; 3]>,
    /// Linear tetrahedra, for cut planes and isosurfaces.
    pub tets: Vec<[u32; 4]>,
    /// The outer surface, as linear triangles.
    pub surface: Vec<[u32; 3]>,
    /// Where cuts through the volume start in `surface`: what comes before is the walls.
    pub cut_start: Option<usize>,
}

/// One line of a solver's plot (a residual, a probe, a force history).
#[derive(Debug, Clone, Default)]
pub struct Plot {
    pub name: String,
    pub unit: String,
    pub points: Vec<(f64, f64)>,
}

#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub mesh: ResultMesh,
    pub steps: Vec<ResultStep>,
    pub plots: Vec<Plot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(components: &[&str], data: &[f32]) -> Field {
        Field { name: "x".into(), unit: "".into(), location: Location::Node, components: components.iter().map(|c| c.to_string()).collect(), data: data.into() }
    }

    #[test]
    fn scalar_is_magnitude_for_vectors() {
        let f = field(&["a", "b", "c"], &[3.0, 4.0, 0.0, 0.0, 0.0, 2.0]);
        assert_eq!(f.len(), 2);
        assert_eq!(f.scalar(), vec![5.0, 2.0]);
        assert_eq!(f.range(), Some((2.0, 5.0)));
    }

    #[test]
    fn von_mises_of_uniaxial_and_pure_shear() {
        assert!((von_mises(&[100.0, 0.0, 0.0, 0.0, 0.0, 0.0]) - 100.0).abs() < 1e-3);
        assert!((von_mises(&[0.0, 0.0, 0.0, 100.0, 0.0, 0.0]) - 173.205).abs() < 1e-2);
    }

    #[test]
    fn non_finite_values_do_not_poison_the_range() {
        assert_eq!(field(&["a"], &[f32::NAN, 1.0, 2.0, f32::INFINITY]).range(), Some((1.0, 2.0)));
        assert_eq!(field(&["a"], &[f32::NAN]).range(), None);
    }
}
