//! The assistant's antenna engineering tools: requirements, judging a run, formulas,
//! and the Studio's sweep, mesh study, matching, yield and array analyses. The judgement
//! and the formulas are `antenna_core::assess` and `antenna_core::calc`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use antenna_core::array::{self, ArrayConfig, Taper};
use antenna_core::assess::{self, Context, Requirements};
use antenna_core::calc;
use antenna_core::matching;
use antenna_core::project::SimResult;
use bevy::prelude::*;
use crossbeam_channel::Sender;
use serde_json::{Value, json};

use super::Studio;
use crate::agent::llm::ToolDef;
use crate::agent::tools::{ToolJob, ToolOutput};

/// What the current antenna has to do, set by `antenna_spec`.
#[derive(Resource, Default)]
pub struct AntennaSpec(pub Option<Requirements>);

/// The tool the harness and `antenna_wait` poll. Not offered to the model.
pub const JOB_STATE: &str = "antenna_job_state";
pub const WAIT: &str = "antenna_wait";
const WAIT_DEFAULT_S: f64 = 120.0;
const WAIT_MAX_S: f64 = 600.0;

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let num = json!({ "type": "number" });
    let int = json!({ "type": "integer" });
    vec![
        def(
            "antenna_spec",
            "Set what the antenna has to do, before designing it. antenna_assess judges every run against this. Band in GHz; worst S11 in dB (default -10); optional gain in dBi (realized), total efficiency 0..1, axial ratio, front to back, and largest size in mm.",
            json!({ "type": "object", "properties": { "f_lo_ghz": num, "f_hi_ghz": num, "s11_db": num, "min_gain_dbi": num, "min_efficiency": num, "max_axial_ratio_db": num, "min_front_to_back_db": num, "max_size_mm": { "type": "array", "items": num, "minItems": 3, "maxItems": 3 } }, "required": ["f_lo_ghz", "f_hi_ghz"] }),
        ),
        def(
            "antenna_assess",
            "Judge the latest run against antenna_spec. Returns a verdict (not_trustworthy, not_met, met_mesh_unchecked, met_mesh_checked), each requirement with its measured value, why the numbers may be wrong, a diagnosis of what the input impedance, Q and pattern show, and what to do next. Call it after every run instead of judging S11 by eye.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "antenna_calc",
            &format!("Compute an antenna or RF formula instead of doing the arithmetic yourself. Use it for starting dimensions and conversions. Formulas: {}", calc::FORMULAS),
            json!({ "type": "object", "properties": { "formula": { "type": "string" }, "args": { "type": "object", "description": "The formula's inputs, SI units unless the name says otherwise" } }, "required": ["formula"] }),
        ),
        def(
            WAIT,
            "Wait for the running antenna job to finish instead of polling, then return its state. Read the results with the matching tool afterwards.",
            json!({ "type": "object", "properties": { "seconds": { "type": "number", "description": "Longest wait, default 120, at most 600" } } }),
        ),
        def(
            "antenna_sweep",
            "Sweep a variable of the current design, or a grid of two, in the background, to see what a dimension does. Give values, or from, to and steps, in SI units. Read it with antenna_sweep_results.",
            json!({ "type": "object", "properties": { "var": { "type": "string" }, "values": { "type": "array", "items": num }, "from": num, "to": num, "steps": int, "var2": { "type": "string" }, "values2": { "type": "array", "items": num }, "parallel": int }, "required": ["var"] }),
        ),
        def("antenna_sweep_results", "Results of the parametric sweep: for each point, the values, the resonance, S11 at the band centre and the peak gain, and which point is best.", json!({ "type": "object", "properties": {} })),
        def(
            "antenna_converge",
            "Run a mesh convergence study of the current design in the background: the same model at finer and finer meshes until the results stop moving. Do this before quoting final numbers. Read it with antenna_converge_results.",
            json!({ "type": "object", "properties": { "limit": { "type": "number", "description": "Error left in S to stop at, default 0.02" }, "max_passes": { "type": "integer", "description": "3 to 10, default 4" } } }),
        ),
        def("antenna_converge_results", "Each pass of the mesh study: density, size, resonance, peak gain, the change against the pass before and the error left, and whether it converged.", json!({ "type": "object", "properties": {} })),
        def(
            "antenna_match",
            "Design a matching network from the latest run: L, pi, T, stub and band-optimised networks, with their values, S11 at the frequency and worst over the band, the same with standard E24 parts, and the power lost in the parts. It doesn't solve anything new and doesn't change the design. Finite-Q parts are assumed.",
            json!({ "type": "object", "properties": { "f_ghz": num, "q_l": { "type": "number", "description": "Inductor Q, default 40" }, "q_c": { "type": "number", "description": "Capacitor Q, default 200" }, "z0": num } }),
        ),
        def(
            "antenna_yield",
            "Monte Carlo manufacturing yield of the current design in the background: how many built units keep S11 within the limit over the band, with tolerances on variables. Tolerances are 1 sigma: rel is relative, abs is in metres. Read it with antenna_yield_results.",
            json!({ "type": "object", "properties": { "vars": { "type": "array", "description": "[{name, rel, abs}]" }, "samples": int, "f_lo_ghz": num, "f_hi_ghz": num, "limit_db": num, "parallel": int }, "required": ["vars"] }),
        ),
        def("antenna_yield_results", "Yield of the study, with its 95 % interval, whether its lower bound clears 90 %, and which variable matters most.", json!({ "type": "object", "properties": {} })),
        def(
            "antenna_array",
            "Array factor analysis at a frequency: directivity, beam widths, side lobes, grating lobes and how far it can scan. With a run in hand also the array's gain from that element's pattern. Coupling between elements isn't included. Spacing in metres or in wavelengths.",
            json!({ "type": "object", "properties": { "f_ghz": num, "nx": int, "ny": int, "dx_m": num, "dy_m": num, "dx_lambda": num, "dy_lambda": num, "taper": { "type": "string", "enum": ["uniform", "binomial", "chebyshev", "taylor", "hamming", "blackman", "cosine"] }, "sll_db": { "type": "number", "description": "Side lobes for chebyshev and taylor, e.g. -25" }, "steer_theta": num, "steer_phi": num, "phase_bits": int }, "required": ["f_ghz", "nx"] }),
        ),
        def(
            "antenna_save_pattern",
            "Save the latest run's pattern to the antenna library, so devices in the scene can use it with site_set_antenna. apply_to_all also uses it on all infrastructure of its technology.",
            json!({ "type": "object", "properties": { "name": { "type": "string" }, "apply_to_all": { "type": "boolean" } } }),
        ),
    ]
}

fn text(v: Value) -> ToolOutput {
    ToolOutput::text(v.to_string())
}

fn r3(v: f64) -> Value {
    if v.is_finite() { json!((v * 1000.0).round() / 1000.0) } else { Value::Null }
}

fn job_json(st: &Studio) -> Value {
    match st.status() {
        super::Status::Idle => json!({ "state": "idle" }),
        super::Status::Busy(m, p) => json!({ "state": "running", "message": m, "progress": p }),
        super::Status::Failed(e) => json!({ "state": "failed", "error": e }),
    }
}

/// Blocks a worker thread until the job ends. Called from `tools::call`, so the app's
/// update loop keeps running and the wait counts as one tool call.
pub fn wait(tx: &Sender<ToolJob>, input: &Value, cancel: Option<&AtomicBool>) -> ToolOutput {
    let limit = input["seconds"].as_f64().unwrap_or(WAIT_DEFAULT_S).clamp(1.0, WAIT_MAX_S);
    let started = Instant::now();
    loop {
        let out = crate::agent::tools::call(tx, JOB_STATE, json!({}), false, cancel);
        let running = out.summary().contains("\"running\"");
        if out.is_error || !running {
            return out;
        }
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return ToolOutput::err("cancelled by the user");
        }
        if started.elapsed().as_secs_f64() >= limit {
            return text(json!({ "state": "running", "note": format!("Still running after {limit:.0} s. Call antenna_wait again, or antenna_cancel.") }));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The band to judge: the spec's, or a narrow one at the design frequency.
fn band_of(world: &World) -> (f64, f64) {
    if let Some(r) = world.get_resource::<AntennaSpec>().and_then(|s| s.0.as_ref()) {
        return (r.f_lo, r.f_hi);
    }
    let f0 = world.resource::<Studio>().project.f0();
    (f0 * 0.99, f0 * 1.01)
}

/// Whether the design changed after this result. Runs from the history have their
/// project; others (an optimiser's best) are compared by their variables.
fn is_stale(st: &Studio, r: &Arc<SimResult>) -> bool {
    // The Solve page rewrites the far-field list as it shows it, and which frequencies
    // have a pattern is judged on its own, so that list doesn't make a run stale.
    let design = |p: &antenna_core::project::Project| {
        let mut p = p.clone();
        p.setup.far_freqs.clear();
        p.name.clear();
        p.to_json().ok()
    };
    if let Some((p, _)) = st.history().iter().rev().find(|(_, h)| Arc::ptr_eq(h, r)) {
        return design(p) != design(&st.project);
    }
    st.project.vars().is_ok_and(|v| r.vars.iter().any(|(k, x)| v.get(k).is_some_and(|y| (*y - *x).abs() > 1e-12)))
}

/// The state of a mesh study, if the last one was of the variables of this result.
fn mesh_state(st: &Studio, r: &SimResult) -> Option<bool> {
    let c = st.converge()?;
    if !c.finished {
        return None;
    }
    let last = c.steps.last()?;
    (last.result.vars == r.vars).then_some(c.converged)
}

fn spec_tool(world: &mut World, i: &Value) -> ToolOutput {
    let (Some(lo), Some(hi)) = (i["f_lo_ghz"].as_f64(), i["f_hi_ghz"].as_f64()) else {
        return ToolOutput::err("give f_lo_ghz and f_hi_ghz");
    };
    if !(lo > 0.0 && hi >= lo) {
        return ToolOutput::err("the band must have f_hi_ghz at or above f_lo_ghz, both above 0");
    }
    let size = i["max_size_mm"].as_array().and_then(|a| {
        let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
        (v.len() == 3).then(|| [v[0] * 1e-3, v[1] * 1e-3, v[2] * 1e-3])
    });
    let req = Requirements {
        f_lo: lo * 1e9,
        f_hi: hi * 1e9,
        s11_db: i["s11_db"].as_f64().unwrap_or(-10.0),
        min_gain_dbi: i["min_gain_dbi"].as_f64(),
        min_efficiency: i["min_efficiency"].as_f64(),
        max_axial_ratio_db: i["max_axial_ratio_db"].as_f64(),
        min_front_to_back_db: i["min_front_to_back_db"].as_f64(),
        max_size_m: size,
    };
    let fc = req.center();
    let setup = world.resource::<Studio>().project.setup.clone();
    let mut note = Vec::new();
    if setup.f_lo > req.f_lo || setup.f_hi < req.f_hi {
        note.push(format!("The current sweep, {:.3} to {:.3} GHz, doesn't cover this band: set it with antenna_set.", setup.f_lo / 1e9, setup.f_hi / 1e9));
    }
    if !setup.far_freqs.iter().any(|f| (f - fc).abs() <= (req.f_hi - req.f_lo).max(fc * 0.01)) {
        note.push(format!("No far field is set near the band centre: set far_ghz to {:.4} with antenna_set.", fc / 1e9));
    }
    let frac = (req.f_hi - req.f_lo) / fc * 100.0;
    world.insert_resource(AntennaSpec(Some(req)));
    text(json!({ "ok": true, "fractional_bandwidth_pct": r3(frac), "notes": note }))
}

fn assess_tool(world: &mut World) -> ToolOutput {
    let spec = world.get_resource::<AntennaSpec>().and_then(|s| s.0.clone());
    let Some(req) = spec else {
        return ToolOutput::err("Set the requirements first with antenna_spec.");
    };
    let st = world.resource::<Studio>();
    if st.busy() {
        return text(json!({ "job": job_json(st), "note": "A job is running: use antenna_wait, then assess." }));
    }
    let Some(r) = st.latest() else {
        return ToolOutput::err("Nothing has been simulated yet: call antenna_run.");
    };
    let size_m = st.project.geometry().ok().map(|g| {
        let (lo, hi) = g.bounds();
        [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
    });
    let ctx = Context { stale: is_stale(st, &r), mesh_converged: mesh_state(st, &r), size_m };
    let a = assess::assess(&r, &req, ctx);
    ToolOutput::text(serde_json::to_string(&a).unwrap_or_default())
}

fn values_of(i: &Value, values: &str) -> Vec<f64> {
    if let Some(a) = i[values].as_array() {
        return a.iter().filter_map(Value::as_f64).collect();
    }
    let (Some(a), Some(b)) = (i["from"].as_f64(), i["to"].as_f64()) else { return Vec::new() };
    let n = i["steps"].as_u64().unwrap_or(7).clamp(2, 41) as usize;
    (0..n).map(|k| a + (b - a) * k as f64 / (n - 1) as f64).collect()
}

fn sweep_results(world: &World) -> ToolOutput {
    let (lo, hi) = band_of(world);
    let fc = (lo + hi) / 2.0;
    let st = world.resource::<Studio>();
    let job = job_json(st);
    let Some(s) = st.sweep() else { return text(json!({ "job": job, "sweep": Value::Null })) };
    let score = |r: &SimResult| antenna_core::project::band_worst(&r.network, 0, 0, lo, hi).unwrap_or(f64::INFINITY);
    let points: Vec<Value> = s
        .points
        .iter()
        .map(|p| {
            let net = &p.result.network;
            let res = net.resonances(0).into_iter().min_by(|a, b| (a.0 - fc).abs().total_cmp(&(b.0 - fc).abs()));
            let gain = p.result.far_index(fc).and_then(|k| p.result.metrics(0, k)).map(|m| r3(m.peak_realized_dbi));
            let values: serde_json::Map<String, Value> = p.values.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            json!({ "values": values, "resonance_ghz": res.map(|x| r3(x.0 / 1e9)), "resonance_db": res.map(|x| r3(x.1)), "worst_s11_in_band_db": r3(score(&p.result)), "gain_dbi": gain })
        })
        .collect();
    let best = s.points.iter().min_by(|a, b| score(&a.result).total_cmp(&score(&b.result))).map(|p| p.values.clone());
    text(json!({ "job": job, "done": s.points.len(), "total": s.total, "points": points, "best_for_band": best }))
}

fn converge_results(world: &World) -> ToolOutput {
    let st = world.resource::<Studio>();
    let job = job_json(st);
    let Some(c) = st.converge() else { return text(json!({ "job": job, "study": Value::Null })) };
    let passes: Vec<Value> = c.steps.iter().map(|p| json!({ "density": p.density, "size": p.size, "seconds": r3(p.seconds), "resonance_ghz": r3(p.f_res / 1e9), "peak_gain_dbi": r3(p.peak_dbi), "delta_s": r3(p.delta_s), "error_left": r3(p.remaining), "gain_change_db": r3(p.gain_change_db) })).collect();
    text(json!({ "job": job, "finished": c.finished, "converged": c.converged, "limit": c.limit, "note": c.note, "passes": passes }))
}

fn yield_results(world: &World) -> ToolOutput {
    let st = world.resource::<Studio>();
    let job = job_json(st);
    let Some(y) = st.yield_state() else { return text(json!({ "job": job, "yield": Value::Null })) };
    let Some(o) = &y.outcome else { return text(json!({ "job": job, "done": y.done, "samples": y.samples })) };
    let sens: Vec<Value> = y.names.iter().zip(&o.sensitivities).map(|(n, s)| json!({ "name": n, "sensitivity": r3(*s) })).collect();
    text(json!({
        "job": job,
        "yield": r3(o.fraction),
        "ci95": [r3(o.ci95.0), r3(o.ci95.1)],
        "meets_90_pct_target": o.meets_target,
        "limit_db": y.limit_db,
        "mean_worst_s11_db": r3(o.mean),
        "std_db": r3(o.std),
        "samples": o.samples,
        "failed_to_solve": o.failed,
        "first_failure": o.first_failure,
        "sensitivity": sens,
    }))
}

/// Parts with finite Q, as they'd be built.
fn with_q(mut n: matching::Network, q_l: f64, q_c: f64) -> matching::Network {
    for e in &mut n.elements {
        e.q = match e.kind {
            matching::Kind::SeriesL | matching::Kind::ShuntL => Some(q_l),
            matching::Kind::SeriesC | matching::Kind::ShuntC => Some(q_c),
            _ => None,
        };
    }
    n
}

fn element_json(e: &matching::Element) -> Value {
    use matching::Kind::*;
    match e.kind {
        SeriesL => json!({ "part": "series L", "nH": r3(e.value * 1e9) }),
        ShuntL => json!({ "part": "shunt L", "nH": r3(e.value * 1e9) }),
        SeriesC => json!({ "part": "series C", "pF": r3(e.value * 1e12) }),
        ShuntC => json!({ "part": "shunt C", "pF": r3(e.value * 1e12) }),
        Line { z0, deg, .. } => json!({ "part": "line", "z0": r3(z0), "deg": r3(deg) }),
        Stub { z0, deg, shorted, .. } => json!({ "part": if shorted { "shorted stub" } else { "open stub" }, "z0": r3(z0), "deg": r3(deg) }),
    }
}

fn match_tool(world: &World, i: &Value) -> ToolOutput {
    let (lo, hi) = band_of(world);
    let st = world.resource::<Studio>();
    let Some(r) = st.latest() else { return ToolOutput::err("Nothing has been simulated yet: call antenna_run.") };
    let net = &r.network;
    if net.f.is_empty() || net.ports() == 0 {
        return ToolOutput::err("The latest run has no network data.");
    }
    let f = i["f_ghz"].as_f64().map_or((lo + hi) / 2.0, |v| v * 1e9);
    let z0 = i["z0"].as_f64().unwrap_or_else(|| net.z0.first().copied().unwrap_or(50.0));
    let (q_l, q_c) = (i["q_l"].as_f64().unwrap_or(40.0), i["q_c"].as_f64().unwrap_or(200.0));
    let zin = net.zin(0);
    let k = net.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map_or(0, |x| x.0);
    let zl = zin[k];
    if zl.re <= 0.0 {
        return ToolOutput::err(format!("The input resistance at {:.4} GHz is {:.2} ohm: fix the result before matching it.", f / 1e9, zl.re));
    }
    let mut cands: Vec<matching::Network> = matching::best_for_band(&net.f, &zin, z0, lo, hi, Some(q_l), Some(q_c)).into_iter().map(|x| x.0).collect();
    cands.extend(matching::synth_l(zl, z0, f));
    let q_design = ((zl.re / z0).max(z0 / zl.re.max(1e-3)) - 1.0).max(0.0).sqrt() + 2.0;
    cands.extend(matching::synth_pi(zl, z0, f, q_design));
    cands.extend(matching::synth_t(zl, z0, f, q_design));
    cands.extend(matching::synth_stub(zl, z0, f));
    cands.extend(matching::synth_quarter_wave(zl, z0, f));
    if cands.is_empty() {
        return ToolOutput::err("No matching network was found for this load.");
    }
    let band: Vec<(f64, antenna_core::C64)> = net.f.iter().copied().zip(zin.iter().copied()).filter(|(x, _)| (lo..=hi).contains(x)).collect();
    let worst = |n: &matching::Network| band.iter().map(|(x, z)| antenna_core::db20(matching::reflection(n, *z, z0, *x).norm())).fold(f64::NEG_INFINITY, f64::max);
    let mut scored: Vec<(matching::Network, f64)> = cands.into_iter().map(|n| with_q(n, q_l, q_c)).map(|n| (worst(&n), n)).map(|(w, n)| (n, w)).collect();
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    let top: Vec<Value> = scored
        .iter()
        .take(4)
        .map(|(n, w)| {
            let parts = matching::snapped(n, matching::Series::E24);
            let (s_ideal, power) = matching::as_built(n, zl, z0, f);
            let (s_parts, power_parts) = matching::as_built(&parts, zl, z0, f);
            json!({
                "name": n.name,
                "elements": n.elements.iter().map(element_json).collect::<Vec<_>>(),
                "s11_db_at_f": r3(s_ideal),
                "worst_s11_db_in_band": r3(*w),
                "power_lost_db": r3(-power),
                "with_e24_parts": { "elements": parts.elements.iter().map(element_json).collect::<Vec<_>>(), "s11_db_at_f": r3(s_parts), "worst_s11_db_in_band": r3(worst(&parts)), "power_lost_db": r3(-power_parts) },
            })
        })
        .collect();
    let q = net.q_factor(0).get(k).copied().filter(|q| q.is_finite() && *q > 0.0);
    let limit = q.map(|q| matching::bode_fano_bandwidth(q, f, 10f64.powf(-10.0 / 20.0)));
    text(json!({
        "load_ohm": { "re": r3(zl.re), "im": r3(zl.im) },
        "f_ghz": r3(f / 1e9),
        "band_ghz": [r3(lo / 1e9), r3(hi / 1e9)],
        "assumed_q": { "inductor": q_l, "capacitor": q_c },
        "antenna_q": q.map(r3),
        "bode_fano_limit_at_10db_mhz": limit.map(|v| r3(v / 1e6)),
        "networks": top,
        "note": "Network values assume the simulated load. Pads, traces and part tolerance move a real board, so plan to tune it with a VNA. The design itself isn't changed.",
    }))
}

fn array_tool(world: &World, i: &Value) -> ToolOutput {
    let Some(f) = i["f_ghz"].as_f64().map(|v| v * 1e9).filter(|f| *f > 0.0) else { return ToolOutput::err("give f_ghz") };
    let lambda = antenna_core::project::wavelength(f);
    let nx = i["nx"].as_u64().unwrap_or(1).clamp(1, 256) as usize;
    let ny = i["ny"].as_u64().unwrap_or(1).clamp(1, 256) as usize;
    let spacing = |m: &str, l: &str| i[m].as_f64().or(i[l].as_f64().map(|v| v * lambda)).unwrap_or(lambda / 2.0);
    let sll = i["sll_db"].as_f64().unwrap_or(-25.0).abs();
    let taper = match i["taper"].as_str() {
        Some("binomial") => Taper::Binomial,
        Some("chebyshev") => Taper::DolphChebyshev { sll_db: sll },
        Some("taylor") => Taper::Taylor { sll_db: sll, nbar: 4 },
        Some("hamming") => Taper::Hamming,
        Some("blackman") => Taper::Blackman,
        Some("cosine") => Taper::Cosine { power: 1.0 },
        _ => Taper::Uniform,
    };
    let c = ArrayConfig {
        nx,
        ny,
        dx: spacing("dx_m", "dx_lambda"),
        dy: spacing("dy_m", "dy_lambda"),
        taper_x: taper,
        taper_y: taper,
        steer_theta: i["steer_theta"].as_f64().unwrap_or(0.0),
        steer_phi: i["steer_phi"].as_f64().unwrap_or(0.0),
        phase_bits: i["phase_bits"].as_u64().map(|b| b.clamp(1, 12) as u8),
        ..Default::default()
    };
    let s = array::summarize(&c, f);
    let st = world.resource::<Studio>();
    let element = st.latest().and_then(|r| r.far_index(f).and_then(|k| r.far.first().and_then(|p| p.get(k).cloned())));
    let gain = element.map(|e| {
        let g = array::gain(&e, &c);
        json!({ "element_from": "the latest run", "directivity_dbi": r3(g.directivity_dbi), "gain_dbi": r3(g.gain_dbi), "realized_dbi": r3(g.realized_dbi), "peak_theta": r3(g.peak_theta), "peak_phi": r3(g.peak_phi), "estimated": g.estimated })
    });
    text(json!({
        "isotropic_directivity_dbi": r3(s.directivity_dbi),
        "taper_loss_db": r3(s.taper_loss_db),
        "hpbw_x_deg": r3(s.hpbw_x_deg),
        "hpbw_y_deg": r3(s.hpbw_y_deg),
        "side_lobes_db": r3(s.sll_db),
        "grating_lobe": s.grating_lobe.map(|g| json!({ "theta": r3(g.0), "phi": r3(g.1) })),
        "max_scan_no_grating_deg": r3(s.max_scan_no_grating_deg),
        "with_element": gain,
        "note": "Array factor only: coupling between elements, feed network loss and scan blindness aren't included. Check them by solving the real array.",
    }))
}

fn save_pattern(world: &mut World, i: &Value) -> ToolOutput {
    let st = world.resource::<Studio>();
    let Some(r) = st.latest() else { return ToolOutput::err("Nothing has been simulated yet: call antenna_run.") };
    let f0 = st.project.f0();
    let forward = st.project.geometry().is_ok_and(|g| g.axis_forward);
    let name = i["name"].as_str().map_or_else(|| format!("{} {:.2} GHz", st.project.name, f0 / 1e9), str::to_string);
    let Some(a) = super::ui::analysis::planner_antenna(&r, name.clone(), forward, f0) else {
        return ToolOutput::err("The latest run has no far field at the design frequency, or a band the planner doesn't use.");
    };
    if let Err(e) = world.resource_mut::<crate::rtls::antenna::AntennaLibrary>().save(a.clone()) {
        return ToolOutput::err(format!("Could not save the pattern: {e}"));
    }
    let applied = i["apply_to_all"].as_bool().unwrap_or(false);
    if applied {
        world.resource_mut::<crate::rtls::planning::AppliedAntenna>().0 = Some(a.clone());
    }
    text(json!({ "ok": true, "name": name, "technology": a.tech.label(), "applied_to_all_infrastructure": applied, "next": "Give it to one device with site_set_antenna, then re-run rtls_coverage." }))
}

/// Handles this module's tools; `None` for anything else.
pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        JOB_STATE => text(job_json(world.resource::<Studio>())),
        "antenna_spec" => spec_tool(world, i),
        "antenna_assess" => assess_tool(world),
        "antenna_calc" => match calc::run(i["formula"].as_str().unwrap_or(""), &i["args"]) {
            Ok(v) => text(v),
            Err(e) => ToolOutput::err(e),
        },
        "antenna_sweep" => {
            let Some(var) = i["var"].as_str().map(str::to_string) else { return Some(ToolOutput::err("give var")) };
            let values = values_of(i, "values");
            if values.is_empty() {
                return Some(ToolOutput::err("give values, or from, to and steps"));
            }
            let var2 = i["var2"].as_str().map(|v| (v.to_string(), values_of(i, "values2")));
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            let known = st.project.vars().ok();
            for (v, _) in std::iter::once(&(var.clone(), Vec::new())).chain(var2.iter()) {
                if !known.as_ref().is_some_and(|k| k.get(v).is_some()) {
                    return Some(ToolOutput::err(format!("no variable '{v}': see antenna_get")));
                }
            }
            let n = values.len() * var2.as_ref().map_or(1, |v| v.1.len().max(1));
            st.run_sweep((var, values), var2, i["parallel"].as_u64().unwrap_or(1).clamp(1, 8) as usize);
            text(json!({ "started": true, "points": n, "next": "antenna_wait, then antenna_sweep_results" }))
        }
        "antenna_sweep_results" => sweep_results(world),
        "antenna_converge" => {
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            st.run_converge(i["limit"].as_f64().unwrap_or(0.02).clamp(0.001, 0.3), i["max_passes"].as_u64().unwrap_or(4).clamp(3, 10) as usize);
            text(json!({ "started": true, "next": "antenna_wait, then antenna_converge_results" }))
        }
        "antenna_converge_results" => converge_results(world),
        "antenna_match" => match_tool(world, i),
        "antenna_yield" => {
            let vars: Vec<(String, f64, f64)> = i["vars"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| Some((v["name"].as_str()?.to_string(), v["rel"].as_f64().unwrap_or(0.0), v["abs"].as_f64().unwrap_or(0.0)))).collect());
            if vars.is_empty() {
                return Some(ToolOutput::err("give vars: [{name, rel, abs}]"));
            }
            let (lo, hi) = band_of(world);
            let (lo, hi) = (i["f_lo_ghz"].as_f64().map_or(lo, |v| v * 1e9), i["f_hi_ghz"].as_f64().map_or(hi, |v| v * 1e9));
            let limit = i["limit_db"].as_f64().or(world.get_resource::<AntennaSpec>().and_then(|s| s.0.as_ref()).map(|r| r.s11_db)).unwrap_or(-10.0);
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            st.run_yield(vars, i["samples"].as_u64().unwrap_or(30).clamp(5, 500) as usize, lo, hi, limit, i["parallel"].as_u64().unwrap_or(1).clamp(1, 8) as usize);
            text(json!({ "started": true, "next": "antenna_wait, then antenna_yield_results" }))
        }
        "antenna_yield_results" => yield_results(world),
        "antenna_array" => array_tool(world, i),
        "antenna_save_pattern" => save_pattern(world, i),
        _ => return None,
    })
}
