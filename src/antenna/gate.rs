//! The RFID gate test's background jobs and the assistant's tools for it, and for
//! bringing in a drawing. The physics is `antenna_core::recipes::gate`.

use std::sync::Arc;

use antenna_core::cad::{self, CadPlane, LayerRole, Placement, Resolve};
use antenna_core::geometry::Geometry;
use antenna_core::project::{Drive, DriveState, Project};
use antenna_core::recipes::gate::{self, GateField, GateReport, GateSpec, Preset};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::Studio;
use super::run;
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

/// One solved gate. The ports' fields stay, so states and power change without a new solve.
#[derive(Clone)]
pub struct GateRun {
    pub field: Arc<GateField>,
    pub spec: GateSpec,
    /// The model and mesh it was solved for, to notice when it no longer applies.
    pub geometry: Geometry,
    pub edges_per_lambda: f64,
}

impl GateRun {
    /// Whether this solve still answers for the project and the aisle as they stand.
    pub fn applies_to(&self, project: &Project, spec: &GateSpec) -> bool {
        self.spec.same_samples(spec) && self.edges_per_lambda == project.setup.edges_per_lambda && project.geometry().is_ok_and(|g| g == self.geometry)
    }
}

impl Studio {
    pub fn gate_run(&self) -> Option<GateRun> {
        self.shared.lock().unwrap().gate.clone()
    }

    /// States a finished search left, once.
    pub fn take_found_states(&self) -> Option<Vec<DriveState>> {
        self.shared.lock().unwrap().gate_found.take()
    }

    /// Solves the model at the spec's frequency, by whichever solver it takes.
    pub fn run_gate(&mut self, spec: GateSpec) {
        let project = self.project.clone();
        self.spawn(move |ctx, shared| {
            let geometry = project.geometry()?;
            let field = run::gate_field(&project, &spec, ctx)?;
            shared.lock().unwrap().gate = Some(GateRun { field: Arc::new(field), spec, geometry, edges_per_lambda: project.setup.edges_per_lambda });
            Ok(())
        });
    }

    /// Searches phases on the solved field. No new solve.
    pub fn search_gate_phases(&mut self, spec: GateSpec, states: usize, evals: usize) {
        let Some(run) = self.gate_run() else { return };
        self.spawn(move |ctx, shared| {
            ctx.busy(format!("Searching the phases of {states} states…"), -1.0);
            let found = gate::search_phases(&run.field, &spec, states, evals, 7);
            shared.lock().unwrap().gate_found = Some(found);
            Ok(())
        });
    }
}

// ---------------------------------------------------------------------------
// The assistant's tools

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let num = json!({ "type": "number" });
    vec![
        def(
            "antenna_import_cad",
            "Bring an antenna drawing into Antenna Studio from a DXF or Gerber file on this computer. Layers named FEED or PORT mark feeds (lines across gaps, circles or points for probes); without one, gaps between metal are used. Reports each layer, the size and the feeds it found. New antenna by default; add=true adds to the current model.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "f0": { "type": "string", "description": "Design frequency, e.g. 915MHz" },
                "unit_mm": { "type": "number", "description": "Millimetres per drawing unit, when the file doesn't say or says wrong" },
                "plane": { "type": "string", "enum": ["flat", "facing_x", "facing_y"], "description": "How the drawing stands in the model" },
                "center": { "type": "boolean", "description": "Move the metal's centre to the origin (default true)" },
                "add": { "type": "boolean" },
                "layers": { "type": "object", "description": "By layer name: {role: metal|feed|board|ignore, height: expression, material: name}" }
            }, "required": ["path"] }),
        ),
        def(
            "antenna_states",
            "Set the excitation states: what a phase network or switch applies to the ports, one state per moment of the reader's cycle. Give a preset (in_phase, progressive with step_deg, alternating, one_at_a_time, sweep) or explicit states [{name, phases:[deg per port], amps:[per port]}]. Applying a state only changes post-processing.",
            json!({ "type": "object", "properties": { "preset": { "type": "string", "enum": ["in_phase", "progressive", "alternating", "one_at_a_time", "sweep"] }, "step_deg": { "type": "number" }, "states": { "type": "array" } } }),
        ),
        def(
            "antenna_gate_test",
            "Test the current model as an RFID security gate: solve the field per port through a read zone, then judge the excitation states by tags read walking through in any orientation, tags on display read, and radiated power against the limit. Background job: poll antenna_gate_results. Link and state changes re-judge without a new solve when only those changed.",
            json!({ "type": "object", "properties": {
                "aisle_width_m": num, "length_m": num, "low_m": num, "high_m": num, "spacing_m": num,
                "stray_from_m": num, "stray_depth_m": num,
                "tx_dbm": num, "feed_loss_db": num, "tag_sensitivity_dbm": num, "tag_gain_dbi": num, "product_loss_db": num, "product_loss_spread_db": num,
                "reader_sensitivity_dbm": num, "walk_speed": num, "dwell_ms": num, "min_read": num, "max_stray": num, "eirp_limit_dbm": num
            } }),
        ),
        def("antenna_gate_results", "Job status and the gate's results: read and stray shares with their range and evidence label, by orientation, by state, the reader power that meets the goal, and the power the EIRP limit allows.", json!({ "type": "object", "properties": {} })),
        def("antenna_find_phases", "Search the excitation phases on the solved gate for the most tags read and the fewest strays, at the power each candidate may legally use. Needs antenna_gate_test first. Background job; the states found replace the current ones.", json!({ "type": "object", "properties": { "states": { "type": "integer" }, "evals": { "type": "integer" } } })),
    ]
}

fn text(v: Value) -> ToolOutput {
    ToolOutput::text(v.to_string())
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// A spec from the tool's arguments, on top of what the Studio has.
fn spec_from(st: &Studio, i: &Value) -> Result<GateSpec, String> {
    let g = st.project.geometry().map_err(|e| format!("{e:#}"))?;
    let mut s = st.ui.gate.spec.unwrap_or_else(|| GateSpec::for_project(&st.project, &g));
    let num = |key: &str, slot: &mut f64| {
        if let Some(v) = i[key].as_f64() {
            *slot = v;
        }
    };
    num("aisle_width_m", &mut s.aisle.width);
    num("length_m", &mut s.aisle.length);
    num("low_m", &mut s.aisle.low);
    num("high_m", &mut s.aisle.high);
    num("spacing_m", &mut s.spacing);
    num("stray_from_m", &mut s.aisle.stray_from);
    num("stray_depth_m", &mut s.aisle.stray_depth);
    num("tx_dbm", &mut s.tx_dbm);
    num("feed_loss_db", &mut s.feed_loss_db);
    num("tag_sensitivity_dbm", &mut s.tag_sensitivity_dbm);
    num("tag_gain_dbi", &mut s.tag_gain_dbi);
    num("product_loss_db", &mut s.product_loss_db.0);
    num("product_loss_spread_db", &mut s.product_loss_db.1);
    num("reader_sensitivity_dbm", &mut s.reader_sensitivity_dbm);
    num("walk_speed", &mut s.walk_speed);
    num("min_read", &mut s.min_read);
    num("max_stray", &mut s.max_stray);
    num("eirp_limit_dbm", &mut s.eirp_limit_dbm);
    if let Some(ms) = i["dwell_ms"].as_f64() {
        s.dwell_s = ms / 1e3;
    }
    Ok(s)
}

/// A report as the assistant reads it. A number keeps its evidence label.
fn report_json(r: &GateReport, spec: &GateSpec, field: &GateField) -> Value {
    let (read, stray) = gate::predict(r, spec, 300);
    json!({
        "evidence": read.prediction.evidence.label(),
        "solver": field.solver,
        "size": field.size,
        "read": { "nominal": r3(r.read), "low": r3(read.prediction.low), "high": r3(read.prediction.high), "chance_of_goal": read.pass_probability.map(r3), "main_driver": read.main_driver().map(|s| s.factor.clone()) },
        "stray": { "nominal": r3(r.stray), "low": r3(stray.prediction.low), "high": r3(stray.prediction.high), "chance_of_goal": stray.pass_probability.map(r3) },
        "by_orientation": r.by_orientation.iter().map(|o| json!({ "axis": o.0, "read": r3(o.1) })).collect::<Vec<_>>(),
        "states": r.states.iter().map(|s| json!({ "name": s.name, "read_alone": r3(s.read), "stray": r3(s.stray), "tarc_db": r3(s.tarc_db), "eirp_dbm": s.eirp_dbm.map(r3), "peak_field_v_per_m_rms": r3(s.peak_field_vm) })).collect::<Vec<_>>(),
        "needed_path_gain_db": r3(r.needed_db),
        "limited_by": if r.reverse_limited { "reader receive" } else { "tag chip" },
        "power_for_goal_dbm": r.power_for_goal.map(|p| json!({ "reader_dbm": p.0, "stray": r3(p.1) })),
        "eirp_limit": r.legal.map(|l| json!({ "exceeded": true, "reader_dbm_allowed": r3(l.0), "read_there": r3(l.1), "stray_there": r3(l.2) })),
        "samples": field.samples.points.len(),
        "too_close_to_metal": field.samples.excluded,
        "assumptions": gate::assumptions(field, spec),
        "warnings": r.warnings,
    })
}

fn import(world: &mut World, i: &Value) -> ToolOutput {
    let Some(path) = i["path"].as_str() else { return ToolOutput::err("give the file's path") };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ToolOutput::err(format!("can't read {path}: {e}")),
    };
    let mut raw = match cad::read(path, &bytes) {
        Ok(r) => r,
        Err(e) => return ToolOutput::err(format!("{e:#}")),
    };
    let unit_given = i["unit_mm"].as_f64().map(|u| u * 1e-3);
    if unit_given.is_none() && raw.units_to_m.is_none() {
        return ToolOutput::err("the file doesn't say what its unit is: give unit_mm");
    }
    let layer_opts = &i["layers"];
    for l in &mut raw.layers {
        match layer_opts[&l.name]["role"].as_str() {
            Some("metal") => l.role = LayerRole::Metal,
            Some("feed") => l.role = LayerRole::Feed,
            Some("board") => l.role = LayerRole::Board,
            Some("ignore") => l.role = LayerRole::Ignore,
            _ => {}
        }
    }
    let scale = unit_given.or(raw.units_to_m).unwrap_or(1e-3);
    let drawing = match raw.resolve(Resolve { scale_to_m: scale, ..Default::default() }) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("{e:#}")),
    };
    let mut st = world.resource_mut::<Studio>();
    let metal = st.project.metal.clone();
    let mut layers = cad::default_placements(&drawing, &metal);
    for (place, l) in layers.iter_mut().zip(&drawing.layers) {
        let o = &layer_opts[&l.name];
        if let Some(h) = o["height"].as_str() {
            place.height = h.to_string();
        }
        if let Some(m) = o["material"].as_str() {
            place.material = m.to_string();
        }
        if !o["role"].is_null() {
            place.include = l.role == LayerRole::Metal || l.role == LayerRole::Board;
        }
    }
    let plane = match i["plane"].as_str() {
        Some("facing_x") => CadPlane::FacingX,
        Some("facing_y") => CadPlane::FacingY,
        _ => CadPlane::Flat,
    };
    let how = Placement { plane, center: i["center"].as_bool().unwrap_or(true), layers, feeds: drawing.feeds.clone() };
    let done = if i["add"].as_bool().unwrap_or(false) {
        let mut p = st.project.clone();
        cad::add_to_project(&mut p, &drawing, path, &how).map(|a| (p, a))
    } else {
        cad::new_project(&drawing, path, i["f0"].as_str().unwrap_or("915MHz"), &how)
    };
    let (project, added) = match done {
        Ok(v) => v,
        Err(e) => return ToolOutput::err(format!("{e:#}")),
    };
    let built = project.scatterer().map(|g| g.bounds());
    st.project = project;
    st.ui.page = super::ui::Page::Parts;
    let layers: Vec<Value> = drawing.layers.iter().map(|l| json!({ "name": l.name, "role": l.role.label(), "outlines": l.islands.len(), "area_mm2": r3(l.area() * 1e6), "open_paths": l.open.len(), "height_m": r3(l.z) })).collect();
    let size = built.map(|(lo, hi)| [r3((hi[0] - lo[0]) * 1e3), r3((hi[1] - lo[1]) * 1e3), r3((hi[2] - lo[2]) * 1e3)]).ok();
    let out = json!({
        "ok": true,
        "format": drawing.format,
        "layers": layers,
        "parts_added": added.parts,
        "ports_added": added.ports,
        "feeds_guessed": drawing.feeds_guessed,
        "size_mm": size,
        "warnings": drawing.warnings,
        "next": if added.ports == 0 { "No feed was found. Add ports with coordinates under Parts and ports before solving." } else { "Ready to solve. Port impedance is the Z0 variable." },
    });
    drop(st);
    world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
    text(out)
}

/// Handles this module's tools; `None` for anything else.
pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        "antenna_import_cad" => import(world, i),
        "antenna_states" => {
            let mut st = world.resource_mut::<Studio>();
            let n = st.project.geometry().map_or(0, |g| g.ports.len());
            if n == 0 {
                return Some(ToolOutput::err("the model has no ports yet"));
            }
            let states = if let Some(list) = i["states"].as_array() {
                let mut out = Vec::with_capacity(list.len());
                for (k, s) in list.iter().enumerate() {
                    let phases: Vec<f64> = s["phases"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(Value::as_f64).collect());
                    if phases.len() != n {
                        return Some(ToolOutput::err(format!("state {} sets {} phases and the model has {n} ports", k + 1, phases.len())));
                    }
                    let amps: Vec<f64> = s["amps"].as_array().map_or(vec![1.0; n], |a| a.iter().filter_map(Value::as_f64).collect());
                    out.push(DriveState { name: s["name"].as_str().map_or_else(|| format!("State {}", k + 1), str::to_string), drive: (0..n).map(|p| Drive { amp: amps.get(p).copied().unwrap_or(1.0), phase: phases[p] }).collect() });
                }
                out
            } else {
                let preset = match i["preset"].as_str() {
                    Some("progressive") => Preset::Progressive(i["step_deg"].as_f64().unwrap_or(360.0 / n as f64).round() as i32),
                    Some("alternating") => Preset::Alternating,
                    Some("one_at_a_time") => Preset::OneAtATime,
                    Some("sweep") => Preset::Sweep,
                    Some("in_phase") => Preset::InPhase,
                    _ => return Some(ToolOutput::err("give a preset or states")),
                };
                gate::preset_states(preset, n)
            };
            let names: Vec<String> = states.iter().map(|s| s.name.clone()).collect();
            st.project.states = states;
            text(json!({ "ok": true, "states": names }))
        }
        "antenna_gate_test" => {
            let mut st = world.resource_mut::<Studio>();
            if st.project.reconfig.is_on() {
                return Some(ToolOutput::err("this model has switches or varactors on some ports, and the gate test drives every port: remove them first"));
            }
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            let spec = match spec_from(&st, i) {
                Ok(s) => s,
                Err(e) => return Some(ToolOutput::err(e)),
            };
            let project = st.project.clone();
            st.ui.gate.keep_spec(spec, &project);
            st.ui.page = super::ui::Page::Gate;
            let fresh = st.gate_run().is_some_and(|r| r.applies_to(&st.project, &spec));
            if fresh {
                return Some(text(json!({ "started": false, "note": "Only the link or states changed: the solved field still applies. Read antenna_gate_results." })));
            }
            st.run_gate(spec);
            text(json!({ "started": true }))
        }
        "antenna_gate_results" => {
            let st = world.resource::<Studio>();
            let job = match st.status() {
                super::Status::Idle => json!({ "state": "idle" }),
                super::Status::Busy(m, p) => json!({ "state": "running", "message": m, "progress": p }),
                super::Status::Failed(e) => json!({ "state": "failed", "error": e }),
            };
            let Some(run) = st.gate_run() else { return Some(text(json!({ "job": job, "gate": Value::Null }))) };
            let spec = st.ui.gate.spec.unwrap_or(run.spec);
            if !run.applies_to(&st.project, &spec) {
                return Some(text(json!({ "job": job, "gate": Value::Null, "note": "The model or aisle changed since the last solve: run antenna_gate_test again." })));
            }
            let r = gate::evaluate(&run.field, &spec, &st.project.states);
            text(json!({ "job": job, "gate": report_json(&r, &spec, &run.field) }))
        }
        "antenna_find_phases" => {
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            let Some(run) = st.gate_run() else { return Some(ToolOutput::err("solve the gate first with antenna_gate_test")) };
            let spec = st.ui.gate.spec.unwrap_or(run.spec);
            if !run.applies_to(&st.project, &spec) {
                return Some(ToolOutput::err("the model or aisle changed since the last solve: run antenna_gate_test again"));
            }
            st.search_gate_phases(spec, i["states"].as_u64().unwrap_or(2).clamp(1, 8) as usize, i["evals"].as_u64().unwrap_or(800) as usize);
            text(json!({ "started": true }))
        }
        _ => return None,
    })
}
