//! A design project: template, variable table, user parts, solver setup, and
//! what a simulation returns. Everything here serialises, so a project with its
//! results is one JSON file.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::expr::{self, Vars};
use crate::farfield::{FarField, Metrics};
use crate::geometry::{Geometry, MeshHint, P3, PortDef, Shape, Solid, SolverFit};
use crate::sparam::Network;
use crate::{C0, C64, material, templates};

/// One row of the variable table; `expr` may refer to the rows and to `er`,
/// `tand`, and may carry units (`12mm`, `2.45GHz`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub expr: String,
    pub note: String,
    /// Optimiser and sweep range; `None`: ±30 % around the current value.
    #[serde(default)]
    pub range: Option<(f64, f64)>,
    /// Helper row (derived value): collapsed in the table, not offered for optimisation.
    #[serde(default)]
    pub derived: bool,
}

impl Param {
    pub fn new(name: &str, expr: &str, note: &str) -> Self {
        Self { name: name.into(), expr: expr.into(), note: note.into(), range: None, derived: false }
    }
    pub fn derived(name: &str, expr: &str, note: &str) -> Self {
        Self { derived: true, ..Self::new(name, expr, note) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SolverKind {
    /// MoM when the geometry allows it, FDTD otherwise.
    #[default]
    Auto,
    /// openEMS time domain (dielectrics, anything).
    Fdtd,
    /// Method of moments on wires.
    MomWire,
    /// Method of moments on conducting surfaces.
    MomSurface,
}

impl SolverKind {
    pub fn label(self) -> &'static str {
        match self {
            SolverKind::Auto => "Automatic",
            SolverKind::Fdtd => "FDTD time domain (openEMS)",
            SolverKind::MomWire => "MoM, wires",
            SolverKind::MomSurface => "MoM, surfaces (RWG)",
        }
    }

    /// What `Auto` means for a geometry.
    pub fn resolve(self, fit: SolverFit) -> SolverKind {
        match self {
            SolverKind::Auto if fit.wire_mom => SolverKind::MomWire,
            SolverKind::Auto if fit.surface_mom => SolverKind::MomSurface,
            SolverKind::Auto => SolverKind::Fdtd,
            other => other,
        }
    }
}

/// Above this many surface unknowns, `Auto` prefers FDTD: a dense MoM matrix then
/// costs gigabytes and tens of seconds per frequency.
pub const AUTO_MOM_LIMIT: usize = 6000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    pub f_lo: f64,
    pub f_hi: f64,
    /// Output points of the frequency sweep.
    pub points: usize,
    pub solver: SolverKind,
    /// Frequencies with a far field.
    pub far_freqs: Vec<f64>,
    /// Far-field grid step, degrees.
    pub far_step: f64,
    /// FDTD cells per wavelength at `f_hi`.
    pub cells_per_lambda: f64,
    /// FDTD: stop at this energy decay (dB, positive number).
    pub end_db: f64,
    /// FDTD: safety limit on time steps.
    pub max_steps: u64,
    /// MoM: solved frequencies (interpolated to `points`).
    pub mom_samples: usize,
    pub segments_per_lambda: f64,
    pub edges_per_lambda: f64,
    /// Threads for one openEMS run (0: chosen for the machine).
    pub threads: usize,
    /// Extra field planes at the design frequency (FDTD).
    pub field_planes: bool,
}

impl Setup {
    pub fn for_band(f_lo: f64, f_hi: f64, f0: f64) -> Self {
        Self {
            f_lo,
            f_hi,
            points: 401,
            solver: SolverKind::Auto,
            far_freqs: vec![f0],
            far_step: 3.0,
            cells_per_lambda: 20.0,
            // Leaves 0.3 % of the amplitude, so S11 near −20 dB is good to about 0.3 dB.
            end_db: 50.0,
            max_steps: 1_000_000,
            mom_samples: 41,
            segments_per_lambda: 24.0,
            edges_per_lambda: 12.0,
            threads: 0,
            field_planes: true,
        }
    }

    /// Fails with a plain message if the sweep doesn't run from low to high.
    pub fn check_band(&self) -> Result<()> {
        if !(self.f_lo > 0.0 && self.f_hi > self.f_lo) {
            bail!("The sweep has to run upward, but it goes from {:.3} GHz to {:.3} GHz.", self.f_lo / 1e9, self.f_hi / 1e9);
        }
        Ok(())
    }
}

/// A name an expression may use, with what's needed to offer it.
#[derive(Clone, Debug, PartialEq)]
pub struct VarHint {
    pub name: String,
    /// `None` while the variable table doesn't evaluate.
    pub value: Option<f64>,
    pub note: String,
}

/// A solid the user added on top of the template. Coordinates are expressions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraSolid {
    pub name: String,
    pub material: String,
    pub prim: ExtraPrim,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtraPrim {
    Box { min: [String; 3], max: [String; 3] },
    Cylinder { a: [String; 3], b: [String; 3], r: String },
    Sphere { c: [String; 3], r: String },
    Wire { pts: Vec<[String; 3]>, r: String },
    Polygon { axis: usize, at: String, pts: Vec<[String; 2]>, thick: String },
    /// A triangle model from a file, as read at import time. `scale` converts file units to metres.
    Imported { file: String, tris: Vec<[P3; 3]>, solid: bool },
    /// A flat outline from a drawing (see [`crate::cad`]). Its points stay numbers; only
    /// its height is an expression, so a stack follows the substrate thickness. `cell`
    /// is the FDTD cell its finest detail asks for, 0 for none.
    Outline { file: String, axis: usize, at: String, pts: Vec<[f64; 2]>, thick: String, cell: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraPort {
    pub name: String,
    pub a: [String; 3],
    pub b: [String; 3],
    pub width: String,
    pub z0: String,
}

/// How the ports are driven when results are viewed (amplitude, phase in degrees).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drive {
    pub amp: f64,
    pub phase: f64,
}

/// One way of driving all the ports: the weights a phase network or a switch sets
/// at one moment. A reader that cycles through several has several of these.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriveState {
    pub name: String,
    pub drive: Vec<Drive>,
}

impl DriveState {
    /// The weights, scaled so the state puts 1 W into the feed in total. `None` if
    /// every amplitude is zero. Missing ports get nothing, unlike [`Project::weights`]:
    /// a state says what is driven.
    pub fn weights(&self, ports: usize) -> Option<Vec<C64>> {
        let raw: Vec<C64> = (0..ports).map(|k| self.drive.get(k).map_or(C64::new(0.0, 0.0), |d| C64::from_polar(d.amp, d.phase.to_radians()))).collect();
        let power: f64 = raw.iter().map(C64::norm_sqr).sum();
        (power > 0.0).then(|| raw.iter().map(|w| *w / power.sqrt()).collect())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub template: String,
    pub params: Vec<Param>,
    pub metal: String,
    pub substrate: String,
    #[serde(default)]
    pub extra: Vec<ExtraSolid>,
    #[serde(default)]
    pub extra_ports: Vec<ExtraPort>,
    /// Template parts the user switched off (by name).
    #[serde(default)]
    pub hidden: Vec<String>,
    pub setup: Setup,
    #[serde(default)]
    pub drive: Vec<Drive>,
    /// Excitation states for a test that cycles through them (see [`crate::recipes::gate`]).
    #[serde(default)]
    pub states: Vec<DriveState>,
    /// Switches and varactors across gaps, and their states (see [`crate::reconfig`]).
    #[serde(default)]
    pub reconfig: crate::reconfig::Reconfig,
}

impl Project {
    /// A new project from a template at its default design frequency.
    pub fn from_template(id: &str, f0_expr: Option<&str>) -> Result<Self> {
        let t = templates::by_id(id).with_context(|| format!("no template '{id}'"))?;
        let mut params = (t.params)();
        if let Some(f) = f0_expr
            && let Some(p) = params.iter_mut().find(|p| p.name == "f0")
        {
            p.expr = f.to_string();
        }
        let mut pr = Self {
            name: t.title.to_string(),
            template: id.to_string(),
            params,
            metal: "PEC".into(),
            substrate: t.substrate.to_string(),
            extra: Vec::new(),
            extra_ports: Vec::new(),
            hidden: Vec::new(),
            setup: Setup::for_band(1.0, 2.0, 1.5),
            drive: Vec::new(),
            states: Vec::new(),
            reconfig: Default::default(),
        };
        pr.reset_band()?;
        if let Some(rc) = crate::reconfig::for_template(id, pr.f0()) {
            pr.reconfig = rc;
        }
        // Small tabs and narrow gaps are what its surface mesh has to resolve.
        if id == "reconfig_pol_patch" {
            pr.setup.edges_per_lambda = 20.0;
            pr.setup.mom_samples = 13;
            pr.setup.far_freqs = vec![pr.f0()];
        }
        // A narrow slot is what the surface mesh has to resolve, not the wavelength.
        if id == "slot" {
            pr.setup.edges_per_lambda = 30.0;
            pr.setup.mom_samples = 21;
        }
        // A panel this size is thousands of unknowns, so each sweep point is real work.
        if id == "rfid_gate" {
            pr.setup.edges_per_lambda = 10.0;
            pr.setup.mom_samples = 7;
            pr.setup.far_step = 5.0;
            pr.states = crate::recipes::gate::default_states(5);
        }
        Ok(pr)
    }

    /// Sets the sweep band to the template's, around the current f0.
    pub fn reset_band(&mut self) -> Result<()> {
        let t = templates::by_id(&self.template).with_context(|| format!("no template '{}'", self.template))?;
        let vars = self.vars()?;
        let f0 = vars.get("f0").copied().unwrap_or(1e9);
        self.setup.f_lo = f0 * t.band.0;
        self.setup.f_hi = f0 * t.band.1;
        self.setup.far_freqs = vec![f0];
        Ok(())
    }

    /// Sweeps exactly this band and sizes the antenna for its centre: `f0` drives the
    /// template's dimensions, and the far field follows it.
    pub fn fit_band(&mut self, f_lo: f64, f_hi: f64) {
        let f0 = (f_lo + f_hi) / 2.0;
        if let Some(p) = self.params.iter_mut().find(|p| p.name == "f0") {
            p.expr = format!("{}MHz", (f0 / 1e3).round() / 1e3);
        }
        self.setup.f_lo = f_lo;
        self.setup.f_hi = f_hi;
        self.setup.far_freqs = vec![f0];
    }

    /// The material's constants as expression variables.
    fn base_vars(&self) -> Vars {
        let mut base = Vars::new();
        let m = material::by_name(&self.substrate).unwrap_or_else(|| material::Material::dielectric("Air", 1.0, 0.0));
        base.insert("er".into(), m.eps_r());
        base.insert("tand".into(), if let material::MatKind::Dielectric { tan_d, .. } = m.kind { tan_d } else { 0.0 });
        base
    }

    pub fn vars(&self) -> Result<Vars> {
        let pairs: Vec<(String, String)> = self.params.iter().map(|p| (p.name.clone(), p.expr.clone())).collect();
        expr::resolve(&pairs, &self.base_vars())
    }

    /// Every name an expression may use, table rows first. Their values go when the
    /// table doesn't evaluate; the names stay, for it can be mended by using them.
    pub fn var_hints(&self) -> Vec<VarHint> {
        let vars = self.vars().ok();
        let mut out: Vec<VarHint> = self.params.iter().map(|p| VarHint { name: p.name.clone(), value: vars.as_ref().and_then(|v| v.get(&p.name).copied()), note: p.note.clone() }).collect();
        for (name, value) in self.base_vars() {
            if !out.iter().any(|h| h.name == name) {
                let note = match name.as_str() {
                    "er" => "Substrate permittivity",
                    "tand" => "Substrate loss tangent",
                    _ => "",
                };
                out.push(VarHint { name, value: Some(value), note: note.to_string() });
            }
        }
        out
    }

    /// A copy with these values as literal expressions, for a sweep or an optimiser step.
    pub fn with_values(&self, values: &[(String, f64)]) -> Self {
        let mut p = self.clone();
        for (name, v) in values {
            if let Some(row) = p.params.iter_mut().find(|r| &r.name == name) {
                row.expr = format!("{v:e}");
            }
        }
        p
    }

    pub fn f0(&self) -> f64 {
        self.vars().ok().and_then(|v| v.get("f0").copied()).unwrap_or((self.setup.f_lo + self.setup.f_hi) / 2.0)
    }

    /// Evaluates everything into the geometry the solvers take; it must have a feed.
    pub fn geometry(&self) -> Result<Geometry> {
        let g = self.scatterer()?;
        if g.ports.is_empty() {
            bail!("The model has no port. Add a feed under Parts and ports.");
        }
        Ok(g)
    }

    /// The same model when nothing has to feed it: a part list being built, or a
    /// target for a radar cross section.
    pub fn scatterer(&self) -> Result<Geometry> {
        // `base_vars` would quietly read an unknown substrate as air.
        if material::by_name(&self.substrate).is_none() {
            bail!("The substrate '{}' isn't in the material library.", self.substrate);
        }
        let vars = self.vars()?;
        let t = templates::by_id(&self.template).with_context(|| format!("no template '{}'", self.template))?;
        let ctx = templates::Ctx { v: &vars, metal: &self.metal, substrate: &self.substrate };
        let mut g = (t.build)(&ctx)?;
        g.solids.retain(|s| !self.hidden.contains(&s.name));
        // An air substrate is no substrate at all (and keeps MoM available).
        g.solids.retain(|s| s.material != "Air");
        let finest = C0 / self.setup.f_hi.max(1.0) / 300.0;
        for e in &self.extra {
            let solid = extra_solid(e, &vars)?;
            // A drawing has no template to say where its fine detail is.
            if let ExtraPrim::Outline { cell, .. } = &e.prim
                && *cell > 0.0
            {
                let (min, max) = solid.shape.bounds();
                g.hints.push(MeshHint { min, max, cell: cell.max(finest) });
            }
            g.solids.push(solid);
        }
        for p in &self.extra_ports {
            let e3 = |a: &[String; 3]| -> Result<P3> { Ok([expr::eval(&a[0], &vars)?, expr::eval(&a[1], &vars)?, expr::eval(&a[2], &vars)?]) };
            g.ports.push(PortDef { name: p.name.clone(), a: e3(&p.a)?, b: e3(&p.b)?, width: expr::eval(&p.width, &vars)?, z0: expr::eval(&p.z0, &vars)? });
        }
        check(&g)?;
        Ok(g)
    }

    /// The same model with every part and port its own to edit: the template's formulas
    /// are left behind, its variables stay for whatever else uses them.
    pub fn editable_copy(&self) -> Result<Self> {
        let g = self.geometry().or_else(|_| self.scatterer())?;
        let mut p = self.clone();
        p.template = "custom".into();
        p.hidden.clear();
        p.extra.clear();
        p.extra_ports.clear();
        let num = |v: f64| format!("{v:e}");
        let n3 = |q: P3| [num(q[0]), num(q[1]), num(q[2])];
        for s in &g.solids {
            let prim = match &s.shape {
                // A flat box is a sheet, and sheets are what the drawing tools edit.
                Shape::Box { min, max } => match (0..3).find(|k| max[*k] - min[*k] <= 0.0) {
                    Some(axis) => {
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        let pts = vec![[min[u], min[v]], [max[u], min[v]], [max[u], max[v]], [min[u], max[v]]];
                        let cell = crate::cad::narrowest_of(std::slice::from_ref(&pts));
                        ExtraPrim::Outline { file: "drawn".into(), axis, at: num(min[axis]), pts, thick: "0".into(), cell: if cell.is_finite() { cell / 3.0 } else { 0.0 } }
                    }
                    None => ExtraPrim::Box { min: n3(*min), max: n3(*max) },
                },
                Shape::Cylinder { a, b, r } => ExtraPrim::Cylinder { a: n3(*a), b: n3(*b), r: num(*r) },
                Shape::Sphere { c, r } => ExtraPrim::Sphere { c: n3(*c), r: num(*r) },
                Shape::Wire { pts, r } => ExtraPrim::Wire { pts: pts.iter().map(|q| n3(*q)).collect(), r: num(*r) },
                Shape::Polygon { axis, at, pts, thick } => {
                    let cell = crate::cad::narrowest_of(std::slice::from_ref(pts));
                    ExtraPrim::Outline { file: "drawn".into(), axis: *axis, at: num(*at), pts: pts.clone(), thick: num(*thick), cell: if cell.is_finite() { cell / 3.0 } else { 0.0 } }
                }
                Shape::Tris { tris, solid, .. } => ExtraPrim::Imported { file: "template".into(), tris: tris.clone(), solid: *solid },
            };
            p.extra.push(ExtraSolid { name: s.name.clone(), material: s.material.clone(), prim });
        }
        for port in &g.ports {
            p.extra_ports.push(ExtraPort { name: port.name.clone(), a: n3(port.a), b: n3(port.b), width: num(port.width), z0: num(port.z0) });
        }
        Ok(p)
    }

    /// The solver a run of this geometry will really use.
    pub fn solver_for(&self, geom: &Geometry) -> SolverKind {
        let chosen = self.setup.solver.resolve(geom.solver_fit());
        if self.setup.solver == SolverKind::Auto && chosen == SolverKind::MomSurface {
            let opts = crate::mom::MomOptions { edges_per_lambda: self.setup.edges_per_lambda, segments_per_lambda: self.setup.segments_per_lambda, ..Default::default() };
            // The UI asks this whenever the project changes, so far past the limit it
            // has to answer without meshing.
            if crate::mom::estimate_unknowns(geom, self.setup.f_hi, &opts) > 4.0 * AUTO_MOM_LIMIT as f64 {
                return SolverKind::Fdtd;
            }
            if crate::mom::MomProblem::new(geom, self.setup.f_hi, opts).is_ok_and(|p| p.unknowns() > AUTO_MOM_LIMIT) {
                return SolverKind::Fdtd;
            }
        }
        chosen
    }

    /// The drive weights, one per port (missing ports get 1∠0°).
    pub fn weights(&self, ports: usize) -> Vec<C64> {
        (0..ports)
            .map(|k| match self.drive.get(k) {
                Some(d) => C64::from_polar(d.amp, d.phase.to_radians()),
                None => C64::new(1.0, 0.0),
            })
            .collect()
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
    pub fn from_json(s: &str) -> Result<Self> {
        let p: Self = serde_json::from_str(s).context("this is not a Sense Sim antenna project")?;
        Ok(p)
    }
}

fn extra_solid(e: &ExtraSolid, v: &Vars) -> Result<Solid> {
    let ev = |s: &String| expr::eval(s, v).with_context(|| format!("part '{}'", e.name));
    let e3 = |a: &[String; 3]| -> Result<P3> { Ok([ev(&a[0])?, ev(&a[1])?, ev(&a[2])?]) };
    let shape = match &e.prim {
        ExtraPrim::Box { min, max } => Shape::Box { min: e3(min)?, max: e3(max)? },
        ExtraPrim::Cylinder { a, b, r } => Shape::Cylinder { a: e3(a)?, b: e3(b)?, r: ev(r)? },
        ExtraPrim::Sphere { c, r } => Shape::Sphere { c: e3(c)?, r: ev(r)? },
        ExtraPrim::Wire { pts, r } => Shape::Wire { pts: pts.iter().map(e3).collect::<Result<_>>()?, r: ev(r)? },
        ExtraPrim::Polygon { axis, at, pts, thick } => Shape::Polygon {
            axis: *axis,
            at: ev(at)?,
            pts: pts.iter().map(|p| Ok::<_, anyhow::Error>([ev(&p[0])?, ev(&p[1])?])).collect::<Result<_>>()?,
            thick: ev(thick)?,
        },
        ExtraPrim::Imported { tris, solid, .. } => Shape::Tris { tris: tris.clone(), solid: *solid, sheet: 0.0 },
        ExtraPrim::Outline { axis, at, pts, thick, .. } => Shape::Polygon { axis: *axis, at: ev(at)?, pts: pts.clone(), thick: ev(thick)? },
    };
    Ok(Solid { name: e.name.clone(), material: e.material.clone(), shape })
}

/// Rejects what no solver could run, in terms a user can act on.
fn check(g: &Geometry) -> Result<()> {
    if g.solids.is_empty() {
        bail!("The model has no parts.");
    }
    for s in &g.solids {
        let (lo, hi) = s.shape.bounds();
        if (0..3).any(|k| !lo[k].is_finite() || !hi[k].is_finite()) {
            bail!("Part '{}' has an invalid size.", s.name);
        }
        if material::by_name(&s.material).is_none() {
            bail!("Part '{}' is made of '{}', which isn't in the material library.", s.name, s.material);
        }
        if let Shape::Wire { pts, r } = &s.shape
            && (pts.len() < 2 || *r <= 0.0)
        {
            bail!("Wire '{}' needs at least two points and a positive radius.", s.name);
        }
    }
    for p in &g.ports {
        let d = crate::geometry::sub(p.b, p.a);
        let off_axis = (0..3).filter(|k| d[*k].abs() > 1e-12).count();
        if off_axis != 1 {
            bail!("Port '{}' must lie along x, y or z (its two ends differ along {off_axis} axes).", p.name);
        }
        if p.z0 <= 0.0 {
            bail!("Port '{}' needs a positive impedance.", p.name);
        }
    }
    // A wire's centre line can sit on the plane even though its radius dips below.
    let below = |s: &Solid| match &s.shape {
        Shape::Wire { pts, .. } => pts.iter().any(|p| p[2] < -1e-9),
        other => other.bounds().0[2] < -1e-9,
    };
    if g.ground && g.solids.iter().any(below) {
        bail!("With an infinite ground plane everything must stay at z ≥ 0.");
    }
    Ok(())
}

/// A plane of field samples at one frequency, in dB-free linear magnitude
/// relative to its own peak (FDTD).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FieldPlane {
    pub name: String,
    pub nu: usize,
    pub nv: usize,
    pub u: (f64, f64),
    pub v: (f64, f64),
    /// Corner and its two edges, metres.
    pub corner: P3,
    pub du: P3,
    pub dv: P3,
    pub mag: Vec<f32>,
    #[serde(default, skip_serializing)]
    pub frames: Vec<Vec<f32>>,
}

/// Surface currents from MoM: the discretised model and a value per element.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CurrentView {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub segments: Vec<[u32; 2]>,
    /// One per segment, then one per triangle.
    pub current: Vec<f64>,
}

/// What a mesh convergence study found for the mesh of a result.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshCheck {
    pub converged: bool,
    pub passes: usize,
    /// The largest change of any S entry against the previous pass.
    pub delta_s: f64,
    /// What that change says is left: `delta_s / (ratio − 1)` for a first-order error.
    pub remaining: f64,
}

/// Everything one simulation returns.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SimResult {
    pub label: String,
    pub solver: String,
    pub network: Network,
    /// `far[port][k]`: the far field for that port driven, at `far_f[k]`.
    pub far: Vec<Vec<FarField>>,
    pub far_f: Vec<f64>,
    #[serde(default)]
    pub planes: Vec<FieldPlane>,
    #[serde(default)]
    pub currents: Option<CurrentView>,
    /// FDTD: cells; MoM: unknowns.
    pub size: u64,
    pub seconds: f64,
    pub dir: String,
    pub vars: BTreeMap<String, f64>,
    pub warnings: Vec<String>,
    /// Mesh density of the run, in the unit below.
    #[serde(default)]
    pub density: f64,
    #[serde(default)]
    pub density_unit: String,
    /// FDTD: the energy decay it stopped at (dB, positive). 0 for MoM.
    #[serde(default)]
    pub end_db: f64,
    /// The metal the model was solved with, as named in the project.
    #[serde(default)]
    pub metal: String,
    /// No loss is modelled: radiated over accepted power only checks the far-field integral.
    #[serde(default)]
    pub lossless: bool,
    /// `None`: no convergence study was run for this result.
    #[serde(default)]
    pub convergence: Option<MeshCheck>,
}

impl SimResult {
    /// The pattern with the ports driven by `weights`, at `far_f[k]`. Accepted power comes
    /// from the network, `‖a‖² − ‖S·a‖²`, so it counts how the ports' waves combine.
    pub fn combined(&self, k: usize, weights: &[C64]) -> Option<FarField> {
        let list: Vec<&FarField> = self.far.iter().filter_map(|p| p.get(k)).collect();
        let mut out = combine(&list, weights)?;
        if let Some(p) = self.far_f.get(k).and_then(|f| accepted_from_s(&self.network, *f, &list, weights)) {
            out.p_accepted = p;
        }
        Some(out)
    }

    pub fn metrics(&self, port: usize, k: usize) -> Option<Metrics> {
        self.far.get(port)?.get(k).map(FarField::metrics)
    }

    /// Index of the far-field frequency closest to `f`.
    pub fn far_index(&self, f: f64) -> Option<usize> {
        self.far_f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map(|x| x.0)
    }
}

/// `‖a‖² − ‖S·a‖²` at `f`, with each port's incident wave from its weight and field
/// normalisation. `None` without network data covering `f`.
fn accepted_from_s(net: &Network, f: f64, list: &[&FarField], weights: &[C64]) -> Option<f64> {
    let n = net.ports();
    if n == 0 || list.len() != n || weights.len() != n || net.s.is_empty() || f < *net.f.first()? || f > *net.f.last()? {
        return None;
    }
    let s = net.interpolate(&[f]).s.swap_remove(0);
    let a: Vec<C64> = weights.iter().zip(list).map(|(w, ff)| *w * ff.p_incident.max(0.0).sqrt()).collect();
    let incident: f64 = a.iter().map(C64::norm_sqr).sum();
    let reflected: f64 = s.iter().map(|row| row.iter().zip(&a).map(|(v, x)| v * x).sum::<C64>().norm_sqr()).sum();
    Some(incident - reflected)
}

/// Superposition of per-port far fields, each for 1 W incident: the fields add with
/// `weights`, incident power with `|w|²`. Accepted power is only estimated from the
/// terms, which is off where the ports couple: realized gain doesn't depend on it, and
/// [`SimResult::combined`] has the network to do it exactly.
pub fn combine(list: &[&FarField], weights: &[C64]) -> Option<FarField> {
    let first = *list.first()?;
    let mut out = FarField { f: first.f, theta: first.theta.clone(), phi: first.phi.clone(), ground: first.ground, ..Default::default() };
    let zero = C64::new(0.0, 0.0);
    out.e_theta = vec![vec![zero; first.phi.len()]; first.theta.len()];
    out.e_phi = out.e_theta.clone();
    for (ff, w) in list.iter().zip(weights) {
        if ff.theta.len() != first.theta.len() || ff.phi.len() != first.phi.len() {
            return None;
        }
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                out.e_theta[ti][pi] += ff.e_theta[ti][pi] * *w;
                out.e_phi[ti][pi] += ff.e_phi[ti][pi] * *w;
            }
        }
        out.p_incident += ff.p_incident * w.norm_sqr();
        out.p_accepted += ff.p_accepted * w.norm_sqr();
    }
    Some(out)
}

/// One design goal. `penalty` is 0 when met and grows in dB as it is missed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Goal {
    /// |S11| at or below `db` over a band.
    MatchBelow { port: usize, f_lo: f64, f_hi: f64, db: f64 },
    /// Isolation: |Sij| at or below `db` over a band.
    IsolationBelow { i: usize, j: usize, f_lo: f64, f_hi: f64, db: f64 },
    /// Resonance (deepest match) at `f` within `tol` (Hz).
    ResonanceAt { port: usize, f: f64, tol: f64 },
    /// Peak realised gain at or above `dbi`.
    GainAbove { f: f64, dbi: f64 },
    /// Radiation efficiency at or above (0..1).
    EfficiencyAbove { f: f64, eff: f64 },
    /// Front-to-back at or above `db`.
    FrontToBackAbove { f: f64, db: f64 },
    /// Axial ratio at the peak at or below `db`.
    AxialRatioBelow { f: f64, db: f64 },
    /// Side lobes at or below `db` (negative).
    SideLobesBelow { f: f64, db: f64 },
}

impl Goal {
    pub fn label(&self) -> String {
        let ghz = |f: &f64| f / 1e9;
        match self {
            Goal::MatchBelow { port, f_lo, f_hi, db } => format!("S{0}{0} ≤ {db:.0} dB over {1:.3}–{2:.3} GHz", port + 1, ghz(f_lo), ghz(f_hi)),
            Goal::IsolationBelow { i, j, f_lo, f_hi, db } => format!("S{}{} ≤ {db:.0} dB over {:.3}–{:.3} GHz", i + 1, j + 1, ghz(f_lo), ghz(f_hi)),
            Goal::ResonanceAt { port, f, tol } => format!("Port {} resonance at {:.3} GHz ± {:.0} MHz", port + 1, ghz(f), tol / 1e6),
            Goal::GainAbove { f, dbi } => format!("Realized gain ≥ {dbi:.1} dBi at {:.3} GHz", ghz(f)),
            Goal::EfficiencyAbove { f, eff } => format!("Radiation efficiency ≥ {:.0} % at {:.3} GHz", eff * 100.0, ghz(f)),
            Goal::FrontToBackAbove { f, db } => format!("Front/back ≥ {db:.0} dB at {:.3} GHz", ghz(f)),
            Goal::AxialRatioBelow { f, db } => format!("Axial ratio ≤ {db:.1} dB at {:.3} GHz", ghz(f)),
            Goal::SideLobesBelow { f, db } => format!("Side lobes ≤ {db:.0} dB at {:.3} GHz", ghz(f)),
        }
    }

    /// Distance from the goal in dB (or dB-like units). Zero when met.
    pub fn penalty(&self, r: &SimResult) -> f64 {
        // A figure that is NaN has met nothing.
        let pos = |x: f64| if x.is_nan() { 40.0 } else { x.max(0.0) };
        let far = |f: &f64| -> Option<Metrics> { r.far_index(*f).and_then(|k| r.metrics(0, k)) };
        match self {
            Goal::MatchBelow { port, f_lo, f_hi, db } => band_worst(&r.network, *port, *port, *f_lo, *f_hi).map_or(40.0, |w| pos(w - db)),
            Goal::IsolationBelow { i, j, f_lo, f_hi, db } => band_worst(&r.network, *i, *j, *f_lo, *f_hi).map_or(40.0, |w| pos(w - db)),
            Goal::ResonanceAt { port, f, tol } => {
                let s = r.network.sij_db(*port, *port);
                let best = r.network.f.iter().zip(&s).min_by(|a, b| a.1.total_cmp(b.1)).map(|x| *x.0);
                best.map_or(40.0, |fr| pos(((fr - f).abs() - tol) / (tol.max(1.0)) * 3.0))
            }
            Goal::GainAbove { f, dbi } => far(f).map_or(40.0, |m| pos(dbi - m.peak_realized_dbi)),
            Goal::EfficiencyAbove { f, eff } => far(f).map_or(40.0, |m| pos(crate::db10(*eff) - crate::db10(m.rad_efficiency))),
            Goal::FrontToBackAbove { f, db } => far(f).map_or(40.0, |m| m.front_to_back_db.map_or(0.0, |v| pos(db - v))),
            Goal::AxialRatioBelow { f, db } => far(f).map_or(40.0, |m| pos(m.axial_ratio_db.min(40.0) - db)),
            Goal::SideLobesBelow { f, db } => far(f).map_or(40.0, |m| m.side_lobe_db.map_or(0.0, |v| pos(v - db))),
        }
    }
}

/// Worst |Sij| in dB over a band.
pub fn band_worst(n: &Network, i: usize, j: usize, f_lo: f64, f_hi: f64) -> Option<f64> {
    if n.s.is_empty() || i >= n.ports() || j >= n.ports() {
        return None;
    }
    let db = n.sij_db(i, j);
    let mut worst: Option<f64> = None;
    for (f, v) in n.f.iter().zip(&db) {
        if *f >= f_lo && *f <= f_hi {
            worst = Some(worst.map_or(*v, |w: f64| if w.is_nan() || v.is_nan() { f64::NAN } else { w.max(*v) }));
        }
    }
    worst.or_else(|| {
        // Band narrower than the sampling: the nearest sample.
        let mid = (f_lo + f_hi) / 2.0;
        n.f.iter().zip(&db).min_by(|a, b| (a.0 - mid).abs().total_cmp(&(b.0 - mid).abs())).map(|x| *x.1)
    })
}

/// Weighted total of goal penalties; `goals` as (goal, weight).
pub fn total_penalty(goals: &[(Goal, f64)], r: &SimResult) -> f64 {
    goals.iter().map(|(g, w)| g.penalty(r) * w).sum()
}

/// The frequency of a free-space wavelength.
pub fn wavelength(f: f64) -> f64 {
    C0 / f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_builds_a_valid_geometry() {
        for t in templates::all().iter().filter(|t| t.id != "custom") {
            let p = Project::from_template(t.id, None).unwrap_or_else(|e| panic!("{}: {e:#}", t.id));
            let g = p.geometry().unwrap_or_else(|e| panic!("{}: {e:#}", t.id));
            assert!(!g.ports.is_empty(), "{}", t.id);
            assert!(p.setup.f_lo < p.setup.f_hi, "{}", t.id);
            let fit = g.solver_fit();
            assert!(fit.fdtd, "{}", t.id);
        }
    }

    #[test]
    fn fitting_a_band_sweeps_it_and_sizes_the_antenna_for_its_centre() {
        let mut p = Project::from_template("rfid_gate", None).unwrap();
        let width = |p: &Project| {
            let (lo, hi) = p.geometry().unwrap().bounds();
            hi[1] - lo[1]
        };
        let at_915 = width(&p);
        p.fit_band(865e6, 868e6);
        assert_eq!((p.setup.f_lo, p.setup.f_hi), (865e6, 868e6));
        assert_eq!(p.setup.far_freqs, vec![866.5e6]);
        assert!((p.f0() - 866.5e6).abs() < 1.0, "{}", p.f0());
        assert!((width(&p) / at_915 - 915.0 / 866.5).abs() < 1e-6, "the panel scales with the wavelength");
    }

    #[test]
    fn project_round_trips_through_json() {
        let p = Project::from_template("patch_probe", Some("5.8GHz")).unwrap();
        let q = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(p, q);
        assert!((q.f0() - 5.8e9).abs() < 1.0);
    }

    #[test]
    fn changing_a_variable_changes_the_model() {
        let p = Project::from_template("dipole", Some("1GHz")).unwrap();
        let a = p.geometry().unwrap().bounds();
        let q = p.with_values(&[("L".to_string(), 0.3)]);
        let b = q.geometry().unwrap().bounds();
        // Half the length, plus the wire radius in the bounds.
        assert!((a.1[2] - 0.0707).abs() < 1e-3, "{:?}", a);
        assert!((b.1[2] - 0.1503).abs() < 1e-3, "{:?}", b);
    }

    #[test]
    fn extra_parts_use_expressions_and_ports_are_checked() {
        let mut p = Project::from_template("dipole", Some("1GHz")).unwrap();
        p.extra.push(ExtraSolid {
            name: "reflector".into(),
            material: "PEC".into(),
            prim: ExtraPrim::Box { min: ["-L/2".into(), "0.25*lambda0".into(), "-L/2".into()], max: ["L/2".into(), "0.25*lambda0".into(), "L/2".into()] },
        });
        let g = p.geometry().unwrap();
        assert_eq!(g.solids.len(), 2);
        p.extra_ports.push(ExtraPort { name: "bad".into(), a: ["0".into(), "0".into(), "0".into()], b: ["1mm".into(), "1mm".into(), "0".into()], width: "0".into(), z0: "50".into() });
        assert!(p.geometry().is_err());
    }

    #[test]
    fn a_template_made_editable_solves_the_same() {
        for id in ["strip_dipole", "slot", "patch_air", "rfid_gate"] {
            let t = Project::from_template(id, None).unwrap();
            let e = t.editable_copy().unwrap();
            assert_eq!(e.template, "custom");
            let (gt, ge) = (t.geometry().unwrap(), e.geometry().unwrap());
            assert_eq!(gt.solids.len(), ge.solids.len(), "{id}");
            assert_eq!(gt.ports.len(), ge.ports.len(), "{id}");
            let f = t.f0();
            let opts = crate::mom::MomOptions { edges_per_lambda: t.setup.edges_per_lambda.min(12.0), ..Default::default() };
            let z = |g: &Geometry| crate::mom::MomProblem::new(g, f, opts).unwrap().solve(f, false).unwrap().z[0][0];
            let (zt, ze) = (z(&gt), z(&ge));
            assert!((zt - ze).norm() / zt.norm() < 0.03, "{id}: template {zt} vs editable {ze}");
        }
        // A substrate stays a solid.
        let p = Project::from_template("patch_probe", None).unwrap().editable_copy().unwrap();
        assert!(p.geometry().unwrap().solids.iter().any(|s| s.material == "FR-4"));
    }

    #[test]
    fn goals_score_a_network() {
        let f = vec![1e9, 1.1e9, 1.2e9];
        let s = |db: f64| vec![vec![C64::new(10f64.powf(db / 20.0), 0.0)]];
        let n = Network { f, z0: vec![50.0], s: vec![s(-5.0), s(-20.0), s(-5.0)] };
        let r = SimResult { network: n, ..Default::default() };
        let ok = Goal::MatchBelow { port: 0, f_lo: 1.05e9, f_hi: 1.15e9, db: -10.0 };
        assert_eq!(ok.penalty(&r), 0.0);
        let miss = Goal::MatchBelow { port: 0, f_lo: 0.9e9, f_hi: 1.3e9, db: -10.0 };
        assert!((miss.penalty(&r) - 5.0).abs() < 1e-9);
        assert_eq!(Goal::ResonanceAt { port: 0, f: 1.1e9, tol: 1e7 }.penalty(&r), 0.0);
    }

    /// With S11 = 0.2 and S21 = 0.3 driven in phase, 1.5 W of the 2 W goes in, not
    /// the 1.92 W the per-port terms add up to.
    #[test]
    fn combined_drive_accepts_what_s_says() {
        let mut one = FarField::grid(1e9, 30.0, 30.0);
        one.p_incident = 1.0;
        one.p_accepted = 0.96;
        let s = vec![vec![C64::new(0.2, 0.0), C64::new(0.3, 0.0)], vec![C64::new(0.3, 0.0), C64::new(0.2, 0.0)]];
        let r = SimResult { network: Network { f: vec![0.9e9, 1e9, 1.1e9], z0: vec![50.0, 50.0], s: vec![s.clone(), s.clone(), s] }, far: vec![vec![one.clone()], vec![one]], far_f: vec![1e9], ..Default::default() };
        let even = r.combined(0, &[C64::new(1.0, 0.0), C64::new(1.0, 0.0)]).unwrap();
        assert!((even.p_accepted - 1.5).abs() < 1e-9, "{}", even.p_accepted);
        assert!((even.p_incident - 2.0).abs() < 1e-12);
        let odd = r.combined(0, &[C64::new(1.0, 0.0), C64::new(-1.0, 0.0)]).unwrap();
        // |S11 − S21| = 0.1 at both ports.
        assert!((odd.p_accepted - 1.98).abs() < 1e-9, "{}", odd.p_accepted);
    }

    /// Remaining amplitude 10^(−50/20) = 0.3 %, which puts −20 dB S11 inside ±0.3 dB.
    #[test]
    fn the_default_fdtd_stop_is_minus_50_db() {
        assert_eq!(Setup::for_band(1e9, 2e9, 1.5e9).end_db, 50.0);
    }

    #[test]
    fn an_unknown_material_is_an_error_not_pec() {
        let mut p = Project::from_template("dipole", Some("1GHz")).unwrap();
        p.metal = "Unobtainium".into();
        let e = p.geometry().unwrap_err().to_string();
        assert!(e.contains("Unobtainium"), "{e}");
        let mut p = Project::from_template("patch_probe", Some("2.45GHz")).unwrap();
        p.substrate = "Fibreglass".into();
        let e = p.geometry().unwrap_err().to_string();
        assert!(e.contains("Fibreglass"), "{e}");
        // The library is matched without regard to case, everywhere.
        let mut p = Project::from_template("dipole", Some("1GHz")).unwrap();
        p.metal = "copper".into();
        assert!(p.geometry().is_ok());
    }

    #[test]
    fn a_figure_that_is_nan_does_not_meet_a_goal() {
        let mut ff = FarField::grid(1e9, 30.0, 30.0);
        ff.p_accepted = f64::NAN;
        ff.p_incident = 1.0;
        let r = SimResult { far: vec![vec![ff]], far_f: vec![1e9], network: Network { f: vec![1e9, 1.1e9], z0: vec![50.0], s: vec![vec![vec![C64::new(f64::NAN, 0.0)]]; 2] }, ..Default::default() };
        assert!(Goal::EfficiencyAbove { f: 1e9, eff: 0.5 }.penalty(&r) > 0.0);
        assert!(Goal::MatchBelow { port: 0, f_lo: 1e9, f_hi: 1.1e9, db: -10.0 }.penalty(&r) > 0.0);
    }

    #[test]
    fn far_fields_combine_with_weights() {
        let mut a = FarField::grid(1e9, 30.0, 30.0);
        let mut b = FarField::grid(1e9, 30.0, 30.0);
        for ti in 0..a.theta.len() {
            for pi in 0..a.phi.len() {
                a.e_theta[ti][pi] = C64::new(1.0, 0.0);
                b.e_phi[ti][pi] = C64::new(1.0, 0.0);
            }
        }
        a.p_incident = 1.0;
        b.p_incident = 1.0;
        let c = combine(&[&a, &b], &[C64::new(1.0, 0.0), C64::new(0.0, 1.0)]).unwrap();
        assert_eq!(c.e_phi[2][2], C64::new(0.0, 1.0));
        assert!((c.p_incident - 2.0).abs() < 1e-12);
    }
}
