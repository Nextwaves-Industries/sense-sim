//! The antenna catalogue: each template has a variable table with textbook
//! starting dimensions and builds the geometry from the evaluated variables, so
//! every dimension stays editable and can be swept or optimised.

use std::f64::consts::PI;

use anyhow::{Result, bail};

use crate::expr::Vars;
use crate::geometry::{Geometry, MeshHint, P3, PortDef, Shape, Solid};
use crate::project::Param;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Wire,
    Planar,
    Printed,
    Aperture,
    Wideband,
    Reconfigurable,
    Custom,
}

impl Category {
    pub const ALL: [Category; 7] = [Category::Wire, Category::Planar, Category::Printed, Category::Aperture, Category::Wideband, Category::Reconfigurable, Category::Custom];
    pub fn label(self) -> &'static str {
        match self {
            Category::Wire => "Wire",
            Category::Planar => "Metal sheet",
            Category::Printed => "Printed (PCB)",
            Category::Aperture => "Aperture",
            Category::Wideband => "Wideband",
            Category::Reconfigurable => "Reconfigurable",
            Category::Custom => "Custom",
        }
    }
}

pub struct Ctx<'a> {
    pub v: &'a Vars,
    pub metal: &'a str,
    pub substrate: &'a str,
}

impl Ctx<'_> {
    fn get(&self, name: &str) -> Result<f64> {
        match self.v.get(name) {
            Some(v) => Ok(*v),
            None => bail!("variable '{name}' is missing from the table"),
        }
    }
    fn count(&self, name: &str, min: usize, max: usize) -> Result<usize> {
        Ok((self.get(name)?.round().max(0.0) as usize).clamp(min, max))
    }
}

pub struct Template {
    pub id: &'static str,
    pub title: &'static str,
    pub category: Category,
    pub blurb: &'static str,
    /// Sweep band relative to f0.
    pub band: (f64, f64),
    /// Default substrate; "Air" for none.
    pub substrate: &'static str,
    pub params: fn() -> Vec<Param>,
    pub build: fn(&Ctx) -> Result<Geometry>,
}

fn p(name: &str, expr: &str, note: &str) -> Param {
    Param::new(name, expr, note)
}
fn d(name: &str, expr: &str, note: &str) -> Param {
    Param::derived(name, expr, note)
}

/// The two rows every template starts with.
fn base(f0: &str) -> Vec<Param> {
    vec![p("f0", f0, "Design frequency"), d("lambda0", "c0/f0", "Free-space wavelength")]
}

fn wire(name: &str, mat: &str, pts: Vec<P3>, r: f64) -> Solid {
    Solid { name: name.into(), material: mat.into(), shape: Shape::Wire { pts, r } }
}
/// Axis-aligned box or flat sheet.
fn bx(name: &str, mat: &str, a: P3, b: P3) -> Solid {
    let min = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
    let max = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
    Solid { name: name.into(), material: mat.into(), shape: Shape::Box { min, max } }
}
fn poly(name: &str, mat: &str, axis: usize, at: f64, pts: Vec<[f64; 2]>) -> Solid {
    Solid { name: name.into(), material: mat.into(), shape: Shape::Polygon { axis, at, pts, thick: 0.0 } }
}
fn port(name: &str, a: P3, b: P3, width: f64, z0: f64) -> PortDef {
    PortDef { name: name.into(), a, b, width, z0 }
}
fn hint(a: P3, b: P3, cell: f64) -> MeshHint {
    MeshHint { min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])], max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])], cell }
}
fn circle(cx: f64, cy: f64, r: f64, n: usize) -> Vec<[f64; 2]> {
    (0..n).map(|k| { let t = 2.0 * PI * k as f64 / n as f64; [cx + r * t.cos(), cy + r * t.sin()] }).collect()
}
/// A sheet quad as two triangles (for tilted plates).
fn quad(name: &str, mat: &str, q: [P3; 4], sheet: f64) -> Solid {
    Solid { name: name.into(), material: mat.into(), shape: Shape::Tris { tris: vec![[q[0], q[1], q[2]], [q[0], q[2], q[3]]], solid: false, sheet } }
}

// ---------------------------------------------------------------------------
// Wire antennas

fn dipole_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([p("L", "0.47*lambda0", "Total length"), p("a", "0.001*lambda0", "Wire radius"), p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"), p("Z0", "73", "Port impedance")]);
    v
}
fn dipole(c: &Ctx) -> Result<Geometry> {
    let (l, a, g, z0) = (c.get("L")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![wire("dipole", c.metal, vec![[0.0, 0.0, -l / 2.0], [0.0, 0.0, l / 2.0]], a)],
        ports: vec![port("feed", [0.0, 0.0, -g / 2.0], [0.0, 0.0, g / 2.0], 0.0, z0)],
        hints: vec![hint([0.0, 0.0, -l / 2.0], [0.0, 0.0, l / 2.0], l / 40.0)],
        ..Default::default()
    })
}

fn monopole_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([p("h", "0.235*lambda0", "Height above ground"), p("a", "0.001*lambda0", "Wire radius"), p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"), p("Z0", "50", "Port impedance")]);
    v
}
fn monopole(c: &Ctx) -> Result<Geometry> {
    let (h, a, g, z0) = (c.get("h")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![wire("monopole", c.metal, vec![[0.0, 0.0, 0.0], [0.0, 0.0, h]], a)],
        ports: vec![port("feed", [0.0, 0.0, 0.0], [0.0, 0.0, g], 0.0, z0)],
        ground: true,
        hints: vec![hint([0.0, 0.0, 0.0], [0.0, 0.0, h], h / 20.0)],
        ..Default::default()
    })
}

fn folded_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([p("L", "0.47*lambda0", "Length"), p("s", "0.02*lambda0", "Spacing of the two wires"), p("a", "0.002*lambda0", "Wire radius"), p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"), p("Z0", "300", "Port impedance")]);
    v
}
fn folded(c: &Ctx) -> Result<Geometry> {
    let (l, s, a, g, z0) = (c.get("L")?, c.get("s")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let pts = vec![[-s / 2.0, 0.0, -l / 2.0], [-s / 2.0, 0.0, l / 2.0], [s / 2.0, 0.0, l / 2.0], [s / 2.0, 0.0, -l / 2.0], [-s / 2.0, 0.0, -l / 2.0]];
    Ok(Geometry {
        solids: vec![wire("folded dipole", c.metal, pts, a)],
        ports: vec![port("feed", [-s / 2.0, 0.0, -g / 2.0], [-s / 2.0, 0.0, g / 2.0], 0.0, z0)],
        hints: vec![hint([-s / 2.0, 0.0, -l / 2.0], [s / 2.0, 0.0, l / 2.0], s / 4.0)],
        ..Default::default()
    })
}

fn loop_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([p("S", "0.28*lambda0", "Side of the square"), p("a", "0.002*lambda0", "Wire radius"), p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"), p("Z0", "130", "Port impedance")]);
    v
}
fn square_loop(c: &Ctx) -> Result<Geometry> {
    let (s, a, g, z0) = (c.get("S")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let h = s / 2.0;
    let pts = vec![[-h, 0.0, -h], [h, 0.0, -h], [h, 0.0, h], [-h, 0.0, h], [-h, 0.0, -h]];
    Ok(Geometry {
        solids: vec![wire("loop", c.metal, pts, a)],
        ports: vec![port("feed", [-g / 2.0, 0.0, -h], [g / 2.0, 0.0, -h], 0.0, z0)],
        hints: vec![hint([-h, 0.0, -h], [h, 0.0, h], s / 20.0)],
        ..Default::default()
    })
}

fn yagi_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("n_dir", "4", "Number of directors"),
        p("Lr", "0.5*lambda0", "Reflector length"),
        p("Ld", "0.47*lambda0", "Driven element length"),
        p("Ldir", "0.42*lambda0", "First director length"),
        p("taper", "0.985", "Each next director's length ratio"),
        p("sr", "0.2*lambda0", "Reflector spacing"),
        p("sd", "0.2*lambda0", "Director spacing"),
        p("a", "0.002*lambda0", "Element radius"),
        p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn yagi(c: &Ctx) -> Result<Geometry> {
    let (lr, ld, ldir, taper, sr, sd, a, g, z0) = (c.get("Lr")?, c.get("Ld")?, c.get("Ldir")?, c.get("taper")?, c.get("sr")?, c.get("sd")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let n = c.count("n_dir", 1, 30)?;
    let el = |name: String, z: f64, len: f64| wire(&name, c.metal, vec![[0.0, -len / 2.0, z], [0.0, len / 2.0, z]], a);
    let mut solids = vec![el("reflector".into(), -sr, lr), el("driven".into(), 0.0, ld)];
    for k in 0..n {
        solids.push(el(format!("director {}", k + 1), sd * (k as f64 + 1.0), ldir * taper.powi(k as i32)));
    }
    Ok(Geometry {
        solids,
        ports: vec![port("feed", [0.0, -g / 2.0, 0.0], [0.0, g / 2.0, 0.0], 0.0, z0)],
        axis_forward: true,
        hints: vec![hint([0.0, -lr / 2.0, -sr], [0.0, lr / 2.0, sd * n as f64], lr / 40.0)],
        ..Default::default()
    })
}

fn lpda_params() -> Vec<Param> {
    let mut v = base("1.5GHz");
    v.extend([
        p("f_lo", "0.7*f0", "Lowest frequency"),
        p("f_hi", "1.4*f0", "Highest frequency"),
        p("tau", "0.88", "Scale factor τ"),
        p("sigma", "0.06", "Spacing factor σ"),
        p("a", "0.002*lambda0", "Element radius"),
        p("s", "0.03*lambda0", "Boom spacing"),
        p("Z0", "100", "Port impedance"),
    ]);
    v
}
fn lpda(c: &Ctx) -> Result<Geometry> {
    let (f_lo, f_hi, tau, sigma, a, s, z0) = (c.get("f_lo")?, c.get("f_hi")?, c.get("tau")?, c.get("sigma")?, c.get("a")?, c.get("s")?, c.get("Z0")?);
    if !(0.5..0.99).contains(&tau) {
        bail!("τ must be between 0.5 and 0.99");
    }
    let c0 = crate::C0;
    let l1 = c0 / (2.0 * f_lo);
    // Enough elements for the active region at f_hi to stay on the antenna.
    let n = (1.0 + ((1.3 * f_hi) / f_lo).ln() / (1.0 / tau).ln()).ceil().clamp(3.0, 40.0) as usize;
    let mut solids = Vec::new();
    let (mut z, mut l) = (0.0, l1);
    let xa = -s / 2.0;
    let xb = s / 2.0;
    for k in 0..n {
        let flip = if k % 2 == 0 { 1.0 } else { -1.0 };
        solids.push(wire(&format!("arm {}a", k + 1), c.metal, vec![[xa, 0.0, z], [xa, flip * l / 2.0, z]], a));
        solids.push(wire(&format!("arm {}b", k + 1), c.metal, vec![[xb, 0.0, z], [xb, -flip * l / 2.0, z]], a));
        z += 2.0 * sigma * l;
        l *= tau;
    }
    let zf = z - 2.0 * sigma * l / tau;
    solids.push(wire("boom A", c.metal, vec![[xa, 0.0, 0.0], [xa, 0.0, zf]], a));
    solids.push(wire("boom B", c.metal, vec![[xb, 0.0, 0.0], [xb, 0.0, zf]], a));
    solids.push(wire("feed link", c.metal, vec![[xa, 0.0, zf], [xb, 0.0, zf]], a));
    let g = s / 3.0;
    Ok(Geometry {
        solids,
        ports: vec![port("feed", [-g / 2.0, 0.0, zf], [g / 2.0, 0.0, zf], 0.0, z0)],
        axis_forward: true,
        hints: vec![hint([xa, 0.0, 0.0], [xb, 0.0, 0.0], s / 4.0), hint([0.0, -s, 0.0], [0.0, s, 0.0], s / 2.0)],
        ..Default::default()
    })
}

fn helix_points(r: f64, pitch: f64, turns: f64, z0: f64, hand: f64) -> Vec<P3> {
    let per_turn = 24.0;
    let n = (turns * per_turn).ceil() as usize;
    (0..=n)
        .map(|k| {
            let t = 2.0 * PI * turns * k as f64 / n as f64;
            [r * t.cos(), hand * r * t.sin(), z0 + pitch * t / (2.0 * PI)]
        })
        .collect()
}

fn helix_axial_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("C", "lambda0", "Circumference of one turn"),
        p("S", "0.23*lambda0", "Turn spacing"),
        p("N", "8", "Turns"),
        p("hand", "1", "Winding: +1 right-hand (RHCP), −1 left-hand"),
        p("a", "0.005*lambda0", "Wire radius"),
        p("g", "lambda0/60", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("Z0", "140", "Port impedance"),
    ]);
    v
}
fn helix_axial(c: &Ctx) -> Result<Geometry> {
    let (circ, s, turns, hand, a, g, z0) = (c.get("C")?, c.get("S")?, c.get("N")?, c.get("hand")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let r = circ / (2.0 * PI);
    let hs = 2.0 * g;
    let mut pts = vec![[r, 0.0, 0.0]];
    pts.extend(helix_points(r, s, turns.max(0.25), hs, hand.signum()));
    let top = hs + s * turns;
    Ok(Geometry {
        solids: vec![wire("helix", c.metal, pts, a)],
        ports: vec![port("feed", [r, 0.0, 0.0], [r, 0.0, g], 0.0, z0)],
        ground: true,
        axis_forward: true,
        hints: vec![hint([-r, -r, 0.0], [r, r, top], circ / 40.0)],
        ..Default::default()
    })
}

fn helix_normal_params() -> Vec<Param> {
    let mut v = base("868MHz");
    v.extend([
        p("D", "0.035*lambda0", "Coil diameter"),
        p("S", "0.07*lambda0", "Turn spacing"),
        p("N", "2", "Turns"),
        p("hand", "1", "Winding direction"),
        p("a", "0.001*lambda0", "Wire radius"),
        p("g", "lambda0/120", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("Z0", "20", "Port impedance"),
    ]);
    v
}
fn helix_normal(c: &Ctx) -> Result<Geometry> {
    let (dia, s, turns, hand, a, g, z0) = (c.get("D")?, c.get("S")?, c.get("N")?, c.get("hand")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let r = dia / 2.0;
    let hs = 2.0 * g;
    let mut pts = vec![[r, 0.0, 0.0]];
    pts.extend(helix_points(r, s, turns.max(0.25), hs, hand.signum()));
    let top = hs + s * turns;
    Ok(Geometry {
        solids: vec![wire("helix", c.metal, pts, a)],
        ports: vec![port("feed", [r, 0.0, 0.0], [r, 0.0, g], 0.0, z0)],
        ground: true,
        hints: vec![hint([-r, -r, 0.0], [r, r, top], (dia / 8.0).min(s / 3.0))],
        ..Default::default()
    })
}

fn turnstile_params() -> Vec<Param> {
    let mut v = base("437MHz");
    v.extend([p("L", "0.47*lambda0", "Length of each dipole"), p("a", "0.002*lambda0", "Wire radius"), p("dz", "lambda0/60", "Height offset between the dipoles"), p("g", "lambda0/100", "Feed gap (FDTD; wire MoM feeds one point)"), p("Z0", "73", "Port impedance")]);
    v
}
fn turnstile(c: &Ctx) -> Result<Geometry> {
    let (l, a, dz, g, z0) = (c.get("L")?, c.get("a")?, c.get("dz")?, c.get("g")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            wire("dipole x", c.metal, vec![[-l / 2.0, 0.0, 0.0], [l / 2.0, 0.0, 0.0]], a),
            wire("dipole y", c.metal, vec![[0.0, -l / 2.0, dz], [0.0, l / 2.0, dz]], a),
        ],
        ports: vec![
            port("x", [-g / 2.0, 0.0, 0.0], [g / 2.0, 0.0, 0.0], 0.0, z0),
            port("y", [0.0, -g / 2.0, dz], [0.0, g / 2.0, dz], 0.0, z0),
        ],
        hints: vec![hint([-l / 2.0, -l / 2.0, 0.0], [l / 2.0, l / 2.0, dz], l / 40.0)],
        ..Default::default()
    })
}

fn discone_params() -> Vec<Param> {
    let mut v = base("1.2GHz");
    v.extend([
        p("f_lo", "0.6*f0", "Lowest frequency"),
        p("Lc", "c0/(4*f_lo)", "Cone slant length"),
        p("cone", "30", "Cone half angle, degrees"),
        p("Dd", "0.7*2*Lc*sin(cone*pi/180)", "Disc diameter"),
        p("g", "lambda0/60", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("spokes", "16", "Wires per disc and cone"),
        p("a", "0.002*lambda0", "Wire radius"),
        p("Z0", "75", "Port impedance"),
    ]);
    v
}
fn discone(c: &Ctx) -> Result<Geometry> {
    let (lc, cone, dd, g, a, z0) = (c.get("Lc")?, c.get("cone")?, c.get("Dd")?, c.get("g")?, c.get("a")?, c.get("Z0")?);
    let n = c.count("spokes", 4, 48)?;
    let (sa, ca) = cone.to_radians().sin_cos();
    let mut solids = vec![wire("axis", c.metal, vec![[0.0, 0.0, -g], [0.0, 0.0, 0.0]], a)];
    for k in 0..n {
        let t = 2.0 * PI * k as f64 / n as f64;
        let (st, ct) = t.sin_cos();
        solids.push(wire(&format!("disc {}", k + 1), c.metal, vec![[0.0, 0.0, 0.0], [dd / 2.0 * ct, dd / 2.0 * st, 0.0]], a));
        solids.push(wire(&format!("cone {}", k + 1), c.metal, vec![[0.0, 0.0, -g], [lc * sa * ct, lc * sa * st, -g - lc * ca]], a));
    }
    Ok(Geometry { solids, ports: vec![port("feed", [0.0, 0.0, -g], [0.0, 0.0, 0.0], 0.0, z0)], hints: vec![hint([0.0, 0.0, -g], [0.0, 0.0, 0.0], g / 2.0)], ..Default::default() })
}

fn meander_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("h0", "0.02*lambda0", "Straight section above ground"),
        p("W", "0.047*lambda0", "Meander width"),
        p("s", "0.03*lambda0", "Meander pitch"),
        p("N", "4", "Number of folds"),
        p("a", "0.001*lambda0", "Wire radius"),
        p("g", "lambda0/120", "Feed gap (FDTD; wire MoM feeds one point)"),
        p("Z0", "15", "Port impedance"),
    ]);
    v
}
fn meander(c: &Ctx) -> Result<Geometry> {
    let (h0, w, s, a, g, z0) = (c.get("h0")?, c.get("W")?, c.get("s")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let n = c.count("N", 1, 40)?;
    let mut pts = vec![[0.0, 0.0, 0.0], [0.0, 0.0, h0]];
    let mut z = h0;
    for k in 0..n {
        let x = if k % 2 == 0 { w } else { 0.0 };
        pts.push([x, 0.0, z]);
        z += s;
        pts.push([x, 0.0, z]);
    }
    Ok(Geometry {
        solids: vec![wire("meander", c.metal, pts, a)],
        ports: vec![port("feed", [0.0, 0.0, 0.0], [0.0, 0.0, g], 0.0, z0)],
        ground: true,
        hints: vec![hint([0.0, 0.0, 0.0], [w, 0.0, z], s / 3.0)],
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Metal sheets

fn strip_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([p("L", "0.46*lambda0", "Total length"), p("w", "lambda0/50", "Strip width"), p("g", "lambda0/60", "Feed gap"), p("Z0", "73", "Port impedance")]);
    v
}
fn strip_dipole(c: &Ctx) -> Result<Geometry> {
    let (l, w, g, z0) = (c.get("L")?, c.get("w")?, c.get("g")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            bx("upper arm", c.metal, [-w / 2.0, 0.0, g / 2.0], [w / 2.0, 0.0, l / 2.0]),
            bx("lower arm", c.metal, [-w / 2.0, 0.0, -l / 2.0], [w / 2.0, 0.0, -g / 2.0]),
        ],
        ports: vec![port("feed", [0.0, 0.0, -g / 2.0], [0.0, 0.0, g / 2.0], w, z0)],
        hints: vec![hint([-w / 2.0, 0.0, -l / 2.0], [w / 2.0, 0.0, l / 2.0], l / 40.0)],
        ..Default::default()
    })
}

fn bowtie_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("L", "0.33*lambda0", "Tip to tip length"),
        p("W", "0.2*lambda0", "Width at the ends"),
        p("wf", "lambda0/60", "Width at the feed"),
        p("g", "lambda0/60", "Feed gap"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn bowtie(c: &Ctx) -> Result<Geometry> {
    let (l, w, wf, g, z0) = (c.get("L")?, c.get("W")?, c.get("wf")?, c.get("g")?, c.get("Z0")?);
    // Plane normal to y: points are (z, x).
    let arm = |s: f64| vec![[s * g / 2.0, -wf / 2.0], [s * g / 2.0, wf / 2.0], [s * l / 2.0, w / 2.0], [s * l / 2.0, -w / 2.0]];
    Ok(Geometry {
        solids: vec![poly("upper arm", c.metal, 1, 0.0, arm(1.0)), poly("lower arm", c.metal, 1, 0.0, arm(-1.0))],
        ports: vec![port("feed", [0.0, 0.0, -g / 2.0], [0.0, 0.0, g / 2.0], wf, z0)],
        hints: vec![hint([-wf / 2.0, 0.0, -g], [wf / 2.0, 0.0, g], g / 3.0)],
        ..Default::default()
    })
}

fn patch_air_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("h", "0.025*lambda0", "Height above the ground plate"),
        p("L", "0.46*lambda0", "Patch length (resonant, along y)"),
        p("W", "0.6*lambda0", "Patch width"),
        p("G", "1.1*lambda0", "Ground plate size"),
        p("yf", "0.22*L", "Probe offset from the centre"),
        p("wp", "lambda0/80", "Probe width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn patch_air(c: &Ctx) -> Result<Geometry> {
    let (h, l, w, gs, yf, wp, z0) = (c.get("h")?, c.get("L")?, c.get("W")?, c.get("G")?, c.get("yf")?, c.get("wp")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            bx("ground", c.metal, [-gs / 2.0, -gs / 2.0, 0.0], [gs / 2.0, gs / 2.0, 0.0]),
            bx("patch", c.metal, [-w / 2.0, -l / 2.0, h], [w / 2.0, l / 2.0, h]),
        ],
        ports: vec![port("probe", [0.0, yf, 0.0], [0.0, yf, h], wp, z0)],
        axis_forward: true,
        hints: vec![hint([-w / 2.0, -l / 2.0, 0.0], [w / 2.0, l / 2.0, h], l / 40.0)],
        ..Default::default()
    })
}

fn pifa_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("L", "0.207*lambda0", "Plate length from the short"),
        p("W", "0.1*lambda0", "Plate width"),
        p("h", "0.05*lambda0", "Height"),
        p("xf", "0.035*lambda0", "Feed distance from the short"),
        p("Gx", "0.6*lambda0", "Ground length"),
        p("Gy", "0.4*lambda0", "Ground width"),
        p("wp", "lambda0/80", "Feed width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn pifa(c: &Ctx) -> Result<Geometry> {
    let (l, w, h, xf, gx, gy, wp, z0) = (c.get("L")?, c.get("W")?, c.get("h")?, c.get("xf")?, c.get("Gx")?, c.get("Gy")?, c.get("wp")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            bx("ground", c.metal, [-gx / 4.0, -gy / 2.0, 0.0], [3.0 * gx / 4.0, gy / 2.0, 0.0]),
            bx("top plate", c.metal, [0.0, -w / 2.0, h], [l, w / 2.0, h]),
            bx("short", c.metal, [0.0, -w / 2.0, 0.0], [0.0, w / 2.0, h]),
        ],
        ports: vec![port("feed", [xf, 0.0, 0.0], [xf, 0.0, h], wp, z0)],
        hints: vec![hint([0.0, -w / 2.0, 0.0], [l, w / 2.0, h], (h / 4.0).min(l / 30.0))],
        ..Default::default()
    })
}

fn slot_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("Ls", "0.44*lambda0", "Slot length (along y)"),
        p("ws", "lambda0/40", "Slot width"),
        p("Gx", "0.8*lambda0", "Plate size along x"),
        p("Gy", "1.0*lambda0", "Plate size along y"),
        p("wp", "lambda0/60", "Feed width"),
        p("Z0", "500", "Port impedance"),
    ]);
    v
}
fn slot(c: &Ctx) -> Result<Geometry> {
    let (ls, ws, gx, gy, wp, z0) = (c.get("Ls")?, c.get("ws")?, c.get("Gx")?, c.get("Gy")?, c.get("wp")?, c.get("Z0")?);
    let (x, y, sx, sy) = (gx / 2.0, gy / 2.0, ws / 2.0, ls / 2.0);
    // Outer rectangle, then the slot cut through a zero-width keyhole.
    let pts = vec![[-x, -y], [x, -y], [x, y], [-x, y], [-x, -y], [-sx, -sy], [-sx, sy], [sx, sy], [sx, -sy], [-sx, -sy]];
    Ok(Geometry {
        solids: vec![poly("plate", c.metal, 2, 0.0, pts)],
        ports: vec![port("feed", [-sx, 0.0, 0.0], [sx, 0.0, 0.0], wp, z0)],
        hints: vec![hint([-sx, -sy, 0.0], [sx, sy, 0.0], ws / 3.0)],
        ..Default::default()
    })
}

fn corner_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("s", "0.4*lambda0", "Dipole distance from the apex"),
        p("P", "1.0*lambda0", "Plate width"),
        p("H", "0.8*lambda0", "Plate height"),
        p("L", "0.46*lambda0", "Dipole length"),
        p("w", "lambda0/50", "Strip width"),
        p("g", "lambda0/60", "Feed gap"),
        p("Z0", "100", "Port impedance"),
    ]);
    v
}
fn corner(c: &Ctx) -> Result<Geometry> {
    let (s, pl, h, l, w, g, z0) = (c.get("s")?, c.get("P")?, c.get("H")?, c.get("L")?, c.get("w")?, c.get("g")?, c.get("Z0")?);
    let q = s * std::f64::consts::FRAC_1_SQRT_2;
    Ok(Geometry {
        solids: vec![
            bx("plate x", c.metal, [0.0, 0.0, -h / 2.0], [pl, 0.0, h / 2.0]),
            bx("plate y", c.metal, [0.0, 0.0, -h / 2.0], [0.0, pl, h / 2.0]),
            bx("upper arm", c.metal, [q - w / 2.0, q, g / 2.0], [q + w / 2.0, q, l / 2.0]),
            bx("lower arm", c.metal, [q - w / 2.0, q, -l / 2.0], [q + w / 2.0, q, -g / 2.0]),
        ],
        ports: vec![port("feed", [q, q, -g / 2.0], [q, q, g / 2.0], w, z0)],
        hints: vec![hint([q - w / 2.0, q, -l / 2.0], [q + w / 2.0, q, l / 2.0], l / 30.0)],
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Apertures

fn horn_params() -> Vec<Param> {
    let mut v = base("10GHz");
    v.extend([
        p("a", "0.71*lambda0", "Waveguide width (x)"),
        p("b", "0.35*lambda0", "Waveguide height (y)"),
        p("Lw", "0.8*lambda0", "Waveguide length"),
        p("A", "2.4*lambda0", "Aperture width"),
        p("B", "1.8*lambda0", "Aperture height"),
        p("Lf", "2.2*lambda0", "Flare length"),
        p("zp", "0.16*lambda0", "Probe distance from the back wall"),
        p("hp", "0.25*lambda0", "Probe length"),
        p("wp", "lambda0/30", "Probe width"),
        p("t", "lambda0/60", "Wall thickness in FDTD"),
        p("Z0", "75", "Port impedance"),
    ]);
    v
}
fn waveguide_walls(c: &Ctx, a: f64, b: f64, lw: f64, zp: f64, hp: f64, wp: f64, z0: f64, lambda: f64) -> (Vec<Solid>, PortDef) {
    let (x, y) = (a / 2.0, b / 2.0);
    let g = lambda / 60.0;
    let solids = vec![
        bx("wall x-", c.metal, [-x, -y, 0.0], [-x, y, lw]),
        bx("wall x+", c.metal, [x, -y, 0.0], [x, y, lw]),
        bx("wall y-", c.metal, [-x, -y, 0.0], [x, -y, lw]),
        bx("wall y+", c.metal, [-x, y, 0.0], [x, y, lw]),
        bx("back wall", c.metal, [-x, -y, 0.0], [x, y, 0.0]),
        bx("probe", c.metal, [-wp / 2.0, -y + g, zp], [wp / 2.0, -y + hp, zp]),
    ];
    (solids, port("probe", [0.0, -y, zp], [0.0, -y + g, zp], wp, z0))
}
fn horn(c: &Ctx) -> Result<Geometry> {
    let (a, b, lw, aw, bw, lf, zp, hp, wp, t, z0) = (c.get("a")?, c.get("b")?, c.get("Lw")?, c.get("A")?, c.get("B")?, c.get("Lf")?, c.get("zp")?, c.get("hp")?, c.get("wp")?, c.get("t")?, c.get("Z0")?);
    let lambda = c.get("lambda0")?;
    let (mut solids, feed) = waveguide_walls(c, a, b, lw, zp, hp, wp, z0, lambda);
    let (x, y, xa, ya, z2) = (a / 2.0, b / 2.0, aw / 2.0, bw / 2.0, lw + lf);
    let corner = |sx: f64, sy: f64, ap: bool| if ap { [sx * xa, sy * ya, z2] } else { [sx * x, sy * y, lw] };
    for (name, s1, s2) in [("flare y-", (-1.0, -1.0), (1.0, -1.0)), ("flare x+", (1.0, -1.0), (1.0, 1.0)), ("flare y+", (1.0, 1.0), (-1.0, 1.0)), ("flare x-", (-1.0, 1.0), (-1.0, -1.0))] {
        solids.push(quad(name, c.metal, [corner(s1.0, s1.1, false), corner(s2.0, s2.1, false), corner(s2.0, s2.1, true), corner(s1.0, s1.1, true)], t));
    }
    Ok(Geometry { solids, ports: vec![feed], axis_forward: true, hints: vec![hint([-wp / 2.0, -y, zp], [wp / 2.0, -y + hp, zp], wp / 3.0)], ..Default::default() })
}

fn oewg_params() -> Vec<Param> {
    let mut v = base("10GHz");
    v.extend([
        p("a", "0.71*lambda0", "Waveguide width (x)"),
        p("b", "0.35*lambda0", "Waveguide height (y)"),
        p("Lw", "1.0*lambda0", "Waveguide length"),
        p("zp", "0.16*lambda0", "Probe distance from the back wall"),
        p("hp", "0.25*lambda0", "Probe length"),
        p("wp", "lambda0/30", "Probe width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn oewg(c: &Ctx) -> Result<Geometry> {
    let (a, b, lw, zp, hp, wp, z0) = (c.get("a")?, c.get("b")?, c.get("Lw")?, c.get("zp")?, c.get("hp")?, c.get("wp")?, c.get("Z0")?);
    let (solids, feed) = waveguide_walls(c, a, b, lw, zp, hp, wp, z0, c.get("lambda0")?);
    Ok(Geometry { solids, ports: vec![feed], axis_forward: true, hints: vec![hint([-wp / 2.0, -b / 2.0, zp], [wp / 2.0, -b / 2.0 + hp, zp], wp / 3.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// Printed antennas

/// Rows for the transmission-line model of a rectangular patch.
/// `shorten` trims the textbook length: full-wave runs of probe-fed patches resonate
/// about 2.5 % below what the transmission-line model predicts.
fn tl_patch_rows(shorten: &str) -> Vec<Param> {
    vec![
        p("h", "1.6mm", "Substrate thickness"),
        p("W", "c0/(2*f0)*sqrt(2/(er+1))", "Patch width"),
        d("eeff", "(er+1)/2+(er-1)/2/sqrt(1+12*h/W)", "Effective permittivity"),
        d("dL", "0.412*h*(eeff+0.3)*(W/h+0.264)/((eeff-0.258)*(W/h+0.8))", "Fringing length extension"),
        p("L", &format!("{shorten}*(c0/(2*f0*sqrt(eeff))-2*dL)"), "Patch length (resonant, along y)"),
    ]
}

fn patch_probe_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend(tl_patch_rows("0.975"));
    v.extend([
        p("Gx", "max(W+6*h,1.3*W)", "Ground size along x"),
        p("Gy", "max(L+6*h,1.3*L)", "Ground size along y"),
        p("yf", "0.25*L", "Probe offset from the centre"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn patch_probe(c: &Ctx) -> Result<Geometry> {
    let (h, w, l, gx, gy, yf, z0) = (c.get("h")?, c.get("W")?, c.get("L")?, c.get("Gx")?, c.get("Gy")?, c.get("yf")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            bx("substrate", c.substrate, [-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, h]),
            bx("ground", c.metal, [-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, 0.0]),
            bx("patch", c.metal, [-w / 2.0, -l / 2.0, h], [w / 2.0, l / 2.0, h]),
        ],
        ports: vec![port("probe", [0.0, yf, 0.0], [0.0, yf, h], h / 2.0, z0)],
        axis_forward: true,
        hints: vec![hint([-w / 2.0, -l / 2.0, 0.0], [w / 2.0, l / 2.0, h], l.min(w) / 40.0)],
        ..Default::default()
    })
}

/// Rows for a microstrip line width (Hammerstad) from `er`, `h`, `Z0`.
fn msl_rows() -> Vec<Param> {
    vec![
        d("msA", "Z0/60*sqrt((er+1)/2)+(er-1)/(er+1)*(0.23+0.11/er)", "Hammerstad A"),
        d("msB", "377*pi/(2*Z0*sqrt(er))", "Hammerstad B"),
        p("wf", "h*if(msA>1.52, 8*exp(msA)/(exp(2*msA)-2), 2/pi*(msB-1-ln(2*msB-1)+(er-1)/(2*er)*(ln(msB-1)+0.39-0.61/er)))", "50 Ω line width"),
    ]
}

fn patch_inset_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend(tl_patch_rows("1"));
    v.push(p("Z0", "50", "Port impedance"));
    v.extend(msl_rows());
    v.extend([
        p("y0", "0.25*L", "Inset depth"),
        p("gs", "wf/2", "Gap beside the feed in the notch"),
        p("Gx", "W+6*h+lambda0/4", "Ground size along x"),
        p("Lfeed", "lambda0/8", "Feed line length outside the patch"),
        p("Gyb", "lambda0/16", "Ground margin beyond the far edge"),
    ]);
    v
}
fn patch_inset(c: &Ctx) -> Result<Geometry> {
    let (h, w, l, z0, wf, y0, gs, gx, lfeed, gyb) = (c.get("h")?, c.get("W")?, c.get("L")?, c.get("Z0")?, c.get("wf")?, c.get("y0")?, c.get("gs")?, c.get("Gx")?, c.get("Lfeed")?, c.get("Gyb")?);
    let (x, y) = (w / 2.0, l / 2.0);
    let n = wf / 2.0 + gs;
    let ye = -y - lfeed;
    let top = y + gyb;
    let outline = vec![[-x, -y], [-n, -y], [-n, -y + y0], [n, -y + y0], [n, -y], [x, -y], [x, y], [-x, y]];
    Ok(Geometry {
        solids: vec![
            bx("substrate", c.substrate, [-gx / 2.0, ye, 0.0], [gx / 2.0, top, h]),
            bx("ground", c.metal, [-gx / 2.0, ye, 0.0], [gx / 2.0, top, 0.0]),
            poly("patch", c.metal, 2, h, outline),
            bx("feed line", c.metal, [-wf / 2.0, ye, h], [wf / 2.0, -y + y0, h]),
        ],
        ports: vec![port("feed", [0.0, ye, 0.0], [0.0, ye, h], wf, z0)],
        axis_forward: true,
        hints: vec![hint([-x, -y, 0.0], [x, y, h], l.min(w) / 40.0), hint([-n, -y, 0.0], [n, -y + y0, 0.0], (wf / 3.0).min(l / 40.0))],
        ..Default::default()
    })
}

fn patch_circular_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("h", "1.6mm", "Substrate thickness"),
        d("F", "8.791e7/(f0*sqrt(er))", "Uncorrected radius"),
        p("R", "0.98*F/sqrt(1+2*h/(pi*er*F)*(ln(pi*F/(2*h))+1.7726))", "Patch radius, 2 % under the textbook value as full-wave runs show"),
        p("G", "2*R+6*h+lambda0/8", "Ground size"),
        p("xf", "R/3", "Probe offset from the centre"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn patch_circular(c: &Ctx) -> Result<Geometry> {
    let (h, r, gs, xf, z0) = (c.get("h")?, c.get("R")?, c.get("G")?, c.get("xf")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            bx("substrate", c.substrate, [-gs / 2.0, -gs / 2.0, 0.0], [gs / 2.0, gs / 2.0, h]),
            bx("ground", c.metal, [-gs / 2.0, -gs / 2.0, 0.0], [gs / 2.0, gs / 2.0, 0.0]),
            poly("patch", c.metal, 2, h, circle(0.0, 0.0, r, 48)),
        ],
        ports: vec![port("probe", [xf, 0.0, 0.0], [xf, 0.0, h], h / 2.0, z0)],
        axis_forward: true,
        hints: vec![hint([-r, -r, 0.0], [r, r, h], r / 20.0)],
        ..Default::default()
    })
}

fn ifa_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("h", "1.6mm", "Board thickness"),
        d("eeff", "1+0.15*(er-1)", "Effective permittivity (trace on a thin board, mostly air)"),
        p("H", "0.05*lambda0", "Height above the ground edge"),
        p("La", "lambda0/(4*sqrt(eeff))-H", "Arm length"),
        p("S", "H/2", "Feed distance from the short"),
        p("wt", "lambda0/100", "Trace width"),
        p("gap", "lambda0/120", "Feed gap"),
        p("Bx", "La+0.2*lambda0", "Board length"),
        p("Gy", "0.35*lambda0", "Ground depth"),
        p("x0", "0.1*lambda0", "Short position along the board"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn ifa(c: &Ctx) -> Result<Geometry> {
    let (h, hh, la, s, wt, gap, bxl, gy, x0, z0) = (c.get("h")?, c.get("H")?, c.get("La")?, c.get("S")?, c.get("wt")?, c.get("gap")?, c.get("Bx")?, c.get("Gy")?, c.get("x0")?, c.get("Z0")?);
    let top = hh + wt / 2.0;
    let air = c.substrate == "Air";
    let z = if air { 0.0 } else { h };
    let mut solids = Vec::new();
    if !air {
        solids.push(bx("board", c.substrate, [0.0, -gy, 0.0], [bxl, top + wt, h]));
    }
    solids.extend([
        bx("ground", c.metal, [0.0, -gy, z], [bxl, 0.0, z]),
        bx("arm", c.metal, [x0 - wt / 2.0, hh - wt / 2.0, z], [x0 + la, top, z]),
        bx("short", c.metal, [x0 - wt / 2.0, 0.0, z], [x0 + wt / 2.0, hh - wt / 2.0, z]),
        bx("feed stub", c.metal, [x0 + s - wt / 2.0, gap, z], [x0 + s + wt / 2.0, hh - wt / 2.0, z]),
    ]);
    Ok(Geometry {
        solids,
        ports: vec![port("feed", [x0 + s, 0.0, z], [x0 + s, gap, z], wt, z0)],
        hints: vec![hint([x0 - wt, 0.0, z], [x0 + la, top, z], wt / 3.0)],
        ..Default::default()
    })
}

fn uwb_params() -> Vec<Param> {
    let mut v = base("6.5GHz");
    v.extend([p("h", "1.6mm", "Substrate thickness"), p("Z0", "50", "Port impedance")]);
    v.extend(msl_rows());
    v.extend([
        p("R", "0.22*lambda0", "Disc radius"),
        p("Lg", "0.33*lambda0", "Ground and feed length"),
        p("gap", "0.02*lambda0", "Gap from ground to disc"),
        p("Wb", "0.75*lambda0", "Board width"),
        p("mg", "0.1*lambda0", "Board margin above the disc"),
    ]);
    v
}
fn uwb(c: &Ctx) -> Result<Geometry> {
    let (h, z0, wf, r, lg, gap, wb, mg) = (c.get("h")?, c.get("Z0")?, c.get("wf")?, c.get("R")?, c.get("Lg")?, c.get("gap")?, c.get("Wb")?, c.get("mg")?);
    let yc = gap + r;
    Ok(Geometry {
        solids: vec![
            bx("substrate", c.substrate, [-wb / 2.0, -lg, 0.0], [wb / 2.0, yc + r + mg, h]),
            bx("ground", c.metal, [-wb / 2.0, -lg, 0.0], [wb / 2.0, 0.0, 0.0]),
            bx("feed line", c.metal, [-wf / 2.0, -lg, h], [wf / 2.0, gap + r * 0.02, h]),
            poly("disc", c.metal, 2, h, circle(0.0, yc, r, 48)),
        ],
        ports: vec![port("feed", [0.0, -lg, 0.0], [0.0, -lg, h], wf, z0)],
        hints: vec![hint([-r, gap, 0.0], [r, yc + r, h], r / 16.0)],
        ..Default::default()
    })
}

fn vivaldi_params() -> Vec<Param> {
    let mut v = base("6GHz");
    v.extend([
        p("h", "0.8mm", "Substrate thickness"),
        p("Lt", "1.3*lambda0", "Taper length"),
        p("Wa", "1.0*lambda0", "Aperture width"),
        p("w0", "lambda0/60", "Slot width at the feed"),
        p("Ls", "lambda0/8", "Short-circuited slot behind the feed"),
        p("Lb", "lambda0/5", "Metal behind the slot"),
        p("Wb", "1.2*lambda0", "Board width"),
        p("Z0", "100", "Port impedance"),
    ]);
    v
}
fn vivaldi(c: &Ctx) -> Result<Geometry> {
    let (h, lt, wa, w0, ls, lb, wb, z0) = (c.get("h")?, c.get("Lt")?, c.get("Wa")?, c.get("w0")?, c.get("Ls")?, c.get("Lb")?, c.get("Wb")?, c.get("Z0")?);
    let rate = (wa / w0).ln() / lt;
    let half = |z: f64| w0 / 2.0 * (rate * z).exp();
    let n = 32;
    // Plane normal to x: points are (y, z). A single sheet with the slot cut in it.
    let mut pts: Vec<[f64; 2]> = Vec::new();
    pts.push([-wb / 2.0, lt]);
    for k in (0..=n).rev() {
        let z = lt * k as f64 / n as f64;
        pts.push([-half(z), z]);
    }
    pts.push([-w0 / 2.0, -ls]);
    pts.push([w0 / 2.0, -ls]);
    for k in 0..=n {
        let z = lt * k as f64 / n as f64;
        pts.push([half(z), z]);
    }
    pts.push([wb / 2.0, lt]);
    pts.push([wb / 2.0, -ls - lb]);
    pts.push([-wb / 2.0, -ls - lb]);
    Ok(Geometry {
        solids: vec![
            bx("substrate", c.substrate, [-h, -wb / 2.0, -ls - lb], [0.0, wb / 2.0, lt]),
            poly("metal", c.metal, 0, 0.0, pts),
        ],
        ports: vec![port("feed", [0.0, -w0 / 2.0, 0.0], [0.0, w0 / 2.0, 0.0], w0, z0)],
        axis_forward: true,
        hints: vec![hint([-h, -w0, -w0], [0.0, w0, w0], w0 / 3.0)],
        ..Default::default()
    })
}

fn mimo_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend(tl_patch_rows("0.975"));
    v.extend([
        p("d", "0.5*lambda0", "Centre spacing along x"),
        p("Gx", "d+W+lambda0/4", "Ground size along x"),
        p("Gy", "L+lambda0/4", "Ground size along y"),
        p("yf", "0.25*L", "Probe offset"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn mimo(c: &Ctx) -> Result<Geometry> {
    let (h, w, l, dd, gx, gy, yf, z0) = (c.get("h")?, c.get("W")?, c.get("L")?, c.get("d")?, c.get("Gx")?, c.get("Gy")?, c.get("yf")?, c.get("Z0")?);
    let patch = |name: &str, x: f64| bx(name, c.metal, [x - w / 2.0, -l / 2.0, h], [x + w / 2.0, l / 2.0, h]);
    Ok(Geometry {
        solids: vec![
            bx("substrate", c.substrate, [-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, h]),
            bx("ground", c.metal, [-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, 0.0]),
            patch("patch 1", -dd / 2.0),
            patch("patch 2", dd / 2.0),
        ],
        ports: vec![
            port("P1", [-dd / 2.0, yf, 0.0], [-dd / 2.0, yf, h], h / 2.0, z0),
            port("P2", [dd / 2.0, yf, 0.0], [dd / 2.0, yf, h], h / 2.0, z0),
        ],
        axis_forward: true,
        hints: vec![hint([-dd / 2.0 - w / 2.0, -l / 2.0, 0.0], [dd / 2.0 + w / 2.0, l / 2.0, h], l.min(w) / 30.0)],
        ..Default::default()
    })
}

fn dra_params() -> Vec<Param> {
    let mut v = base("5GHz");
    v.extend([
        p("Rd", "0.96*2.877/sqrt(er+2)/(2*pi/lambda0)", "Dielectric radius (HE11δ mode, aspect 1, trimmed from full-wave runs)"),
        p("Hd", "Rd", "Dielectric height"),
        p("lp", "1.8*Hd", "Probe length"),
        p("rp", "lambda0/200", "Probe radius"),
        p("g", "lambda0/100", "Feed gap"),
        p("G", "2*Rd+lambda0/2", "Ground plate size"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn dra(c: &Ctx) -> Result<Geometry> {
    let (r, hh, lp, rp, g, gs, z0) = (c.get("Rd")?, c.get("Hd")?, c.get("lp")?, c.get("rp")?, c.get("g")?, c.get("G")?, c.get("Z0")?);
    let xp = r + 2.0 * rp;
    Ok(Geometry {
        solids: vec![
            Solid { name: "resonator".into(), material: c.substrate.into(), shape: Shape::Cylinder { a: [0.0, 0.0, 0.0], b: [0.0, 0.0, hh], r } },
            bx("ground", c.metal, [-gs / 2.0, -gs / 2.0, 0.0], [gs / 2.0, gs / 2.0, 0.0]),
            wire("probe", c.metal, vec![[xp, 0.0, 0.0], [xp, 0.0, lp]], rp),
        ],
        ports: vec![port("feed", [xp, 0.0, 0.0], [xp, 0.0, g], 0.0, z0)],
        axis_forward: true,
        hints: vec![hint([-r, -r, 0.0], [xp, r, hh], r / 12.0)],
        ..Default::default()
    })
}

fn gate_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("N", "5", "Elements on each panel"),
        p("L", "0.44*lambda0", "Element length"),
        p("w", "lambda0/25", "Element strip width"),
        p("g", "lambda0/40", "Feed gap"),
        p("dz", "0.85*lambda0", "Height between element centres"),
        p("zc", "0.95", "Height of the middle element"),
        p("D", "lambda0/4", "Elements in front of the panel"),
        p("Pw", "1.4*lambda0", "Panel width"),
        p("Pm", "0.3*lambda0", "Panel beyond the top and bottom elements"),
        p("pol", "1", "0: all vertical; 1: vertical and horizontal in turn"),
        p("sides", "1", "Panels: 1, or 2 facing across the aisle"),
        p("A", "1", "Distance between the two panels"),
        p("floor", "0", "1: a metal floor at z = 0; 0: no floor"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn gate(c: &Ctx) -> Result<Geometry> {
    let n = c.count("N", 1, 8)?;
    let (l, w, g, dz, zc, d, pw, pm, a, z0) = (c.get("L")?, c.get("w")?, c.get("g")?, c.get("dz")?, c.get("zc")?, c.get("D")?, c.get("Pw")?, c.get("Pm")?, c.get("A")?, c.get("Z0")?);
    let (alternate, two, floor) = (c.get("pol")? >= 0.5, c.get("sides")? >= 1.5, c.get("floor")? >= 0.5);
    let height = |k: usize| zc + (k as f64 - (n as f64 - 1.0) / 2.0) * dz;
    let (bottom, top) = (height(0) - l / 2.0 - pm, height(n - 1) + l / 2.0 + pm);
    if floor && bottom < 0.0 {
        bail!("With a floor everything must stay above z = 0: raise zc or use fewer elements.");
    }
    let mut solids = Vec::new();
    let mut ports = Vec::new();
    let mut hints = Vec::new();
    for side in 0..if two { 2 } else { 1 } {
        // The second panel looks back across the aisle.
        let (x_panel, x_el, tag) = if side == 0 { (0.0, d, "") } else { (a, a - d, "B ") };
        solids.push(bx(&format!("{tag}panel"), c.metal, [x_panel, -pw / 2.0, bottom], [x_panel, pw / 2.0, top]));
        for k in 0..n {
            let z = height(k);
            let name = format!("{tag}E{}", k + 1);
            if alternate && k % 2 == 1 {
                solids.push(bx(&format!("{name} left"), c.metal, [x_el, -l / 2.0, z - w / 2.0], [x_el, -g / 2.0, z + w / 2.0]));
                solids.push(bx(&format!("{name} right"), c.metal, [x_el, g / 2.0, z - w / 2.0], [x_el, l / 2.0, z + w / 2.0]));
                ports.push(port(&name, [x_el, -g / 2.0, z], [x_el, g / 2.0, z], w, z0));
                hints.push(hint([x_el, -l / 2.0, z - w / 2.0], [x_el, l / 2.0, z + w / 2.0], l / 40.0));
            } else {
                solids.push(bx(&format!("{name} lower"), c.metal, [x_el, -w / 2.0, z - l / 2.0], [x_el, w / 2.0, z - g / 2.0]));
                solids.push(bx(&format!("{name} upper"), c.metal, [x_el, -w / 2.0, z + g / 2.0], [x_el, w / 2.0, z + l / 2.0]));
                ports.push(port(&name, [x_el, 0.0, z - g / 2.0], [x_el, 0.0, z + g / 2.0], w, z0));
                hints.push(hint([x_el, -w / 2.0, z - l / 2.0], [x_el, w / 2.0, z + l / 2.0], l / 40.0));
            }
        }
    }
    Ok(Geometry { solids, ports, ground: floor, hints, ..Default::default() })
}

// ---------------------------------------------------------------------------
// Reconfigurable antennas. A part's gap is a port; `reconfig::for_template` says
// which port carries which part, and what each state does.

fn reconfig_dipole_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("L1", "0.44*lambda0", "Length with every switch off"),
        p("L2", "1.38*L1", "Length with the inner switches on"),
        p("L3", "1.85*L1", "Length with every switch on"),
        p("a", "0.001*lambda0", "Wire radius"),
        p("g", "lambda0/120", "Feed and switch gaps"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn reconfig_dipole(c: &Ctx) -> Result<Geometry> {
    let (l1, l2, l3, a, g, z0) = (c.get("L1")?, c.get("L2")?, c.get("L3")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    if !(l1 < l2 && l2 < l3) {
        bail!("The lengths have to grow: L1 < L2 < L3.");
    }
    let gap = |name: &str, z: f64| port(name, [0.0, 0.0, z - g / 2.0], [0.0, 0.0, z + g / 2.0], 0.0, z0);
    Ok(Geometry {
        solids: vec![wire("dipole", c.metal, vec![[0.0, 0.0, -l3 / 2.0], [0.0, 0.0, l3 / 2.0]], a)],
        ports: vec![gap("feed", 0.0), gap("D1 up", l1 / 2.0), gap("D1 down", -l1 / 2.0), gap("D2 up", l2 / 2.0), gap("D2 down", -l2 / 2.0)],
        hints: vec![hint([0.0, 0.0, -l3 / 2.0], [0.0, 0.0, l3 / 2.0], l3 / 60.0)],
        ..Default::default()
    })
}

fn reconfig_pifa_params() -> Vec<Param> {
    let mut v = base("900MHz");
    v.extend([
        p("L", "0.14*lambda0", "Plate length from the short"),
        p("W", "0.1*lambda0", "Plate width"),
        p("h", "0.05*lambda0", "Height"),
        p("xf", "0.03*lambda0", "Feed distance from the short"),
        p("Gx", "0.6*lambda0", "Ground length"),
        p("Gy", "0.4*lambda0", "Ground width"),
        p("wp", "lambda0/80", "Feed and tuner width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn reconfig_pifa(c: &Ctx) -> Result<Geometry> {
    let (l, w, h, xf, gx, gy, wp, z0) = (c.get("L")?, c.get("W")?, c.get("h")?, c.get("xf")?, c.get("Gx")?, c.get("Gy")?, c.get("wp")?, c.get("Z0")?);
    Ok(Geometry {
        solids: vec![
            bx("ground", c.metal, [-gx / 4.0, -gy / 2.0, 0.0], [3.0 * gx / 4.0, gy / 2.0, 0.0]),
            bx("top plate", c.metal, [0.0, -w / 2.0, h], [l, w / 2.0, h]),
            bx("short", c.metal, [0.0, -w / 2.0, 0.0], [0.0, w / 2.0, h]),
        ],
        ports: vec![port("feed", [xf, 0.0, 0.0], [xf, 0.0, h], wp, z0), port("tuner", [l, 0.0, 0.0], [l, 0.0, h], wp, z0)],
        hints: vec![hint([0.0, -w / 2.0, 0.0], [l, w / 2.0, h], (h / 4.0).min(l / 30.0))],
        ..Default::default()
    })
}

fn reconfig_parasitic_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("h", "0.235*lambda0", "Fed monopole's height"),
        p("hp", "0.26*lambda0", "Parasitics' height"),
        p("s", "0.3*lambda0", "Parasitics' distance from the centre"),
        p("a", "0.002*lambda0", "Wire radius"),
        p("g", "lambda0/100", "Feed and switch gaps"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn reconfig_parasitic(c: &Ctx) -> Result<Geometry> {
    let (h, hp, s, a, g, z0) = (c.get("h")?, c.get("hp")?, c.get("s")?, c.get("a")?, c.get("g")?, c.get("Z0")?);
    let mut solids = vec![wire("fed", c.metal, vec![[0.0, 0.0, 0.0], [0.0, 0.0, h]], a)];
    let mut ports = vec![port("feed", [0.0, 0.0, 0.0], [0.0, 0.0, g], 0.0, z0)];
    for (name, x, y) in [("+x", s, 0.0), ("+y", 0.0, s), ("-x", -s, 0.0), ("-y", 0.0, -s)] {
        solids.push(wire(&format!("parasitic {name}"), c.metal, vec![[x, y, 0.0], [x, y, hp]], a));
        ports.push(port(&format!("D {name}"), [x, y, 0.0], [x, y, g], 0.0, z0));
    }
    Ok(Geometry { solids, ports, ground: true, hints: vec![hint([-s, -s, 0.0], [s, s, hp], hp / 20.0)], ..Default::default() })
}

fn reconfig_pol_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([
        p("h", "0.025*lambda0", "Height above the ground plane"),
        p("L", "0.45*lambda0", "Patch side"),
        p("t", "0.062*lambda0", "Corner tab size"),
        p("g", "lambda0/100", "Gap around a tab"),
        p("xf", "0.11*lambda0", "Probe offset from the centre"),
        p("wp", "lambda0/80", "Probe width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}
fn reconfig_pol(c: &Ctx) -> Result<Geometry> {
    let (h, l, t, g, xf, wp, z0) = (c.get("h")?, c.get("L")?, c.get("t")?, c.get("g")?, c.get("xf")?, c.get("wp")?, c.get("Z0")?);
    let s = l / 2.0;
    if !(g > 0.0 && g < t && t < s) {
        bail!("A corner tab has to be wider than its gap and smaller than half the patch.");
    }
    let n = s - t;
    let patch = vec![[-n, -s], [n, -s], [n, -n], [s, -n], [s, n], [n, n], [n, s], [-n, s], [-n, n], [-s, n], [-s, -n], [-n, -n]];
    let mut solids = vec![poly("patch", c.metal, 2, h, patch)];
    let mut ports = vec![port("probe", [xf, 0.0, 0.0], [xf, 0.0, h], wp, z0)];
    // A tab sits in each corner's notch, joined across its gap along x by a part.
    let side = t - g;
    for (name, sx, sy) in [("A1", 1.0, 1.0), ("A2", -1.0, -1.0), ("B1", 1.0, -1.0), ("B2", -1.0, 1.0)] {
        let (x0, y0) = (sx * (n + g), sy * (n + g));
        solids.push(bx(&format!("tab {name}"), c.metal, [x0, y0, h], [sx * s, sy * s, h]));
        let y = sy * (s - side / 2.0);
        ports.push(port(name, [sx * n, y, h], [x0, y, h], 0.8 * side, z0));
    }
    Ok(Geometry { solids, ports, ground: true, axis_forward: true, hints: vec![hint([-s, -s, 0.0], [s, s, h], g)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// UHF RFID, as the real things are built

fn rfid_tag_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Lx", "0.29*lambda0", "Overall length, tip to tip"),
        p("w", "lambda0/330", "Trace width"),
        p("a", "0.04*lambda0", "T-match width: mostly sets the inductance the chip needs"),
        p("b", "0.012*lambda0", "T-match height, from the dipole to the chip"),
        p("g", "lambda0/330", "Chip gap"),
        p("n", "2", "Meander periods on each side, for fitting a short label"),
        p("Hm", "0.008*lambda0", "Meander height either side of the axis"),
        p("s", "0.008*lambda0", "Meander step"),
        p("x0", "0.035*lambda0", "Where the meander starts, from the centre"),
        p("Z0", "50", "Port impedance. It doesn't matter here: a tag is judged against its chip"),
    ]);
    v
}

/// The two sides of a path of right angles, offset by half a width.
fn ortho_sides(path: &[[f64; 2]], w: f64) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
    let dir = |a: [f64; 2], b: [f64; 2]| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let n = (dx * dx + dy * dy).sqrt().max(1e-15);
        [dx / n, dy / n]
    };
    let h = w / 2.0;
    let (mut left, mut right) = (Vec::with_capacity(path.len()), Vec::with_capacity(path.len()));
    for (i, q) in path.iter().enumerate() {
        let d1 = if i > 0 { dir(path[i - 1], *q) } else { dir(path[0], path[1]) };
        let d2 = if i + 1 < path.len() { dir(*q, path[i + 1]) } else { d1 };
        let (n1, n2) = ([-d1[1], d1[0]], [-d2[1], d2[0]]);
        // Straight on: one normal. Around a right angle: the corner of the two.
        let off = if d1[0] * d2[0] + d1[1] * d2[1] > 0.5 { [n1[0] * h, n1[1] * h] } else { [(n1[0] + n2[0]) * h, (n1[1] + n2[1]) * h] };
        left.push([q[0] + off[0], q[1] + off[1]]);
        right.push([q[0] - off[0], q[1] - off[1]]);
    }
    (left, right)
}

/// A meandered dipole with a T-match, the usual UHF inlay. The chip sits in a gap in the T's bar,
/// and the T makes the antenna inductive, as a capacitive chip needs (NXP AN1715). One sheet: the
/// gap keeps its outline a single polygon.
fn rfid_tag(c: &Ctx) -> Result<Geometry> {
    let (lx, w, a, b, g, hm, s, x0, z0) = (c.get("Lx")?, c.get("w")?, c.get("a")?, c.get("b")?, c.get("g")?, c.get("Hm")?, c.get("s")?, c.get("x0")?, c.get("Z0")?);
    let n = c.count("n", 0, 12)?;
    if !(w > 0.0 && g > 0.0 && g < a - 2.0 * w && b > 2.0 * w) {
        bail!("The T-match needs room: gap and trace width have to fit inside its width and height.");
    }
    if n > 0 && !(x0 > a / 2.0 + w && hm > w && s > 2.0 * w) {
        bail!("The meander has to start outside the T-match, with its steps wider than two traces.");
    }
    let mut arm = vec![[a / 2.0, 0.0]];
    let mut x = if n > 0 { x0 } else { a / 2.0 };
    if n > 0 {
        arm.push([x, 0.0]);
    }
    for _ in 0..n {
        arm.push([x, hm]);
        x += s;
        arm.push([x, hm]);
        arm.push([x, -hm]);
        x += s;
        arm.push([x, -hm]);
        arm.push([x, 0.0]);
    }
    if lx / 2.0 <= x + w {
        bail!("The meanders don't fit in this length: shorten them or lengthen Lx.");
    }
    arm.push([lx / 2.0, 0.0]);
    let (up, down) = ortho_sides(&arm, w);
    let mirror = |q: &[f64; 2]| [-q[0], q[1]];
    let h = w / 2.0;
    let mut pts = vec![[g / 2.0, b + h], [a / 2.0 + h, b + h], [a / 2.0 + h, h]];
    pts.extend(up.iter().skip(1));
    pts.extend(down.iter().rev());
    pts.extend(down.iter().map(mirror));
    pts.extend(up.iter().skip(1).rev().map(mirror));
    pts.extend([[-a / 2.0 - h, h], [-a / 2.0 - h, b + h], [-g / 2.0, b + h], [-g / 2.0, b - h], [-a / 2.0 + h, b - h], [-a / 2.0 + h, h], [a / 2.0 - h, h], [a / 2.0 - h, b - h], [g / 2.0, b - h]]);
    pts.reverse();
    Ok(Geometry {
        solids: vec![poly("tag", c.metal, 2, 0.0, pts)],
        ports: vec![port("chip", [-g / 2.0, b, 0.0], [g / 2.0, b, 0.0], w, z0)],
        hints: vec![hint([-lx / 2.0, -hm.max(w), 0.0], [lx / 2.0, b.max(hm) + w, 0.0], w * 4.0)],
        ..Default::default()
    })
}

fn rfid_reader_cp_params() -> Vec<Param> {
    let mut v = base("866.5MHz");
    v.extend([
        p("h", "0.04*lambda0", "Air gap above the ground plate"),
        p("L", "0.44*lambda0", "Patch side"),
        p("t", "0.075*lambda0", "Size of the two cut corners: what splits the patch into two modes"),
        p("xf", "0.1*lambda0", "Probe offset from the centre"),
        p("G", "0.9*lambda0", "Ground plate size"),
        p("wp", "lambda0/80", "Probe width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}

/// A square air-spaced patch with two opposite corners cut, fed once: the simplest way to a
/// circular reader panel. Which corners decides the hand.
fn rfid_reader_cp(c: &Ctx) -> Result<Geometry> {
    let (h, l, t, xf, gs, wp, z0) = (c.get("h")?, c.get("L")?, c.get("t")?, c.get("xf")?, c.get("G")?, c.get("wp")?, c.get("Z0")?);
    let s = l / 2.0;
    if !(t > 0.0 && t < s) {
        bail!("The cut corners have to be smaller than half the patch.");
    }
    let patch = vec![[-s, -s], [s - t, -s], [s, -s + t], [s, s], [-s + t, s], [-s, s - t]];
    Ok(Geometry {
        solids: vec![bx("ground", c.metal, [-gs / 2.0, -gs / 2.0, 0.0], [gs / 2.0, gs / 2.0, 0.0]), poly("patch", c.metal, 2, h, patch)],
        ports: vec![port("probe", [xf, 0.0, 0.0], [xf, 0.0, h], wp, z0)],
        axis_forward: true,
        hints: vec![hint([-s, -s, 0.0], [s, s, h], l / 40.0)],
        ..Default::default()
    })
}

fn rfid_reader_dual_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("h", "0.05*lambda0", "Air gap above the ground plate"),
        p("L", "0.43*lambda0", "Patch side"),
        p("xf", "0.1*lambda0", "Probe offset from the centre, on each axis"),
        p("G", "0.9*lambda0", "Ground plate size"),
        p("wp", "lambda0/80", "Probe width"),
        p("Z0", "50", "Port impedance"),
    ]);
    v
}

/// A square patch fed on two axes, a quarter cycle apart by a hybrid: the circular panel for a
/// wide band like 902-928 MHz, where one feed and cut corners run out of axial ratio.
fn rfid_reader_dual(c: &Ctx) -> Result<Geometry> {
    let (h, l, xf, gs, wp, z0) = (c.get("h")?, c.get("L")?, c.get("xf")?, c.get("G")?, c.get("wp")?, c.get("Z0")?);
    let s = l / 2.0;
    Ok(Geometry {
        solids: vec![bx("ground", c.metal, [-gs / 2.0, -gs / 2.0, 0.0], [gs / 2.0, gs / 2.0, 0.0]), bx("patch", c.metal, [-s, -s, h], [s, s, h])],
        ports: vec![port("probe x", [xf, 0.0, 0.0], [xf, 0.0, h], wp, z0), port("probe y", [0.0, xf, 0.0], [0.0, xf, h], wp, z0)],
        axis_forward: true,
        hints: vec![hint([-s, -s, 0.0], [s, s, h], l / 40.0)],
        ..Default::default()
    })
}

fn custom_params() -> Vec<Param> {
    let mut v = base("2.45GHz");
    v.extend([p("L", "0.47*lambda0", "Example variable"), p("Z0", "50", "Port impedance")]);
    v
}
fn custom(c: &Ctx) -> Result<Geometry> {
    let _ = c;
    Ok(Geometry::default())
}

const fn tpl(id: &'static str, title: &'static str, category: Category, blurb: &'static str, band: (f64, f64), substrate: &'static str, params: fn() -> Vec<Param>, build: fn(&Ctx) -> Result<Geometry>) -> Template {
    Template { id, title, category, blurb, band, substrate, params, build }
}

static TEMPLATES: [Template; 36] = [
    tpl("dipole", "Half-wave dipole", Category::Wire, "The reference antenna: omnidirectional, about 2.15 dBi, 73 Ω.", (0.7, 1.3), "Air", dipole_params, dipole),
    tpl("monopole", "Monopole on ground", Category::Wire, "Quarter-wave over an infinite ground plane: about 5.15 dBi, 36 Ω.", (0.7, 1.3), "Air", monopole_params, monopole),
    tpl("folded_dipole", "Folded dipole", Category::Wire, "A dipole's pattern with four times the impedance, about 300 Ω.", (0.7, 1.3), "Air", folded_params, folded),
    tpl("loop", "Square loop", Category::Wire, "One wavelength around; radiates broadside at about 3.3 dBi.", (0.7, 1.3), "Air", loop_params, square_loop),
    tpl("yagi", "Yagi-Uda", Category::Wire, "Reflector, driven element and directors: 8–14 dBi end-fire towards +z.", (0.8, 1.2), "Air", yagi_params, yagi),
    tpl("lpda", "Log-periodic dipole array", Category::Wideband, "Twin-boom LPDA with a frequency-independent pattern of 7–9 dBi over an octave.", (0.7, 1.4), "Air", lpda_params, lpda),
    tpl("helix_axial", "Axial-mode helix", Category::Wire, "Circularly polarised end-fire over ground: 11–15 dBi, about 140 Ω.", (0.7, 1.3), "Air", helix_axial_params, helix_axial),
    tpl("helix_normal", "Normal-mode helix", Category::Wire, "Electrically small coil over ground; narrow band, monopole-like pattern.", (0.7, 1.3), "Air", helix_normal_params, helix_normal),
    tpl("turnstile", "Turnstile", Category::Wire, "Two crossed dipoles on two ports: drive them in quadrature for circular polarisation.", (0.8, 1.2), "Air", turnstile_params, turnstile),
    tpl("discone", "Discone", Category::Wideband, "Wire-grid disc over a cone: omnidirectional over several octaves.", (0.6, 2.4), "Air", discone_params, discone),
    tpl("meander", "Meander-line monopole", Category::Wire, "A folded monopole for tight spaces, on an infinite ground plane.", (0.7, 1.3), "Air", meander_params, meander),
    tpl("strip_dipole", "Strip dipole", Category::Planar, "A dipole cut from flat sheet, as etched or stamped.", (0.7, 1.3), "Air", strip_params, strip_dipole),
    tpl("bowtie", "Bow-tie", Category::Wideband, "Flared flat dipole with much more bandwidth than a strip.", (0.6, 1.5), "Air", bowtie_params, bowtie),
    tpl("patch_air", "Patch in air", Category::Planar, "A plate above a ground plate with a probe feed: 8–9 dBi, the low-loss reader panel.", (0.85, 1.15), "Air", patch_air_params, patch_air),
    tpl("pifa", "PIFA", Category::Planar, "Planar inverted-F: a shorted plate over ground, the classic device antenna.", (0.7, 1.3), "Air", pifa_params, pifa),
    tpl("slot", "Slot in a plate", Category::Planar, "The dual of a dipole, with a high feed impedance of a few hundred ohms.", (0.85, 1.15), "Air", slot_params, slot),
    tpl("corner_reflector", "Corner reflector", Category::Planar, "A dipole in a 90° corner of two plates: 10–13 dBi at φ = 45°.", (0.8, 1.2), "Air", corner_params, corner),
    tpl("horn", "Pyramidal horn", Category::Aperture, "Probe-fed waveguide flared to an aperture: 15–20 dBi with low side lobes.", (0.85, 1.15), "Air", horn_params, horn),
    tpl("oewg", "Open-ended waveguide", Category::Aperture, "Probe-fed rectangular guide, open at one end.", (0.85, 1.15), "Air", oewg_params, oewg),
    tpl("patch_probe", "Patch, probe fed", Category::Printed, "Rectangular microstrip patch from the transmission-line model.", (0.8, 1.2), "FR-4", patch_probe_params, patch_probe),
    tpl("patch_inset", "Patch, inset microstrip", Category::Printed, "Patch fed by a 50 Ω line in a notch.", (0.8, 1.2), "FR-4", patch_inset_params, patch_inset),
    tpl("patch_circular", "Patch, circular", Category::Printed, "Circular microstrip patch, TM11 mode.", (0.8, 1.2), "FR-4", patch_circular_params, patch_circular),
    tpl("ifa_pcb", "PCB inverted-F", Category::Printed, "Trace antenna at the edge of a board, as on BLE and WiFi modules.", (0.7, 1.3), "FR-4", ifa_params, ifa),
    tpl("uwb_monopole", "UWB disc monopole", Category::Wideband, "Printed disc over a truncated ground: a matched band several GHz wide.", (0.4, 1.7), "FR-4", uwb_params, uwb),
    tpl("vivaldi", "Vivaldi", Category::Wideband, "Exponentially tapered slot: end-fire and very wide band.", (0.5, 1.8), "RO4003C", vivaldi_params, vivaldi),
    tpl("mimo_patch", "Two-port patch pair", Category::Printed, "Two patches on one board: study coupling, ECC and diversity.", (0.8, 1.2), "FR-4", mimo_params, mimo),
    tpl("dra", "Dielectric resonator", Category::Aperture, "A ceramic cylinder on a ground plate, probe fed: low loss at high frequency.", (0.75, 1.25), "Ceramic (εr 20)", dra_params, dra),
    tpl("rfid_gate", "RFID security gate", Category::Planar, "A pedestal of five separately fed elements in front of a metal panel: test read rate through the aisle under any phases.", (0.9, 1.1), "Air", gate_params, gate),
    tpl("rfid_tag", "RFID tag inlay", Category::Planar, "A meandered dipole with a T-match, inductive to suit a chip's capacitive input. Judged by power into the chip, not by S11.", (0.85, 1.15), "Air", rfid_tag_params, rfid_tag),
    tpl("rfid_reader_cp", "RFID reader panel, cut corners", Category::Planar, "An air-spaced patch with two corners cut and one feed: circular over a narrow band, such as 865-868 MHz.", (0.9, 1.1), "Air", rfid_reader_cp_params, rfid_reader_cp),
    tpl("rfid_reader_dual", "RFID reader panel, two feeds", Category::Planar, "An air-spaced patch fed on two axes in quadrature: circular over a band as wide as 902-928 MHz.", (0.85, 1.15), "Air", rfid_reader_dual_params, rfid_reader_dual),
    tpl("reconfig_dipole", "Switched-length dipole", Category::Reconfigurable, "Frequency: PIN diodes join sections of a dipole, one band per state.", (0.45, 1.25), "Air", reconfig_dipole_params, reconfig_dipole),
    tpl("reconfig_pifa", "Varactor-tuned PIFA", Category::Reconfigurable, "Frequency: a varactor at the plate's open end tunes it continuously with bias, as on a phone.", (0.5, 1.3), "Air", reconfig_pifa_params, reconfig_pifa),
    tpl("reconfig_parasitic", "Switched-beam parasitic array", Category::Reconfigurable, "Pattern: a monopole ringed by four parasitics. A shorted one reflects, so PIN diodes turn the beam.", (0.85, 1.15), "Air", reconfig_parasitic_params, reconfig_parasitic),
    tpl("reconfig_pol_patch", "Polarisation-switched patch", Category::Reconfigurable, "Polarisation: a square patch with switched corners over ground, for linear and either hand of circular.", (0.88, 1.12), "Air", reconfig_pol_params, reconfig_pol),
    tpl("custom", "Blank model", Category::Custom, "Start empty: add your own parts, ports and variables, or import a DXF, Gerber or STL.", (0.7, 1.3), "Air", custom_params, custom),
];

pub fn all() -> &'static [Template] {
    &TEMPLATES
}

pub fn by_id(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    #[test]
    fn ids_are_unique_and_default_models_are_real() {
        for (i, t) in all().iter().enumerate() {
            assert!(!all()[i + 1..].iter().any(|o| o.id == t.id), "{}", t.id);
            if t.id == "custom" {
                continue;
            }
            let p = Project::from_template(t.id, None).unwrap();
            let g = p.geometry().unwrap_or_else(|e| panic!("{}: {e:#}", t.id));
            let (lo, hi) = g.bounds();
            let size = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
            let lambda = crate::C0 / p.f0();
            assert!(size > 0.02 * lambda && size < 12.0 * lambda, "{}: {size} m at λ {lambda}", t.id);
        }
    }

    #[test]
    fn solver_choice_follows_the_geometry() {
        let fit = |id: &str| Project::from_template(id, None).unwrap().geometry().unwrap().solver_fit();
        assert!(fit("dipole").wire_mom && !fit("dipole").surface_mom);
        assert!(fit("helix_axial").wire_mom);
        assert!(fit("strip_dipole").surface_mom && !fit("strip_dipole").wire_mom);
        assert!(fit("horn").surface_mom);
        let patch = fit("patch_probe");
        assert!(!patch.wire_mom && !patch.surface_mom && patch.fdtd);
        // With air under the trace the printed IFA becomes a sheet antenna.
        let mut p = Project::from_template("ifa_pcb", None).unwrap();
        p.substrate = "Air".into();
        assert!(p.geometry().unwrap().solver_fit().surface_mom);
    }

    #[test]
    fn patch_dimensions_match_the_textbook() {
        let p = Project::from_template("patch_probe", Some("2.45GHz")).unwrap();
        let v = p.vars().unwrap();
        assert!((0.0265..0.0305).contains(&v["L"]), "{}", v["L"]);
        assert!((0.0345..0.0395).contains(&v["W"]), "{}", v["W"]);
    }

    #[test]
    fn microstrip_width_is_right_for_fr4() {
        let p = Project::from_template("patch_inset", Some("2.45GHz")).unwrap();
        let v = p.vars().unwrap();
        // 50 Ω on 1.6 mm FR-4 is about 3 mm.
        assert!((2.7e-3..3.4e-3).contains(&v["wf"]), "{}", v["wf"]);
        assert!(v["y0"] > 0.0 && v["y0"] < v["L"] / 2.0, "{}", v["y0"]);
    }

    #[test]
    fn scale_with_frequency() {
        let a = Project::from_template("yagi", Some("1GHz")).unwrap().geometry().unwrap().bounds();
        let b = Project::from_template("yagi", Some("2GHz")).unwrap().geometry().unwrap().bounds();
        let size = |x: (P3, P3)| x.1[2] - x.0[2];
        assert!((size(a) / size(b) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn helix_winding_sense() {
        let mut p = Project::from_template("helix_axial", None).unwrap();
        let first_y = |p: &Project| {
            let g = p.geometry().unwrap();
            let Shape::Wire { pts, .. } = &g.solids[0].shape else { panic!() };
            pts[2][1]
        };
        assert!(first_y(&p) > 0.0);
        p.params.iter_mut().find(|r| r.name == "hand").unwrap().expr = "-1".into();
        assert!(first_y(&p) < 0.0);
    }
}

/// Prints where each MoM template really resonates against its design frequency:
/// `cargo test -p antenna_core --release template_audit -- --ignored --nocapture`
#[cfg(test)]
mod audit {
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::Project;
    use crate::sparam::Network;

    #[test]
    #[ignore]
    fn template_audit() {
        for t in super::all() {
            if t.id == "custom" {
                continue;
            }
            let p = Project::from_template(t.id, None).unwrap();
            let g = p.geometry().unwrap();
            let fit = g.solver_fit();
            if !(fit.wire_mom || fit.surface_mom) {
                continue;
            }
            let f0 = p.f0();
            let opts = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, theta_step: 5.0, phi_step: 5.0 };
            let prob = MomProblem::new(&g, p.setup.f_hi, opts).unwrap();
            if prob.unknowns() > crate::project::AUTO_MOM_LIMIT {
                println!("{:18} {:6} unknowns: FDTD by default, skipped", t.id, prob.unknowns());
                continue;
            }
            let fs = crate::linspace(p.setup.f_lo, p.setup.f_hi, 21);
            let z: Vec<_> = fs.iter().map(|f| prob.solve(*f, false).unwrap().z).collect();
            let net = Network::from_z(fs, &z, g.ports.iter().map(|x| x.z0).collect());
            let net = net.interpolate(&crate::linspace(p.setup.f_lo, p.setup.f_hi, 201));
            let db = net.sij_db(0, 0);
            let k = net.f.iter().enumerate().min_by(|a, b| (a.1 - f0).abs().total_cmp(&(b.1 - f0).abs())).unwrap().0;
            let z0 = net.zin(0)[k];
            let best = net.f.iter().zip(&db).min_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            let m = prob.solve(f0, true).unwrap().far[0].metrics();
            println!(
                "{:18} {:5} unk  Z({:.0}Ω) {:7.1}{:+7.1}j  S11@f0 {:6.1}  best {:6.1} dB at {:.3}·f0  RG {:5.2} dBi θ{:3.0} φ{:3.0} eff {:.3}",
                t.id, prob.unknowns(), net.z0[0], z0.re, z0.im, db[k], best.1, best.0 / f0, m.peak_realized_dbi, m.peak_theta, m.peak_phi, m.rad_efficiency
            );
        }
    }
}

