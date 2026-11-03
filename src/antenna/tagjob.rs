//! Designing RFID tags from what they have to do: the background search, the retune, and the tools
//! the assistant works them with. The physics and the search are in `antenna_core::tag_design`.

use antenna_core::freeform;
use antenna_core::stack::{self, Layer, Stack};
use antenna_core::tag_design::{self, Candidate, Control, Effort, FAMILIES, Progress, Retune, TagGoal, TagOption};
use antenna_core::tag_tune::{self, Knobs, Reading, Tune};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::Studio;
use super::assist::AntennaSpec;
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

/// What the search found, kept so the page and the tools can read it while it runs.
#[derive(Clone, Default)]
pub struct TagRun {
    pub goal: Option<TagGoal>,
    pub options: Vec<TagOption>,
    pub retune: Option<Retune>,
    /// The last tune of the loop and the line.
    pub tune: Option<Tune>,
    /// The last reading of those two with nothing changed.
    pub reading: Option<Reading>,
    pub progress: Option<Progress>,
    pub finished: bool,
}

/// How many solves run at once. The machine's left room for anything else it's doing.
fn parallel() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 8).clamp(2, 12))
}

impl Studio {
    pub fn tag_run(&self) -> Option<TagRun> {
        self.shared.lock().unwrap().tag.clone()
    }

    /// Searches every structure asked for, and anything the designer added, against the goal. With
    /// `evolve`, also lays out metal cell by cell on that grid.
    pub fn run_tag_design(&mut self, goal: TagGoal, templates: Vec<String>, extra: Vec<Candidate>, effort: Effort, evolve: Option<(usize, usize, usize)>) {
        self.spawn(move |ctx, shared| {
            shared.lock().unwrap().tag = Some(TagRun { goal: Some(goal.clone()), ..Default::default() });
            let report = {
                let shared = shared.clone();
                let ctx = ctx.clone();
                move |p: Progress| {
                    ctx.busy(format!("{}: {} of {} solves, best {:.1} m", p.message, p.done, p.total, p.best_range_m), p.done as f32 / p.total.max(1) as f32);
                    if let Some(t) = shared.lock().unwrap().tag.as_mut() {
                        t.progress = Some(p);
                    }
                }
            };
            let ctl = Control { cancel: &ctx.cancel, report: &report, parallel: parallel() };
            let mut options = tag_design::explore(&goal, &templates, extra, effort, &ctl)?;
            if let Some((nx, ny, solves)) = evolve {
                let o = tag_design::evolve(&goal, nx, ny, solves, &ctl)?;
                options.push(o);
                options.sort_by(|a, b| b.score.worst_range_m.total_cmp(&a.score.worst_range_m));
                for (k, o) in options.iter_mut().enumerate() {
                    o.id = format!("option_{}", k + 1);
                    o.stands_out.clear();
                }
                tag_design::mark_out(&mut options, &goal);
            }
            if ctx.cancelled() {
                anyhow::bail!("cancelled");
            }
            if let Some(t) = shared.lock().unwrap().tag.as_mut() {
                t.options = options;
                t.finished = true;
            }
            Ok(())
        });
    }

    /// Tells what new surroundings do to the current design, and tunes it back by small changes.
    pub fn run_tag_retune(&mut self, goal: TagGoal, reach: f64, solves: usize) {
        let project = self.project.clone();
        self.spawn(move |ctx, shared| {
            let mut run = shared.lock().unwrap().tag.clone().unwrap_or_default();
            run.goal = Some(goal.clone());
            run.retune = None;
            run.finished = false;
            shared.lock().unwrap().tag = Some(run);
            let report = {
                let ctx = ctx.clone();
                move |p: Progress| ctx.busy(format!("{}: {} of {} solves", p.message, p.done, p.total), p.done as f32 / p.total.max(1) as f32)
            };
            let ctl = Control { cancel: &ctx.cancel, report: &report, parallel: parallel() };
            let r = tag_design::retune(&project, &goal, &project.stack, reach, solves, &ctl)?;
            if ctx.cancelled() {
                anyhow::bail!("cancelled");
            }
            if let Some(t) = shared.lock().unwrap().tag.as_mut() {
                t.retune = Some(r);
                t.finished = true;
            }
            Ok(())
        });
    }

    /// Tunes the current design's loop and line for a frequency, or with `only_read` just reports both.
    pub fn run_tag_tune(&mut self, goal: TagGoal, f0: f64, knobs: Knobs, only_read: bool) {
        let project = self.project.clone();
        self.spawn(move |ctx, shared| {
            let mut run = shared.lock().unwrap().tag.clone().unwrap_or_default();
            run.goal = Some(goal.clone());
            run.tune = None;
            run.reading = None;
            run.finished = false;
            shared.lock().unwrap().tag = Some(run);
            let report = {
                let ctx = ctx.clone();
                move |p: Progress| ctx.busy(format!("{}: {} solves", p.message, p.done), p.done as f32 / p.total.max(1) as f32)
            };
            let ctl = Control { cancel: &ctx.cancel, report: &report, parallel: parallel() };
            let (tune, reading) = if only_read {
                (None, Some(tag_tune::read(&tag_design::fitted(&project, &goal), &goal, f0, &knobs, &ctl)?))
            } else {
                (Some(tag_tune::tune(&project, &goal, f0, &knobs, &ctl)?), None)
            };
            if ctx.cancelled() {
                anyhow::bail!("cancelled");
            }
            if let Some(t) = shared.lock().unwrap().tag.as_mut() {
                t.tune = tune;
                t.reading = reading;
                t.finished = true;
            }
            Ok(())
        });
    }

    /// A fresh tag of this structure to tune, sized for the frequency.
    pub fn start_tag_from(&mut self, template: &str, f0: f64) {
        match antenna_core::project::Project::from_template(template, Some(&format!("{}MHz", f0 / 1e6))) {
            Ok(p) => {
                self.project = p;
                self.clear_latest();
            }
            Err(e) => self.ui.message = Some((format!("{e:#}"), false)),
        }
    }

    /// Makes an option the current design, and the goal it was found for the requirements.
    pub fn open_tag_option(&mut self, project: antenna_core::project::Project) {
        self.project = project;
        self.clear_latest();
        self.ui.page = super::ui::Page::Variables;
    }
}

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let layer = json!({ "type": "object", "properties": { "name": { "type": "string" }, "material": { "type": "string" }, "um": { "type": "number", "description": "Thickness, micrometres" } }, "required": ["material", "um"] });
    let layers = json!({ "type": "array", "items": layer, "description": "Nearest the metal first" });
    let surroundings = |extra: Value| {
        let mut p = json!({ "preset": { "type": "string", "description": "A named build-up: see antenna_stack with no arguments" }, "above": layers, "below": layers, "metal_behind": { "type": "boolean", "description": "A metal surface behind the last layer below" } });
        p.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        p
    };
    vec![
        def(
            "antenna_stack",
            "What the antenna is in: paper over it, its film, glue, a carton, a bottle, metal behind a spacer. Layers cost no unknowns however thin, and they move the design, so set them before tuning. Metal has to be flat in one plane of z; otherwise a run goes to FDTD, which builds them to size. With no arguments, lists the presets and materials. clear removes them.",
            json!({ "type": "object", "properties": surroundings(json!({ "clear": { "type": "boolean" } })) }),
        ),
        def(
            "antenna_draw",
            "Draw your own antenna, any shape, instead of adapting a template. Every coordinate is an expression over variables with units, like \"L/2\" or \"12mm\", so antenna_sweep, antenna_optimize and antenna_tag_design can move it by name. fresh starts blank, keeping band, layers and metal. parts, each {kind, name, axis (default z), at (default 0), material, mirror (x, y or xy)}: strip {path, w}; rect {a, b}; polygon {pts}; plate {outline, holes}; disc {c, r}; arc {c, r, from_deg, to_deg, w}; meander {start, step, height, n, w}; box {a, b} and via {a, b, r} and wire {pts, r} in [x, y, z], which need FDTD. Coplanar metal that overlaps merges. feeds: {a, b, width, at}, across a gap, along x or y. Nothing is kept if it doesn't build.",
            json!({ "type": "object", "properties": { "fresh": { "type": "boolean" }, "name": { "type": "string" }, "metal": { "type": "string" }, "vars": { "type": "object", "description": "Name to expression, with units" }, "parts": { "type": "array", "items": { "type": "object" } }, "feeds": { "type": "array", "items": { "type": "object" } } } }),
        ),
        def(
            "antenna_tag_design",
            "Design an RFID tag from scratch, in the background: searches many different structures against the chip, band, footprint and surroundings, judged by read range, and gives several real options to choose from. Use this for any tag request. Read it with antenna_wait then antenna_tag_options. vary adds the current drawing or template as one more candidate: [{name, min, max}] in SI, the way to test your own idea against the known ones. evolve also lays out metal cell by cell with no assumed shape: {nx, ny, solves}. effort: quick (about 60 solves a structure), normal (240), deep (900), exhaustive (3000); each solve takes under a second.",
            json!({ "type": "object", "properties": surroundings(json!({
                "f_lo_ghz": { "type": "number" }, "f_hi_ghz": { "type": "number" },
                "chip": { "type": "string", "description": "A chip id from antenna_references" },
                "chip_z": { "type": "array", "items": { "type": "number" }, "description": "[R, X] series at 915 MHz, X negative, if it's not in the table" },
                "read_dbm": { "type": "number", "description": "Chip sensitivity, if not from the table" },
                "eirp_dbm": { "type": "number", "description": "Reader power with antenna gain, 36 by default" },
                "max_mm": { "type": "array", "items": { "type": "number" }, "description": "Footprint [x, y]" },
                "min_feature_um": { "type": "number", "description": "Thinnest trace and gap the process makes, 200 by default" },
                "metal": { "type": "string", "description": "Aluminium by default" },
                "structures": { "type": "array", "items": { "type": "string" }, "description": "Limit to these: see antenna_tag_options" },
                "vary": { "type": "array", "items": { "type": "object" } },
                "only_vary": { "type": "boolean", "description": "Search your own candidate alone, when the others have already been seen" },
                "title": { "type": "string", "description": "What to call your own candidate" },
                "idea": { "type": "string", "description": "What's different about it" },
                "evolve": { "type": "object" },
                "effort": { "type": "string", "enum": ["quick", "normal", "deep", "exhaustive"] }
            })), "required": ["f_lo_ghz", "f_hi_ghz"] }),
        ),
        def(
            "antenna_tag_options",
            "The options antenna_tag_design found, best first: each with its structure, why it's different, range, power into the chip, gain, size, the band it really covers, what it does if its surroundings change, and what it stands out for. Also the structures there are, and the last retune.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "antenna_tag_open",
            "Make one of antenna_tag_design's options the current design, with its surroundings and requirements, to run, assess, sweep or draw on.",
            json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
        ),
        def(
            "antenna_tag_retune",
            "What new surroundings do to the current tag, such as putting it under paper, and how far small changes to its dimensions get it back, as it's the same structure afterwards. Works in the background: antenna_wait, then antenna_tag_options. apply makes the retuned design current once it's done, with no surroundings given.",
            json!({ "type": "object", "properties": surroundings(json!({ "reach": { "type": "number", "description": "Largest change to any dimension, as a fraction: 0.15 by default" }, "solves": { "type": "integer" }, "apply": { "type": "boolean" } })) }),
        ),
        def(
            "antenna_tag_tune",
            "Put the current tag on an exact frequency, the way a designer does by hand, and report the two metrics that decide a tag. Metric 1, the loop: its length and width match the antenna to the chip's impedance. Metric 2, the outer line: its length, starting from a quarter wavelength per arm, sets the frequency. Moves them until the antenna is the chip's conjugate at f_mhz, inside the footprint, in its surroundings. It takes seconds, far less than antenna_tag_design, so use it after choosing a structure, when a frequency changes, or when asked what a design's metrics are. Also gives what a millimetre of each is worth. Works in the background: antenna_wait, then antenna_tag_options, under tune. When asked a tag's metrics or current numbers, in any language, call this with only_read before answering: it changes nothing, and needs no earlier run. A tune doesn't change the open design: afterwards, a separate call with only apply: true makes it current, and sets the mesh, the sweep and any missing requirements to match. Knows the structures in antenna_tag_options apart from the two-radiator one; for a drawing, name loop_vars and arm_var. It says plainly when a footprint can't do it.",
            json!({ "type": "object", "properties": surroundings(json!({
                "f_mhz": { "type": "number", "description": "The frequency to tune to, such as 902" },
                "f_lo_ghz": { "type": "number", "description": "Band to report range across. The requirements' band, or just f_mhz, by default" },
                "f_hi_ghz": { "type": "number" },
                "chip": { "type": "string", "description": "A chip id from antenna_references. The requirements' chip by default" },
                "chip_z": { "type": "array", "items": { "type": "number" }, "description": "[R, X] series at 915 MHz, X negative, if it's not in the table" },
                "read_dbm": { "type": "number", "description": "Chip sensitivity, if not from the table" },
                "eirp_dbm": { "type": "number", "description": "Reader power with antenna gain, 36 by default" },
                "max_mm": { "type": "array", "items": { "type": "number" }, "description": "Footprint [x, y]. The requirements' size by default" },
                "min_feature_um": { "type": "number", "description": "Thinnest trace and gap the process makes, 200 by default" },
                "metal": { "type": "string", "description": "The design's own by default, or aluminium if it has none yet" },
                "loop_vars": { "type": "array", "items": { "type": "string" }, "description": "For a drawing: the variables of the loop's length and width" },
                "arm_var": { "type": "string", "description": "For a drawing: the variable that lengthens the line" },
                "only_read": { "type": "boolean" },
                "apply": { "type": "boolean", "description": "On its own, after reading the result: makes the last tune the open design" }
            })) }),
        ),
    ]
}

fn text(v: Value) -> ToolOutput {
    ToolOutput::text(v.to_string())
}

fn layers_of(v: &Value) -> Result<Vec<Layer>, String> {
    let Some(a) = v.as_array() else { return Ok(Vec::new()) };
    a.iter()
        .map(|l| {
            let material = l["material"].as_str().ok_or("A layer needs a material.")?;
            let um = l["um"].as_f64().ok_or("A layer needs its thickness in um.")?;
            Ok(Layer::new(l["name"].as_str().unwrap_or(material), material, um * 1e-6))
        })
        .collect()
}

/// The surroundings a tool call asks for: `None` if it asks for nothing.
fn stack_of(i: &Value) -> Result<Option<Stack>, String> {
    if let Some(id) = i["preset"].as_str() {
        let Some(p) = stack::preset(id) else {
            let names: Vec<&str> = stack::PRESETS.iter().map(|p| p.id).collect();
            return Err(format!("No preset '{id}': there's {}.", names.join(", ")));
        };
        return Ok(Some((p.stack)()));
    }
    if i["above"].is_null() && i["below"].is_null() && i["metal_behind"].is_null() {
        return Ok(None);
    }
    let s = Stack { above: layers_of(&i["above"])?, below: layers_of(&i["below"])?, metal_behind: i["metal_behind"].as_bool().unwrap_or(false), margin: 0.0 };
    s.check().map_err(|e| format!("{e:#}"))?;
    Ok(Some(s))
}

fn chip_of(i: &Value) -> Result<((f64, f64), f64), String> {
    let chip = i["chip"].as_str().filter(|c| !c.is_empty());
    let found = match chip {
        Some(id) => Some(antenna_core::tag::chip(id).ok_or_else(|| format!("No chip '{id}': see antenna_references, or give chip_z and read_dbm."))?),
        None => None,
    };
    let given = i["chip_z"].as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)));
    let Some(z) = given.or_else(|| found.and_then(|c| c.z)) else {
        return Err(match found {
            Some(c) => format!("{} has no published impedance, so a design can't be matched to it yet. {} Ask the user for the vendor's figure or a measurement, then give it as chip_z [R, X]. Don't borrow another chip's value.", c.name, c.note),
            None => "Give a chip from antenna_references, or chip_z [R, X] and read_dbm.".to_string(),
        });
    };
    if z.0 <= 0.0 || z.1 >= 0.0 {
        return Err("chip_z is [R, X] in series form: a chip has positive resistance and negative reactance, like [15, -231].".into());
    }
    let read = i["read_dbm"].as_f64().or(found.and_then(|c| c.read_dbm)).ok_or("Give read_dbm, the chip's sensitivity: it isn't in the table for this chip.")?;
    Ok((z, read))
}

fn r2(v: f64) -> Value {
    if v.is_finite() { json!((v * 100.0).round() / 100.0) } else { Value::Null }
}

/// What a design has to do, from the goal it was found or tuned for.
fn requirements_of(g: &TagGoal) -> antenna_core::assess::Requirements {
    let size = g.max_x.zip(g.max_y).map(|(x, y)| [x * 1.001, y * 1.001, 0.01]);
    antenna_core::assess::Requirements { f_lo: g.f_lo, f_hi: g.f_hi, s11_db: -10.0, min_gain_dbi: None, min_efficiency: None, max_axial_ratio_db: None, min_front_to_back_db: None, max_size_m: size, chip_z: Some(g.chip_z), min_power_transfer: None, read_range_from: Some((g.read_dbm, g.eirp_dbm)) }
}

/// Both metrics, the way they're asked for.
fn reading_json(r: &Reading) -> Value {
    json!({
        "target_mhz": r2(r.f0 / 1e6),
        "loop": {
            "dimensions_mm": r.loop_dims.iter().map(|(n, v)| json!([n, r2(v * 1e3)])).collect::<Vec<_>>(),
            "antenna_z": [r2(r.z.0), r2(r.z.1)],
            "needed_z": [r2(r.needed.0), r2(r.needed.1)],
            "power_transfer": r2(r.transfer),
            "matched": r.matched(),
        },
        "line": {
            "variable": [r.arm_var.0, r2(r.arm_var.1 * 1e3)],
            "arm_along_metal_mm": r.arm_wire.map(|w| r2(w * 1e3)),
            "quarter_wave_mm": r2(r.quarter_wave * 1e3),
            "arm_in_quarter_waves": r.arm_wire.map(|w| r2(w / r.quarter_wave)),
            "peak_mhz": r.peak_hz.map(|f| r2(f / 1e6)),
            "peak_power_transfer": r2(r.peak_transfer),
            "on_frequency": r.on_frequency(),
        },
        "range_at_target_m": r2(r.range_m),
        "gain_dbi": r2(r.gain_dbi),
        "radiation_efficiency": r2(r.efficiency),
        "size_mm": [r2(r.size_x * 1e3), r2(r.size_y * 1e3)],
    })
}

fn tune_json(t: &Tune) -> Value {
    json!({
        "before": reading_json(&t.before),
        "after": reading_json(&t.after),
        "changes": t.changes.iter().map(|(n, a, b)| if tag_tune::is_count(n) { json!({ "variable": n, "from": a, "to": b }) } else { json!({ "variable": n, "from_mm": r2(a * 1e3), "to_mm": r2(b * 1e3) }) }).collect::<Vec<_>>(),
        "per_extra_mm": t.worth.iter().map(|w| json!({ "variable": w.var, "part": if w.is_line { "line" } else { "loop" }, "resistance_ohm": r2(w.r_per_mm), "reactance_ohm": r2(w.x_per_mm), "peak_mhz": w.mhz_per_mm.map(r2) })).collect::<Vec<_>>(),
        "band_worst_range_m": r2(t.score.worst_range_m),
        "band_worst_power_transfer": r2(t.score.worst_transfer),
        "notes": t.notes,
        "solves": t.solves,
        "to_use_it": "This hasn't changed the open design. Call antenna_tag_tune with only apply: true, then antenna_run, antenna_wait, antenna_assess. Don't copy the dimensions across by hand: more than these moved, and apply also sets the mesh and sweep to match.",
    })
}

fn option_json(o: &TagOption) -> Value {
    let s = &o.score;
    json!({
        "id": o.id,
        "structure": o.title,
        "idea": o.idea,
        "good_for": o.good_for,
        "stands_out": o.stands_out,
        "worst_range_m": r2(s.worst_range_m),
        "worst_range_with_circular_reader_m": r2(s.worst_range_m * std::f64::consts::FRAC_1_SQRT_2),
        "centre_range_m": r2(s.center_range_m),
        "worst_power_transfer": r2(s.worst_transfer),
        "gain_dbi": r2(s.gain_dbi),
        "radiation_efficiency": r2(s.efficiency),
        "size_mm": [r2(s.size_x * 1e3), r2(s.size_y * 1e3)],
        "antenna_z": [r2(s.z_center.0), r2(s.z_center.1)],
        "band_above_70pct_of_best_mhz": o.band.map(|b| [r2(b.0 / 1e6), r2(b.1 / 1e6)]),
        "if_surroundings_change": o.if_changed.iter().map(|(what, r)| json!({ "what": what, "worst_range_m": r2(*r) })).collect::<Vec<_>>(),
        "dimensions": o.values.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
        "mesh_shift": r2(o.mesh_shift),
        "warnings": o.warnings,
    })
}

const EVIDENCE: &str = "Simulated, not measured: a forward-link estimate, aligned, with layers as wide as the world and a surface wave counted as loss. Measure before committing to a build.";

fn options_tool(world: &World) -> ToolOutput {
    let st = world.resource::<Studio>();
    let run = st.tag_run().unwrap_or_default();
    let job = match st.status() {
        super::Status::Idle => json!({ "state": "idle" }),
        super::Status::Busy(m, p) => json!({ "state": "running", "message": m, "progress": p }),
        super::Status::Failed(e) => json!({ "state": "failed", "error": e }),
    };
    let structures: Vec<Value> = FAMILIES.iter().map(|f| json!({ "id": f.template, "title": f.title, "idea": f.idea, "good_for": f.good_for })).collect();
    let retune = run.retune.as_ref().map(|r| {
        json!({
            "summary": r.note,
            "as_designed": { "worst_range_m": r2(r.before.worst_range_m), "z": [r2(r.before.z_center.0), r2(r.before.z_center.1)] },
            "in_new_surroundings": { "worst_range_m": r2(r.detuned.worst_range_m), "z": [r2(r.detuned.z_center.0), r2(r.detuned.z_center.1)] },
            "retuned": { "worst_range_m": r2(r.after.worst_range_m), "z": [r2(r.after.z_center.0), r2(r.after.z_center.1)] },
            "changes_mm": r.changes.iter().map(|c| json!({ "variable": c.0, "from": r2(c.1 * 1e3), "to": r2(c.2 * 1e3) })).collect::<Vec<_>>(),
        })
    });
    let mut out = json!({
        "job": job,
        "surroundings": run.goal.as_ref().map(|g| g.stack.describe()),
        "options": run.options.iter().map(option_json).collect::<Vec<_>>(),
        "retune": retune,
        "structures": structures,
        "evidence": EVIDENCE,
    });
    // Left out rather than null: a model reads an empty tune as a tune that found nothing.
    if let Some(t) = &run.tune {
        out["tune"] = tune_json(t);
    }
    if let Some(r) = &run.reading {
        out["reading"] = reading_json(r);
    }
    text(out)
}

fn goal_from(world: &World, i: &Value) -> Result<TagGoal, String> {
    let (Some(lo), Some(hi)) = (i["f_lo_ghz"].as_f64(), i["f_hi_ghz"].as_f64()) else { return Err("give f_lo_ghz and f_hi_ghz".into()) };
    if !(lo > 0.0 && hi >= lo) {
        return Err("the band must have f_hi_ghz at or above f_lo_ghz, both above 0".into());
    }
    let (chip_z, read_dbm) = chip_of(i)?;
    let st = world.resource::<Studio>();
    let stack = stack_of(i)?.unwrap_or_else(|| st.project.stack.clone());
    let size = i["max_mm"].as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()? * 1e-3, a[1].as_f64()? * 1e-3)));
    let metal = i["metal"].as_str().unwrap_or("Aluminium").to_string();
    if !antenna_core::material::by_name(&metal).is_some_and(|m| m.is_conductor()) {
        return Err(format!("'{metal}' isn't a metal in the material library."));
    }
    Ok(TagGoal { f_lo: lo * 1e9, f_hi: hi * 1e9, chip_z, read_dbm, eirp_dbm: i["eirp_dbm"].as_f64().unwrap_or(36.0), max_x: size.map(|s| s.0), max_y: size.map(|s| s.1), min_feature: i["min_feature_um"].as_f64().unwrap_or(200.0) * 1e-6, stack, metal })
}

fn design_tool(world: &mut World, i: &Value) -> ToolOutput {
    if world.resource::<Studio>().busy() {
        return ToolOutput::err("a job is already running");
    }
    let goal = match goal_from(world, i) {
        Ok(g) => g,
        Err(e) => return ToolOutput::err(e),
    };
    let effort = i["effort"].as_str().and_then(Effort::parse).unwrap_or(Effort::Normal);
    let templates: Vec<String> = i["structures"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    for t in &templates {
        if tag_design::family(t).is_none() {
            let names: Vec<&str> = FAMILIES.iter().map(|f| f.template).collect();
            return ToolOutput::err(format!("No structure '{t}': there's {}. Draw anything else, and give it as vary.", names.join(", ")));
        }
    }
    let mut extra = Vec::new();
    if let Some(vary) = i["vary"].as_array().filter(|a| !a.is_empty()) {
        let vars: Vec<(String, f64, f64)> = vary.iter().filter_map(|v| Some((v["name"].as_str()?.to_string(), v["min"].as_f64()?, v["max"].as_f64()?))).collect();
        let st = world.resource::<Studio>();
        let known = st.project.vars().unwrap_or_default();
        if let Some(bad) = vars.iter().find(|v| !known.contains_key(&v.0)) {
            return ToolOutput::err(format!("no variable '{}': see antenna_get", bad.0));
        }
        if vars.is_empty() {
            return ToolOutput::err("vary is [{name, min, max}], in SI units");
        }
        let own = tag_design::fitted(&st.project, &goal);
        if let Err(e) = own.geometry() {
            return ToolOutput::err(format!("The current design can't be a candidate: {e:#}"));
        }
        extra.push(Candidate { title: i["title"].as_str().unwrap_or("Your own design").to_string(), idea: i["idea"].as_str().unwrap_or("Drawn for this job.").to_string(), good_for: "What it was drawn for".to_string(), project: own, vars });
    }
    // Asking only for your own candidate shouldn't also search all of them.
    let templates = if templates.is_empty() && !extra.is_empty() && i["only_vary"].as_bool().unwrap_or(false) { vec!["__none__".to_string()] } else { templates };
    let evolve = i["evolve"].as_object().map(|e| {
        let n = |k: &str, d: u64| e.get(k).and_then(Value::as_u64).unwrap_or(d) as usize;
        (n("nx", 14).clamp(3, 40), n("ny", 6).clamp(2, 24), n("solves", 480).clamp(24, 20000))
    });
    let count = if templates.is_empty() { FAMILIES.len() } else { templates.len() } + extra.len();
    // Structures with many dimensions get more than this, so it's the least.
    let total = effort.solves() * count + evolve.map_or(0, |e| e.2);
    let says = goal.stack.describe();
    world.resource_mut::<Studio>().run_tag_design(goal, templates, extra, effort, evolve);
    text(json!({ "started": true, "structures": count, "about_solves": total, "surroundings": says, "next": "antenna_wait until it's done (it can take several calls), then antenna_tag_options" }))
}

fn stack_tool(world: &mut World, i: &Value) -> ToolOutput {
    if i["clear"].as_bool().unwrap_or(false) {
        world.resource_mut::<Studio>().project.stack = Stack::default();
        return text(json!({ "ok": true, "surroundings": "antenna / nothing else" }));
    }
    match stack_of(i) {
        Err(e) => ToolOutput::err(e),
        Ok(None) => {
            let presets: Vec<Value> = stack::PRESETS.iter().map(|p| json!({ "id": p.id, "about": p.about, "layers": (p.stack)().describe() })).collect();
            let materials: Vec<String> = antenna_core::material::library().into_iter().filter(|m| !m.is_conductor()).map(|m| format!("{} (er {})", m.name, m.eps_r())).collect();
            text(json!({ "presets": presets, "materials": materials, "current": world.resource::<Studio>().project.stack.describe(), "note": "Paper, PET and label glue vary from stock to stock, and the glue and corrugated board figures are assumed: measure yours for a design that has to be right." }))
        }
        Ok(Some(s)) => {
            let mut st = world.resource_mut::<Studio>();
            st.project.stack = s;
            let solver = st.project.geometry().map(|g| st.project.solver_for(&g).label().to_string()).unwrap_or_default();
            text(json!({ "ok": true, "surroundings": st.project.stack.describe(), "solver": solver, "note": "The design hasn't moved, but its resonance has: run it, then tune, or antenna_tag_retune." }))
        }
    }
}

fn draw_tool(world: &mut World, i: &Value) -> ToolOutput {
    let mut st = world.resource_mut::<Studio>();
    if st.busy() {
        return ToolOutput::err("a job is already running");
    }
    let mut project = st.project.clone();
    // Drawing on a template would mix its parts up with these unless that's what was meant.
    if project.template != "custom" && !i["fresh"].as_bool().unwrap_or(false) {
        match project.editable_copy() {
            Ok(p) => project = p,
            Err(e) => return ToolOutput::err(format!("The current design can't be drawn on: {e:#}. Use fresh to start blank.")),
        }
    }
    match freeform::draw(&mut project, i) {
        Ok(d) => {
            let design = json!({
                "parts": project.extra.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
                "feeds": project.extra_ports.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
                "variables": project.params.iter().filter(|r| !r.derived).map(|r| json!([r.name, r.expr])).collect::<Vec<_>>(),
            });
            let (size, solver, warn) = match project.geometry() {
                Ok(g) => {
                    let (lo, hi) = g.bounds();
                    (json!([r2((hi[0] - lo[0]) * 1e3), r2((hi[1] - lo[1]) * 1e3), r2((hi[2] - lo[2]) * 1e3)]), project.solver_for(&g).label().to_string(), Value::Null)
                }
                Err(e) => (Value::Null, String::new(), json!(format!("{e:#}"))),
            };
            st.project = project;
            st.clear_latest();
            st.ui.page = super::ui::Page::Draw;
            let added = json!({ "parts": d.parts, "feeds": d.feeds, "variables": d.vars });
            drop(st);
            world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
            text(json!({ "ok": true, "added": added, "design": design, "size_mm": size, "solver": solver, "not_ready": warn }))
        }
        Err(e) => ToolOutput::err(format!("{e:#}")),
    }
}

fn open_tool(world: &mut World, i: &Value) -> ToolOutput {
    let id = i["id"].as_str().unwrap_or("");
    let st = world.resource::<Studio>();
    if st.busy() {
        return ToolOutput::err("a job is already running");
    }
    let Some(run) = st.tag_run() else { return ToolOutput::err("Nothing has been searched yet: antenna_tag_design.") };
    let Some(option) = run.options.iter().find(|o| o.id == id) else {
        let ids: Vec<&str> = run.options.iter().map(|o| o.id.as_str()).collect();
        return ToolOutput::err(format!("No option '{id}': there's {}.", ids.join(", ")));
    };
    let project = option.project.clone();
    let body = option_json(option);
    if let Some(g) = &run.goal {
        let read = Some((g.read_dbm, g.eirp_dbm));
        let (size, fc) = (g.max_x.zip(g.max_y).map(|(x, y)| [x * 1.001, y * 1.001, 0.01]), g.fc());
        let _ = (read, size);
        world.insert_resource(AntennaSpec(Some(requirements_of(g))));
        let mut st = world.resource_mut::<Studio>();
        st.open_tag_option(project);
        st.project.setup.f_lo = fc * 0.9;
        st.project.setup.f_hi = fc * 1.1;
        st.project.setup.edges_per_lambda = tag_design::clean_mesh(&st.project);
        st.project.setup.mom_samples = 21;
    }
    world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
    text(json!({ "ok": true, "opened": body, "next": "antenna_run, antenna_wait, antenna_assess. The requirements are set from the search." }))
}

fn retune_tool(world: &mut World, i: &Value) -> ToolOutput {
    if i["apply"].as_bool().unwrap_or(false) && i["preset"].is_null() && i["above"].is_null() && i["below"].is_null() {
        let st = world.resource::<Studio>();
        let Some(r) = st.tag_run().and_then(|t| t.retune) else { return ToolOutput::err("No retune to apply yet.") };
        if st.busy() {
            return ToolOutput::err("a job is already running");
        }
        let mut st = world.resource_mut::<Studio>();
        st.open_tag_option(r.project.clone());
        return text(json!({ "ok": true, "surroundings": r.project.stack.describe(), "next": "antenna_run, antenna_wait, antenna_assess" }));
    }
    let st = world.resource::<Studio>();
    if st.busy() {
        return ToolOutput::err("a job is already running");
    }
    let stack = match stack_of(i) {
        Ok(Some(s)) => s,
        Ok(None) => return ToolOutput::err("Say what it's going in: preset, or above and below. antenna_stack lists them."),
        Err(e) => return ToolOutput::err(e),
    };
    let spec = world.get_resource::<AntennaSpec>().and_then(|s| s.0.clone());
    let Some(req) = spec.filter(|r| r.chip_z.is_some()) else { return ToolOutput::err("The design needs requirements with a chip first: antenna_spec, with chip or chip_z.") };
    let st = world.resource::<Studio>();
    let (read, eirp) = req.read_range_from.unwrap_or((-22.0, 36.0));
    let goal = TagGoal { f_lo: req.f_lo, f_hi: req.f_hi, chip_z: req.chip_z.unwrap_or((15.0, -231.0)), read_dbm: read, eirp_dbm: eirp, max_x: req.max_size_m.map(|m| m[0]), max_y: req.max_size_m.map(|m| m[1]), min_feature: 100e-6, stack, metal: st.project.metal.clone() };
    let reach = i["reach"].as_f64().unwrap_or(0.15).clamp(0.01, 0.5);
    let solves = i["solves"].as_u64().unwrap_or(120).clamp(10, 3000) as usize;
    let says = goal.stack.describe();
    world.resource_mut::<Studio>().run_tag_retune(goal, reach, solves);
    text(json!({ "started": true, "surroundings": says, "next": "antenna_wait, then antenna_tag_options" }))
}

fn tune_tool(world: &mut World, i: &Value) -> ToolOutput {
    let st = world.resource::<Studio>();
    if st.busy() {
        return ToolOutput::err("a job is already running");
    }
    if i["apply"].as_bool().unwrap_or(false) && i["f_mhz"].is_null() {
        let run = st.tag_run();
        let Some(t) = run.as_ref().and_then(|r| r.tune.clone()) else { return ToolOutput::err("No tune to apply yet: antenna_tag_tune with f_mhz.") };
        // Requirements someone already set stand. Without any, it's judged by what it was tuned for.
        let has_chip = world.get_resource::<AntennaSpec>().and_then(|s| s.0.as_ref()).is_some_and(|r| r.chip_z.is_some());
        if let (false, Some(g)) = (has_chip, run.as_ref().and_then(|r| r.goal.as_ref())) {
            world.insert_resource(AntennaSpec(Some(requirements_of(g))));
        }
        world.resource_mut::<Studio>().open_tag_option(t.project.clone());
        world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
        return text(json!({ "ok": true, "surroundings": t.project.stack.describe(), "after": reading_json(&t.after), "next": "antenna_run, antenna_wait, antenna_assess. The requirements are set from the tune unless you already set them." }));
    }
    if i["apply"].as_bool().unwrap_or(false) {
        return ToolOutput::err("Nothing has run. apply is its own call, made after reading the result: antenna_tag_tune with f_mhz, antenna_wait, antenna_tag_options, then antenna_tag_tune with only apply: true.");
    }
    let Some(mhz) = i["f_mhz"].as_f64().filter(|f| *f > 0.0) else { return ToolOutput::err("Give f_mhz, the frequency to tune to, such as 902.") };
    let spec = world.get_resource::<AntennaSpec>().and_then(|s| s.0.clone());
    // What wasn't said comes from the requirements, then the design.
    let mut asked = i.clone();
    let Some(o) = asked.as_object_mut() else { return ToolOutput::err("The arguments have to be an object.") };
    if !o.contains_key("f_lo_ghz") {
        o.insert("f_lo_ghz".into(), json!(spec.as_ref().map_or(mhz / 1e3, |r| r.f_lo / 1e9)));
    }
    if !o.contains_key("f_hi_ghz") {
        o.insert("f_hi_ghz".into(), json!(spec.as_ref().map_or(mhz / 1e3, |r| r.f_hi / 1e9)));
    }
    if let Some(r) = &spec {
        if !o.contains_key("chip") && !o.contains_key("chip_z") && let Some(z) = r.chip_z {
            o.insert("chip_z".into(), json!([z.0, z.1]));
        }
        if !o.contains_key("read_dbm") && let Some((read, _)) = r.read_range_from {
            o.insert("read_dbm".into(), json!(read));
        }
        if !o.contains_key("eirp_dbm") && let Some((_, eirp)) = r.read_range_from {
            o.insert("eirp_dbm".into(), json!(eirp));
        }
        if !o.contains_key("max_mm") && let Some(m) = r.max_size_m {
            o.insert("max_mm".into(), json!([m[0] * 1e3, m[1] * 1e3]));
        }
    }
    let st = world.resource::<Studio>();
    if !o.contains_key("metal") && antenna_core::material::by_name(&st.project.metal).is_some_and(|m| m.is_conductor()) && st.project.metal != "PEC" {
        o.insert("metal".into(), json!(st.project.metal));
    }
    let goal = match goal_from(world, &asked) {
        Ok(g) => g,
        Err(e) => return ToolOutput::err(e),
    };
    let st = world.resource::<Studio>();
    let named = i["arm_var"].as_str().map(|arm| Knobs { loop_vars: i["loop_vars"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()), arm_var: arm.to_string() });
    let Some(knobs) = named.or_else(|| tag_tune::knobs_for(&st.project)) else {
        return ToolOutput::err("This design has no one loop and line this knows: name them with loop_vars and arm_var, see antenna_get. A two-radiator tag has two frequencies, so use antenna_tag_design for that.");
    };
    let known = st.project.vars().unwrap_or_default();
    if knobs.loop_vars.is_empty() {
        return ToolOutput::err("Give loop_vars as well: the variables of the loop's length and width.");
    }
    if let Some(bad) = knobs.loop_vars.iter().chain([&knobs.arm_var]).find(|n| !known.contains_key(*n)) {
        return ToolOutput::err(format!("no variable '{bad}': see antenna_get"));
    }
    let only_read = i["only_read"].as_bool().unwrap_or(false);
    let says = goal.stack.describe();
    world.resource_mut::<Studio>().run_tag_tune(goal, mhz * 1e6, knobs.clone(), only_read);
    text(json!({ "started": true, "target_mhz": mhz, "loop": knobs.loop_vars, "line": knobs.arm_var, "only_read": only_read, "surroundings": says, "next": "antenna_wait, then antenna_tag_options, under tune or reading. The open design doesn't change until a separate call with only apply: true." }))
}

/// Handles this module's tools; `None` for anything else.
pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        "antenna_stack" => stack_tool(world, i),
        "antenna_draw" => draw_tool(world, i),
        "antenna_tag_design" => design_tool(world, i),
        "antenna_tag_options" => options_tool(world),
        "antenna_tag_open" => open_tool(world, i),
        "antenna_tag_retune" => retune_tool(world, i),
        "antenna_tag_tune" => tune_tool(world, i),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn world() -> World {
        let mut w = World::new();
        w.insert_resource(Studio::default());
        w.insert_resource(crate::ui::LeftPanelSlot::default());
        w.insert_resource(AntennaSpec::default());
        w
    }

    fn call(w: &mut World, name: &str, input: Value) -> Result<Value, String> {
        let out = crate::antenna::tools::start(w, name, &input).unwrap_or_else(|| panic!("{name} isn't a tool"));
        if out.is_error { Err(out.summary()) } else { Ok(serde_json::from_str(&out.summary()).unwrap_or(Value::Null)) }
    }

    fn wait_for_job(w: &World) {
        let t = Instant::now();
        while w.resource::<Studio>().busy() {
            assert!(t.elapsed() < Duration::from_secs(900), "the job didn't finish");
            std::thread::sleep(Duration::from_millis(50));
        }
        if let crate::antenna::Status::Failed(e) = w.resource::<Studio>().status() {
            panic!("the job failed: {e}");
        }
    }

    /// The way the assistant would work: surroundings, a search of different structures, options that
    /// differ in kind, opening one and proving it, its own drawing tested against them, and a retune.
    #[test]
    #[ignore = "real searches: about a minute. cargo test --bin SenseSim antenna_tag_tools -- --ignored --nocapture"]
    fn antenna_tag_tools_work_a_whole_design() {
        let mut w = world();
        let listed = call(&mut w, "antenna_stack", json!({})).unwrap();
        assert!(listed["presets"].as_array().unwrap().iter().any(|p| p["id"] == "paper_label"), "{listed}");

        let bad = call(&mut w, "antenna_tag_design", json!({ "f_lo_ghz": 0.902, "f_hi_ghz": 0.928, "chip": "qstar_7u", "preset": "paper_label" })).unwrap_err();
        assert!(bad.contains("no published impedance"), "{bad}");

        let started = call(&mut w, "antenna_tag_design", json!({ "f_lo_ghz": 0.902, "f_hi_ghz": 0.928, "chip": "ucode9", "preset": "paper_label", "max_mm": [95, 30], "structures": ["tag_bowtie", "tag_loop", "tag_dogbone"], "effort": "quick" })).unwrap();
        assert_eq!(started["structures"], 3, "{started}");
        wait_for_job(&w);
        let found = call(&mut w, "antenna_tag_options", json!({})).unwrap();
        let options = found["options"].as_array().unwrap();
        println!("{}", serde_json::to_string_pretty(&found["options"]).unwrap());
        assert_eq!(options.len(), 3, "{found}");
        let kinds: std::collections::HashSet<&str> = options.iter().map(|o| o["structure"].as_str().unwrap()).collect();
        assert_eq!(kinds.len(), 3, "options that differ in kind: {kinds:?}");
        assert!(options.iter().any(|o| !o["stands_out"].as_array().unwrap().is_empty()));
        assert!(options[0]["worst_range_m"].as_f64().unwrap() >= options[2]["worst_range_m"].as_f64().unwrap(), "best first");
        assert!(found["evidence"].as_str().unwrap().contains("not measured"));

        call(&mut w, "antenna_tag_open", json!({ "id": "option_1" })).unwrap();
        assert!(!w.resource::<Studio>().project.stack.is_empty(), "it opens in its surroundings");
        call(&mut w, "antenna_run", json!({})).unwrap();
        wait_for_job(&w);
        let judged = call(&mut w, "antenna_assess", json!({})).unwrap();
        println!("{judged}");
        assert!(judged["verdict"].as_str().unwrap().starts_with("met"), "{judged}");

        // An idea of its own: a straight dipole with a shunt loop, drawn from nothing and searched.
        let drawn = call(&mut w, "antenna_draw", json!({
            "fresh": true, "name": "Own idea",
            "vars": { "L": "80mm", "w": "1mm", "gp": "1mm", "hs": "10mm", "xs": "8mm" },
            "parts": [
                { "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" },
                { "kind": "strip", "name": "hairpin", "path": [["xs", "0"], ["xs", "hs"], ["-xs", "hs"], ["-xs", "0"]], "w": "w" }
            ],
            "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
        })).unwrap();
        assert_eq!(drawn["added"]["feeds"][0], "chip", "{drawn}");
        call(&mut w, "antenna_tag_design", json!({ "f_lo_ghz": 0.902, "f_hi_ghz": 0.928, "chip": "ucode9", "max_mm": [95, 30], "effort": "quick", "only_vary": true, "title": "Hairpin dipole", "vary": [{ "name": "L", "min": 0.05, "max": 0.095 }, { "name": "hs", "min": 0.004, "max": 0.025 }, { "name": "xs", "min": 0.003, "max": 0.03 }] })).unwrap();
        wait_for_job(&w);
        let own = call(&mut w, "antenna_tag_options", json!({})).unwrap();
        assert_eq!(own["options"][0]["structure"], "Hairpin dipole", "{own}");
        assert!(own["surroundings"].as_str().unwrap().contains("paper"), "it stays in its surroundings: {own}");

        // What a thicker stock does to it.
        call(&mut w, "antenna_tag_open", json!({ "id": "option_1" })).unwrap();
        call(&mut w, "antenna_tag_retune", json!({ "preset": "thick_paper_label", "solves": 30 })).unwrap();
        wait_for_job(&w);
        let r = call(&mut w, "antenna_tag_options", json!({})).unwrap();
        println!("{}", r["retune"]);
        let (was, then) = (r["retune"]["as_designed"]["worst_range_m"].as_f64().unwrap(), r["retune"]["in_new_surroundings"]["worst_range_m"].as_f64().unwrap());
        assert!(then < was, "thicker paper detunes it: {was} to {then}");
        assert!(r["retune"]["retuned"]["worst_range_m"].as_f64().unwrap() >= then, "{r}");
        call(&mut w, "antenna_tag_retune", json!({ "apply": true })).unwrap();
        assert!(w.resource::<Studio>().project.stack.describe().contains("300 µm"));
    }

    /// Asked the way the assistant would ask: for the metrics of a tag at 902 MHz, then the tune itself.
    #[test]
    fn antenna_tag_tune_reports_both_metrics_and_lands_on_the_frequency() {
        let mut w = world();
        let no_f = call(&mut w, "antenna_tag_tune", json!({ "chip": "ucode9" })).unwrap_err();
        assert!(no_f.contains("f_mhz"), "{no_f}");
        assert!(call(&mut w, "antenna_tag_tune", json!({ "apply": true })).unwrap_err().contains("No tune"));
        let together = call(&mut w, "antenna_tag_tune", json!({ "apply": true, "f_mhz": 902, "chip": "ucode9" })).unwrap_err();
        assert!(together.contains("its own call"), "it mustn't quietly tune and not apply: {together}");
        w.resource_mut::<Studio>().start_tag_from("rfid_tag", 902e6);

        let unknown = call(&mut w, "antenna_tag_tune", json!({ "f_mhz": 902, "chip": "qstar_7u" })).unwrap_err();
        assert!(unknown.contains("no published impedance"), "{unknown}");
        let bad = call(&mut w, "antenna_tag_tune", json!({ "f_mhz": 902, "chip": "ucode9", "loop_vars": ["nope"], "arm_var": "Hm" })).unwrap_err();
        assert!(bad.contains("nope"), "{bad}");

        call(&mut w, "antenna_tag_tune", json!({ "f_mhz": 902, "chip": "ucode9", "max_mm": [95, 30], "only_read": true })).unwrap();
        wait_for_job(&w);
        let read = call(&mut w, "antenna_tag_options", json!({})).unwrap();
        assert!(read["reading"]["loop"]["antenna_z"].is_array(), "{read}");
        assert_eq!(read["reading"]["loop"]["matched"], false, "the template isn't matched yet: {read}");
        assert!(read.get("tune").is_none(), "nothing was tuned, so it mustn't look like a tune that found nothing: {read}");

        let was = w.resource::<Studio>().project.clone();
        call(&mut w, "antenna_tag_tune", json!({ "f_mhz": 902, "f_lo_ghz": 0.902, "f_hi_ghz": 0.928, "chip": "ucode9", "max_mm": [95, 30] })).unwrap();
        wait_for_job(&w);
        let out = call(&mut w, "antenna_tag_options", json!({})).unwrap();
        let tune = &out["tune"];
        println!("{}", serde_json::to_string_pretty(tune).unwrap());
        assert_eq!(tune["after"]["loop"]["matched"], true, "{tune}");
        assert_eq!(tune["after"]["line"]["on_frequency"], true, "{tune}");
        assert!(out["evidence"].as_str().unwrap().contains("not measured"));
        assert!(tune["per_extra_mm"].as_array().unwrap().iter().any(|r| r["part"] == "line"), "{tune}");
        assert!(tune["to_use_it"].as_str().unwrap().contains("apply"), "{tune}");
        // What it says fits, the assessment agrees fits, with nothing allowed for.
        let size = tune["after"]["size_mm"].as_array().unwrap();
        assert!(size[0].as_f64().unwrap() <= 95.0 && size[1].as_f64().unwrap() <= 30.0, "{size:?}");
        assert_eq!(w.resource::<Studio>().project.params.iter().map(|r| r.expr.clone()).collect::<Vec<_>>(), was.params.iter().map(|r| r.expr.clone()).collect::<Vec<_>>(), "tuning doesn't change the design until it's applied");

        // That tune went for one end of a band, and it says what that costs.
        assert!(tune["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("902-928")), "{tune}");

        // Now the way the assistant finishes a job: one frequency, applied, then proved by its own run.
        call(&mut w, "antenna_tag_tune", json!({ "f_mhz": 902, "chip": "ucode9", "max_mm": [95, 30] })).unwrap();
        wait_for_job(&w);
        let applied = call(&mut w, "antenna_tag_tune", json!({ "apply": true })).unwrap();
        assert!(applied["after"]["loop"]["matched"] == true, "{applied}");
        let now = w.resource::<Studio>().project.vars().unwrap();
        assert!((now["Hm"] - was.vars().unwrap()["Hm"]).abs() > 1e-4, "applying it does");
        assert!(w.resource::<AntennaSpec>().0.as_ref().and_then(|r| r.chip_z).is_some(), "there's something to judge it by");
        call(&mut w, "antenna_run", json!({})).unwrap();
        wait_for_job(&w);
        let judged = call(&mut w, "antenna_assess", json!({})).unwrap();
        println!("{judged}");
        assert!(judged["verdict"].as_str().unwrap().starts_with("met"), "the app's own run has to agree with the tune: {judged}");
    }

    #[test]
    fn surroundings_are_read_and_refused_in_words() {
        let mut w = world();
        let e = call(&mut w, "antenna_stack", json!({ "preset": "moon" })).unwrap_err();
        assert!(e.contains("paper_label"), "{e}");
        let e = call(&mut w, "antenna_stack", json!({ "below": [{ "material": "Aluminium", "um": 10 }] })).unwrap_err();
        assert!(e.contains("metal_behind"), "{e}");
        let ok = call(&mut w, "antenna_stack", json!({ "above": [{ "name": "paper", "material": "Paper", "um": 80 }], "below": [{ "material": "PET film", "um": 50 }] })).unwrap();
        assert_eq!(ok["surroundings"], "paper 80 µm / antenna / PET film 50 µm");
        call(&mut w, "antenna_stack", json!({ "clear": true })).unwrap();
        assert!(w.resource::<Studio>().project.stack.is_empty());
    }

    #[test]
    fn a_tool_is_gated_like_the_rest_of_its_kind() {
        use crate::agent::gate::{Class, class_of};
        assert_eq!(class_of("antenna_tag_options", &json!({})), Class::Read);
        assert_eq!(class_of("antenna_tag_tune", &json!({ "f_mhz": 902, "only_read": true })), Class::Read);
        assert_eq!(class_of("antenna_tag_tune", &json!({ "only_read": true, "apply": true })), Class::Project);
        for name in ["antenna_stack", "antenna_draw", "antenna_tag_design", "antenna_tag_open", "antenna_tag_retune", "antenna_tag_tune"] {
            assert_eq!(class_of(name, &json!({})), Class::Project, "{name}");
        }
    }
}
