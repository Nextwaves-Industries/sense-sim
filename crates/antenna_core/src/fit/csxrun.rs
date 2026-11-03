//! Runs a CSXCAD model, the XML the openEMS paths write, on the built-in solver, and writes the same
//! result files openEMS would: `port_ut1`, `port_it1`, and `<name>_f=<Hz>_abs.vtr` with its `_arg` and
//! `_p=NNN` companions. So everything that reads an openEMS run folder works on this too: the scene and
//! wall studies, the SAR pre-check, the hardware recipes, without any of them knowing which ran.
//!
//! The mesh is whatever the XML holds, drawn for openEMS, so metal is corrected inside the cells it
//! was already refined around. That costs time against the built-in planner, and changes no physics.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use fdtd_kernel::assemble::assemble;
use fdtd_kernel::engine::{Engine, Stop, stable_dt};
use fdtd_kernel::grid::Grid;
use fdtd_kernel::problem::{self as kp, Body, Mat, Port, Problem, Pulse, Sheet, Surface, Wall, Wire};
use fdtd_kernel::record::{NodeDump, NodePlane};

use super::Backend;

// ---------------------------------------------------------------------------
// A small reader for the XML the app itself writes.

#[derive(Debug, Default, Clone)]
pub struct Elem {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Elem>,
    pub text: String,
}

impl Elem {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    fn num(&self, name: &str) -> Option<f64> {
        self.attr(name).and_then(|v| v.trim().parse().ok())
    }
    fn kid(&self, tag: &str) -> Option<&Elem> {
        self.kids.iter().find(|k| k.tag == tag)
    }
    fn point(&self, tag: &str) -> Option<[f64; 3]> {
        let p = self.kid(tag)?;
        Some([p.num("X")?, p.num("Y")?, p.num("Z")?])
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Parses elements, attributes and text. No namespaces, entities beyond the five, or CDATA: what the
/// app writes doesn't use them.
pub fn parse_xml(text: &str) -> Result<Elem> {
    let mut stack: Vec<Elem> = vec![Elem::default()];
    let b = text.as_bytes();
    let mut at = 0;
    while at < b.len() {
        if b[at] != b'<' {
            let end = text[at..].find('<').map_or(text.len(), |n| at + n);
            let run = text[at..end].trim();
            if !run.is_empty() {
                stack.last_mut().unwrap().text.push_str(run);
            }
            at = end;
            continue;
        }
        let close = text[at..].find('>').context("a tag that never closes")? + at;
        let inner = &text[at + 1..close];
        if inner.starts_with('?') || inner.starts_with('!') {
            at = close + 1;
            continue;
        }
        if let Some(name) = inner.strip_prefix('/') {
            let done = stack.pop().context("a closing tag with nothing open")?;
            if done.tag != name.trim() {
                bail!("<{}> closed by </{}>", done.tag, name.trim());
            }
            stack.last_mut().context("too many closing tags")?.kids.push(done);
            at = close + 1;
            continue;
        }
        let self_closed = inner.ends_with('/');
        let inner = inner.strip_suffix('/').unwrap_or(inner);
        let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
        let mut e = Elem { tag: inner[..name_end].to_string(), ..Default::default() };
        let mut rest = inner[name_end..].trim_start();
        while !rest.is_empty() {
            let eq = rest.find('=').context("an attribute with no value")?;
            let key = rest[..eq].trim().to_string();
            let after = rest[eq + 1..].trim_start();
            let quote = after.chars().next().context("an attribute with no value")?;
            let end = after[1..].find(quote).context("an attribute that never closes")? + 1;
            e.attrs.push((key, unescape(&after[1..end])));
            rest = after[end + 1..].trim_start();
        }
        if self_closed {
            stack.last_mut().unwrap().kids.push(e);
        } else {
            stack.push(e);
        }
        at = close + 1;
    }
    if stack.len() != 1 {
        bail!("<{}> never closed", stack.last().unwrap().tag);
    }
    let mut root = stack.pop().unwrap();
    root.kids.pop().context("an empty document")
}

// ---------------------------------------------------------------------------
// From the model to the kernel's problem.

/// One port as the XML gives it.
#[derive(Clone, Debug)]
struct CsxPort {
    nr: u32,
    excited: bool,
}

#[derive(Clone, Debug)]
struct CsxDump {
    name: String,
    magnetic: bool,
    freqs: Vec<f64>,
    min: [f64; 3],
    max: [f64; 3],
    resolution: Option<f64>,
}

/// A parsed run.
pub struct CsxModel {
    pub problem: Problem,
    ports: Vec<CsxPort>,
    dumps: Vec<CsxDump>,
    pub pulse: Pulse,
    pub end_db: f64,
    pub max_steps: u64,
}

fn lines_of(e: &Elem, tag: &str) -> Result<Vec<f64>> {
    let k = e.kid(tag).with_context(|| format!("no <{tag}> in the mesh"))?;
    let v: Vec<f64> = k.text.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if v.len() < 2 {
        bail!("<{tag}> holds {} lines", v.len());
    }
    Ok(v)
}

fn wall_of(v: Option<&str>) -> Wall {
    match v.unwrap_or("PEC").trim() {
        "PEC" | "0" => Wall::Pec,
        // Anything absorbing: an absorber here, whichever kind was asked for.
        other => Wall::Pml(other.strip_prefix("PML_").and_then(|n| n.parse().ok()).unwrap_or(8)),
    }
}

/// A primitive and where it sits among the others.
struct Prim {
    priority: i32,
    kind: PrimKind,
}

enum PrimKind {
    Body(kp::Solid),
    Flat { axis: usize, at: f64, pts: Vec<[f64; 2]> },
    Line(Vec<[f64; 3]>),
}

fn primitives(owner: &Elem) -> Result<Vec<Prim>> {
    let Some(list) = owner.kid("Primitives") else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for p in &list.kids {
        let priority = p.num("Priority").unwrap_or(0.0) as i32;
        let kind = match p.tag.as_str() {
            "Box" => {
                let (a, b) = (p.point("P1").context("a Box with no P1")?, p.point("P2").context("a Box with no P2")?);
                let min: [f64; 3] = std::array::from_fn(|k| a[k].min(b[k]));
                let max: [f64; 3] = std::array::from_fn(|k| a[k].max(b[k]));
                let flat: Vec<usize> = (0..3).filter(|k| max[*k] <= min[*k]).collect();
                match flat.len() {
                    0 => PrimKind::Body(kp::Solid::Box { min, max }),
                    1 => {
                        let axis = flat[0];
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        PrimKind::Flat { axis, at: min[axis], pts: vec![[min[u], min[v]], [max[u], min[v]], [max[u], max[v]], [min[u], max[v]]] }
                    }
                    2 => PrimKind::Line(vec![min, max]),
                    _ => continue,
                }
            }
            "Cylinder" => PrimKind::Body(kp::Solid::Cylinder { a: p.point("P1").context("a Cylinder with no P1")?, b: p.point("P2").context("a Cylinder with no P2")?, r: p.num("Radius").context("a Cylinder with no radius")? }),
            "Sphere" => PrimKind::Body(kp::Solid::Sphere { c: p.point("Center").context("a Sphere with no centre")?, r: p.num("Radius").context("a Sphere with no radius")? }),
            "Polygon" | "LinPoly" => {
                let axis = p.num("NormDir").context("a polygon with no NormDir")? as usize;
                let at = p.num("Elevation").unwrap_or(0.0);
                let pts: Vec<[f64; 2]> = p.kids.iter().filter(|v| v.tag == "Vertex").filter_map(|v| Some([v.num("X1")?, v.num("X2")?])).collect();
                let thick = p.num("Length").unwrap_or(0.0);
                if thick > 0.0 {
                    PrimKind::Body(kp::Solid::Prism { axis, at, pts, thick })
                } else {
                    PrimKind::Flat { axis, at, pts }
                }
            }
            "Curve" => PrimKind::Line(p.kids.iter().filter(|v| v.tag == "Vertex").filter_map(|v| Some([v.num("X")?, v.num("Y")?, v.num("Z")?])).collect()),
            "Polyhedron" => {
                let verts: Vec<[f64; 3]> = p.kids.iter().filter(|v| v.tag == "Vertex").filter_map(|v| { let n: Vec<f64> = v.text.split(',').filter_map(|x| x.trim().parse().ok()).collect(); (n.len() == 3).then(|| [n[0], n[1], n[2]]) }).collect();
                let tris: Vec<[[f64; 3]; 3]> = p.kids.iter().filter(|v| v.tag == "Face").filter_map(|v| { let n: Vec<usize> = v.text.split(',').filter_map(|x| x.trim().parse().ok()).collect(); (n.len() == 3 && n.iter().all(|i| *i < verts.len())).then(|| [verts[n[0]], verts[n[1]], verts[n[2]]]) }).collect();
                PrimKind::Body(kp::Solid::Mesh { tris })
            }
            other => bail!("a {other} primitive, which the built-in solver doesn't read"),
        };
        out.push(Prim { priority, kind });
    }
    Ok(out)
}

/// Reads a model.
pub fn parse_model(xml: &str) -> Result<CsxModel> {
    let root = parse_xml(xml)?;
    if root.tag != "openEMS" {
        bail!("not an openEMS model: the root is <{}>", root.tag);
    }
    let fdtd = root.kid("FDTD").context("no <FDTD> settings")?;
    let exc = fdtd.kid("Excitation").context("no excitation")?;
    let pulse = Pulse { f0: exc.num("f0").context("no f0")?, fc: exc.num("fc").context("no fc")? };
    let bc = fdtd.kid("BoundaryCond").context("no boundary conditions")?;
    let walls = [wall_of(bc.attr("xmin")), wall_of(bc.attr("xmax")), wall_of(bc.attr("ymin")), wall_of(bc.attr("ymax")), wall_of(bc.attr("zmin")), wall_of(bc.attr("zmax"))];
    let cs = root.kid("ContinuousStructure").context("no <ContinuousStructure>")?;
    let grid_e = cs.kid("RectilinearGrid").context("no mesh")?;
    let grid = Grid::new([lines_of(grid_e, "XLines")?, lines_of(grid_e, "YLines")?, lines_of(grid_e, "ZLines")?]);
    let props = cs.kid("Properties").context("no <Properties>")?;

    let mut problem = Problem::new(grid);
    problem.walls = walls;
    problem.design_f = pulse.f0;
    // Bodies in rising priority, as CSXCAD resolves overlaps; within one priority, as written.
    let mut bodies: Vec<(i32, Body)> = Vec::new();
    let mut resist: Vec<(u32, usize, f64, [f64; 3], [f64; 3])> = Vec::new();
    let mut excited: Vec<u32> = Vec::new();
    let mut ut: Vec<(u32, [f64; 3], [f64; 3], f64)> = Vec::new();
    let mut dumps = Vec::new();
    for p in &props.kids {
        let name = p.attr("Name").unwrap_or("");
        match p.tag.as_str() {
            "Material" => {
                let prop = p.kid("Property");
                let mat = Mat::Dielectric {
                    eps_r: prop.and_then(|x| x.num("Epsilon")).unwrap_or(1.0),
                    mu_r: prop.and_then(|x| x.num("Mue")).unwrap_or(1.0),
                    kappa: prop.and_then(|x| x.num("Kappa")).unwrap_or(0.0),
                    debye: None,
                };
                for prim in primitives(p)? {
                    let PrimKind::Body(solid) = prim.kind else { continue };
                    bodies.push((prim.priority, Body { solid, mat }));
                }
            }
            "Metal" | "ConductingSheet" => {
                let surface = if p.tag == "ConductingSheet" {
                    let (sigma, thick) = (p.num("Conductivity").context("a sheet with no conductivity")?, p.num("Thickness").context("a sheet with no thickness")?);
                    Some(Surface::sheet(sigma, thick, pulse.f0))
                } else {
                    None
                };
                for prim in primitives(p)? {
                    match prim.kind {
                        PrimKind::Body(solid) => bodies.push((prim.priority, Body { solid, mat: Mat::Pec })),
                        PrimKind::Flat { axis, at, pts } => problem.sheets.push(Sheet { axis, at, polys: vec![pts], surface }),
                        // A radius isn't in the file: as openEMS, a line of metal edges.
                        PrimKind::Line(pts) => problem.wires.push(Wire { pts, r: 0.0, surface: None }),
                    }
                }
            }
            "LumpedElement" => {
                let nr = name.rsplit('_').next().and_then(|n| n.parse().ok()).with_context(|| format!("a lumped element '{name}' that isn't a port"))?;
                let axis = p.num("Direction").context("a port with no direction")? as usize;
                let r = p.num("R").context("a port with no resistance")?;
                let boxes = p.kid("Primitives").and_then(|l| l.kid("Box")).context("a port with no box")?;
                resist.push((nr, axis, r, boxes.point("P1").context("no P1")?, boxes.point("P2").context("no P2")?));
            }
            "Excitation" => {
                if let Some(nr) = name.rsplit('_').next().and_then(|n| n.parse().ok()) {
                    excited.push(nr);
                }
            }
            "ProbeBox" => {
                if let Some(rest) = name.strip_prefix("port_ut")
                    && let Ok(nr) = rest.parse()
                    && let Some(b) = p.kid("Primitives").and_then(|l| l.kid("Box"))
                {
                    ut.push((nr, b.point("P1").context("no P1")?, b.point("P2").context("no P2")?, p.num("Weight").unwrap_or(-1.0)));
                }
            }
            "DumpBox" => {
                let magnetic = match p.num("DumpType").unwrap_or(0.0) as i32 {
                    10 => false,
                    11 => true,
                    other => bail!("dump '{name}' is of type {other}: only frequency-domain E and H are read"),
                };
                let freqs: Vec<f64> = p.kid("FD_Samples").map_or(Vec::new(), |s| s.text.split(',').filter_map(|x| x.trim().parse().ok()).collect());
                let b = p.kid("Primitives").and_then(|l| l.kid("Box")).context("a dump with no box")?;
                let (a, c) = (b.point("P1").context("no P1")?, b.point("P2").context("no P2")?);
                dumps.push(CsxDump { name: name.to_string(), magnetic, freqs, min: std::array::from_fn(|k| a[k].min(c[k])), max: std::array::from_fn(|k| a[k].max(c[k])), resolution: p.num("OptResolution") });
            }
            other => bail!("a <{other}> property, which the built-in solver doesn't read"),
        }
    }
    bodies.sort_by_key(|(prio, _)| *prio);
    problem.bodies = bodies.into_iter().map(|(_, b)| b).collect();
    let mut ports = Vec::new();
    resist.sort_by_key(|r| r.0);
    for (nr, axis, r, lo, hi) in resist {
        // The voltage probe gives the port's direction. The kernel's voltage is minus the integral of E
        // from `a` to `b`, which is what a weight of -1 asks for from the probe's first point to its second.
        let (a, b) = match ut.iter().find(|u| u.0 == nr) {
            Some(&(_, p1, p2, w)) if w < 0.0 => (p1, p2),
            Some(&(_, p1, p2, _)) => (p2, p1),
            None => (lo, hi),
        };
        let centre: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5);
        let (mut pa, mut pb) = (centre, centre);
        pa[axis] = a[axis];
        pb[axis] = b[axis];
        let widths: Vec<(usize, f64)> = (0..3).filter(|k| *k != axis && (hi[*k] - lo[*k]).abs() > 0.0).map(|k| (k, (hi[k] - lo[k]).abs())).collect();
        problem.ports.push(Port { a: pa, b: pb, axis, width_axis: widths.first().map(|w| w.0), width: widths.first().map_or(0.0, |w| w.1), second: widths.get(1).copied(), z0: r });
        ports.push(CsxPort { nr, excited: excited.contains(&nr) });
    }
    let end = fdtd.num("endCriteria").unwrap_or(1e-4);
    let end_db = if end > 0.0 { -10.0 * end.log10() } else { 80.0 };
    let max_steps = fdtd.num("NumberOfTimesteps").unwrap_or(1e9) as u64;
    Ok(CsxModel { problem, ports, dumps, pulse, end_db, max_steps })
}

// ---------------------------------------------------------------------------
// Results, as openEMS writes them.

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn data_array(s: &mut String, name: &str, ty: &str, comps: usize, bytes: &[u8]) {
    let mut raw = Vec::with_capacity(bytes.len() + 4);
    raw.extend((bytes.len() as u32).to_le_bytes());
    raw.extend_from_slice(bytes);
    let _ = writeln!(s, r#"<DataArray type="{ty}" Name="{name}" NumberOfComponents="{comps}" format="binary">{}</DataArray>"#, b64(&raw));
}

/// One VTK rectilinear grid with a 3-vector per point. The reader takes uncompressed arrays too.
fn write_vtr(path: &Path, x: &[f64], y: &[f64], z: &[f64], values: &[[f32; 3]]) -> Result<()> {
    let mut s = String::new();
    let (nx, ny, nz) = (x.len(), y.len(), z.len());
    let extent = format!("0 {} 0 {} 0 {}", nx - 1, ny - 1, nz - 1);
    let _ = writeln!(s, r#"<?xml version="1.0"?><VTKFile type="RectilinearGrid" version="0.1" byte_order="LittleEndian">"#);
    let _ = writeln!(s, r#"<RectilinearGrid WholeExtent="{extent}"><Piece Extent="{extent}"><PointData Vectors="field">"#);
    let flat: Vec<u8> = values.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    data_array(&mut s, "field", "Float32", 3, &flat);
    let _ = writeln!(s, "</PointData><Coordinates>");
    for (name, c) in [("x", x), ("y", y), ("z", z)] {
        let b: Vec<u8> = c.iter().flat_map(|v| v.to_le_bytes()).collect();
        data_array(&mut s, name, "Float64", 1, &b);
    }
    let _ = writeln!(s, "</Coordinates></Piece></RectilinearGrid></VTKFile>");
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

/// The files for one dump at one frequency.
fn write_dump(dir: &Path, grid: &Grid, plane: &NodePlane) -> Result<()> {
    let coords = |k: usize| -> Vec<f64> { plane.lines[k].iter().map(|i| grid.lines[k][*i]).collect() };
    let (x, y, z) = (coords(0), coords(1), coords(2));
    let token = format!("{:.6}", plane.f);
    let abs: Vec<[f32; 3]> = plane.values.iter().map(|v| std::array::from_fn(|k| v[k].norm() as f32)).collect();
    let arg: Vec<[f32; 3]> = plane.values.iter().map(|v| std::array::from_fn(|k| v[k].arg() as f32)).collect();
    write_vtr(&dir.join(format!("{}_f={token}_abs.vtr", plane.name)), &x, &y, &z, &abs)?;
    write_vtr(&dir.join(format!("{}_f={token}_arg.vtr", plane.name)), &x, &y, &z, &arg)?;
    // Animation frames, for a plane only: a volume's would be a lot of disk for nothing that reads it.
    let flat = [x.len(), y.len(), z.len()].iter().filter(|n| **n > 1).count() <= 2;
    if flat && !plane.name.starts_with("nf2ff") {
        const FRAMES: usize = 21;
        for n in 0..FRAMES {
            let deg = (360.0 * n as f64 / FRAMES as f64).round();
            let turn = num_complex::Complex64::from_polar(1.0, deg.to_radians());
            let frame: Vec<[f32; 3]> = plane.values.iter().map(|v| std::array::from_fn(|k| (v[k] * turn).re as f32)).collect();
            write_vtr(&dir.join(format!("{}_f={token}_p={:03}.vtr", plane.name, deg as u32)), &x, &y, &z, &frame)?;
        }
    }
    Ok(())
}

fn write_probe(path: &Path, offset: f64, dt: f64, values: &[f64]) -> Result<()> {
    let mut s = String::with_capacity(values.len() * 28 + 32);
    s.push_str("% time/s\tvalue\n");
    for (n, v) in values.iter().enumerate() {
        let _ = writeln!(s, "{:e}\t{v:e}", (n as f64 + offset) * dt);
    }
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

/// Which lines of a box to keep: all of them, or those at least `resolution` apart.
fn dump_lines(grid: &Grid, d: &CsxDump) -> [Vec<usize>; 3] {
    std::array::from_fn(|k| {
        let l = &grid.lines[k];
        let eps = 1e-9 * (l[l.len() - 1] - l[0]).abs().max(1e-30);
        let mut idx: Vec<usize> = (0..l.len()).filter(|i| l[*i] >= d.min[k] - eps && l[*i] <= d.max[k] + eps).collect();
        // A flat dump between two lines takes the nearer.
        if idx.is_empty() {
            idx.push(grid.nearest(k, (d.min[k] + d.max[k]) * 0.5));
        }
        if let Some(res) = d.resolution
            && idx.len() > 2
        {
            let last = *idx.last().unwrap();
            let mut kept = vec![idx[0]];
            for &i in &idx[1..idx.len() - 1] {
                if l[i] - l[*kept.last().unwrap()] >= res * 0.999 && l[last] - l[i] >= res * 0.5 {
                    kept.push(i);
                }
            }
            kept.push(last);
            idx = kept;
        }
        idx
    })
}

/// What a run came to.
#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    /// It hit the step limit before the energy had decayed.
    pub truncated: bool,
    pub cells: u64,
    pub steps: u64,
    /// Millions of cell updates a second.
    pub speed: f64,
    /// The energy on the last report, in dB below its peak.
    pub energy_db: f64,
}

/// Runs `sim.xml` in `dir` and writes its results there. `tick` hears the step, the energy in dB below
/// its peak, and the speed in MC/s, and the run stops when `cancel` is set.
pub fn run_dir(dir: &Path, threads: usize, backend: Backend, cancel: &AtomicBool, tick: &(dyn Fn(u64, f64, f64)), log: &(dyn Fn(String))) -> Result<Outcome> {
    let xml = std::fs::read_to_string(dir.join("sim.xml")).context("reading sim.xml")?;
    let model = parse_model(&xml)?;
    let asm = assemble(&model.problem);
    for w in &asm.warnings {
        log(w.clone());
    }
    let grid = asm.grid.clone();
    let cells = grid.cells();
    let threads = if threads > 0 { threads } else { super::threads_for(cells) };
    let driven = model.ports.iter().position(|p| p.excited);
    let dumps: Vec<NodeDump> = model.dumps.iter().map(|d| NodeDump { name: d.name.clone(), magnetic: d.magnetic, freqs: d.freqs.clone(), lines: dump_lines(&grid, d) }).collect();
    // Ten samples per period of the top frequency asked of any dump, as the far-field recording does.
    let f_top = model.dumps.iter().flat_map(|d| d.freqs.iter().copied()).fold(model.pulse.f0 + model.pulse.fc, f64::max);
    let stable = stable_dt(&asm);
    let started = std::time::Instant::now();
    let run_cells = cells as f64;
    let last_db = std::sync::Mutex::new(0.0f64);
    // A slow blow-up can get past the estimate of the largest eigenvalue. A shorter step is always
    // stable and the answer barely moves, so that's worth one more go before calling it a fault.
    let mut outcome = None;
    for scale in [1.0, 0.75] {
        let dt = stable * scale;
        let every = ((1.0 / (10.0 * f_top * dt)).floor() as u64).max(1);
        let attempt = std::time::Instant::now();
        let mut hook = |step: u64, db: f64| -> bool {
            *last_db.lock().unwrap() = db;
            tick(step, db, run_cells * step as f64 / attempt.elapsed().as_secs_f64().max(1e-6) / 1e6);
            !cancel.load(Ordering::SeqCst)
        };
        let run = run_engine(&asm, dt, threads, backend, cells, every, &dumps, driven, model.pulse, model.end_db, model.max_steps, &mut hook, log)?;
        if run.0 == Stop::Diverged && scale == 1.0 {
            log(format!("The run diverged at step {}: going again on a shorter time step.", run.1));
            continue;
        }
        outcome = Some((run, dt));
        break;
    }
    let ((stop, steps, traces, planes), dt) = outcome.context("no run finished")?;
    match stop {
        Stop::Cancelled => bail!("cancelled"),
        Stop::Diverged => bail!("The run diverged at step {steps} even on a shorter time step. Try a finer mesh around small features."),
        _ => {}
    }
    for (p, t) in model.ports.iter().zip(&traces) {
        write_probe(&dir.join(format!("port_ut{}", p.nr)), 1.0, dt, &t.u)?;
        write_probe(&dir.join(format!("port_it{}", p.nr)), 0.5, dt, &t.i)?;
    }
    for plane in &planes {
        write_dump(dir, &grid, plane)?;
    }
    let energy_db = *last_db.lock().unwrap();
    Ok(Outcome { truncated: stop == Stop::StepLimit, cells, steps, speed: run_cells * steps as f64 / started.elapsed().as_secs_f64().max(1e-6) / 1e6, energy_db })
}

type RunResult = (Stop, u64, Vec<fdtd_kernel::engine::PortTrace>, Vec<NodePlane>);

#[allow(clippy::too_many_arguments)]
fn run_engine(asm: &fdtd_kernel::assemble::Assembled, dt: f64, threads: usize, backend: Backend, cells: u64, every: u64, dumps: &[NodeDump], driven: Option<usize>, pulse: Pulse, end_db: f64, max_steps: u64, hook: &mut dyn FnMut(u64, f64) -> bool, log: &(dyn Fn(String))) -> Result<RunResult> {
    #[cfg(feature = "gpu")]
    {
        let want = match backend {
            Backend::Auto => cells >= super::GPU_FROM_CELLS,
            Backend::Gpu => true,
            Backend::Cpu => false,
        };
        if want {
            match fdtd_kernel::gpu::GpuEngine::new(asm, dt, every) {
                Ok(mut g) => {
                    g.record_nodes(asm, dumps, every);
                    if let Some(p) = driven {
                        g.drive(p, pulse);
                    }
                    match g.run(end_db, max_steps, &mut *hook) {
                        Ok(stop) => return Ok((stop, g.step, g.traces.clone(), g.node_planes())),
                        Err(e) => log(format!("The GPU run stopped ({e:#}): going again on the CPU.")),
                    }
                }
                Err(e) => log(format!("No GPU run ({e:#}): on the CPU instead.")),
            }
        }
    }
    let _ = (backend, cells, &log);
    let mut e = Engine::<f32>::new(asm, dt, threads);
    e.record_nodes(&asm.grid, dumps, every);
    if let Some(p) = driven {
        e.drive(p, pulse);
    }
    let stop = e.run(end_db, max_steps, hook);
    let planes = e.node_planes();
    Ok((stop, e.step, e.traces.clone(), planes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_xml_reader_keeps_structure_attributes_and_text() {
        let e = parse_xml(r#"<?xml version="1.0"?><a x="1" y="a &amp; b"><b/><c>1,2,3</c><d z='q'><e>t</e></d></a>"#).unwrap();
        assert_eq!(e.tag, "a");
        assert_eq!(e.attr("y"), Some("a & b"));
        assert_eq!(e.kids.len(), 3);
        assert_eq!(e.kids[1].text, "1,2,3");
        assert_eq!(e.kids[2].kids[0].text, "t");
        assert!(parse_xml("<a><b></a>").is_err());
        assert!(parse_xml("<a>").is_err());
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
        assert_eq!(b64(b""), "");
    }

    /// Everything the antenna path can write has to parse, for every template.
    #[test]
    fn every_template_reads_back() {
        for t in crate::templates::all() {
            if t.id == "custom" {
                continue;
            }
            let p = crate::project::Project::from_template(t.id, None).unwrap();
            let g = p.geometry().unwrap();
            let plan = crate::csx::plan(&g, &p.setup, p.f0()).unwrap();
            for port in 0..g.ports.len() {
                let m = parse_model(&crate::csx::to_xml(&g, &plan, port)).unwrap_or_else(|e| panic!("{}: {e:#}", t.id));
                assert_eq!(m.problem.ports.len(), g.ports.len(), "{}", t.id);
                assert_eq!(m.ports.iter().filter(|x| x.excited).count(), 1, "{}", t.id);
                assert_eq!(m.ports.iter().position(|x| x.excited), Some(port), "{}", t.id);
            }
        }
    }

    fn at_probe(dir: &Path, name: &str, f: f64) -> num_complex::Complex64 {
        let text = std::fs::read_to_string(dir.join(name)).unwrap();
        let mut sum = num_complex::Complex64::new(0.0, 0.0);
        for l in text.lines().skip(1) {
            let mut it = l.split_whitespace().map(|x| x.parse::<f64>().unwrap());
            let (t, v) = (it.next().unwrap(), it.next().unwrap());
            sum += num_complex::Complex64::from_polar(v, -2.0 * std::f64::consts::PI * f * t);
        }
        sum
    }

    /// The same antenna two ways, on the XML's mesh here and the planner's there: they're different
    /// meshes of one structure, so they should land close but not on top of each other.
    #[test]
    #[ignore]
    fn a_csx_run_agrees_with_the_built_in_planner() {
        let mut p = crate::project::Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        p.setup.cells_per_lambda = 20.0;
        p.setup.field_planes = false;
        let g = p.geometry().unwrap();
        let cancel = AtomicBool::new(false);
        let (busy, log) = (|_: String, _: f32| {}, |s: String| println!("  {s}"));
        let direct = super::super::simulate(&p, &g, &super::super::Options::default(), &super::super::Hooks { cancel: &cancel, busy: &busy, log: &log }).unwrap();
        let k = direct.network.f.iter().enumerate().min_by(|a, b| (a.1 - 1e9).abs().total_cmp(&(b.1 - 1e9).abs())).unwrap().0;
        let z_direct = direct.network.zin(0)[k];
        let plan = crate::csx::plan(&g, &p.setup, p.f0()).unwrap();
        let dir = std::env::temp_dir().join(format!("csxrun_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sim.xml"), crate::csx::to_xml(&g, &plan, 0)).unwrap();
        let out = run_dir(&dir, 0, Backend::Cpu, &cancel, &|_, _, _| {}, &|s| println!("  {s}")).unwrap();
        let z = at_probe(&dir, "port_ut1", 1e9) / at_probe(&dir, "port_it1", 1e9);
        println!("planner Z {z_direct:.2}, {} cells; XML Z {z:.2}, {} cells, {} steps, {:.0} MC/s", direct.size, out.cells, out.steps, out.speed);
        assert!(!out.truncated);
        assert!((z - z_direct).norm() / z_direct.norm() < 0.10, "{z} against {z_direct}");
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(names.iter().any(|n| n.starts_with("nf2ff_E_xn_f=") && n.ends_with("_abs.vtr")), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("nf2ff_H_zp_f=") && n.ends_with("_arg.vtr")), "{names:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A dump from the GPU has to read like one from the CPU, E and H, on a model with a dielectric.
    #[cfg(feature = "gpu")]
    #[test]
    #[ignore]
    fn dumps_from_the_gpu_match_the_cpu() {
        let mut p = crate::project::Project::from_template("patch_probe", Some("2.45GHz")).unwrap();
        p.setup.cells_per_lambda = 20.0;
        let g = p.geometry().unwrap();
        let plan = crate::csx::plan(&g, &p.setup, p.f0()).unwrap();
        let m = parse_model(&crate::csx::to_xml(&g, &plan, 0)).unwrap();
        let asm = assemble(&m.problem);
        let dt = stable_dt(&asm);
        let dumps: Vec<NodeDump> = m.dumps.iter().map(|d| NodeDump { name: d.name.clone(), magnetic: d.magnetic, freqs: d.freqs.clone(), lines: dump_lines(&asm.grid, d) }).collect();
        let f_top = m.pulse.f0 + m.pulse.fc;
        let every = ((1.0 / (10.0 * f_top * dt)).floor() as u64).max(1);
        let cells = asm.grid.cells();
        let run = |b: Backend| {
            let mut hook = |_: u64, _: f64| true;
            run_engine(&asm, dt, 16, b, cells, every, &dumps, m.ports.iter().position(|p| p.excited), m.pulse, m.end_db, m.max_steps, &mut hook, &|s| println!("  {s}")).unwrap()
        };
        let (cpu, gpu) = (run(Backend::Cpu), run(Backend::Gpu));
        println!("{} cells, CPU {} steps, GPU {} steps, {} planes", cells, cpu.1, gpu.1, cpu.3.len());
        assert!(!cpu.3.is_empty());
        assert_eq!(cpu.3.len(), gpu.3.len());
        for (a, b) in cpu.3.iter().zip(&gpu.3) {
            assert_eq!(a.values.len(), b.values.len(), "{}", a.name);
            let peak = a.values.iter().flatten().map(|v| v.norm()).fold(0.0, f64::max);
            let worst = a.values.iter().flatten().zip(b.values.iter().flatten()).map(|(x, y)| (x - y).norm()).fold(0.0, f64::max);
            println!("{} at {:.3} GHz: peak {peak:.3e}, worst difference {:.2e} of it", a.name, a.f / 1e9, worst / peak);
            assert!(peak > 0.0 && worst / peak < 2e-2, "{}: {worst} of {peak}", a.name);
        }
    }

    /// The field a dump reports, against the voltage the port reads across the same gap: both come
    /// from one run, so a wrong scale or a half-step shift in the dumps shows up here.
    #[test]
    #[ignore]
    fn a_dump_reads_the_same_field_as_the_port() {
        // A short line across a PEC box: the port's own gap, one cell high.
        let lines = |n: usize, half: f64| -> Vec<f64> { (0..=n).map(|i| -half + 2.0 * half * i as f64 / n as f64).collect() };
        let (x, z) = (lines(20, 0.02), lines(20, 0.02));
        let cell = x[1] - x[0];
        let list = |v: &[f64]| v.iter().map(|x| format!("{x:.9}")).collect::<Vec<_>>().join(",");
        let f = 1e9;
        let xml = format!(
            r#"<openEMS><FDTD NumberOfTimesteps="20000" endCriteria="1e-5" f_max="2e9"><Excitation Type="0" f0="1e9" fc="1e9"/><BoundaryCond xmin="PML_8" xmax="PML_8" ymin="PML_8" ymax="PML_8" zmin="PML_8" zmax="PML_8"/></FDTD>
            <ContinuousStructure><Properties>
            <LumpedElement Name="port_resist_1" Direction="2" Caps="1" R="50"><Primitives><Box Priority="20"><P1 X="0" Y="0" Z="0"/><P2 X="0" Y="0" Z="{c}"/></Box></Primitives></LumpedElement>
            <Excitation Name="port_excite_1" Type="0" Excite="0,0,-1"><Primitives><Box Priority="20"><P1 X="0" Y="0" Z="0"/><P2 X="0" Y="0" Z="{c}"/></Box></Primitives></Excitation>
            <ProbeBox Name="port_ut1" Type="0" Weight="-1"><Primitives><Box Priority="20"><P1 X="0" Y="0" Z="0"/><P2 X="0" Y="0" Z="{c}"/></Box></Primitives></ProbeBox>
            <DumpBox Name="E_gap" DumpType="10" DumpMode="1" FileType="0"><FD_Samples>{f:e}</FD_Samples><Primitives><Box Priority="0"><P1 X="0" Y="0" Z="0"/><P2 X="0" Y="0" Z="{c}"/></Box></Primitives></DumpBox>
            </Properties><RectilinearGrid DeltaUnit="1" CoordSystem="0"><XLines>{xs}</XLines><YLines>{xs}</YLines><ZLines>{zs}</ZLines></RectilinearGrid></ContinuousStructure></openEMS>"#,
            c = cell,
            xs = list(&x),
            zs = list(&z.iter().map(|v| v + cell * 10.0).collect::<Vec<_>>()),
        );
        let m = parse_model(&xml).unwrap();
        assert_eq!(m.problem.ports[0].width_axis, None);
        let asm = assemble(&m.problem);
        let dt = stable_dt(&asm);
        let dumps: Vec<NodeDump> = m.dumps.iter().map(|d| NodeDump { name: d.name.clone(), magnetic: d.magnetic, freqs: d.freqs.clone(), lines: dump_lines(&asm.grid, d) }).collect();
        let mut hook = |_: u64, _: f64| true;
        let (stop, _, traces, planes) = run_engine(&asm, dt, 8, Backend::Cpu, asm.grid.cells(), 1, &dumps, Some(0), m.pulse, m.end_db, m.max_steps, &mut hook, &|s| println!("  {s}")).unwrap();
        assert_ne!(stop, Stop::Diverged);
        // Line and field to one number each: the dump's two nodes at the ends of the gap's one edge.
        let plane = &planes[0];
        assert_eq!(plane.values.len(), 2);
        // Port voltage in the same transform as the dump.
        let mut u = num_complex::Complex64::new(0.0, 0.0);
        for (n, v) in traces[0].u.iter().enumerate() {
            u += num_complex::Complex64::from_polar(*v, -2.0 * std::f64::consts::PI * f * (n as f64 + 1.0) * dt);
        }
        u *= 2.0 * dt;
        // The edge has a neighbour above and below: the average at a node is half of it plus half of the
        // other one. So add the two ends: each edge counted, the gap twice, the neighbours once each.
        // Rather than guess the neighbours, take the time-domain phase and the size of the gap's field.
        let e = (plane.values[0][2] + plane.values[1][2]) * 0.5;
        // The kernel's voltage is minus the integral of E: so E across the gap is -U / length.
        let want = -u / cell;
        println!("E at the gap {e:.4e}, -U/length {want:.4e}, ratio {:.4}", e / want);
        assert!((e.arg() - want.arg()).abs() < 0.03 || (e.arg() - want.arg()).abs() > 2.0 * std::f64::consts::PI - 0.03, "phase: {e} against {want}");
        assert!((e.norm() / want.norm() - 1.0).abs() < 0.5, "size: {e} against {want}");
    }
}
