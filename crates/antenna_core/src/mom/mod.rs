//! Method-of-moments solvers: thin wires (triangle basis) and conducting
//! surfaces (RWG basis), both with the mixed-potential EFIE and Galerkin testing.
//!
//! A [`MomProblem`] discretises the geometry once for the highest frequency of a
//! sweep and then solves any frequency up to that one. The matrix equation is
//! `Z·x = v`, with `Z_mn = jωμ∬f_m·f_n G + (1/jωε)∬(∇·f_m)(∇'·f_n) G` plus the
//! surface-impedance term, and `G = exp(-jkR)/(4πR)`.

mod far;
mod fill;
mod layered;
pub mod mesher;
mod quad;
mod surface;
mod weld;
mod wire;

#[cfg(test)]
mod template_tests;

use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use rayon::prelude::*;

use crate::farfield::FarField;
use crate::geometry::{Geometry, PortDef, Shape, cross, norm, sub};
use crate::linalg::{Lu, invert};
use crate::{C0, C64, material};

pub use far::{Huygens, field_outside};
use far::{Moment, PlaneWave};

#[derive(Clone, Copy, Debug)]
pub struct MomOptions {
    /// Wire segments per wavelength.
    pub segments_per_lambda: f64,
    /// Surface mesh edge = λ / this.
    pub edges_per_lambda: f64,
    /// Far-field grid, degrees.
    pub theta_step: f64,
    pub phi_step: f64,
}

impl Default for MomOptions {
    fn default() -> Self {
        Self { segments_per_lambda: 20.0, edges_per_lambda: 12.0, theta_step: 3.0, phi_step: 3.0 }
    }
}

/// The most unknowns a model may come to. The dense matrix alone is 58 GB here and
/// one factorisation takes hours, so a bigger model is a typing slip rather than a
/// plan, and meshing it could keep the caller busy for many minutes.
pub const MAX_UNKNOWNS: usize = 60_000;

/// About how many unknowns `geom` comes to at `f_max`, from its size alone and with
/// nothing meshed, so that a hopeless model can be turned away at once.
pub fn estimate_unknowns(geom: &Geometry, f_max: f64, opts: &MomOptions) -> f64 {
    let lambda = C0 / f_max;
    let (h, delta) = (lambda / opts.edges_per_lambda, lambda / opts.segments_per_lambda);
    // A lattice triangle covers 0.42 h² and brings 1.5 edges; a strip narrower than
    // the mesh has only its outline, about one edge per 0.9 h of it.
    let sheet = |area: f64, outline: f64| 3.6 * area / (h * h) + 1.1 * outline / h;
    let solid = |shape: &Shape| match shape {
        Shape::Wire { pts, .. } => pts.windows(2).map(|w| norm(sub(w[1], w[0])) / delta).sum(),
        Shape::Box { min, max } => {
            let e = [0, 1, 2].map(|k| (max[k] - min[k]).abs());
            // One sheet stands in for a thin box, as in the mesher.
            let sides = if e.iter().any(|x| *x <= 0.05 * h) { 1.0 } else { 2.0 };
            sheet(sides * (e[0] * e[1] + e[1] * e[2] + e[2] * e[0]), sides * 2.0 * (e[0] + e[1] + e[2]))
        }
        Shape::Polygon { pts, .. } => {
            let (mut twice_area, mut outline) = (0.0, 0.0);
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                twice_area += a[0] * b[1] - b[0] * a[1];
                outline += (b[0] - a[0]).hypot(b[1] - a[1]);
            }
            sheet(0.5 * f64::abs(twice_area), outline)
        }
        Shape::Cylinder { a, b, r } => sheet(2.0 * PI * r * (norm(sub(*b, *a)) + r), 4.0 * PI * r),
        Shape::Sphere { r, .. } => sheet(4.0 * PI * r * r, 0.0),
        // Refined where too coarse, never coarsened.
        Shape::Tris { tris, .. } => {
            let area: f64 = tris.iter().map(|t| 0.5 * norm(cross(sub(t[1], t[0]), sub(t[2], t[0])))).sum();
            sheet(area, 0.0).max(1.5 * tris.len() as f64)
        }
    };
    geom.solids.iter().map(|s| solid(&s.shape)).sum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Formulation {
    Wire,
    Surface,
}

/// Discretised geometry for the 3D view.
#[derive(Clone, Debug, Default)]
pub struct MeshView {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub segments: Vec<[u32; 2]>,
}

/// Solution at one frequency.
#[derive(Clone, Debug)]
pub struct MomPoint {
    pub f: f64,
    /// Port impedance matrix (ohm), nports × nports: `Z = Y⁻¹`, `Y` from the delta-gap solutions.
    pub z: Vec<Vec<C64>>,
    /// With `want_far`, one per driven port: 1 W incident at that port from a
    /// source of the port's z0, the other ports terminated in theirs.
    pub far: Vec<FarField>,
    /// Current magnitude for display, port 0 driven as above, one value per view
    /// element (segment or triangle), A or A/m peak.
    pub current: Vec<f64>,
}

/// What the two solvers have in common.
trait Model: Send + Sync {
    fn unknowns(&self) -> usize;
    /// The column-major impedance matrix at `f`.
    fn matrix(&self, f: f64) -> Vec<C64>;
    /// The right-hand side for 1 V at a port, the other gaps shorted.
    fn port_rhs(&self, port: usize) -> Vec<C64>;
    /// Current through a port's gap in the direction a → b.
    fn port_current(&self, port: usize, x: &[C64]) -> C64;
    fn plane_wave_rhs(&self, wave: &PlaneWave) -> Vec<C64>;
    /// Current samples for the far field, ground images included.
    fn moments(&self, x: &[C64]) -> Vec<Moment>;
    /// One magnitude per view element.
    fn element_currents(&self, x: &[C64]) -> Vec<f64>;
    fn view(&self) -> MeshView;
    /// What the discretisation found marginal, with `lambda` the wavelength at the top of the sweep.
    fn warnings(&self, lambda: f64) -> Vec<String>;
}

/// Nothing in this model can lose power in the method of moments: every part is PEC, and no
/// layer around it has a loss tangent.
pub fn models_no_loss(geom: &Geometry) -> bool {
    let lossy_layer = geom.stack.as_ref().is_some_and(|s| s.layers().any(|l| material::by_name(&l.material).is_some_and(|m| matches!(m.kind, material::MatKind::Dielectric { tan_d, .. } if tan_d > 0.0))));
    !lossy_layer && geom.solids.iter().all(|s| material::by_name(&s.material).is_none_or(|m| matches!(m.kind, material::MatKind::Pec)))
}


/// Below these a dipole's resonance is more than 1 % high and a sphere's RCS more than 10 % low.
const FEW_SEGMENTS: f64 = 10.0;
const FEW_EDGES: f64 = 10.0;
/// Half of double precision's digits gone. Every template solves above 1e-5. An interior
/// resonance doesn't reach this: a sphere at ka = 2.74 only dips to 3e-4, and its RCS holds.
const POOR_RCOND: f64 = 1e-8;

/// A geometry discretised once, for one whole sweep.
pub struct MomProblem {
    model: Box<dyn Model>,
    formulation: Formulation,
    ports: Vec<PortDef>,
    opts: MomOptions,
    ground: bool,
    f_max: f64,
    /// Layers around the metal, if any.
    stack: Option<crate::stack::Stack>,
    /// The last factorisation, so `solve` then `rcs` at one frequency, or a
    /// monostatic sweep, factorise only once.
    last: Mutex<Option<(f64, Arc<Lu>)>>,
    /// The worst conditioned frequency solved so far: `(rcond, f)`.
    worst_rcond: Mutex<Option<(f64, f64)>>,
}

impl MomProblem {
    /// Discretises for the highest frequency of the sweep. Fails with a plain
    /// message if the geometry can't be handled.
    pub fn new(geom: &Geometry, f_max: f64, opts: MomOptions) -> Result<Self> {
        if !(f_max > 0.0) || !f_max.is_finite() {
            bail!("the highest frequency must be above zero (got {f_max} Hz)");
        }
        if !(opts.segments_per_lambda >= 1.0 && opts.edges_per_lambda >= 1.0) {
            bail!("the mesh needs at least one segment or edge per wavelength");
        }
        if !(opts.theta_step > 0.0 && opts.theta_step <= 90.0 && opts.phi_step > 0.0 && opts.phi_step <= 90.0) {
            bail!("the far-field steps must be between 0 and 90 degrees");
        }
        if geom.solids.is_empty() {
            bail!("the model has no parts to solve");
        }
        for s in &geom.solids {
            let Some(m) = material::by_name(&s.material) else {
                bail!("'{}' is made of '{}', which isn't in the material library", s.name, s.material);
            };
            if !m.is_conductor() {
                bail!("'{}' is made of {}, a dielectric: the method of moments here only handles conductors in free space, so use FDTD for this model", s.name, m.name);
            }
        }
        for p in &geom.ports {
            if !(p.z0 > 0.0) {
                bail!("port '{}' needs a reference impedance above zero (it has {} Ω)", p.name, p.z0);
            }
        }
        let stack = geom.stack.clone().filter(|s| !s.is_empty());
        if let Some(s) = &stack {
            s.check()?;
            if geom.flat_height().is_none() {
                bail!("Layers around a model need all its metal in one flat plane of constant z here. Use FDTD for metal on several layers, or anything that isn't flat.");
            }
            if geom.ground {
                bail!("A model with layers can't also have the infinite ground plane: use metal_behind in its layers.");
            }
        }
        let wires = geom.solids.iter().filter(|s| s.shape.is_wire()).count();
        if wires > 0 && wires < geom.solids.len() {
            bail!("the model mixes wires and surfaces, which the method of moments here can't join: draw the wires as thin strips, or use FDTD");
        }
        let estimate = estimate_unknowns(geom, f_max, &opts);
        if estimate > MAX_UNKNOWNS as f64 {
            let count = if estimate < 1e6 { format!("{:.0} thousand", estimate / 1e3) } else { format!("{:.0} million", estimate / 1e6) };
            bail!("At {:.3} GHz this model comes to about {count} unknowns, and the solver's matrix alone would need {:.1} TB. Check the frequency and the model's size, or use FDTD.", f_max / 1e9, 16.0 * estimate * estimate / 1e12);
        }
        let lambda = C0 / f_max;
        let (model, formulation): (Box<dyn Model>, _) = if wires > 0 {
            (Box::new(wire::WireModel::build(geom, lambda / opts.segments_per_lambda)?), Formulation::Wire)
        } else {
            (Box::new(surface::SurfaceModel::build(geom, lambda / opts.edges_per_lambda, lambda, stack.clone())?), Formulation::Surface)
        };
        if model.unknowns() == 0 {
            bail!("the model has no paths for current: a wire or sheet needs to carry current across at least one edge");
        }
        Ok(Self { model, formulation, ports: geom.ports.clone(), opts, ground: geom.ground, f_max, stack, last: Mutex::new(None), worst_rcond: Mutex::new(None) })
    }

    pub fn formulation(&self) -> Formulation {
        self.formulation
    }

    /// What a user should know before believing this model's numbers: an under-resolved
    /// mesh, a wire past its limits, a poor surface mesh. After a solve, also the worst
    /// conditioning seen so far.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = self.model.warnings(C0 / self.f_max);
        match self.formulation {
            Formulation::Wire if self.opts.segments_per_lambda < FEW_SEGMENTS => out.push(format!("{:.0} segments per wavelength is too coarse for the resonance: below {FEW_SEGMENTS:.0} it moves by more than about 1 %.", self.opts.segments_per_lambda)),
            Formulation::Surface if self.opts.edges_per_lambda < FEW_EDGES => out.push(format!("{:.0} edges per wavelength is too coarse: below {FEW_EDGES:.0} scattering is off by more than about 10 % and resonances move too.", self.opts.edges_per_lambda)),
            _ => {}
        }
        if let Some((rcond, f)) = *self.worst_rcond.lock().unwrap()
            && rcond < POOR_RCOND
        {
            out.push(format!("The moment matrix is near singular at {:.4} GHz (reciprocal condition {rcond:.0e}), so results around there are unreliable. A model far smaller than a wavelength does this, and so do parts lying on top of each other.", f / 1e9));
        }
        out
    }

    /// Reciprocal condition of the moment matrix, the worst of what has been solved.
    pub fn worst_rcond(&self) -> Option<(f64, f64)> {
        *self.worst_rcond.lock().unwrap()
    }

    pub fn unknowns(&self) -> usize {
        self.model.unknowns()
    }

    /// Discretised geometry for the 3D view.
    pub fn view(&self) -> MeshView {
        self.model.view()
    }

    fn check_frequency(&self, f: f64) -> Result<()> {
        if !(f > 0.0) || !f.is_finite() {
            bail!("the frequency must be above zero (got {f} Hz)");
        }
        if f > self.f_max * 1.001 {
            bail!("{f} Hz is above the {} Hz this model was meshed for", self.f_max);
        }
        Ok(())
    }

    fn factored(&self, f: f64) -> Result<Arc<Lu>> {
        self.check_frequency(f)?;
        if let Some((cached, lu)) = self.last.lock().unwrap().as_ref()
            && *cached == f
        {
            return Ok(lu.clone());
        }
        let lu = Arc::new(self.factor_at(f)?);
        *self.last.lock().unwrap() = Some((f, lu.clone()));
        Ok(lu)
    }

    fn factor_at(&self, f: f64) -> Result<Lu> {
        let n = self.model.unknowns();
        let lu = Lu::factor(self.model.matrix(f), n).map_err(|e| anyhow::anyhow!("the moment matrix can't be solved at {f} Hz ({e}): the mesh probably has duplicate or disconnected parts"))?;
        let rcond = lu.rcond();
        let mut worst = self.worst_rcond.lock().unwrap();
        if worst.is_none_or(|w| rcond < w.0) {
            *worst = Some((rcond, f));
        }
        Ok(lu)
    }

    pub fn solve(&self, f: f64, want_far: bool) -> Result<MomPoint> {
        let lu = self.factored(f)?;
        let np = self.ports.len();
        let mut sol: Vec<Vec<C64>> = (0..np).map(|p| self.model.port_rhs(p)).collect();
        lu.solve_many(&mut sol);

        let y: Vec<Vec<C64>> = (0..np).map(|q| (0..np).map(|p| self.model.port_current(q, &sol[p])).collect()).collect();
        let z = if np > 0 { invert(&y).map_err(|e| anyhow::anyhow!("the port admittance matrix can't be inverted at {f} Hz ({e}): two ports may be shorted together"))? } else { Vec::new() };

        // Port k driven with 1 W incident: (Z + Z0)·I = Vs·e_k, then x = Σ V_p·x_p.
        let mut loaded = z.clone();
        for (p, row) in loaded.iter_mut().enumerate() {
            row[p] += self.ports[p].z0;
        }
        let loaded_inv = if np > 0 { invert(&loaded)? } else { Vec::new() };
        let excitation = |k: usize| -> (Vec<C64>, f64) {
            let z0 = self.ports[k].z0;
            let vs = 2.0 * (2.0 * z0).sqrt();
            let currents: Vec<C64> = (0..np).map(|i| loaded_inv[i][k] * vs).collect();
            let volts: Vec<C64> = (0..np).map(|i| (0..np).map(|j| z[i][j] * currents[j]).sum()).collect();
            let s = (volts[k] - currents[k] * z0) / (volts[k] + currents[k] * z0);
            let mut x = vec![C64::new(0.0, 0.0); self.model.unknowns()];
            for (p, v) in volts.iter().enumerate() {
                for (xi, si) in x.iter_mut().zip(&sol[p]) {
                    *xi += *si * *v;
                }
            }
            // A negative feed resistance stays negative and reads as NaN efficiency;
            // only a rounding error around zero is tidied away.
            let accepted = 1.0 - s.norm_sqr();
            (x, if accepted < -1e-9 { accepted } else { accepted.max(0.0) })
        };

        let mut far = Vec::new();
        if want_far {
            for k in 0..np {
                let (x, accepted) = excitation(k);
                let moments = self.model.moments(&x);
                let mut ff = match &self.stack {
                    Some(stack) => layered::far_field(&moments, f, self.opts.theta_step, self.opts.phi_step, stack),
                    None => far::far_field(&moments, f, self.opts.theta_step, self.opts.phi_step, self.ground),
                };
                ff.p_accepted = accepted;
                ff.p_incident = 1.0;
                far.push(ff);
            }
        }
        let current = if np > 0 { self.model.element_currents(&excitation(0).0) } else { self.model.element_currents(&vec![C64::new(0.0, 0.0); self.model.unknowns()]) };
        Ok(MomPoint { f, z, far, current })
    }

    /// Current magnitudes for display with these incident waves on the ports, each
    /// `1` meaning 1 W incident. One value per view element, as in [`MomPoint::current`].
    /// A port whose wave is sent back by a part is how a reconfigurable state is drawn.
    pub fn currents_for_waves(&self, f: f64, waves: &[C64]) -> Result<Vec<f64>> {
        let np = self.ports.len();
        if waves.len() != np {
            bail!("{} waves for {np} ports", waves.len());
        }
        let lu = self.factored(f)?;
        let mut sol: Vec<Vec<C64>> = (0..np).map(|p| self.model.port_rhs(p)).collect();
        lu.solve_many(&mut sol);
        let y: Vec<Vec<C64>> = (0..np).map(|q| (0..np).map(|p| self.model.port_current(q, &sol[p])).collect()).collect();
        let z = invert(&y).map_err(|e| anyhow::anyhow!("the port admittance matrix can't be inverted at {f} Hz ({e})"))?;
        let mut loaded = z.clone();
        for (p, row) in loaded.iter_mut().enumerate() {
            row[p] += self.ports[p].z0;
        }
        let loaded_inv = invert(&loaded)?;
        // V + Z0·I = 2√Z0·a, with a = √2·w for peak amplitudes.
        let vs: Vec<C64> = (0..np).map(|p| waves[p] * (2.0 * (2.0 * self.ports[p].z0).sqrt())).collect();
        let currents: Vec<C64> = (0..np).map(|i| (0..np).map(|j| loaded_inv[i][j] * vs[j]).sum()).collect();
        let volts: Vec<C64> = (0..np).map(|i| (0..np).map(|j| z[i][j] * currents[j]).sum()).collect();
        let mut x = vec![C64::new(0.0, 0.0); self.model.unknowns()];
        for (p, v) in volts.iter().enumerate() {
            for (xi, si) in x.iter_mut().zip(&sol[p]) {
                *xi += *si * *v;
            }
        }
        Ok(self.model.element_currents(&x))
    }

    /// The electric field (V/m, peak) at `points` with 1 W incident on `port` and
    /// the other ports terminated, mismatch included. Scale amplitudes with the
    /// square root of the real incident power. Points closer to the metal than
    /// about one mesh edge are unreliable.
    pub fn near_field(&self, f: f64, port: usize, points: &[crate::geometry::P3]) -> Result<Vec<[C64; 3]>> {
        let np = self.ports.len();
        if port >= np {
            bail!("port {} doesn't exist (the model has {np})", port + 1);
        }
        Ok(self.fields_of(f, &[port], points)?.swap_remove(0))
    }

    /// [`Self::near_field`] for every port in turn: `out[port][point]`. Fields add with
    /// a state's weights, since each term already holds what the other ports couple.
    pub fn near_fields(&self, f: f64, points: &[crate::geometry::P3]) -> Result<Vec<Vec<[C64; 3]>>> {
        let all: Vec<usize> = (0..self.ports.len()).collect();
        self.fields_of(f, &all, points)
    }

    fn fields_of(&self, f: f64, driven: &[usize], points: &[crate::geometry::P3]) -> Result<Vec<Vec<[C64; 3]>>> {
        if self.stack.is_some() {
            bail!("Near fields aren't worked out for a model in layers: use FDTD for those.");
        }
        let lu = self.factored(f)?;
        let np = self.ports.len();
        let mut sol: Vec<Vec<C64>> = (0..np).map(|p| self.model.port_rhs(p)).collect();
        lu.solve_many(&mut sol);
        let y: Vec<Vec<C64>> = (0..np).map(|q| (0..np).map(|p| self.model.port_current(q, &sol[p])).collect()).collect();
        let z = invert(&y).map_err(|e| anyhow::anyhow!("the port admittance matrix can't be inverted at {f} Hz ({e})"))?;
        let mut loaded = z.clone();
        for (p, row) in loaded.iter_mut().enumerate() {
            row[p] += self.ports[p].z0;
        }
        let loaded_inv = invert(&loaded)?;
        let k = far::wavenumber(f);
        let mut out = Vec::with_capacity(driven.len());
        for &port in driven {
            let vs = 2.0 * (2.0 * self.ports[port].z0).sqrt();
            let currents: Vec<C64> = (0..np).map(|i| loaded_inv[i][port] * vs).collect();
            let volts: Vec<C64> = (0..np).map(|i| (0..np).map(|j| z[i][j] * currents[j]).sum()).collect();
            let mut x = vec![C64::new(0.0, 0.0); self.model.unknowns()];
            for (p, v) in volts.iter().enumerate() {
                for (xi, si) in x.iter_mut().zip(&sol[p]) {
                    *xi += *si * *v;
                }
            }
            let moments = self.model.moments(&x);
            out.push(points.par_iter().map(|p| far::field_near(&moments, k, *p)).collect());
        }
        Ok(out)
    }

    /// Monostatic or bistatic RCS (m²) for a plane wave arriving from
    /// `(theta_inc, phi_inc)` degrees with unit E along `pol_theta·θ̂ + pol_phi·φ̂`,
    /// observed in the given `(theta, phi)` directions. Port gaps count as shorted.
    /// The specular return of an infinite ground plane itself is not included.
    pub fn rcs(&self, f: f64, theta_inc: f64, phi_inc: f64, pol_theta: C64, pol_phi: C64, observe: &[(f64, f64)]) -> Result<Vec<f64>> {
        if self.stack.is_some() {
            bail!("Radar cross section isn't worked out for a model in layers: use FDTD for that.");
        }
        let lu = self.factored(f)?;
        let power = pol_theta.norm_sqr() + pol_phi.norm_sqr();
        if !(power > 0.0) {
            bail!("the incident polarisation has zero amplitude");
        }
        if self.ground && theta_inc > 90.0 {
            return Ok(vec![0.0; observe.len()]);
        }
        let wave = PlaneWave::new(f, theta_inc, phi_inc, pol_theta, pol_phi, self.ground);
        let mut x = self.model.plane_wave_rhs(&wave);
        lu.solve(&mut x);
        let moments = self.model.moments(&x);
        let k = far::wavenumber(f);
        Ok(observe
            .par_iter()
            .map(|&(t, p)| {
                if self.ground && t > 90.0 {
                    return 0.0;
                }
                let (et, ep) = far::field_at(&moments, k, t, p);
                4.0 * std::f64::consts::PI * (et.norm_sqr() + ep.norm_sqr()) / power
            })
            .collect())
    }
}

#[cfg(test)]
#[test]
fn a_problem_can_be_shared_between_threads() {
    fn check<T: Send + Sync>() {}
    check::<MomProblem>();
}

/// Time of the fill and of the factorisation at `f`, in seconds, for reporting.
#[cfg(test)]
pub(crate) fn timed_fill_and_factor(p: &MomProblem, f: f64) -> (f64, f64) {
    let t = std::time::Instant::now();
    let z = p.model.matrix(f);
    let fill = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let lu = Lu::factor(z, p.model.unknowns()).unwrap();
    let factor = t.elapsed().as_secs_f64();
    assert_eq!(lu.size(), p.model.unknowns());
    (fill, factor)
}
