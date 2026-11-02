//! Assistant and MCP tools for 3D design (`design_*`) and simulation (`sim_*`), hooked into
//! `agent::tools`. A study made here is the same study the panel shows: the user can see it, change it
//! and run it. Installing solvers and opening or saving files stay with the user.

use std::sync::Arc;

use bevy::prelude::*;
use serde_json::{Value, json};
use sim_core::catalog::{self, AnalysisId, Needs, Readiness};
use sim_core::geometry::Partition;
use sim_core::jobs::State;
use sim_core::materials;
use sim_core::post;
use sim_core::results::{ResultSet, StepKind};
use sim_core::study::{BcKind, Condition, FaceRef, Settings, Study};
use solidcraft_engine::io::MeshQuality;

use super::{SimState, panel};
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;
use crate::design::{self, Slot, Workspace, bridge};

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    vec![
        def(
            "design_commands",
            "List the parametric CAD commands that design_run can run: sketches, extrudes, fillets, booleans and hundreds more. Each has an id, a label and its parameters. Filter by a word, such as 'extrude' or 'sketch.circle'.",
            json!({ "type": "object", "properties": { "filter": { "type": "string" }, "limit": { "type": "integer", "description": "default 40" } } }),
        ),
        def(
            "design_run",
            "Run CAD commands in the design workspace, in order, stopping at the first error. Lengths are in millimetres unless a unit is given. Each step can be undone by the user. A typical part: sketch.create {plane:'XY'}, sketch.rectangle.two_point {p0:[0,0],p1:[100,50]}, sketch.finish, solid.extrude {distance:'20 mm',body_name:'Plate'}. Commands that open or write files are refused: files are the user's.",
            json!({ "type": "object", "properties": {
                "commands": { "type": "array", "items": { "type": "object", "properties": { "command": { "type": "string" }, "params": { "type": "object" } }, "required": ["command"] } }
            }, "required": ["commands"] }),
        ),
        def(
            "design_bodies",
            "The design's bodies, each with its size in metres and whether it's a closed solid.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "design_faces",
            "A body's faces, with area, centre and outward direction, in metres. A body can be a design body's name, or 'scene:NAME' for an object in the scene. Use these to choose which faces a simulation's conditions apply to.",
            json!({ "type": "object", "properties": { "body": { "type": "string" } }, "required": ["body"] }),
        ),
        def(
            "design_show",
            "Show the design workspace to the user, or go back to the 3D scene.",
            json!({ "type": "object", "properties": { "show": { "type": "boolean" } }, "required": ["show"] }),
        ),
        def(
            "design_send_to_scene",
            "Place every design body in the 3D scene as a USD object, side by side, so it can be moved, used for radio occlusion or simulated.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "sim_catalog",
            "Every analysis type: its id, what it does and can't do, the solver behind it, how far it can be trusted (verified: its benchmarks pass, beta: not all of them), whether it can run on this PC now, and what's missing if not. Also the materials on offer.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "sim_studies",
            "The simulation studies set up so far, with what each still needs before it can run.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "sim_study_create",
            "Make a new simulation study. Returns its index, what it still needs, and the conditions it can take. Then set it up with sim_study_set and start it with sim_run.",
            json!({ "type": "object", "properties": {
                "analysis": { "type": "string", "description": "an id from sim_catalog, such as 'static'" },
                "name": { "type": "string" }
            }, "required": ["analysis"] }),
        ),
        def(
            "sim_study_set",
            "Change a study. Anything left out stays as it is. conditions replaces all of them. Each has a kind and the face numbers it applies to, from design_faces. Kinds: {type:'fixed'}, {type:'force',force:[x,y,z]} in N, {type:'pressure',pressure} in Pa, {type:'temperature',value} in C, {type:'heat_flux',flux} in W/m2, {type:'convection',coefficient,ambient}, {type:'velocity_inlet',speed} in m/s, {type:'pressure_outlet',pressure}, {type:'wall'}, {type:'potential',value} in V, {type:'displacement',displacement:[x,y,z]} in m, {type:'contact',friction}. For contact, set second_body too, and give the touching faces on each body: faces on the first, second_faces on the second. Each body needs a fixed support or a displacement, because a force alone leaves it floating. settings are given by name: element_size (share of the body's diagonal), modes, f_min, f_max, points, damping, dt, duration, transient, initial_temperature, reference_temperature, load_increment, free_stream, resolution, directions, fill_fraction.",
            json!({ "type": "object", "properties": {
                "study": { "type": "integer" },
                "name": { "type": "string" },
                "body": { "type": "string", "description": "a design body's name, or 'scene:NAME' for an object in the scene" },
                "second_body": { "type": "string", "description": "Nonlinear only: a second design body, touching the first" },
                "material": { "type": "string" },
                "fluid": { "type": "string" },
                "settings": { "type": "object" },
                "conditions": { "type": "array", "items": { "type": "object", "properties": { "kind": { "type": "object" }, "faces": { "type": "array", "items": { "type": "integer" } }, "second_faces": { "type": "array", "items": { "type": "integer" } } }, "required": ["kind", "faces"] } }
            }, "required": ["study"] }),
        ),
        def(
            "sim_run",
            "Start a study's run. Returns a job number at once: the run goes on in the background, and sim_jobs follows it. Refused, with the reason, if the study isn't ready or its solvers aren't installed. Only the user can install solvers, from the Simulations panel: ask them to.",
            json!({ "type": "object", "properties": { "study": { "type": "integer" } }, "required": ["study"] }),
        ),
        def(
            "sim_jobs",
            "The runs so far: state, stage and progress, the error of a failed one, and optionally the end of a run's log.",
            json!({ "type": "object", "properties": { "job": { "type": "integer", "description": "just this one" }, "log_lines": { "type": "integer" } } }),
        ),
        def(
            "sim_cancel",
            "Stop a run.",
            json!({ "type": "object", "properties": { "job": { "type": "integer" } }, "required": ["job"] }),
        ),
        def(
            "sim_check_faces",
            "Before running: what each condition really lands on, as its number of faces and its area in m², and the faces with no condition. Compare the areas with what was meant. It's an error if a pick has gone stale after an edit to the design. show:true also colours the body by condition in the 3D view, for the user and for screenshots.",
            json!({ "type": "object", "properties": { "study": { "type": "integer" }, "show": { "type": "boolean" } }, "required": ["study"] }),
        ),
        def(
            "sim_results",
            "A finished run's results: each step with its time or frequency, each field's unit and range, and the plots. show:true opens them in the 3D view for the user, and for screenshots.",
            json!({ "type": "object", "properties": { "job": { "type": "integer" }, "show": { "type": "boolean" } }, "required": ["job"] }),
        ),
        def(
            "sim_probe",
            "A result field's value at a point in metres, in the solvers' frame (Z up), interpolated from the mesh when the point is inside it. Without step, the last one.",
            json!({ "type": "object", "properties": {
                "job": { "type": "integer" }, "field": { "type": "string" }, "step": { "type": "integer" },
                "point": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 }
            }, "required": ["job", "field", "point"] }),
        ),
    ]
}

/// Commands that read or write files. Those stay with the user: a tool call shouldn't overwrite their
/// work, or read whatever it's pointed at.
fn touches_files(id: &str) -> bool {
    id.starts_with("file.") || id == "doc.open"
}

fn readiness_word(r: Readiness) -> &'static str {
    match r {
        Readiness::Verified => "verified",
        Readiness::Experimental => "beta",
    }
}

/// What's missing to run on this PC, if anything.
fn missing(needs: Needs, installed: super::dialog::Installed) -> Option<&'static str> {
    match needs {
        Needs::NativeFea if !installed.native_fea => Some("Gmsh and CalculiX aren't installed. The user can install them from the Simulations panel."),
        Needs::Foam | Needs::Elmer if !installed.wsl_available => Some("Windows Subsystem for Linux isn't available on this PC."),
        Needs::Foam | Needs::Elmer if !installed.solvers_distro => Some("The solver environment isn't set up. The user can set it up from the Simulations panel."),
        _ => None,
    }
}

pub(super) fn partition_of(state: &SimState, slot: &mut Slot, body: &str) -> Result<Partition, String> {
    if let Some(object) = body.strip_prefix(sim_core::study::SCENE_PREFIX) {
        return match state.scene.bodies.get(object) {
            Some(b) => Ok(b.regions.partition.clone()),
            None => Err(state.scene.errors.get(object).cloned().unwrap_or_else(|| format!("{object} hasn't been read from the scene yet: set it as a study's body first, and try again in a moment."))),
        };
    }
    let host = design::ensure_host(slot)?;
    bridge::partition(&host.app.session, body, MeshQuality::Fine).map_err(|e| format!("{e:#}"))
}

/// A face's numbers, for choosing it.
fn face_summary(part: &Partition, face: u32) -> Option<Value> {
    let (mut area, mut largest, mut point) = (0.0, 0.0, [0.0; 3]);
    let (mut centre, mut normal) = ([0.0; 3], [0.0; 3]);
    for (i, f) in part.tri_face.iter().enumerate() {
        if *f != face {
            continue;
        }
        let [a, b, c] = part.triangle(i);
        let t = sim_core::geometry::partition::tri_area(a, b, c);
        let n = sim_core::geometry::partition::cross(sim_core::geometry::partition::sub(b, a), sim_core::geometry::partition::sub(c, a));
        let mid: [f64; 3] = std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0);
        area += t;
        for k in 0..3 {
            centre[k] += mid[k] * t;
            normal[k] += n[k] * 0.5;
        }
        if t > largest {
            largest = t;
            point = mid;
        }
    }
    if area <= 0.0 {
        return None;
    }
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    let round = |v: f64| (v * 1e6).round() / 1e6;
    Some(json!({
        "face": face,
        "area_m2": round(area),
        "centre_m": centre.map(|c| round(c / area)),
        // Near zero for a face that curves all the way round, like a cylinder's side.
        "outward": if length > area * 0.05 { json!(normal.map(|n| round(n / length))) } else { Value::Null },
        "point_m": point.map(round),
    }))
}

fn face_ref(part: &Partition, face: u32) -> Result<FaceRef, String> {
    let summary = face_summary(part, face).ok_or_else(|| format!("{} has no face {face}: it has {}.", part.name, part.faces))?;
    let point = serde_json::from_value(summary["point_m"].clone()).map_err(|e| e.to_string())?;
    Ok(FaceRef { face, point, second: false })
}

fn analysis_named(id: &str) -> Result<AnalysisId, String> {
    serde_json::from_value(json!(id)).map_err(|_| {
        let known: Vec<String> = catalog::ANALYSES.iter().filter_map(|a| serde_json::to_value(a.id).ok()).filter_map(|v| v.as_str().map(str::to_string)).collect();
        format!("no analysis called '{id}'. They are: {}", known.join(", "))
    })
}

fn study_summary(i: usize, s: &Study) -> Value {
    let a = catalog::analysis(s.analysis);
    json!({
        "study": i,
        "name": s.name,
        "analysis": s.analysis,
        "readiness": readiness_word(a.readiness),
        "body": s.body,
        "material": s.material.name,
        "fluid": s.fluid.name,
        "second_body": s.second_body,
        "conditions": s.conditions.iter().map(|c| json!({ "kind": c.kind, "faces": c.faces.iter().filter(|f| !f.second).map(|f| f.face).collect::<Vec<_>>(), "second_faces": c.faces.iter().filter(|f| f.second).map(|f| f.face).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "settings": s.settings,
        "still_needs": s.problems(),
    })
}

/// Sets the given settings by name. A name that doesn't exist is an error, not silently nothing.
fn merge_settings(current: &Settings, given: &Value) -> Result<Settings, String> {
    let mut v = serde_json::to_value(current).map_err(|e| e.to_string())?;
    let Some(fields) = given.as_object() else { return Err("settings must be an object".into()) };
    for (k, value) in fields {
        if v.get(k).is_none() {
            let known: Vec<&String> = v.as_object().map(|o| o.keys().collect()).unwrap_or_default();
            return Err(format!("no setting called '{k}'. They are: {known:?}"));
        }
        v[k] = value.clone();
    }
    serde_json::from_value(v).map_err(|e| format!("settings: {e}"))
}

fn result_summary(id: u64, set: &ResultSet) -> Value {
    let range = |f: &sim_core::results::Field| f.range().map_or(Value::Null, |(a, b)| json!([a, b]));
    let steps: Vec<Value> = set
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "step": i,
                "kind": match s.kind { StepKind::Time => "time_s", StepKind::Mode => "mode_hz", StepKind::Frequency => "frequency_hz" },
                "value": s.value,
                "fields": s.fields.iter().map(|f| json!({ "name": f.name, "unit": f.unit, "components": f.stride(), "range": range(f) })).collect::<Vec<_>>(),
            })
        })
        .collect();
    // Long runs have many steps, all with the same fields: keep the answer readable.
    let shown: Vec<Value> = if steps.len() > 12 { steps.iter().take(4).chain(steps.iter().rev().take(4).rev()).cloned().collect() } else { steps.clone() };
    let plots: Vec<Value> = set
        .plots
        .iter()
        .map(|p| {
            let y = p.points.iter().map(|v| v.1);
            let (lo, hi) = y.clone().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v)));
            json!({ "name": p.name, "unit": p.unit, "points": p.points.len(), "min": lo, "max": hi, "last": p.points.last().map(|v| v.1) })
        })
        .collect();
    json!({ "job": id, "nodes": set.mesh.nodes.len(), "step_count": steps.len(), "steps": shown, "plots": plots })
}

/// A field's values at a point, as numbers.
pub fn probe_value(set: &ResultSet, step: usize, field: &str, p: [f64; 3]) -> Result<Vec<f64>, String> {
    let v = probe(set, step, field, p)?;
    serde_json::from_value(v["values"].clone()).map_err(|e| e.to_string())
}

/// A field's value at a point.
fn probe(set: &ResultSet, step: usize, field: &str, p: [f64; 3]) -> Result<Value, String> {
    let s = set.steps.get(step).ok_or_else(|| format!("no step {step}: there are {}", set.steps.len()))?;
    let f = s.field(field).ok_or_else(|| {
        let names: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();
        format!("no field '{field}' on this step. There are: {names:?}")
    })?;
    if f.location != sim_core::results::Location::Node || f.len() != set.mesh.nodes.len() {
        return Err(format!("'{field}' isn't stored at the mesh's nodes, so it can't be read at a point."));
    }
    let stride = f.stride();
    if !set.mesh.tets.is_empty() {
        let locator = post::Locator::new(&set.mesh);
        if let Some((tet, weights)) = locator.locate(p) {
            let mut values = vec![0.0f64; stride];
            for (node, w) in set.mesh.tets[tet].iter().zip(weights) {
                for (k, v) in f.at(*node as usize).iter().enumerate() {
                    values[k] += w * f64::from(*v);
                }
            }
            return Ok(json!({ "field": f.name, "unit": f.unit, "components": f.components, "values": values, "how": "interpolated inside the mesh" }));
        }
    }
    let nearest = set.mesh.nodes.iter().enumerate().min_by(|(_, a), (_, b)| {
        let d = |n: &[f64; 3]| (n[0] - p[0]).powi(2) + (n[1] - p[1]).powi(2) + (n[2] - p[2]).powi(2);
        d(a).total_cmp(&d(b))
    });
    let (i, n) = nearest.ok_or("this result has no nodes")?;
    let distance = ((n[0] - p[0]).powi(2) + (n[1] - p[1]).powi(2) + (n[2] - p[2]).powi(2)).sqrt();
    Ok(json!({ "field": f.name, "unit": f.unit, "components": f.components, "values": f.at(i), "how": "nearest node: the point is outside the volume", "node_m": n, "distance_m": distance }))
}

/// Whether a tool is handled here. The `design_` prefix is shared with the outcome recipes,
/// so these are named rather than matched by prefix.
pub fn is_tool(name: &str) -> bool {
    name.starts_with("sim_") || matches!(name, "design_commands" | "design_run" | "design_bodies" | "design_faces" | "design_show" | "design_send_to_scene")
}

/// Runs a `design_*` or `sim_*` tool on the world.
pub fn start(world: &mut World, name: &str, i: &Value) -> ToolOutput {
    match run(world, name, i) {
        Ok(v) => ToolOutput::ok(v),
        Err(e) => ToolOutput::err(e),
    }
}

fn number(i: &Value, key: &str) -> Result<usize, String> {
    i[key].as_u64().map(|v| v as usize).ok_or_else(|| format!("'{key}' must be a number"))
}

fn run(world: &mut World, name: &str, i: &Value) -> Result<Value, String> {
    match name {
        "design_commands" => {
            let filter = i["filter"].as_str().unwrap_or("").to_lowercase();
            let limit = i["limit"].as_u64().unwrap_or(40) as usize;
            let matching: Vec<_> = solidcraft_engine::cmd::command_specs()
                .into_iter()
                .filter(|c| c.planned.is_none() && !touches_files(c.id))
                .filter(|c| filter.is_empty() || c.id.to_lowercase().contains(&filter) || c.label.to_lowercase().contains(&filter))
                .collect();
            let total = matching.len();
            let list: Vec<Value> = matching.iter().take(limit).map(|c| json!({ "id": c.id, "label": c.label, "params": c.params })).collect();
            Ok(json!({ "total": total, "shown": list.len(), "commands": list }))
        }
        "design_run" => {
            let commands = i["commands"].as_array().ok_or("'commands' must be a list")?.clone();
            for c in &commands {
                let id = c["command"].as_str().ok_or("each step needs a 'command'")?;
                if touches_files(id) {
                    return Err(format!("'{id}' works on files, which this tool leaves to the user. Nothing was run."));
                }
            }
            let mut slot = world.non_send_mut::<Slot>();
            let host = design::ensure_host(&mut slot)?;
            host.app.home.open = false;
            let session = &mut host.app.session;
            let mut done = 0usize;
            let outcome = session.run_script_each(&json!({ "commands": commands }), &mut |_, _| done += 1);
            let bodies = bridge::bodies(&host.app.session);
            match outcome {
                Ok(results) => Ok(json!({ "ran": results.len(), "results": results, "bodies": bodies })),
                Err(e) => Err(format!("{e}. {done} steps before it succeeded and stayed in the design. Bodies now: {bodies:?}")),
            }
        }
        "design_bodies" => {
            let mut slot = world.non_send_mut::<Slot>();
            let host = design::ensure_host(&mut slot)?;
            let session = &host.app.session;
            let mut out = Vec::new();
            for b in bridge::bodies(session) {
                let part = bridge::partition(session, &b, MeshQuality::Standard).map_err(|e| format!("{e:#}"))?;
                let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
                for p in &part.positions {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                out.push(json!({ "name": b, "faces": part.faces, "closed": part.is_closed(), "size_m": [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]], "lowest_corner_m": lo }));
            }
            Ok(json!({ "bodies": out }))
        }
        "design_faces" => {
            let body = i["body"].as_str().ok_or("'body' is needed")?.to_string();
            let part = world.resource_scope(|world, state: Mut<SimState>| {
                let mut slot = world.non_send_mut::<Slot>();
                partition_of(&state, &mut slot, &body)
            })?;
            let faces: Vec<Value> = (0..part.faces).filter_map(|f| face_summary(&part, f)).take(300).collect();
            Ok(json!({ "body": body, "face_count": part.faces, "closed": part.is_closed(), "faces": faces }))
        }
        "design_show" => {
            let show = i["show"].as_bool().ok_or("'show' must be true or false")?;
            *world.resource_mut::<Workspace>() = if show { Workspace::Design } else { Workspace::Scene };
            Ok(json!({ "showing": if show { "design" } else { "scene" } }))
        }
        "design_send_to_scene" => {
            let bodies = world.resource_scope(|world, mut pending: Mut<crate::scene::PendingSpawns>| {
                let mut slot = world.non_send_mut::<Slot>();
                let host = design::ensure_host(&mut slot)?;
                design::bridge_send(host, &mut pending).map_err(|e| format!("{e:#}"))
            })?;
            Ok(json!({ "placed": bodies, "note": "They appear when loaded. They're named after their bodies." }))
        }
        "sim_catalog" => {
            let installed = world.resource::<SimState>().installed;
            let analyses: Vec<Value> = catalog::ANALYSES
                .iter()
                .map(|a| {
                    let blocked = missing(a.needs, installed);
                    json!({
                        "id": a.id, "label": a.label, "group": a.group.label(), "readiness": readiness_word(a.readiness),
                        "summary": a.summary, "limits": a.limits, "engine": a.engine,
                        "can_run_now": blocked.is_none(), "missing": blocked,
                    })
                })
                .collect();
            let solids: Vec<String> = materials::solids().into_iter().map(|m| m.name).collect();
            let fluids: Vec<String> = materials::fluids().into_iter().map(|m| m.name).collect();
            Ok(json!({ "analyses": analyses, "solid_materials": solids, "fluids": fluids }))
        }
        "sim_studies" => {
            let state = world.resource::<SimState>();
            Ok(json!({ "studies": state.studies.iter().enumerate().map(|(i, s)| study_summary(i, s)).collect::<Vec<_>>(), "active": state.active }))
        }
        "sim_study_create" => {
            let analysis = analysis_named(i["analysis"].as_str().ok_or("'analysis' is needed")?)?;
            let mut state = world.resource_mut::<SimState>();
            let label = catalog::analysis(analysis).label;
            let n = state.studies.len() + 1;
            let name = i["name"].as_str().map_or_else(|| format!("{label} {n}"), str::to_string);
            let study = Study::new(name, analysis);
            let offered = study.offered_conditions();
            state.studies.push(study);
            let index = state.studies.len() - 1;
            state.active = Some(index);
            state.panel_open = true;
            let mut out = study_summary(index, &state.studies[index]);
            out["conditions_on_offer"] = json!(offered);
            Ok(out)
        }
        "sim_study_set" => {
            let index = number(i, "study")?;
            world.resource_scope(|world, mut state: Mut<SimState>| {
                let current = state.studies.get(index).cloned().ok_or_else(|| format!("no study {index}: there are {}", state.studies.len()))?;
                // Everything is worked out first: a mistake leaves the study as it was.
                let mut study = current;
                if let Some(v) = i["name"].as_str() {
                    study.name = v.to_string();
                }
                if let Some(v) = i["body"].as_str() {
                    study.body = Some(v.to_string());
                }
                if let Some(v) = i["second_body"].as_str() {
                    study.second_body = Some(v.to_string());
                }
                if let Some(v) = i["material"].as_str() {
                    study.material = materials::solids().into_iter().find(|m| m.name == v).ok_or_else(|| format!("no material '{v}': see sim_catalog"))?;
                    // Naming it is confirming it.
                    study.material_confirmed = true;
                }
                if let Some(v) = i["fluid"].as_str() {
                    study.fluid = materials::fluids().into_iter().find(|m| m.name == v).ok_or_else(|| format!("no fluid '{v}': see sim_catalog"))?;
                }
                if !i["settings"].is_null() {
                    study.settings = merge_settings(&study.settings, &i["settings"])?;
                }
                if let Some(list) = i["conditions"].as_array() {
                    let body = study.body.clone().ok_or("Set the study's body first: conditions are on its faces.")?;
                    let part = {
                        let mut slot = world.non_send_mut::<Slot>();
                        partition_of(&state, &mut slot, &body)?
                    };
                    let mut conditions = Vec::with_capacity(list.len());
                    for c in list {
                        let kind: BcKind = serde_json::from_value(c["kind"].clone()).map_err(|e| format!("a condition's kind: {e}"))?;
                        let mut faces = Vec::new();
                        for f in c["faces"].as_array().ok_or("each condition needs a list of faces")? {
                            faces.push(face_ref(&part, f.as_u64().ok_or("face numbers are whole numbers")? as u32)?);
                        }
                        if let Some(list) = c["second_faces"].as_array() {
                            let other = study.second_body.clone().ok_or("second_faces needs a second_body.")?;
                            let other_part = {
                                let mut slot = world.non_send_mut::<Slot>();
                                partition_of(&state, &mut slot, &other)?
                            };
                            for f in list {
                                let mut face = face_ref(&other_part, f.as_u64().ok_or("face numbers are whole numbers")? as u32)?;
                                face.second = true;
                                faces.push(face);
                            }
                        }
                        conditions.push(Condition { kind, faces });
                    }
                    study.conditions = conditions;
                }
                state.studies[index] = study;
                Ok(study_summary(index, &state.studies[index]))
            })
        }
        "sim_check_faces" => {
            let index = number(i, "study")?;
            let show = i["show"].as_bool().unwrap_or(false);
            world.resource_scope(|world, mut state: Mut<SimState>| {
                let before = state.view.take();
                let mut slot = world.non_send_mut::<Slot>();
                let rows = panel::preview_faces(&mut state, &mut slot, index);
                if !show || rows.is_err() {
                    state.view = before;
                }
                let rows = rows?;
                Ok(json!({ "coverage": rows.iter().map(|r| json!({ "condition": r.label, "faces": r.faces, "area_m2": r.area })).collect::<Vec<_>>() }))
            })
        }
        "sim_run" => {
            let index = number(i, "study")?;
            world.resource_scope(|world, mut state: Mut<SimState>| {
                let study = state.studies.get(index).ok_or_else(|| format!("no study {index}: there are {}", state.studies.len()))?;
                let a = catalog::analysis(study.analysis);
                if let Some(m) = missing(a.needs, state.installed) {
                    return Err(m.to_string());
                }
                let problems = study.problems();
                if !problems.is_empty() {
                    return Err(format!("Not ready: {}", problems.join(" ")));
                }
                let mut slot = world.non_send_mut::<Slot>();
                let job = panel::start_run(&mut state, &mut slot, index)?;
                state.active = Some(index);
                Ok(json!({ "job": job, "note": "Running in the background: follow it with sim_jobs." }))
            })
        }
        "sim_jobs" => {
            let state = world.resource::<SimState>();
            let only = i["job"].as_u64();
            let log_lines = i["log_lines"].as_u64().unwrap_or(0) as usize;
            let jobs: Vec<Value> = state
                .jobs
                .list()
                .into_iter()
                .filter(|j| only.is_none_or(|o| o == j.id))
                .rev()
                .take(20)
                .map(|j| {
                    let mut v = json!({ "job": j.id, "name": j.name, "study": j.study, "state": j.state, "has_result": j.has_result });
                    if log_lines > 0 {
                        v["log"] = json!(state.jobs.log(j.id, log_lines));
                    }
                    v
                })
                .collect();
            Ok(json!({ "jobs": jobs }))
        }
        "sim_cancel" => {
            let job = i["job"].as_u64().ok_or("'job' must be a number")?;
            world.resource::<SimState>().jobs.cancel(job);
            Ok(json!({ "cancelled": job }))
        }
        "sim_results" => {
            let job = i["job"].as_u64().ok_or("'job' must be a number")?;
            let mut state = world.resource_mut::<SimState>();
            let info = state.jobs.list().into_iter().find(|j| j.id == job).ok_or_else(|| format!("no job {job}"))?;
            match &info.state {
                State::Done => {}
                State::Failed { error } => return Err(format!("That run failed: {error}")),
                other if other.is_active() => return Err("That run is still going: see sim_jobs.".to_string()),
                _ => return Err("That run has no results: it was stopped, or was from an earlier session.".to_string()),
            }
            let set: Arc<ResultSet> = state.jobs.result(job).ok_or("That run has no results: they're kept until Sense Sim closes.")?;
            let out = result_summary(job, &set);
            if i["show"].as_bool().unwrap_or(false) {
                panel::show_result(&mut state, job, &info.study, set);
                state.panel_open = true;
            }
            Ok(out)
        }
        "sim_probe" => {
            let job = i["job"].as_u64().ok_or("'job' must be a number")?;
            let state = world.resource::<SimState>();
            let set = state.jobs.result(job).ok_or_else(|| format!("job {job} has no results"))?;
            let point: [f64; 3] = serde_json::from_value(i["point"].clone()).map_err(|_| "'point' must be three numbers")?;
            let field = i["field"].as_str().ok_or("'field' is needed")?;
            let step = i["step"].as_u64().map_or(set.steps.len().saturating_sub(1), |s| s as usize);
            probe(&set, step, field, point)
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::results::{Field, Location, ResultMesh, ResultStep};

    fn part() -> Partition {
        // One flat square, of two triangles, as face 0, and a triangle facing up the x axis as face 1.
        Partition {
            name: "t".into(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0], [2.0, 0.0, 1.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3], [4, 5, 6]],
            tri_face: vec![0, 0, 1],
            faces: 2,
        }
    }

    #[test]
    fn a_face_is_summed_up_and_its_point_is_on_it() {
        let p = part();
        let s = face_summary(&p, 0).unwrap();
        assert_eq!(s["area_m2"], 1.0);
        assert_eq!(s["centre_m"], json!([0.5, 0.5, 0.0]));
        assert_eq!(s["outward"], json!([0.0, 0.0, 1.0]));
        let r = face_ref(&p, 1).unwrap();
        assert_eq!(r.face, 1);
        assert!((r.point[0] - 2.0).abs() < 1e-5, "{:?}", r.point);
        assert!(face_summary(&p, 7).is_none());
        assert!(face_ref(&p, 7).unwrap_err().contains("no face 7"));
    }

    #[test]
    fn files_stay_with_the_user() {
        for id in ["file.save_as", "file.export", "file.new", "doc.open"] {
            assert!(touches_files(id), "{id}");
        }
        assert!(!touches_files("solid.extrude"));
    }

    #[test]
    fn every_analysis_can_be_named_and_a_wrong_one_lists_them() {
        for a in catalog::ANALYSES {
            let id = serde_json::to_value(a.id).unwrap();
            assert_eq!(analysis_named(id.as_str().unwrap()).unwrap(), a.id);
        }
        let e = analysis_named("warp").unwrap_err();
        assert!(e.contains("static") && e.contains("incompressible-lbm"), "{e}");
    }

    #[test]
    fn settings_merge_by_name_and_wrong_ones_are_refused() {
        let s = Settings::default();
        let m = merge_settings(&s, &json!({ "modes": 12, "transient": true })).unwrap();
        assert_eq!(m.modes, 12);
        assert!(m.transient);
        assert_eq!(m.dt, s.dt);
        assert!(merge_settings(&s, &json!({ "modez": 12 })).unwrap_err().contains("no setting called"));
        assert!(merge_settings(&s, &json!({ "modes": "lots" })).is_err());
    }

    #[test]
    fn conditions_parse_as_the_tool_says() {
        for (text, label) in [
            (json!({"type":"fixed"}), "Fixed support"),
            (json!({"type":"force","force":[0,0,-100]}), "Force"),
            (json!({"type":"pressure","pressure":1e5}), "Pressure"),
            (json!({"type":"temperature","value":100}), "Temperature"),
            (json!({"type":"heat_flux","flux":1000}), "Heat flux"),
            (json!({"type":"convection","coefficient":10,"ambient":20}), "Convection"),
            (json!({"type":"velocity_inlet","speed":1}), "Velocity inlet"),
            (json!({"type":"pressure_outlet","pressure":0}), "Pressure outlet"),
            (json!({"type":"wall"}), "Wall"),
            (json!({"type":"potential","value":100}), "Electric potential"),
        ] {
            assert_eq!(serde_json::from_value::<BcKind>(text).unwrap().label(), label);
        }
    }

    #[test]
    fn a_probe_reads_inside_the_mesh_and_says_when_it_doesnt() {
        let nodes = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let x: Vec<f32> = nodes.iter().map(|p| p[0] as f32).collect();
        let f = Field { name: "x".into(), unit: "m".into(), location: Location::Node, components: vec!["x".into()], data: Arc::from(x) };
        let set = ResultSet { mesh: ResultMesh { nodes, tets: vec![[0, 1, 2, 3]], surface: vec![], cut_start: None }, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields: vec![f] }], plots: vec![] };
        let inside = probe(&set, 0, "x", [0.25, 0.25, 0.25]).unwrap();
        assert!((inside["values"][0].as_f64().unwrap() - 0.25).abs() < 1e-6, "{inside}");
        let outside = probe(&set, 0, "x", [5.0, 0.0, 0.0]).unwrap();
        assert!(outside["how"].as_str().unwrap().contains("outside"));
        assert!(probe(&set, 0, "nope", [0.0; 3]).unwrap_err().contains("no field"));
        assert!(probe(&set, 9, "x", [0.0; 3]).unwrap_err().contains("no step"));
    }

    #[test]
    fn the_tool_list_is_well_formed_and_its_names_route_here() {
        for d in defs() {
            assert!(d.name.starts_with("design_") || d.name.starts_with("sim_"), "{}", d.name);
            assert_eq!(d.schema["type"], "object", "{}", d.name);
        }
    }
}
