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

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::cad;
use crate::material;
use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Param, Project};

/// What a description added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawn {
    pub parts: Vec<String>,
    pub feeds: Vec<String>,
    pub vars: Vec<String>,
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
                None => q.params.push(Param::new(name, &e, "Drawn")),
            }
            out.vars.push(name.clone());
        }
    }
    for part in spec["parts"].as_array().into_iter().flatten() {
        let kind = part["kind"].as_str().ok_or_else(|| anyhow!("A part needs a kind."))?;
        let base = part["name"].as_str().unwrap_or(kind);
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
        q.extra_ports.push(ExtraPort { name: name.clone(), a: end(&feed["a"], "feed a")?, b: end(&feed["b"], "feed b")?, width, z0 });
        out.feeds.push(name);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mom::{MomOptions, MomProblem};
    use serde_json::json;

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
