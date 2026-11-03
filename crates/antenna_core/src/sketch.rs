//! Editing a model by shape: what the Studio's drawing tools do, without the screen.
//! Everything here works in a plane's own `(u, v)`, the [`crate::geometry::plane_point`]
//! convention, in metres, and leaves the result in the project as ordinary parts and
//! ports, so what is drawn solves, saves and exports like anything else.

use anyhow::{Context, Result, anyhow, bail};

use crate::cad::{self, CadFeed, P2, signed_area};
use crate::geometry::{Geometry, P3, Shape, plane_point};
use crate::expr::Vars;
use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Form, Param, Project, extra_solid, form_outline};
use crate::{expr, material};
use std::borrow::Cow;

/// Where a shape goes and what it is made of. `at` and `thick` are expressions, so a
/// layer can follow a variable: `0`, `h`, `1.6mm`.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub axis: usize,
    pub at: String,
    pub material: String,
    pub thick: String,
}

pub fn rectangle(a: P2, b: P2) -> Vec<P2> {
    vec![a, [b[0], a[1]], b, [a[0], b[1]]]
}

/// A circle whose chords stay within `chord` of the true curve.
pub fn circle(c: P2, r: f64, chord: f64) -> Vec<P2> {
    let step = 2.0 * (1.0 - (chord / r.max(1e-12)).min(0.5)).acos();
    let n = ((std::f64::consts::TAU / step.max(1e-3)).ceil() as usize).clamp(12, 96);
    (0..n).map(|k| { let t = std::f64::consts::TAU * k as f64 / n as f64; [c[0] + r * t.cos(), c[1] + r * t.sin()] }).collect()
}

/// A strip `w` wide along a path.
pub fn strip(path: &[P2], w: f64) -> Result<Vec<P2>> {
    if !(w > 0.0) {
        bail!("A strip needs a width above zero.");
    }
    cad::stroke(path, w, false).into_iter().next().ok_or_else(|| anyhow!("A strip needs two different points."))
}

/// The first free `base 1`, `base 2`, …
pub fn free_name(p: &Project, base: &str) -> String {
    let taken = |n: &str| p.extra.iter().any(|e| e.name == n) || p.extra_ports.iter().any(|e| e.name == n);
    (1..).map(|k| format!("{base} {k}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

pub fn height_of(p: &Project, at: &str) -> Result<f64> {
    let vars = p.vars()?;
    expr::eval(at, &vars).with_context(|| format!("the height '{at}'"))
}

fn cell_for(pts: &[P2]) -> f64 {
    let n = cad::narrowest_of(std::slice::from_ref(&pts.to_vec()));
    if n.is_finite() { n / 3.0 } else { 0.0 }
}

/// Adds a closed outline as a new part and returns its name.
pub fn add_shape(p: &mut Project, base: &str, sheet: &Sheet, pts: Vec<P2>) -> Result<String> {
    if pts.len() < 3 {
        bail!("A shape needs at least three different points.");
    }
    if signed_area(&pts).abs() <= 0.0 {
        bail!("That shape has no area.");
    }
    height_of(p, &sheet.at)?;
    let vars = p.vars()?;
    expr::eval(&sheet.thick, &vars).with_context(|| format!("the thickness '{}'", sheet.thick))?;
    if material::by_name(&sheet.material).is_none() {
        bail!("There's no material called {}.", sheet.material);
    }
    let name = free_name(p, base);
    let cell = cell_for(&pts);
    p.extra.push(ExtraSolid { name: name.clone(), material: sheet.material.clone(), prim: ExtraPrim::Outline { file: "drawn".into(), axis: sheet.axis, at: sheet.at.clone(), pts, thick: sheet.thick.clone(), cell } });
    Ok(name)
}

/// A length as an expression a person can read and edit: `12.5mm`, good to a picometre.
pub fn mm(v: f64) -> String {
    let rounded = (v * 1e12).round() / 1e9;
    if rounded == 0.0 { "0".to_string() } else { format!("{rounded}mm") }
}

fn mm2(q: P2) -> [String; 2] {
    [mm(q[0]), mm(q[1])]
}

/// A plain number or length, with no variable in it: dragging can rewrite it. One that uses
/// a variable says what it's tied to, so the mouse leaves it alone.
pub fn is_free(e: &str) -> bool {
    expr::names(e).is_ok_and(|n| n.is_empty())
}

fn check_sheet(p: &Project, sheet: &Sheet) -> Result<()> {
    height_of(p, &sheet.at)?;
    let vars = p.vars()?;
    expr::eval(&sheet.thick, &vars).with_context(|| format!("the thickness '{}'", sheet.thick))?;
    if material::by_name(&sheet.material).is_none() {
        bail!("There's no material called {}.", sheet.material);
    }
    Ok(())
}

fn add_form(p: &mut Project, base: &str, sheet: &Sheet, form: Form) -> Result<String> {
    check_sheet(p, sheet)?;
    let name = free_name(p, base);
    p.extra.push(ExtraSolid { name: name.clone(), material: sheet.material.clone(), prim: ExtraPrim::Drawn { axis: sheet.axis, at: sheet.at.clone(), thick: sheet.thick.clone(), form, holes: Vec::new() } });
    Ok(name)
}

/// A rectangle from two corners, kept as its centre and sides.
pub fn add_rect(p: &mut Project, base: &str, sheet: &Sheet, a: P2, b: P2) -> Result<String> {
    if a[0] == b[0] || a[1] == b[1] {
        bail!("A rectangle needs two corners that differ both ways.");
    }
    add_form(p, base, sheet, Form::Rect { c: mm2([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]), size: mm2([(a[0] - b[0]).abs(), (a[1] - b[1]).abs()]) })
}

pub fn add_circle(p: &mut Project, base: &str, sheet: &Sheet, c: P2, r: f64) -> Result<String> {
    if !(r > 0.0) {
        bail!("A circle needs a radius above zero.");
    }
    add_form(p, base, sheet, Form::Circle { c: mm2(c), r: mm(r) })
}

pub fn add_poly(p: &mut Project, base: &str, sheet: &Sheet, pts: &[P2]) -> Result<String> {
    if pts.len() < 3 {
        bail!("A shape needs at least three different points.");
    }
    if signed_area(pts).abs() <= 0.0 {
        bail!("That shape has no area.");
    }
    add_form(p, base, sheet, Form::Poly { pts: pts.iter().map(|q| mm2(*q)).collect() })
}

/// A trace along a path. Its width stays a dimension when it's flat metal; a dielectric strip
/// has thickness, which only an outline carries.
pub fn add_strip(p: &mut Project, base: &str, sheet: &Sheet, path: &[P2], w: f64) -> Result<String> {
    let ring = strip(path, w)?;
    let vars = p.vars()?;
    if !is_metal(&sheet.material) || expr::eval(&sheet.thick, &vars).is_ok_and(|t| t != 0.0) {
        return add_shape(p, base, sheet, ring);
    }
    check_sheet(p, sheet)?;
    let name = free_name(p, base);
    p.extra.push(ExtraSolid { name: name.clone(), material: sheet.material.clone(), prim: ExtraPrim::Strip { axis: sheet.axis, at: sheet.at.clone(), path: path.iter().map(|q| mm2(*q)).collect(), w: mm(w) } });
    Ok(name)
}

/// A new row in the variable table. Refused if the name is taken or isn't one an expression
/// can use, or if the value doesn't work out.
pub fn add_variable(p: &mut Project, name: &str, value: &str, note: &str) -> Result<()> {
    let name = name.trim();
    if expr::names(name).ok().as_deref() != Some(&[name.to_string()][..]) {
        bail!("'{name}' can't be a variable's name: use letters, digits and _, starting with a letter.");
    }
    if p.var_hints().iter().any(|h| h.name == name) {
        bail!("There's already a variable called {name}.");
    }
    p.params.push(Param::new(name, value, note));
    if let Err(e) = p.vars() {
        p.params.pop();
        return Err(e.context(format!("the value of {name}")));
    }
    Ok(())
}

/// A flat part as it comes out, in its plane.
pub struct FlatPart<'a> {
    pub index: usize,
    pub axis: usize,
    pub at: f64,
    pub outline: Cow<'a, [P2]>,
}

/// Every part the canvas can show and edit, in the project's order. A part whose expression
/// doesn't work out just isn't there until it does.
pub fn flat_parts<'a>(p: &'a Project, vars: &Vars) -> Vec<FlatPart<'a>> {
    p.extra
        .iter()
        .enumerate()
        .filter_map(|(index, e)| match &e.prim {
            ExtraPrim::Outline { axis, at, pts, .. } => Some(FlatPart { index, axis: *axis, at: expr::eval(at, vars).ok()?, outline: Cow::Borrowed(pts.as_slice()) }),
            ExtraPrim::Drawn { .. } | ExtraPrim::Polygon { .. } | ExtraPrim::Plate { .. } | ExtraPrim::Strip { .. } | ExtraPrim::Disc { .. } | ExtraPrim::Arc { .. } | ExtraPrim::Meander { .. } => match extra_solid(e, vars).ok()?.shape {
                Shape::Polygon { axis, at, pts, .. } => Some(FlatPart { index, axis, at, outline: Cow::Owned(pts) }),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn point_at(q: &[String; 2], v: &Vars) -> Option<P2> {
    Some([expr::eval(&q[0], v).ok()?, expr::eval(&q[1], v).ok()?])
}

fn near(a: P2, b: P2) -> bool {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1e-9
}

/// The corners and other points of a part that can be grabbed on the canvas.
pub fn handles(p: &Project, part: usize) -> Vec<P2> {
    let Ok(v) = p.vars() else { return Vec::new() };
    let Some(e) = p.extra.get(part) else { return Vec::new() };
    let list = |l: &[[String; 2]]| l.iter().filter_map(|q| point_at(q, &v)).collect::<Vec<_>>();
    let rim = |c: &[String; 2], r: &String| -> Vec<P2> {
        let (Some(c), Ok(r)) = (point_at(c, &v), expr::eval(r, &v)) else { return Vec::new() };
        vec![c, [c[0] + r, c[1]]]
    };
    let mut out = match &e.prim {
        ExtraPrim::Outline { pts, .. } => pts.clone(),
        ExtraPrim::Drawn { form, holes, .. } => {
            let mut h = match form {
                Form::Rect { .. } => form_outline(&e.name, form, &v).unwrap_or_default(),
                Form::Circle { c, r } => rim(c, r),
                Form::Poly { pts } => list(pts),
            };
            holes.iter().for_each(|o| h.extend(list(o)));
            h
        }
        ExtraPrim::Polygon { pts, .. } => list(pts),
        ExtraPrim::Plate { outline, holes, .. } => {
            let mut h = list(outline);
            holes.iter().for_each(|o| h.extend(list(o)));
            h
        }
        ExtraPrim::Strip { path, .. } => list(path),
        ExtraPrim::Disc { c, r, .. } => rim(c, r),
        ExtraPrim::Arc { c, .. } => list(std::slice::from_ref(c)),
        ExtraPrim::Meander { start, .. } => list(std::slice::from_ref(start)),
        _ => Vec::new(),
    };
    let mut seen: Vec<P2> = Vec::with_capacity(out.len());
    out.retain(|q| {
        let new = !seen.iter().any(|s| near(*s, *q));
        if new {
            seen.push(*q);
        }
        new
    });
    out
}

/// Whether any of a part's coordinates or dimensions is written in a variable.
pub fn is_tied(p: &Project, part: usize) -> bool {
    let Some(mut prim) = p.extra.get(part).map(|e| e.prim.clone()) else { return false };
    let pts = points_of(&mut prim).iter().any(|q| !is_free(&q[0]) || !is_free(&q[1]));
    pts || match &prim {
        ExtraPrim::Drawn { form: Form::Rect { size, .. }, .. } => size.iter().any(|s| !is_free(s)),
        ExtraPrim::Drawn { form: Form::Circle { r, .. }, .. } | ExtraPrim::Disc { r, .. } => !is_free(r),
        _ => false,
    }
}

/// The coordinates of a part that move with it, in plain expression pairs.
fn points_of(prim: &mut ExtraPrim) -> Vec<&mut [String; 2]> {
    match prim {
        ExtraPrim::Drawn { form, holes, .. } => {
            let mut out: Vec<&mut [String; 2]> = match form {
                Form::Rect { c, .. } | Form::Circle { c, .. } => vec![c],
                Form::Poly { pts } => pts.iter_mut().collect(),
            };
            out.extend(holes.iter_mut().flatten());
            out
        }
        ExtraPrim::Polygon { pts, .. } => pts.iter_mut().collect(),
        ExtraPrim::Plate { outline, holes, .. } => {
            let mut out: Vec<&mut [String; 2]> = outline.iter_mut().collect();
            out.extend(holes.iter_mut().flatten());
            out
        }
        ExtraPrim::Strip { path, .. } => path.iter_mut().collect(),
        ExtraPrim::Disc { c, .. } | ExtraPrim::Arc { c, .. } => vec![c],
        ExtraPrim::Meander { start, .. } => vec![start],
        _ => Vec::new(),
    }
}

/// Moves one point of a pair, each coordinate only if it's free. Gives where it ended up.
fn move_pair(q: &mut [String; 2], at: P2, to: P2) -> P2 {
    let mut out = at;
    for k in 0..2 {
        if is_free(&q[k]) {
            q[k] = mm(to[k]);
            out[k] = to[k];
        }
    }
    out
}

/// A corner of a drawn rectangle: the one across from it stays where it is.
fn drag_rect(c: &mut [String; 2], size: &mut [String; 2], v: &Vars, from: P2, to: P2) -> Option<P2> {
    let (cc, ss) = (point_at(c, v)?, point_at(size, v)?);
    let side = |k: usize| [-1.0, 1.0].into_iter().find(|s| (cc[k] + s * ss[k] / 2.0 - from[k]).abs() < 1e-9);
    let sides = [side(0)?, side(1)?];
    let mut out = from;
    for k in 0..2 {
        let across = cc[k] - sides[k] * ss[k] / 2.0;
        let span = (to[k] - across).abs();
        if is_free(&c[k]) && is_free(&size[k]) && span >= 1e-6 {
            c[k] = mm((to[k] + across) / 2.0);
            size[k] = mm(span);
            out[k] = to[k];
        }
    }
    Some(out)
}

/// A rectangle as four corners that still follow its dimensions.
fn rect_corners(c: &[String; 2], size: &[String; 2], v: &Vars) -> Vec<[String; 2]> {
    let at = |k: usize, s: f64| -> String {
        if is_free(&c[k]) && is_free(&size[k])
            && let (Ok(cv), Ok(sv)) = (expr::eval(&c[k], v), expr::eval(&size[k], v))
        {
            mm(cv + s * sv / 2.0)
        } else if s < 0.0 {
            format!("({})-({})/2", c[k], size[k])
        } else {
            format!("({})+({})/2", c[k], size[k])
        }
    };
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].into_iter().map(|(a, b)| [at(0, a), at(1, b)]).collect()
}

/// An imported or template outline turned into a drawn part, so its corners take expressions.
/// A plain rectangle becomes one, with a centre and sides.
pub fn make_drawn(p: &mut Project, part: usize) -> Result<()> {
    let Some(e) = p.extra.get_mut(part) else { bail!("That part is gone.") };
    let ExtraPrim::Outline { axis, at, pts, thick, .. } = &e.prim else { bail!("Only an outline needs this.") };
    let square = pts.len() == 4 && (0..4).all(|k| {
        let (a, b) = (pts[k], pts[(k + 1) % 4]);
        (a[0] == b[0]) != (a[1] == b[1])
    });
    let form = if square {
        let lo = [pts.iter().map(|q| q[0]).fold(f64::MAX, f64::min), pts.iter().map(|q| q[1]).fold(f64::MAX, f64::min)];
        let hi = [pts.iter().map(|q| q[0]).fold(f64::MIN, f64::max), pts.iter().map(|q| q[1]).fold(f64::MIN, f64::max)];
        Form::Rect { c: mm2([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]), size: mm2([hi[0] - lo[0], hi[1] - lo[1]]) }
    } else {
        Form::Poly { pts: pts.iter().map(|q| mm2(*q)).collect() }
    };
    e.prim = ExtraPrim::Drawn { axis: *axis, at: at.clone(), thick: thick.clone(), form, holes: Vec::new() };
    Ok(())
}

/// Parts the tools can edit, in the project's order: `(index, axis, height expression, outline)`.
pub fn outlines(p: &Project) -> Vec<(usize, usize, &str, &[P2])> {
    p.extra
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.prim {
            ExtraPrim::Outline { axis, at, pts, .. } => Some((i, *axis, at.as_str(), pts.as_slice())),
            _ => None,
        })
        .collect()
}

fn is_metal(name: &str) -> bool {
    material::by_name(name).is_none_or(|m| m.is_conductor())
}

/// Cuts a hole in the metal under it and returns the part it cut.
pub fn cut_in(p: &mut Project, axis: usize, at: f64, hole: &[P2]) -> Result<String> {
    if hole.len() < 3 {
        bail!("A cut needs at least three different points.");
    }
    let vars = p.vars()?;
    let found = flat_parts(p, &vars).iter().find(|f| f.axis == axis && (f.at - at).abs() < 1e-9 && is_metal(&p.extra[f.index].material) && hole.iter().all(|q| cad::point_inside(&f.outline, *q))).map(|f| f.index);
    let Some(i) = found else {
        bail!("A cut has to sit wholly inside one shape on this plane. A template's parts need making editable first.");
    };
    let ring: Vec<[String; 2]> = hole.iter().map(|q| mm2(*q)).collect();
    let name = p.extra[i].name.clone();
    match &mut p.extra[i].prim {
        ExtraPrim::Outline { pts, cell, .. } => {
            *pts = cad::cut_hole(pts, hole);
            let finer = cell_for(hole);
            if finer > 0.0 && (*cell <= 0.0 || finer < *cell) {
                *cell = finer;
            }
        }
        ExtraPrim::Drawn { holes, .. } | ExtraPrim::Plate { holes, .. } => holes.push(ring),
        // These two have nowhere to keep a hole: they become parts that do, dimensions intact.
        ExtraPrim::Polygon { axis, at, pts, thick } => {
            p.extra[i].prim = ExtraPrim::Drawn { axis: *axis, at: at.clone(), thick: thick.clone(), form: Form::Poly { pts: pts.clone() }, holes: vec![ring] };
        }
        ExtraPrim::Disc { axis, at, c, r } => {
            p.extra[i].prim = ExtraPrim::Drawn { axis: *axis, at: at.clone(), thick: "0".into(), form: Form::Circle { c: c.clone(), r: r.clone() }, holes: vec![ring] };
        }
        _ => bail!("{name} is a trace, and a trace can't hold a cut. Draw the plate as a shape instead."),
    }
    Ok(name)
}

/// The flat metal of a model on one plane, as outlines in the plane's `(u, v)`.
pub fn flat_metal(g: &Geometry, axis: usize, at: f64) -> Vec<Vec<P2>> {
    let mut out = Vec::new();
    for s in g.solids.iter().filter(|s| is_metal(&s.material)) {
        match &s.shape {
            Shape::Box { min, max } if max[axis] - min[axis] <= 0.0 && (min[axis] - at).abs() < 1e-9 => {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                out.push(rectangle([min[u], min[v]], [max[u], max[v]]));
            }
            Shape::Polygon { axis: a, at: h, pts, thick } if *a == axis && *thick <= 0.0 && (*h - at).abs() < 1e-9 => out.push(pts.clone()),
            _ => {}
        }
    }
    out
}

/// A second point pulled onto a line along u or v through the first: ports lie along an axis.
pub fn along_axis(a: P2, b: P2) -> P2 {
    if (b[0] - a[0]).abs() >= (b[1] - a[1]).abs() { [b[0], a[1]] } else { [a[0], b[1]] }
}

fn ensure_z0(p: &mut Project) {
    if !p.params.iter().any(|r| r.name == "Z0") {
        p.params.push(Param::new("Z0", "50", "Port impedance"));
    }
}

fn num3(q: P3) -> [String; 3] {
    [mm(q[0]), mm(q[1]), mm(q[2])]
}

/// Where the metal stops, going from `from` towards `to` along the line through them.
fn metal_edge(all: &[Vec<P2>], from: P2, to: P2) -> Option<P2> {
    let axis = if (to[0] - from[0]).abs() >= (to[1] - from[1]).abs() { 0 } else { 1 };
    let across = 1 - axis;
    let sign = (to[axis] - from[axis]).signum();
    let hit = all
        .iter()
        .flat_map(|o| cad::line_hits(o, axis, from[across]))
        .filter(|h| (h - from[axis]) * sign > 0.0)
        .min_by(|x, y| (x - from[axis]).abs().total_cmp(&(y - from[axis]).abs()))?;
    let mut q = from;
    q[axis] = hit;
    Some(q)
}

/// Feeds across the gap between `a` and `b`, or across a line on unbroken metal. The
/// line is pulled onto an axis and its ends onto the metal. Returns the port's name.
pub fn add_feed(p: &mut Project, axis: usize, at: &str, a: P2, b: P2) -> Result<String> {
    let g = p.scatterer()?;
    let h = height_of(p, at)?;
    let metal = flat_metal(&g, axis, h);
    if metal.is_empty() {
        bail!("There's no flat metal on this plane to feed. Draw it first, or choose the plane it's on.");
    }
    let b = along_axis(a, b);
    if a == b {
        bail!("A feed needs two different points.");
    }
    let on = |q: P2| metal.iter().any(|o| cad::point_inside(o, q));
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let (pa, pb, width) = if on(a) && on(b) && on(mid) {
        // Unbroken metal: the port is a cut across it, as wide as the metal there.
        (a, b, 0.0)
    } else {
        // An end drawn on the metal means the gap starts where that metal stops.
        let start = if on(a) { metal_edge(&metal, a, b).unwrap_or(a) } else { a };
        let end = if on(b) { metal_edge(&metal, b, a).unwrap_or(b) } else { b };
        let layer = cad::sheet_layer("this plane", metal);
        match cad::gap_feed(&[layer], [start, end]).map_err(|e| anyhow!("{}", e.replace("feed line", "feed")))? {
            CadFeed::Gap { a, b, width, .. } => (a, b, width),
            CadFeed::Probe { .. } => bail!("That isn't a gap."),
        }
    };
    ensure_z0(p);
    let name = free_name(p, "Feed");
    let end = |q: P2| {
        let mut s = num3(plane_point(axis, 0.0, q));
        s[axis] = at.to_string();
        s
    };
    p.extra_ports.push(ExtraPort { name: name.clone(), a: end(pa), b: end(pb), width: mm(width), z0: "Z0".into() });
    Ok(name)
}

/// A probe at `(u, v)` between the lowest and highest metal over that point, along
/// the plane's axis. Returns the port's name.
pub fn add_probe(p: &mut Project, axis: usize, at: P2) -> Result<String> {
    let g = p.scatterer()?;
    let mut heights: Vec<f64> = Vec::new();
    if axis == 2 && g.ground {
        heights.push(0.0);
    }
    for s in g.solids.iter().filter(|s| is_metal(&s.material)) {
        let here = match &s.shape {
            Shape::Box { min, max } if max[axis] - min[axis] <= 0.0 => {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                (at[0] >= min[u] && at[0] <= max[u] && at[1] >= min[v] && at[1] <= max[v]).then_some(min[axis])
            }
            Shape::Polygon { axis: a, at: h, pts, thick } if *a == axis && *thick <= 0.0 => cad::point_inside(pts, at).then_some(*h),
            _ => None,
        };
        heights.extend(here);
    }
    heights.sort_by(f64::total_cmp);
    heights.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let (Some(lo), Some(hi)) = (heights.first().copied(), heights.last().copied()) else {
        bail!("A probe joins two sheets, and this point has no metal over it.");
    };
    if heights.len() < 2 {
        bail!("A probe joins two sheets, and this point has metal at only one height.");
    }
    ensure_z0(p);
    let name = free_name(p, "Feed");
    p.extra_ports.push(ExtraPort { name: name.clone(), a: num3(plane_point(axis, lo, at)), b: num3(plane_point(axis, hi, at)), width: "0".into(), z0: "Z0".into() });
    Ok(name)
}

/// The nearest editable part under a ray, and the point it hit, in its plane. Where
/// shapes overlap on one plane the smaller wins, so a patch on a ground can be picked.
pub fn pick(p: &Project, origin: P3, dir: P3) -> Option<(usize, P2)> {
    let vars = p.vars().ok()?;
    let mut best: Option<(f64, f64, usize, P2)> = None;
    for f in flat_parts(p, &vars) {
        let (axis, h) = (f.axis, f.at);
        if dir[axis].abs() < 1e-6 {
            continue;
        }
        let t = (h - origin[axis]) / dir[axis];
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let q = [origin[u] + dir[u] * t, origin[v] + dir[v] * t];
        if !cad::point_inside(&f.outline, q) {
            continue;
        }
        let area = signed_area(&f.outline).abs();
        let nearer = best.as_ref().is_none_or(|b| t < b.0 - 1e-9 || ((t - b.0).abs() <= 1e-9 && area < b.1));
        if nearer {
            best = Some((t, area, f.index, q));
        }
    }
    best.map(|b| (b.2, b.3))
}

fn outline_mut(p: &mut Project, part: usize) -> Option<&mut Vec<P2>> {
    match &mut p.extra.get_mut(part)?.prim {
        ExtraPrim::Outline { pts, .. } => Some(pts),
        _ => None,
    }
}

/// Moves the corner at `from`, and any keyhole twin sitting on it, towards `to`. A coordinate
/// tied to a variable stays where the variable puts it. Gives where the corner ended up.
pub fn move_corner(p: &mut Project, part: usize, from: P2, to: P2) -> P2 {
    if let Some(pts) = outline_mut(p, part) {
        for q in pts.iter_mut().filter(|q| ((q[0] - from[0]).abs() + (q[1] - from[1]).abs()) < 1e-12) {
            *q = to;
        }
        return to;
    }
    let Ok(v) = p.vars() else { return from };
    let Some(e) = p.extra.get_mut(part) else { return from };
    let rim = |c: &[String; 2], r: &mut String| -> Option<P2> {
        let (cc, rr) = (point_at(c, &v)?, expr::eval(r, &v).ok()?);
        if !near(from, [cc[0] + rr, cc[1]]) {
            return None;
        }
        if !is_free(r) {
            return Some(from);
        }
        let new = (to[0] - cc[0]).hypot(to[1] - cc[1]).max(1e-6);
        *r = mm(new);
        Some([cc[0] + new, cc[1]])
    };
    // A rectangle's corners and a circle's rim are worked out, not stored.
    match &mut e.prim {
        ExtraPrim::Drawn { form: Form::Rect { c, size }, .. } => {
            if let Some(out) = drag_rect(c, size, &v, from, to) {
                return out;
            }
        }
        ExtraPrim::Drawn { form: Form::Circle { c, r }, .. } | ExtraPrim::Disc { c, r, .. } => {
            if let Some(out) = rim(c, r) {
                return out;
            }
        }
        _ => {}
    }
    let mut out = from;
    for q in points_of(&mut e.prim) {
        if point_at(q, &v).is_some_and(|at| near(at, from)) {
            out = move_pair(q, from, to);
        }
    }
    out
}

/// Moves a whole part. Along an axis where any of its points is tied to a variable it stays,
/// rather than coming apart. Gives how far it really went.
pub fn move_part(p: &mut Project, part: usize, by: P2) -> P2 {
    if let Some(pts) = outline_mut(p, part) {
        for q in pts.iter_mut() {
            *q = [q[0] + by[0], q[1] + by[1]];
        }
        return by;
    }
    let Ok(v) = p.vars() else { return [0.0, 0.0] };
    let Some(e) = p.extra.get_mut(part) else { return [0.0, 0.0] };
    let mut pts = points_of(&mut e.prim);
    let mut out = by;
    for k in 0..2 {
        if pts.is_empty() || pts.iter().any(|q| !is_free(&q[k])) {
            out[k] = 0.0;
        }
    }
    for q in &mut pts {
        for k in 0..2 {
            if out[k] != 0.0
                && let Ok(now) = expr::eval(&q[k], &v)
            {
                q[k] = mm(now + out[k]);
            }
        }
    }
    out
}

fn segment_distance(q: P2, a: P2, b: P2) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let t = (((q[0] - a[0]) * d[0] + (q[1] - a[1]) * d[1]) / (d[0] * d[0] + d[1] * d[1]).max(1e-30)).clamp(0.0, 1.0);
    (q[0] - a[0] - d[0] * t).hypot(q[1] - a[1] - d[1] * t)
}

/// A rectangle gets corners of its own, for a change only corners can make.
fn rect_to_poly(p: &mut Project, part: usize) {
    let Ok(v) = p.vars() else { return };
    if let Some(ExtraPrim::Drawn { form, .. }) = p.extra.get_mut(part).map(|e| &mut e.prim) {
        let corners = match &*form {
            Form::Rect { c, size } => rect_corners(c, size, &v),
            _ => return,
        };
        *form = Form::Poly { pts: corners };
    }
}

/// The list of corners a new corner or a removal works on: the outline, not its holes.
fn corner_list(prim: &mut ExtraPrim) -> Option<(&mut Vec<[String; 2]>, bool)> {
    match prim {
        ExtraPrim::Drawn { form: Form::Poly { pts }, .. } | ExtraPrim::Polygon { pts, .. } => Some((pts, true)),
        ExtraPrim::Plate { outline, .. } => Some((outline, true)),
        ExtraPrim::Strip { path, .. } => Some((path, false)),
        _ => None,
    }
}

/// Puts a new corner on the edge after corner `edge`, or for a part kept in dimensions on the
/// edge nearest `at`. A circle has no corners to add to.
pub fn insert_corner(p: &mut Project, part: usize, edge: usize, at: P2) {
    if let Some(pts) = outline_mut(p, part) {
        if edge < pts.len() {
            pts.insert(edge + 1, at);
        }
        return;
    }
    rect_to_poly(p, part);
    let Ok(v) = p.vars() else { return };
    let Some(e) = p.extra.get_mut(part) else { return };
    let Some((list, closed)) = corner_list(&mut e.prim) else { return };
    let corners: Vec<P2> = list.iter().filter_map(|q| point_at(q, &v)).collect();
    if corners.len() != list.len() || corners.len() < 2 {
        return;
    }
    let edges = if closed { corners.len() } else { corners.len() - 1 };
    let nearest = (0..edges).min_by(|x, y| {
        let d = |k: usize| segment_distance(at, corners[k], corners[(k + 1) % corners.len()]);
        d(*x).total_cmp(&d(*y))
    });
    if let Some(k) = nearest {
        list.insert(k + 1, mm2(at));
    }
}

/// Removes a corner, unless it's a keyhole's or too few would be left.
pub fn remove_corner(p: &mut Project, part: usize, at: P2) -> Result<()> {
    if let Some(pts) = outline_mut(p, part) {
        let same = |q: &P2| ((q[0] - at[0]).abs() + (q[1] - at[1]).abs()) < 1e-12;
        if pts.iter().filter(|q| same(q)).count() > 1 {
            bail!("This corner joins a hole to its outline, so it has to stay.");
        }
        if pts.len() <= 3 {
            bail!("A shape needs at least three corners.");
        }
        pts.retain(|q| !same(q));
        return Ok(());
    }
    rect_to_poly(p, part);
    let v = p.vars()?;
    let Some(e) = p.extra.get_mut(part) else { bail!("That part is gone.") };
    let Some((list, closed)) = corner_list(&mut e.prim) else { bail!("This part's shape comes from its dimensions, so it has no corners to remove.") };
    let least = if closed { 3 } else { 2 };
    if list.len() <= least {
        bail!("{} needs at least {least} corners.", if closed { "A shape" } else { "A strip" });
    }
    let Some(k) = list.iter().position(|q| point_at(q, &v).is_some_and(|c| near(c, at))) else { bail!("There's no corner there to remove: a hole's corners stay.") };
    list.remove(k);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::C64;
    use crate::mom::{MomOptions, MomProblem};

    fn sheet(axis: usize, at: &str) -> Sheet {
        Sheet { axis, at: at.into(), material: "PEC".into(), thick: "0".into() }
    }

    fn solve_z(p: &Project, f: f64, edges: f64) -> C64 {
        let g = p.geometry().unwrap();
        let opts = MomOptions { edges_per_lambda: edges, ..Default::default() };
        MomProblem::new(&g, f, opts).unwrap().solve(f, false).unwrap().z[0][0]
    }

    fn set(p: &mut Project, name: &str, expr: &str) {
        match p.params.iter_mut().find(|r| r.name == name) {
            Some(r) => r.expr = expr.into(),
            None => p.params.push(Param::new(name, expr, "")),
        }
    }

    /// Drawn the way a person would: two rectangles, then a feed clicked between them.
    #[test]
    fn a_dipole_drawn_by_hand_solves_like_the_template() {
        let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let v = t.vars().unwrap();
        let (l, w, g) = (v["L"], v["w"], v["g"]);
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        set(&mut p, "Z0", "73");
        // On the plane y = 0, whose (u, v) is (z, x).
        let s = sheet(1, "0");
        add_shape(&mut p, "Arm", &s, rectangle([g / 2.0, -w / 2.0], [l / 2.0, w / 2.0])).unwrap();
        add_shape(&mut p, "Arm", &s, rectangle([-l / 2.0, -w / 2.0], [-g / 2.0, w / 2.0])).unwrap();
        // Clicked from one arm to the other, a little off-axis: the feed finds the gap.
        let name = add_feed(&mut p, 1, "0", [-g * 2.0, 0.0], [g * 2.0, 0.0005]).unwrap();
        assert_eq!(name, "Feed 1");
        let port = &p.geometry().unwrap().ports[0];
        assert!((port.a[2] + g / 2.0).abs() < 1e-9 && (port.b[2] - g / 2.0).abs() < 1e-9, "{port:?}");
        assert!((port.width - w).abs() < 1e-6, "{port:?}");
        let (zt, zd) = (solve_z(&t, 1e9, 14.0), solve_z(&p, 1e9, 14.0));
        println!("template {zt:.2} Ω, drawn {zd:.2} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.02, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn a_slot_cut_by_hand_solves_like_the_template() {
        let t = Project::from_template("slot", Some("2.45GHz")).unwrap();
        let v = t.vars().unwrap();
        let (gx, gy, ls, ws) = (v["Gx"], v["Gy"], v["Ls"], v["ws"]);
        let mut p = Project::from_template("custom", Some("2.45GHz")).unwrap();
        set(&mut p, "Z0", "500");
        let s = sheet(2, "0");
        add_shape(&mut p, "Plate", &s, rectangle([-gx / 2.0, -gy / 2.0], [gx / 2.0, gy / 2.0])).unwrap();
        let cut = cut_in(&mut p, 2, 0.0, &rectangle([-ws / 2.0, -ls / 2.0], [ws / 2.0, ls / 2.0])).unwrap();
        assert_eq!(cut, "Plate 1");
        // Clicked across the slot from metal to metal.
        add_feed(&mut p, 2, "0", [-ws, 0.0], [ws, 0.0]).unwrap();
        let wp = v["wp"];
        p.extra_ports[0].width = format!("{wp:e}");
        let edges = t.setup.edges_per_lambda;
        let (zt, zd) = (solve_z(&t, 2.45e9, edges), solve_z(&p, 2.45e9, edges));
        println!("template {zt:.1} Ω, drawn {zd:.1} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.08, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn a_patch_with_a_clicked_probe_solves_like_the_template() {
        let t = Project::from_template("patch_air", Some("2.45GHz")).unwrap();
        let g = t.geometry().unwrap();
        let tp = &g.ports[0];
        let mut p = Project::from_template("custom", Some("2.45GHz")).unwrap();
        let z0 = g.ports[0].z0;
        set(&mut p, "Z0", &format!("{z0}"));
        set(&mut p, "h", &format!("{:e}", tp.b[2]));
        for (name, s) in g.solids.iter().map(|s| (s.name.clone(), s)) {
            let Shape::Box { min, max } = &s.shape else { continue };
            let at = if min[2] > 0.0 { "h" } else { "0" };
            add_shape(&mut p, &name, &sheet(2, at), rectangle([min[0], min[1]], [max[0], max[1]])).unwrap();
        }
        assert_eq!(p.extra.len(), g.solids.len());
        add_probe(&mut p, 2, [tp.a[0], tp.a[1]]).unwrap();
        p.extra_ports[0].width = format!("{:e}", tp.width);
        let (zt, zd) = (solve_z(&t, 2.45e9, 12.0), solve_z(&p, 2.45e9, 12.0));
        println!("template {zt:.1} Ω, drawn {zd:.1} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.05, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn feeds_and_probes_say_what_is_wrong() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        assert!(add_feed(&mut p, 2, "0", [0.0, 0.0], [0.01, 0.0]).is_err(), "nothing drawn yet");
        add_shape(&mut p, "Plate", &sheet(2, "0"), rectangle([0.0, 0.0], [0.1, 0.1])).unwrap();
        let e = add_probe(&mut p, 2, [0.05, 0.05]).unwrap_err().to_string();
        assert!(e.contains("only one height"), "{e}");
        let e = add_probe(&mut p, 2, [0.5, 0.5]).unwrap_err().to_string();
        assert!(e.contains("no metal"), "{e}");
        let e = add_feed(&mut p, 2, "0", [0.3, 0.3], [0.4, 0.3]).unwrap_err().to_string();
        assert!(e.contains("metal"), "{e}");
        // On unbroken metal the port is a line across it.
        add_feed(&mut p, 2, "0", [0.05, 0.04], [0.05, 0.06]).unwrap();
        assert_eq!(p.geometry().unwrap().ports.len(), 1);
        let opts = MomOptions { edges_per_lambda: 12.0, ..Default::default() };
        assert!(MomProblem::new(&p.geometry().unwrap(), 1e9, opts).is_ok());
    }

    #[test]
    fn a_strip_meander_is_one_piece_with_the_right_area() {
        let path = [[0.0, 0.0], [0.02, 0.0], [0.02, 0.01], [0.0, 0.01], [0.0, 0.02], [0.02, 0.02]];
        let pts = strip(&path, 0.002).unwrap();
        // Length of the centre line times the width.
        let want = (0.02 + 0.01 + 0.02 + 0.01 + 0.02) * 0.002;
        assert!((signed_area(&pts).abs() - want).abs() / want < 1e-6, "{}", signed_area(&pts));
        assert!(strip(&path, 0.0).is_err());
        assert!(strip(&[[0.0, 0.0]], 0.002).is_err());
    }

    #[test]
    fn corners_move_with_their_keyhole_twins_and_stay_valid() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut p, "Plate", &sheet(2, "0"), rectangle([0.0, 0.0], [0.1, 0.1])).unwrap();
        cut_in(&mut p, 2, 0.0, &rectangle([0.04, 0.04], [0.06, 0.06])).unwrap();
        let pts = outlines(&p)[0].3.to_vec();
        let twin = *pts.iter().find(|q| pts.iter().filter(|r| r == q).count() == 2).unwrap();
        let before = signed_area(&pts).abs();
        assert!(remove_corner(&mut p, 0, twin).is_err(), "a keyhole's corner stays");
        move_corner(&mut p, 0, twin, [twin[0] + 0.001, twin[1]]);
        let moved = outlines(&p)[0].3.to_vec();
        assert_eq!(moved.iter().filter(|q| **q == [twin[0] + 0.001, twin[1]]).count(), 2);
        assert!((signed_area(&moved).abs() - before).abs() < 2e-4);
        move_part(&mut p, 0, [0.01, 0.02]);
        let (lo, _) = p.scatterer().unwrap().bounds();
        assert!((lo[0] - 0.01).abs() < 1e-9 && (lo[1] - 0.02).abs() < 1e-9, "{lo:?}");
        insert_corner(&mut p, 0, 0, [0.05, 0.015]);
        assert_eq!(outlines(&p)[0].3.len(), moved.len() + 1);
        let mut q = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut q, "T", &sheet(2, "0"), vec![[0.0, 0.0], [0.1, 0.0], [0.0, 0.1]]).unwrap();
        assert!(remove_corner(&mut q, 0, [0.0, 0.0]).is_err(), "three corners is the least");
    }

    #[test]
    fn picking_takes_the_nearest_and_then_the_smaller() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut p, "Ground", &sheet(2, "0"), rectangle([-0.1, -0.1], [0.1, 0.1])).unwrap();
        add_shape(&mut p, "On it", &sheet(2, "0"), rectangle([-0.01, -0.01], [0.01, 0.01])).unwrap();
        add_shape(&mut p, "Above", &sheet(2, "0.02"), rectangle([0.03, 0.03], [0.05, 0.05])).unwrap();
        let down = [0.0, 0.0, -1.0];
        assert_eq!(pick(&p, [0.0, 0.0, 1.0], down).unwrap().0, 1, "smaller on one plane");
        assert_eq!(pick(&p, [0.04, 0.04, 1.0], down).unwrap().0, 2, "nearer the eye");
        assert_eq!(pick(&p, [-0.05, 0.05, 1.0], down).unwrap().0, 0);
        assert!(pick(&p, [0.5, 0.5, 1.0], down).is_none());
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        assert!(add_shape(&mut p, "x", &sheet(2, "nope"), rectangle([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(add_shape(&mut p, "x", &sheet(2, "0"), vec![[0.0, 0.0], [1.0, 1.0]]).is_err());
        assert!(add_shape(&mut p, "x", &sheet(2, "0"), vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]).is_err(), "no area");
        let mut wrong = sheet(2, "0");
        wrong.material = "Kryptonite".into();
        assert!(add_shape(&mut p, "x", &wrong, rectangle([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(cut_in(&mut p, 2, 0.0, &rectangle([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(p.extra.is_empty());
    }

    /// The point of keeping dimensions: a shape drawn with the mouse, then tied to variables,
    /// follows them, saves and loads, and solves like the template it copies.
    #[test]
    fn a_drawn_dipole_tied_to_variables_follows_them() {
        let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let v = t.vars().unwrap();
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        set(&mut p, "Z0", "73");
        add_variable(&mut p, "L", &format!("{}mm", v["L"] * 1e3), "Total length").unwrap();
        add_variable(&mut p, "w", &format!("{}mm", v["w"] * 1e3), "Strip width").unwrap();
        add_variable(&mut p, "g", &format!("{}mm", v["g"] * 1e3), "Feed gap").unwrap();
        let s = sheet(1, "0");
        for sign in [1.0, -1.0] {
            add_rect(&mut p, "Arm", &s, [sign * 0.01, -0.005], [sign * 0.05, 0.005]).unwrap();
        }
        for (i, sign) in ["", "-"].iter().enumerate() {
            let ExtraPrim::Drawn { form: Form::Rect { c, size }, .. } = &mut p.extra[i].prim else { panic!() };
            *c = [format!("{sign}(L+g)/4"), "0".into()];
            *size = ["(L-g)/2".into(), "w".into()];
        }
        add_feed(&mut p, 1, "0", [-v["g"] * 2.0, 0.0], [v["g"] * 2.0, 0.0]).unwrap();
        p.extra_ports[0].a = ["0".into(), "0".into(), "-g/2".into()];
        p.extra_ports[0].b = ["0".into(), "0".into(), "g/2".into()];
        p.extra_ports[0].width = "w".into();

        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back.extra, p.extra, "saves and loads");

        let (zt, zd) = (solve_z(&t, 1e9, 14.0), solve_z(&p, 1e9, 14.0));
        assert!((zt - zd).norm() / zt.norm() < 0.02, "template {zt} vs drawn {zd}");

        let length = |p: &Project| {
            let (lo, hi) = p.geometry().unwrap().bounds();
            hi[2] - lo[2]
        };
        let before = length(&p);
        set(&mut p, "L", &format!("{}mm", v["L"] * 1e3 * 1.1));
        assert!((length(&p) / before - 1.1).abs() < 1e-6, "{} to {}", before, length(&p));
    }

    #[test]
    fn dragging_keeps_what_a_variable_decides() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_variable(&mut p, "W", "20mm", "").unwrap();
        add_rect(&mut p, "Plate", &sheet(2, "0"), [0.0, 0.0], [0.04, 0.02]).unwrap();
        // Free: the corner goes where it's dragged and the one across from it stays.
        assert_eq!(move_corner(&mut p, 0, [0.04, 0.02], [0.05, 0.03]), [0.05, 0.03]);
        let bounds = |p: &Project| p.scatterer().unwrap().bounds();
        let (lo, hi) = bounds(&p);
        assert!(near([lo[0], lo[1]], [0.0, 0.0]) && near([hi[0], hi[1]], [0.05, 0.03]), "{lo:?} {hi:?}");

        // Tied along x: only y follows the mouse.
        let ExtraPrim::Drawn { form: Form::Rect { size, .. }, .. } = &mut p.extra[0].prim else { panic!() };
        size[0] = "W".into();
        let (lo, hi) = bounds(&p);
        let out = move_corner(&mut p, 0, [hi[0], hi[1]], [0.09, 0.04]);
        assert!(near(out, [hi[0], 0.04]), "{out:?}");
        assert!((bounds(&p).1[0] - hi[0]).abs() < 1e-9 && (bounds(&p).0[0] - lo[0]).abs() < 1e-9);

        // The part still moves, since its centre is free, and its width stays W.
        assert_eq!(move_part(&mut p, 0, [0.01, 0.0]), [0.01, 0.0]);
        let (a, b) = bounds(&p);
        assert!((b[0] - a[0] - 0.02).abs() < 1e-9, "{a:?} {b:?}");
        // A centre tied to a variable doesn't move at all along that axis.
        let ExtraPrim::Drawn { form: Form::Rect { c, .. }, .. } = &mut p.extra[0].prim else { panic!() };
        c[0] = "W".into();
        assert_eq!(move_part(&mut p, 0, [0.01, 0.002]), [0.0, 0.002]);
    }

    #[test]
    fn a_circle_keeps_its_radius_and_a_rectangle_its_dimensions_through_edits() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        let s = sheet(2, "0");
        add_circle(&mut p, "Disc", &s, [0.0, 0.0], 0.01).unwrap();
        assert_eq!(handles(&p, 0).len(), 2, "centre and rim");
        assert_eq!(move_corner(&mut p, 0, [0.01, 0.0], [0.0, 0.02]), [0.02, 0.0]);
        let ExtraPrim::Drawn { form: Form::Circle { r, .. }, .. } = &p.extra[0].prim else { panic!() };
        assert_eq!(r, "20mm");
        assert!(move_corner(&mut p, 0, [0.0, 0.0], [0.005, 0.0])[0] == 0.005, "the centre moves it");

        // A new corner needs corners, and tied dimensions come through in the expressions.
        add_variable(&mut p, "W", "40mm", "").unwrap();
        add_rect(&mut p, "Plate", &s, [0.1, 0.0], [0.14, 0.02]).unwrap();
        let ExtraPrim::Drawn { form: Form::Rect { size, .. }, .. } = &mut p.extra[1].prim else { panic!() };
        size[0] = "W".into();
        insert_corner(&mut p, 1, 0, [0.12, 0.0]);
        let ExtraPrim::Drawn { form: Form::Poly { pts }, .. } = &p.extra[1].prim else { panic!("{:?}", p.extra[1].prim) };
        assert_eq!(pts.len(), 5);
        assert!(pts.iter().any(|q| q[0].contains('W')), "{pts:?}");
        assert!(remove_corner(&mut p, 1, [0.12, 0.0]).is_ok());
    }

    /// A hole is a dimension too: cut with the mouse, its corners take variables, and it solves
    /// like the same slot from the template.
    #[test]
    fn a_slot_in_a_drawn_plate_follows_its_variables() {
        let t = Project::from_template("slot", Some("2.45GHz")).unwrap();
        let v = t.vars().unwrap();
        let (gx, gy, ls, ws, wp) = (v["Gx"], v["Gy"], v["Ls"], v["ws"], v["wp"]);
        let mut p = Project::from_template("custom", Some("2.45GHz")).unwrap();
        set(&mut p, "Z0", "500");
        add_rect(&mut p, "Plate", &sheet(2, "0"), [-gx / 2.0, -gy / 2.0], [gx / 2.0, gy / 2.0]).unwrap();
        assert_eq!(cut_in(&mut p, 2, 0.0, &rectangle([-ws / 2.0, -ls / 2.0], [ws / 2.0, ls / 2.0])).unwrap(), "Plate 1");
        add_feed(&mut p, 2, "0", [-ws, 0.0], [ws, 0.0]).unwrap();
        p.extra_ports[0].width = mm(wp);
        let edges = t.setup.edges_per_lambda;
        let (zt, zd) = (solve_z(&t, 2.45e9, edges), solve_z(&p, 2.45e9, edges));
        assert!((zt - zd).norm() / zt.norm() < 0.08, "template {zt} vs drawn {zd}");

        // Now the slot's length is a variable.
        add_variable(&mut p, "Ls", &mm(ls), "").unwrap();
        let ExtraPrim::Drawn { holes, .. } = &mut p.extra[0].prim else { panic!() };
        for q in holes[0].iter_mut() {
            q[1] = if q[1].starts_with('-') { "-Ls/2".into() } else { "Ls/2".into() };
        }
        let z_at = |p: &Project| solve_z(p, 2.45e9, edges);
        let before = z_at(&p);
        assert!((before - zd).norm() / zd.norm() < 1e-3, "tied at the same value it's the same: {before} {zd}");
        set(&mut p, "Ls", &mm(ls * 0.85));
        assert!((z_at(&p) - before).norm() / before.norm() > 0.05, "the slot's length moved it");
    }

    #[test]
    fn variables_need_a_usable_new_name_and_a_value_that_works() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_variable(&mut p, "armL", "12mm", "").unwrap();
        assert!(add_variable(&mut p, "armL", "1mm", "").is_err(), "taken");
        assert!(add_variable(&mut p, "f0", "1mm", "").is_err(), "taken");
        assert!(add_variable(&mut p, "2x", "1mm", "").is_err());
        assert!(add_variable(&mut p, "a b", "1mm", "").is_err());
        assert!(add_variable(&mut p, "pi", "1mm", "").is_err(), "a constant");
        let rows = p.params.len();
        assert!(add_variable(&mut p, "armW", "nope*2", "").is_err());
        assert_eq!(p.params.len(), rows, "nothing kept");
    }

    #[test]
    fn numbers_read_as_lengths() {
        assert_eq!(mm(0.0125), "12.5mm");
        assert_eq!(mm(-0.003), "-3mm");
        assert_eq!(mm(0.0), "0");
        assert!(is_free("12.5mm") && is_free("-3mm") && !is_free("L/2"));
        let v = Project::from_template("custom", Some("1GHz")).unwrap().vars().unwrap();
        assert!((expr::eval(&mm(-0.0123456), &v).unwrap() + 0.0123456).abs() < 1e-12);
    }

    #[test]
    fn an_imported_outline_can_become_dimensions() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut p, "Plate", &sheet(2, "0"), rectangle([0.0, 0.0], [0.04, 0.02])).unwrap();
        add_shape(&mut p, "Tri", &sheet(2, "0.01"), vec![[0.0, 0.0], [0.04, 0.0], [0.0, 0.03]]).unwrap();
        let before = p.scatterer().unwrap().bounds();
        make_drawn(&mut p, 0).unwrap();
        make_drawn(&mut p, 1).unwrap();
        assert!(matches!(&p.extra[0].prim, ExtraPrim::Drawn { form: Form::Rect { .. }, .. }));
        assert!(matches!(&p.extra[1].prim, ExtraPrim::Drawn { form: Form::Poly { .. }, .. }));
        let after = p.scatterer().unwrap().bounds();
        assert!((0..3).all(|k| (before.0[k] - after.0[k]).abs() < 1e-9 && (before.1[k] - after.1[k]).abs() < 1e-9));
    }

    #[test]
    fn a_circle_follows_its_chord_limit() {
        let c = circle([0.0, 0.0], 0.05, 50e-6);
        assert!(c.len() >= 12 && c.len() <= 96);
        let exact = std::f64::consts::PI * 0.05 * 0.05;
        assert!((signed_area(&c).abs() - exact).abs() / exact < 0.01, "{} points: {}", c.len(), signed_area(&c));
        assert!(circle([0.0, 0.0], 0.05, 1e-3).len() < c.len());
    }
}
