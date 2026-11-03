//! Designing RFID tags from what they have to do: the background search, the retune, and the tools
//! the assistant works them with. The physics and the search are in `antenna_core::tag_design`.

use antenna_core::freeform;
use antenna_core::stack::{self, Layer, Stack};
use antenna_core::tag_design::{self, Candidate, Control, Effort, FAMILIES, Origin, Progress, Retune, TagGoal, TagOption};
use antenna_core::tag_tune::{self, Knobs, Reading, Tune};
use bevy::prelude::*;
use crossbeam_channel::Sender;
use serde_json::{Value, json};

use super::Studio;
use super::assist::{self, AntennaSpec};
use crate::agent::llm::{Block, ToolDef};
use crate::agent::gate::CallCtx;
use crate::agent::tools::{self, ToolJob, ToolOutput};

/// One round of designing. It runs on the caller's thread, because it waits for a search.
pub const TRY: &str = "antenna_tag_try";

/// How to write a structure for `antenna_tag_try`. Only the syntax: a bare dipole, which has nothing to
/// match a chip with, because a real model's first idea was the hairpin dipole this used to show.
const DRAW_EXAMPLE: &str = r#"{"fresh":true,"vars":{"L":"80mm","w":"1mm","gp":"1mm"},"notes":{"L":"Tip to tip","w":"Trace width","gp":"Gap at the feed"},"parts":[{"kind":"strip","name":"arm","path":[["gp/2","0"],["L/2","0"]],"w":"w","mirror":"x"}],"feeds":[{"a":["-gp/2","0"],"b":["gp/2","0"],"width":"w"}]}"#;
const TRY_EXAMPLE: &str = r#"{"title":"...","idea":"...","vars":{"L":"80mm","w":"1mm","gp":"1mm"},"notes":{"L":"Tip to tip","w":"Trace width","gp":"Gap at the chip"},"parts":[{"kind":"strip","name":"arm","path":[["gp/2","0"],["L/2","0"]],"w":"w","mirror":"x"}],"feeds":[{"a":["-gp/2","0"],"b":["gp/2","0"],"width":"w"}]}"#;

/// What's known for this goal so far: a structure tried again replaces its last result, and the rest
/// stay, best first. Results for another goal aren't comparable, so the caller starts those afresh.
fn merged(prior: Vec<TagOption>, new: Vec<TagOption>, goal: &TagGoal) -> Vec<TagOption> {
    let mut all: Vec<TagOption> = prior.into_iter().filter(|o| !new.iter().any(|n| n.title == o.title)).collect();
    all.extend(new);
    all.sort_by(|a, b| b.score.worst_range_m.total_cmp(&a.score.worst_range_m));
    for (k, o) in all.iter_mut().enumerate() {
        o.id = format!("option_{}", k + 1);
    }
    tag_design::mark_out(&mut all, goal);
    all
}

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
        // The page shows what's being searched, whoever asked for it.
        self.ui.tag.follow(&goal);
        self.spawn(move |ctx, shared| {
            {
                // Everything tried for this goal stays on the list, so rounds of designing add up.
                let mut s = shared.lock().unwrap();
                let prior = s.tag.take().filter(|t| t.goal.as_ref() == Some(&goal)).map_or(Vec::new(), |t| t.options);
                s.tag = Some(TagRun { goal: Some(goal.clone()), options: prior, ..Default::default() });
            }
            let report = {
                let shared = shared.clone();
                let ctx = ctx.clone();
                let goal = goal.clone();
                move |mut p: Progress| {
                    ctx.busy(format!("{}: {} of {} solves, best {:.1} m", p.message, p.done, p.total, p.best_range_m), p.done as f32 / p.total.max(1) as f32);
                    let found = p.found.take();
                    if let Some(t) = shared.lock().unwrap().tag.as_mut() {
                        if let Some(o) = found {
                            t.options = merged(std::mem::take(&mut t.options), vec![*o], &goal);
                        }
                        t.progress = Some(p);
                    }
                }
            };
            let ctl = Control { cancel: &ctx.cancel, report: &report, parallel: parallel() };
            let mut options = tag_design::explore(&goal, &templates, extra, effort, &ctl)?;
            if let Some((nx, ny, solves)) = evolve {
                options.push(tag_design::evolve(&goal, nx, ny, solves, &ctl)?);
            }
            if ctx.cancelled() {
                anyhow::bail!("cancelled");
            }
            if let Some(t) = shared.lock().unwrap().tag.as_mut() {
                t.options = merged(std::mem::take(&mut t.options), options, &goal);
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
            "antenna_sketch_view",
            "Look at a picture of an antenna the way it's meant to be read, before drawing from it. turn is how far to turn it clockwise, 0, 90, 180 or 270 degrees, until the writing is the right way up. box cuts out the antenna, as [left, top, right, bottom], shares from 0 to 1 of the picture once it's turned, and enlarges it so the strokes can be read: keep it close to the metal and leave out arrows and writing where you can. It never flips. Read the antenna from what comes back, not from the original. antenna_draw then brings its drawing back under this view, the same way up. Call it again to change either.",
            json!({ "type": "object", "properties": {
                "image_id": { "type": "string", "description": "The picture's id, from the chat" },
                "turn": { "type": "integer", "enum": [0, 90, 180, 270], "description": "Degrees clockwise" },
                "box": { "type": "array", "items": { "type": "number" }, "description": "left, top, right, bottom, from 0 to 1, of the picture once it's turned" }
            }, "required": ["image_id"] }),
        ),
        def(
            "antenna_draw",
            &format!("{} Only the syntax, a plain dipole: {DRAW_EXAMPLE}", "Draw your own antenna, any shape, instead of adapting a template. From a picture, call antenna_sketch_view first, and draw a closed outline as a filled polygon, not a strip. A drawing is always parametric, so it can be changed later by changing a variable: name every dimension first. vars holds each with its value and unit, like {\"L\":\"80mm\",\"w\":\"1mm\",\"gp\":\"1mm\"}, and notes says what each is, like {\"L\":\"Overall length\"}. parts and feeds are then written only over those names, with plain factors and 0: \"L/2\", \"-gp/2\", \"2*w\". A number with a unit in them, like \"12mm\" or \"a1+2mm\", or a plain figure, is refused and nothing is kept; only whole quarter turns in an angle are fine as they are. Use one name for whatever should move together, and write what follows from other dimensions as an expression in vars, like \"t\":\"L/4\". To change a size later use antenna_set on its variable, not this. To change a shape, send just that part or a named feed again: its name replaces it, mirror copies included. remove takes parts and feeds away by name. fresh starts blank, keeping band, layers and metal, for a different antenna. A point is a pair [u, v]. parts, each {kind, name, axis (default z), at (default 0, the height of its sheet), material, mirror (x, y or xy)}: strip {path, w}; rect {a, b}; polygon {pts}; plate {outline, holes}; disc {c, r}; arc {c, r, from_deg, to_deg, w}; meander {start, step, height, n, w}; box {a, b} and via {a, b, r} and wire {pts, r} in [x, y, z], which need FDTD. Coplanar metal that overlaps merges. feeds: {a, b, width, at, name, wave}, with a and b points either side of a gap, along x or y. wave true makes it a wave port for a microstrip, stripline or coax: a and b in [x, y, z] join the line's two conductors at the model's outer edge, and the built-in FDM solver launches the line's own mode there. Nothing is kept if it doesn't build. The answer shows a top view of what was drawn, to hold against a sketch, and names any new variable nothing uses."),
            json!({ "type": "object", "properties": { "fresh": { "type": "boolean" }, "name": { "type": "string" }, "metal": { "type": "string" }, "vars": { "type": "object", "description": "Every dimension: name to value with its unit, or an expression over other names" }, "notes": { "type": "object", "description": "Name to what that dimension is, in a few words" }, "parts": { "type": "array", "items": { "type": "object" }, "description": "Written over the names in vars: no numbers with units" }, "feeds": { "type": "array", "items": { "type": "object" }, "description": "Written over the names in vars: no numbers with units" }, "remove": { "type": "array", "items": { "type": "string" }, "description": "Names of parts and feeds to take away" } } }),
        ),
        def(
            TRY,
            &format!("One round of designing a tag: try a structure of your own. This one call draws it from nothing, searches its dimensions against the chip, band, footprint and surroundings, and answers with its range, power into the chip, what_limits_it in dB, what each dimension moves, which dimensions ran out of room, a map of where its current runs, and how it stands against everything tried so far. Takes a minute or two, and everything tried stays on the list. Band, chip, footprint and surroundings are needed on the first try and carry over. title and idea say what's different about it. vars are its dimensions with units, and each moves 40 % either way unless vary gives [{{name, min, max}}] in SI. parts and feeds are as in antenna_draw. A tag has one feed, the chip, across a gap. Only the syntax, a bare dipole with nothing to match a chip: {TRY_EXAMPLE}"),
            json!({ "type": "object", "properties": surroundings(json!({
                "title": { "type": "string", "description": "What to call this structure. A title used again replaces its last result" },
                "idea": { "type": "string", "description": "Why this structure: what's different about it and what it should do better" },
                "vars": { "type": "object", "description": "Every dimension: name to value with its unit, or an expression over other names, which then isn't searched by itself" },
                "notes": { "type": "object", "description": "Name to what that dimension is, in a few words" },
                "parts": { "type": "array", "items": { "type": "object" }, "description": "Written over the names in vars: no numbers with units" },
                "feeds": { "type": "array", "items": { "type": "object" }, "description": "Written over the names in vars: no numbers with units" },
                "vary": { "type": "array", "items": { "type": "object" }, "description": "[{name, min, max}] in SI, in place of 40 % either way" },
                "effort": { "type": "string", "enum": ["quick", "normal", "deep"], "description": "normal by default. Quick under-tunes a new shape, so it can look worse than it is" },
                "f_lo_ghz": { "type": "number" }, "f_hi_ghz": { "type": "number" },
                "chip": { "type": "string", "description": "A chip id from antenna_references" },
                "chip_z": { "type": "array", "items": { "type": "number" }, "description": "[R, X] series at 915 MHz, X negative, if it's not in the table" },
                "read_dbm": { "type": "number", "description": "Chip sensitivity, if not from the table" },
                "eirp_dbm": { "type": "number", "description": "Reader power with antenna gain, 36 by default" },
                "max_mm": { "type": "array", "items": { "type": "number" }, "description": "Footprint [x, y]" },
                "min_feature_um": { "type": "number", "description": "Thinnest trace and gap the process makes, 200 by default" },
                "metal": { "type": "string", "description": "Aluminium by default" }
            })), "required": ["title", "idea", "parts", "feeds"] }),
        ),
        def(
            "antenna_tag_design",
            "Search the nine known tag shapes against the chip, band, footprint and surroundings, in the background, judged by read range. It only resizes them: nothing new comes out of it, so it isn't how to design a tag. Use antenna_tag_try for that, and this for the numbers to beat. Read it with antenna_wait then antenna_tag_options; what's already been tried for the same requirements stays on the list. vary adds the current drawing or template as one more candidate: [{name, min, max}] in SI. evolve also lays out metal cell by cell with no assumed shape: {nx, ny, solves}. effort: quick (about 60 solves a structure), normal (240), deep (900), exhaustive (3000); each solve takes under a second. Band, chip, footprint and surroundings carry over from the last search if the band is left out.",
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
            })) }),
        ),
        def(
            "antenna_tag_options",
            "Everything tried so far, best first: each with where it came from, its structure, why it's different, range, power into the chip, gain, size, the band it really covers, what limits it, what it does if its surroundings change, and what it stands out for. Also the known structures, and the last retune. only gives one in full, by title, with the others as a line each.",
            json!({ "type": "object", "properties": { "only": { "type": "string", "description": "A title" } } }),
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

/// Where a design's range goes, in the terms the next change is decided in.
fn limits_json(o: &TagOption, goal: &TagGoal) -> Value {
    let l = tag_design::limits(o, goal);
    json!({
        "biggest": l.biggest,
        "needed_z": [r2(l.needed_z.0), r2(l.needed_z.1)],
        "reactance_short_ohm": r2(l.reactance_short_ohm),
        "db_lost": { "mismatch_at_centre": r2(l.mismatch_centre_db), "mismatch_at_band_worst": r2(l.mismatch_worst_db), "in_metal_and_layers": r2(l.loss_db), "directivity_below_a_dipole": r2(l.directivity_short_db) },
        "range_if_perfectly_matched_m": r2(l.range_if_matched_m),
        "range_if_also_lossless_m": r2(l.range_if_lossless_m),
    })
}

fn option_json(o: &TagOption, goal: Option<&TagGoal>) -> Value {
    let s = &o.score;
    let mut out = json!({
        "id": o.id,
        "origin": o.origin.id(),
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
    });
    if let Some(g) = goal {
        out["what_limits_it"] = limits_json(o, g);
    }
    if !o.pinned.is_empty() {
        out["ran_out_of_room"] = json!(o.pinned.iter().map(|(n, top)| format!("{n} at the {} of its range", if *top { "top" } else { "bottom" })).collect::<Vec<_>>());
    }
    if !o.effects.is_empty() {
        out["each_dimension"] = json!(o.effects.iter().map(|e| {
            let count = tag_tune::is_count(&e.var) || e.step.abs() >= 0.5;
            let step = if count { json!({ "step": r2(e.step) }) } else { json!({ "step_mm": r2(e.step * 1e3) }) };
            json!({ "variable": e.var, "change": step, "resistance_ohm": r2(e.d_resistance), "reactance_ohm": r2(e.d_reactance), "range_m": r2(e.d_range_m) })
        }).collect::<Vec<_>>());
    }
    out
}

const EVIDENCE: &str = "Simulated, not measured: a forward-link estimate, aligned, with layers as wide as the world and a surface wave counted as loss. Measure before committing to a build.";

fn options_tool(world: &World, i: &Value) -> ToolOutput {
    let st = world.resource::<Studio>();
    let run = st.tag_run().unwrap_or_default();
    // One in full and the rest as a line each: every round goes back to the model on every call after
    // it, so a round can't carry the whole list.
    if let Some(title) = i["only"].as_str() {
        let Some(tried) = run.options.iter().find(|o| o.title == title) else {
            let titles: Vec<&str> = run.options.iter().map(|o| o.title.as_str()).collect();
            return ToolOutput::err(format!("Nothing called '{title}' has a result. There's {titles:?}. If that was just tried, no version of it built inside its range, so look at its footprint and its vary."));
        };
        let standings: Vec<Value> = run.options.iter().map(|o| json!({ "id": o.id, "origin": o.origin.id(), "structure": o.title, "worst_range_m": r2(o.score.worst_range_m), "worst_power_transfer": r2(o.score.worst_transfer), "size_mm": [r2(o.score.size_x * 1e3), r2(o.score.size_y * 1e3)] })).collect();
        let result = json!({ "tried": option_json(tried, run.goal.as_ref()), "standings": standings, "surroundings": run.goal.as_ref().map(|g| g.stack.describe()), "evidence": EVIDENCE });
        // As lines rather than inside the JSON, where they'd lose their columns.
        return ToolOutput::text(if tried.map.is_empty() { result.to_string() } else { format!("{result}\n\nCurrent map. {}", tried.map) });
    }
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
        "options": run.options.iter().map(|o| option_json(o, run.goal.as_ref())).collect::<Vec<_>>(),
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

/// The last search's goal with whatever this call changes. For carrying on with the same job: a call
/// that names a band is a new job, and starts from what it says.
fn goal_carried(mut g: TagGoal, i: &Value) -> Result<TagGoal, String> {
    if !i["chip"].is_null() || !i["chip_z"].is_null() {
        (g.chip_z, g.read_dbm) = chip_of(i)?;
    } else if let Some(r) = i["read_dbm"].as_f64() {
        g.read_dbm = r;
    }
    if let Some(s) = stack_of(i)? {
        g.stack = s;
    }
    if let Some((x, y)) = i["max_mm"].as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_f64()? * 1e-3, a[1].as_f64()? * 1e-3))) {
        (g.max_x, g.max_y) = (Some(x), Some(y));
    }
    if let Some(m) = i["min_feature_um"].as_f64() {
        g.min_feature = m * 1e-6;
    }
    if let Some(e) = i["eirp_dbm"].as_f64() {
        g.eirp_dbm = e;
    }
    if let Some(m) = i["metal"].as_str() {
        if !antenna_core::material::by_name(m).is_some_and(|x| x.is_conductor()) {
            return Err(format!("'{m}' isn't a metal in the material library."));
        }
        g.metal = m.to_string();
    }
    Ok(g)
}

fn goal_from(world: &World, i: &Value) -> Result<TagGoal, String> {
    if i["f_lo_ghz"].is_null() && i["f_hi_ghz"].is_null() {
        let Some(last) = world.resource::<Studio>().tag_run().and_then(|t| t.goal) else { return Err("give f_lo_ghz and f_hi_ghz, the chip, max_mm and the surroundings: nothing has been searched yet to carry them over from".into()) };
        return goal_carried(last, i);
    }
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
    let mut vary_given = i["vary"].as_array().filter(|a| !a.is_empty()).cloned();
    // A try names what it drew instead: each moves either way, down to what the process can make.
    if vary_given.is_none()
        && let Some(names) = i["vary_names"].as_array().filter(|a| !a.is_empty())
    {
        let known = world.resource::<Studio>().project.vars().unwrap_or_default();
        vary_given = Some(names.iter().filter_map(Value::as_str).filter_map(|n| {
            let v = *known.get(n)?;
            (v > 0.0).then(|| json!({ "name": n, "min": (0.6 * v).max(if tag_tune::is_count(n) { 0.0 } else { goal.min_feature.min(v) }), "max": 1.4 * v }))
        }).collect());
    }
    if let Some(vary) = vary_given.filter(|a| !a.is_empty()) {
        // A drawing started from nothing keeps the frequency of whatever was open before it, which
        // read 2450 MHz on a 915 MHz tag. Anything drawn from f0 should follow the band too.
        {
            let mut st = world.resource_mut::<Studio>();
            if st.project.template == "custom"
                && let Some(row) = st.project.params.iter_mut().find(|r| r.name == "f0")
            {
                row.expr = format!("{}MHz", goal.fc() / 1e6);
            }
        }
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
        // A template with its sizes searched is still that template, whoever asked for it.
        let origin = if own.template == "custom" { Origin::Drawn } else { Origin::Known };
        extra.push(Candidate { origin, title: i["title"].as_str().unwrap_or("Your own design").to_string(), idea: i["idea"].as_str().unwrap_or("Drawn for this job.").to_string(), good_for: "What it was drawn for".to_string(), project: own, vars });
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

/// The metal seen from above, one outline for each piece, in metres.
fn flat_outlines(p: &antenna_core::project::Project) -> Option<Vec<Vec<[f64; 2]>>> {
    use antenna_core::geometry::Shape;
    let g = p.scatterer().ok()?;
    let mut shapes: Vec<Vec<[f64; 2]>> = Vec::new();
    for s in &g.solids {
        match &s.shape {
            Shape::Polygon { axis: 2, pts, .. } => shapes.push(pts.clone()),
            Shape::Box { min, max } => shapes.push(vec![[min[0], min[1]], [max[0], min[1]], [max[0], max[1]], [min[0], max[1]]]),
            _ => {}
        }
    }
    Some(shapes)
}

/// Calls `put` for each cell of a `w` by `h` grid inside an outline given in cells.
pub(super) fn fill_outline(points: &[(f64, f64)], w: u32, h: u32, mut put: impl FnMut(u32, u32)) {
    let (top, bottom) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q.1), b.max(q.1)));
    for r in (top.floor().max(0.0) as u32)..=(bottom.ceil().min(h as f64 - 1.0) as u32) {
        let centre = r as f64 + 0.5;
        let mut crossings: Vec<f64> = (0..points.len())
            .filter_map(|k| {
                let (a, b) = (points[k], points[(k + 1) % points.len()]);
                ((a.1 <= centre) != (b.1 <= centre)).then(|| a.0 + (centre - a.1) / (b.1 - a.1) * (b.0 - a.0))
            })
            .collect();
        crossings.sort_by(f64::total_cmp);
        for pair in crossings.chunks_exact(2) {
            for c in (pair[0].floor().max(0.0) as u32)..=(pair[1].ceil().min(w as f64 - 1.0) as u32) {
                put(c, r);
            }
        }
    }
}

/// How much of the metal lands on metal when it's folded over the feed, left onto right and top onto
/// bottom, and how far the feed is from the middle. A model's own eye says "matches" of a sketch it
/// drew lopsided, so this is measured: an antenna drawn with mirror reads 100.
fn symmetry(p: &antenna_core::project::Project) -> Option<Value> {
    let shapes = flat_outlines(p)?;
    let every = || shapes.iter().flatten();
    let (x0, x1) = every().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q[0]), b.max(q[0])));
    let (y0, y1) = every().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q[1]), b.max(q[1])));
    if !(x1 > x0 && y1 > y0) {
        return None;
    }
    let middle = [(x0 + x1) / 2.0, (y0 + y1) / 2.0];
    let at = p.geometry().ok().and_then(|g| g.ports.first().map(|port| port.center())).map_or(middle, |c| [c[0], c[1]]);
    // Wide enough to reach the far end either way from the feed, so that a fold stays on the grid.
    let half = [(x1 - at[0]).max(at[0] - x0), (y1 - at[1]).max(at[1] - y0)];
    let cell = half[0].max(half[1]) / 300.0;
    let (w, h) = ((2.0 * half[0] / cell).ceil().max(1.0) as u32, (2.0 * half[1] / cell).ceil().max(1.0) as u32);
    // The folded outlines are filled, rather than the filled cells folded, so that what's really
    // symmetric covers exactly the same cells.
    let fill = |sx: f64, sy: f64| {
        let mut cells = vec![false; (w * h) as usize];
        for shape in &shapes {
            let points: Vec<(f64, f64)> = shape.iter().map(|q| ((sx * (q[0] - at[0]) + half[0]) / cell, (sy * (q[1] - at[1]) + half[1]) / cell)).collect();
            fill_outline(&points, w, h, |c, r| cells[(r * w + c) as usize] = true);
        }
        cells
    };
    let metal = fill(1.0, 1.0);
    let overlap = |folded: Vec<bool>| {
        let both = metal.iter().zip(&folded).filter(|(a, b)| **a && **b).count();
        let either = metal.iter().zip(&folded).filter(|(a, b)| **a || **b).count();
        (100.0 * both as f64 / either.max(1) as f64).round()
    };
    Some(json!({
        "left_on_right_pct": overlap(fill(-1.0, 1.0)),
        "top_on_bottom_pct": overlap(fill(1.0, -1.0)),
        "feed_from_middle_mm": [r2((at[0] - middle[0]) * 1e3), r2((at[1] - middle[1]) * 1e3)],
    }))
}

/// What's been drawn, seen from above: without it a model draws blind, and it drew a sketch's closed
/// loops as open arms and never knew. PNG, with how far apart the grid lines are, in millimetres.
fn top_view(p: &antenna_core::project::Project) -> Option<(std::sync::Arc<Vec<u8>>, f64)> {
    let shapes = flat_outlines(p)?;
    let every = || shapes.iter().flatten();
    let (x0, x1) = every().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q[0]), b.max(q[0])));
    let (y0, y1) = every().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q[1]), b.max(q[1])));
    if !(x1 > x0 || y1 > y0) {
        return None;
    }
    const MARGIN: f64 = 24.0;
    // Big enough to see a trace and small enough not to weigh on every later call to the model.
    let scale = (1100.0 / (x1 - x0).max(1e-6)).min(700.0 / (y1 - y0).max(1e-6)).min(40_000.0);
    let (w, h) = (((x1 - x0) * scale + 2.0 * MARGIN) as u32, ((y1 - y0) * scale + 2.0 * MARGIN) as u32);
    let px = |x: f64, y: f64| (MARGIN + (x - x0) * scale, h as f64 - MARGIN - (y - y0) * scale);
    let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]));
    // Grid lines at least 30 pixels apart, on round numbers of millimetres.
    let grid_mm = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0].into_iter().find(|m| m * 1e-3 * scale >= 30.0).unwrap_or(1000.0);
    let step = grid_mm * 1e-3;
    let lines = |lo: f64, hi: f64| {
        let first = (lo / step).floor() as i64 - 1;
        let last = (hi / step).ceil() as i64 + 1;
        (first..=last).map(|k| (k, k as f64 * step)).collect::<Vec<_>>()
    };
    for (k, x) in lines(x0, x1) {
        let c = (px(x, y0).0.round()) as i64;
        if (0..w as i64).contains(&c) {
            let shade = if k == 0 { 120 } else { 222 };
            (0..h).for_each(|r| img.put_pixel(c as u32, r, image::Rgb([shade, shade, shade])));
        }
    }
    for (k, y) in lines(y0, y1) {
        let r = (px(x0, y).1.round()) as i64;
        if (0..h as i64).contains(&r) {
            let shade = if k == 0 { 120 } else { 222 };
            (0..w).for_each(|c| img.put_pixel(c, r as u32, image::Rgb([shade, shade, shade])));
        }
    }
    // Each outline filled across every row of pixels it spans.
    for shape in &shapes {
        let points: Vec<(f64, f64)> = shape.iter().map(|q| px(q[0], q[1])).collect();
        fill_outline(&points, w, h, |c, r| img.put_pixel(c, r, image::Rgb([38, 70, 130])));
    }
    for port in p.geometry().map_or(Vec::new(), |g| g.ports) {
        let c = port.center();
        let (cx, cy) = px(c[0], c[1]);
        for r in (cy as i64 - 5)..=(cy as i64 + 5) {
            for col in (cx as i64 - 5)..=(cx as i64 + 5) {
                if (col as f64 - cx).hypot(r as f64 - cy) <= 5.0 && (0..w as i64).contains(&col) && (0..h as i64).contains(&r) {
                    img.put_pixel(col as u32, r as u32, image::Rgb([220, 30, 30]));
                }
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some((std::sync::Arc::new(out.into_inner()), grid_mm))
}

fn draw_tool(world: &mut World, i: &Value) -> ToolOutput {
    // A new drawing in a chat with a picture nobody has looked at properly. One refused call is cheap
    // against a whole run on a mirror image.
    if i["fresh"].as_bool().unwrap_or(false) || world.resource::<Studio>().project.template != "custom" {
        if let Some(id) = super::picture::unread(world) {
            return ToolOutput::err(format!("This chat has a picture, {id}, and nothing was drawn. If this drawing comes from it, call antenna_sketch_view first to turn it the right way up and crop it to the antenna, then read the antenna from that view. A sideways photo read as it stood came out as its mirror image. If the picture has nothing to do with this drawing, send the same call again."));
        }
    }
    if let Some(said) = super::picture::wire_for_an_outline(world, i) {
        return ToolOutput::err(said);
    }
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
    // Refused here, at the start, not noticed after a run: all that's lost is this one call, and it
    // says everything wrong with the drawing at once. Only this project's copy has been touched.
    let loose = freeform::loose_numbers(i);
    let drawn = freeform::draw(&mut project, i);
    if !loose.is_empty() {
        let also = drawn.err().map_or(String::new(), |e| format!("\nAnd: {e:#}"));
        return ToolOutput::err(format!("{}{also}", freeform::loose_message(&loose)));
    }
    match drawn {
        Ok(d) => {
            let design = json!({
                "parts": project.extra.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
                "feeds": project.extra_ports.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
                "variables": project.params.iter().filter(|r| !r.derived).map(|r| json!([r.name, r.expr, r.note])).collect::<Vec<_>>(),
            });
            let (size, solver, warn) = match project.geometry() {
                Ok(g) => {
                    let (lo, hi) = g.bounds();
                    (json!([r2((hi[0] - lo[0]) * 1e3), r2((hi[1] - lo[1]) * 1e3), r2((hi[2] - lo[2]) * 1e3)]), project.solver_for(&g).label().to_string(), Value::Null)
                }
                Err(e) => (Value::Null, String::new(), json!(format!("{e:#}"))),
            };
            let picture = top_view(&project);
            let folded = symmetry(&project);
            let outlines = flat_outlines(&project).unwrap_or_default();
            st.project = project;
            st.clear_latest();
            st.ui.page = super::ui::Page::Draw;
            let added = json!({ "parts": d.parts, "feeds": d.feeds, "variables": d.vars, "replaced": d.replaced });
            drop(st);
            world.resource_mut::<crate::ui::LeftPanelSlot>().studio_open = true;
            let sketch = super::picture::current(world);
            let mut out = json!({ "ok": true, "added": added, "design": design, "size_mm": size, "solver": solver, "not_ready": warn });
            if let Some(said) = sketch.as_ref().and_then(|s| super::picture::mirror_hint(s, &outlines)) {
                out["mirror_warning"] = json!(said);
            }
            if let Some(folded) = folded {
                let lopsided = folded["left_on_right_pct"].as_f64().is_some_and(|v| v < 90.0) && folded["top_on_bottom_pct"].as_f64().is_some_and(|v| v < 90.0);
                if sketch.is_some() && lopsided {
                    out["symmetry_note"] = json!("This drawing isn't symmetric about its feed. If both sides of the sketch's port make the same turns, the sketch is of a symmetric antenna and the difference is the hand: draw one side and use mirror.");
                }
                out["symmetry"] = folded;
            }
            if !d.unused.is_empty() {
                out["unused_variables"] = json!(d.unused);
                out["unused_note"] = json!("Nothing in the drawing uses these, so changing them does nothing. Write a part over them, or say why they're there.");
            }
            let mut content = Vec::new();
            if let Some((data, grid_mm)) = picture {
                let both = sketch.and_then(|s| super::picture::beside(&s, &data)).map(std::sync::Arc::new);
                let about = format!("x across, y up, pale lines every {grid_mm} mm, darker lines through the origin, the feed in red.");
                if let Some(both) = both {
                    out["picture"] = json!(format!("The sketch on top, as you turned it, and your drawing under it, the same way up: {about} What's in a corner of one belongs in that corner of the other, and a long run along the top of the sketch is a long run along the top of the drawing. Go through them feature by feature, and if the top and bottom or the left and right are swapped, it's a mirror image: redraw it."));
                    let shown = crate::agent::images::put(both.clone(), "image/jpeg");
                    if let Some(mut agent) = world.get_resource_mut::<crate::agent::Agent>() {
                        agent.transcript.push(crate::agent::ChatItem::Image(shown));
                    }
                    content.push(Block::Image { data: both, mime: "image/jpeg".into() });
                } else {
                    out["picture"] = json!(format!("Top view below: {about}"));
                    content.push(Block::Image { data, mime: "image/png".into() });
                }
            }
            content.insert(0, Block::Text(out.to_string()));
            ToolOutput { content, is_error: false }
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
    let body = option_json(option, run.goal.as_ref());
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

/// One round of designing: draws the structure, searches it and reads the result, in one call, so a
/// round costs one trip to the model. Each step goes through the bridge, and so through the gate, as if
/// it had been asked for itself. Nothing is searched if the drawing doesn't build.
pub fn try_structure(tx: &Sender<ToolJob>, i: &Value, ctx: CallCtx) -> ToolOutput {
    let Some(title) = i["title"].as_str().map(str::trim).filter(|t| !t.is_empty()) else { return ToolOutput::err("Give it a title.") };
    if i["idea"].as_str().is_none_or(|t| t.trim().is_empty()) {
        return ToolOutput::err("Say what the idea is: what's different about this structure, and what it should do better.");
    }
    if FAMILIES.iter().any(|f| f.title.eq_ignore_ascii_case(title)) {
        return ToolOutput::err(format!("'{title}' is one of the known shapes, which antenna_tag_design searches. Try something else, under its own title."));
    }
    // With nothing to vary, the search would quietly run the known shapes instead of this one.
    if i["vars"].as_object().is_none_or(|v| v.is_empty()) && i["vary"].as_array().is_none_or(|v| v.is_empty()) {
        return ToolOutput::err("Name its dimensions in vars and write the parts over them, so they can be searched.");
    }
    let mut draw = json!({ "fresh": true, "name": title, "parts": i["parts"], "feeds": i["feeds"] });
    for key in ["vars", "notes", "metal"] {
        if !i[key].is_null() {
            draw[key] = i[key].clone();
        }
    }
    let drawn = tools::call_ctx(tx, "antenna_draw", draw, ctx.clone());
    if drawn.is_error {
        return drawn;
    }
    let mut search = json!({ "only_vary": true, "title": title, "idea": i["idea"], "effort": i["effort"].as_str().unwrap_or("normal") });
    for key in ["f_lo_ghz", "f_hi_ghz", "chip", "chip_z", "read_dbm", "eirp_dbm", "max_mm", "min_feature_um", "metal", "preset", "above", "below", "metal_behind", "vary"] {
        if !i[key].is_null() {
            search[key] = i[key].clone();
        }
    }
    if let Some(vars) = i["vars"].as_object() {
        // A dimension written over another follows it. Searching it by itself would write a number over
        // the expression, and the design that came back would have lost the link.
        let free = vars.iter().filter(|(_, v)| !v.as_str().and_then(|s| antenna_core::expr::names(s).ok()).is_some_and(|used| used.iter().any(|n| vars.contains_key(n)))).map(|(name, _)| name);
        search["vary_names"] = json!(free.collect::<Vec<_>>());
    }
    let started = tools::call_ctx(tx, "antenna_tag_design", search, ctx.clone());
    if started.is_error {
        return started;
    }
    let state = assist::wait(tx, &json!({}), ctx.cancel.as_deref());
    if state.is_error {
        return state;
    }
    let said: Value = serde_json::from_str(&state.summary()).unwrap_or(Value::Null);
    match said["state"].as_str() {
        Some("failed") => return ToolOutput::err(format!("The search of '{title}' failed: {}", said["error"].as_str().unwrap_or("no reason given"))),
        Some("running") => return text(json!({ "state": "running", "note": format!("Still searching. Call antenna_wait, then antenna_tag_options with only: \"{title}\".") })),
        _ => {}
    }
    tools::call_ctx(tx, "antenna_tag_options", json!({ "only": title }), ctx)
}

/// Handles this module's tools; `None` for anything else.
pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        "antenna_stack" => stack_tool(world, i),
        "antenna_draw" => draw_tool(world, i),
        "antenna_sketch_view" => super::picture::view_tool(world, i),
        "antenna_tag_design" => design_tool(world, i),
        "antenna_tag_options" => options_tool(world, i),
        // On this thread it would stop the app for as long as the search takes.
        TRY => ToolOutput::err("antenna_tag_try runs through the tool bridge."),
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
        // Just the JSON: a current map can follow it.
        if out.is_error { Err(out.summary()) } else { Ok(serde_json::Deserializer::from_str(&out.summary()).into_iter::<Value>().next().and_then(Result::ok).unwrap_or(Value::Null)) }
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
        let own = call(&mut w, "antenna_tag_options", json!({ "only": "Hairpin dipole" })).unwrap();
        assert_eq!(own["tried"]["structure"], "Hairpin dipole", "{own}");
        assert_eq!(own["tried"]["origin"], "drawn", "{own}");
        assert!(own["tried"]["what_limits_it"]["biggest"].is_string(), "{own}");
        assert!(own["tried"]["each_dimension"].as_array().is_some_and(|a| a.len() == 3), "{own}");
        assert_eq!(own["standings"].as_array().unwrap().len(), 4, "what was tried before stays on the list, for the same requirements: {own}");
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

    /// A picture to draw from, as the chat would hold it.
    fn a_picture() -> String {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(40, 80, image::Rgb([200, 200, 200]))).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)).unwrap();
        crate::agent::images::put(std::sync::Arc::new(out.into_inner()), "image/jpeg")
    }

    fn the_picture_in(out: &ToolOutput) -> (String, (u32, u32)) {
        let Some(Block::Image { data, mime }) = out.content.iter().find(|b| matches!(b, Block::Image { .. })) else { panic!("no picture: {}", out.summary()) };
        (mime.clone(), image::load_from_memory(data).unwrap().to_rgb8().dimensions())
    }

    /// The real model's check was one sideways picture and one upright one, and a mirror image got past
    /// it. Now they arrive in one picture, the same way up, and how lopsided it is comes as a number.
    #[test]
    fn a_drawing_from_a_picture_comes_back_under_it_with_how_symmetric_it_is() {
        let mut w = world();
        let id = a_picture();
        // Turned, 40 by 80 is 80 by 40: at the top view's width, that's half as tall as it's wide.
        let view = crate::antenna::tools::start(&mut w, "antenna_sketch_view", &json!({ "image_id": id, "turn": 90 })).unwrap();
        assert!(!view.is_error, "{}", view.summary());
        assert_eq!(the_picture_in(&view).1, (320, 160), "enlarged as far as it goes");

        let vars = json!({ "a": "40mm", "b": "30mm", "t": "10mm", "w": "2mm", "gp": "2mm" });
        let feeds = json!([{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]);
        let lopsided = crate::antenna::tools::start(&mut w, "antenna_draw", &json!({ "fresh": true, "vars": vars, "feeds": feeds, "parts": [
            { "kind": "strip", "name": "right", "path": [["gp/2", "0"], ["a", "0"], ["a", "t"]], "w": "w" },
            { "kind": "strip", "name": "left", "path": [["-gp/2", "0"], ["-b", "0"], ["-b", "-t"]], "w": "w" }
        ] })).unwrap();
        assert!(!lopsided.is_error, "{}", lopsided.summary());
        let said: Value = serde_json::Deserializer::from_str(&lopsided.summary()).into_iter().next().unwrap().unwrap();
        assert!(said["symmetry"]["left_on_right_pct"].as_f64().unwrap() < 60.0, "{said}");
        assert_eq!(said["symmetry"]["feed_from_middle_mm"][0], json!(-5.0), "{said}");
        assert!(said["symmetry_note"].as_str().unwrap().contains("mirror"), "{said}");
        assert!(said["picture"].as_str().unwrap().contains("same way up"), "{said}");
        let (mime, (wide, tall)) = the_picture_in(&lopsided);
        assert_eq!(mime, "image/jpeg");
        assert!(tall > wide / 2, "half as tall for the sketch, and the drawing under it: {wide} x {tall}");

        let even = call(&mut w, "antenna_draw", json!({ "fresh": true, "vars": vars, "feeds": feeds, "parts": [
            { "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["a", "0"], ["a", "t"]], "w": "w", "mirror": "x" }
        ] })).unwrap();
        assert_eq!(even["symmetry"]["left_on_right_pct"], json!(100.0), "{even}");
        assert_eq!(even["symmetry"]["feed_from_middle_mm"][0], json!(0.0), "{even}");
        assert!(even.get("symmetry_note").is_none(), "{even}");
    }

    /// A real run drew a user's filled tag as 1 mm wires along its outline, and they had to say so.
    #[test]
    fn a_closed_path_from_a_sketch_is_asked_about_once_and_labels_keep_their_names() {
        let wire = json!({ "fresh": true, "vars": { "A1": "20mm", "A2": "10mm", "w": "1mm" },
            "parts": [{ "kind": "strip", "name": "ring", "w": "w", "path": [["0", "0"], ["A1", "0"], ["A1", "A2"], ["0", "A2"], ["0", "0"]] }] });
        // With nothing to draw from, a loop is a loop.
        let mut w = world();
        assert!(call(&mut w, "antenna_draw", wire.clone()).is_ok());

        let mut w = world();
        call(&mut w, "antenna_sketch_view", json!({ "image_id": a_picture() })).unwrap();
        let e = call(&mut w, "antenna_draw", wire.clone()).unwrap_err();
        assert!(e.contains("ring") && e.contains("polygon") && e.contains("Nothing was drawn"), "{e}");
        assert_eq!(w.resource::<Studio>().project.template, "patch_probe", "the design wasn't touched");
        // It may really be a wire, and it's been told.
        assert!(call(&mut w, "antenna_draw", wire.clone()).is_ok());

        // Filled, over the sketch's own names, in lengths that add up.
        let filled = call(&mut w, "antenna_draw", json!({ "fresh": true, "vars": { "A1": "20mm", "A2": "10mm", "B1": "5mm", "a1": "1mm" },
            "parts": [{ "kind": "polygon", "name": "half", "mirror": "x", "pts": [["a1", "0"], ["a1+A1", "0"], ["a1+A1+A2", "B1"], ["a1", "B1"]] }],
            "feeds": [{ "a": ["-a1", "0"], "b": ["a1", "0"], "width": "a1" }] })).unwrap();
        assert_eq!(filled["size_mm"][0], json!(62.0), "{filled}");
        let names: Vec<&str> = filled["design"]["variables"].as_array().unwrap().iter().filter_map(|v| v[0].as_str()).collect();
        for name in ["A1", "A2", "B1", "a1"] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
        assert!(filled.get("unused_variables").is_none(), "{filled}");
    }

    #[test]
    fn a_new_drawing_waits_once_for_a_picture_in_the_chat_to_be_looked_at() {
        use crate::agent::{Agent, ChatItem};
        let dipole = json!({ "fresh": true, "vars": { "L": "80mm", "w": "1mm", "gp": "1mm" },
            "parts": [{ "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" }],
            "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }] });
        let attach = |w: &mut World| {
            let id = a_picture();
            w.resource_mut::<Agent>().transcript.push(ChatItem::User { text: "draw this".into(), images: vec![id.clone()] });
            id
        };
        let mut w = world();
        w.insert_resource(Agent::new(Default::default()));
        // With no picture there's nothing to wait for.
        assert!(call(&mut w, "antenna_draw", dipole.clone()).is_ok());

        attach(&mut w);
        let e = call(&mut w, "antenna_draw", dipole.clone()).unwrap_err();
        assert!(e.contains("antenna_sketch_view") && e.contains("nothing was drawn"), "{e}");
        // It may have nothing to do with the picture, and it's been told.
        assert!(call(&mut w, "antenna_draw", dipole.clone()).is_ok());

        let id = attach(&mut w);
        assert!(call(&mut w, "antenna_draw", dipole.clone()).is_err(), "another picture, another reminder");
        call(&mut w, "antenna_sketch_view", json!({ "image_id": id })).unwrap();
        let drawn = crate::antenna::tools::start(&mut w, "antenna_draw", &dipole).unwrap();
        assert_eq!(the_picture_in(&drawn).0, "image/jpeg", "under the view");

        // A new chat has nothing to do with that sketch.
        w.resource_mut::<Agent>().transcript.clear();
        let drawn = crate::antenna::tools::start(&mut w, "antenna_draw", &dipole).unwrap();
        assert_eq!(the_picture_in(&drawn).0, "image/png", "only the drawing");
    }

    /// What the model checks its drawing against, so what's in it has to be where the metal is.
    #[test]
    fn the_top_view_shows_the_metal_where_it_is_and_the_feed_in_red() {
        let mut w = world();
        let out = crate::antenna::tools::start(&mut w, "antenna_draw", &json!({
            "fresh": true, "vars": { "L": "80mm", "t": "20mm", "w": "2mm", "gp": "2mm" },
            "parts": [
                { "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "0"], ["L/2", "t"]], "w": "w", "mirror": "x" }
            ],
            "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
        })).unwrap();
        assert!(!out.is_error, "{}", out.summary());
        let Some(Block::Image { data, mime }) = out.content.iter().find(|b| matches!(b, Block::Image { .. })) else { panic!("no picture: {}", out.summary()) };
        assert_eq!(mime, "image/png");
        let img = image::load_from_memory(data).unwrap().to_rgb8();
        let (wide, tall) = img.dimensions();
        assert!(wide > tall, "80 mm across and about 20 up: {wide} x {tall}");
        let dark = |x: f64, y: f64| {
            // From the model: 82 mm across its outer edges, 21 mm up from the trace's lower edge.
            let (mx, my) = (24.0 + (x + 41.0) / 82.0 * (wide as f64 - 48.0), tall as f64 - 24.0 - (y + 1.0) / 22.0 * (tall as f64 - 48.0));
            let p = img.get_pixel(mx as u32, my as u32).0;
            p[0] < 80 && p[2] > p[0]
        };
        assert!(dark(20.0, 0.0), "the right arm");
        assert!(dark(-20.0, 0.0), "the mirrored left arm");
        assert!(dark(40.0, 10.0), "the upright at its end");
        assert!(!dark(20.0, 10.0), "nothing in the middle of the frame");
        assert!(img.pixels().any(|p| p.0 == [220, 30, 30]), "the feed");
    }

    /// Plain figures are what a real model drew from a sketch with its dimensions labelled, and nobody
    /// could change them afterwards. They don't get as far as the design.
    #[test]
    fn a_drawing_in_plain_numbers_is_refused_and_a_redraw_over_names_can_be_changed_by_name() {
        let mut w = world();
        let before = w.resource::<Studio>().project.clone();
        let refused = crate::antenna::tools::start(&mut w, "antenna_draw", &json!({ "fresh": true, "parts": [{ "kind": "strip", "name": "arm", "path": [["1mm", "0"], ["40mm", "0"]], "w": "1mm", "mirror": "x" }], "feeds": [{ "a": ["-1mm", "0"], "b": ["1mm", "0"], "width": "1mm" }] })).unwrap();
        assert!(refused.is_error, "{}", refused.summary());
        let said = refused.summary();
        assert!(said.contains("40mm at arm.path[1][0]") && said.contains("1mm at arm.path[0][0]") && said.contains("vars"), "{said}");
        assert_eq!(w.resource::<Studio>().project, before, "nothing kept");
        // Whatever else is wrong comes in the same answer: each refusal costs a call to the model.
        let both = crate::antenna::tools::start(&mut w, "antenna_draw", &json!({ "parts": [{ "kind": "rect", "name": "r", "a": ["3mm", "nope"], "b": ["0", "0"] }] })).unwrap();
        assert!(both.is_error && both.summary().contains("3mm") && both.summary().contains("nope"), "{}", both.summary());

        let drawn = call(&mut w, "antenna_draw", json!({
            "fresh": true, "vars": { "L": "80mm", "w": "1mm", "gp": "1mm", "spare": "2mm" }, "notes": { "L": "Tip to tip" },
            "parts": [{ "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2", "0"]], "w": "w", "mirror": "x" }],
            "feeds": [{ "a": ["-gp/2", "0"], "b": ["gp/2", "0"], "width": "w" }]
        })).unwrap();
        assert_eq!(drawn["unused_variables"], json!(["spare"]), "{drawn}");
        let row = |d: &Value, name: &str| d["design"]["variables"].as_array().unwrap().iter().find(|r| r[0] == name).cloned().unwrap();
        assert_eq!(row(&drawn, "L")[2], "Tip to tip", "{drawn}");
        assert_eq!(row(&drawn, "w")[2], "Used by arm, arm m, chip", "{drawn}");

        // A later change is one name, and everything written over it follows.
        let span = |w: &World| {
            let (lo, hi) = w.resource::<Studio>().project.geometry().unwrap().bounds();
            hi[0] - lo[0]
        };
        assert!((span(&w) - 0.08).abs() < 2e-3, "{}", span(&w));
        call(&mut w, "antenna_set", json!({ "vars": { "L": "100mm" } })).unwrap();
        assert!((span(&w) - 0.1).abs() < 2e-3, "{}", span(&w));
        // A name nothing uses is said, not quietly added as a row that does nothing.
        let typo = call(&mut w, "antenna_set", json!({ "vars": { "Lx": "100mm" } })).unwrap();
        assert_eq!(typo["changes_nothing"], json!(["Lx"]), "{typo}");
        assert!(!w.resource::<Studio>().project.params.iter().any(|r| r.name == "Lx"), "{typo}");
        // And the assistant can read how it's written, to change one part.
        let read = call(&mut w, "antenna_get", json!({})).unwrap();
        assert_eq!(read["drawing"]["parts"][0]["shape"]["path"][1][0], "L/2", "{read}");
        let changed = call(&mut w, "antenna_draw", json!({ "parts": [{ "kind": "strip", "name": "arm", "path": [["gp/2", "0"], ["L/2-w", "0"]], "w": "w", "mirror": "x" }] })).unwrap();
        assert_eq!(changed["design"]["parts"], json!(["arm", "arm m"]), "replaced, not added to: {changed}");
    }

    /// One written over another follows it: searched by itself it'd come back as a number.
    #[test]
    fn a_try_searches_only_the_dimensions_that_are_free() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let worker = std::thread::spawn(move || {
            let mut asked = Vec::new();
            while let Ok(job) = rx.recv_timeout(std::time::Duration::from_secs(20)) {
                let job: ToolJob = job;
                asked.push((job.name.clone(), job.input.clone()));
                let done = job.name == "antenna_tag_design";
                let _ = job.reply.send(if done { ToolOutput::err("stop here") } else { text(json!({ "ok": true })) });
                if done {
                    break;
                }
            }
            asked
        });
        let out = try_structure(&tx, &json!({ "title": "Mine", "idea": "B", "vars": { "L": "80mm", "t": "L/4", "lam": "lambda0/8" }, "parts": [], "feeds": [] }), CallCtx::default());
        assert!(out.is_error);
        let asked = worker.join().unwrap();
        let search = &asked.iter().find(|(n, _)| n == "antenna_tag_design").unwrap().1;
        let mut names: Vec<&str> = search["vary_names"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        names.sort();
        assert_eq!(names, ["L", "lam"], "{search}");
    }

    /// A try with nothing to vary would search the known shapes instead, and read as a result for it.
    #[test]
    fn a_try_with_no_dimensions_is_refused() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let out = try_structure(&tx, &json!({ "title": "A", "idea": "B", "parts": [{ "kind": "rect" }], "feeds": [] }), CallCtx::default());
        assert!(out.is_error && out.summary().contains("vars"), "{}", out.summary());
        let known = try_structure(&tx, &json!({ "title": "Dogbone", "idea": "B", "vars": { "L": "1mm" } }), CallCtx::default());
        assert!(known.is_error && known.summary().contains("known shapes"), "{}", known.summary());
    }

    /// What the tools' own descriptions show the model has to build: a wrong example would cost every
    /// run its first round.
    #[test]
    fn the_examples_in_the_tool_descriptions_build() {
        for text in [TRY_EXAMPLE, DRAW_EXAMPLE] {
            let mut example: Value = serde_json::from_str(text).unwrap();
            example["fresh"] = json!(true);
            let mut w = world();
            let drawn = call(&mut w, "antenna_draw", example).unwrap();
            assert_eq!(drawn["added"]["feeds"].as_array().unwrap().len(), 1, "{drawn}");
            assert!(drawn["not_ready"].is_null(), "{drawn}");
            assert!(drawn["unused_variables"].is_null(), "{drawn}");
        }
    }

    /// A round carries on with the job in hand, and a call naming a band is a new job.
    #[test]
    fn what_a_tag_has_to_do_carries_over_only_while_the_job_does() {
        let w = world();
        let empty = goal_from(&w, &json!({})).unwrap_err();
        assert!(empty.contains("f_lo_ghz"), "{empty}");

        let first = goal_from(&w, &json!({ "f_lo_ghz": 0.902, "f_hi_ghz": 0.928, "chip": "ucode9", "preset": "paper_label", "max_mm": [50, 30] })).unwrap();
        let shared = w.resource::<Studio>().shared.clone();
        shared.lock().unwrap().tag = Some(TagRun { goal: Some(first.clone()), ..Default::default() });
        assert_eq!(goal_from(&w, &json!({})).unwrap(), first, "nothing said, nothing changes");
        let wider = goal_from(&w, &json!({ "max_mm": [60, 30] })).unwrap();
        assert_eq!(wider.max_x, Some(0.06));
        assert_eq!(wider.stack, first.stack, "the rest stays");
        let other = goal_from(&w, &json!({ "f_lo_ghz": 0.865, "f_hi_ghz": 0.868, "chip": "ucode9" })).unwrap();
        assert_eq!(other.max_x, None, "a new band is a new job");
        assert!(goal_from(&w, &json!({ "metal": "Paper" })).unwrap_err().contains("isn't a metal"), "carrying over doesn't skip the check");
    }

    /// Rounds add up: the list keeps everything for the same goal, a title used again replaces itself.
    #[test]
    fn rounds_of_designing_add_up_and_a_title_used_again_replaces_itself() {
        let goal = TagGoal { max_x: Some(0.095), max_y: Some(0.03), ..tag_design::goal_for(902e6, 928e6, (15.0, -231.0), -24.0) };
        let option = |title: &str, range: f64| TagOption {
            id: "x".into(), origin: Origin::Drawn, title: title.into(), idea: String::new(), good_for: String::new(),
            project: antenna_core::project::Project::from_template("custom", None).unwrap(), values: Vec::new(),
            score: tag_design::TagScore { worst_range_m: range, center_range_m: range, worst_transfer: 0.9, center_transfer: 0.9, gain_dbi: 1.0, efficiency: 0.9, size_x: 0.09, size_y: 0.03, z_center: (15.0, 231.0), unknowns: 100 },
            mesh_shift: 0.0, if_changed: Vec::new(), stands_out: Vec::new(), warnings: Vec::new(), band: None, pinned: Vec::new(), effects: Vec::new(), map: String::new(),
        };
        let one = merged(Vec::new(), vec![option("A", 5.0)], &goal);
        let two = merged(one, vec![option("B", 7.0)], &goal);
        assert_eq!(two.iter().map(|o| (o.id.as_str(), o.title.as_str())).collect::<Vec<_>>(), [("option_1", "B"), ("option_2", "A")]);
        let three = merged(two, vec![option("A", 9.0)], &goal);
        assert_eq!(three.len(), 2, "the same title again is a better version of it, not another option");
        assert_eq!(three[0].title, "A");
        assert_eq!(three[0].score.worst_range_m, 9.0);
    }

    /// The model reads this rather than the numbers beneath it, so what's in it has to be there.
    #[test]
    fn a_tried_structure_is_read_with_what_limits_it() {
        let goal = TagGoal { max_x: Some(0.095), max_y: Some(0.03), ..tag_design::goal_for(902e6, 928e6, (15.0, -231.0), -24.0) };
        let o = TagOption {
            id: "option_1".into(), origin: Origin::Drawn, title: "A".into(), idea: String::new(), good_for: String::new(),
            project: antenna_core::project::Project::from_template("custom", None).unwrap(), values: Vec::new(),
            score: tag_design::TagScore { worst_range_m: 5.0, center_range_m: 6.0, worst_transfer: 0.3, center_transfer: 0.4, gain_dbi: 1.0, efficiency: 0.9, size_x: 0.09, size_y: 0.03, z_center: (15.0, 120.0), unknowns: 100 },
            mesh_shift: 0.0, if_changed: Vec::new(), stands_out: Vec::new(), warnings: Vec::new(), band: None,
            pinned: vec![("L".into(), true)],
            effects: vec![tag_design::Effect { var: "L".into(), step: 0.002, d_resistance: 3.0, d_reactance: 20.0, d_range_m: 0.5 }, tag_design::Effect { var: "n".into(), step: 1.0, d_resistance: 1.0, d_reactance: 40.0, d_range_m: 0.1 }],
            map: "One character
.9F9.".into(),
        };
        let v = option_json(&o, Some(&goal));
        assert_eq!(v["origin"], "drawn");
        assert!(v["what_limits_it"]["biggest"].as_str().unwrap().starts_with("Reactance"), "{v}");
        assert_eq!(v["ran_out_of_room"][0], "L at the top of its range");
        assert_eq!(v["each_dimension"][0]["change"]["step_mm"], 2.0, "{v}");
        assert_eq!(v["each_dimension"][1]["change"]["step"], 1.0, "a count isn't a length: {v}");
        // With nothing to say, and no goal to read it against, none of that is there at all.
        let known = TagOption { origin: Origin::Known, pinned: Vec::new(), effects: Vec::new(), map: String::new(), ..o };
        let k = option_json(&known, None);
        assert_eq!(k["origin"], "known_shape");
        assert!(k.get("what_limits_it").is_none() && k.get("each_dimension").is_none() && k.get("ran_out_of_room").is_none(), "{k}");
    }

    #[test]
    fn a_tool_is_gated_like_the_rest_of_its_kind() {
        use crate::agent::gate::{Class, class_of};
        assert_eq!(class_of("antenna_tag_options", &json!({})), Class::Read);
        assert_eq!(class_of("antenna_tag_tune", &json!({ "f_mhz": 902, "only_read": true })), Class::Read);
        assert_eq!(class_of("antenna_tag_tune", &json!({ "only_read": true, "apply": true })), Class::Project);
        for name in ["antenna_stack", "antenna_draw", TRY, "antenna_tag_design", "antenna_tag_open", "antenna_tag_retune", "antenna_tag_tune"] {
            assert_eq!(class_of(name, &json!({})), Class::Project, "{name}");
        }
    }
}
