//! Drawing an antenna from a description: traces, plates with holes, discs, arcs, meanders, vias and
//! feeds, every coordinate an expression over the variable table. Whatever is drawn this way can be
//! swept and optimised by name like a template's dimensions, so a designer isn't limited to shapes
//! somebody already wrote down.
//!
//! The description is JSON, for the assistant's tools and for files:
//!
//! ```json
//! { "vars": { "L": "80mm", "w": "1mm" },
//!   "parts": [ { "kind": "strip", "name": "arm", "path": [["g/2", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" } ],
//!   "feeds": [ { "a": ["-g/2", "0"], "b": ["g/2", "0"], "width": "w" } ] }
//! ```
//!
//! `notes` says what a variable is. A part or a named feed sent under a name already in the drawing
//! replaces it, and `remove` takes parts and feeds away, so a change needn't redraw the whole antenna.

use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::cad;
use crate::expr;
use crate::material;
use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Param, Project};

/// What a description added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawn {
    pub parts: Vec<String>,
    pub feeds: Vec<String>,
    pub vars: Vec<String>,
    /// Parts and feeds it replaced, by name.
    pub replaced: Vec<String>,
    /// New variables that nothing in the drawing is written over.
    pub unused: Vec<String>,
}

/// The note of a variable nobody described: what it moves, which is at least always true.
const NOTE_AUTO: &str = "Used by ";
const NOTE_NEW: &str = "Drawn";

/// Every variable an expression somewhere in a part's JSON uses.
fn names_in(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => out.extend(expr::names(s).unwrap_or_default()),
        Value::Array(a) => a.iter().for_each(|x| names_in(x, out)),
        Value::Object(o) => o.iter().filter(|(k, x)| !(x.is_string() && matches!(k.as_str(), "type" | "form" | "file"))).for_each(|(_, x)| names_in(x, out)),
        _ => {}
    }
}

/// The parts and feeds written over a variable, directly or through other rows of the table.
pub fn what_it_moves(p: &Project, name: &str) -> Vec<String> {
    let rows: Vec<(String, String)> = p.params.iter().map(|r| (r.name.clone(), r.expr.clone())).collect();
    let mut through = expr::dependents(&rows, name);
    through.insert(name.to_string());
    let uses = |v: Value| {
        let mut used = BTreeSet::new();
        names_in(&v, &mut used);
        !used.is_disjoint(&through)
    };
    let parts = p.extra.iter().filter(|e| uses(serde_json::to_value(&e.prim).unwrap_or(Value::Null))).map(|e| e.name.clone());
    let feeds = p.extra_ports.iter().filter(|f| uses(json!([f.a, f.b, f.width, f.z0]))).map(|f| f.name.clone());
    parts.chain(feeds).collect()
}

/// Takes a part away, with the copies its mirror made.
fn drop_part(q: &mut Project, base: &str) -> bool {
    let copies = [base.to_string(), format!("{base} m"), format!("{base} m1"), format!("{base} m2"), format!("{base} m3")];
    let before = q.extra.len();
    q.extra.retain(|e| !copies.contains(&e.name));
    q.extra.len() != before
}

/// What's in the drawing, written the way it was drawn, so that it can be changed part by part.
pub fn written(p: &Project) -> Value {
    let parts: Vec<Value> = p
        .extra
        .iter()
        .map(|e| match &e.prim {
            ExtraPrim::Imported { .. } | ExtraPrim::Outline { .. } => json!({ "name": e.name, "material": e.material, "shape": "from a file, in plain numbers" }),
            prim => json!({ "name": e.name, "material": e.material, "shape": prim }),
        })
        .collect();
    let feeds: Vec<Value> = p.extra_ports.iter().map(|f| json!({ "name": f.name, "a": f.a, "b": f.b, "width": f.width, "wave": p.wave_feeds.contains(&f.name) })).collect();
    let mut out = json!({ "parts": parts, "feeds": feeds });
    // What can't be tuned yet, so it can be tied to a variable before it needs to be.
    let mut loose = Vec::new();
    for e in &p.extra {
        if !matches!(e.prim, ExtraPrim::Imported { .. } | ExtraPrim::Outline { .. }) {
            walk(&serde_json::to_value(&e.prim).unwrap_or(Value::Null), "", &e.name, &mut loose);
        }
    }
    for f in &p.extra_ports {
        walk(&json!({ "a": f.a, "b": f.b, "width": f.width }), "", &f.name, &mut loose);
    }
    if !loose.is_empty() {
        out["plain_numbers"] = json!(loose.iter().map(|l| json!({ "number": l.number, "at": l.at })).collect::<Vec<_>>());
    }
    out
}

/// A coordinate or size: an expression. A bare number other than zero is refused, since nobody can
/// tell metres from millimetres in it.
fn expr(v: &Value, what: &str) -> Result<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Ok(s.clone()),
        Value::Number(n) if n.as_f64() == Some(0.0) => Ok("0".into()),
        Value::Number(n) => bail!("{what} is the number {n}: write it with its unit, like \"12mm\", or as an expression over the variables."),
        Value::Null => bail!("{what} is missing."),
        other => bail!("{what} has to be an expression, not {other}."),
    }
}

fn point(v: &Value, what: &str) -> Result<[String; 2]> {
    let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| anyhow!("{what} has to be a pair [u, v]."))?;
    Ok([expr(&a[0], what)?, expr(&a[1], what)?])
}

fn points(v: &Value, what: &str) -> Result<Vec<[String; 2]>> {
    v.as_array().ok_or_else(|| anyhow!("{what} has to be a list of [u, v] pairs."))?.iter().map(|q| point(q, what)).collect()
}

fn triple(v: &Value, what: &str) -> Result<[String; 3]> {
    let a = v.as_array().filter(|a| a.len() == 3).ok_or_else(|| anyhow!("{what} has to be [x, y, z]."))?;
    Ok([expr(&a[0], what)?, expr(&a[1], what)?, expr(&a[2], what)?])
}

fn negated(e: &str) -> String {
    format!("-({e})")
}

/// The same point mirrored: `x` across the v axis, `y` across the u axis.
fn mirror_point(q: &[String; 2], across_u: bool, across_v: bool) -> [String; 2] {
    [if across_u { negated(&q[0]) } else { q[0].clone() }, if across_v { negated(&q[1]) } else { q[1].clone() }]
}

fn mirrored(prim: &ExtraPrim, mu: bool, mv: bool) -> Option<ExtraPrim> {
    let m = |q: &[String; 2]| mirror_point(q, mu, mv);
    Some(match prim {
        ExtraPrim::Strip { axis, at, path, w } => ExtraPrim::Strip { axis: *axis, at: at.clone(), path: path.iter().map(m).collect(), w: w.clone() },
        ExtraPrim::Polygon { axis, at, pts, thick } => ExtraPrim::Polygon { axis: *axis, at: at.clone(), pts: pts.iter().map(m).collect(), thick: thick.clone() },
        ExtraPrim::Plate { axis, at, outline, holes } => ExtraPrim::Plate { axis: *axis, at: at.clone(), outline: outline.iter().map(m).collect(), holes: holes.iter().map(|h| h.iter().map(m).collect()).collect() },
        ExtraPrim::Disc { axis, at, c, r } => ExtraPrim::Disc { axis: *axis, at: at.clone(), c: m(c), r: r.clone() },
        ExtraPrim::Meander { axis, at, start, step, height, n, w } => ExtraPrim::Meander { axis: *axis, at: at.clone(), start: m(start), step: if mu { negated(step) } else { step.clone() }, height: if mv { negated(height) } else { height.clone() }, n: n.clone(), w: w.clone() },
        ExtraPrim::Arc { axis, at, c, r, from_deg, to_deg, w } => {
            // Mirroring an arc turns its angles round.
            let turn = |a: &String| match (mu, mv) {
                (true, true) => format!("({a})+180"),
                (true, false) => format!("180-({a})"),
                (false, true) => negated(a),
                (false, false) => a.clone(),
            };
            ExtraPrim::Arc { axis: *axis, at: at.clone(), c: m(c), r: r.clone(), from_deg: turn(from_deg), to_deg: turn(to_deg), w: w.clone() }
        }
        _ => return None,
    })
}

fn prim_of(part: &Value, kind: &str, axis: usize, at: &str) -> Result<ExtraPrim> {
    let at = at.to_string();
    Ok(match kind {
        "strip" => ExtraPrim::Strip { axis, at, path: points(&part["path"], "path")?, w: expr(&part["w"], "w")? },
        "rect" => {
            let (a, b) = (point(&part["a"], "a")?, point(&part["b"], "b")?);
            ExtraPrim::Polygon { axis, at, pts: vec![[a[0].clone(), a[1].clone()], [b[0].clone(), a[1].clone()], [b[0].clone(), b[1].clone()], [a[0].clone(), b[1].clone()]], thick: "0".into() }
        }
        "polygon" => ExtraPrim::Polygon { axis, at, pts: points(&part["pts"], "pts")?, thick: "0".into() },
        "plate" => {
            let holes = part["holes"].as_array().map_or(Ok(Vec::new()), |h| h.iter().map(|o| points(o, "hole")).collect::<Result<Vec<_>>>())?;
            ExtraPrim::Plate { axis, at, outline: points(&part["outline"], "outline")?, holes }
        }
        "disc" => ExtraPrim::Disc { axis, at, c: point(&part["c"], "c")?, r: expr(&part["r"], "r")? },
        "arc" => ExtraPrim::Arc { axis, at, c: point(&part["c"], "c")?, r: expr(&part["r"], "r")?, from_deg: expr(&part["from_deg"], "from_deg")?, to_deg: expr(&part["to_deg"], "to_deg")?, w: expr(&part["w"], "w")? },
        "meander" => ExtraPrim::Meander { axis, at, start: point(&part["start"], "start")?, step: expr(&part["step"], "step")?, height: expr(&part["height"], "height")?, n: expr(&part["n"], "n")?, w: expr(&part["w"], "w")? },
        "box" => ExtraPrim::Box { min: triple(&part["a"], "a")?, max: triple(&part["b"], "b")? },
        "via" => ExtraPrim::Cylinder { a: triple(&part["a"], "a")?, b: triple(&part["b"], "b")?, r: expr(&part["r"], "r")? },
        "wire" => ExtraPrim::Wire { pts: part["pts"].as_array().ok_or_else(|| anyhow!("a wire needs pts"))?.iter().map(|q| triple(q, "wire point")).collect::<Result<_>>()?, r: expr(&part["r"], "r")? },
        other => bail!("No part of kind '{other}': use strip, rect, polygon, plate, disc, arc, meander, box, via or wire."),
    })
}

fn unique_name(p: &Project, base: &str) -> String {
    if !p.extra.iter().any(|e| e.name == base) {
        return base.to_string();
    }
    (2..).map(|k| format!("{base} {k}")).find(|n| !p.extra.iter().any(|e| &e.name == n)).unwrap_or_else(|| base.to_string())
}

/// Applies a description to a project. Nothing is kept if the result doesn't build, so a mistake can't
/// leave half a model. `fresh` starts from a blank model on the same band, keeping the layers and metal.
pub fn draw(p: &mut Project, spec: &Value) -> Result<Drawn> {
    let mut q = p.clone();
    let fresh = spec["fresh"].as_bool().unwrap_or(false);
    if fresh {
        let f0 = q.f0();
        let mut blank = Project::from_template("custom", Some(&format!("{}MHz", f0 / 1e6)))?;
        blank.setup = q.setup.clone();
        blank.stack = q.stack.clone();
        blank.metal = q.metal.clone();
        blank.substrate = q.substrate.clone();
        q = blank;
    }
    if let Some(n) = spec["name"].as_str() {
        q.name = n.to_string();
    }
    if let Some(m) = spec["metal"].as_str() {
        if !material::by_name(m).is_some_and(|x| x.is_conductor()) {
            bail!("'{m}' isn't a metal in the material library.");
        }
        q.metal = m.to_string();
    }
    let mut out = Drawn::default();
    let mut created = Vec::new();
    if let Some(vars) = spec["vars"].as_object() {
        for (name, v) in vars {
            let e = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) if n.as_f64() == Some(0.0) => "0".into(),
                Value::Number(n) if n.as_f64().is_some_and(|x| x.abs() >= 1e-12) && matches!(name.as_str(), "n" | "turns" | "order") => n.to_string(),
                other => bail!("Variable '{name}' is {other}: give it as an expression with its unit, like \"12mm\"."),
            };
            match q.params.iter_mut().find(|r| &r.name == name) {
                Some(r) => r.expr = e,
                None => {
                    q.params.push(Param::new(name, &e, NOTE_NEW));
                    created.push(name.clone());
                }
            }
            out.vars.push(name.clone());
        }
    }
    // Only what was there before this call can be replaced: two parts of one name in it are two parts.
    let had_parts: Vec<String> = q.extra.iter().map(|e| e.name.clone()).collect();
    let had_feeds: Vec<String> = q.extra_ports.iter().map(|f| f.name.clone()).collect();
    for name in spec["remove"].as_array().into_iter().flatten() {
        let name = name.as_str().ok_or_else(|| anyhow!("remove is a list of part and feed names."))?;
        let gone_feed = had_feeds.iter().any(|n| n == name);
        q.extra_ports.retain(|f| f.name != name);
        if !drop_part(&mut q, name) && !gone_feed {
            bail!("Nothing called '{name}' to remove: there's {}.", had_parts.iter().chain(&had_feeds).cloned().collect::<Vec<_>>().join(", "));
        }
    }
    for part in spec["parts"].as_array().into_iter().flatten() {
        let kind = part["kind"].as_str().ok_or_else(|| anyhow!("A part needs a kind."))?;
        let base = part["name"].as_str().unwrap_or(kind);
        if part["name"].is_string() && had_parts.iter().any(|n| n == base) && !out.replaced.iter().any(|n| n == base) {
            drop_part(&mut q, base);
            out.replaced.push(base.to_string());
        }
        let axis = match part["axis"].as_str().unwrap_or("z") {
            "x" => 0,
            "y" => 1,
            "z" => 2,
            other => bail!("Part '{base}' is on axis '{other}': use x, y or z."),
        };
        let at = part.get("at").map_or(Ok("0".to_string()), |v| expr(v, "at"))?;
        let mat = part["material"].as_str().unwrap_or(&q.metal).to_string();
        if material::by_name(&mat).is_none() {
            bail!("Part '{base}' is made of '{mat}', which isn't in the material library.");
        }
        let prim = prim_of(part, kind, axis, &at).with_context(|| format!("part '{base}'"))?;
        let name = unique_name(&q, base);
        q.extra.push(ExtraSolid { name: name.clone(), material: mat.clone(), prim: prim.clone() });
        out.parts.push(name);
        let (mu, mv, both) = match part["mirror"].as_str() {
            None => (false, false, false),
            Some("x") => (true, false, false),
            Some("y") => (false, true, false),
            Some("xy") => (true, true, true),
            Some(other) => bail!("Part '{base}': mirror is x, y or xy, not '{other}'."),
        };
        if mu || mv {
            let Some(copy) = mirrored(&prim, mu && !both, mv && !both) else { bail!("Part '{base}': a {kind} can't be mirrored; draw the other side.") };
            if both {
                let in_x = mirrored(&prim, true, false).expect("mirrorable");
                let in_y = mirrored(&prim, false, true).expect("mirrorable");
                let through = mirrored(&prim, true, true).expect("mirrorable");
                for (k, c) in [in_x, in_y, through].into_iter().enumerate() {
                    let name = unique_name(&q, &format!("{base} m{}", k + 1));
                    q.extra.push(ExtraSolid { name: name.clone(), material: mat.clone(), prim: c });
                    out.parts.push(name);
                }
            } else {
                let name = unique_name(&q, &format!("{base} m"));
                q.extra.push(ExtraSolid { name: name.clone(), material: mat.clone(), prim: copy });
                out.parts.push(name);
            }
        }
    }
    for (k, feed) in spec["feeds"].as_array().into_iter().flatten().enumerate() {
        if !q.params.iter().any(|r| r.name == "Z0") {
            q.params.push(Param::new("Z0", "50", "Port impedance"));
        }
        let name = feed["name"].as_str().map_or_else(|| if k == 0 && q.extra_ports.is_empty() { "chip".to_string() } else { format!("feed {}", q.extra_ports.len() + 1) }, str::to_string);
        let at = feed.get("at").map_or(Ok("0".to_string()), |v| expr(v, "at"))?;
        // A feed may be given in the plane, or as two points in space for a probe.
        let end = |v: &Value, what: &str| -> Result<[String; 3]> {
            match v.as_array().map(Vec::len) {
                Some(3) => triple(v, what),
                _ => {
                    let q = point(v, what)?;
                    Ok([q[0].clone(), q[1].clone(), at.clone()])
                }
            }
        };
        let width = feed.get("width").map_or(Ok("0".to_string()), |v| expr(v, "width"))?;
        let z0 = feed.get("z0").map_or(Ok("Z0".to_string()), |v| expr(v, "z0").or_else(|_| v.as_f64().map(|x| x.to_string()).ok_or_else(|| anyhow!("z0 has to be a number"))))?;
        let port = ExtraPort { name: name.clone(), a: end(&feed["a"], "feed a")?, b: end(&feed["b"], "feed b")?, width, z0 };
        match feed.get("wave").and_then(Value::as_bool) {
            Some(true) if !q.wave_feeds.contains(&name) => q.wave_feeds.push(name.clone()),
            Some(false) => q.wave_feeds.retain(|f| f != &name),
            _ => {}
        }
        // In its place: the order of the feeds is the order of the ports.
        match q.extra_ports.iter().position(|f| f.name == name).filter(|_| feed["name"].is_string() && had_feeds.contains(&name)) {
            Some(slot) => {
                q.extra_ports[slot] = port;
                out.replaced.push(name.clone());
            }
            None => q.extra_ports.push(port),
        }
        out.feeds.push(name);
    }
    if let Some(notes) = spec["notes"].as_object() {
        for (name, note) in notes {
            let Some(row) = q.params.iter_mut().find(|r| &r.name == name) else { bail!("notes has '{name}', which isn't a variable.") };
            let Some(note) = note.as_str() else { bail!("The note for '{name}' has to be words.") };
            row.note = note.to_string();
        }
    }
    // A row with no note says what it moves, and keeps saying it as the drawing changes.
    let moved: Vec<(String, Vec<String>)> = q.params.iter().map(|r| (r.name.clone(), what_it_moves(&q, &r.name))).collect();
    for (row, (_, moves)) in q.params.iter_mut().zip(&moved) {
        if row.note == NOTE_NEW || row.note.starts_with(NOTE_AUTO) {
            let shown = moves.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
            let more = moves.len().saturating_sub(4);
            row.note = match (moves.is_empty(), more) {
                (true, _) => NOTE_NEW.to_string(),
                (false, 0) => format!("{NOTE_AUTO}{shown}"),
                (false, n) => format!("{NOTE_AUTO}{shown} and {n} more"),
            };
        }
    }
    out.unused = created.into_iter().filter(|n| moved.iter().any(|(m, moves)| m == n && moves.is_empty())).collect();
    // Everything has to build now, and be a sensible size.
    let g = q.scatterer()?;
    let (lo, hi) = g.bounds();
    let lambda = crate::C0 / q.f0();
    let span = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
    if span > 30.0 * lambda {
        bail!("This model spans {span:.1} m, or {:.0} wavelengths: probably a length with no unit. Lengths need units, like \"12mm\".", span / lambda);
    }
    let _ = cad::signed_area;
    *p = q;
    Ok(out)
}

/// A number written into a drawing, and everywhere it's written.
#[derive(Clone, Debug, PartialEq)]
pub struct Loose {
    pub number: String,
    pub at: Vec<String>,
}

/// The numbers in one expression that no variable can move: anything with a unit, or a whole
/// expression that's a figure. A factor over a name, like `L/2` or `2*w`, follows the name. Zero
/// doesn't count, since nothing needs a name to sit on an axis, and nor does a whole number of
/// quarter turns in an angle, which is the shape of the thing and not a dimension of it.
fn loose_in(text: &str, angle: bool) -> Vec<String> {
    // What doesn't parse is for `draw` to say.
    let Ok(names) = expr::names(text) else { return Vec::new() };
    let nothing = Default::default();
    let tokens = expr::tokens(text, |_| true);
    let mut out: Vec<String> = tokens
        .windows(2)
        .filter(|t| t[0].kind == expr::TokenKind::Number && t[1].kind == expr::TokenKind::Unit)
        .map(|t| &text[t[0].start..t[1].end])
        .filter(|n| expr::eval(n, &nothing).is_ok_and(|v| v != 0.0))
        .map(str::to_string)
        .collect();
    if out.is_empty() && names.is_empty() {
        let structural = expr::eval(text, &nothing).is_ok_and(|v| v == 0.0 || (angle && v % 90.0 == 0.0));
        if !structural {
            out.push(text.trim().to_string());
        }
    }
    out
}

fn walk(v: &Value, key: &str, at: &str, out: &mut Vec<Loose>) {
    let mut found = |number: String| match out.iter_mut().find(|l| l.number == number) {
        Some(l) => l.at.push(at.to_string()),
        None => out.push(Loose { number, at: vec![at.to_string()] }),
    };
    match v {
        Value::String(s) => loose_in(s, matches!(key, "from_deg" | "to_deg")).into_iter().for_each(&mut found),
        Value::Number(n) if n.as_f64() != Some(0.0) => found(n.to_string()),
        Value::Array(a) => a.iter().enumerate().for_each(|(k, x)| walk(x, key, &format!("{at}[{k}]"), out)),
        Value::Object(o) => o.iter().filter(|(k, x)| !matches!(k.as_str(), "kind" | "name" | "axis" | "mirror" | "material" | "z0" | "type") && !(k.as_str() == "form" && x.is_string())).for_each(|(k, x)| walk(x, k, &format!("{at}.{k}"), out)),
        _ => {}
    }
}

/// The numbers in a description's parts and feeds that aren't written over a variable. No sweep,
/// optimiser or later change can move those, and a real model drew a whole tag that way from a sketch
/// with its dimensions labelled.
pub fn loose_numbers(spec: &Value) -> Vec<Loose> {
    let mut out = Vec::new();
    for (k, part) in spec["parts"].as_array().into_iter().flatten().enumerate() {
        walk(part, "", part["name"].as_str().map_or_else(|| format!("part {}", k + 1), str::to_string).as_str(), &mut out);
    }
    for (k, feed) in spec["feeds"].as_array().into_iter().flatten().enumerate() {
        walk(feed, "", feed["name"].as_str().map_or_else(|| format!("feed {}", k + 1), str::to_string).as_str(), &mut out);
    }
    out
}

/// Why a drawing was refused, and how to send it again. A refusal costs a whole call to the model, so
/// this has everything in it at once, each number with every place it's written.
pub fn loose_message(loose: &[Loose]) -> String {
    let mut lines: Vec<String> = loose
        .iter()
        .take(40)
        .map(|l| {
            let shown = l.at.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
            match l.at.len().saturating_sub(6) {
                0 => format!("{} at {shown}", l.number),
                n => format!("{} at {shown} and {n} more", l.number),
            }
        })
        .collect();
    if loose.len() > 40 {
        lines.push(format!("and {} more", loose.len() - 40));
    }
    format!(
        "Not drawn: every dimension has to be a named variable, so it can be changed later by name. These numbers are written into the parts and feeds:\n{}\nPut each dimension in vars with its value and unit, and in notes what it is, then send the drawing again with the parts and feeds written over those names. Use one name for whatever should move together, and the same name with a minus for its mirror image. A dimension that follows from others goes in vars as an expression over them. Only vars holds numbers with units: parts and feeds hold names, plain factors like L/2 or 2*w, and 0.",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mom::{MomOptions, MomProblem};

    #[test]
    fn numbers_with_nothing_to_move_them_are_found_and_expressions_and_zero_are_not() {
        let tied = json!({ "parts": [{ "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "0mm"]], "w": "2*w", "mirror": "x" }], "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w", "z0": 50 }] });
        assert!(loose_numbers(&tied).is_empty(), "names, factors, zeros and a port's impedance aren't dimensions to name");
        // What a real model sent for a sketch: the same shape, every figure typed in. The same size
        // either way round is one number, with every place it's written.
        let bare = json!({ "parts": [{ "kind": "strip", "name": "arm", "path": [["0.5mm", "0"], ["6mm", "0mm"], ["6mm", "10.5mm"]], "w": "0.4mm" }], "feeds": [{ "a": ["0.5mm", "0"], "b": ["-0.5mm", "0"], "width": "0.4mm" }] });
        let found = loose_numbers(&bare);
        let numbers: Vec<&str> = found.iter().map(|l| l.number.as_str()).collect();
        assert_eq!(numbers, ["0.5mm", "6mm", "10.5mm", "0.4mm"], "{found:?}");
        assert_eq!(found[0].at, ["arm.path[0][0]", "feed 1.a[0]", "feed 1.b[0]"]);
        assert_eq!(found[3].at, ["arm.w", "feed 1.width"]);
        // An offset hides a dimension as surely as a plain figure does.
        let offset = loose_numbers(&json!({ "parts": [{ "kind": "rect", "a": ["a1+2mm", "t/2"], "b": ["3mm", "0"] }] }));
        assert_eq!(offset.iter().map(|l| l.number.as_str()).collect::<Vec<_>>(), ["2mm", "3mm"], "{offset:?}");
        // A half circle is a shape. A 120 degree arc is a choice somebody may want to change, and so
        // is how many periods a meander has.
        let arc = json!({ "parts": [{ "kind": "arc", "name": "a", "c": ["0", "0"], "r": "r", "from_deg": "0", "to_deg": "180", "w": "w" }] });
        assert!(loose_numbers(&arc).is_empty());
        let chosen = json!({ "parts": [{ "kind": "arc", "name": "a", "c": ["0", "0"], "r": "r", "from_deg": "0", "to_deg": "120", "w": "w" }, { "kind": "meander", "name": "m", "start": ["0", "0"], "step": "s", "height": "h", "n": "3", "w": "w" }] });
        assert_eq!(loose_numbers(&chosen).iter().map(|l| l.number.as_str()).collect::<Vec<_>>(), ["120", "3"]);
        // What it says is enough to act on.
        let said = loose_message(&found);
        assert!(said.contains("6mm at arm.path[1][0], arm.path[2][0]") && said.contains("vars") && said.contains("notes"), "{said}");
    }

    /// A later change shouldn't take a redraw, and shouldn't pile a second copy on top of the first.
    #[test]
    fn a_part_sent_again_replaces_it_and_remove_takes_it_away() {
        let mut p = blank("915MHz");
        draw(&mut p, &json!({
            "vars": { "L": "80mm", "w": "1mm", "gp": "1mm", "t": "L/4" },
            "parts": [
                { "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" },
                { "kind": "rect", "name": "cap", "a": ["L/2", "0"], "b": ["L/2+w", "t"] }
            ],
            "feeds": [{ "name": "chip", "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
        })).unwrap();
        let names = |p: &Project| p.extra.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&p), ["arm", "arm m", "cap"]);

        // Its mirror goes with it, whether or not the new one has one.
        let d = draw(&mut p, &json!({ "parts": [{ "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "t"]], "w": "w" }] })).unwrap();
        assert_eq!(d.replaced, ["arm"]);
        assert_eq!(names(&p), ["cap", "arm"]);
        // Two of one name in a single call are two parts, as they always were.
        let d = draw(&mut p, &json!({ "parts": [{ "kind": "rect", "name": "pad", "a": ["0", "0"], "b": ["w", "w"] }, { "kind": "rect", "name": "pad", "a": ["w", "0"], "b": ["w+w", "w"] }] })).unwrap();
        assert_eq!(d.parts, ["pad", "pad 2"]);
        assert_eq!(d.replaced, Vec::<String>::new());
        // A feed keeps its place, since the order of the feeds is the order of the ports.
        draw(&mut p, &json!({ "feeds": [{ "name": "second", "a": ["-gp/2", "t"], "b": ["gp/2", "t"], "width": "w" }] })).unwrap();
        let d = draw(&mut p, &json!({ "feeds": [{ "name": "chip", "a": ["-gp", "0"], "b": ["gp", "0"], "width": "w" }] })).unwrap();
        assert_eq!(d.replaced, ["chip"]);
        assert_eq!(p.extra_ports.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["chip", "second"]);
        assert_eq!(p.extra_ports[0].a[0], "-gp");

        draw(&mut p, &json!({ "remove": ["cap", "second"] })).unwrap();
        assert_eq!(names(&p), ["arm", "pad", "pad 2"]);
        assert_eq!(p.extra_ports.len(), 1);
        let before = p.clone();
        let e = format!("{:#}", draw(&mut p, &json!({ "remove": ["nothing"] })).unwrap_err());
        assert!(e.contains("nothing") && e.contains("arm") && e.contains("chip"), "{e}");
        assert_eq!(p, before);
    }

    /// The table is where a change starts, so a row has to say what it's for, and one that moves
    /// nothing has to be pointed out.
    #[test]
    fn a_variable_says_what_it_is_and_what_it_moves() {
        let mut p = blank("915MHz");
        let d = draw(&mut p, &json!({
            "vars": { "L": "80mm", "w": "1mm", "t": "L/4", "spare": "3mm", "gone": "spare*2" },
            "notes": { "w": "Trace width" },
            "parts": [
                { "kind": "strip", "name": "arm", "path": [["0", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" },
                { "kind": "rect", "name": "cap", "a": ["0", "0"], "b": ["w", "t"] }
            ]
        })).unwrap();
        let note = |p: &Project, n: &str| p.params.iter().find(|r| r.name == n).unwrap().note.clone();
        assert_eq!(note(&p, "w"), "Trace width", "what it was told");
        assert_eq!(note(&p, "t"), "Used by cap");
        assert_eq!(note(&p, "L"), "Used by arm, arm m, cap", "directly, and through t");
        let mut unused = d.unused.clone();
        unused.sort();
        assert_eq!(unused, ["gone", "spare"], "one using the other doesn't make them do anything");
        // The note follows the drawing, and what it was told stays.
        draw(&mut p, &json!({ "remove": ["cap"] })).unwrap();
        assert_eq!(note(&p, "L"), "Used by arm, arm m");
        assert_eq!(note(&p, "w"), "Trace width");
        // A note on a row that isn't there is a mistake worth saying.
        let e = format!("{:#}", draw(&mut p, &json!({ "notes": { "nope": "x" } })).unwrap_err());
        assert!(e.contains("nope"), "{e}");
    }

    /// What the assistant reads to change a drawing: how it's written, and what isn't tied yet.
    #[test]
    fn what_is_written_shows_the_expressions_and_the_numbers_left_to_tie() {
        let mut p = blank("915MHz");
        draw(&mut p, &json!({
            "vars": { "L": "80mm", "w": "1mm" },
            "parts": [{ "kind": "strip", "name": "arm", "path": [["0", "0"], ["L/2", "12mm"]], "w": "w" }, { "kind": "disc", "name": "pad", "c": ["L", "0"], "r": "w" }],
            "feeds": [{ "name": "chip", "a": ["-w", "0"], "b": ["w", "0"], "width": "w" }]
        })).unwrap();
        let on = written(&p);
        assert_eq!(on["parts"][0]["shape"]["path"][1][0], "L/2", "{on}");
        assert_eq!(on["feeds"][0]["width"], "w", "{on}");
        assert_eq!(on["plain_numbers"][0]["number"], "12mm", "{on}");
        assert_eq!(on["plain_numbers"][0]["at"][0], "arm.path[1][1]", "{on}");
        assert_eq!(on["plain_numbers"].as_array().unwrap().len(), 1, "the type of a shape isn't a number: {on}");
    }

    fn blank(f: &str) -> Project {
        Project::from_template("custom", Some(f)).unwrap()
    }

    fn solve_z(p: &Project, f: f64, edges: f64) -> crate::C64 {
        let g = p.geometry().unwrap();
        MomProblem::new(&g, f, MomOptions { edges_per_lambda: edges, ..Default::default() }).unwrap().solve(f, false).unwrap().z[0][0]
    }

    /// A dipole described in words solves like the same dipole from the template, and the variable it
    /// was drawn with moves it.
    #[test]
    fn a_described_dipole_solves_like_the_template() {
        let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let v = t.vars().unwrap();
        let (l, w, g) = (v["L"], v["w"], v["g"]);
        let mut p = blank("1GHz");
        let drawn = draw(&mut p, &json!({
            "vars": { "L": format!("{}mm", l * 1e3), "w": format!("{}mm", w * 1e3), "gg": format!("{}mm", g * 1e3), "Z0": "73" },
            "parts": [{ "kind": "strip", "name": "arm", "path": [["gg/2", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" }],
            "feeds": [{ "a": ["-gg/2", "0"], "b": ["gg/2", "0"], "width": "w" }]
        })).unwrap();
        assert_eq!(drawn.parts, ["arm", "arm m"]);
        assert_eq!(drawn.feeds, ["chip"]);
        let port = &p.geometry().unwrap().ports[0];
        assert!((port.a[0] + g / 2.0).abs() < 1e-9 && (port.b[0] - g / 2.0).abs() < 1e-9, "{port:?}");
        // The template's strip runs along z, this one along x: same dipole, turned.
        let (zt, zd) = (solve_z(&t, 1e9, 14.0), solve_z(&p, 1e9, 14.0));
        println!("template {zt:.2}, described {zd:.2}");
        assert!((zt - zd).norm() / zt.norm() < 0.03, "{zt} vs {zd}");
        let longer = p.with_values(&[("L".to_string(), l * 1.1)]);
        let (lo, hi) = longer.geometry().unwrap().bounds();
        assert!(((hi[0] - lo[0]) - l * 1.1).abs() < 1e-3, "{}", hi[0] - lo[0]);
    }

    #[test]
    fn every_kind_of_part_builds_and_meshes() {
        let mut p = blank("915MHz");
        draw(&mut p, &json!({
            "vars": { "w": "1mm", "n": 3 },
            "parts": [
                { "kind": "strip", "name": "s", "path": [["1mm", "0"], ["20mm", "0"], ["20mm", "10mm"]], "w": "w", "mirror": "x" },
                { "kind": "rect", "name": "r", "a": ["22mm", "-5mm"], "b": ["30mm", "5mm"], "mirror": "x" },
                { "kind": "polygon", "name": "t", "pts": [["32mm", "-5mm"], ["40mm", "-8mm"], ["40mm", "8mm"], ["32mm", "5mm"]] },
                { "kind": "plate", "name": "p", "outline": [["-20mm", "20mm"], ["20mm", "20mm"], ["20mm", "40mm"], ["-20mm", "40mm"]], "holes": [[["-5mm", "25mm"], ["5mm", "25mm"], ["5mm", "35mm"], ["-5mm", "35mm"]]] },
                { "kind": "disc", "name": "d", "c": ["0", "-30mm"], "r": "8mm" },
                { "kind": "arc", "name": "a", "c": ["30mm", "30mm"], "r": "10mm", "from_deg": "0", "to_deg": "120", "w": "w", "mirror": "x" },
                { "kind": "meander", "name": "m", "start": ["45mm", "0"], "step": "3mm", "height": "4mm", "n": "n", "w": "w", "mirror": "x" }
            ],
            "feeds": [{ "a": ["-1mm", "0"], "b": ["1mm", "0"], "width": "w" }]
        })).unwrap();
        let g = p.geometry().unwrap();
        assert_eq!(g.solids.len(), 11);
        assert!(g.solver_fit().surface_mom);
        let (lo, hi) = g.bounds();
        assert!((lo[0] + hi[0]).abs() < 1e-6, "{lo:?} {hi:?}");
        let m = MomProblem::new(&g, 915e6, MomOptions { edges_per_lambda: 24.0, ..Default::default() }).unwrap();
        assert!(m.unknowns() > 50);
        // The window is open: its centre has no metal.
        let plate = g.solids.iter().find(|s| s.name == "p").unwrap();
        let crate::geometry::Shape::Polygon { pts, .. } = &plate.shape else { panic!() };
        assert!(!cad::point_inside(pts, [0.0, 0.03]), "the hole");
        assert!(cad::point_inside(pts, [0.015, 0.03]), "the metal round it");
    }

    #[test]
    fn a_mirror_in_both_makes_four() {
        let mut p = blank("915MHz");
        let d = draw(&mut p, &json!({ "parts": [{ "kind": "rect", "name": "r", "a": ["2mm", "3mm"], "b": ["4mm", "5mm"], "mirror": "xy" }] })).unwrap();
        assert_eq!(d.parts.len(), 4);
        let (lo, hi) = p.scatterer().unwrap().bounds();
        assert!((lo[0] + 0.004).abs() < 1e-9 && (hi[1] - 0.005).abs() < 1e-9, "{lo:?} {hi:?}");
    }

    #[test]
    fn a_mistake_says_so_and_leaves_the_model_alone() {
        let mut p = blank("915MHz");
        draw(&mut p, &json!({ "parts": [{ "kind": "rect", "name": "keep", "a": ["0", "0"], "b": ["10mm", "10mm"] }] })).unwrap();
        let before = p.clone();
        for (bad, says) in [
            (json!({ "parts": [{ "kind": "blob" }] }), "No part of kind"),
            (json!({ "parts": [{ "kind": "rect", "name": "x", "a": [10, 0], "b": ["1mm", "1mm"] }] }), "with its unit"),
            (json!({ "parts": [{ "kind": "strip", "path": [["0", "0"], ["1mm", "0"]] }] }), "w is missing"),
            (json!({ "parts": [{ "kind": "rect", "name": "x", "a": ["nope", "0"], "b": ["1mm", "1mm"] }] }), "nope"),
            (json!({ "parts": [{ "kind": "rect", "name": "x", "a": ["0", "0"], "b": ["500", "1mm"] }] }), "no unit"),
            (json!({ "metal": "Paper" }), "isn't a metal"),
            (json!({ "vars": { "w": 3 } }), "with its unit"),
        ] {
            let e = format!("{:#}", draw(&mut p, &bad).unwrap_err());
            assert!(e.contains(says), "{bad}: {e}");
            assert_eq!(p, before, "{bad}");
        }
    }

    #[test]
    fn fresh_starts_over_and_keeps_the_surroundings() {
        let mut p = Project::from_template("rfid_tag", Some("915MHz")).unwrap();
        p.stack.above.push(crate::stack::Layer::new("paper", "Paper", 80e-6));
        p.metal = "Aluminium".into();
        draw(&mut p, &json!({ "fresh": true, "parts": [{ "kind": "rect", "a": ["0", "0"], "b": ["10mm", "10mm"] }] })).unwrap();
        assert_eq!(p.template, "custom");
        assert_eq!(p.extra.len(), 1);
        assert_eq!(p.stack.above.len(), 1);
        assert_eq!(p.metal, "Aluminium");
        assert!((p.f0() - 915e6).abs() < 1.0);
    }
}
