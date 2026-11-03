//! What a run is asked to solve: bodies, flat sheets, wires and ports on a grid.

use crate::grid::Grid;

pub type P3 = [f64; 3];

/// A closed shape.
#[derive(Clone, Debug, PartialEq)]
pub enum Solid {
    Box { min: P3, max: P3 },
    Cylinder { a: P3, b: P3, r: f64 },
    Sphere { c: P3, r: f64 },
    /// An outline in the plane normal to `axis` at `at`, as `(axis + 1, axis + 2)` coordinates,
    /// extruded by `thick` along `+axis`.
    Prism { axis: usize, at: f64, pts: Vec<[f64; 2]>, thick: f64 },
    /// A closed triangle surface.
    Mesh { tris: Vec<[P3; 3]> },
    /// Everything outside a solid: how a metal cavity is drawn.
    Outside(Box<Solid>),
}

impl Solid {
    pub fn bounds(&self) -> (P3, P3) {
        match self {
            Solid::Box { min, max } => (*min, *max),
            Solid::Cylinder { a, b, r } => {
                // The caps' true reach along each axis, not the corner of a box around them.
                let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let l2 = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).max(1e-300);
                let mut lo = [0.0; 3];
                let mut hi = [0.0; 3];
                for k in 0..3 {
                    let ext = r * (1.0 - d[k] * d[k] / l2).max(0.0).sqrt();
                    lo[k] = a[k].min(b[k]) - ext;
                    hi[k] = a[k].max(b[k]) + ext;
                }
                (lo, hi)
            }
            Solid::Sphere { c, r } => ([c[0] - r, c[1] - r, c[2] - r], [c[0] + r, c[1] + r, c[2] + r]),
            Solid::Prism { axis, at, pts, thick } => {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                for p in pts {
                    lo[u] = lo[u].min(p[0]);
                    hi[u] = hi[u].max(p[0]);
                    lo[v] = lo[v].min(p[1]);
                    hi[v] = hi[v].max(p[1]);
                }
                lo[*axis] = *at;
                hi[*axis] = at + thick;
                (lo, hi)
            }
            Solid::Outside(_) => ([-1e30; 3], [1e30; 3]),
            Solid::Mesh { tris } => {
                let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                for t in tris {
                    for p in t {
                        for k in 0..3 {
                            lo[k] = lo[k].min(p[k]);
                            hi[k] = hi[k].max(p[k]);
                        }
                    }
                }
                (lo, hi)
            }
        }
    }

    /// Whether cuts through it are exact from one column per breakpoint rectangle: the faces of
    /// a box lie on the breakpoints, so nothing changes inside a rectangle.
    pub fn is_box(&self) -> bool {
        match self {
            Solid::Box { .. } => true,
            Solid::Outside(inner) => inner.is_box(),
            _ => false,
        }
    }

    /// Planes along `axis` where this solid starts or stops: exact cuts for a sum over a cell.
    pub fn planes(&self, axis: usize) -> Vec<f64> {
        match self {
            Solid::Outside(inner) => inner.planes(axis),
            _ => {
                let (l, h) = self.bounds();
                vec![l[axis], h[axis]]
            }
        }
    }

    /// The parts of the line through `p` along `axis` inside the solid, within `lo..hi`, added to
    /// `out`.
    pub fn chord(&self, p: P3, axis: usize, lo: f64, hi: f64, out: &mut Vec<(f64, f64)>) {
        let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut push = |a: f64, b: f64| {
            let (a, b) = (a.max(lo), b.min(hi));
            if b > a {
                out.push((a, b));
            }
        };
        match self {
            Solid::Box { min, max } => {
                if p[t1] >= min[t1] && p[t1] <= max[t1] && p[t2] >= min[t2] && p[t2] <= max[t2] {
                    push(min[axis], max[axis]);
                }
            }
            Solid::Sphere { c, r } => {
                let (a, b) = (p[t1] - c[t1], p[t2] - c[t2]);
                let q = r * r - a * a - b * b;
                if q > 0.0 {
                    let s = q.sqrt();
                    push(c[axis] - s, c[axis] + s);
                }
            }
            Solid::Cylinder { a, b, r } => {
                // Points p + t·ê with ê the axis direction: inside the tube and between the caps.
                let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if len <= 0.0 {
                    return;
                }
                let u = [d[0] / len, d[1] / len, d[2] / len];
                let m = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
                let along = |v: [f64; 3]| v[0] * u[0] + v[1] * u[1] + v[2] * u[2];
                // Take the line parameter t as the coordinate along `axis`.
                let mut base = m;
                base[axis] = -a[axis];
                let mut dir = [0.0; 3];
                dir[axis] = 1.0;
                let (bd, dd) = (along(base), along(dir));
                let perp_b = [base[0] - bd * u[0], base[1] - bd * u[1], base[2] - bd * u[2]];
                let perp_d = [dir[0] - dd * u[0], dir[1] - dd * u[1], dir[2] - dd * u[2]];
                let aa = perp_d[0] * perp_d[0] + perp_d[1] * perp_d[1] + perp_d[2] * perp_d[2];
                let bb = perp_b[0] * perp_d[0] + perp_b[1] * perp_d[1] + perp_b[2] * perp_d[2];
                let cc = perp_b[0] * perp_b[0] + perp_b[1] * perp_b[1] + perp_b[2] * perp_b[2] - r * r;
                let (mut t0, mut t1) = if aa < 1e-14 {
                    // Parallel to the cylinder's axis.
                    if cc > 0.0 {
                        return;
                    }
                    (f64::MIN, f64::MAX)
                } else {
                    let disc = bb * bb - aa * cc;
                    if disc <= 0.0 {
                        return;
                    }
                    let s = disc.sqrt();
                    ((-bb - s) / aa, (-bb + s) / aa)
                };
                // Between the caps: 0 ≤ (base + t·dir)·u ≤ len.
                if dd.abs() < 1e-14 {
                    if bd < 0.0 || bd > len {
                        return;
                    }
                } else {
                    let (x, y) = ((0.0 - bd) / dd, (len - bd) / dd);
                    t0 = t0.max(x.min(y));
                    t1 = t1.min(x.max(y));
                }
                if t1 > t0 {
                    push(t0, t1);
                }
            }
            Solid::Prism { axis: pa, at, pts, thick } => {
                let (u, v) = ((pa + 1) % 3, (pa + 2) % 3);
                if axis == *pa {
                    if point_in_polygon(pts, [p[u], p[v]]) {
                        push(*at, at + thick);
                    }
                } else if p[*pa] >= *at && p[*pa] <= at + thick {
                    // Along one of the outline's own directions.
                    let (along_u, fixed) = if axis == u { (true, p[v]) } else { (false, p[u]) };
                    let mut iv = Vec::new();
                    polygon_line(pts, along_u, fixed, &mut iv);
                    for (a, b) in iv {
                        push(a, b);
                    }
                }
            }
            Solid::Outside(inner) => {
                // A cavity's wall is metal, so a line lying on it is not inside. Taken a hair to each
                // side: inside only where all four agree.
                let eps = 1e-12 * (hi - lo).abs().max(1e-30);
                let mut iv: Vec<(f64, f64)> = Vec::new();
                for (s1, s2) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let mut q = p;
                    q[t1] += s1 * eps;
                    q[t2] += s2 * eps;
                    let mut part = Vec::new();
                    inner.chord(q, axis, lo, hi, &mut part);
                    merge_intervals(&mut part, 0.0);
                    if s1 < 0.0 && s2 < 0.0 {
                        iv = part;
                    } else {
                        let mut both = Vec::new();
                        for a in &iv {
                            for b in &part {
                                let (x, y) = (a.0.max(b.0), a.1.min(b.1));
                                if y > x {
                                    both.push((x, y));
                                }
                            }
                        }
                        iv = both;
                    }
                }
                merge_intervals(&mut iv, 0.0);
                let mut at = lo;
                for (a, b) in iv {
                    push(at, a);
                    at = b;
                }
                push(at, hi);
            }
            Solid::Mesh { tris } => {
                let mut hits: Vec<f64> = Vec::new();
                for t in tris {
                    if let Some(x) = ray_triangle(p, axis, t) {
                        hits.push(x);
                    }
                }
                hits.sort_by(f64::total_cmp);
                hits.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
                for c in hits.chunks_exact(2) {
                    push(c[0], c[1]);
                }
            }
        }
    }
}

/// Where the line through `p` along `axis` crosses a triangle.
fn ray_triangle(p: P3, axis: usize, t: &[P3; 3]) -> Option<f64> {
    let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
    let (x0, y0) = (t[0][a] - p[a], t[0][b] - p[b]);
    let (x1, y1) = (t[1][a] - p[a], t[1][b] - p[b]);
    let (x2, y2) = (t[2][a] - p[a], t[2][b] - p[b]);
    let w0 = x1 * y2 - x2 * y1;
    let w1 = x2 * y0 - x0 * y2;
    let w2 = x0 * y1 - x1 * y0;
    let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
    let sum = w0 + w1 + w2;
    if !inside || sum.abs() < 1e-300 {
        return None;
    }
    Some((w0 * t[0][axis] + w1 * t[1][axis] + w2 * t[2][axis]) / sum)
}

pub fn point_in_polygon(pts: &[[f64; 2]], q: [f64; 2]) -> bool {
    let n = pts.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        if (a[1] > q[1]) != (b[1] > q[1]) && q[0] < a[0] + (q[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]) {
            inside = !inside;
        }
    }
    inside
}

/// The parts of a polygon on a line of constant `v` (if `along_u`) or constant `u`, by the
/// even-odd rule, so a keyhole's coincident edges cancel. Pairs are added to `out`, unsorted
/// relative to anything already there.
pub fn polygon_line(pts: &[[f64; 2]], along_u: bool, fixed: f64, out: &mut Vec<(f64, f64)>) {
    let (a, f) = if along_u { (0, 1) } else { (1, 0) };
    let n = pts.len();
    let mut cross: Vec<f64> = Vec::new();
    for i in 0..n {
        let (p, q) = (pts[i], pts[(i + 1) % n]);
        if (p[f] <= fixed) != (q[f] <= fixed) {
            cross.push(p[a] + (fixed - p[f]) * (q[a] - p[a]) / (q[f] - p[f]));
        }
    }
    cross.sort_by(f64::total_cmp);
    for c in cross.chunks_exact(2) {
        if c[1] > c[0] {
            out.push((c[0], c[1]));
        }
    }
}

/// Sorts intervals and joins any that touch.
pub fn merge_intervals(v: &mut Vec<(f64, f64)>, tol: f64) {
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1 + tol => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *v = out;
}

/// A first-order Debye relaxation on top of a material's `eps_r`, which is then its value at
/// infinite frequency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Debye {
    pub delta_eps: f64,
    pub tau: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mat {
    Pec,
    /// A metal body: perfect inside, its surface impedance on its skin.
    Metal { sigma: f64 },
    Dielectric { eps_r: f64, mu_r: f64, kappa: f64, debye: Option<Debye> },
}

impl Mat {
    pub fn is_metal(&self) -> bool {
        matches!(self, Mat::Pec | Mat::Metal { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    pub solid: Solid,
    pub mat: Mat,
}

/// What a real metal adds to a perfect one. `r` in ohms per square, `l` in henries per square,
/// for the whole current of a sheet (or of a body's skin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surface {
    pub r: f64,
    pub l: f64,
}

impl Surface {
    /// A metal sheet of `thick`, carrying its current on both faces, fitted at `f`: series R and L
    /// that give its impedance `(γ/2σ)·coth(γt/2)` at that frequency.
    pub fn sheet(sigma: f64, thick: f64, f: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * f;
        let delta = (2.0 / (w * crate::MU0 * sigma)).sqrt();
        let g = num_complex::Complex64::new(1.0, 1.0) / delta;
        let x = g * thick * 0.5;
        // coth(x) from exponentials would overflow on a thick sheet.
        let coth = if x.re > 20.0 { num_complex::Complex64::new(1.0, 0.0) } else { x.cosh() / x.sinh() };
        let z = g / (2.0 * sigma) * coth;
        Self { r: z.re, l: z.im / w }
    }

    /// The skin of a thick body, one side only, at `f`.
    pub fn skin(sigma: f64, f: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * f;
        let r = (w * crate::MU0 / (2.0 * sigma)).sqrt();
        Self { r, l: r / w }
    }
}

/// A flat piece of metal of no thickness, in the plane normal to `axis` at `at`. Outlines are
/// `(axis + 1, axis + 2)` coordinates. The plane has to be one of the grid's lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub axis: usize,
    pub at: f64,
    pub polys: Vec<Vec<[f64; 2]>>,
    /// `None` is perfect metal.
    pub surface: Option<Surface>,
}

/// A thin round wire through points, which have to differ along one axis at a time to run on the
/// grid's lines. Anything else is stepped along lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Wire {
    pub pts: Vec<P3>,
    pub r: f64,
    pub surface: Option<Surface>,
}

/// A lumped port across the gap from `a` to `b` along `axis`, `width_axis` giving the sideways
/// extent of a wide one. `a → b` is the rise of the port's voltage.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub a: P3,
    pub b: P3,
    pub axis: usize,
    pub width_axis: Option<usize>,
    pub width: f64,
    /// A second sideways extent, for a port that fills a box across both.
    pub second: Option<(usize, f64)>,
    pub z0: f64,
}

/// The outer walls: perfect metal, or an absorber this many cells deep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wall {
    Pec,
    Pml(usize),
}

/// Gaussian pulse, carrier `f0`, half-bandwidth `fc`: the same shape openEMS uses, so the two
/// put the same energy in the same band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pulse {
    pub f0: f64,
    pub fc: f64,
}

impl Pulse {
    pub fn delay(&self) -> f64 {
        9.0 / (2.0 * std::f64::consts::PI * self.fc)
    }
    pub fn at(&self, t: f64) -> f64 {
        let t = t - self.delay();
        let s = 3.0 / (2.0 * std::f64::consts::PI * self.fc);
        (2.0 * std::f64::consts::PI * self.f0 * t).cos() * (-(t / s) * (t / s)).exp()
    }
}

/// Makes the model behind a plane the same as it is on that plane, out to the wall: the line a wave
/// port feeds runs on into the absorber, with nothing to reflect from at the plane or the absorber.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extrude {
    pub axis: usize,
    /// The line of `axis` it starts from.
    pub plane: usize,
    /// `+1.0` if the model is on the high side, so the low side is what gets filled.
    pub direction: f64,
}

/// A complete input.
#[derive(Clone, Debug)]
pub struct Problem {
    pub grid: Grid,
    /// xmin, xmax, ymin, ymax, zmin, zmax.
    pub walls: [Wall; 6],
    /// In rising priority: a later body overrides an earlier one where they overlap.
    pub bodies: Vec<Body>,
    pub sheets: Vec<Sheet>,
    pub wires: Vec<Wire>,
    pub ports: Vec<Port>,
    pub extrude: Vec<Extrude>,
    /// Whether to correct cells next to thin metal from the local static field.
    pub subcell: bool,
    /// Where a real metal body is fitted its skin impedance.
    pub design_f: f64,
}

impl Problem {
    pub fn new(grid: Grid) -> Self {
        Self { grid, walls: [Wall::Pml(8); 6], bodies: Vec::new(), sheets: Vec::new(), wires: Vec::new(), ports: Vec::new(), extrude: Vec::new(), subcell: true, design_f: 1e9 }
    }
}
