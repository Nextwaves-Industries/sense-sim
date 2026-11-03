//! Assistant / MCP tools for Antenna Studio, on the `antenna_` prefix.

use antenna_core::farfield::{Cut, Pol, Quantity};
use antenna_core::optimize::Method;
use antenna_core::project::{Goal, Project, SimResult, SolverKind};
use antenna_core::templates;
use bevy::prelude::*;
use serde_json::{Value, json};

use super::{Status, Studio};
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let mut all = vec![
        def("antenna_references", "What's known about a job: the structure real products use, why, the order to tune it in, target numbers, limits and sources. Read it to learn the job, not as the answer: it's one structure among several, and it was solved in free space. Also lists RFID tag chips with their impedance and sensitivity. For a tag, design with antenna_tag_try. antenna_open_sample opens a reference to study.", json!({ "type": "object", "properties": { "job": { "type": "string", "description": "What it's for, in words: 'RFID tag on cartons', 'circular reader panel for the US band'" } } })),
        def("antenna_list_samples", "List the sample antenna projects: real uses like a UHF RFID reader panel, a Wi-Fi PCB antenna or a GPS helix, each in its band and checked against a solver, with what they are for. Ready to open and study, or to start a design from.", json!({ "type": "object", "properties": {} })),
        def("antenna_open_sample", "Open a sample project in Antenna Studio on its results, replacing the current design. Those that are slow to solve come with results stored; the rest solve when opened, so poll antenna_results until the job is idle. id: from antenna_list_samples.", json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] })),
        def("antenna_list_templates", "List the antenna templates of Antenna Studio (dipole, Yagi, helix, patch, horn, Vivaldi, ...), with what they are and which solvers apply.", json!({ "type": "object", "properties": {} })),
        def(
            "antenna_new",
            "Start a new antenna in Antenna Studio from a template at a design frequency, and open the Studio. Replaces the current design.",
            json!({ "type": "object", "properties": { "template": { "type": "string", "description": "Template id from antenna_list_templates" }, "f0": { "type": "string", "description": "Design frequency, e.g. 2.45GHz" }, "name": { "type": "string" } }, "required": ["template"] }),
        ),
        def(
            "antenna_set",
            "Change the current design: variables (expressions with units are fine, e.g. {\"L\":\"31mm\",\"h\":\"0.04*lambda0\"}), which is how any size in a template or drawing is changed, metal and substrate material names, sweep band in GHz, solver (auto, fdm for the built-in finite-difference solver, fdtd for openEMS, mom_wire, mom_surface), far-field frequencies in GHz, mesh density, adaptive_mesh (the fdm solver repeats the run, adding cells where the field sits, until S stops changing).",
            json!({ "type": "object", "properties": { "vars": { "type": "object" }, "metal": { "type": "string" }, "substrate": { "type": "string" }, "f_lo_ghz": { "type": "number" }, "f_hi_ghz": { "type": "number" }, "far_ghz": { "type": "array", "items": { "type": "number" } }, "solver": { "type": "string", "enum": ["auto", "fdm", "fdtd", "mom_wire", "mom_surface"] }, "cells_per_lambda": { "type": "number" }, "adaptive_mesh": { "type": "boolean" }, "wave_feeds": { "type": "array", "items": { "type": "string" }, "description": "Names of the feeds that are wave ports, for the fdm solver: each has to join a line's two conductors at the model's outer edge" }, "segments_per_lambda": { "type": "number" }, "edges_per_lambda": { "type": "number" }, "mom_samples": { "type": "integer" } } }),
        ),
        def("antenna_get", "The current design: template, variable table with evaluated values, materials, sweep, solver, overall size, ports, and the estimate of a run. A drawing also comes with how each part and feed is written, and any numbers in them not yet tied to a variable.", json!({ "type": "object", "properties": {} })),
        def("antenna_run", "Start a simulation of the current design in the background. Use antenna_wait until it's done, then antenna_assess.", json!({ "type": "object", "properties": {} })),
        def(
            "antenna_results",
            "Job status and results of the latest run: resonances, bandwidth, impedance, VSWR at the design frequency, peak gain, directivity, efficiencies, beam widths, front/back, side lobes, axial ratio, polarisation, pattern cuts on request.",
            json!({ "type": "object", "properties": { "cuts": { "type": "boolean", "description": "Include φ=0/90° pattern cuts every 15°" } } }),
        ),
        def(
            "antenna_optimize",
            "Optimise variables of the current design in the background. vars: [{name,min,max}] with SI values. goals: objects {type:'match_below',port,f_lo,f_hi,db} | {type:'resonance_at',port,f,tol} | {type:'gain_above',f,dbi} | {type:'efficiency_above',f,eff} | {type:'front_to_back_above',f,db} | {type:'axial_ratio_below',f,db} | {type:'side_lobes_below',f,db} | {type:'isolation_below',i,j,f_lo,f_hi,db} | {type:'chip_power_above',f_lo,f_hi,z:[r,x],at_least} for a tag, frequencies in Hz. method: nelder_mead (default), surrogate, particle_swarm, differential_evolution.",
            json!({ "type": "object", "properties": { "vars": { "type": "array" }, "goals": { "type": "array" }, "method": { "type": "string" }, "max_runs": { "type": "integer" }, "parallel": { "type": "integer" }, "apply": { "type": "boolean" } }, "required": ["vars", "goals"] }),
        ),
        def("antenna_optimize_status", "Progress of the optimiser. With apply=true the best values found go into the variable table.", json!({ "type": "object", "properties": { "apply": { "type": "boolean" } } })),
        def("antenna_cancel", "Stop the running antenna job.", json!({ "type": "object", "properties": {} })),
    ];
    all.extend(super::gate::defs());
    all.extend(super::model_tools::defs());
    all.extend(super::reconfig::defs());
    all.extend(super::phased::defs());
    all.extend(super::assist::defs());
    all.extend(super::tagjob::defs());
    all
}

fn text(v: Value) -> ToolOutput {
    ToolOutput::text(v.to_string())
}

fn result_json(r: &SimResult, f0: f64, with_cuts: bool) -> Value {
    let net = &r.network;
    let mut ports = Vec::new();
    for p in 0..net.ports() {
        let k = net.f.iter().enumerate().min_by(|a, b| (a.1 - f0).abs().total_cmp(&(b.1 - f0).abs())).map_or(0, |x| x.0);
        let z = net.zin(p);
        ports.push(json!({
            "port": p + 1,
            "z0": net.z0[p],
            "at_f0": { "s_db": net.sij_db(p, p)[k], "z_re": z[k].re, "z_im": z[k].im, "vswr": net.vswr(p)[k] },
            "resonances": net.resonances(p).iter().map(|(f, db)| json!({ "f_ghz": f / 1e9, "db": db })).collect::<Vec<_>>(),
            "bands_below_10db": net.bandwidths(p, -10.0).iter().map(|(a, b)| json!([a / 1e9, b / 1e9])).collect::<Vec<_>>(),
        }));
    }
    let mut far = Vec::new();
    for (k, f) in r.far_f.iter().enumerate() {
        let Some(ff) = r.far.first().and_then(|p| p.get(k)) else { continue };
        let m = ff.metrics();
        let mut o = json!({
            "f_ghz": f / 1e9,
            "peak_realized_dbi": m.peak_realized_dbi,
            "peak_gain_dbi": m.peak_gain_dbi,
            "peak_directivity_dbi": m.peak_directivity_dbi,
            "peak_theta": m.peak_theta,
            "peak_phi": m.peak_phi,
            "rad_efficiency": m.rad_efficiency,
            "total_efficiency": m.total_efficiency,
            "hpbw_main_deg": m.hpbw_e,
            "hpbw_cross_deg": m.hpbw_h,
            "front_to_back_db": m.front_to_back_db,
            "side_lobes_db": m.side_lobe_db,
            "axial_ratio_db": m.axial_ratio_db,
            "polarisation": if m.axial_ratio_db > 20.0 { "linear" } else if m.rhcp_at_peak { "rhcp" } else { "lhcp" },
        });
        if with_cuts {
            let cut = |c: Cut| -> Vec<Value> { ff.cut(c, Quantity::RealizedGain, Pol::Total).iter().filter(|p| (p.0.rem_euclid(15.0)) < 1e-6).map(|p| json!([p.0, (p.1 * 100.0).round() / 100.0])).collect() };
            o["cut_phi0"] = json!(cut(Cut::Phi(0.0)));
            o["cut_phi90"] = json!(cut(Cut::Phi(90.0)));
        }
        far.push(o);
    }
    let convergence = match r.convergence {
        None => json!("not checked"),
        Some(c) => json!({ "converged": c.converged, "passes": c.passes, "delta_s": c.delta_s, "error_left": c.remaining }),
    };
    json!({
        "solver": r.solver,
        "size": r.size,
        "seconds": r.seconds,
        "metal": r.metal,
        // With no loss in the model, rad_efficiency only checks the far-field integral.
        "no_loss_modelled": r.lossless,
        "mesh_density": r.density,
        "mesh_density_unit": r.density_unit,
        "stop_db": r.end_db,
        "mesh_convergence": convergence,
        "ports": ports,
        "far_field": far,
        "warnings": r.warnings
    })
}

fn status_json(s: &Status) -> Value {
    match s {
        Status::Idle => json!({ "state": "idle" }),
        Status::Busy(m, p) => json!({ "state": "running", "message": m, "progress": p }),
        Status::Failed(e) => json!({ "state": "failed", "error": e }),
    }
}

fn parse_goal(v: &Value) -> Result<Goal, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("goal {v}: {e}"))
}

pub fn start(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    if let Some(out) = super::model_tools::tool(world, name, i) {
        return Some(out);
    }
    if let Some(out) = super::gate::tool(world, name, i) {
        return Some(out);
    }
    if let Some(out) = super::reconfig::tool(world, name, i) {
        return Some(out);
    }
    if let Some(out) = super::phased::tool(world, name, i) {
        return Some(out);
    }
    if let Some(out) = super::assist::tool(world, name, i) {
        return Some(out);
    }
    if let Some(out) = super::tagjob::tool(world, name, i) {
        return Some(out);
    }
    let mut st = world.resource_mut::<Studio>();
    Some(match name {
        "antenna_references" => {
            use antenna_core::references;
            let found: Vec<Value> = references::find(i["job"].as_str().unwrap_or("")).into_iter().map(|r| {
                let s = antenna_core::samples::by_id(r.sample);
                json!({ "open_with": r.sample, "job": r.job, "title": s.map(|s| s.title), "band": s.map(|s| s.band_text()), "template": s.map(|s| s.template), "why": r.why, "tune_in_this_order": r.tune, "targets": r.targets, "limits": r.limits, "sources": r.sources })
            }).collect();
            let chips: Vec<Value> = antenna_core::tag::CHIPS.iter().map(|c| json!({ "id": c.id, "name": c.name, "impedance_ohm": c.z.map(|(r, x)| format!("{r} {x:+}j")), "read_dbm": c.read_dbm, "write_dbm": c.write_dbm, "source": c.source, "note": c.note })).collect();
            let others: Vec<Value> = references::other_samples().map(|s| json!({ "open_with": s.id, "title": s.title, "use": s.group.label(), "band": s.band_text(), "about": s.about })).collect();
            text(json!({ "references": found, "tag_chips": chips, "other_samples": others, "evidence": "Simulated, not measured: metal in free space unless a sample says otherwise" }))
        }
        "antenna_list_samples" => {
            let list: Vec<Value> = antenna_core::samples::all().iter().map(|s| json!({ "id": s.id, "title": s.title, "use": s.group.label(), "band": s.band_text(), "about": s.about, "template": s.template, "held_to": s.held_to(), "opening": super::ui::samples::opening_words(s) })).collect();
            text(json!({ "samples": list, "evidence": "Simulated, not measured" }))
        }
        "antenna_open_sample" => {
            let id = i["id"].as_str().unwrap_or("");
            let Some(s) = antenna_core::samples::by_id(id) else {
                return Some(ToolOutput::err(format!("no sample '{id}': see antenna_list_samples")));
            };
            match super::ui::samples::open(&mut st, s) {
                Ok(()) => {
                    let body = json!({ "ok": true, "title": s.title, "band": s.band_text(), "about": s.about, "stored_results": super::ui::samples::has_stored(s.id), "job": status_json(&st.status()), "design": describe(&st.project) });
                    drop(st);
                    world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
                    text(body)
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "antenna_list_templates" => {
            let list: Vec<Value> = templates::all()
                .iter()
                .map(|t| {
                    let fit = Project::from_template(t.id, None).and_then(|p| p.geometry()).map(|g| g.solver_fit()).unwrap_or_default();
                    json!({ "id": t.id, "title": t.title, "kind": t.category.label(), "about": t.blurb, "substrate": t.substrate, "mom": fit.wire_mom || fit.surface_mom })
                })
                .collect();
            text(json!({ "templates": list }))
        }
        "antenna_new" => {
            let id = i["template"].as_str().unwrap_or("");
            match Project::from_template(id, i["f0"].as_str()) {
                Ok(mut p) => {
                    if let Some(n) = i["name"].as_str() {
                        p.name = n.to_string();
                    }
                    st.project = p;
                    st.ui.page = super::ui::Page::Variables;
                    drop(st);
                    world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
                    text(json!({ "ok": true, "design": describe(&world.resource::<Studio>().project) }))
                }
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "antenna_set" => {
            // Known before this call, so that a new row can be told from a change to one that's there.
            let known: Vec<String> = st.project.var_hints().into_iter().map(|h| h.name).collect();
            let mut created = Vec::new();
            if let Some(vars) = i["vars"].as_object() {
                for (k, v) in vars {
                    let expr = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    match st.project.params.iter_mut().find(|p| &p.name == k) {
                        Some(p) => p.expr = expr,
                        None => {
                            st.project.params.push(antenna_core::project::Param::new(k, &expr, "Set by the assistant"));
                            if !known.contains(k) {
                                created.push(k.clone());
                            }
                        }
                    }
                }
            }
            for (key, slot) in [("metal", true), ("substrate", false)] {
                if let Some(m) = i[key].as_str() {
                    if antenna_core::material::by_name(m).is_none() {
                        let names: Vec<String> = antenna_core::material::library().into_iter().map(|x| x.name).collect();
                        return Some(ToolOutput::err(format!("no material '{m}'; known: {}", names.join(", "))));
                    }
                    if slot {
                        st.project.metal = m.to_string();
                    } else {
                        st.project.substrate = m.to_string();
                    }
                }
            }
            if let Some(f) = i["f_lo_ghz"].as_f64() {
                st.project.setup.f_lo = f * 1e9;
            }
            if let Some(f) = i["f_hi_ghz"].as_f64() {
                st.project.setup.f_hi = f * 1e9;
            }
            if let Some(a) = i["far_ghz"].as_array() {
                st.project.setup.far_freqs = a.iter().filter_map(|v| v.as_f64()).map(|v| v * 1e9).collect();
            }
            if let Some(s) = i["solver"].as_str() {
                st.project.setup.solver = match s {
                    "fdtd" => SolverKind::Fdtd,
                    "fdm" | "fit" => SolverKind::Fit,
                    "mom_wire" => SolverKind::MomWire,
                    "mom_surface" => SolverKind::MomSurface,
                    _ => SolverKind::Auto,
                };
            }
            if let Some(v) = i["cells_per_lambda"].as_f64() {
                st.project.setup.cells_per_lambda = v;
            }
            if let Some(v) = i["adaptive_mesh"].as_bool() {
                st.project.setup.adaptive = v;
            }
            if let Some(a) = i["wave_feeds"].as_array() {
                st.project.wave_feeds = a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            }
            if let Some(v) = i["segments_per_lambda"].as_f64() {
                st.project.setup.segments_per_lambda = v;
            }
            if let Some(v) = i["edges_per_lambda"].as_f64() {
                st.project.setup.edges_per_lambda = v;
            }
            if let Some(v) = i["mom_samples"].as_u64() {
                st.project.setup.mom_samples = v as usize;
            }
            match st.project.geometry() {
                Ok(_) => {
                    // A template's rows are read by its code, so only a drawing can tell. A new name
                    // nothing uses is usually a wrong one, and would sit in the user's table doing nothing.
                    created.retain(|name| st.project.template == "custom" && antenna_core::freeform::what_it_moves(&st.project, name).is_empty());
                    st.project.params.retain(|r| !created.contains(&r.name));
                    let mut out = json!({ "ok": true, "design": describe(&st.project) });
                    if !created.is_empty() {
                        out["changes_nothing"] = json!(created);
                        out["changes_nothing_note"] = json!("No part is written over these, so they weren't added and nothing moved. A wrong name? See variables in design. To add a dimension, send the part again with antenna_draw, written over it.");
                    }
                    text(out)
                }
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "antenna_get" => {
            let mut d = describe(&st.project);
            if !st.project.extra.is_empty() || !st.project.extra_ports.is_empty() {
                d["drawing"] = antenna_core::freeform::written(&st.project);
            }
            text(d)
        }
        "antenna_run" => {
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            if let Err(e) = st.project.geometry() {
                return Some(ToolOutput::err(format!("{e:#}")));
            }
            st.run();
            text(json!({ "started": true }))
        }
        "antenna_results" => {
            let status = status_json(&st.status());
            let f0 = st.project.f0();
            let res = st.latest().map(|r| result_json(&r, f0, i["cuts"].as_bool().unwrap_or(false)));
            text(json!({ "job": status, "latest": res }))
        }
        "antenna_optimize" => {
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            let vars: Vec<(String, f64, f64)> = i["vars"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| Some((v["name"].as_str()?.to_string(), v["min"].as_f64()?, v["max"].as_f64()?))).collect());
            if vars.is_empty() {
                return Some(ToolOutput::err("give at least one variable with name, min and max"));
            }
            let mut goals = Vec::new();
            for g in i["goals"].as_array().map_or(&[][..], |a| a.as_slice()) {
                match parse_goal(g) {
                    Ok(x) => goals.push((x, g["weight"].as_f64().unwrap_or(1.0))),
                    Err(e) => return Some(ToolOutput::err(e)),
                }
            }
            if goals.is_empty() {
                return Some(ToolOutput::err("give at least one goal"));
            }
            let method = match i["method"].as_str() {
                Some("surrogate") => Method::Surrogate,
                Some("particle_swarm") => Method::ParticleSwarm,
                Some("differential_evolution") => Method::DifferentialEvolution,
                _ => Method::NelderMead,
            };
            let n = i["max_runs"].as_u64().unwrap_or(40) as usize;
            let par = i["parallel"].as_u64().unwrap_or(1) as usize;
            st.run_optimize(vars, goals, method, n, par);
            text(json!({ "started": true }))
        }
        "antenna_optimize_status" => {
            let job = status_json(&st.status());
            let Some(o) = st.opt() else { return Some(text(json!({ "job": job, "optimizer": Value::Null }))) };
            if i["apply"].as_bool().unwrap_or(false) && !o.best_x.is_empty() {
                let values: Vec<(String, f64)> = o.names.iter().cloned().zip(o.best_x.iter().copied()).collect();
                st.project = st.project.with_values(&values);
                if let Some(b) = o.best.clone() {
                    st.show_result(b);
                }
            }
            let best: serde_json::Map<String, Value> = o.names.iter().cloned().zip(o.best_x.iter().map(|v| json!(v))).collect();
            text(json!({ "job": job, "optimizer": { "runs": o.evals, "finished": o.finished, "best_penalty": o.best_value, "best": best } }))
        }
        "antenna_cancel" => {
            st.cancel();
            text(json!({ "ok": true }))
        }
        _ => return None,
    })
}

fn describe(p: &Project) -> Value {
    let vars = p.vars().ok();
    let rows: Vec<Value> = p.params.iter().map(|r| json!({ "name": r.name, "expr": r.expr, "value": vars.as_ref().and_then(|v| v.get(&r.name)), "note": r.note })).collect();
    let g = p.geometry();
    let mut d = json!({
        "name": p.name,
        "template": p.template,
        "metal": p.metal,
        "substrate": p.substrate,
        "variables": rows,
        "sweep_ghz": [p.setup.f_lo / 1e9, p.setup.f_hi / 1e9],
        "far_ghz": p.setup.far_freqs.iter().map(|f| f / 1e9).collect::<Vec<_>>(),
        "solver": p.setup.solver.label(),
        // Higher is finer. Saying so stops anyone "refining" a mesh downwards.
        "mesh": { "edges_per_lambda": p.setup.edges_per_lambda, "cells_per_lambda": p.setup.cells_per_lambda },
    });
    if !p.stack.is_empty() {
        d["surroundings"] = json!(p.stack.describe());
    }
    match g {
        Ok(g) => {
            let (lo, hi) = g.bounds();
            d["size_mm"] = json!([(hi[0] - lo[0]) * 1e3, (hi[1] - lo[1]) * 1e3, (hi[2] - lo[2]) * 1e3]);
            d["ports"] = json!(g.ports.iter().map(|x| json!({ "name": x.name, "z0": x.z0 })).collect::<Vec<_>>());
            d["solver_used"] = json!(p.solver_for(&g).label());
            if p.reconfig.is_on() {
                d["reconfigurable"] = json!({
                    "parts": p.reconfig.elements.iter().map(|e| json!({ "name": e.name, "port": e.port, "part": e.device.part })).collect::<Vec<_>>(),
                    "states": p.reconfig.states.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                    "active_state": p.reconfig.active,
                    "note": "A run solves the active state, feeds only. antenna_reconfig_solve answers for every state.",
                });
            }
            if let Ok(plan) = antenna_core::csx::plan(&g, &p.setup, p.f0()) {
                d["fdtd_estimate"] = json!({ "cells_m": plan.cells() as f64 / 1e6, "memory_gb": plan.memory_bytes() as f64 / 1e9 });
            }
        }
        Err(e) => d["error"] = json!(format!("{e:#}")),
    }
    d
}
