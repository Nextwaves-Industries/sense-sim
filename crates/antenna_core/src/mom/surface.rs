//! Surface solver: RWG basis on the conformal mesh, mixed-potential EFIE, Galerkin
//! testing. No CFIE, so closed bodies have the usual EFIE interior resonances.
//!
//! Each triangle's three local functions are `g_i(r) = (r - v_i)/(2A)`, and an RWG
//! function is `±l·g_i` on its two triangles. All 3×3 couplings of a triangle pair
//! come from four integrals: `∬G`, `∬(r-c)·G`, `∬(r'-c')·G` and `∬(r-c)·(r'-c')·G`.
//! Far pairs use plain Gauss rules. Near pairs integrate the `1/R` part over the
//! source triangle in closed form (Wilton et al.) and the smooth `(exp(-jkR)-1)/R`
//! rest by quadrature.

use std::collections::HashMap;
use std::f64::consts::PI;

use anyhow::Result;

use crate::geometry::{Geometry, P3, add, cross, dot, norm, scale, sub, unit};
use crate::material::{self, Material};
use crate::{C64, EPS0, MU0};

use super::far::{Moment, PlaneWave, wavenumber};
use super::fill::{Dof, fill_symmetric};
use super::layered::{self, Kernels};
use super::mesher::{SurfaceMesh, mesh_geometry};
use super::quad::{TRI3, TRI6, TRI7, TRI16, TriPoint};
use super::wire::smooth_kernel;
use super::{MeshView, Model};

/// Pairs closer than this many summed radii get singularity extraction.
const NEAR: f64 = 1.6;
/// Pairs closer than this many summed radii get the richer plain rule.
const MID: f64 = 4.0;

#[derive(Clone, Copy)]
struct Edge {
    a: P3,
    t: P3,
    /// In-plane unit normal, pointing out of the triangle.
    out: P3,
    len: f64,
}

#[derive(Clone, Copy)]
struct Tri {
    v: [P3; 3],
    c: P3,
    area: f64,
    n: P3,
    /// Largest distance from the centroid to a vertex.
    radius: f64,
    edges: [Edge; 3],
    far_pts: [P3; 3],
}

impl Tri {
    fn new(v: [P3; 3]) -> Self {
        let c = scale(add(add(v[0], v[1]), v[2]), 1.0 / 3.0);
        let normal = cross(sub(v[1], v[0]), sub(v[2], v[0]));
        let area = 0.5 * norm(normal);
        let n = unit(normal);
        let radius = v.iter().map(|p| norm(sub(*p, c))).fold(0.0, f64::max);
        let edges = [0, 1, 2].map(|i| {
            let (a, b) = (v[i], v[(i + 1) % 3]);
            let t = unit(sub(b, a));
            Edge { a, t, out: cross(t, n), len: norm(sub(b, a)) }
        });
        let at = |w: [f64; 3]| add(add(scale(v[0], w[0]), scale(v[1], w[1])), scale(v[2], w[2]));
        let far_pts = [at(TRI3[0].0), at(TRI3[1].0), at(TRI3[2].0)];
        Self { v, c, area, n, radius, edges, far_pts }
    }

    fn at(&self, w: &[f64; 3]) -> P3 {
        add(add(scale(self.v[0], w[0]), scale(self.v[1], w[1])), scale(self.v[2], w[2]))
    }

    fn reflected(&self) -> Self {
        Self::new(self.v.map(|p| [p[0], p[1], -p[2]]))
    }
}

/// The four integrals of a triangle pair (with the `1/4π`).
#[derive(Clone, Copy, Default)]
struct PairIntegrals {
    g: C64,
    gr: [C64; 3],
    gs: [C64; 3],
    grs: C64,
}

/// `∫1/R dS'` and `∫(r'-c)/R dS'` over `q`, from the point `r`.
fn potentials(q: &Tri, r: P3) -> (f64, P3) {
    let w0 = dot(sub(r, q.v[0]), q.n);
    let rho = sub(r, scale(q.n, w0));
    let w_abs = w0.abs();
    let tiny = 1e-24 * q.radius * q.radius;
    let (mut i0, mut iv) = (0.0, [0.0; 3]);
    for e in &q.edges {
        let d = sub(e.a, rho);
        let s_minus = dot(d, e.t);
        let s_plus = s_minus + e.len;
        let p0 = dot(d, e.out);
        let r02 = p0 * p0 + w0 * w0;
        let r_minus = (s_minus * s_minus + r02).sqrt();
        let r_plus = (s_plus * s_plus + r02).sqrt();
        // R + s, kept accurate when s is negative and R0 small.
        let sum = |s: f64, rr: f64| if s >= 0.0 { rr + s } else { r02 / (rr - s) };
        let f = if r02 > tiny { (sum(s_plus, r_plus) / sum(s_minus, r_minus)).ln() } else { 0.0 };
        let beta = if w_abs > 0.0 && r02 > tiny { (p0 * s_plus / (r02 + w_abs * r_plus)).atan() - (p0 * s_minus / (r02 + w_abs * r_minus)).atan() } else { 0.0 };
        i0 += p0 * f - w_abs * beta;
        iv = add(iv, scale(e.out, 0.5 * (r02 * f + s_plus * r_plus - s_minus * r_minus)));
    }
    (i0, add(iv, scale(sub(rho, q.c), i0)))
}

/// Plain product rule on the full kernel.
fn product_rule(p: &Tri, q: &Tri, k: f64, pp: &[TriPoint], qp: &[TriPoint]) -> PairIntegrals {
    let mut out = PairIntegrals::default();
    for (wi, ai) in pp {
        let r = p.at(wi);
        accumulate_row(&mut out, r, sub(r, p.c), q, k, *ai, qp);
    }
    scale_integrals(out, p.area * q.area / (4.0 * PI))
}

#[inline]
fn accumulate_row(out: &mut PairIntegrals, r: P3, rp: P3, q: &Tri, k: f64, weight: f64, qp: &[TriPoint]) {
    for (wj, aj) in qp {
        let s = q.at(wj);
        let rq = sub(s, q.c);
        let d = sub(r, s);
        let rr = dot(d, d).sqrt();
        let (sn, cs) = (k * rr).sin_cos();
        let g = C64::new(cs, -sn) * (weight * aj / rr);
        out.g += g;
        for m in 0..3 {
            out.gr[m] += g * rp[m];
            out.gs[m] += g * rq[m];
        }
        out.grs += g * dot(rp, rq);
    }
}

/// The cheap far rule, with points cached on the triangles.
fn far_rule(p: &Tri, q: &Tri, k: f64) -> PairIntegrals {
    let mut out = PairIntegrals::default();
    let weight = 1.0 / 9.0;
    for r in &p.far_pts {
        let rp = sub(*r, p.c);
        for s in &q.far_pts {
            let rq = sub(*s, q.c);
            let d = sub(*r, *s);
            let rr = dot(d, d).sqrt();
            let (sn, cs) = (k * rr).sin_cos();
            let g = C64::new(cs, -sn) * (weight / rr);
            out.g += g;
            for m in 0..3 {
                out.gr[m] += g * rp[m];
                out.gs[m] += g * rq[m];
            }
            out.grs += g * dot(rp, rq);
        }
    }
    scale_integrals(out, p.area * q.area / (4.0 * PI))
}

fn scale_integrals(mut a: PairIntegrals, f: f64) -> PairIntegrals {
    a.g *= f;
    a.grs *= f;
    for m in 0..3 {
        a.gr[m] *= f;
        a.gs[m] *= f;
    }
    a
}

/// Singularity extraction: closed form inside, rich rule outside.
fn near_rule(p: &Tri, q: &Tri, k: f64) -> PairIntegrals {
    let mut out = PairIntegrals::default();
    for (wi, ai) in TRI16.iter() {
        let r = p.at(wi);
        let rp = sub(r, p.c);
        let (i0, iv) = potentials(q, r);
        let mut inner = C64::new(i0, 0.0);
        let mut inner_v = [C64::new(iv[0], 0.0), C64::new(iv[1], 0.0), C64::new(iv[2], 0.0)];
        for (wj, aj) in TRI7.iter() {
            let s = q.at(wj);
            let rq = sub(s, q.c);
            let d = sub(r, s);
            let g = smooth_kernel(k, dot(d, d).sqrt()) * (aj * q.area);
            inner += g;
            for m in 0..3 {
                inner_v[m] += g * rq[m];
            }
        }
        out.g += inner * *ai;
        for m in 0..3 {
            out.gr[m] += inner * (*ai * rp[m]);
            out.gs[m] += inner_v[m] * *ai;
            out.grs += inner_v[m] * (*ai * rp[m]);
        }
    }
    scale_integrals(out, p.area / (4.0 * PI))
}

fn pair_integrals(p: &Tri, q: &Tri, k: f64) -> PairIntegrals {
    let d = norm(sub(p.c, q.c));
    let reach = p.radius + q.radius;
    if d < NEAR * reach {
        near_rule(p, q, k)
    } else if d < MID * reach {
        product_rule(p, q, k, &TRI6, &TRI6)
    } else {
        far_rule(p, q, k)
    }
}

/// The 3×3 block for local functions `g_i` on `p` and `g_j` on `q`.
fn efie_block(p: &Tri, q: &Tri, k: f64, jwmu: C64, over_jwe: C64) -> [[C64; 3]; 3] {
    let a = pair_integrals(p, q, k);
    let mut b = [[C64::new(0.0, 0.0); 3]; 3];
    let scalar = a.g * over_jwe * (1.0 / (p.area * q.area));
    let vector = jwmu * (0.25 / (p.area * q.area));
    let dotc = |u: &[C64; 3], v: P3| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    for i in 0..3 {
        let di = sub(p.c, p.v[i]);
        let gs_i = dotc(&a.gs, di);
        for j in 0..3 {
            let dj = sub(q.c, q.v[j]);
            let term = a.grs + dotc(&a.gr, dj) + gs_i + a.g * dot(di, dj);
            b[i][j] = vector * term + scalar;
        }
    }
    b
}

/// The four integrals of the vector potential's kernel and the scalar potential's `∬G`, inside
/// layers: `exp(-jkρ)/ρ` plus what the layers add.
fn layered_rule(p: &Tri, q: &Tri, k: f64, kn: &Kernels, pp: &[TriPoint], qp: &[TriPoint]) -> (PairIntegrals, C64) {
    let mut out = PairIntegrals::default();
    let mut phi = C64::new(0.0, 0.0);
    for (wi, ai) in pp {
        let r = p.at(wi);
        let rp = sub(r, p.c);
        for (wj, aj) in qp {
            let s = q.at(wj);
            let rq = sub(s, q.c);
            let d = sub(r, s);
            let rr = dot(d, d).sqrt();
            let (sn, cs) = (k * rr).sin_cos();
            let free = C64::new(cs, -sn) / rr;
            let w = ai * aj;
            let g = (free + kn.a.whole_at(rr)) * w;
            phi += (free + kn.phi.whole_at(rr)) * w;
            out.g += g;
            for m in 0..3 {
                out.gr[m] += g * rp[m];
                out.gs[m] += g * rq[m];
            }
            out.grs += g * dot(rp, rq);
        }
    }
    let f = p.area * q.area / (4.0 * PI);
    (scale_integrals(out, f), phi * f)
}

/// The closed forms of the free-space `1/R` and of every image, the rest by quadrature.
fn layered_near(p: &Tri, q: &Tri, k: f64, kn: &Kernels) -> (PairIntegrals, C64) {
    let mut out = PairIntegrals::default();
    let mut phi = C64::new(0.0, 0.0);
    for (wi, ai) in TRI16.iter() {
        let r = p.at(wi);
        let rp = sub(r, p.c);
        let (i0, iv) = potentials(q, r);
        let mut inner = C64::new(i0, 0.0);
        let mut inner_v = [C64::new(iv[0], 0.0), C64::new(iv[1], 0.0), C64::new(iv[2], 0.0)];
        let mut inner_phi = C64::new(i0, 0.0);
        let at_depth = |b: f64| if b == 0.0 { (i0, iv) } else { potentials(q, [r[0], r[1], r[2] + b]) };
        for (a, b) in &kn.a.images {
            let (i, v) = at_depth(*b);
            inner += *a * i;
            for m in 0..3 {
                inner_v[m] += *a * v[m];
            }
        }
        for (a, b) in &kn.phi.images {
            inner_phi += *a * at_depth(*b).0;
        }
        for (wj, aj) in TRI7.iter() {
            let s = q.at(wj);
            let rq = sub(s, q.c);
            let d = sub(r, s);
            let rr = dot(d, d).sqrt();
            let free = smooth_kernel(k, rr);
            let w = aj * q.area;
            let g = (free + kn.a.smooth_at(rr)) * w;
            inner += g;
            for m in 0..3 {
                inner_v[m] += g * rq[m];
            }
            inner_phi += (free + kn.phi.smooth_at(rr)) * w;
        }
        out.g += inner * *ai;
        phi += inner_phi * *ai;
        for m in 0..3 {
            out.gr[m] += inner * (*ai * rp[m]);
            out.gs[m] += inner_v[m] * *ai;
            out.grs += inner_v[m] * (*ai * rp[m]);
        }
    }
    let f = p.area / (4.0 * PI);
    (scale_integrals(out, f), phi * f)
}

/// [`efie_block`] inside layers, where the two potentials have kernels of their own.
fn layered_block(p: &Tri, q: &Tri, k: f64, jwmu: C64, over_jwe: C64, kn: &Kernels) -> [[C64; 3]; 3] {
    let d = norm(sub(p.c, q.c));
    let reach = p.radius + q.radius;
    let (a, phi) = if d < NEAR * reach {
        layered_near(p, q, k, kn)
    } else if d < MID * reach {
        layered_rule(p, q, k, kn, &TRI6, &TRI6)
    } else {
        layered_rule(p, q, k, kn, &TRI3, &TRI3)
    };
    let mut b = [[C64::new(0.0, 0.0); 3]; 3];
    let scalar = phi * over_jwe * (1.0 / (p.area * q.area));
    let vector = jwmu * (0.25 / (p.area * q.area));
    let dotc = |u: &[C64; 3], v: P3| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    for i in 0..3 {
        let di = sub(p.c, p.v[i]);
        let gs_i = dotc(&a.gs, di);
        for j in 0..3 {
            let dj = sub(q.c, q.v[j]);
            let term = a.grs + dotc(&a.gr, dj) + gs_i + a.g * dot(di, dj);
            b[i][j] = vector * term + scalar;
        }
    }
    b
}

pub struct SurfaceModel {
    mesh: SurfaceMesh,
    tris: Vec<Tri>,
    images: Vec<Tri>,
    dofs: Vec<Vec<Dof>>,
    nbasis: usize,
    /// Per port: `(unknown, signed edge length)` of its feed edges.
    feeds: Vec<Vec<(usize, f64)>>,
    /// Per triangle, a real metal's data and the share of its surface impedance the
    /// net current sees; `None` for PEC.
    materials: Vec<Option<(Material, f64)>>,
    ground: bool,
    /// Layers around the metal, and how far the metal spans for their tables.
    stack: Option<(crate::stack::Stack, f64)>,
    /// What the mesh check found.
    notes: Vec<String>,
}

/// A triangle's smallest angle below this is a sliver: usually a strip narrower than the mesh.
const SLIVER_DEG: f64 = 5.0;

/// Lossy sheets close enough to another sheet or the ground plane that their current
/// crowds onto the facing sides, where the two-faced `Zs/2` reads the loss low.
fn facing_sheets(geom: &Geometry, lambda: f64) -> Vec<String> {
    let close = 0.05 * lambda;
    let flat_axis = |s: &crate::geometry::Solid| {
        let (lo, hi) = s.shape.bounds();
        (0..3).find(|k| hi[*k] <= lo[*k]).map(|k| (k, lo, hi))
    };
    let lossy = |s: &crate::geometry::Solid| material::by_name(&s.material).is_some_and(|m| matches!(m.kind, material::MatKind::Metal { .. }));
    let mut out = Vec::new();
    for (i, s) in geom.solids.iter().enumerate() {
        if !s.shape.is_sheet() || !lossy(s) {
            continue;
        }
        let Some((k, lo, hi)) = flat_axis(s) else { continue };
        let over_ground = geom.ground && k == 2 && lo[2] > 0.0 && lo[2] < close;
        let faces = geom.solids.iter().enumerate().find(|(j, o)| {
            *j != i
                && o.shape.is_sheet()
                && flat_axis(o).is_some_and(|(ko, lo2, hi2)| ko == k && (lo2[k] - lo[k]).abs() > 0.0 && (lo2[k] - lo[k]).abs() < close && (0..3).all(|a| a == k || (lo2[a] < hi[a] && lo[a] < hi2[a])))
        });
        if over_ground || faces.is_some() {
            let other = faces.map_or("the ground plane".to_string(), |(_, o)| format!("'{}'", o.name));
            out.push(format!("'{}' faces {other} closely, so its current crowds onto one side and its loss can be up to twice what is shown: sheets are modelled with the current shared between two faces.", s.name));
        }
    }
    out
}

/// What attaches to one mesh edge: a triangle and the local index of the vertex
/// opposite the edge.
type Wing = (usize, u8);

impl SurfaceModel {
    /// Meshes `geom` with edges no longer than `h`, then numbers the RWG functions.
    pub fn build(geom: &Geometry, h: f64, lambda: f64, stack: Option<crate::stack::Stack>) -> Result<Self> {
        let mesh = mesh_geometry(geom, h, lambda)?;
        let (lo, hi) = geom.bounds();
        let stack = stack.map(|s| (s, norm(sub(hi, lo))));
        let tris: Vec<Tri> = mesh.tris.iter().map(|t| Tri::new(t.map(|i| mesh.verts[i as usize]))).collect();
        let images = if geom.ground { tris.iter().map(Tri::reflected).collect() } else { Vec::new() };
        // A sheet's net current is shared by its two faces, so Zs/2 for what it loses. A
        // closed body has one outer face, and the whole of Zs.
        let materials = mesh
            .solid
            .iter()
            .map(|s| s.and_then(|i| material::by_name(&geom.solids[i].material).filter(|m| !matches!(m.kind, material::MatKind::Pec)).map(|m| (m, if geom.solids[i].shape.meshed_as_sheet() { 0.5 } else { 1.0 }))))
            .collect();
        let mut notes = facing_sheets(geom, lambda);
        let quality = mesh.quality();
        if quality.min_angle_deg < SLIVER_DEG {
            notes.push(format!("The surface mesh has sliver triangles, the sharpest with a {:.1}° corner: usually a strip narrower than the mesh edge. Raise the edges per wavelength until the strip is meshed across.", quality.min_angle_deg));
        }

        let mut wings: HashMap<(u32, u32), Vec<Wing>> = HashMap::new();
        for (ti, t) in mesh.tris.iter().enumerate() {
            for i in 0..3 {
                let (a, b) = (t[(i + 1) % 3], t[(i + 2) % 3]);
                wings.entry((a.min(b), a.max(b))).or_default().push((ti, i as u8));
            }
        }
        let mut keys: Vec<(u32, u32)> = wings.keys().copied().collect();
        keys.sort_unstable();

        let mut dofs: Vec<Vec<Dof>> = vec![Vec::new(); tris.len()];
        let mut nbasis = 0usize;
        // Every unknown on an edge: (unknown, positive wing, negative wing if any).
        let mut on_edge: HashMap<(u32, u32), Vec<(usize, Wing, Option<Wing>)>> = HashMap::new();
        for key in &keys {
            let list = &wings[key];
            let (pa, pb) = (mesh.verts[key.0 as usize], mesh.verts[key.1 as usize]);
            let len = norm(sub(pa, pb));
            if geom.ground && pa[2] == 0.0 && pb[2] == 0.0 {
                // On the plane: each sheet ending here continues into its image.
                for &w in list {
                    dofs[w.0].push(Dof { basis: nbasis as u32, local: w.1, weight: len });
                    on_edge.entry(*key).or_default().push((nbasis, w, None));
                    nbasis += 1;
                }
            } else {
                for &w in &list[1..] {
                    dofs[list[0].0].push(Dof { basis: nbasis as u32, local: list[0].1, weight: len });
                    dofs[w.0].push(Dof { basis: nbasis as u32, local: w.1, weight: -len });
                    on_edge.entry(*key).or_default().push((nbasis, list[0], Some(w)));
                    nbasis += 1;
                }
            }
        }

        let mut feeds = Vec::with_capacity(geom.ports.len());
        for (edges, port) in mesh.feeds.iter().zip(&geom.ports) {
            let gap = unit(sub(port.b, port.a));
            let mut list = Vec::with_capacity(edges.len());
            for e in edges {
                let key = (e[0].min(e[1]), e[0].max(e[1]));
                let (pa, pb) = (mesh.verts[key.0 as usize], mesh.verts[key.1 as usize]);
                let mid = scale(add(pa, pb), 0.5);
                let Some(unknowns) = on_edge.get(&key) else {
                    anyhow::bail!("port '{}' feeds an edge that carries no current", port.name);
                };
                // Of the functions across this edge, the one lined up with the gap.
                let opposite = |w: &Wing| tris[w.0].v[w.1 as usize];
                let mut best = (0usize, 0.0f64);
                for (basis, plus, minus) in unknowns {
                    let mut flow = unit(sub(mid, opposite(plus)));
                    if let Some(m) = minus {
                        flow = add(flow, unit(sub(opposite(m), mid)));
                    }
                    let along = dot(flow, gap);
                    if along.abs() > best.1.abs() {
                        best = (*basis, along);
                    }
                }
                if best.1 == 0.0 {
                    anyhow::bail!("port '{}' runs along its feed edges instead of across them", port.name);
                }
                list.push((best.0, best.1.signum() * norm(sub(pa, pb))));
            }
            feeds.push(list);
        }
        Ok(Self { mesh, tris, images, dofs, nbasis, feeds, materials, ground: geom.ground, stack, notes })
    }

    /// Current density at a point of a triangle.
    fn current_at(&self, ti: usize, r: P3, x: &[C64]) -> [C64; 3] {
        let t = &self.tris[ti];
        let mut j = [C64::new(0.0, 0.0); 3];
        for d in &self.dofs[ti] {
            let rho = sub(r, t.v[d.local as usize]);
            let f = x[d.basis as usize] * (d.weight / (2.0 * t.area));
            j[0] += f * rho[0];
            j[1] += f * rho[1];
            j[2] += f * rho[2];
        }
        j
    }
}

impl Model for SurfaceModel {
    fn unknowns(&self) -> usize {
        self.nbasis
    }

    fn matrix(&self, f: f64) -> Vec<C64> {
        let w = 2.0 * PI * f;
        let k = wavenumber(f);
        let jwmu = C64::new(0.0, w * MU0);
        let over_jwe = C64::new(0.0, -1.0 / (w * EPS0));
        let tables = self.stack.as_ref().map(|(s, span)| layered::kernels(s, f, *span));
        fill_symmetric::<3>(self.nbasis, &self.dofs, |p, q| {
            let tp = &self.tris[p];
            let mut b = match &tables {
                Some(kn) => layered_block(tp, &self.tris[q], k, jwmu, over_jwe, kn),
                None => efie_block(tp, &self.tris[q], k, jwmu, over_jwe),
            };
            if self.ground {
                // The image of an RWG function is minus its reflection.
                let img = efie_block(tp, &self.images[q], k, jwmu, over_jwe);
                for i in 0..3 {
                    for j in 0..3 {
                        b[i][j] -= img[i][j];
                    }
                }
            }
            if p == q
                && let Some((m, share)) = &self.materials[p]
            {
                let zs = m.surface_impedance(f) * *share;
                let scale_factor = tp.area / (4.0 * tp.area * tp.area);
                for i in 0..3 {
                    for j in 0..3 {
                        let (vi, vj) = (tp.v[i], tp.v[j]);
                        let sum: f64 = TRI3.iter().map(|(wt, a)| a * dot(sub(tp.at(wt), vi), sub(tp.at(wt), vj))).sum();
                        b[i][j] += zs * (scale_factor * sum);
                    }
                }
            }
            b
        })
    }

    fn port_rhs(&self, port: usize) -> Vec<C64> {
        let mut v = vec![C64::new(0.0, 0.0); self.nbasis];
        for &(basis, weight) in &self.feeds[port] {
            v[basis] += weight;
        }
        v
    }

    fn port_current(&self, port: usize, x: &[C64]) -> C64 {
        self.feeds[port].iter().map(|&(basis, weight)| x[basis] * weight).sum()
    }

    fn plane_wave_rhs(&self, wave: &PlaneWave) -> Vec<C64> {
        let mut v = vec![C64::new(0.0, 0.0); self.nbasis];
        for (t, dofs) in self.tris.iter().zip(&self.dofs) {
            for (wt, a) in TRI6.iter() {
                let r = t.at(wt);
                let e = wave.at(r);
                for d in dofs {
                    let rho = sub(r, t.v[d.local as usize]);
                    let along = e[0] * rho[0] + e[1] * rho[1] + e[2] * rho[2];
                    v[d.basis as usize] += along * (a * d.weight * 0.5);
                }
            }
        }
        v
    }

    fn moments(&self, x: &[C64]) -> Vec<Moment> {
        let mut out = Vec::with_capacity(self.tris.len() * TRI6.len() * if self.ground { 2 } else { 1 });
        for (ti, t) in self.tris.iter().enumerate() {
            for (wt, a) in TRI6.iter() {
                let r = t.at(wt);
                let j = self.current_at(ti, r, x);
                let w = a * t.area;
                let m = Moment { pos: r, j: [j[0] * w, j[1] * w, j[2] * w] };
                out.push(m);
                if self.ground {
                    out.push(m.image());
                }
            }
        }
        out
    }

    fn element_currents(&self, x: &[C64]) -> Vec<f64> {
        (0..self.tris.len())
            .map(|ti| {
                let j = self.current_at(ti, self.tris[ti].c, x);
                (j[0].norm_sqr() + j[1].norm_sqr() + j[2].norm_sqr()).sqrt()
            })
            .collect()
    }

    fn view(&self) -> MeshView {
        MeshView { verts: self.mesh.verts.clone(), tris: self.mesh.tris.clone(), segments: Vec::new() }
    }

    fn warnings(&self, _lambda: f64) -> Vec<String> {
        self.notes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MomOptions, MomProblem};
    use super::*;
    use crate::C0;
    use crate::farfield::{FarField, Pol, Quantity};
    use crate::geometry::{PortDef, Shape, Solid};

    /// λ = 1 m throughout.
    const F: f64 = C0;

    fn sheet(name: &str, min: P3, max: P3) -> Solid {
        Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min, max } }
    }
    fn port(a: P3, b: P3, width: f64, z0: f64) -> PortDef {
        PortDef { name: "feed".into(), a, b, width, z0 }
    }
    fn opts(edges: f64) -> MomOptions {
        MomOptions { edges_per_lambda: edges, ..Default::default() }
    }

    /// Spherical Bessel `j_n` and `y_n` by upward recurrence (fine for n ≲ x + 15
    /// on `y`; `j` comes from downward recurrence for stability).
    fn sph_bessel(nmax: usize, x: f64) -> (Vec<f64>, Vec<f64>) {
        let mut y = vec![0.0; nmax + 2];
        y[0] = -x.cos() / x;
        y[1] = -x.cos() / (x * x) - x.sin() / x;
        for n in 1..=nmax {
            y[n + 1] = (2 * n + 1) as f64 / x * y[n] - y[n - 1];
        }
        let start = nmax + 40 + x as usize;
        let mut j = vec![0.0; start + 2];
        j[start] = 1e-30;
        for n in (1..=start).rev() {
            j[n - 1] = (2 * n + 1) as f64 / x * j[n] - j[n + 1];
        }
        let norm0 = x.sin() / x / j[0];
        (j.iter().take(nmax + 2).map(|v| v * norm0).collect(), y)
    }

    /// Exact monostatic RCS of a PEC sphere, `σ/πa²`, by the Mie series.
    fn mie_monostatic(x: f64) -> f64 {
        let nmax = (x + 4.0 * x.cbrt() + 15.0) as usize;
        let (j, y) = sph_bessel(nmax, x);
        let mut sum = C64::new(0.0, 0.0);
        for n in 1..=nmax {
            let nf = n as f64;
            let h = C64::new(j[n], y[n]);
            let hm = C64::new(j[n - 1], y[n - 1]);
            // [x·f_n(x)]' = x·f_{n-1}(x) - n·f_n(x).
            let xj_d = x * j[n - 1] - nf * j[n];
            let xh_d = hm * x - h * nf;
            let an = C64::new(j[n], 0.0) / h;
            let bn = C64::new(xj_d, 0.0) / xh_d;
            let sign = if n % 2 == 0 { 1.0 } else { -1.0 };
            sum += (an - bn) * (sign * (2.0 * nf + 1.0));
        }
        sum.norm_sqr() / (x * x)
    }

    #[test]
    fn mie_series_has_the_right_limits() {
        // Rayleigh: σ/πa² = 9x⁴. Optical: tends to 1.
        let x = 0.02;
        assert!((mie_monostatic(x) / (9.0 * x.powi(4)) - 1.0).abs() < 0.01, "{}", mie_monostatic(x));
        assert!((mie_monostatic(60.0) - 1.0).abs() < 0.15, "{}", mie_monostatic(60.0));
        // Published peak near ka = 1.
        assert!((mie_monostatic(1.0) - 3.65).abs() < 0.1, "{}", mie_monostatic(1.0));
    }

    fn sphere_rcs(ka: f64, edges: f64) -> (f64, f64, usize) {
        let r = ka / (2.0 * PI);
        let g = Geometry { solids: vec![Solid { name: "ball".into(), material: "PEC".into(), shape: Shape::Sphere { c: [0.0, 0.0, 0.0], r } }], ..Default::default() };
        let m = MomProblem::new(&g, F, opts(edges)).unwrap();
        let s = m.rcs(F, 0.0, 0.0, C64::new(1.0, 0.0), C64::new(0.0, 0.0), &[(0.0, 0.0)]).unwrap()[0];
        (s / (PI * r * r), mie_monostatic(ka), m.unknowns())
    }

    #[test]
    fn t10_sphere_against_the_mie_series() {
        for ka in [1.0, 2.0] {
            let (got, want, n) = sphere_rcs(ka, 12.0);
            let (fine, _, nf) = sphere_rcs(ka, 24.0);
            println!("T10 ka = {ka}: σ/πa² = {got:.4} ({n} unknowns), {fine:.4} ({nf} unknowns), Mie {want:.4}; errors {:.2} %, {:.2} %", (got / want - 1.0) * 100.0, (fine / want - 1.0) * 100.0);
            assert!((got / want - 1.0).abs() < 0.085, "ka = {ka}: {got} vs {want}");
            assert!((fine / want - 1.0).abs() < 0.02, "ka = {ka}: {fine} vs {want}");
        }
    }

    /// A survey behind the mesh warnings: scattering against density, conditioning near an
    /// interior resonance, the strip dipole's resonance against density.
    /// `cargo test -p antenna_core surface_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn surface_survey() {
        for edges in [4.0, 6.0, 8.0, 10.0, 12.0] {
            let errors: Vec<String> = [1.0, 2.0].iter().map(|ka| { let (g, w, _) = sphere_rcs(*ka, edges); format!("ka {ka}: {:+.1} %", (g / w - 1.0) * 100.0) }).collect();
            println!("sphere at {edges} edges/λ: {}", errors.join(", "));
        }
        // The first interior resonance of a PEC sphere is at ka = 2.744.
        for ka in [1.0, 2.0, 2.6, 2.7, 2.74, 2.76, 2.8, 2.9, 3.0, 3.5, 4.0, 4.49, 4.6] {
            let r = ka / (2.0 * PI);
            let g = Geometry { solids: vec![Solid { name: "ball".into(), material: "PEC".into(), shape: Shape::Sphere { c: [0.0, 0.0, 0.0], r } }], ..Default::default() };
            let m = MomProblem::new(&g, F, opts(14.0)).unwrap();
            let s = m.rcs(F, 0.0, 0.0, C64::new(1.0, 0.0), C64::new(0.0, 0.0), &[(0.0, 0.0)]).unwrap()[0];
            println!("sphere ka {ka}: rcond {:.2e}, σ/πa² {:.3} against Mie {:.3}, {} unknowns", m.worst_rcond().unwrap().0, s / (PI * r * r), mie_monostatic(ka), m.unknowns());
        }
        for t in crate::templates::all() {
            let Ok(p) = crate::project::Project::from_template(t.id, None) else { continue };
            let Ok(g) = p.geometry() else { continue };
            let fit = g.solver_fit();
            if !fit.wire_mom && !fit.surface_mom {
                continue;
            }
            let o = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, ..Default::default() };
            let m = MomProblem::new(&g, p.setup.f_hi, o).unwrap();
            let _ = m.solve(p.f0(), false).unwrap();
            let quality = if fit.surface_mom { super::super::mesher::mesh_geometry(&g, C0 / p.setup.f_hi / p.setup.edges_per_lambda, C0 / p.setup.f_hi).unwrap().quality().min_angle_deg } else { 0.0 };
            println!("{:<18} {:>6} unknowns, rcond {:.2e}, sharpest corner {quality:.1}°", t.id, m.unknowns(), m.worst_rcond().unwrap().0);
        }
        let reactance_zero = |o: MomOptions, g: &Geometry| {
            let m = MomProblem::new(g, 1.3e9, o).unwrap();
            let x = |f: f64| m.solve(f, false).unwrap().z[0][0].im;
            let (mut a, mut b) = (0.9e9, 1.1e9);
            let (mut xa, mut xb) = (x(a), x(b));
            for _ in 0..7 {
                let c = a - xa * (b - a) / (xb - xa);
                let xc = x(c);
                if xc < 0.0 {
                    (a, xa) = (c, xc);
                } else {
                    (b, xb) = (c, xc);
                }
            }
            (a - xa * (b - a) / (xb - xa), m.unknowns())
        };
        for id in ["strip_dipole", "dipole"] {
            let p = crate::project::Project::from_template(id, Some("1GHz")).unwrap();
            let g = p.geometry().unwrap();
            for d in [6.0, 8.0, 10.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0, 96.0, 128.0, 192.0, 256.0, 384.0] {
                if id == "dipole" && d > 100.0 {
                    continue;
                }
                let (f, n) = reactance_zero(MomOptions { segments_per_lambda: d, edges_per_lambda: d, ..Default::default() }, &g);
                println!("{id} at {d}/λ of 1.3 GHz: X = 0 at {:.2} MHz, {n} unknowns", f / 1e6);
            }
        }
    }

    fn strip_dipole(len: f64, width: f64, gap: f64) -> Geometry {
        let w = width / 2.0;
        Geometry {
            solids: vec![sheet("up", [-w, 0.0, gap / 2.0], [w, 0.0, len / 2.0]), sheet("down", [-w, 0.0, -len / 2.0], [w, 0.0, -gap / 2.0])],
            ports: vec![port([0.0, 0.0, -gap / 2.0], [0.0, 0.0, gap / 2.0], width, 73.0)],
            ..Default::default()
        }
    }

    fn equivalent_wire(len: f64, a: f64) -> Geometry {
        Geometry {
            solids: vec![Solid { name: "w".into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[0.0, 0.0, -len / 2.0], [0.0, 0.0, len / 2.0]], r: a } }],
            ports: vec![port([0.0, 0.0, -0.01], [0.0, 0.0, 0.01], 0.0, 73.0)],
            ..Default::default()
        }
    }

    #[test]
    fn t11_strip_dipole_matches_its_equivalent_wire() {
        let (len, w, gap) = (0.47, 1.0 / 50.0, 1.0 / 60.0);
        let problem = MomProblem::new(&strip_dipole(len, w, gap), F, opts(40.0)).unwrap();
        let p = problem.solve(F, true).unwrap();
        let z = p.z[0][0];
        let wire = MomProblem::new(&equivalent_wire(len, w / 4.0), F, MomOptions { segments_per_lambda: 40.0, ..Default::default() }).unwrap().solve(F, false).unwrap().z[0][0];
        let delta = (z - wire).norm() / wire.norm();
        let m = p.far[0].metrics();
        println!("T11 strip Z = {z:.2} ({} unknowns), wire a = w/4 Z = {wire:.2}, |ΔZ|/|Z| = {:.1} %, D = {:.3} dBi", problem.unknowns(), delta * 100.0, m.peak_directivity_dbi);
        assert!(delta < 0.02, "{z} vs {wire}");
        assert!((m.peak_directivity_dbi - 2.15).abs() < 0.05, "{}", m.peak_directivity_dbi);
    }

    #[test]
    fn t12_strip_dipole_conserves_power() {
        let z0: f64 = 73.0;
        let p = MomProblem::new(&strip_dipole(0.47, 1.0 / 50.0, 1.0 / 60.0), F, opts(40.0)).unwrap().solve(F, true).unwrap();
        let z = p.z[0][0];
        let vs = 2.0 * (2.0 * z0).sqrt();
        let i = vs / (z + z0);
        let terminal = 0.5 * (z * i * i.conj()).re;
        let ff = &p.far[0];
        let ratio = ff.p_rad() / terminal;
        println!("T12 ½Re(VI*) = {terminal:.5} W, p_rad = {:.5} W, ratio {ratio:.4}, rad_eff {:.4}", ff.p_rad(), ff.rad_efficiency());
        assert!((ratio - 1.0).abs() < 0.03, "{ratio}");
    }

    #[test]
    fn t13_flat_plate_broadside_rcs() {
        let g = Geometry { solids: vec![sheet("plate", [-1.0, -1.0, 0.0], [1.0, 1.0, 0.0])], ..Default::default() };
        let m = MomProblem::new(&g, F, opts(12.0)).unwrap();
        let s = m.rcs(F, 0.0, 0.0, C64::new(1.0, 0.0), C64::new(0.0, 0.0), &[(0.0, 0.0)]).unwrap()[0];
        let want = 4.0 * PI * 16.0;
        let db = 10.0 * (s / want).log10();
        println!("T13 plate RCS {s:.2} m² vs PO {want:.2} m²: {db:+.2} dB, {} unknowns", m.unknowns());
        assert!(db.abs() < 0.4, "{db}");
    }

    fn patch(side: f64, height: f64, ground_side: f64, offset: f64) -> Geometry {
        let (s, g) = (side / 2.0, ground_side / 2.0);
        Geometry {
            solids: vec![sheet("ground", [-g, -g, 0.0], [g, g, 0.0]), sheet("patch", [-s, -s, height], [s, s, height])],
            ports: vec![port([0.0, offset, 0.0], [0.0, offset, height], 1.0 / 80.0, 50.0)],
            ..Default::default()
        }
    }

    #[test]
    fn t14_patch_in_air() {
        // Resonance by the reactance crossing, over a coarse sweep of the patch size.
        let mut best = (f64::MAX, 0.0, C64::new(0.0, 0.0));
        for side in [0.42, 0.43, 0.44, 0.45, 0.46, 0.47, 0.48, 0.49] {
            let z = MomProblem::new(&patch(side, 0.03, 1.0, 0.08), F, opts(14.0)).unwrap().solve(F, false).unwrap().z[0][0];
            // Peak of the resistance is the resonance of a probe-fed patch.
            println!("T14 side {side}: Z = {z:.1}");
            if -z.re < best.0 {
                best = (-z.re, side, z);
            }
        }
        let side = best.1;
        let problem = MomProblem::new(&patch(side, 0.03, 1.0, 0.08), F, opts(14.0)).unwrap();
        let p = problem.solve(F, true).unwrap();
        let m = p.far[0].metrics();
        println!("T14 resonant side {side}: Z = {:.1}, D = {:.2} dBi at θ = {}, F/B {:?}, {} unknowns, rad_eff {:.3}", p.z[0][0], m.peak_directivity_dbi, m.peak_theta, m.front_to_back_db, problem.unknowns(), m.rad_efficiency);
        assert!((6.0..10.0).contains(&m.peak_directivity_dbi), "{}", m.peak_directivity_dbi);
        assert!(m.peak_theta < 20.0, "peak at θ = {}", m.peak_theta);
    }

    #[test]
    fn t15_image_theory_agrees_with_a_mirrored_model() {
        let (s, h, y) = (0.15, 0.1, 0.05);
        let w = 1.0 / 60.0;
        let link = h / 4.0;
        let over_ground = Geometry {
            solids: vec![sheet("plate", [-s, -s, h], [s, s, h]), sheet("post", [-w / 2.0, y, 0.0], [w / 2.0, y, link])],
            ports: vec![port([0.0, y, link], [0.0, y, h], w, 50.0)],
            ground: true,
            ..Default::default()
        };
        // The explicit mirror: both plates, one post through z = 0, two gaps in series (odd mode).
        let mirrored = Geometry {
            solids: vec![sheet("plate", [-s, -s, h], [s, s, h]), sheet("mirror", [-s, -s, -h], [s, s, -h]), sheet("post", [-w / 2.0, y, -link], [w / 2.0, y, link])],
            ports: vec![port([0.0, y, link], [0.0, y, h], w, 50.0), port([0.0, y, -h], [0.0, y, -link], w, 50.0)],
            ..Default::default()
        };
        let a = MomProblem::new(&over_ground, F, opts(16.0)).unwrap().solve(F, true).unwrap();
        let b = MomProblem::new(&mirrored, F, opts(16.0)).unwrap().solve(F, false).unwrap();
        let (z, zm) = (a.z[0][0], b.z[0][0] + b.z[0][1]);
        let delta = (z - zm).norm() / zm.norm();
        println!("T15 over ground Z = {z:.2}, mirrored Z11+Z12 = {zm:.2}, difference {:.2} %", delta * 100.0);
        assert!(delta < 0.05, "{z} vs {zm}");
        let ff: &FarField = &a.far[0];
        assert!(ff.ground);
        assert_eq!(ff.e_theta[ff.theta.len() - 1][0].norm(), 0.0);
        assert!(ff.value_db(0, 0, Quantity::Directivity, Pol::Total) > 0.0);
    }

    #[test]
    fn junctions_and_power() {
        // A PIFA: plate, shorting strip and ground plate meet in T-junctions.
        let (l, w, h, xf) = (0.2, 0.1, 0.05, 0.045);
        let g = Geometry {
            solids: vec![sheet("ground", [-0.15, -0.2, 0.0], [0.45, 0.2, 0.0]), sheet("top", [0.0, -w / 2.0, h], [l, w / 2.0, h]), sheet("short", [0.0, -w / 2.0, 0.0], [0.0, w / 2.0, h])],
            ports: vec![port([xf, 0.0, 0.0], [xf, 0.0, h], 1.0 / 80.0, 50.0)],
            ..Default::default()
        };
        let problem = MomProblem::new(&g, F, opts(20.0)).unwrap();
        let p = problem.solve(F, true).unwrap();
        let eff = p.far[0].rad_efficiency();
        println!("PIFA Z = {:.1}, rad_eff {eff:.4}, {} unknowns", p.z[0][0], problem.unknowns());
        assert!((eff - 1.0).abs() < 0.03, "{eff}");
        assert_eq!(p.current.len(), problem.view().tris.len());
        assert!(p.current.iter().all(|c| c.is_finite()));
    }

    #[test]
    fn a_lossy_sheet_loses_power() {
        let (len, w) = (0.47, 1.0 / 50.0);
        let run = |material: &str| {
            let mut g = strip_dipole(len, w, 1.0 / 60.0);
            for s in g.solids.iter_mut() {
                s.material = material.into();
            }
            let p = MomProblem::new(&g, F, opts(40.0)).unwrap().solve(F, true).unwrap();
            (p.far[0].rad_efficiency(), p.z[0][0])
        };
        let (pec, _) = run("PEC");
        let (copper, _) = run("Copper");
        let (steel, z) = run("Stainless steel");
        // With a sinusoidal current, the loss resistance at the feed is about R'·L/2, and
        // a strip's two faces give R' = Rs/(2w).
        let rs = material::by_name("Stainless steel").unwrap().surface_impedance(F).re;
        let want = rs / (2.0 * w) * len / 2.0;
        let got = (1.0 - steel) * z.re;
        println!("lossy strip rad_eff: PEC {pec:.4}, copper {copper:.4}, stainless {steel:.4}; loss resistance {got:.3} Ω, estimate {want:.3} Ω");
        assert!((pec - 1.0).abs() < 0.01, "{pec}");
        assert!(steel < copper && copper < pec, "{steel} {copper} {pec}");
        // Current crowds to a strip's edges, so the estimate is a lower bound.
        assert!((1.0..1.25).contains(&(got / want)), "{got} vs {want}");
    }

    /// A free-standing sheet shares its current between two faces, so it loses half what
    /// one face carrying all of it would. A closed body still has one face: same metal,
    /// same perimeter, and the strip reads close to the cylinder.
    #[test]
    fn a_free_standing_sheet_shares_its_current_between_two_faces() {
        let (len, w) = (0.47, 1.0 / 50.0);
        let loss = |g: &Geometry| {
            let mut g = g.clone();
            g.solids.iter_mut().for_each(|s| s.material = "Stainless steel".into());
            let p = MomProblem::new(&g, F, opts(40.0)).unwrap().solve(F, true).unwrap();
            (1.0 - p.far[0].rad_efficiency()) * p.z[0][0].re
        };
        let strip = loss(&strip_dipole(len, w, 1.0 / 60.0));
        // A cylinder of the strip's perimeter: one face all round, R' = Rs/(2πr) = Rs/(2w).
        let r = w / PI;
        let gap = 1.0 / 60.0;
        let cyl = |name: &str, a: P3, b: P3| Solid { name: name.into(), material: "PEC".into(), shape: Shape::Cylinder { a, b, r } };
        let rod = Geometry { solids: vec![cyl("up", [0.0, 0.0, gap / 2.0], [0.0, 0.0, len / 2.0]), cyl("down", [0.0, 0.0, -len / 2.0], [0.0, 0.0, -gap / 2.0])], ports: vec![port([0.0, 0.0, -gap / 2.0], [0.0, 0.0, gap / 2.0], w, 73.0)], ..Default::default() };
        let round = loss(&rod);
        println!("loss resistance: strip {strip:.3} Ω, cylinder of the same perimeter {round:.3} Ω, ratio {:.3}", strip / round);
        // Sheets taking the whole of Zs read about twice the cylinder.
        assert!((0.8..1.25).contains(&(strip / round)), "{strip} vs {round}");
    }

    #[test]
    fn scattering_over_ground_follows_image_theory() {
        let (len, w) = (0.46, 0.02);
        let unit = C64::new(1.0, 0.0);
        let zero = C64::new(0.0, 0.0);
        let free = Geometry { solids: vec![sheet("strip", [-w / 2.0, 0.0, -len / 2.0], [w / 2.0, 0.0, len / 2.0])], ..Default::default() };
        let over = Geometry { solids: vec![sheet("strip", [-w / 2.0, 0.0, 0.0], [w / 2.0, 0.0, len / 2.0])], ground: true, ..Default::default() };
        let a = MomProblem::new(&free, F, opts(30.0)).unwrap().rcs(F, 90.0, 90.0, unit, zero, &[(90.0, 90.0)]).unwrap()[0];
        let b = MomProblem::new(&over, F, opts(30.0)).unwrap().rcs(F, 90.0, 90.0, unit, zero, &[(90.0, 90.0)]).unwrap()[0];
        println!("strip grazing RCS over ground {b:.4} m², free-space double {a:.4} m², ratio {:.3} (want 4)", b / a);
        assert!((b / a - 4.0).abs() < 0.2, "{}", b / a);
    }

    #[test]
    fn wire_and_surface_mixes_and_dielectrics_are_refused() {
        let mut g = strip_dipole(0.47, 0.02, 0.02);
        g.solids.push(Solid { name: "w".into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], r: 0.001 } });
        assert!(MomProblem::new(&g, F, opts(12.0)).err().unwrap().to_string().contains("mixes"));
        let mut g = strip_dipole(0.47, 0.02, 0.02);
        g.solids[0].material = "FR-4".into();
        assert!(MomProblem::new(&g, F, opts(12.0)).err().unwrap().to_string().contains("dielectric"));
    }

    #[test]
    fn a_port_can_feed_a_cylinder_and_a_sphere() {
        // A fat dipole of two cylinders, end to end.
        let cyl = |name: &str, a: P3, b: P3| Solid { name: name.into(), material: "PEC".into(), shape: Shape::Cylinder { a, b, r: 0.02 } };
        let g = Geometry { solids: vec![cyl("up", [0.0, 0.0, 0.02], [0.0, 0.0, 0.24]), cyl("down", [0.0, 0.0, -0.24], [0.0, 0.0, -0.02])], ports: vec![port([0.0, 0.0, -0.02], [0.0, 0.0, 0.02], 0.02, 50.0)], ..Default::default() };
        let problem = MomProblem::new(&g, F, opts(20.0)).unwrap();
        let p = problem.solve(F, true).unwrap();
        let m = p.far[0].metrics();
        println!("cylinder dipole Z = {:.1}, D = {:.2} dBi, rad_eff {:.3}, {} unknowns", p.z[0][0], m.peak_directivity_dbi, m.rad_efficiency, problem.unknowns());
        assert!((m.peak_directivity_dbi - 2.15).abs() < 0.4, "{}", m.peak_directivity_dbi);
        assert!((m.rad_efficiency - 1.0).abs() < 0.05, "{}", m.rad_efficiency);

        // Two spheres joined by a strip that attaches to mesh edges.
        let ball = |name: &str, z: f64| Solid { name: name.into(), material: "PEC".into(), shape: Shape::Sphere { c: [0.0, 0.0, z], r: 0.1 } };
        let g = Geometry { solids: vec![ball("up", 0.14), ball("down", -0.14)], ports: vec![port([0.0, 0.0, -0.04], [0.0, 0.0, 0.04], 0.0, 50.0)], ..Default::default() };
        let problem = MomProblem::new(&g, F, opts(14.0)).unwrap();
        let p = problem.solve(F, true).unwrap();
        println!("sphere dipole Z = {:.1}, rad_eff {:.3}", p.z[0][0], p.far[0].rad_efficiency());
        assert!((p.far[0].rad_efficiency() - 1.0).abs() < 0.1, "{}", p.far[0].rad_efficiency());
    }

    #[test]
    fn near_integrals_match_brute_force() {
        let p = Tri::new([[0.0, 0.0, 0.0], [0.08, 0.0, 0.0], [0.02, 0.07, 0.0]]);
        let k = 2.0 * PI;
        let cases = [("self", p), ("edge neighbour", Tri::new([[0.08, 0.0, 0.0], [0.0, 0.0, 0.0], [0.05, -0.07, 0.0]])), ("across a fold", Tri::new([[0.08, 0.0, 0.0], [0.0, 0.0, 0.0], [0.04, 0.0, 0.07]])), ("parallel, close", Tri::new([[0.0, 0.0, 0.02], [0.08, 0.0, 0.02], [0.02, 0.07, 0.02]]))];
        for (name, q) in cases {
            let got = near_rule(&p, &q, k);
            let want = brute_force(&p, &q, k);
            let check = |a: C64, b: C64, what: &str, scale: f64| {
                let e = (a - b).norm() / scale;
                assert!(e < 2e-3, "{name} {what}: {a} vs {b}, error {e}");
            };
            let s = want.g.norm();
            let size = p.radius;
            check(got.g, want.g, "g", s);
            for m in 0..3 {
                check(got.gr[m], want.gr[m], "gr", s * size);
                check(got.gs[m], want.gs[m], "gs", s * size);
            }
            check(got.grs, want.grs, "grs", s * size * size);
        }
    }

    /// The same integrals with the outer triangle split into many pieces and the
    /// smooth part into several. The 1/R closed form is checked separately below.
    fn brute_force(p: &Tri, q: &Tri, k: f64) -> PairIntegrals {
        let n = 24;
        let sub_tris = |t: &Tri| -> Vec<Tri> {
            let mut out = Vec::new();
            let pt = |i: usize, j: usize| {
                let (a, b) = (i as f64 / n as f64, j as f64 / n as f64);
                t.at(&[1.0 - a - b, a, b])
            };
            for i in 0..n {
                for j in 0..n - i {
                    out.push(Tri::new([pt(i, j), pt(i + 1, j), pt(i, j + 1)]));
                    if i + j + 1 < n {
                        out.push(Tri::new([pt(i + 1, j), pt(i + 1, j + 1), pt(i, j + 1)]));
                    }
                }
            }
            out
        };
        let mut out = PairIntegrals::default();
        for tp in sub_tris(p) {
            for (wi, ai) in TRI16.iter() {
                let r = tp.at(wi);
                let rp = sub(r, p.c);
                let (i0, iv) = potentials(q, r);
                let mut inner = C64::new(i0, 0.0);
                let mut inner_v = [C64::new(iv[0], 0.0), C64::new(iv[1], 0.0), C64::new(iv[2], 0.0)];
                for tq in split_for_smooth_part(q) {
                    for (wj, aj) in TRI3.iter() {
                        let s = tq.at(wj);
                        let d = sub(r, s);
                        let g = smooth_kernel(k, dot(d, d).sqrt()) * (aj * tq.area);
                        inner += g;
                        let rq = sub(s, q.c);
                        for m in 0..3 {
                            inner_v[m] += g * rq[m];
                        }
                    }
                }
                let w = ai * tp.area;
                out.g += inner * w;
                for m in 0..3 {
                    out.gr[m] += inner * (w * rp[m]);
                    out.gs[m] += inner_v[m] * w;
                    out.grs += inner_v[m] * (w * rp[m]);
                }
            }
        }
        scale_integrals(out, 1.0 / (4.0 * PI))
    }

    fn split_for_smooth_part(q: &Tri) -> Vec<Tri> {
        let m = 4;
        let pt = |i: usize, j: usize| {
            let (a, b) = (i as f64 / m as f64, j as f64 / m as f64);
            q.at(&[1.0 - a - b, a, b])
        };
        let mut out = Vec::new();
        for i in 0..m {
            for j in 0..m - i {
                out.push(Tri::new([pt(i, j), pt(i + 1, j), pt(i, j + 1)]));
                if i + j + 1 < m {
                    out.push(Tri::new([pt(i + 1, j), pt(i + 1, j + 1), pt(i, j + 1)]));
                }
            }
        }
        out
    }

    #[test]
    fn potentials_match_a_refined_sum_off_the_triangle() {
        let q = Tri::new([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.2, 0.9, 0.0]]);
        for r in [[0.3, 0.3, 0.5], [2.0, 1.0, 0.3], [0.3, 0.3, 0.05], [1.5, 0.0, 0.0]] {
            let (i0, iv) = potentials(&q, r);
            let n = 300;
            let (mut a, mut b) = (0.0, [0.0; 3]);
            for i in 0..n {
                for j in 0..n - i {
                    let (u, v) = ((i as f64 + 1.0 / 3.0) / n as f64, (j as f64 + 1.0 / 3.0) / n as f64);
                    let s = q.at(&[1.0 - u - v, u, v]);
                    let da = q.area / (n * n) as f64;
                    let rr = norm(sub(r, s));
                    a += da / rr;
                    b = add(b, scale(sub(s, q.c), da / rr));
                    if i + j + 1 < n {
                        let (u, v) = ((i as f64 + 2.0 / 3.0) / n as f64, (j as f64 + 2.0 / 3.0) / n as f64);
                        let s = q.at(&[1.0 - u - v, u, v]);
                        let rr = norm(sub(r, s));
                        a += da / rr;
                        b = add(b, scale(sub(s, q.c), da / rr));
                    }
                }
            }
            assert!((i0 / a - 1.0).abs() < 1e-3, "{r:?}: {i0} vs {a}");
            assert!(norm(sub(iv, b)) < 2e-3 * norm(b).max(a * 0.1), "{r:?}: {iv:?} vs {b:?}");
        }
    }

    #[test]
    fn timing_fill_and_factor_near_3000_unknowns() {
        let side = 2.35;
        let g = Geometry { solids: vec![sheet("plate", [-side / 2.0, -side / 2.0, 0.0], [side / 2.0, side / 2.0, 0.0])], ..Default::default() };
        let t = std::time::Instant::now();
        let m = MomProblem::new(&g, F, opts(12.0)).unwrap();
        let mesh = t.elapsed().as_secs_f64();
        let (fill, factor) = super::super::timed_fill_and_factor(&m, F);
        println!("Timing: {} unknowns: mesh {mesh:.3} s, fill {fill:.3} s, factor {factor:.3} s", m.unknowns());
        // The times are printed, not asserted: on a shared machine they measure the load.
        assert!(m.unknowns() > 2500);
    }
}
