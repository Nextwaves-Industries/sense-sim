//! Assistant / MCP tools for connected systems (`plant_*`), hooked into
//! `agent::tools`. Connections are made port to port and measured, never
//! guessed: placing returns where every port ended up, and `plant_run`
//! simulates the result.

use std::collections::HashSet;

use bevy::prelude::*;
use serde_json::{Value, json};

use super::connect::{self, Link, LinkKind, Links, PortRef, Turn};
use super::parts::Spec;
use super::ports::{self, PortKind, Role};
use super::recipes::{self, Assembly, Namer};
use super::route::{self, Obstacle};
use super::sim::{self, Model, Plant};
use crate::agent::llm::ToolDef;
use crate::agent::tools::{ConnectTo, PlaceItem, Started, ToolOutput, r2, resolve_asset, vec3};
use crate::library::Library;
use crate::prototype::PrototypeCache;

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let v3 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
    vec![
        def(
            "plant_list_ports",
            "Connection points of placed objects (or of an asset before placing it): pipe nozzles, shafts, pulleys and conveyor ends, with world position, outward direction, diameter, what each is linked to, and the object's role (engine, motor, pump, rotor, tank, valve, fitting, conveyor). source tells how a port was found: generated (exact), catalog, detected (from the mesh) or inferred (by convention: check with a screenshot). Call this before connecting anything.",
            json!({ "type": "object", "properties": {
                "objects": { "type": "array", "items": { "type": "string" }, "description": "object names; default: every object that has ports" },
                "asset": { "type": "string", "description": "asset name/path, to see its ports in local coordinates before placing" }
            } }),
        ),
        def(
            "plant_add_part",
            "Add a generated plant part with exact ports, optionally snapped onto a port of a placed object. Kinds: pipe (diameter_m, length_m), elbow (diameter_m, angle_deg 90/45), tee, reducer (diameter_m → diameter2_m), valve (ball valve with lever: 0° open, 90° closed), pump (centrifugal; motor:false gives a bare shaft to drive by coupling/belt), tank (tank_diameter_m, height_m; ports inlet top, outlet and return low), motor (shaft diameter), pulley (pulley_diameter_m, fits a shaft), engine (inline, cylinders 1-8: crank, pistons and rods move; ports flywheel (shaft) and pulley), conveyor (length_m, width_m, height_m; rollers spin, items ride). Ports: pipe a/b (tee also c), valve a/b, pump inlet/outlet (+shaft), tank inlet/outlet/return, motor shaft, pulley shaft/belt, engine flywheel/pulley, conveyor in/out.",
            json!({ "type": "object", "properties": {
                "kind": { "type": "string", "enum": ["pipe", "elbow", "tee", "reducer", "valve", "pump", "tank", "motor", "pulley", "engine", "conveyor"] },
                "diameter_m": { "type": "number", "description": "pipe/nozzle/shaft diameter, default 0.05" },
                "diameter2_m": { "type": "number" }, "length_m": { "type": "number" }, "angle_deg": { "type": "number" },
                "tank_diameter_m": { "type": "number" }, "height_m": { "type": "number" }, "width_m": { "type": "number" },
                "pulley_diameter_m": { "type": "number" }, "cylinders": { "type": "integer" }, "motor": { "type": "boolean" }, "plinth_m": { "type": "number", "description": "pump: height of the concrete plinth (to line the suction up with a tank nozzle)" },
                "connect_to": { "type": "string", "description": "\"object.port\" to snap onto" },
                "my_port": { "type": "string", "description": "which port of the new part mates (default: first that fits)" },
                "aim": v3.clone(),
                "x": { "type": "number" }, "y": { "type": "number" }, "z": { "type": "number" }, "yaw_deg": { "type": "number" },
                "name": { "type": "string" }
            }, "required": ["kind"] }),
        ),
        def(
            "plant_route_pipe",
            "Pipe two pipe ports together: an orthogonal run of generated straight pipes and 90° elbows that leaves each port straight along its direction, avoids other objects, and links every piece. Ports must point along an axis (rotate objects to multiples of 90° first). Returns the pieces, length and elbow count. Sizes must match (add a reducer otherwise).",
            json!({ "type": "object", "properties": {
                "from": { "type": "string", "description": "\"object.port\"" },
                "to": { "type": "string", "description": "\"object.port\"" },
                "diameter_m": { "type": "number", "description": "default: the ports' size" },
                "via": { "type": "array", "items": v3.clone(), "description": "points the run must pass through, e.g. to go round something" }
            }, "required": ["from", "to"] }),
        ),
        def(
            "plant_connect",
            "Link two ports of placed objects. With move (default \"b\") that object is moved/rotated so its port mates exactly with the other; with move \"none\" the ports must already line up (within 5 mm / 2°). Pipe↔pipe (flow), shaft↔shaft (coupling, same speed), pulley↔pulley (belt, speed × diameter ratio; axes parallel, same plane), conveyor_out↔conveyor_in (items transfer).",
            json!({ "type": "object", "properties": {
                "a": { "type": "string", "description": "\"object.port\"" }, "b": { "type": "string" },
                "move": { "type": "string", "enum": ["a", "b", "none"] }, "aim": v3.clone()
            }, "required": ["a", "b"] }),
        ),
        def(
            "plant_disconnect",
            "Remove links: of one port (\"object.port\") or of every port of an object.",
            json!({ "type": "object", "properties": { "port": { "type": "string" }, "object": { "type": "string" } } }),
        ),
        def(
            "plant_build",
            "ONE-SHOT: build a complete, fully connected and working system from generated parts. recipe \"pump_loop\": two tanks, a motor pump and three valves (suction, discharge, throttled gravity return), all piped (diameter_m, tank_diameter_m, height_m). \"engine_drive\": an inline engine (cylinders) driving a coolant pump by belt, a generator on the flywheel and a coolant tank piped to the pump. \"conveyor_line\": count roller conveyors end to end (length_m, width_m, height_m, speed_mps) with boxes on them. Placed at x/z with yaw_deg, avoiding existing objects; everything is switched on.",
            json!({ "type": "object", "properties": {
                "recipe": { "type": "string", "enum": ["pump_loop", "engine_drive", "conveyor_line"] },
                "x": { "type": "number" }, "z": { "type": "number" }, "yaw_deg": { "type": "number" },
                "diameter_m": { "type": "number" }, "tank_diameter_m": { "type": "number" }, "height_m": { "type": "number" },
                "cylinders": { "type": "integer" }, "count": { "type": "integer" }, "length_m": { "type": "number" },
                "width_m": { "type": "number" }, "boxes": { "type": "integer" }, "speed_mps": { "type": "number" },
                "prefix": { "type": "string", "description": "name prefix for the parts" }
            }, "required": ["recipe"] }),
        ),
        def(
            "plant_set",
            "Operate an object: on (engines, motors, pumps with a motor, conveyors), rpm, speed_mps (conveyor), valve_open 0..1 (the lever moves), fill 0..1 (tank level), medium (water, oil, coolant, fuel, chemical: colours the flow), open_ends_supply (its open pipe ends are fed from outside, like a mains: otherwise an open end only spills).",
            json!({ "type": "object", "properties": {
                "object": { "type": "string" }, "on": { "type": "boolean" }, "rpm": { "type": "number" }, "speed_mps": { "type": "number" },
                "valve_open": { "type": "number" }, "fill": { "type": "number" }, "medium": { "type": "string" }, "open_ends_supply": { "type": "boolean" }
            }, "required": ["object"] }),
        ),
        def(
            "plant_run",
            "Simulate the plant for `seconds` (default 30) from its current state and report: machine speeds, pump flow and head, valve flows, tank levels (start/end/min/max), conveyor speeds and what feeds what, open pipe ports, leaks and alarms with the time they appear, the mass balance, and what's assumed rather than known. ok=true when nothing leaks or alarms and the liquid adds up. Not validated on hardware. Also starts the live animation.",
            json!({ "type": "object", "properties": { "seconds": { "type": "number" } } }),
        ),
        def("plant_status", "Live plant state now: speeds, flows, tank levels, alarms, links.", json!({ "type": "object", "properties": {} })),
        def(
            "plant_define_port",
            "Teach the catalog a port of an asset (saved for every copy of it): give a placed object and the port's world position/direction (e.g. measured from a screenshot or plant_list_ports), or an asset with a local position. Use when detection missed a nozzle or shaft.",
            json!({ "type": "object", "properties": {
                "object": { "type": "string" }, "asset": { "type": "string" },
                "id": { "type": "string" }, "kind": { "type": "string", "enum": ["pipe", "shaft", "pulley", "conveyor_in", "conveyor_out"] },
                "position": v3.clone(), "direction": v3, "diameter_m": { "type": "number" },
                "role": { "type": "string", "enum": ["engine", "motor", "pump", "rotor", "tank", "valve", "fitting", "conveyor", "other"] }
            }, "required": ["id", "kind", "position", "direction", "diameter_m"] }),
        ),
    ]
}

fn f(v: &Value, k: &str) -> Option<f32> {
    v[k].as_f64().map(|x| x as f32)
}

fn names_in_scene(world: &mut World) -> HashSet<String> {
    let mut q = world.query::<&crate::scene::PlacedObject>();
    let mut s: HashSet<String> = q.iter(world).map(|o| o.name.clone()).collect();
    s.extend(world.resource::<crate::scene::PendingSpawns>().0.iter().filter_map(|p| p.name.clone()));
    s
}

fn obstacles(model: &Model) -> Vec<Obstacle> {
    use crate::agent::structures::Kind;
    model
        .objs
        .iter()
        .filter(|o| !matches!(Kind::of_asset(&o.asset), Some(Kind::Floor | Kind::Ceiling)))
        .map(|o| {
            let (min, max) = o.world_bounds();
            Obstacle { name: o.name.clone(), min, max }
        })
        .collect()
}

fn ports_json(o: &sim::Obj, links: &Links, plant: &Plant) -> Value {
    let ports: Vec<Value> = o
        .info
        .ports
        .iter()
        .map(|p| {
            let mut v = ports::port_json(p, &o.t);
            let r = PortRef::new(&o.name, &p.id);
            if let Some((l, other)) = links.partner(&r) {
                v["linked_to"] = json!(other.label());
                v["link"] = json!(l.kind.name());
            }
            if let Some(q) = plant.live.flow.port_flow.get(&r) {
                v["flow_out_lps"] = json!(r2(q * 1000.0));
            }
            v
        })
        .collect();
    let mut v = json!({ "object": o.name, "role": o.role.name(), "ports": ports });
    if let Some(s) = o.info.spec {
        v["generated"] = json!(s.kind());
    }
    if o.role.is_source() || o.role == Role::Rotor {
        let w = plant.live.omega.get(&o.name).copied().unwrap_or(0.0);
        v["on"] = json!(plant.control(&o.name).on);
        v[if o.role == Role::Conveyor { "speed_mps" } else { "rpm" }] = json!(if o.role == Role::Conveyor { r2(w) } else { (w * 60.0 / std::f32::consts::TAU).round() as f64 });
    }
    if o.info.ports.is_empty() {
        v["note"] = json!("no ports known: if it should have one, add it with plant_define_port");
    }
    v
}

fn spec_from(i: &Value) -> Result<Spec, String> {
    let d = f(i, "diameter_m").unwrap_or(0.05);
    Ok(match i["kind"].as_str().unwrap_or("") {
        "pipe" => Spec::Pipe { d, len: f(i, "length_m").unwrap_or(1.0) },
        "elbow" => Spec::Elbow { d, angle: f(i, "angle_deg").unwrap_or(90.0) },
        "tee" => Spec::Tee { d },
        "reducer" => Spec::Reducer { d1: d, d2: f(i, "diameter2_m").ok_or("reducer needs diameter2_m")? },
        "valve" => Spec::Valve { d },
        "pump" => Spec::Pump { d, motor: i["motor"].as_bool().unwrap_or(true), base: f(i, "plinth_m").unwrap_or(0.0) },
        "tank" => Spec::Tank { dia: f(i, "tank_diameter_m").unwrap_or(1.0), h: f(i, "height_m").unwrap_or(1.2), d },
        "motor" => Spec::Motor { d: f(i, "diameter_m").unwrap_or(0.03) },
        "pulley" => Spec::Pulley { dia: f(i, "pulley_diameter_m").unwrap_or(0.12), d: f(i, "diameter_m").unwrap_or(0.03) },
        "engine" => Spec::Engine { n: i["cylinders"].as_u64().unwrap_or(4) as u32 },
        "conveyor" => Spec::Conveyor { len: f(i, "length_m").unwrap_or(2.0), w: f(i, "width_m").unwrap_or(0.6), h: f(i, "height_m").unwrap_or(0.8) },
        other => return Err(format!("unknown part kind '{other}'")),
    }
    .normalized())
}

fn assembly_items(a: Assembly, world: &mut World) -> (Vec<PlaceItem>, Value) {
    {
        let mut plant = world.resource_mut::<Plant>();
        for (n, c) in &a.controls {
            plant.controls.insert(n.clone(), c.clone());
        }
        for (n, deg) in &a.levers {
            plant.lever_targets.insert(n.clone(), *deg);
        }
        plant.running = true;
    }
    let mut items: Vec<PlaceItem> = a.items.into_iter().map(|it| PlaceItem { usd: it.usd, transform: it.transform, name: Some(it.name), ..Default::default() }).collect();
    // All links go with the last item: by then every part exists.
    if let Some(last) = items.last_mut() {
        last.links = a.links;
    }
    (items, a.summary)
}

/// Handles a `plant_*` tool.
pub fn start(world: &mut World, name: &str, i: &Value) -> Started {
    match run(world, name, i) {
        Ok(s) => s,
        Err(e) => Started::Done(ToolOutput::err(e)),
    }
}

fn parse_ref(i: &Value, k: &str) -> Result<PortRef, String> {
    let s = i[k].as_str().unwrap_or("");
    PortRef::parse(s).ok_or_else(|| format!("{k} must be \"object.port\", got '{s}'"))
}

fn run(world: &mut World, name: &str, i: &Value) -> Result<Started, String> {
    let done = |v: Value| Ok(Started::Done(ToolOutput::ok(v)));
    match name {
        "plant_list_ports" => {
            if let Some(asset) = i["asset"].as_str().filter(|s| !s.is_empty()) {
                let usd = resolve_asset(world.resource::<Library>(), asset)?;
                let u = usd.clone();
                return Ok(Started::After {
                    usds: vec![usd],
                    then: Box::new(move |world: &mut World| {
                        let Some(p) = world.resource::<PrototypeCache>().get(&u) else {
                            return Started::Done(ToolOutput::err("the asset failed to load"));
                        };
                        let info = ports::info(&u, &p);
                        let t = Transform::IDENTITY;
                        let size = p.aabb_max - p.aabb_min;
                        Started::Done(ToolOutput::ok(json!({
                            "asset": u.to_string_lossy().replace('\\', "/"),
                            "role": info.role.name(),
                            "size_m": [r2(size.x), r2(size.y), r2(size.z)],
                            "local_ports": info.ports.iter().map(|q| ports::port_json(q, &t)).collect::<Vec<_>>(),
                            "note": "local frame: Y up, origin at the bottom center; placing at x/z with yaw_deg rotates these about Y",
                        })))
                    }),
                });
            }
            let model = Model::from_world(world);
            let links = world.resource::<Links>();
            let plant = world.resource::<Plant>();
            let wanted: Option<Vec<String>> = i["objects"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect());
            let mut out = Vec::new();
            let mut missing = Vec::new();
            match &wanted {
                Some(names) => {
                    for n in names {
                        match model.get(n) {
                            Some(o) => out.push(ports_json(o, links, plant)),
                            None => missing.push(n.clone()),
                        }
                    }
                }
                None => {
                    for o in model.objs.iter().filter(|o| !o.info.ports.is_empty()).take(80) {
                        out.push(ports_json(o, links, plant));
                    }
                }
            }
            let mut v = json!({ "objects": out, "links": links.0.len() });
            if !missing.is_empty() {
                v["unknown_objects"] = json!(missing);
            }
            done(v)
        }
        "plant_add_part" => {
            let spec = spec_from(i)?;
            let usd = spec.usd().map_err(|e| format!("could not write the part: {e}"))?;
            let yaw = f(i, "yaw_deg").unwrap_or(0.0).to_radians();
            let t = Transform::from_xyz(f(i, "x").unwrap_or(0.0), f(i, "y").unwrap_or(0.0), f(i, "z").unwrap_or(0.0)).with_rotation(Quat::from_rotation_y(yaw));
            let connect = match i["connect_to"].as_str().filter(|s| !s.is_empty()) {
                Some(s) => Some(ConnectTo {
                    target: PortRef::parse(s).ok_or_else(|| format!("connect_to must be \"object.port\", got '{s}'"))?,
                    my_port: i["my_port"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                    aim: vec3(&i["aim"]),
                }),
                None => None,
            };
            let name = i["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string).or_else(|| {
                let taken = names_in_scene(world);
                Some(Namer::new("", taken).name(spec.kind()))
            });
            let ports: Vec<Value> = spec.ports().iter().map(|p| json!({ "id": p.id, "kind": p.kind.name(), "diameter_m": r2(p.size) })).collect();
            Ok(Started::Place { items: vec![PlaceItem { usd, transform: t, name, connect, ..Default::default() }], extra: json!({ "part": spec.kind(), "ports": ports }) })
        }
        "plant_route_pipe" => {
            let (from, to) = (parse_ref(i, "from")?, parse_ref(i, "to")?);
            let model = Model::from_world(world);
            let (p0, d0, pa) = model.port_world(&from).ok_or_else(|| format!("no port {} (see plant_list_ports)", from.label()))?;
            let (p1, d1, pb) = model.port_world(&to).ok_or_else(|| format!("no port {} (see plant_list_ports)", to.label()))?;
            if pa.kind != PortKind::Pipe || pb.kind != PortKind::Pipe {
                return Err("both ends must be pipe ports".into());
            }
            let d = match f(i, "diameter_m") {
                Some(d) => d,
                None => {
                    if pa.size.max(pb.size) > pa.size.min(pb.size) * 1.15 + 0.002 {
                        return Err(format!(
                            "the ports differ in size ({:.0} vs {:.0} mm): add a reducer on one (plant_add_part kind reducer) and route from it",
                            pa.size * 1000.0,
                            pb.size * 1000.0
                        ));
                    }
                    pa.size
                }
            };
            let links = world.resource::<Links>();
            for r in [&from, &to] {
                if let Some((_, o)) = links.partner(r) {
                    return Err(format!("{} is already linked to {}: plant_disconnect it first", r.label(), o.label()));
                }
            }
            let via: Vec<Vec3> = i["via"].as_array().into_iter().flatten().filter_map(vec3).collect();
            let obs = obstacles(&model);
            let mut namer = Namer::new("", names_in_scene(world));
            let mut nm = |k: &str| namer.name(k);
            let r = route::plan((&from, p0, d0), (&to, p1, d1), d, &via, &obs, &mut nm)?;
            let mut items: Vec<PlaceItem> = r
                .pieces
                .into_iter()
                .map(|p| PlaceItem { usd: p.spec.usd().unwrap_or_default(), transform: p.transform, name: Some(p.name), ..Default::default() })
                .collect();
            if items.iter().any(|it| it.usd.as_os_str().is_empty()) {
                return Err("could not write the pipe parts".into());
            }
            if let Some(last) = items.last_mut() {
                last.links = r.links;
            } else {
                // Ports already face each other at the right distance: just link them.
                world.resource_mut::<Links>().add(Link { a: from.clone(), b: to.clone(), kind: LinkKind::Pipe });
                return done(json!({ "linked": [from.label(), to.label()], "pieces": 0 }));
            }
            Ok(Started::Place {
                items,
                extra: json!({
                    "route": { "from": from.label(), "to": to.label(), "diameter_m": d, "length_m": r2(r.length), "elbows": r.elbows,
                               "corners": r.points.iter().map(|p| [r2(p.x), r2(p.y), r2(p.z)]).collect::<Vec<_>>() }
                }),
            })
        }
        "plant_connect" => {
            let (a, b) = (parse_ref(i, "a")?, parse_ref(i, "b")?);
            let mv = i["move"].as_str().unwrap_or("b");
            let model = Model::from_world(world);
            let (pa, da, porta) = model.port_world(&a).ok_or_else(|| format!("no port {}", a.label()))?;
            let (pb, db, portb) = model.port_world(&b).ok_or_else(|| format!("no port {}", b.label()))?;
            let kind = connect::link_kind(porta, portb)?;
            let (fixed_ref, moving_ref) = if mv == "a" { (&b, &a) } else { (&a, &b) };
            let mut moved = None;
            if mv != "none" && kind != LinkKind::Belt {
                let (tp, td, _) = model.port_world(fixed_ref).unwrap();
                let o = model.get(&moving_ref.object).unwrap();
                if world.get::<crate::scene::PlacedObject>(o.entity).is_some_and(|p| p.locked) {
                    return Err(format!("{} is locked: ask the user to unlock it, or move the other end", o.name));
                }
                let mine = o.port(&moving_ref.port).unwrap();
                let other = o.info.ports.iter().find(|q| q.id != mine.id && q.kind == mine.kind);
                let free = o.info.spec.is_some() && matches!(o.role, Role::Fitting | Role::Valve | Role::Rotor);
                let turn = if free { Turn::Free { aim: vec3(&i["aim"]) } } else { Turn::Yaw };
                let t = connect::snap(tp, td, kind, mine, other, &o.t, turn)?;
                if let Some(mut tr) = world.get_mut::<Transform>(o.entity) {
                    *tr = t;
                }
                // Piping hangs on its connections; equipment keeps standing.
                if free || t.translation.y > 0.01 {
                    world.entity_mut(o.entity).insert(crate::scene::Floating);
                }
                moved = Some(json!({ "object": o.name, "position": [r2(t.translation.x), r2(t.translation.y), r2(t.translation.z)] }));
            } else {
                let (e, ang) = connect::misalignment(kind, (pa, da), (pb, db));
                let ok = if kind == LinkKind::Belt { ang < 2.0 || ang > 178.0 } else { e < 0.005 && ang < 2.0 };
                if !ok {
                    return Err(format!("the ports don't line up ({:.0} mm, {:.1}°): use move \"b\" to snap", e * 1000.0, ang));
                }
                if kind == LinkKind::Belt && e > 0.01 {
                    return Err(format!("the pulleys are {:.0} mm out of plane: move one along its axis", e * 1000.0));
                }
            }
            let link = Link { a: a.clone(), b: b.clone(), kind };
            let replaced: Vec<String> = {
                let mut links = world.resource_mut::<Links>();
                let r = links.0.iter().filter(|l| l.other(&a).is_some() || l.other(&b).is_some()).map(|l| format!("{} ↔ {}", l.a.label(), l.b.label())).collect();
                links.add(link);
                r
            };
            // Other links of a moved object may no longer line up.
            let mut broken = Vec::new();
            if let Some(m) = &moved {
                let name = m["object"].as_str().unwrap_or("").to_string();
                let model = Model::from_world(world);
                for l in world.resource::<Links>().0.iter().filter(|l| l.touches(&name) && !(l.other(&a).is_some() && l.other(&b).is_some())) {
                    if let (Some(x), Some(y)) = (model.port_world(&l.a), model.port_world(&l.b)) {
                        let (e, ang) = connect::misalignment(l.kind, (x.0, x.1), (y.0, y.1));
                        if e > 0.005 || (ang > 2.0 && l.kind != LinkKind::Belt) {
                            broken.push(format!("{} ↔ {} now {:.0} mm / {:.0}° apart", l.a.label(), l.b.label(), e * 1000.0, ang));
                        }
                    }
                }
            }
            let mut v = json!({ "linked": [a.label(), b.label()], "kind": kind.name() });
            if let Some(m) = moved {
                v["moved"] = m;
            }
            if !replaced.is_empty() {
                v["replaced_links"] = json!(replaced);
            }
            if !broken.is_empty() {
                v["warning"] = json!(broken);
            }
            done(v)
        }
        "plant_disconnect" => {
            let mut links = world.resource_mut::<Links>();
            let n = if let Some(p) = i["port"].as_str().and_then(PortRef::parse) {
                links.remove_port(&p)
            } else if let Some(o) = i["object"].as_str() {
                links.remove_object(o)
            } else {
                return Err("give port or object".into());
            };
            done(json!({ "removed_links": n }))
        }
        "plant_build" => {
            let recipe = i["recipe"].as_str().unwrap_or("");
            let model = Model::from_world(world);
            let obs = obstacles(&model);
            let at = Vec3::new(f(i, "x").unwrap_or(0.0), 0.0, f(i, "z").unwrap_or(0.0));
            let yaw = f(i, "yaw_deg").unwrap_or(0.0).to_radians();
            let prefix = i["prefix"].as_str().unwrap_or("").to_string();
            let mut namer = Namer::new(&prefix, names_in_scene(world));
            // Recipes are built at the origin and moved: obstacles go into recipe space.
            let to_local = Transform::from_translation(at).with_rotation(Quat::from_rotation_y(yaw)).compute_affine().inverse();
            let local_obs: Vec<Obstacle> = obs
                .iter()
                .map(|o| {
                    let (a, b) = recipes::world_box(&Transform::from_matrix(Mat4::from(to_local)), (o.min, o.max));
                    Obstacle { name: o.name.clone(), min: a, max: b }
                })
                .collect();
            let a = match recipe {
                "pump_loop" => recipes::pump_loop(&mut namer, at, yaw, f(i, "diameter_m").unwrap_or(0.05), f(i, "tank_diameter_m").unwrap_or(1.0), f(i, "height_m").unwrap_or(1.2), &local_obs),
                "engine_drive" => recipes::engine_drive(&mut namer, at, yaw, i["cylinders"].as_u64().unwrap_or(4) as u32, &local_obs),
                "conveyor_line" => recipes::conveyor_line(
                    &mut namer,
                    at,
                    yaw,
                    i["count"].as_u64().unwrap_or(3) as usize,
                    f(i, "length_m").unwrap_or(2.0),
                    f(i, "width_m").unwrap_or(0.6),
                    f(i, "height_m").unwrap_or(0.8),
                    i["boxes"].as_u64().unwrap_or(3) as usize,
                    f(i, "speed_mps").unwrap_or(0.4),
                ),
                other => return Err(format!("unknown recipe '{other}' (pump_loop, engine_drive, conveyor_line)")),
            }
            .map_err(|e| format!("{e:#}"))?;
            // Refuse to build on top of existing objects.
            let mut clashes = Vec::new();
            for it in a.items.iter().filter(|it| it.spec.is_some_and(|s| !matches!(s.role(), Role::Fitting | Role::Valve))) {
                let (mn, mx) = recipes::world_box(&it.transform, recipes::spec_bounds(&it.spec.unwrap()));
                for o in &obs {
                    if mn.x < o.max.x && mx.x > o.min.x && mn.z < o.max.z && mx.z > o.min.z && mn.y < o.max.y && mx.y > o.min.y {
                        clashes.push(format!("{} would overlap {}", it.name, o.name));
                    }
                }
            }
            if !clashes.is_empty() {
                clashes.truncate(6);
                return Err(format!("not built: {}. Pick another x/z (get_scene shows what is where).", clashes.join("; ")));
            }
            let (items, summary) = assembly_items(a, world);
            Ok(Started::Place { items, extra: json!({ "recipe": recipe, "system": summary, "next": "plant_run to check it works" }) })
        }
        "plant_set" => {
            let obj = i["object"].as_str().unwrap_or("").to_string();
            let model = Model::from_world(world);
            let o = model.get(&obj).ok_or_else(|| format!("no object named '{obj}'"))?;
            let role = o.role;
            let lever = o.proto.joints.iter().any(|j| j.name.to_ascii_lowercase().contains("lever"));
            let mut plant = world.resource_mut::<Plant>();
            {
                let c = plant.controls.entry(obj.clone()).or_default();
                if let Some(on) = i["on"].as_bool() {
                    c.on = on;
                }
                if let Some(r) = f(i, "rpm") {
                    c.rpm = Some(r.max(0.0));
                }
                if let Some(s) = f(i, "speed_mps") {
                    c.speed_mps = Some(s);
                }
                if let Some(fl) = f(i, "fill") {
                    c.fill = Some(fl.clamp(0.0, 1.0));
                }
                if let Some(m) = i["medium"].as_str() {
                    c.medium = Some(m.to_string());
                }
                if let Some(v) = f(i, "valve_open") {
                    c.valve_open = Some(v.clamp(0.0, 1.0));
                }
                if let Some(s) = i["open_ends_supply"].as_bool() {
                    c.open_ends_supply = s;
                }
            }
            let mut valve_note = None;
            if let Some(v) = f(i, "valve_open")
                && lever
            {
                plant.lever_targets.insert(obj.clone(), (1.0 - v.clamp(0.0, 1.0)) * 90.0);
                valve_note = Some("the lever is turning");
            }
            let c = plant.control(&obj);
            if let Some(fl) = f(i, "fill") {
                if role == Role::Tank {
                    let g = sim::tank_geom(o);
                    plant.live.volume.insert(obj.clone(), fl.clamp(0.0, 1.0) * g.capacity);
                }
            }
            plant.running = true;
            let mut v = json!({ "object": obj, "role": role.name(), "control": c });
            if let Some(n) = valve_note {
                v["note"] = json!(n);
            }
            if c.on && !(role.is_source()) {
                v["warning"] = json!(format!("a {} does not run by itself: drive it through a shaft or belt", role.name()));
            }
            done(v)
        }
        "plant_run" => {
            let secs = f(i, "seconds").unwrap_or(30.0).clamp(1.0, 1200.0);
            let model = Model::from_world(world);
            let links = world.resource::<Links>().clone();
            world.resource_mut::<Plant>().running = true;
            let plant = world.resource::<Plant>();
            let report = sim::simulate(&model, &links, plant, secs);
            Ok(Started::Done(ToolOutput { is_error: false, ..ToolOutput::ok(report) }))
        }
        "plant_status" => {
            let model = Model::from_world(world);
            let links = world.resource::<Links>();
            let plant = world.resource::<Plant>();
            let mut machines = Vec::new();
            for o in &model.objs {
                let w = plant.live.omega.get(&o.name).copied().unwrap_or(0.0);
                if w != 0.0 {
                    machines.push(json!({ "name": o.name, "role": o.role.name(), "rpm_or_mps": if o.role == Role::Conveyor { r2(w) } else { (w * 60.0 / std::f32::consts::TAU).round() as f64 } }));
                }
            }
            let tanks: Vec<Value> = model
                .objs
                .iter()
                .filter(|o| o.role == Role::Tank)
                .map(|o| {
                    let g = sim::tank_geom(o);
                    let v = plant.live.volume.get(&o.name).copied().unwrap_or_else(|| sim::initial_volume(o, &plant.controls));
                    json!({ "name": o.name, "level_pct": r2(v / g.capacity.max(1e-6) * 100.0), "liters": r2(v * 1000.0) })
                })
                .collect();
            let pumps: Vec<Value> = plant.live.flow.pumps.iter().map(|(n, (q, h))| json!({ "name": n, "flow_lps": r2(q * 1000.0), "head_m": r2(*h) })).collect();
            done(json!({
                "running": plant.running, "time_s": r2(plant.live.time as f32), "moving": machines, "pumps": pumps, "tanks": tanks,
                "links": links.0.iter().map(|l| format!("{} ↔ {} ({})", l.a.label(), l.b.label(), l.kind.name())).take(60).collect::<Vec<_>>(),
                "alarms": plant.live.alarms,
                "evidence": "Simulated from hand-checked physics: not validated on hardware",
            }))
        }
        "plant_define_port" => {
            let kind = i["kind"].as_str().and_then(PortKind::parse).ok_or("kind must be pipe, shaft, pulley, conveyor_in or conveyor_out")?;
            let pos = vec3(&i["position"]).ok_or("position [x,y,z] required")?;
            let dir = vec3(&i["direction"]).ok_or("direction [x,y,z] required")?;
            let (asset, local_pos, local_dir) = if let Some(obj) = i["object"].as_str() {
                let model = Model::from_world(world);
                let o = model.get(obj).ok_or_else(|| format!("no object named '{obj}'"))?;
                let inv = o.t.compute_affine().inverse();
                (o.asset.clone(), inv.transform_point3(pos), (o.t.rotation.inverse() * dir).normalize_or(Vec3::X))
            } else {
                let a = i["asset"].as_str().ok_or("give object or asset")?;
                (resolve_asset(world.resource::<Library>(), a)?, pos, dir.normalize_or(Vec3::X))
            };
            if Spec::of_asset(&asset).is_some() {
                return Err("generated parts already have exact ports".into());
            }
            let role = i["role"].as_str().and_then(|r| serde_json::from_value::<Role>(json!(r)).ok());
            let folder = ports::folder_of(&asset);
            let port = ports::CatalogPort {
                id: i["id"].as_str().unwrap_or("p").to_string(),
                kind: kind.name().to_string(),
                pos: Some(local_pos.to_array()),
                at: None,
                dir: local_dir.to_array(),
                diameter_m: f(i, "diameter_m").unwrap_or(0.05),
            };
            ports::define_port(&folder, port, role).map_err(|e| format!("could not save: {e:#}"))?;
            done(json!({ "asset": folder, "saved": "user catalog (%APPDATA%/SenseSim/plant/catalog.json)", "local_position": [r2(local_pos.x), r2(local_pos.y), r2(local_pos.z)] }))
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

/// Connection problems, for `verify_scene` and the headless test.
pub fn check_issues(world: &mut World) -> Vec<String> {
    let model = Model::from_world(world);
    let links = world.resource::<Links>().clone();
    let mut issues = Vec::new();
    if links.0.is_empty() && model.objs.iter().all(|o| o.info.spec.is_none()) {
        return issues;
    }
    for l in &links.0 {
        match (model.port_world(&l.a), model.port_world(&l.b)) {
            (Some(a), Some(b)) => {
                let (e, ang) = connect::misalignment(l.kind, (a.0, a.1), (b.0, b.1));
                let bad = match l.kind {
                    LinkKind::Belt => (ang > 2.0 && ang < 178.0) || e > 0.01,
                    _ => e > 0.005 || ang > 2.0,
                };
                if bad {
                    issues.push(format!("{} and {} are linked but {:.0} mm / {:.1}° apart: reconnect with plant_connect (move b)", l.a.label(), l.b.label(), e * 1000.0, ang));
                }
                if l.kind == LinkKind::Pipe && a.2.size.max(b.2.size) > a.2.size.min(b.2.size) * 1.15 + 0.002 {
                    issues.push(format!("{} ({:.0} mm) is joined to {} ({:.0} mm) without a reducer", l.a.label(), a.2.size * 1000.0, l.b.label(), b.2.size * 1000.0));
                }
            }
            _ => {
                if model.get(&l.a.object).is_some() && model.get(&l.b.object).is_some() {
                    issues.push(format!("link {} ↔ {} names a port that does not exist", l.a.label(), l.b.label()));
                }
            }
        }
    }
    // Open pipe ends on piping and pumps.
    for o in model.objs.iter().filter(|o| matches!(o.role, Role::Fitting | Role::Valve | Role::Pump)) {
        for p in o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe) {
            let r = PortRef::new(&o.name, &p.id);
            if links.partner(&r).is_none() {
                issues.push(format!("open pipe end at {} (it would leak): connect it or cap it with a valve", r.label()));
            }
        }
        if let Some(Spec::Pump { motor: false, .. }) = o.info.spec {
            let driven = links.partner(&PortRef::new(&o.name, "shaft")).is_some();
            if !driven {
                issues.push(format!("pump {} has no motor and nothing drives its shaft: couple a motor/engine or a pulley", o.name));
            }
        }
    }
    // Conveyors that end at another conveyor's start without a link.
    let belts: Vec<&sim::Obj> = model.objs.iter().filter(|o| o.role == Role::Conveyor).collect();
    for a in &belts {
        let Some(out) = a.info.ports.iter().find(|p| p.kind == PortKind::ConveyorOut) else { continue };
        let ra = PortRef::new(&a.name, &out.id);
        if links.partner(&ra).is_some() {
            continue;
        }
        let (pa, _) = out.world(&a.t);
        for b in belts.iter().filter(|b| b.name != a.name) {
            if let Some(inp) = b.info.ports.iter().find(|p| p.kind == PortKind::ConveyorIn) {
                let (pb, _) = inp.world(&b.t);
                if pa.distance(pb) < 0.5 {
                    issues.push(format!("{} ends {:.0} cm from the start of {} but they are not linked: plant_connect {}.{} {}.{}", a.name, pa.distance(pb) * 100.0, b.name, a.name, out.id, b.name, inp.id));
                }
            }
        }
    }
    // Generated pipes running through other objects.
    for o in model.objs.iter().filter(|o| matches!(o.info.spec, Some(Spec::Pipe { .. } | Spec::Elbow { .. }))) {
        let (mn, mx) = o.world_bounds();
        let vol = (mx - mn).element_product().max(1e-9);
        for b in &model.objs {
            if b.name == o.name || matches!(b.role, Role::Fitting | Role::Valve) || links.0.iter().any(|l| l.touches(&o.name) && l.touches(&b.name)) {
                continue;
            }
            if matches!(crate::agent::structures::Kind::of_asset(&b.asset), Some(crate::agent::structures::Kind::Floor | crate::agent::structures::Kind::Ceiling)) {
                continue;
            }
            let (bn, bx) = b.world_bounds();
            let ov = (mx.min(bx) - mn.max(bn)).max(Vec3::ZERO).element_product();
            if ov > 0.25 * vol {
                issues.push(format!("{} runs through {}: re-route (plant_disconnect, delete the run, plant_route_pipe with via points)", o.name, b.name));
            }
        }
    }
    // What the running plant would report.
    let plant = world.resource::<Plant>();
    if plant.controls.values().any(|c| c.on) {
        let rep = sim::simulate(&model, &links, plant, 10.0);
        for a in rep["alarms"].as_array().into_iter().flatten() {
            if let Some(s) = a["alarm"].as_str()
                && !issues.iter().any(|x| x.contains(s))
                && !s.starts_with("leak")
            {
                issues.push(format!("plant: {s}"));
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_specs_parse() {
        let s = spec_from(&json!({ "kind": "reducer", "diameter_m": 0.08, "diameter2_m": 0.05 })).unwrap();
        assert_eq!(s, Spec::Reducer { d1: 0.08, d2: 0.05 });
        assert!(spec_from(&json!({ "kind": "reducer" })).is_err());
        assert!(matches!(spec_from(&json!({ "kind": "engine", "cylinders": 6 })).unwrap(), Spec::Engine { n: 6 }));

    }
}
