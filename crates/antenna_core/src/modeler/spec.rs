//! A model written out as JSON, by name rather than by id, with every dimension an expression.
//! This is what the assistant writes: a sketch's shapes are given by what they are, and come out
//! as points held by dimensions, so the result is as editable as anything drawn by hand.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use super::mesh::{FaceId, face_tag};
use super::prims::{END_HIGH, END_LOW};
use super::sketch::{Constraint, CurveKind, ORIGIN, P2, Sketch};
use super::tree::*;
use super::length;
use crate::expr::Vars;

fn text(v: &Value, key: &str) -> Option<String> {
    match &v[key] {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn need(v: &Value, key: &str, what: &str) -> Result<String> {
    text(v, key).ok_or_else(|| anyhow!("{what} needs \"{key}\"."))
}

fn pair(v: &Value, what: &str) -> Result<[String; 2]> {
    let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| anyhow!("{what} is [u, v], two expressions."))?;
    let s = |x: &Value| x.as_str().map(str::to_string).or_else(|| x.as_f64().map(|n| n.to_string())).ok_or_else(|| anyhow!("{what} holds expressions."));
    Ok([s(&a[0])?, s(&a[1])?])
}

fn triple(v: &Value, what: &str) -> Result<[String; 3]> {
    let a = v.as_array().filter(|a| a.len() == 3).ok_or_else(|| anyhow!("{what} is [x, y, z], three expressions."))?;
    let s = |x: &Value| x.as_str().map(str::to_string).or_else(|| x.as_f64().map(|n| n.to_string())).ok_or_else(|| anyhow!("{what} holds expressions."));
    Ok([s(&a[0])?, s(&a[1])?, s(&a[2])?])
}

/// What's known while reading: the model so far, and the names a later step can refer to.
struct Reader<'a> {
    model: &'a mut Model,
    vars: &'a Vars,
    /// Sketch points by name: `(sketch, name) → point`.
    points: Vec<(u32, String, u32)>,
}

fn value_at(s: &str, vars: &Vars) -> f64 {
    length(s, vars).unwrap_or(0.0)
}

fn point_at(q: &[String; 2], vars: &Vars) -> P2 {
    [value_at(&q[0], vars), value_at(&q[1], vars)]
}

/// A point held at two expressions.
fn held_point(s: &mut Sketch, q: &[String; 2], vars: &Vars) -> u32 {
    let p = s.add_point(point_at(q, vars));
    s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: p, value: q[0].clone() });
    s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: p, value: q[1].clone() });
    p
}

impl Reader<'_> {
    /// A body's face by name. On a body from a sketch, `side N` is the sketch's Nth curve, counting from
    /// 0, since that's how a person counts them; its stored name comes from the curve's id.
    fn face_of(&self, body: u32, name: &str) -> Result<FaceId> {
        let n = name.trim();
        let Some(k) = n.strip_prefix("side ").and_then(|k| k.trim().parse::<usize>().ok()) else { return face_named(body, n) };
        let sketch = match self.model.feature(body).map(|f| &f.kind) {
            Some(Kind::Extrude { sketch, .. } | Kind::Revolve { sketch, .. } | Kind::Sweep { sketch, .. }) => Some(*sketch),
            Some(Kind::Loft { sections, .. }) => sections.first().copied(),
            _ => None,
        };
        let Some(sketch) = sketch else { return face_named(body, n) };
        let curve = self.model.sketch(sketch).and_then(|s| s.curves.iter().filter(|c| !c.construction).nth(k)).map(|c| c.id);
        curve.map(|c| face_tag(body, 2 + c)).ok_or_else(|| anyhow!("That sketch has no curve {k} to make 'side {k}' from."))
    }

    fn feature_by_name(&self, name: &str) -> Result<u32> {
        self.model.features.iter().find(|f| f.name == name).map(|f| f.id).ok_or_else(|| anyhow!("There's no step called '{name}' before this one."))
    }

    /// A body by the name of the step that made it, or of a copy in the build.
    fn body_by_name(&self, name: &str) -> Result<u32> {
        self.feature_by_name(name)
    }

    fn sketch_shapes(&mut self, id: u32, shapes: &[Value]) -> Result<Sketch> {
        let mut s = Sketch::default();
        let vars = self.vars;
        for shape in shapes {
            if let Some(r) = shape.get("rect") {
                let c = pair(&r["c"], "A rectangle's c")?;
                let size = pair(&r["size"], "A rectangle's size")?;
                let (cc, sv) = (point_at(&c, vars), point_at(&size, vars));
                let (p, _) = s.add_rect([cc[0] - sv[0] / 2.0, cc[1] - sv[1] / 2.0], [cc[0] + sv[0] / 2.0, cc[1] + sv[1] / 2.0]);
                s.constraints.push(Constraint::DistanceU { a: p[0], b: p[1], value: size[0].clone() });
                s.constraints.push(Constraint::DistanceV { a: p[1], b: p[2], value: size[1].clone() });
                s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: p[0], value: format!("({})-({})/2", c[0], size[0]) });
                s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: p[0], value: format!("({})-({})/2", c[1], size[1]) });
            } else if let Some(r) = shape.get("circle") {
                let c = pair(&r["c"], "A circle's c")?;
                let radius = need(r, "r", "A circle")?;
                let cp = held_point(&mut s, &c, vars);
                let curve = s.add_curve(CurveKind::Circle { c: cp, r: value_at(&radius, vars) });
                s.constraints.push(Constraint::Radius { curve, value: radius });
            } else if let Some(list) = shape.get("poly").and_then(Value::as_array) {
                let pts: Vec<[String; 2]> = list.iter().map(|q| pair(q, "A polygon's corner")).collect::<Result<_>>()?;
                if pts.len() < 2 {
                    bail!("A polygon or path needs at least two points.");
                }
                let ids: Vec<u32> = pts.iter().map(|q| held_point(&mut s, q, vars)).collect();
                for w in ids.windows(2) {
                    s.add_line(w[0], w[1]);
                }
                if shape["open"].as_bool() != Some(true) {
                    if ids.len() < 3 {
                        bail!("A closed polygon needs at least three corners.");
                    }
                    s.add_line(ids[ids.len() - 1], ids[0]);
                }
            } else if let Some(r) = shape.get("arc") {
                let c = pair(&r["c"], "An arc's c")?;
                let (ra, from, to) = (need(r, "r", "An arc")?, need(r, "from_deg", "An arc")?, need(r, "to_deg", "An arc")?);
                let (cc, rr) = (point_at(&c, vars), value_at(&ra, vars));
                let deg = |e: &str| super::angle(e, vars).unwrap_or(0.0);
                let (t0, t1) = (deg(&from), deg(&to));
                let at = |t: f64| [cc[0] + rr * t.cos(), cc[1] + rr * t.sin()];
                let cp = held_point(&mut s, &c, vars);
                let (ap, bp) = (s.add_point(at(t0)), s.add_point(at(t1)));
                let curve = s.add_curve(CurveKind::Arc { c: cp, a: ap, b: bp });
                s.constraints.push(Constraint::Radius { curve, value: ra.clone() });
                s.constraints.push(Constraint::DistanceU { a: cp, b: ap, value: format!("({ra})*cos({})", radian(&from)) });
                s.constraints.push(Constraint::DistanceU { a: cp, b: bp, value: format!("({ra})*cos({})", radian(&to)) });
            } else if let Some(r) = shape.get("curve") {
                // u(t), v(t) as expressions in t: an exponential taper, a parabola.
                let (u, v, t0, t1) = (need(r, "u", "A curve")?, need(r, "v", "A curve")?, need(r, "t0", "A curve")?, need(r, "t1", "A curve")?);
                let (a, b) = (s.add_point([0.0, 0.0]), s.add_point([0.0, 0.0]));
                s.add_curve(CurveKind::Equation { a, b, u, v, t0, t1 });
            } else if let Some(r) = shape.get("point") {
                let at = pair(&r["at"], "A point's at")?;
                let p = held_point(&mut s, &at, vars);
                if let Some(name) = r["name"].as_str() {
                    self.points.push((id, name.to_string(), p));
                }
            } else {
                bail!("A sketch's shapes are rect, circle, poly, arc, curve or point: {shape}");
            }
        }
        let solved = s.solve(vars);
        s.keep(&solved);
        Ok(s)
    }

    /// A flat shape's centre in its plane. Left out: the plane's origin.
    fn centre(&self, v: &Value) -> Result<[String; 2]> {
        if v["c"].is_null() { Ok(["0".to_string(), "0".to_string()]) } else { pair(&v["c"], "A centre \"c\"") }
    }

    fn output(&self, v: &Value) -> Result<Output> {
        let mode = match v["mode"].as_str().unwrap_or("new") {
            "new" => Mode::New,
            "add" => Mode::Add,
            "cut" => Mode::Cut,
            "common" => Mode::Common,
            other => bail!("A mode is new, add, cut or common, not '{other}'."),
        };
        let target = match v["target"].as_str() {
            Some(n) => Some(self.body_by_name(n)?),
            None => None,
        };
        Ok(Output { mode, target, material: v["material"].as_str().unwrap_or("").to_string(), mesh: text(v, "mesh").unwrap_or_default(), surface: v["surface"].as_bool().unwrap_or(false), thick: text(v, "thick").unwrap_or_default(), ..Default::default() })
    }

    fn path(&self, v: &Value) -> Result<PathRef> {
        if let Some(s) = v["sketch"].as_str() {
            return Ok(PathRef::Sketch { sketch: self.feature_by_name(s)? });
        }
        if let Some(h) = v.get("helix") {
            return Ok(PathRef::Helix {
                origin: triple(&h["origin"], "A helix's origin").unwrap_or_else(|_| ["0".into(), "0".into(), "0".into()]),
                axis: triple(&h["axis"], "A helix's axis").unwrap_or_else(|_| ["0".into(), "0".into(), "1".into()]),
                radius: need(h, "radius", "A helix")?,
                radius2: text(h, "radius2").unwrap_or_default(),
                pitch: need(h, "pitch", "A helix")?,
                turns: need(h, "turns", "A helix")?,
                left: h["left"].as_bool().unwrap_or(false),
            });
        }
        if let Some(list) = v["points"].as_array() {
            return Ok(PathRef::Points { pts: list.iter().map(|q| triple(q, "A path's point")).collect::<Result<_>>()? });
        }
        bail!("A path is {{\"sketch\": name}}, {{\"helix\": {{...}}}} or {{\"points\": [...]}}.")
    }

    fn names(&self, v: &Value, key: &str) -> Result<Vec<u32>> {
        v[key].as_array().map_or(Ok(Vec::new()), |l| l.iter().map(|n| n.as_str().ok_or_else(|| anyhow!("\"{key}\" lists names.")).and_then(|s| self.body_by_name(s))).collect())
    }

    fn plane(&self, v: &Value) -> Result<PlaneRef> {
        let offset = text(v, "offset").unwrap_or_else(|| "0".into());
        match v["plane"].as_str().unwrap_or("top") {
            "top" | "xy" => Ok(PlaneRef::Base { axis: 2, offset }),
            "front" | "xz" => Ok(PlaneRef::Base { axis: 1, offset }),
            "right" | "yz" => Ok(PlaneRef::Base { axis: 0, offset }),
            "face" => {
                let body = self.body_by_name(v["body"].as_str().ok_or_else(|| anyhow!("A sketch on a face says which \"body\"."))?)?;
                Ok(PlaneRef::Face { face: self.face_of(body, v["face"].as_str().unwrap_or("top"))?, offset })
            }
            other => bail!("A plane is top, front, right or face, not '{other}'."),
        }
    }

    fn add(&mut self, v: &Value) -> Result<u32> {
        let kind = v["kind"].as_str().ok_or_else(|| anyhow!("Each step needs a \"kind\": {v}"))?;
        let id = self.model.fresh_id();
        let made = self.kind(kind, v, id).with_context(|| format!("the {kind} step{}", v["name"].as_str().map_or(String::new(), |n| format!(" '{n}'"))))?;
        let name = v["name"].as_str().map_or_else(|| self.model.free_name(made.label()), str::to_string);
        if self.model.features.iter().any(|f| f.name == name) {
            bail!("There's already a step called '{name}'.");
        }
        self.model.add_named(id, name, made);
        Ok(id)
    }

    fn kind(&mut self, kind: &str, v: &Value, id: u32) -> Result<Kind> {
        let sketch = |r: &Self| -> Result<u32> { r.feature_by_name(v["sketch"].as_str().ok_or_else(|| anyhow!("It needs a \"sketch\", by name."))?) };
        Ok(match kind {
            "sketch" => {
                let shapes = v["shapes"].as_array().cloned().unwrap_or_default();
                let plane = self.plane(v)?;
                Kind::Sketch { plane, sketch: self.sketch_shapes(id, &shapes)? }
            }
            "extrude" => {
                let extent = match v["dir"].as_str().unwrap_or("one") {
                    "one" | "forward" => Extent::One,
                    "reverse" | "back" => Extent::Reverse,
                    "both" => Extent::Both,
                    "through" => Extent::Through,
                    other => bail!("A direction is one, reverse, both or through, not '{other}'."),
                };
                let depth = if extent == Extent::Through { text(v, "depth").unwrap_or_else(|| "0".into()) } else { need(v, "depth", "An extrusion")? };
                Kind::Extrude { sketch: sketch(self)?, depth, extent, out: self.output(v)? }
            }
            "sheet" => Kind::Sheet { sketch: sketch(self)?, out: self.output(v)? },
            "rectangle" => Kind::Face { plane: self.plane(v)?, shape: Flat::Rect { p0: if v["p0"].is_null() { ["0".to_string(), "0".to_string()] } else { pair(&v["p0"], "A rectangle's p0")? }, size: pair(&v["size"], "A rectangle's size")? }, out: self.output(v)? },
            "circle" =>Kind::Face { plane: self.plane(v)?, shape: Flat::Circle { c: self.centre(v)?, r: need(v, "r", "A circle")? }, out: self.output(v)? },
            "ellipse" => Kind::Face { plane: self.plane(v)?, shape: Flat::Ellipse { c: self.centre(v)?, rx: need(v, "rx", "An ellipse")?, ry: need(v, "ry", "An ellipse")? }, out: self.output(v)? },
            "polygon" => Kind::Face { plane: self.plane(v)?, shape: Flat::Polygon { c: self.centre(v)?, r: need(v, "r", "A polygon")?, sides: need(v, "sides", "A polygon")? }, out: self.output(v)? },
            "revolve" => {
                let axis = match v["axis"].as_str().unwrap_or("v") {
                    "u" => AxisRef::U,
                    "v" => AxisRef::V,
                    other => bail!("A revolve's axis is u or v, not '{other}'."),
                };
                Kind::Revolve { sketch: sketch(self)?, axis, angle: text(v, "angle").unwrap_or_else(|| "360deg".into()), out: self.output(v)? }
            }
            "sweep" => Kind::Sweep { sketch: sketch(self)?, path: self.path(&v["path"])?, out: self.output(v)? },
            "loft" => Kind::Loft { sections: self.names(v, "sections")?, out: self.output(v)? },
            "box" if !v["size"].is_null() => Kind::Solid { shape: Prim::Cuboid { p0: if v["p0"].is_null() { ["0".to_string(), "0".to_string(), "0".to_string()] } else { triple(&v["p0"], "A box's p0")? }, size: triple(&v["size"], "A box's size")? }, out: self.output(v)? },
            "box" => Kind::Solid { shape: Prim::Box { min: triple(&v["min"], "A box's min")?, max: triple(&v["max"], "A box's max")? }, out: self.output(v)? },
            "cylinder" => Kind::Solid { shape: Prim::Cylinder { a: triple(&v["a"], "A cylinder's a")?, b: triple(&v["b"], "A cylinder's b")?, r: need(v, "r", "A cylinder")? }, out: self.output(v)? },
            "cone" => Kind::Solid { shape: Prim::Cone { a: triple(&v["a"], "A cone's a")?, b: triple(&v["b"], "A cone's b")?, r0: need(v, "r0", "A cone")?, r1: need(v, "r1", "A cone")? }, out: self.output(v)? },
            "sphere" => Kind::Solid { shape: Prim::Sphere { c: triple(&v["c"], "A sphere's c")?, r: need(v, "r", "A sphere")? }, out: self.output(v)? },
            "torus" => Kind::Solid { shape: Prim::Torus { c: triple(&v["c"], "A torus's c")?, axis: triple(&v["axis"], "A torus's axis")?, big: need(v, "big", "A torus")?, small: need(v, "small", "A torus")? }, out: self.output(v)? },
            "wire" => Kind::Wire { path: self.path(&v["path"])?, radius: need(v, "r", "A wire")?, out: self.output(v)? },
            "combine" => {
                let op = match v["op"].as_str().unwrap_or("add") {
                    "add" => CombineOp::Add,
                    "cut" => CombineOp::Cut,
                    "common" => CombineOp::Common,
                    "exclude" => CombineOp::Exclude,
                    other => bail!("An op is add, cut, common or exclude, not '{other}'."),
                };
                Kind::Combine { op, target: self.body_by_name(v["target"].as_str().ok_or_else(|| anyhow!("It needs a \"target\"."))?)?, tools: self.names(v, "tools")?, keep_tools: v["keep_tools"].as_bool().unwrap_or(false) }
            }
            "pattern" => Kind::Pattern {
                bodies: self.names(v, "bodies")?,
                layout: Layout::Linear { step: triple(&v["step"], "A pattern's step")?, count: need(v, "count", "A pattern")?, step2: if v.get("step2").is_some() { Some(triple(&v["step2"], "A pattern's step2")?) } else { None }, count2: text(v, "count2").unwrap_or_default() },
                merge: v["merge"].as_bool().unwrap_or(false),
                feeds: self.names(v, "feeds")?,
            },
            "circular" => Kind::Pattern {
                bodies: self.names(v, "bodies")?,
                layout: Layout::Circular { origin: triple(&v["origin"], "A circular pattern's origin")?, axis: triple(&v["axis"], "A circular pattern's axis")?, count: need(v, "count", "A circular pattern")?, total: text(v, "total").unwrap_or_else(|| "360deg".into()) },
                merge: v["merge"].as_bool().unwrap_or(false),
                feeds: self.names(v, "feeds")?,
            },
            "mirror" => Kind::Mirror { bodies: self.names(v, "bodies")?, plane: self.plane(v)?, merge: v["merge"].as_bool().unwrap_or(false) },
            "move" => Kind::Move {
                bodies: self.names(v, "bodies")?,
                by: triple(&v["by"], "A move's by")?,
                origin: if v.get("origin").is_some() { Some(triple(&v["origin"], "A move's origin")?) } else { None },
                axis: if v.get("axis").is_some() { Some(triple(&v["axis"], "A move's axis")?) } else { None },
                angle: text(v, "angle").unwrap_or_default(),
                copy: v["copy"].as_bool().unwrap_or(false),
            },
            "fillet" | "chamfer" => {
                let body = self.body_by_name(v["body"].as_str().ok_or_else(|| anyhow!("It needs a \"body\"."))?)?;
                let edges = v["edges"].as_array().ok_or_else(|| anyhow!("It needs \"edges\": pairs of face names, [\"top\", \"side 1\"]."))?;
                let list: Vec<[FaceId; 2]> = edges
                    .iter()
                    .map(|e| {
                        let p = e.as_array().filter(|a| a.len() == 2).ok_or_else(|| anyhow!("An edge is a pair of face names."))?;
                        Ok([self.face_of(body, p[0].as_str().unwrap_or(""))?, self.face_of(body, p[1].as_str().unwrap_or(""))?])
                    })
                    .collect::<Result<_>>()?;
                Kind::Fillet { body, edges: list, radius: need(v, "r", "A fillet")?, chamfer: kind == "chamfer" }
            }
            "shell" => {
                let body = self.body_by_name(v["body"].as_str().ok_or_else(|| anyhow!("It needs a \"body\"."))?)?;
                let open = v["open"].as_array().map_or(Ok(Vec::new()), |l| l.iter().map(|n| self.face_of(body, n.as_str().unwrap_or(""))).collect::<Result<Vec<_>>>())?;
                Kind::Shell { body, open, thick: need(v, "thick", "A shell")? }
            }
            "thicken" => Kind::Thicken { body: self.body_by_name(v["body"].as_str().ok_or_else(|| anyhow!("It needs a \"body\"."))?)?, thick: need(v, "thick", "A thickening")? },
            "feed" => Kind::Feed { a: self.anchor(&v["a"])?, b: self.anchor(&v["b"])?, width: text(v, "width").unwrap_or_default(), z0: text(v, "z0").unwrap_or_else(|| "50".into()) },
            "probe" => {
                let s = sketch(self)?;
                let name = v["point"].as_str().ok_or_else(|| anyhow!("A probe names a sketch \"point\"."))?;
                let point = self.points.iter().find(|p| p.0 == s && p.1 == name).map(|p| p.2).ok_or_else(|| anyhow!("That sketch has no point called '{name}'."))?;
                Kind::Probe { sketch: s, point, width: text(v, "width").unwrap_or_default(), z0: text(v, "z0").unwrap_or_else(|| "50".into()) }
            }
            other => bail!("There's no kind of step called '{other}'."),
        })
    }

    /// `[x, y, z]`, or `{"sketch": name, "point": name}` so it follows the sketch.
    fn anchor(&self, v: &Value) -> Result<Anchor> {
        if let Some(name) = v["sketch"].as_str() {
            let s = self.feature_by_name(name)?;
            let p = v["point"].as_str().ok_or_else(|| anyhow!("It names a sketch \"point\"."))?;
            let point = self.points.iter().find(|q| q.0 == s && q.1 == p).map(|q| q.2).ok_or_else(|| anyhow!("That sketch has no point called '{p}'."))?;
            return Ok(Anchor::Sketch { sketch: s, point, height: text(v, "height").unwrap_or_default() });
        }
        Ok(Anchor::Xyz { at: triple(v, "A feed's end")? })
    }
}

/// `45` as an angle in an expression: a plain number is degrees here too.
fn radian(e: &str) -> String {
    if e.trim().parse::<f64>().is_ok() { format!("({e})*pi/180") } else { format!("({e})") }
}

/// A face by a name a person would use: `top`, `bottom`, or `side N`.
pub fn face_named(body: u32, name: &str) -> Result<FaceId> {
    let n = name.trim();
    match n {
        "top" | "end" | "high" => Ok(face_tag(body, END_HIGH)),
        "bottom" | "start" | "low" => Ok(face_tag(body, END_LOW)),
        _ => n.strip_prefix("side ").and_then(|k| k.trim().parse::<u32>().ok()).map(|k| face_tag(body, 2 + k)).ok_or_else(|| anyhow!("A face is top, bottom or side N, not '{name}'.")),
    }
}

/// Adds steps from JSON to a model. All or nothing: a step that can't be read leaves it as it was.
pub fn add_steps(model: &mut Model, steps: &[Value], vars: &Vars) -> Result<Vec<u32>> {
    let mut working = model.clone();
    let mut reader = Reader { model: &mut working, vars, points: Vec::new() };
    let mut out = Vec::with_capacity(steps.len());
    for s in steps {
        out.push(reader.add(s)?);
    }
    *model = working;
    Ok(out)
}

/// The steps with what isn't a dimension taken out, to look for numbers no variable can move.
pub fn dimensions_only(steps: &[Value]) -> Value {
    fn strip(v: &Value) -> Value {
        match v {
            Value::Object(o) => Value::Object(o.iter().filter(|(k, _)| !matches!(k.as_str(), "sketch" | "plane" | "dir" | "mode" | "target" | "body" | "bodies" | "tools" | "sections" | "open" | "point" | "feeds" | "op" | "face" | "edges" | "left" | "merge" | "copy" | "surface" | "keep_tools" | "count" | "count2" | "turns")).map(|(k, x)| (k.clone(), strip(x))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(strip).collect()),
            other => other.clone(),
        }
    }
    serde_json::json!({ "parts": steps.iter().map(strip).collect::<Vec<_>>() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> Vars {
        let mut v = Vars::new();
        for (k, x) in [("L", 0.04), ("W", 0.03), ("h", 0.002), ("Fx", 0.005), ("Rh", 0.003)] {
            v.insert(k.into(), x);
        }
        v
    }

    #[test]
    fn a_patch_written_out_builds_and_follows_its_variables() {
        let mut m = Model::default();
        let steps = json!([
            { "kind": "sketch", "name": "Board", "plane": "top", "shapes": [{ "rect": { "c": ["0", "0"], "size": ["L", "W"] } }, { "circle": { "c": ["Fx", "0"], "r": "Rh" } }] },
            { "kind": "extrude", "name": "Substrate", "sketch": "Board", "depth": "h", "material": "FR-4" },
            { "kind": "sketch", "name": "Top metal", "plane": "top", "offset": "h", "shapes": [{ "rect": { "c": ["0", "0"], "size": ["L/2", "W/2"] } }, { "point": { "name": "feed", "at": ["Fx", "0"] } }] },
            { "kind": "sheet", "name": "Patch", "sketch": "Top metal" },
            { "kind": "sketch", "name": "Bottom metal", "plane": "top", "shapes": [{ "rect": { "c": ["0", "0"], "size": ["L", "W"] } }] },
            { "kind": "sheet", "name": "Ground", "sketch": "Bottom metal" },
            { "kind": "probe", "name": "Feed", "sketch": "Top metal", "point": "feed" }
        ]);
        let v = vars();
        add_steps(&mut m, steps.as_array().unwrap(), &v).unwrap();
        let b = m.build(&v);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.bodies.len(), 3);
        let sub = b.bodies.iter().find(|x| x.name == "Substrate").unwrap();
        assert_eq!(sub.material, "FR-4");
        let exact = (0.04 * 0.03 - std::f64::consts::PI * 9e-6) * 0.002;
        assert!((sub.mesh.volume() - exact).abs() / exact < 3e-3);
        assert!((b.ports[0].a[0] - 0.005).abs() < 1e-9, "{:?}", b.ports[0]);
        let mut w = v.clone();
        w.insert("Fx".into(), 0.003);
        assert!((m.build(&w).ports[0].a[0] - 0.003).abs() < 1e-9, "the feed follows the variable");
    }

    #[test]
    fn every_kind_of_step_can_be_written_out() {
        let mut m = Model::default();
        let steps = json!([
            { "kind": "box", "name": "A", "min": ["-L/2", "-W/2", "0"], "max": ["L/2", "W/2", "h*5"] },
            { "kind": "cylinder", "name": "B", "a": ["0", "0", "-1mm"], "b": ["0", "0", "20mm"], "r": "Rh", "mode": "cut", "target": "A" },
            { "kind": "fillet", "name": "C", "body": "A", "edges": [["top", "side 0"]], "r": "1mm" },
            { "kind": "sphere", "name": "D", "c": ["0", "0", "50mm"], "r": "Rh" },
            { "kind": "cone", "name": "E", "a": ["30mm", "0", "0"], "b": ["30mm", "0", "10mm"], "r0": "3mm", "r1": "0" },
            { "kind": "torus", "name": "F", "c": ["-40mm", "0", "0"], "axis": ["0", "0", "1"], "big": "9mm", "small": "1mm" },
            { "kind": "wire", "name": "G", "path": { "helix": { "radius": "5mm", "pitch": "3mm", "turns": "3", "origin": ["60mm", "0", "0"] } }, "r": "0.2mm" },
            { "kind": "move", "name": "H", "bodies": ["D"], "by": ["0", "0", "1mm"] },
            { "kind": "mirror", "name": "I", "bodies": ["E"], "plane": "right" },
            { "kind": "pattern", "name": "J", "bodies": ["F"], "step": ["0", "30mm", "0"], "count": "3" },
            { "kind": "circular", "name": "K", "bodies": ["D"], "origin": ["0", "0", "0"], "axis": ["1", "0", "0"], "count": "2", "total": "360deg" },
            { "kind": "sketch", "name": "P1", "plane": "top", "shapes": [{ "rect": { "c": ["100mm", "0"], "size": ["10mm", "10mm"] } }] },
            { "kind": "sketch", "name": "P2", "plane": "top", "offset": "20mm", "shapes": [{ "rect": { "c": ["100mm", "0"], "size": ["30mm", "20mm"] } }] },
            { "kind": "loft", "name": "L", "sections": ["P1", "P2"] },
            { "kind": "shell", "name": "M", "body": "L", "open": ["top", "bottom"], "thick": "0" },
            { "kind": "sketch", "name": "Q", "plane": "front", "shapes": [{ "poly": [["20mm", "100mm"], ["30mm", "120mm"]], "open": true }] },
            { "kind": "revolve", "name": "R", "sketch": "Q", "axis": "v", "surface": true },
            { "kind": "feed", "name": "S", "a": ["0", "0", "0"], "b": ["0", "0", "1mm"], "z0": "75" }
        ]);
        let v = vars();
        add_steps(&mut m, steps.as_array().unwrap(), &v).unwrap();
        let b = m.build(&v);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.ports.len(), 1);
        assert_eq!(b.ports[0].z0, 75.0);
    }

    #[test]
    fn side_n_means_the_sketch_s_nth_curve() {
        let mut m = Model::default();
        let steps = json!([
            { "kind": "sketch", "name": "S", "plane": "top", "shapes": [{ "rect": { "c": ["0", "0"], "size": ["L", "W"] } }] },
            { "kind": "extrude", "name": "B", "sketch": "S", "depth": "h" },
            { "kind": "shell", "name": "T", "body": "B", "open": ["top", "side 0"], "thick": "0" }
        ]);
        let v = vars();
        add_steps(&mut m, steps.as_array().unwrap(), &v).unwrap();
        let b = m.build(&v);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        // Bottom and three walls left: the top and the wall along the first line are open.
        assert_eq!(b.bodies[0].mesh.face_ids().len(), 4, "{:?}", b.bodies[0].mesh.face_ids());
        let bad = json!([{ "kind": "shell", "name": "U", "body": "B", "open": ["side 9"], "thick": "0" }]);
        assert!(format!("{:#}", add_steps(&mut m, bad.as_array().unwrap(), &v).unwrap_err()).contains("side 9"));
    }

    #[test]
    fn a_bad_step_is_named_and_leaves_the_model_alone() {
        let mut m = Model::default();
        let bad = json!([{ "kind": "box", "name": "A", "min": ["0", "0", "0"], "max": ["1", "1", "1"] }, { "kind": "extrude", "name": "B", "sketch": "Nope", "depth": "1" }]);
        let e = format!("{:#}", add_steps(&mut m, bad.as_array().unwrap(), &vars()).unwrap_err());
        assert!(e.contains("Nope") && e.contains("'B'"), "{e}");
        assert!(m.is_empty(), "all or nothing");
        let twice = json!([{ "kind": "box", "name": "A", "min": ["0", "0", "0"], "max": ["1", "1", "1"] }, { "kind": "sphere", "name": "A", "c": ["0", "0", "0"], "r": "1" }]);
        assert!(format!("{:#}", add_steps(&mut m, twice.as_array().unwrap(), &vars()).unwrap_err()).contains("already"));
    }

    /// The rule the assistant draws by: a dimension is a variable. Numbers no variable can move are
    /// found, and names and settings aren't taken for numbers.
    #[test]
    fn what_cant_be_tuned_is_found() {
        let tied = json!([{ "kind": "sketch", "name": "Side 2", "plane": "top", "offset": "h", "shapes": [{ "rect": { "c": ["0", "0"], "size": ["L", "W"] } }] }, { "kind": "extrude", "name": "Block 1", "sketch": "Side 2", "depth": "h", "dir": "one" }, { "kind": "pattern", "bodies": ["Block 1"], "step": ["L", "0", "0"], "count": "4" }]);
        assert!(crate::freeform::loose_numbers(&dimensions_only(tied.as_array().unwrap())).is_empty());
        let bare = json!([{ "kind": "sketch", "name": "S", "plane": "top", "shapes": [{ "rect": { "c": ["0", "0"], "size": ["40mm", "W"] } }] }, { "kind": "extrude", "sketch": "S", "depth": "1.6mm" }]);
        let found = crate::freeform::loose_numbers(&dimensions_only(bare.as_array().unwrap()));
        let numbers: Vec<&str> = found.iter().map(|l| l.number.as_str()).collect();
        assert_eq!(numbers, ["40mm", "1.6mm"], "{found:?}");
    }
}
