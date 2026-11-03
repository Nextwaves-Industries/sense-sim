//! Assistant and MCP tools for modelling in 3D. They write the same feature tree the page does,
//! so whatever the assistant builds is as editable as what's drawn by hand.

use antenna_core::freeform;
use antenna_core::modeler::{BodyKind, spec};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::Studio;
use super::ui::Page;
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "antenna_model_add".into(),
            description: "Add steps to the 3D model of the current antenna, in order, the way a CAD feature tree works. Every dimension is an expression over variables, so set them in `vars` first: a number with a unit is refused, since nothing could tune it. Steps refer to earlier ones by name. kinds: \
sketch {name, plane: top|front|right|face, offset, body+face for a face, shapes: [{rect:{c:[u,v],size:[u,v]}}, {circle:{c,r}}, {poly:[[u,v],...],open:bool}, {arc:{c,r,from_deg,to_deg}}, {curve:{u,v,t0,t1}} in t, {point:{name,at:[u,v]}}]}; top's u,v are x,y, front's x,z, right's y,z. \
extrude {sketch, depth, dir: one|reverse|both|through, surface}, sheet {sketch, thick} for flat metal, where thick is optional and a time-domain solver sees it as a real slab while the method of moments keeps it a sheet; rectangle {plane, offset, p0:[u,v], size:[u,v], thick}, circle {plane, offset, c:[u,v], r, thick}, ellipse {plane, offset, c, rx, ry, thick}, polygon {plane, offset, c, r, sides, thick} are flat metal with no sketch behind them; revolve {sketch, axis: u|v, angle, surface}, sweep {sketch, path}, loft {sections: [names], surface}, \
box {min,max}, cylinder {a,b,r}, cone {a,b,r0,r1}, sphere {c,r}, torus {c,axis,big,small}, wire {path, r}; a path is {sketch}, {helix:{radius,radius2,pitch,turns,origin,axis,left}} or {points:[[x,y,z],...]}. \
All of those take mode: new|add|cut|common, target (a body's step name), material, mesh (a size). \
combine {op: add|cut|common|exclude, target, tools} where sheets on one plane combine as areas, so one can cut another, fillet|chamfer {body, edges: [[face,face]], r}, shell {body, open: [faces], thick} where thick 0 leaves open surfaces for the method of moments, thicken {body, thick}, pattern {bodies, step:[x,y,z], count, step2, count2, merge, feeds}, circular {bodies, origin, axis, count, total, merge, feeds}, mirror {bodies, plane, merge}, move {bodies, by, origin, axis, angle, copy}. \
Faces are named top, bottom, side N, where N counts the curves of the body's sketch from 0. feed {a, b, width, z0} takes [x,y,z] or {sketch, point, height} and must lie along x, y or z; probe {sketch, point, width, z0} runs from the metal on that sketch's plane to the nearest metal below it, or above if there's none below. Metal bodies that touch have to be combined for the method of moments; FDTD doesn't mind. Returns each body's size and any step that failed, with why.".into(),
            schema: json!({ "type": "object", "properties": { "steps": { "type": "array" }, "vars": { "type": "object", "description": "Variables to set first, name to expression" }, "notes": { "type": "object" } }, "required": ["steps"] }),
        },
        ToolDef { name: "antenna_model_get".into(), description: "The 3D model of the current antenna: its steps, what failed and why, its bodies with their sizes, and its feeds.".into(), schema: json!({ "type": "object", "properties": {} }) },
        ToolDef { name: "antenna_model_clear".into(), description: "Removes the 3D model of the current antenna, or from the step named `from` onward.".into(), schema: json!({ "type": "object", "properties": { "from": { "type": "string" } } }) },
    ]
}

fn report(st: &Studio) -> Value {
    let vars = st.project.vars().unwrap_or_default();
    let built = st.project.model.build(&vars);
    let name_of = |id: u32| st.project.model.feature(id).map_or(String::new(), |f| f.name.clone());
    json!({
        "steps": st.project.model.features.iter().map(|f| json!({ "name": f.name, "kind": f.kind.label(), "off": f.off })).collect::<Vec<_>>(),
        "failed": built.errors.iter().map(|(id, why)| json!({ "step": name_of(*id), "why": why })).collect::<Vec<_>>(),
        "bodies": built.bodies.iter().map(|b| {
            let (lo, hi) = b.mesh.bounds();
            json!({
                "name": b.name,
                "material": if b.material.is_empty() { &st.project.metal } else { &b.material },
                "kind": match b.kind { BodyKind::Solid => "solid", BodyKind::Sheet => "surface", BodyKind::Wire => "wire" },
                "from_mm": lo.map(|v| (v * 1e6).round() / 1e3),
                "to_mm": hi.map(|v| (v * 1e6).round() / 1e3),
                "volume_cm3": if b.kind == BodyKind::Solid { b.mesh.volume() * 1e6 } else { 0.0 },
                "area_cm2": b.mesh.area() * 1e4,
                "triangles": b.mesh.tris.len(),
            })
        }).collect::<Vec<_>>(),
        "feeds": built.ports.iter().map(|p| json!({ "name": p.name, "a_mm": p.a.map(|v| v * 1e3), "b_mm": p.b.map(|v| v * 1e3), "z0": p.z0 })).collect::<Vec<_>>(),
        "touching_not_joined": built.touching_unjoined(),
    })
}

fn set_vars(st: &mut Studio, vars: &Value) -> Result<(), String> {
    let Some(map) = vars.as_object() else { return Ok(()) };
    for (name, value) in map {
        let text = value.as_str().map(str::to_string).or_else(|| value.as_f64().map(|n| n.to_string())).ok_or_else(|| format!("{name}: a variable is an expression"))?;
        match st.project.params.iter_mut().find(|p| p.name == *name) {
            Some(row) => row.expr = text,
            None => antenna_core::sketch::add_variable(&mut st.project, name, &text, "From the assistant").map_err(|e| format!("{e:#}"))?,
        }
    }
    st.project.vars().map(|_| ()).map_err(|e| format!("{e:#}"))
}

pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    if !name.starts_with("antenna_model_") {
        return None;
    }
    let mut st = world.resource_mut::<Studio>();
    let st: &mut Studio = &mut st;
    let out = match name {
        "antenna_model_get" => report(st),
        "antenna_model_clear" => {
            super::ui::draw::remember(st);
            match i["from"].as_str() {
                Some(from) => match st.project.model.features.iter().position(|f| f.name == from) {
                    Some(k) => st.project.model.features.truncate(k),
                    None => return Some(ToolOutput::err(format!("There's no step called '{from}'."))),
                },
                None => st.project.model = Default::default(),
            }
            st.ui.m3 = Default::default();
            report(st)
        }
        "antenna_model_add" => {
            let Some(steps) = i["steps"].as_array() else { return Some(ToolOutput::err("It needs \"steps\", a list.")) };
            // A dimension nothing can move is no use to a sweep or an optimiser, so it's sent back.
            let loose = freeform::loose_numbers(&spec::dimensions_only(steps));
            if !loose.is_empty() {
                return Some(ToolOutput::err(freeform::loose_message(&loose)));
            }
            let before = st.project.clone();
            if let Err(e) = set_vars(st, &i["vars"]) {
                st.project = before;
                return Some(ToolOutput::err(e));
            }
            let vars = match st.project.vars() {
                Ok(v) => v,
                Err(e) => {
                    st.project = before;
                    return Some(ToolOutput::err(format!("{e:#}")));
                }
            };
            if let Err(e) = spec::add_steps(&mut st.project.model, steps, &vars) {
                st.project = before;
                return Some(ToolOutput::err(format!("{e:#}")));
            }
            st.ui.page = Page::Draw;
            st.ui.m3.flat = false;
            report(st)
        }
        _ => return None,
    };
    Some(ToolOutput::text(out.to_string()))
}
