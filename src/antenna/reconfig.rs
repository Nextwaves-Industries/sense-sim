//! Reconfigurable antennas' background jobs and the assistant's tools for them. The
//! physics is `antenna_core::reconfig`: one solve of all the ports, then algebra per state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use antenna_core::geometry::Geometry;
use antenna_core::optimize::{self, Method};
use antenna_core::project::{ExtraPort, Project, Setup, SimResult, SolverKind};
use antenna_core::reconfig::{self, Device, Element, Found, Model, Mount, PolGoal, Reconfig, Report, Setting, Solved, StateReport, Target, TuneState};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::run::{self, RunOptions};
use super::{OptState, Studio, slim};
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

/// One solve of all the ports. Parts and states change without another.
#[derive(Clone)]
pub struct ReconfigRun {
    pub basis: Arc<SimResult>,
    pub names: Vec<String>,
    /// What it was solved for, to notice when it no longer applies.
    pub geometry: Geometry,
    pub setup: Setup,
}

/// Whether a solve with `a` answers for `b`: all but the far-field list, which the solve
/// planned for itself.
fn same_solve(a: &Setup, b: &Setup) -> bool {
    let strip = |s: &Setup| {
        let mut s = s.clone();
        s.far_freqs.clear();
        s.field_planes = false;
        s
    };
    strip(a) == strip(b)
}

impl ReconfigRun {
    pub fn applies_to(&self, geometry: &Geometry, setup: &Setup) -> bool {
        *geometry == self.geometry && same_solve(&self.setup, setup)
    }

    pub fn solved<'a>(&'a self, rc: &Reconfig) -> anyhow::Result<Solved<'a>> {
        Solved::new(&self.basis, &self.names, rc)
    }
}

/// The project as the all-ports solve runs it: patterns everywhere a state might work.
fn prepared(project: &Project) -> Project {
    let mut p = project.clone();
    p.setup.far_freqs = p.reconfig.far_plan(&p.setup);
    p.setup.field_planes = false;
    p
}

impl Studio {
    pub fn reconfig_run(&self) -> Option<ReconfigRun> {
        self.shared.lock().unwrap().reconfig.clone()
    }

    /// The last search's findings, once.
    pub fn take_reconfig_found(&self) -> Option<Vec<Found>> {
        self.shared.lock().unwrap().reconfig_found.take()
    }

    /// Solves all the ports, and shows the active state on the results pages.
    pub fn run_reconfig(&mut self) {
        let project = self.project.clone();
        self.spawn(move |ctx, shared| {
            let p = prepared(&project);
            let geometry = p.geometry()?;
            let names: Vec<String> = geometry.ports.iter().map(|x| x.name.clone()).collect();
            let basis = run::simulate_ports(&p, ctx, &RunOptions { keep: false, ..Default::default() })?;
            let run = ReconfigRun { basis: Arc::new(basis), names, geometry, setup: project.setup.clone() };
            let shown = shown_state(&project, &run, ctx)?;
            let mut s = shared.lock().unwrap();
            if let Some(r) = shown {
                s.latest = Some(Arc::new(r));
            }
            s.reconfig = Some(run);
            Ok(())
        });
    }

    /// Puts one state on the result pages, its currents included where the solver has them.
    pub fn view_state(&mut self, k: usize) {
        self.project.reconfig.active = k;
        let Some(run) = self.reconfig_run() else { return };
        let project = self.project.clone();
        self.spawn(move |ctx, shared| {
            let shown = shown_state(&project, &run, ctx)?;
            if let Some(r) = shown {
                shared.lock().unwrap().latest = Some(Arc::new(r));
            }
            Ok(())
        });
    }

    /// Looks for the best settings for each target, on the solved network.
    pub fn search_reconfig(&mut self, volts: usize) {
        let Some(run) = self.reconfig_run() else { return };
        let rc = self.project.reconfig.clone();
        self.spawn(move |ctx, shared| {
            ctx.busy("Searching the settings for each target…", -1.0);
            let found = run.solved(&rc)?.search(&rc, volts);
            shared.lock().unwrap().reconfig_found = Some(found);
            Ok(())
        });
    }

    /// Changes dimensions until the states can meet every target. Each step is a full solve
    /// of all the ports, then a search of the settings, so every part keeps doing its best.
    pub fn run_reconfig_optimize(&mut self, vars: Vec<(String, f64, f64)>, method: Method, max_evals: usize, parallel: usize) {
        let base = self.project.clone();
        let names: Vec<String> = vars.iter().map(|v| v.0.clone()).collect();
        let bounds: Vec<(f64, f64)> = vars.iter().map(|v| (v.1, v.2)).collect();
        let start = base.vars().unwrap_or_default();
        let x0: Vec<f64> = vars.iter().map(|v| start.get(&v.0).copied().unwrap_or((v.1 + v.2) / 2.0).clamp(v.1, v.2)).collect();
        self.spawn(move |ctx, shared| {
            shared.lock().unwrap().opt = Some(OptState { names: names.clone(), best_value: f64::MAX, method: format!("{method:?}"), ..Default::default() });
            let evals = AtomicUsize::new(0);
            let first_error: Mutex<Option<String>> = Mutex::new(None);
            let parallel = parallel.max(1);
            let objective = |x: &[f64]| -> f64 {
                if ctx.cancelled() {
                    return 1e6;
                }
                let values: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
                let p = prepared(&base.with_values(&values));
                let n = evals.fetch_add(1, Ordering::SeqCst) + 1;
                let opts = RunOptions { label: String::new(), keep: false, threads: 0, share: parallel, report: false };
                let done = run::simulate_ports(&p, ctx, &opts).and_then(|basis| {
                    let g = p.geometry()?;
                    let port_names: Vec<String> = g.ports.iter().map(|x| x.name.clone()).collect();
                    let solved = Solved::new(&basis, &port_names, &p.reconfig)?;
                    let value: f64 = solved.search(&p.reconfig, 7).iter().map(|f| f.score.penalty).sum();
                    let state = p.reconfig.active_state().cloned().unwrap_or_else(|| p.reconfig.blank_state("All off"));
                    Ok((value, solved.result(&p.reconfig, &state)?))
                });
                match done {
                    Ok((value, shown)) => {
                        let mut s = shared.lock().unwrap();
                        if let Some(o) = s.opt.as_mut() {
                            o.evals = n;
                            if value < o.best_value {
                                o.best_value = value;
                                o.best_x = x.to_vec();
                                o.best = Some(Arc::new(slim(shown)));
                                o.history.push((n, value));
                            }
                        }
                        ctx.busy(format!("Tuning for every target: {n} of {max_evals} solves, miss {value:.1} dB"), n as f32 / max_evals.max(1) as f32);
                        value
                    }
                    Err(e) => {
                        first_error.lock().unwrap().get_or_insert_with(|| format!("{e:#}"));
                        1e6
                    }
                }
            };
            let options = optimize::Options { method, max_evals, seed: 1, population: None, tol: 1e-3, parallel };
            let outcome = optimize::minimize(objective, &x0, &bounds, &options, |_| !ctx.cancelled());
            if outcome.value >= 1e6
                && let Some(e) = first_error.into_inner().unwrap()
            {
                anyhow::bail!(e);
            }
            if let Some(o) = shared.lock().unwrap().opt.as_mut() {
                o.finished = true;
                o.evals = outcome.evals;
            }
            Ok(())
        });
    }
}

/// The active state as a result, redrawn with its own currents.
fn shown_state(project: &Project, run: &ReconfigRun, _ctx: &run::JobCtx) -> anyhow::Result<Option<SimResult>> {
    let rc = &project.reconfig;
    let Some(state) = rc.active_state() else { return Ok(None) };
    let solved = run.solved(rc)?;
    let mut r = solved.result(rc, state)?;
    if project.solver_for(&run.geometry) != SolverKind::Fdtd {
        let report = solved.state_report(rc, state)?;
        r.currents = run::state_currents(project, &run.geometry, &solved, &report.settings, report.f_op).ok();
    }
    Ok(Some(r))
}

/// A part as the tools name it: from the library, with any of its values replaced.
fn device_from(v: &Value) -> Result<Device, String> {
    let part = v["part"].as_str().unwrap_or("Ideal switch");
    let mut d = Device::by_part(part).ok_or_else(|| {
        let names: Vec<String> = Device::library().into_iter().map(|d| d.part).collect();
        format!("no part '{part}'; known: {}", names.join(", "))
    })?;
    let num = |key: &str, slot: &mut f64, unit: f64| {
        if let Some(x) = v[key].as_f64() {
            *slot = x * unit;
        }
    };
    let mut changed = false;
    let before = d.model;
    match &mut d.model {
        Model::Switch { r_on, c_off, r_off, ls, bias_ma, v_forward, v_reverse } => {
            num("r_on_ohm", r_on, 1.0);
            num("c_off_pf", c_off, 1e-12);
            num("r_off_ohm", r_off, 1.0);
            num("ls_nh", ls, 1e-9);
            num("bias_ma", bias_ma, 1.0);
            num("v_forward", v_forward, 1.0);
            num("v_reverse", v_reverse, 1.0);
        }
        Model::Varactor { cj0, vj, m, cp, rs, ls, v_max } => {
            num("cj0_pf", cj0, 1e-12);
            num("vj", vj, 1.0);
            num("m", m, 1.0);
            num("cp_pf", cp, 1e-12);
            num("rs_ohm", rs, 1.0);
            num("ls_nh", ls, 1e-9);
            num("v_max", v_max, 1.0);
        }
        Model::Fixed { r, l, c } => {
            num("r_ohm", r, 1.0);
            num("l_nh", l, 1e-9);
            num("c_pf", c, 1e-12);
        }
    }
    changed |= d.model != before;
    if changed {
        d.source = format!("{} with values set by hand.", d.part);
    }
    num("v_limit", &mut d.v_limit, 1.0);
    num("i_limit_ma", &mut d.i_limit, 1e-3);
    num("p_limit_mw", &mut d.p_limit, 1e-3);
    Ok(d)
}

fn mount_from(v: &Value) -> Mount {
    let g = |k: &str, unit: f64| v[k].as_f64().map_or(0.0, |x| x * unit);
    Mount { c_block: g("c_block_pf", 1e-12), l_series: g("l_series_nh", 1e-9), l_choke: g("l_choke_nh", 1e-9), choke_srf: g("choke_srf_ghz", 1e9), choke_q: g("choke_q", 1.0), c_pad: g("c_pad_pf", 1e-12) }
}

/// A setting as the tools write it: `"on"`, `"off"` or a number of volts.
fn setting_from(v: &Value) -> Option<Setting> {
    match v {
        Value::Number(n) => n.as_f64().map(Setting::Volts),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "on" => Some(Setting::On),
            "off" => Some(Setting::Off),
            other => other.trim_end_matches('v').trim().parse().ok().map(Setting::Volts),
        },
        _ => None,
    }
}

fn state_from(rc: &Reconfig, k: usize, v: &Value) -> Result<TuneState, String> {
    let mut s = rc.blank_state(v["name"].as_str().unwrap_or(&format!("State {}", k + 1)));
    match &v["settings"] {
        Value::Object(map) => {
            for (name, value) in map {
                let Some(i) = rc.elements.iter().position(|e| e.name == *name) else { return Err(format!("state {}: no part called '{name}'", k + 1)) };
                let Some(set) = setting_from(value) else { return Err(format!("state {}: '{value}' isn't on, off or volts", k + 1)) };
                s.settings[i] = rc.elements[i].device.normalized(set);
            }
        }
        Value::Array(list) => {
            if list.len() != rc.elements.len() {
                return Err(format!("state {} sets {} parts and there are {}", k + 1, list.len(), rc.elements.len()));
            }
            for (i, value) in list.iter().enumerate() {
                let Some(set) = setting_from(value) else { return Err(format!("state {}: '{value}' isn't on, off or volts", k + 1)) };
                s.settings[i] = rc.elements[i].device.normalized(set);
            }
        }
        _ => return Err(format!("state {} needs settings, by part name or in the parts' order", k + 1)),
    }
    s.f_op = v["f_ghz"].as_f64().map(|f| f * 1e9);
    Ok(s)
}

fn target_from(v: &Value) -> Result<Target, String> {
    let (Some(lo), Some(hi)) = (v["f_lo_ghz"].as_f64(), v["f_hi_ghz"].as_f64()) else { return Err("a target needs f_lo_ghz and f_hi_ghz".into()) };
    let mut t = Target::band(v["name"].as_str().unwrap_or("Target"), lo * 1e9, hi * 1e9);
    if let Some(l) = v["limit_db"].as_f64() {
        t.limit_db = l;
    }
    if let (Some(a), Some(b)) = (v["theta"].as_f64(), v["phi"].as_f64()) {
        t.beam = Some([a, b]);
    }
    t.min_gain_dbi = v["min_gain_dbi"].as_f64();
    t.pol = match v["pol"].as_str().map(str::to_ascii_lowercase).as_deref() {
        Some("linear") => Some(PolGoal::Linear),
        Some("rhcp") => Some(PolGoal::Rhcp),
        Some("lhcp") => Some(PolGoal::Lhcp),
        None => None,
        Some(other) => return Err(format!("no polarisation '{other}': linear, rhcp or lhcp")),
    };
    Ok(t)
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn state_json(s: &StateReport) -> Value {
    let m = s.metrics.as_ref();
    json!({
        "state": s.name,
        "f_op_ghz": r3(s.f_op / 1e9),
        "s11_db": r3(s.s_op_db),
        "resonances_ghz": s.resonances.iter().map(|r| json!([r3(r.0 / 1e9), r3(r.1)])).collect::<Vec<_>>(),
        "bands_under_10db_ghz": s.bands.iter().map(|b| json!([r3(b.0 / 1e9), r3(b.1 / 1e9)])).collect::<Vec<_>>(),
        "pattern_at_ghz": s.far_f.map(|f| r3(f / 1e9)),
        "realized_gain_dbi": m.map(|m| r3(m.peak_realized_dbi)),
        "peak_theta": m.map(|m| m.peak_theta),
        "peak_phi": m.map(|m| m.peak_phi),
        "total_efficiency": m.map(|m| r3(m.total_efficiency)),
        "polarisation": s.polarisation,
        "lost_in_parts": r3(s.part_loss),
        "bias_ma": r3(s.dc_ma),
        "bias_mw": r3(s.dc_mw),
        "parts": s.parts.iter().map(|p| json!({
            "name": p.name,
            "setting": p.setting,
            "z_ohm": [r3(p.z_load.re), r3(p.z_load.im)],
            "v_peak": r3(p.v_peak),
            "i_peak_ma": r3(p.i_peak * 1e3),
            "device_mw": r3(p.p_device * 1e3),
            "max_power_dbm": p.max_power_dbm.map(r3),
            "first_limit": p.limit,
            "over_limit": p.over,
            "signal_moves_capacitance": p.nonlinear,
        })).collect::<Vec<_>>(),
    })
}

fn report_json(r: &Report) -> Value {
    json!({
        "states": r.states.iter().map(state_json).collect::<Vec<_>>(),
        "tuning_ghz": r.tuning.map(|t| json!([r3(t.0 / 1e9), r3(t.1 / 1e9)])),
        "covered_under_10db_ghz": r.coverage.iter().map(|b| json!([r3(b.0 / 1e9), r3(b.1 / 1e9)])).collect::<Vec<_>>(),
        "beam_span_deg": r3(r.beam_span_deg),
        "targets_met_by": r.met_by.iter().map(|(t, list)| json!({ "target": t, "states": list })).collect::<Vec<_>>(),
        "warnings": r.warnings,
    })
}

fn found_json(rc: &Reconfig, f: &Found) -> Value {
    let settings: serde_json::Map<String, Value> = rc.elements.iter().zip(&f.settings).map(|(e, s)| (e.name.clone(), json!(e.device.describe(*s)))).collect();
    json!({
        "target": f.target,
        "met": f.score.met,
        "miss_db": r3(f.score.penalty),
        "worst_s11_db": r3(f.score.worst_s_db),
        "gain_dbi": f.score.gain_dbi.map(r3),
        "axial_ratio_db": f.score.axial_ratio_db.map(r3),
        "hand": if f.score.rhcp { "rhcp" } else { "lhcp" },
        "peak": f.score.peak,
        "settings": settings,
        "tried": f.tried,
        "every_combination_tried": f.exhaustive,
    })
}

/// States for what a search found, one per target.
pub fn states_from_found(rc: &Reconfig, found: &[Found], targets: &[Target]) -> Vec<TuneState> {
    let _ = rc;
    found.iter().zip(targets).map(|(f, t)| TuneState { name: f.target.clone(), settings: f.settings.clone(), f_op: Some(t.center()) }).collect()
}

// ---------------------------------------------------------------------------
// The assistant's tools

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    vec![
        def("antenna_reconfig_parts", "The switch, varactor and fixed parts Antenna Studio knows, with their circuit values and where those come from. Use a part's name with antenna_reconfig_set; any value can be overridden with your datasheet's.", json!({ "type": "object", "properties": {} })),
        def(
            "antenna_reconfig_set",
            "Make the current antenna reconfigurable. elements: parts across gaps [{name, port (an existing port), part, or a, b, width (expressions, to add the gap's port) ; overrides r_on_ohm c_off_pf r_off_ohm ls_nh bias_ma v_forward v_reverse cj0_pf vj m cp_pf rs_ohm v_max r_ohm l_nh c_pf v_limit i_limit_ma p_limit_mw; mount c_block_pf l_series_nh l_choke_nh choke_srf_ghz choke_q c_pad_pf}]. Giving elements replaces the parts. states: [{name, settings: {part: \"on\"|\"off\"|volts}, f_ghz}], or all_states: true for every combination. targets: [{name, f_lo_ghz, f_hi_ghz, limit_db, theta, phi, min_gain_dbi, pol: linear|rhcp|lhcp}]. power_dbm for the voltage and current on the parts; active: which state ordinary runs solve (by index). Parts and states never need a new solve, only moved metal does.",
            json!({ "type": "object", "properties": { "elements": { "type": "array" }, "states": { "type": "array" }, "all_states": { "type": "boolean" }, "targets": { "type": "array" }, "power_dbm": { "type": "number" }, "active": { "type": "integer" } } }),
        ),
        def("antenna_reconfig_solve", "Solve every port of the reconfigurable antenna once, in the background, which then answers for all its states. MoM takes one sweep; FDTD one run per port. Poll antenna_reconfig_results.", json!({ "type": "object", "properties": {} })),
        def("antenna_reconfig_results", "Job status and every state's figures: resonance, band, realized gain, beam direction, polarisation, loss in the parts, bias power, RF voltage and current on each part with the power that reaches its limit, what the states cover together, which meet the targets, what it all assumes and its evidence label.", json!({ "type": "object", "properties": {} })),
        def("antenna_reconfig_search", "Find the best settings for each target on the solved network, across every combination of switches and bias steps. No new solve. Background job, then antenna_reconfig_found.", json!({ "type": "object", "properties": { "bias_steps": { "type": "integer", "description": "Steps per varactor (default 9)" } } })),
        def("antenna_reconfig_found", "What the last search found per target. apply=true makes them the states, one per target.", json!({ "type": "object", "properties": { "apply": { "type": "boolean" } } })),
        def("antenna_reconfig_tuning", "A varactor's tuning curve on the solved network: resonance, match, band and part efficiency against bias. element: its name, or leave out to sweep every varactor together as on a shared bias line.", json!({ "type": "object", "properties": { "element": { "type": "string" }, "steps": { "type": "integer" } } })),
        def("antenna_reconfig_spread", "The chance a build still meets a target in a state when parts differ: spread is the relative 1σ of each part's values (0.1 is ±10 %). Names the part that matters most. Needs a solve.", json!({ "type": "object", "properties": { "state": { "type": "integer" }, "target": { "type": "integer" }, "spread": { "type": "number" } } })),
        def("antenna_reconfig_view", "Show one state on the Studio's result pages, far field, currents, array, matching and link: state by index.", json!({ "type": "object", "properties": { "state": { "type": "integer" } }, "required": ["state"] })),
    ]
}

fn text(v: Value) -> ToolOutput {
    ToolOutput::text(v.to_string())
}

/// Handles this module's tools; `None` for anything else.
pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    if !name.starts_with("antenna_reconfig_") {
        return None;
    }
    let mut st = world.resource_mut::<Studio>();
    let job = match st.status() {
        super::Status::Idle => json!({ "state": "idle" }),
        super::Status::Busy(m, p) => json!({ "state": "running", "message": m, "progress": p }),
        super::Status::Failed(e) => json!({ "state": "failed", "error": e }),
    };
    Some(match name {
        "antenna_reconfig_parts" => {
            let parts: Vec<Value> = Device::library().iter().map(|d| json!({ "part": d.part, "values": d.summary(), "source": d.source })).collect();
            text(json!({ "parts": parts, "note": "Typical figures. Override them with your own datasheet's at your bias and frequency." }))
        }
        "antenna_reconfig_set" => {
            if let Some(list) = i["elements"].as_array() {
                let mut project = st.project.clone();
                project.reconfig.elements.clear();
                project.reconfig.states.clear();
                project.reconfig.active = 0;
                for (k, e) in list.iter().enumerate() {
                    let device = match device_from(e) {
                        Ok(d) => d,
                        Err(m) => return Some(ToolOutput::err(m)),
                    };
                    let element_name = e["name"].as_str().map_or_else(|| format!("D{}", k + 1), str::to_string);
                    let port = match (e["port"].as_str(), e["a"].as_array(), e["b"].as_array()) {
                        (Some(p), _, _) => p.to_string(),
                        (None, Some(a), Some(b)) if a.len() == 3 && b.len() == 3 => {
                            let word = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_string);
                            let triple = |v: &Vec<Value>| [word(&v[0]), word(&v[1]), word(&v[2])];
                            let port_name = antenna_core::sketch::free_name(&project, &element_name);
                            let width = e["width"].as_str().map_or_else(|| e["width"].as_f64().map_or("0".to_string(), |w| w.to_string()), str::to_string);
                            project.extra_ports.push(ExtraPort { name: port_name.clone(), a: triple(a), b: triple(b), width, z0: "50".into() });
                            port_name
                        }
                        _ => return Some(ToolOutput::err(format!("part '{element_name}' needs a port, or a and b to add one"))),
                    };
                    project.reconfig.add_element(Element { name: element_name, port, device, mount: mount_from(e) });
                }
                if let Err(e) = project.geometry() {
                    return Some(ToolOutput::err(format!("{e:#}")));
                }
                st.project = project;
            }
            if let Some(list) = i["targets"].as_array() {
                let mut out = Vec::with_capacity(list.len());
                for t in list {
                    match target_from(t) {
                        Ok(v) => out.push(v),
                        Err(m) => return Some(ToolOutput::err(m)),
                    }
                }
                st.project.reconfig.targets = out;
            }
            if let Some(list) = i["states"].as_array() {
                let mut out = Vec::with_capacity(list.len());
                for (k, s) in list.iter().enumerate() {
                    match state_from(&st.project.reconfig, k, s) {
                        Ok(v) => out.push(v),
                        Err(m) => return Some(ToolOutput::err(m)),
                    }
                }
                st.project.reconfig.states = out;
            }
            if i["all_states"].as_bool().unwrap_or(false) {
                match st.project.reconfig.all_combinations(5, 256) {
                    Ok(v) => st.project.reconfig.states = v,
                    Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
                }
            }
            if let Some(p) = i["power_dbm"].as_f64() {
                st.project.reconfig.power_dbm = p;
            }
            if let Some(a) = i["active"].as_u64() {
                st.project.reconfig.active = a as usize;
            }
            st.ui.page = super::ui::Page::Reconfig;
            let rc = &st.project.reconfig;
            let bound = st.project.geometry().map_err(|e| format!("{e:#}")).and_then(|g| rc.bind(&g.ports.iter().map(|p| p.name.clone()).collect::<Vec<_>>()).map(|b| (g, b)).map_err(|e| format!("{e:#}")));
            let (feeds, problem) = match bound {
                Ok((g, b)) => (b.feeds.iter().map(|k| g.ports[*k].name.clone()).collect::<Vec<_>>(), None),
                Err(e) => (Vec::new(), Some(e)),
            };
            let fresh = st.reconfig_run().is_some_and(|r| st.project.geometry().is_ok_and(|g| r.applies_to(&g, &st.project.setup)));
            text(json!({
                "ok": problem.is_none(),
                "problem": problem,
                "feeds": feeds,
                "parts": st.project.reconfig.elements.iter().map(|e| json!({ "name": e.name, "port": e.port, "part": e.device.part, "values": e.device.summary() })).collect::<Vec<_>>(),
                "states": st.project.reconfig.states.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "targets": st.project.reconfig.targets.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
                "solve_still_applies": fresh,
                "next": if fresh { "antenna_reconfig_results has the new figures: no solve needed." } else { "antenna_reconfig_solve, then antenna_reconfig_results." },
            }))
        }
        "antenna_reconfig_solve" => {
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            if !st.project.reconfig.is_on() {
                return Some(ToolOutput::err("this antenna has no parts yet: use antenna_reconfig_set, or antenna_new with a reconfig_ template"));
            }
            let g = match st.project.geometry() {
                Ok(g) => g,
                Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
            };
            if let Err(e) = st.project.reconfig.bind(&g.ports.iter().map(|p| p.name.clone()).collect::<Vec<_>>()) {
                return Some(ToolOutput::err(format!("{e:#}")));
            }
            let (solver, n) = (st.project.solver_for(&g), g.ports.len());
            st.ui.page = super::ui::Page::Reconfig;
            st.run_reconfig();
            text(json!({ "started": true, "solver": solver.label(), "ports": n, "runs": if solver == SolverKind::Fdtd { n } else { 1 } }))
        }
        "antenna_reconfig_results" => {
            let Some(run) = st.reconfig_run() else { return Some(text(json!({ "job": job, "reconfig": Value::Null }))) };
            let applies = st.project.geometry().is_ok_and(|g| run.applies_to(&g, &st.project.setup));
            if !applies {
                return Some(text(json!({ "job": job, "reconfig": Value::Null, "note": "The metal or the solver setup changed since the last solve: run antenna_reconfig_solve again." })));
            }
            let rc = &st.project.reconfig;
            match run.solved(rc).and_then(|s| s.report(rc)) {
                Ok(r) => text(json!({ "job": job, "evidence": proof::Evidence::Simulated.label(), "solver": run.basis.solver, "seconds": run.basis.seconds, "reconfig": report_json(&r), "assumptions": reconfig::assumptions(rc, &run.basis), "solver_warnings": run.basis.warnings })),
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "antenna_reconfig_search" => {
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            if st.project.reconfig.targets.is_empty() {
                return Some(ToolOutput::err("set some targets first, with antenna_reconfig_set"));
            }
            if st.reconfig_run().is_none() {
                return Some(ToolOutput::err("solve it first with antenna_reconfig_solve"));
            }
            st.search_reconfig(i["bias_steps"].as_u64().unwrap_or(9).clamp(2, 41) as usize);
            text(json!({ "started": true }))
        }
        "antenna_reconfig_found" => {
            let Some(found) = st.take_reconfig_found().or_else(|| st.ui.reconfig.found.clone()) else { return Some(text(json!({ "job": job, "found": Value::Null }))) };
            st.ui.reconfig.found = Some(found.clone());
            let out: Vec<Value> = found.iter().map(|f| found_json(&st.project.reconfig, f)).collect();
            if i["apply"].as_bool().unwrap_or(false) {
                let states = states_from_found(&st.project.reconfig, &found, &st.project.reconfig.targets);
                st.project.reconfig.states = states;
                st.project.reconfig.active = 0;
            }
            text(json!({ "job": job, "evidence": proof::Evidence::Simulated.label(), "found": out }))
        }
        "antenna_reconfig_tuning" => {
            let Some(run) = st.reconfig_run() else { return Some(ToolOutput::err("solve it first with antenna_reconfig_solve")) };
            let rc = &st.project.reconfig;
            let which = match i["element"].as_str() {
                Some(n) => match rc.elements.iter().position(|e| e.name == n) {
                    Some(k) => Some(k),
                    None => return Some(ToolOutput::err(format!("no part called '{n}'"))),
                },
                None => None,
            };
            let base = rc.active_state().cloned().unwrap_or_else(|| rc.blank_state("base"));
            let solved = match run.solved(rc) {
                Ok(s) => s,
                Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
            };
            let curve = solved.tuning(rc, &base, which, i["steps"].as_u64().unwrap_or(13).clamp(2, 81) as usize);
            if curve.is_empty() {
                return Some(ToolOutput::err("no varactor to sweep"));
            }
            text(json!({ "curve": curve.iter().map(|c| json!({ "volts": r3(c.volts), "pf": r3(c.capacitance * 1e12), "f_res_ghz": c.f_res.map(|f| r3(f / 1e9)), "depth_db": r3(c.depth_db), "band_ghz": c.band.map(|b| [r3(b.0 / 1e9), r3(b.1 / 1e9)]), "part_efficiency": r3(c.part_efficiency) })).collect::<Vec<_>>() }))
        }
        "antenna_reconfig_spread" => {
            let Some(run) = st.reconfig_run() else { return Some(ToolOutput::err("solve it first with antenna_reconfig_solve")) };
            let rc = &st.project.reconfig;
            let k = i["state"].as_u64().unwrap_or(rc.active as u64) as usize;
            let t = i["target"].as_u64().unwrap_or(0) as usize;
            let (Some(state), Some(target)) = (rc.states.get(k), rc.targets.get(t)) else { return Some(ToolOutput::err("give a state and a target that exist")) };
            let solved = match run.solved(rc) {
                Ok(s) => s,
                Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
            };
            let r = solved.robust(rc, state, target, i["spread"].as_f64().unwrap_or(0.1).clamp(0.001, 0.5), 11);
            text(json!({ "evidence": r.prediction.evidence.label(), "metric": "worst reflection in band, dB", "nominal": r3(r.nominal), "low": r3(r.prediction.low), "high": r3(r.prediction.high), "chance_of_target": r.pass_probability.map(r3), "main_driver": r.main_driver().map(|d| d.factor.clone()), "basis": "Assumed part-to-part spread, not a datasheet's." }))
        }
        "antenna_reconfig_view" => {
            let k = i["state"].as_u64().unwrap_or(0) as usize;
            if k >= st.project.reconfig.states.len() {
                return Some(ToolOutput::err(format!("there are {} states", st.project.reconfig.states.len())));
            }
            if st.reconfig_run().is_none() {
                return Some(ToolOutput::err("solve it first with antenna_reconfig_solve"));
            }
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            st.view_state(k);
            text(json!({ "ok": true, "state": st.project.reconfig.states[k].name }))
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use antenna_core::project::Goal;

    use super::*;
    use crate::antenna::Status;

    fn finish(s: &Studio) {
        let t0 = std::time::Instant::now();
        while s.busy() {
            assert!(t0.elapsed().as_secs() < 300, "job still running: {:?}", s.status());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if let Status::Failed(e) = s.status() {
            panic!("job failed: {e}");
        }
    }

    fn studio(id: &str) -> Studio {
        let mut s = Studio::default();
        s.project = Project::from_template(id, None).unwrap();
        s.project.setup.mom_samples = 15;
        s
    }

    /// A plain run hands every page the state, not the ports.
    #[test]
    fn antenna_reconfig_a_run_is_the_active_state_with_its_feed_alone() {
        let mut s = studio("reconfig_dipole");
        s.project.reconfig.active = 2;
        s.run();
        finish(&s);
        let r = s.latest().unwrap();
        assert_eq!(r.network.ports(), 1, "one feed, not five ports");
        assert_eq!(r.far.len(), 1);
        assert!(r.label.contains("Long"), "{}", r.label);
        assert!(r.currents.is_some(), "its own currents");
        // Everything on is the longest, so the lowest resonance.
        let long = r.network.resonances(0).iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
        s.project.reconfig.active = 0;
        s.run();
        finish(&s);
        let short = s.latest().unwrap().network.resonances(0).iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
        assert!(short > 1.3 * long, "{short} vs {long}");
    }

    /// One solve, then a state's figures and its view without another.
    #[test]
    fn antenna_reconfig_one_solve_answers_for_every_state() {
        let mut s = studio("reconfig_parasitic");
        s.run_reconfig();
        finish(&s);
        let run = s.reconfig_run().expect("a solve");
        assert_eq!(run.basis.network.ports(), 5);
        assert!(run.basis.far_f.len() >= 5, "patterns across the band: {:?}", run.basis.far_f);
        let g = s.project.geometry().unwrap();
        assert!(run.applies_to(&g, &s.project.setup));
        // Parts and states don't call for a new solve; the metal does.
        s.project.reconfig.elements[0].device = Device::by_part("Ideal switch").unwrap();
        s.project.setup.far_freqs = vec![2.5e9];
        assert!(run.applies_to(&s.project.geometry().unwrap(), &s.project.setup));
        s.project.params.iter_mut().find(|p| p.name == "h").unwrap().expr = "0.3*lambda0".into();
        assert!(!run.applies_to(&s.project.geometry().unwrap(), &s.project.setup));
        s.project = Project::from_template("reconfig_parasitic", None).unwrap();
        s.project.setup.mom_samples = 15;

        s.view_state(1);
        finish(&s);
        let r = s.latest().unwrap();
        assert_eq!(s.project.reconfig.active, 1);
        assert!(r.label.contains("Beam +y"), "{}", r.label);
        let k = r.far_index(s.project.f0()).unwrap();
        let m = r.metrics(0, k).unwrap();
        assert!((m.peak_phi - 90.0).abs() < 15.0, "φ {}", m.peak_phi);
        assert!(r.currents.is_some());

        s.search_reconfig(5);
        finish(&s);
        let found = s.take_reconfig_found().unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|f| f.score.met), "{:?}", found.iter().map(|f| &f.score).collect::<Vec<_>>());
        let states = states_from_found(&s.project.reconfig, &found, &s.project.reconfig.targets);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].settings, vec![Setting::Off, Setting::On, Setting::On, Setting::On]);
    }

    /// The Studio's own sweep and optimiser take the active state, so a goal on S11 means
    /// the feed in that state, not port 1 with everything else matched.
    #[test]
    fn antenna_reconfig_the_optimizer_tunes_the_active_state() {
        let mut s = studio("reconfig_dipole");
        s.project.setup.far_freqs.clear();
        s.project.reconfig.active = 0;
        let f = 2.2e9;
        let goals = vec![(Goal::MatchBelow { port: 0, f_lo: f * 0.99, f_hi: f * 1.01, db: -10.0 }, 1.0)];
        let l = s.project.vars().unwrap()["L1"];
        s.run_optimize(vec![("L1".into(), l * 0.9, l * 1.25)], goals, Method::NelderMead, 24, 1);
        finish(&s);
        let o = s.opt().unwrap();
        assert!(o.best_value < 1.0, "penalty {} at {:?}", o.best_value, o.best_x);
        assert!(o.best_x[0] > l, "a lower band takes a longer first section: {} vs {l}", o.best_x[0]);
    }

    /// Tunes metal against what the states can reach, re-searching them at each step.
    #[test]
    fn antenna_reconfig_the_tuner_moves_metal_for_the_targets() {
        let mut s = studio("reconfig_dipole");
        s.project.setup.mom_samples = 13;
        s.project.reconfig.targets = vec![Target::band("Upper", 2.15e9, 2.25e9)];
        let l = s.project.vars().unwrap()["L1"];
        s.run_reconfig_optimize(vec![("L1".into(), l * 0.9, l * 1.3)], Method::NelderMead, 16, 1);
        finish(&s);
        let o = s.opt().unwrap();
        assert!(o.finished);
        assert!(o.best_value < 1.0, "miss {} at {:?}", o.best_value, o.best_x);
        assert!(o.best.as_ref().is_some_and(|r| r.network.ports() == 1), "kept as a state");
    }

    #[test]
    fn antenna_reconfig_the_tools_read_parts_states_and_targets() {
        let d = device_from(&json!({ "part": "SMV1405 varactor", "rs_ohm": 1.5, "cp_pf": 0.5 })).unwrap();
        let Model::Varactor { rs, cp, .. } = d.model else { panic!() };
        assert!((rs - 1.5).abs() < 1e-12 && (cp - 0.5e-12).abs() < 1e-18);
        assert!(d.source.contains("by hand"));
        assert!(device_from(&json!({ "part": "unobtainium" })).unwrap_err().contains("SMV1405"));

        let mut rc = Reconfig::default();
        rc.add_element(Element { name: "A".into(), port: "pa".into(), device: Device::by_part("Ideal switch").unwrap(), mount: mount_from(&json!({ "l_choke_nh": 47.0, "c_block_pf": 100.0 })) });
        rc.add_element(Element { name: "V".into(), port: "pv".into(), device: d, mount: Mount::default() });
        assert!((rc.elements[0].mount.l_choke - 47e-9).abs() < 1e-15);
        let s = state_from(&rc, 0, &json!({ "name": "x", "settings": { "A": "on", "V": 4.5 }, "f_ghz": 1.5 })).unwrap();
        assert_eq!(s.settings, vec![Setting::On, Setting::Volts(4.5)]);
        assert_eq!(s.f_op, Some(1.5e9));
        let by_order = state_from(&rc, 1, &json!({ "settings": ["off", "2 V"] })).unwrap();
        assert_eq!(by_order.settings, vec![Setting::Off, Setting::Volts(2.0)]);
        assert!(state_from(&rc, 0, &json!({ "settings": { "nope": "on" } })).unwrap_err().contains("nope"));
        assert!(state_from(&rc, 0, &json!({ "settings": ["on"] })).unwrap_err().contains("sets 1"));

        let t = target_from(&json!({ "name": "t", "f_lo_ghz": 2.4, "f_hi_ghz": 2.5, "limit_db": -6, "theta": 30, "phi": 90, "min_gain_dbi": 5, "pol": "RHCP" })).unwrap();
        assert_eq!((t.limit_db, t.beam, t.min_gain_dbi, t.pol), (-6.0, Some([30.0, 90.0]), Some(5.0), Some(PolGoal::Rhcp)));
        assert!(target_from(&json!({ "f_lo_ghz": 2.4, "f_hi_ghz": 2.5, "pol": "sideways" })).unwrap_err().contains("sideways"));
        assert!(target_from(&json!({ "name": "no band" })).is_err());
    }

    fn fdtd_dipole(load_z0: &str) -> Project {
        let mut p = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        p.setup.solver = SolverKind::Fdtd;
        p.setup.cells_per_lambda = 20.0;
        p.setup.points = 101;
        p.setup.end_db = 50.0;
        p.setup.field_planes = false;
        p.setup.far_step = 15.0;
        p.setup.far_freqs = vec![1e9];
        // A cut across the upper arm, where the load sits.
        p.extra_ports.push(ExtraPort { name: "load".into(), a: ["0".into(), "0".into(), "0.03".into()], b: ["0".into(), "0".into(), "0.033".into()], width: "w".into(), z0: load_z0.into() });
        p
    }

    /// FDTD has to close ports exactly as the method of moments does, or its states are wrong:
    /// a port closed with 20 Ω, against the same model with a real 20 Ω lumped element there,
    /// in a separate run. Complex far field included, which is what phase-referencing buys.
    /// `cargo test --bin SenseSim antenna_fdtd_closing_a_port -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn antenna_fdtd_closing_a_port_matches_a_real_lumped_element() {
        let c = run::JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() };
        let opts = RunOptions { keep: false, ..Default::default() };
        let (a, b) = (fdtd_dipole("50"), fdtd_dipole("20"));
        let basis = run::simulate_ports(&a, &c, &opts).unwrap_or_else(|e| panic!("{e:#}"));
        let direct = run::simulate_ports(&b, &c, &opts).unwrap_or_else(|e| panic!("{e:#}"));
        let g = a.geometry().unwrap();
        let names: Vec<String> = g.ports.iter().map(|x| x.name.clone()).collect();
        let device = Device { part: "R".into(), model: Model::Fixed { r: 20.0, l: 0.0, c: 0.0 }, v_limit: 0.0, i_limit: 0.0, p_limit: 0.0, source: String::new() };
        let mut rc = Reconfig::default();
        rc.add_element(Element { name: "R".into(), port: "load".into(), device, mount: Mount::default() });
        let solved = Solved::new(&basis, &names, &rc).unwrap();
        let settings = rc.settings_of(&rc.states[0]);
        let net = solved.network(&rc, &settings).unwrap();
        let mut worst: f64 = 0.0;
        for (m, d) in net.s.iter().zip(&direct.network.s) {
            worst = worst.max((m[0][0] - d[0][0]).norm());
        }
        let ff = solved.far(&rc, &settings, 0, 0).unwrap();
        let want = &direct.far[0][0];
        let (mut num, mut den) = (0.0, 0.0);
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                num += (ff.e_theta[ti][pi] - want.e_theta[ti][pi]).norm_sqr() + (ff.e_phi[ti][pi] - want.e_phi[ti][pi]).norm_sqr();
                den += want.e_theta[ti][pi].norm_sqr() + want.e_phi[ti][pi].norm_sqr();
            }
        }
        let rel = (num / den).sqrt();
        eprintln!("FDTD closing a port: worst |ΔS11| {worst:.4}, far-field difference {:.2} %, power: {:.4} vs {:.4}", rel * 100.0, ff.p_rad(), want.p_rad());
        assert!(worst < 0.02, "S11 differs by {worst}");
        assert!(rel < 0.03, "far field differs by {:.1} %", rel * 100.0);
    }
}
