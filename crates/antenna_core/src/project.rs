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
    /// The built-in finite-integration solver, time domain, CPU or GPU.
    Fit,
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
            SolverKind::Fit => "FDM time domain (built in)",
            SolverKind::MomWire => "MoM, wires",
            SolverKind::MomSurface => "MoM, surfaces (RWG)",
        }
    }

    /// Marches a mesh of cells through time, as opposed to solving a matrix per frequency.
    pub fn is_time_domain(self) -> bool {
        matches!(self, SolverKind::Fdtd | SolverKind::Fit)
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
    /// Built-in solver: run again, adding cells where the field sits, until S stops changing.
    #[serde(default)]
    pub adaptive: bool,
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
            adaptive: false,
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
    /// `None` if its row is broken, or uses one that is.
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
    /// A trace `w` wide along a path, each point an expression, so a variable can move any corner.
    Strip { axis: usize, at: String, path: Vec<[String; 2]>, w: String },
    /// A flat plate with holes cut in it, all of it expressions.
    Plate { axis: usize, at: String, outline: Vec<[String; 2]>, holes: Vec<Vec<[String; 2]>> },
    Disc { axis: usize, at: String, c: [String; 2], r: String },
    /// A trace along part of a circle, angles in degrees from +u towards +v.
    Arc { axis: usize, at: String, c: [String; 2], r: String, from_deg: String, to_deg: String, w: String },
    /// `n` periods from `start`: up `height`, along `step`, down twice `height`, along `step`, back
    /// up. The count is an expression too, rounded, so it can be searched.
    Meander { axis: usize, at: String, start: [String; 2], step: String, height: String, n: String, w: String },
    /// A shape drawn on the canvas, every dimension an expression, so what was drawn with the
    /// mouse can be typed exactly, tied to a variable, swept and optimised. It can have
    /// thickness (a dielectric block) and holes (a slot).
    Drawn {
        axis: usize,
        at: String,
        thick: String,
        form: Form,
        #[serde(default)]
        holes: Vec<Vec<[String; 2]>>,
    },
}

/// The outline of a drawn part, by the dimensions a designer thinks of it in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum Form {
    Rect { c: [String; 2], size: [String; 2] },
    Circle { c: [String; 2], r: String },
    Poly { pts: Vec<[String; 2]> },
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
    /// Feeds, by name, that are wave ports (see [`Geometry::wave_ports`]).
    #[serde(default)]
    pub wave_feeds: Vec<String>,
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
    /// What lies over and under flat metal: label paper, film, a carton (see [`crate::stack`]).
    #[serde(default)]
    pub stack: crate::stack::Stack,
    /// Beams, direction finding, the receive chain and radar (see [`crate::phased`]).
    #[serde(default)]
    pub phased: crate::phased::PhasedDesign,
    /// Bodies modelled in 3D: sketches and features in a tree (see [`crate::modeler`]).
    #[serde(default, skip_serializing_if = "crate::modeler::Model::nothing_to_save")]
    pub model: crate::modeler::Model,
    /// Materials the user defined for this project, on top of the library.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<material::Material>,
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
            wave_feeds: Vec::new(),
            hidden: Vec::new(),
            setup: Setup::for_band(1.0, 2.0, 1.5),
            drive: Vec::new(),
            states: Vec::new(),
            reconfig: Default::default(),
            stack: Default::default(),
            phased: Default::default(),
            model: Default::default(),
            materials: Vec::new(),
        };
        pr.reset_band()?;
        if let Some(rc) = crate::reconfig::for_template(id, pr.f0()) {
            pr.reconfig = rc;
        }
        // An array is judged at one frequency, and every port's pattern costs time.
        if id.starts_with("array_") {
            pr.setup.mom_samples = 7;
            pr.setup.far_freqs = vec![pr.f0()];
            pr.setup.far_step = 5.0;
            pr.phased.use_ports = true;
            pr.phased.df.use_ports = true;
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
        // Everything that builds or solves a project comes through here first.
        material::remember_user(&self.materials);
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

    /// Calls a variable something else, here and everywhere an expression uses it. A template's
    /// formulas are code and ask for their rows by name, so those can't be renamed.
    pub fn rename_variable(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        if !self.params.iter().any(|p| p.name == old) {
            bail!("There's no variable called {old}.");
        }
        if expr::names(new).ok().as_deref() != Some(&[new.to_string()][..]) {
            bail!("'{new}' can't be a variable's name: use letters, digits and _, starting with a letter.");
        }
        if self.var_hints().iter().any(|h| h.name == new) {
            bail!("There's already a variable called {new}.");
        }
        if Self::from_template(&self.template, None).is_ok_and(|t| t.params.iter().any(|p| p.name == old)) {
            bail!("The template's own formulas use {old} by that name, so it stays as it is.");
        }
        // Every string that can hold an expression. Names, notes and the like are left alone: a
        // part called L is no business of a variable called L.
        fn walk(v: &mut serde_json::Value, old: &str, new: &str) {
            match v {
                serde_json::Value::String(s) => *s = rewrite(s, old, new),
                serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, old, new)),
                serde_json::Value::Object(o) => {
                    for (k, x) in o.iter_mut() {
                        // Only when it's text: a feature's `kind` is the feature itself.
                        let words = x.is_string() && matches!(k.as_str(), "name" | "note" | "material" | "file" | "template" | "metal" | "substrate" | "type" | "form" | "kind" | "mode" | "extent" | "op");
                        // An equation curve's own parameter is called t, whatever else is.
                        let parameter = old == "t" && matches!(k.as_str(), "u" | "v");
                        if parameter {
                            continue;
                        }
                        if !words && !matches!(k.as_str(), "wave_feeds" | "hidden") {
                            walk(x, old, new);
                        }
                    }
                }
                _ => {}
            }
        }
        fn rewrite(s: &str, old: &str, new: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut at = 0;
            for t in expr::tokens(s, |n| n == old) {
                if t.kind == expr::TokenKind::Variable && &s[t.start..t.end] == old {
                    out.push_str(&s[at..t.start]);
                    out.push_str(new);
                    at = t.end;
                }
            }
            out.push_str(&s[at..]);
            out
        }
        let mut text = serde_json::to_value(&*self)?;
        if let Some(o) = text.as_object_mut() {
            for (k, x) in o.iter_mut() {
                if matches!(k.as_str(), "params" | "extra" | "extra_ports" | "model") {
                    walk(x, old, new);
                }
            }
        }
        let mut p: Project = serde_json::from_value(text)?;
        if let Some(row) = p.params.iter_mut().find(|r| r.name == old) {
            row.name = new.to_string();
        }
        p.vars()?;
        *self = p;
        Ok(())
    }

    /// A material of the project's own has a new name: what uses it follows.
    pub fn rename_material(&mut self, old: &str, new: &str) {
        for e in &mut self.extra {
            if e.material == old {
                e.material = new.to_string();
            }
        }
        for f in &mut self.model.features {
            if let Some(out) = f.kind.output_mut()
                && out.material == old
            {
                out.material = new.to_string();
            }
        }
        if self.metal == old {
            self.metal = new.to_string();
        }
        if self.substrate == old {
            self.substrate = new.to_string();
        }
        for l in self.stack.above.iter_mut().chain(self.stack.below.iter_mut()) {
            if l.material == old {
                l.material = new.to_string();
            }
        }
    }

    /// What uses a material, in words, or nothing if nothing does.
    pub fn uses_of_material(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.metal == name {
            out.push("the antenna's metal".to_string());
        }
        if self.substrate == name {
            out.push("the antenna's substrate".to_string());
        }
        out.extend(self.extra.iter().filter(|e| e.material == name).map(|e| e.name.clone()));
        out.extend(self.model.features.iter().filter(|f| f.kind.output().is_some_and(|o| o.material == name)).map(|f| f.name.clone()));
        out.extend(self.stack.above.iter().chain(self.stack.below.iter()).filter(|l| l.material == name).map(|l| l.name.clone()));
        out
    }

    /// What the table comes to with its broken rows, and what uses them, left out. One
    /// half-typed row stops [`Self::vars`], and all the others still have their values.
    fn partial_vars(&self) -> Vars {
        let mut known = self.base_vars();
        let mut rows: Vec<&Param> = self.params.iter().collect();
        loop {
            let before = rows.len();
            rows.retain(|r| match expr::eval(&r.expr, &known) {
                Ok(v) => {
                    known.insert(r.name.clone(), v);
                    false
                }
                Err(_) => true,
            });
            if rows.len() == before {
                return known;
            }
        }
    }

    /// Every name an expression may use, table rows first. A broken row loses its value,
    /// as does whatever uses it, but never its name: that may be how it gets mended.
    pub fn var_hints(&self) -> Vec<VarHint> {
        let vars = self.vars().unwrap_or_else(|_| self.partial_vars());
        let mut out: Vec<VarHint> = self.params.iter().map(|p| VarHint { name: p.name.clone(), value: vars.get(&p.name).copied(), note: p.note.clone() }).collect();
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
        let mut g = self.scatterer()?;
        if g.ports.is_empty() {
            bail!("The model has no port. Add a feed: Feed on the ribbon's Model tab.");
        }
        g.wave_ports = g.ports.iter().enumerate().filter(|(_, p)| self.wave_feeds.contains(&p.name)).map(|(i, _)| i).collect();
        Ok(g)
    }

    /// The same model when nothing has to feed it: a part list being built, or a
    /// target for a radar cross section.
    pub fn scatterer(&self) -> Result<Geometry> {
        let vars = self.vars()?;
        let mut g = self.parts_geometry(&vars)?;
        if !self.model.is_empty() {
            let built = self.model.build(&vars);
            // A solver can't run on a model with a step missing, whatever the canvas can still show.
            if let Some((id, why)) = built.errors.first() {
                let name = self.model.feature(*id).map_or("A feature", |f| f.name.as_str());
                bail!("{name}: {why}");
            }
            g.solids.extend(built.solids(&self.metal));
            g.hints.extend(built.hints(C0 / self.setup.f_hi.max(1.0) / 300.0));
            g.ports.extend(built.ports);
        }
        if !self.stack.is_empty() {
            self.stack.check()?;
            g.stack = Some(self.stack.clone());
        }
        check(&g)?;
        Ok(g)
    }

    /// The template's parts and those added to it, without what's modelled in 3D and with nothing
    /// checked: what the canvas draws under the bodies while a feature is being worked on.
    pub fn parts_geometry(&self, vars: &Vars) -> Result<Geometry> {
        // `base_vars` would quietly read an unknown substrate as air.
        if material::by_name(&self.substrate).is_none() {
            bail!("The substrate '{}' isn't in the material library.", self.substrate);
        }
        let t = templates::by_id(&self.template).with_context(|| format!("no template '{}'", self.template))?;
        let ctx = templates::Ctx { v: vars, metal: &self.metal, substrate: &self.substrate };
        let mut g = (t.build)(&ctx)?;
        g.solids.retain(|s| !self.hidden.contains(&s.name));
        // An air substrate is no substrate at all (and keeps MoM available).
        g.solids.retain(|s| s.material != "Air");
        let finest = C0 / self.setup.f_hi.max(1.0) / 300.0;
        for e in &self.extra {
            let solid = extra_solid(e, vars)?;
            // A drawing has no template to say where its fine detail is.
            if let ExtraPrim::Outline { cell, .. } = &e.prim
                && *cell > 0.0
            {
                let (min, max) = solid.shape.bounds();
                g.hints.push(MeshHint { min, max, cell: cell.max(finest) });
            }
            // Traces ask for cells a third of their own width.
            if let ExtraPrim::Strip { w, .. } | ExtraPrim::Arc { w, .. } | ExtraPrim::Meander { w, .. } = &e.prim {
                let (min, max) = solid.shape.bounds();
                g.hints.push(MeshHint { min, max, cell: (expr::eval(w, vars)? / 3.0).max(finest) });
            }
            // A drawn shape asks for a third of its narrowest feature, holes included.
            if let ExtraPrim::Drawn { form, holes, .. } = &e.prim {
                let mut rings = vec![form_outline(&e.name, form, vars)?];
                for h in holes {
                    rings.push(eval_points(h, vars)?);
                }
                let narrowest = crate::cad::narrowest_of(&rings);
                if narrowest.is_finite() && narrowest > 0.0 {
                    let (min, max) = solid.shape.bounds();
                    g.hints.push(MeshHint { min, max, cell: (narrowest / 3.0).max(finest) });
                }
            }
            g.solids.push(solid);
        }
        for p in &self.extra_ports {
            let e3 = |a: &[String; 3]| -> Result<P3> { Ok([expr::eval(&a[0], vars)?, expr::eval(&a[1], vars)?, expr::eval(&a[2], vars)?]) };
            g.ports.push(PortDef { name: p.name.clone(), a: e3(&p.a)?, b: e3(&p.b)?, width: expr::eval(&p.width, vars)?, z0: expr::eval(&p.z0, vars)? });
        }
        Ok(g)
    }

    /// The same model with every part and port its own to edit: the template's formulas
    /// are left behind, its variables stay for whatever else uses them.
    pub fn editable_copy(&self) -> Result<Self> {
        // What's modelled in 3D is editable already, and stays in its tree.
        let g = self.parts_geometry(&self.vars()?)?;
        let mut p = self.clone();
        p.template = "custom".into();
        p.hidden.clear();
        p.extra.clear();
        p.extra_ports.clear();
        let num = |v: f64| crate::sketch::mm(v);
        let n3 = |q: P3| [num(q[0]), num(q[1]), num(q[2])];
        for s in &g.solids {
            let prim = match &s.shape {
                // A flat box is a sheet, and sheets are what the drawing tools edit: it opens
                // with its centre and sides to change.
                Shape::Box { min, max } => match (0..3).find(|k| max[*k] - min[*k] <= 0.0) {
                    Some(axis) => {
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        let form = Form::Rect { c: [num((min[u] + max[u]) / 2.0), num((min[v] + max[v]) / 2.0)], size: [num(max[u] - min[u]), num(max[v] - min[v])] };
                        ExtraPrim::Drawn { axis, at: num(min[axis]), thick: "0".into(), form, holes: Vec::new() }
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
            p.extra_ports.push(ExtraPort { name: port.name.clone(), a: n3(port.a), b: n3(port.b), width: num(port.width), z0: format!("{}", port.z0) });
        }
        Ok(p)
    }

    /// The same antenna with its template's parts and flat drawings as steps of the 3D model, so
    /// they can be built on, cut and sketched on. Their sizes become numbers, as in
    /// [`Self::editable_copy`]; the variables stay for whatever else uses them.
    pub fn parts_into_model(&self) -> Result<Self> {
        use crate::modeler::{Anchor, Extent, Kind, Mode, Output, PathRef, PlaneRef, Prim, sketch::Sketch};
        let vars = self.vars()?;
        let g = self.parts_geometry(&vars)?;
        if g.ground {
            bail!("This antenna stands on an infinite ground plane, which only its template can give it.");
        }
        let mut p = self.clone();
        p.template = "custom".into();
        p.hidden.clear();
        p.extra.clear();
        p.extra_ports.clear();
        let num = crate::modeler::sketch::mm_text;
        let n3 = |q: P3| [num(q[0]), num(q[1]), num(q[2])];
        for s in &g.solids {
            let (lo, hi) = s.shape.bounds();
            // What the template asked of the mesh here goes along with the part.
            let hint = g.hints.iter().filter(|h| (0..3).all(|k| h.min[k] <= hi[k] && lo[k] <= h.max[k])).map(|h| h.cell).fold(f64::INFINITY, f64::min);
            let out = Output { mode: Mode::New, target: None, material: s.material.clone(), mesh: if hint.is_finite() { num(hint) } else { String::new() }, surface: false, ..Default::default() };
            let add = |p: &mut Project, kind: Kind| {
                let id = p.model.fresh_id();
                p.model.add_named(id, s.name.clone(), kind);
            };
            // A flat outline in a sketch of its own, named so the body keeps the part's name.
            let flat = |p: &mut Project, axis: usize, at: f64, ring: &[[f64; 2]], thick: f64| -> bool {
                let mut seen: Vec<[f64; 2]> = Vec::with_capacity(ring.len());
                if ring.iter().any(|q| {
                    let repeat = seen.iter().any(|o| (o[0] - q[0]).abs() + (o[1] - q[1]).abs() < 1e-12);
                    seen.push(*q);
                    repeat
                }) {
                    // A hole joined in by a keyhole: triangles find their own way back to an outline.
                    return false;
                }
                // Solver axes for this plane are the next two along, so z-x is a front sketch turned over.
                let (plane, to_uv): (PlaneRef, fn([f64; 2]) -> [f64; 2]) = match axis {
                    2 => (PlaneRef::Base { axis: 2, offset: num(at) }, |q| q),
                    0 => (PlaneRef::Base { axis: 0, offset: num(at) }, |q| q),
                    _ => (PlaneRef::Base { axis: 1, offset: num(-at) }, |q| [q[1], q[0]]),
                };
                let mut sk = Sketch::default();
                let ids: Vec<u32> = ring.iter().map(|q| sk.add_point(to_uv(*q))).collect();
                for k in 0..ids.len() {
                    sk.add_line(ids[k], ids[(k + 1) % ids.len()]);
                }
                let sid = p.model.fresh_id();
                p.model.add_named(sid, format!("{} outline", s.name), Kind::Sketch { plane, sketch: sk });
                let id = p.model.fresh_id();
                let kind = if thick > 0.0 {
                    // Along +axis, whichever way the plane's own normal faces.
                    let extent = if axis == 1 { Extent::Reverse } else { Extent::One };
                    Kind::Extrude { sketch: sid, depth: num(thick), extent, out: out.clone() }
                } else {
                    Kind::Sheet { sketch: sid, out: out.clone() }
                };
                p.model.add_named(id, s.name.clone(), kind);
                true
            };
            let done = match &s.shape {
                Shape::Box { min, max } => match (0..3).find(|k| max[*k] - min[*k] <= 0.0) {
                    Some(axis) => {
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        flat(&mut p, axis, min[axis], &[[min[u], min[v]], [max[u], min[v]], [max[u], max[v]], [min[u], max[v]]], 0.0)
                    }
                    None => {
                        add(&mut p, Kind::Solid { shape: Prim::Box { min: n3(*min), max: n3(*max) }, out: out.clone() });
                        true
                    }
                },
                Shape::Cylinder { a, b, r } => {
                    add(&mut p, Kind::Solid { shape: Prim::Cylinder { a: n3(*a), b: n3(*b), r: num(*r) }, out: out.clone() });
                    true
                }
                Shape::Sphere { c, r } => {
                    add(&mut p, Kind::Solid { shape: Prim::Sphere { c: n3(*c), r: num(*r) }, out: out.clone() });
                    true
                }
                Shape::Wire { pts, r } => {
                    add(&mut p, Kind::Wire { path: PathRef::Points { pts: pts.iter().map(|q| n3(*q)).collect() }, radius: num(*r), out: out.clone() });
                    true
                }
                Shape::Polygon { axis, at, pts, thick } => flat(&mut p, *axis, *at, pts, *thick),
                Shape::Tris { .. } => false,
            };
            if !done {
                let (tris, _) = crate::geometry::tessellate(&s.shape);
                add(&mut p, Kind::Import { file: s.name.clone(), tris, out });
            }
        }
        for port in &g.ports {
            let id = p.model.fresh_id();
            p.model.add_named(id, port.name.clone(), Kind::Feed { a: Anchor::Xyz { at: n3(port.a) }, b: Anchor::Xyz { at: n3(port.b) }, width: num(port.width), z0: format!("{}", port.z0) });
        }
        Ok(p)
    }

    /// The solver a run of this geometry will really use.
    pub fn solver_for(&self, geom: &Geometry) -> SolverKind {
        // Only the built-in solver has them, so asking for one chooses it.
        if self.setup.solver == SolverKind::Auto && !geom.wave_ports.is_empty() {
            return SolverKind::Fit;
        }
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

fn eval_points(pts: &[[String; 2]], v: &Vars) -> Result<Vec<[f64; 2]>> {
    pts.iter().map(|q| Ok([expr::eval(&q[0], v)?, expr::eval(&q[1], v)?])).collect()
}

/// What a drawn part's outline comes to, before any holes.
pub(crate) fn form_outline(name: &str, form: &Form, v: &Vars) -> Result<Vec<[f64; 2]>> {
    let ev = |s: &String| expr::eval(s, v).with_context(|| format!("part '{name}'"));
    match form {
        Form::Rect { c, size } => {
            let (cu, cv, su, sv) = (ev(&c[0])?, ev(&c[1])?, ev(&size[0])?, ev(&size[1])?);
            if !(su > 0.0 && sv > 0.0) {
                bail!("Rectangle '{name}' needs both sides above zero.");
            }
            Ok(crate::sketch::rectangle([cu - su / 2.0, cv - sv / 2.0], [cu + su / 2.0, cv + sv / 2.0]))
        }
        Form::Circle { c, r } => {
            let (cu, cv, r) = (ev(&c[0])?, ev(&c[1])?, ev(r)?);
            if !(r > 0.0) {
                bail!("Circle '{name}' needs a radius above zero.");
            }
            Ok(crate::sketch::circle([cu, cv], r, r * 0.003))
        }
        Form::Poly { pts } => {
            let out = eval_points(pts, v).with_context(|| format!("part '{name}'"))?;
            if out.len() < 3 {
                bail!("Polygon '{name}' needs at least three corners.");
            }
            Ok(out)
        }
    }
}

pub(crate) fn extra_solid(e: &ExtraSolid, v: &Vars) -> Result<Solid> {
    let ev = |s: &String| expr::eval(s, v).with_context(|| format!("part '{}'", e.name));
    let e3 = |a: &[String; 3]| -> Result<P3> { Ok([ev(&a[0])?, ev(&a[1])?, ev(&a[2])?]) };
    let pt_of = |q: &[String; 2]| -> Result<[f64; 2]> { Ok([ev(&q[0])?, ev(&q[1])?]) };
    let pts_of = |l: &[[String; 2]]| -> Result<Vec<[f64; 2]>> { l.iter().map(pt_of).collect() };
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
        ExtraPrim::Drawn { axis, at, thick, form, holes } => {
            let mut pts = form_outline(&e.name, form, v)?;
            for h in holes {
                pts = crate::cad::cut_hole(&pts, &pts_of(h)?);
            }
            Shape::Polygon { axis: *axis, at: ev(at)?, pts, thick: ev(thick)? }
        }
        ExtraPrim::Strip { axis, at, path, w } => {
            let path = pts_of(path)?;
            sheet(*axis, ev(at)?, stroked(&e.name, &path, ev(w)?)?)
        }
        ExtraPrim::Plate { axis, at, outline, holes } => {
            let mut pts = pts_of(outline)?;
            for h in holes {
                pts = crate::cad::cut_hole(&pts, &pts_of(h)?);
            }
            sheet(*axis, ev(at)?, pts)
        }
        ExtraPrim::Disc { axis, at, c, r } => {
            let (c, r) = (pt_of(c)?, ev(r)?);
            if !(r > 0.0) {
                bail!("Disc '{}' needs a radius above zero.", e.name);
            }
            sheet(*axis, ev(at)?, crate::sketch::circle(c, r, r * 0.003))
        }
        ExtraPrim::Arc { axis, at, c, r, from_deg, to_deg, w } => {
            let (c, r, a0, a1, w) = (pt_of(c)?, ev(r)?, ev(from_deg)?, ev(to_deg)?, ev(w)?);
            if !(r > 0.0) || (a1 - a0).abs() > 720.0 {
                bail!("Arc '{}' needs a radius above zero and a sweep of two turns at most.", e.name);
            }
            let n = (((a1 - a0).abs() / 10.0).ceil() as usize).clamp(3, 96);
            let path: Vec<[f64; 2]> = (0..=n).map(|k| {
                let t = (a0 + (a1 - a0) * k as f64 / n as f64).to_radians();
                [c[0] + r * t.cos(), c[1] + r * t.sin()]
            }).collect();
            sheet(*axis, ev(at)?, stroked(&e.name, &path, w)?)
        }
        ExtraPrim::Meander { axis, at, start, step, height, n, w } => {
            let (s, step, h, count, w) = (pt_of(start)?, ev(step)?, ev(height)?, ev(n)?.round().clamp(0.0, 64.0) as usize, ev(w)?);
            if count > 0 && !(step.abs() > 2.0 * w && h.abs() > w) {
                bail!("Meander '{}' needs steps wider than two traces and height over one.", e.name);
            }
            let mut path = vec![s];
            let mut x = s[0];
            for _ in 0..count {
                path.push([x, s[1] + h]);
                x += step;
                path.push([x, s[1] + h]);
                path.push([x, s[1] - h]);
                x += step;
                path.push([x, s[1] - h]);
                path.push([x, s[1]]);
            }
            if count == 0 {
                bail!("Meander '{}' has no periods: use a strip.", e.name);
            }
            sheet(*axis, ev(at)?, stroked(&e.name, &path, w)?)
        }
    };
    Ok(Solid { name: e.name.clone(), material: e.material.clone(), shape })
}

fn sheet(axis: usize, at: f64, pts: Vec<[f64; 2]>) -> Shape {
    Shape::Polygon { axis, at, pts, thick: 0.0 }
}

fn stroked(name: &str, path: &[[f64; 2]], w: f64) -> Result<Vec<[f64; 2]>> {
    if !(w > 0.0) {
        bail!("Trace '{name}' needs a width above zero.");
    }
    crate::cad::strip_outline(path, w).with_context(|| format!("Trace '{name}' needs a path of two different points."))
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

/// How the warning for an FDTD run cut off by its step limit starts.
pub const STOPPED_SHORT: &str = "The run stopped at its step limit";

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
    /// Cut off before its energy decayed, so its narrow-band numbers can't be relied on.
    pub fn stopped_short(&self) -> bool {
        self.warnings.iter().any(|w| w.starts_with(STOPPED_SHORT))
    }

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
    /// Worst share of the power collected that reaches a tag's chip over a band, at or above `at_least`.
    /// `z` is the chip's series impedance at 915 MHz.
    ChipPowerAbove { f_lo: f64, f_hi: f64, z: (f64, f64), at_least: f64 },
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
            Goal::ChipPowerAbove { f_lo, f_hi, at_least, .. } => format!("Power into the chip ≥ {at_least:.2} over {:.3}–{:.3} GHz", ghz(f_lo), ghz(f_hi)),
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
            Goal::ChipPowerAbove { f_lo, f_hi, z, at_least } => {
                // Across the sweep inside the band, in dB, so it weighs like the others.
                if r.network.ports() == 0 {
                    return 40.0;
                }
                let zin = r.network.zin(0);
                let worst = r.network.f.iter().zip(&zin).filter(|(f, _)| (*f_lo..=*f_hi).contains(*f)).map(|(f, za)| crate::tag::power_transfer(*za, crate::tag::chip_at(*z, *f))).fold(f64::INFINITY, f64::min);
                if worst.is_finite() { pos(crate::db10(*at_least) - crate::db10(worst)) } else { 40.0 }
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
    fn renaming_a_variable_follows_it_into_every_expression_and_leaves_names_alone() {
        use crate::modeler::{Kind, Prim};
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        p.params.push(Param::new("L", "40mm", "L as a note"));
        p.params.push(Param::new("half", "L/2", ""));
        p.params.push(Param::new("Long", "L+LL", ""));
        p.params.push(Param::new("LL", "1mm", ""));
        p.model.add_named(1, "L".into(), Kind::Solid { shape: Prim::Cuboid { p0: ["0".into(), "0".into(), "0".into()], size: ["L".into(), "half".into(), "L*0.1+LL".into()] }, out: Default::default() });
        let size = |p: &Project| {
            let b = p.model.build(&p.vars().unwrap());
            assert!(b.errors.is_empty(), "{:?}", b.errors);
            let (lo, hi) = b.bodies[0].mesh.bounds();
            std::array::from_fn::<f64, 3, _>(|k| hi[k] - lo[k])
        };
        let before = size(&p);

        p.rename_variable("L", "Len").unwrap();
        assert!(p.params.iter().any(|r| r.name == "Len" && r.expr == "40mm" && r.note == "L as a note"));
        assert_eq!(p.params.iter().find(|r| r.name == "half").unwrap().expr, "Len/2");
        assert_eq!(p.params.iter().find(|r| r.name == "Long").unwrap().expr, "Len+LL", "whole names only");
        assert_eq!(p.model.features[0].name, "L", "a part called L is its own business");
        let Kind::Solid { shape: Prim::Cuboid { size: s, .. }, .. } = &p.model.features[0].kind else { panic!() };
        assert_eq!(s[2], "Len*0.1+LL");
        assert_eq!(size(&p), before);

        let same = p.clone();
        assert!(p.rename_variable("Len", "half").is_err(), "taken");
        assert!(p.rename_variable("Len", "2 bad").is_err(), "not a name");
        assert!(p.rename_variable("nope", "x").is_err(), "not there");
        assert_eq!(p, same, "a refused rename changes nothing");
    }

    #[test]
    fn a_templates_own_rows_cannot_be_renamed_because_its_formulas_ask_for_them() {
        let mut p = Project::from_template("dipole", None).unwrap();
        let row = p.params.iter().find(|r| r.name != "f0" && !r.derived).unwrap().name.clone();
        assert!(p.rename_variable(&row, "renamed").unwrap_err().to_string().contains("template"));
    }

    #[test]
    fn a_project_can_define_a_material_of_its_own_and_parts_can_use_it() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        p.materials.push(material::Material::dielectric("Test laminate 7", 7.5, 0.01));
        p.vars().unwrap();
        let m = material::by_name("Test laminate 7").expect("found by name, as the solvers look it up");
        assert_eq!(m.eps_r(), 7.5);
        assert!(!material::built_in().iter().any(|b| b.name == "Test laminate 7"));
        assert!(material::library().iter().any(|b| b.name == "Test laminate 7"));

        // Saved with the project, and read back.
        let text = serde_json::to_string(&p).unwrap();
        assert!(text.contains("Test laminate 7"));
        let q: Project = serde_json::from_str(&text).unwrap();
        assert_eq!(q.materials, p.materials);
        // Changing it changes what's found; renaming takes its users along.
        p.materials[0] = material::Material::dielectric("Test laminate 7", 8.0, 0.01);
        p.vars().unwrap();
        assert_eq!(material::by_name("Test laminate 7").unwrap().eps_r(), 8.0);
        p.substrate = "Test laminate 7".into();
        p.rename_material("Test laminate 7", "Test laminate 8");
        assert_eq!(p.substrate, "Test laminate 8");
        // A project with none writes nothing new, so saved files don't churn.
        assert!(!serde_json::to_string(&Project::from_template("dipole", None).unwrap()).unwrap().contains("\"materials\""));
    }

    #[test]
    fn expressions_are_offered_every_name_they_may_use() {
        let p = Project::from_template("rfid_gate", Some("915MHz")).unwrap();
        let hints = p.var_hints();
        let of = |n: &str| hints.iter().find(|h| h.name == n);
        for row in &p.params {
            assert!(of(&row.name).is_some(), "{}", row.name);
        }
        assert!((of("f0").unwrap().value.unwrap() - 915e6).abs() < 1.0);
        assert!(!of("L").unwrap().note.is_empty());
        // The substrate's constants, which the table doesn't list but expressions can use.
        assert!(of("er").unwrap().value.is_some() && of("tand").unwrap().value.is_some());
        // Every one of them evaluates, or offering it would break the row it's put in.
        let vars = p.vars().unwrap();
        for h in &hints {
            assert!(expr::eval(&h.name, &vars).is_ok(), "{}", h.name);
        }

        // One half-typed row: it loses its value, and so does what uses it, and nothing else.
        let mut broken = p.clone();
        broken.params.iter_mut().find(|r| r.name == "lambda0").unwrap().expr = "c0/f".into();
        broken.params.iter_mut().find(|r| r.name == "Pw").unwrap().expr = "5mm".into();
        let hints = broken.var_hints();
        let of = |n: &str| hints.iter().find(|h| h.name == n).unwrap().value;
        assert_eq!(of("lambda0"), None);
        assert_eq!(of("L"), None, "it's 0.44*lambda0");
        assert_eq!(of("Pw"), Some(5e-3), "its row doesn't use the broken one");
        assert!((of("f0").unwrap() - 915e6).abs() < 1.0);
        assert!(of("er").is_some());
        // Rows in any order, and one going round in a circle doesn't hang it.
        let mut circle = p.clone();
        circle.params.iter_mut().find(|r| r.name == "L").unwrap().expr = "w".into();
        circle.params.iter_mut().find(|r| r.name == "w").unwrap().expr = "L".into();
        let hints = circle.var_hints();
        let of = |n: &str| hints.iter().find(|h| h.name == n).unwrap().value;
        assert_eq!((of("L"), of("w")), (None, None));
        assert!(of("lambda0").is_some());
    }

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
