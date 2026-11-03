//! What must hold before the assistant may stop. Each check watches tool results; when the model
//! wants to finish, it either lets it or sends it back to work with the reason.

use serde_json::json;

use super::gate::CallCtx;
use super::harness::{LogEntry, OnStop, StopChecks, Turn, Who};
use super::llm::{Block, Provider};
use super::{Event, photocheck, toolmaker, tools};

const MAX_CHECKS: usize = 4;
const MAX_RTLS_CHECKS: usize = 3;
const MAX_PLANT_CHECKS: usize = 3;
const MAX_ANTENNA_CHECKS: usize = 4;
/// How many structures of its own the assistant has to try before a new tag counts as designed.
const MIN_TAG_TRIES: usize = 3;
const MAX_PLAN_CHECKS: usize = 4;

const MUTATING: [&str; 15] = [
    "set_object_material",
    "add_photo_backdrop",
    "load_template",
    "place_objects",
    "place_from_photo",
    "update_object",
    "delete_objects",
    "clear_scene",
    "add_structure",
    "optimize_layout",
    "plant_add_part",
    "plant_route_pipe",
    "plant_connect",
    "plant_disconnect",
    "plant_build",
];

/// The harness's checks, per turn: when the model wants to stop after changing the scene, verify
/// it and send it back to work while problems remain.
#[derive(Default)]
pub(super) struct SelfChecks {
    changed_scene: bool,
    checks: usize,
    rtls: RtlsCheck,
    plant: PlantCheck,
    antenna: AntennaCheck,
    plan: PlanCheck,
    photo: photocheck::PhotoCheck,
}

impl SelfChecks {
    fn scene_will_change(&mut self) {
        self.changed_scene = true;
        self.photo.scene_changed();
    }
}

impl StopChecks for SelfChecks {
    fn before_tools(&mut self, names: &[&str]) {
        if names.iter().any(|n| MUTATING.contains(n) || n.starts_with(toolmaker::PREFIX)) {
            self.scene_will_change();
        }
    }

    fn observe(&mut self, entry: &LogEntry) {
        let LogEntry { who, name, input, output, is_error } = entry;
        // The model's own calls were announced by `before_tools`; nested ones weren't. A failed
        // step can still have changed something.
        if *who != Who::Model && MUTATING.contains(&name.as_str()) {
            self.scene_will_change();
        }
        if *is_error {
            return;
        }
        if name == "perceive_photo"
            && let Some(id) = input["image_id"].as_str()
        {
            self.photo.perceived(id);
        }
        self.rtls.observe(name, input, output);
        self.plant.observe(name, output);
        self.antenna.observe(name, input, output);
        self.plan.observe(name, input);
    }

    fn on_stop(&mut self, provider: &dyn Provider, turn: &Turn) -> OnStop {
        let (tx, ev) = (&turn.tx, &turn.ev);
        // These checks are the harness's own: asking the user to allow them would make no sense.
        let own = CallCtx::harness(turn.cancel.clone());
        if self.changed_scene {
            self.changed_scene = false;
            let out = tools::call_ctx(tx, "verify_scene", json!({}), own.clone());
            let report = out.summary();
            if out.is_error {
                self.checks += 1;
                let count = serde_json::from_str::<serde_json::Value>(&report).ok().and_then(|v| v["issue_count"].as_u64()).unwrap_or(0);
                if self.checks <= MAX_CHECKS {
                    let _ = ev.send(Event::Info(format!("Self-check found {count} issue(s); sending the assistant back to fix them (round {}/{MAX_CHECKS}).", self.checks)));
                    return OnStop::SendBack(vec![Block::Text(format!(
                        "[Automatic self-check] Before finishing, verify_scene measured these problems in the scene:\n{report}\n\
                         Fix every issue (move, re-stack or delete objects; use get_scene for exact positions), then call verify_scene to confirm. \
                         Only leave an issue if it is intentional, and say why."
                    ))]);
                }
                let _ = ev.send(Event::Info(format!("Self-check still reports {count} issue(s) after {MAX_CHECKS} rounds; stopping here.")));
            } else {
                let _ = ev.send(Event::Info("Self-check passed: nothing floating, sunk or intersecting, and every connection lines up.".into()));
            }
        }
        // Positioning targets from the last RTLS deployment / simulation.
        if let Some(why) = self.rtls.unmet.take() {
            self.rtls.checks += 1;
            if self.rtls.checks <= MAX_RTLS_CHECKS {
                let _ = ev.send(Event::Info(format!("RTLS accuracy target not met; sending the assistant back to improve the deployment (round {}/{MAX_RTLS_CHECKS}).", self.rtls.checks)));
                return OnStop::SendBack(vec![Block::Text(format!(
                    "[Automatic check] The positioning system does not meet its target yet: {why}\n\
                     Improve it before finishing: add or move UWB anchors / BLE beacons / RFID readers where coverage is weak \
                     (rtls_coverage shows where; rtls_optimize_anchors with target_coverage helps; search_knowledge has placement rules and troubleshooting), then rtls_run again. \
                     If the target is physically unreachable, explain why and what would fix it."
                ))]);
            }
            let _ = ev.send(Event::Info("RTLS target still not met after several rounds; stopping here.".into()));
        }
        // Connected systems must be shown to work, not just placed.
        if let Some(why) = self.plant.pending() {
            self.plant.checks += 1;
            if self.plant.checks <= MAX_PLANT_CHECKS {
                let _ = ev.send(Event::Info(format!("Connected system not proven yet; sending the assistant back to run and fix it (round {}/{MAX_PLANT_CHECKS}).", self.plant.checks)));
                return OnStop::SendBack(vec![Block::Text(format!(
                    "[Automatic check] {why}\n\
                     Make it work before finishing: call plant_run, then fix every leak, open pipe end, undriven pump, dry pump, \
                     closed path or misaligned link it reports (plant_route_pipe, plant_connect, plant_add_part, plant_set), and run again until ok is true. \
                     If something is intentional (e.g. a valve kept closed), say so."
                ))]);
            }
            let _ = ev.send(Event::Info("Connected system still reports problems after several rounds; stopping here.".into()));
        }
        // A photo rebuild is judged against the photo by someone who didn't build it.
        if let Some(image) = self.photo.due() {
            let cmp = tools::call_ctx(tx, "compare_to_photo", json!({ "image_id": image }), own.clone());
            let shot = cmp.content.iter().find_map(|b| if let Block::Image { data, mime } = b { Some((data.clone(), mime.clone())) } else { None });
            // The second view can't be flattered by pictures that line up with the photo.
            let side = photocheck::side_view(&cmp.summary()).and_then(|view| {
                tools::call_ctx(tx, "set_camera", view, own.clone());
                let s = tools::call_ctx(tx, "screenshot", json!({}), own.clone());
                s.content.iter().find_map(|b| if let Block::Image { data, mime } = b { Some((data.clone(), mime.clone())) } else { None })
            });
            let verdict = match shot.as_ref().filter(|_| !cmp.is_error) {
                Some((data, mime)) => photocheck::review(provider, data, mime, side.as_ref()),
                None => Err(anyhow::anyhow!("{}", cmp.summary())),
            };
            // Depth is one more witness for the review: it places nothing.
            let depth = tools::call_ctx(tx, "check_against_depth", json!({ "image_id": image }), own.clone());
            let depth_note = (!depth.is_error).then(|| photocheck::depth_note(&depth.summary())).flatten();
            match verdict {
                Ok(c) if c.passes() => {
                    let _ = ev.send(Event::Info(format!("Independent review passed: {:.1}/10 against the photo.", c.score)));
                }
                Ok(c) if self.photo.can_send_back() => {
                    let _ = ev.send(Event::Info(format!("Independent review scored {:.1}/10 against the photo; sending the assistant back to fix it (round {}/{}).", c.score, self.photo.reviews, photocheck::MAX_REVIEWS)));
                    let mut content = vec![Block::Text(c.report())];
                    if let Some(n) = &depth_note {
                        content.push(Block::Text(n.clone()));
                    }
                    content.extend(cmp.content.iter().filter(|b| matches!(b, Block::Image { .. })).cloned());
                    if let Some((data, mime)) = &side {
                        content.push(Block::Text("The same rebuild from a nearby viewpoint:".into()));
                        content.push(Block::Image { data: data.clone(), mime: mime.clone() });
                    }
                    return OnStop::SendBack(content);
                }
                Ok(c) => {
                    let _ = ev.send(Event::Info(format!("Independent review still scores {:.1}/10 after {} rounds; stopping here. Remaining differences:\n{}", c.score, self.photo.reviews, c.outstanding())));
                }
                // Never counted as a pass: the user is told the check didn't happen.
                Err(e) => {
                    let _ = ev.send(Event::Info(format!("Independent review could not run, so this rebuild is unreviewed: {e:#}")));
                }
            }
        }
        // An antenna is judged by the assessor, not by how the design looks.
        if let Some(why) = self.antenna.pending() {
            self.antenna.checks += 1;
            if self.antenna.checks <= MAX_ANTENNA_CHECKS {
                let _ = ev.send(Event::Info(format!("Antenna not proven yet; sending the assistant back to check it (round {}/{MAX_ANTENNA_CHECKS}).", self.antenna.checks)));
                // With nothing asked for there's nothing to reach, and this text would tell it to reach it.
                if self.antenna.only_drawn() {
                    return OnStop::SendBack(vec![Block::Text(format!("[Automatic check] {why}"))]);
                }
                return OnStop::SendBack(vec![Block::Text(format!(
                    "[Automatic check] {why}\n\
                     Before finishing: follow the antenna_assess diagnosis and next steps, change one thing at a time, then antenna_run, antenna_wait and antenna_assess again. \
                     If the requirements can't be reached, stop trying and say so: name the limit (size, Q, material loss), the best result you reached, and what would have to change. \
                     Never present a number from a run that assess calls not_trustworthy as a result."
                ))]);
            }
            let _ = ev.send(Event::Info("The antenna still isn't proven after several rounds; stopping here.".into()));
        }
        // The model's own checklist: don't stop with steps still open (e.g. to ask
        // something in plain text, which ends the turn instead of waiting).
        if let Some(open) = self.plan.open() {
            self.plan.checks += 1;
            if self.plan.checks <= MAX_PLAN_CHECKS {
                let _ = ev.send(Event::Info(format!("Plan not finished ({open}); sending the assistant back to complete it (round {}/{MAX_PLAN_CHECKS}).", self.plan.checks)));
                return OnStop::SendBack(vec![Block::Text(format!(
                    "[Automatic check] You stopped, but your plan still has unfinished steps: {open}.\n\
                     Keep working on them now. Do not end your turn to ask something in plain text: if you really need the user's input, \
                     call ask_user (it waits for the answer and you continue). When a step is done or no longer applies, call update_plan \
                     and mark it done (or failed, saying why in the step text). Finish only when every step is done or failed."
                ))]);
            }
            let _ = ev.send(Event::Info(format!("Plan still has open steps after {MAX_PLAN_CHECKS} rounds; stopping here.")));
        }
        OnStop::Finish
    }
}

/// The checklist the model keeps with `update_plan`, to catch it stopping early.
#[derive(Default)]
struct PlanCheck {
    steps: Vec<(String, String)>,
    checks: usize,
}

impl PlanCheck {
    fn observe(&mut self, tool: &str, input: &serde_json::Value) {
        if tool != "update_plan" {
            return;
        }
        self.steps = input["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some((s["step"].as_str()?.to_string(), s["status"].as_str().unwrap_or("pending").to_string())))
            .collect();
    }

    /// The steps still pending / in progress, as a short list, if any.
    fn open(&self) -> Option<String> {
        let open: Vec<String> = self
            .steps
            .iter()
            .filter(|(_, s)| matches!(s.as_str(), "pending" | "in_progress"))
            .map(|(step, s)| if s == "in_progress" { format!("\"{step}\" (in progress)") } else { format!("\"{step}\"") })
            .collect();
        (!open.is_empty()).then(|| open.join(", "))
    }
}

/// Tracks whether a connected system (pipes, drives, conveyors) was changed
/// without being simulated since, or whose last `plant_run` reported problems.
#[derive(Default)]
struct PlantCheck {
    dirty: bool,
    problem: Option<String>,
    checks: usize,
}

impl PlantCheck {
    fn observe(&mut self, tool: &str, output: &str) {
        let changes = matches!(tool, "plant_add_part" | "plant_route_pipe" | "plant_connect" | "plant_disconnect" | "plant_build" | "plant_set")
            || (tool == "place_objects" && output.contains("\"connected\""));
        if changes {
            self.dirty = true;
            self.problem = None;
        }
        if tool == "plant_run" {
            self.dirty = false;
            let Ok(o) = serde_json::from_str::<serde_json::Value>(output) else { return };
            self.problem = (!o["ok"].as_bool().unwrap_or(true)).then(|| {
                let mut why: Vec<String> = o["alarms"].as_array().into_iter().flatten().filter_map(|a| a["alarm"].as_str().map(str::to_string)).take(8).collect();
                for p in o["open_pipe_ports"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).take(6) {
                    why.push(format!("open pipe end {p}"));
                }
                format!("The last plant_run reported problems: {}.", why.join("; "))
            });
        }
    }

    /// Why the assistant should not finish yet.
    fn pending(&mut self) -> Option<String> {
        if self.dirty {
            self.dirty = false;
            return Some("The connected system was changed but not simulated since (plant_run).".into());
        }
        self.problem.take()
    }
}

/// Tracks whether an antenna design was simulated and judged since it last changed, so
/// the harness can send the assistant back. The judgement itself is `antenna_assess`.
#[derive(Default)]
struct AntennaCheck {
    /// The design changed and hasn't been assessed since.
    dirty: bool,
    /// What the latest assessment says is wrong.
    problem: Option<String>,
    /// The latest assessment met the requirements on an unchecked mesh.
    mesh_unchecked: bool,
    /// The mesh reminder went out: it's advice, not a loop.
    mesh_reminded: bool,
    checks: usize,
    /// A tag is being designed: requirements name a chip.
    tag_job: bool,
    /// Structures of the assistant's own that have been tried for it.
    tag_own_tries: usize,
    /// It's adjusting a tag that already exists, which asks for nothing new.
    tag_adjusting: bool,
    tag_reminded: bool,
    /// The known shapes have been searched, so there's something to compare with.
    tag_known_searched: bool,
    tag_compare_reminded: bool,
    /// Something was drawn, and requirements were set: either can come without the other.
    drawn: bool,
    spec_set: bool,
}

impl AntennaCheck {
    /// Something was drawn and nobody asked for it to meet anything: reading its results is the job.
    fn only_drawn(&self) -> bool {
        self.drawn && !self.spec_set
    }

    fn observe(&mut self, tool: &str, input: &serde_json::Value, output: &str) {
        let changed = matches!(tool, "antenna_new" | "antenna_set" | "antenna_import_cad" | "antenna_spec" | "antenna_draw" | "antenna_model_add" | "antenna_tag_try" | "antenna_tag_open") || (tool == "antenna_stack" && input.as_object().is_some_and(|o| !o.is_empty())) || (tool == "antenna_optimize_status" && input["apply"].as_bool().unwrap_or(false)) || (matches!(tool, "antenna_tag_retune" | "antenna_tag_tune") && input["apply"].as_bool().unwrap_or(false));
        // The design frequency moves nothing that's drawn. A real run put it back after reading its
        // results, was sent off to set requirements, made up 860 to 960 MHz at 50 ohm, and then asked the
        // user whether to make their 70 mm tag 180 mm long.
        let only_f0 = tool == "antenna_set" && self.only_drawn() && input.as_object().is_some_and(|o| o.len() == 1) && input["vars"].as_object().is_some_and(|v| v.len() == 1 && v.contains_key("f0"));
        let changed = changed && !only_f0;
        if tool == "antenna_spec" && (input.get("chip").is_some() || input.get("chip_z").is_some()) {
            self.tag_job = true;
        }
        if tool == "antenna_tag_design" {
            self.tag_job = true;
            // Testing a drawing against them counts as trying it. Only searching them doesn't.
            if input["vary"].as_array().is_some_and(|a| !a.is_empty()) {
                self.tag_own_tries += 1;
            }
            if !input["only_vary"].as_bool().unwrap_or(false) {
                self.tag_known_searched = true;
            }
        }
        if tool == "antenna_tag_try" {
            self.tag_job = true;
            self.tag_own_tries += 1;
        }
        // Tuning a tag that exists, or drawing the one in the user's sketch, asks for nothing new.
        if matches!(tool, "antenna_tag_tune" | "antenna_tag_retune" | "antenna_draw") && self.tag_own_tries == 0 {
            self.tag_adjusting = true;
        }
        // Having it judged says there's something to judge it against.
        if matches!(tool, "antenna_spec" | "antenna_assess") {
            self.spec_set = true;
        }
        // A drawing with no requirements has nothing to be judged against, and reading its results is
        // the job. Sent back to set some, a real run made up 0.5 to 6 GHz, failed it, and said so.
        if tool == "antenna_results" && self.only_drawn() {
            self.dirty = false;
        }
        if tool == "antenna_draw" {
            self.drawn = true;
        }
        if changed {
            self.dirty = true;
            self.problem = None;
            self.mesh_unchecked = false;
        }
        if tool != "antenna_assess" {
            return;
        }
        let Ok(o) = serde_json::from_str::<serde_json::Value>(output) else { return };
        let Some(verdict) = o["verdict"].as_str() else { return };
        self.dirty = false;
        self.problem = None;
        self.mesh_unchecked = verdict == "met_mesh_unchecked";
        let lines = |key: &str| -> Vec<String> { o[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect() };
        match verdict {
            "not_trustworthy" => self.problem = Some(format!("antenna_assess says the result can't be believed: {}", lines("trust_issues").join(" "))),
            "not_met" => {
                let failed: Vec<String> = o["checks"].as_array().into_iter().flatten().filter(|c| c["pass"] == false).map(|c| format!("{}: {} (needs {})", c["name"].as_str().unwrap_or(""), c["measured"].as_str().unwrap_or(""), c["required"].as_str().unwrap_or(""))).collect();
                self.problem = Some(format!("antenna_assess says the requirements aren't met: {}. Diagnosis: {} Next: {}", failed.join("; "), lines("diagnosis").join(" "), lines("next").join(" ")));
            }
            _ => {}
        }
    }

    /// Why the assistant should not finish yet.
    fn pending(&mut self) -> Option<String> {
        // Ahead of proving anything: there's no use proving a design that's about to be replaced.
        if self.tag_job && self.tag_own_tries < MIN_TAG_TRIES && !self.tag_adjusting && !self.tag_reminded {
            self.tag_reminded = true;
            return Some(format!("A new tag hasn't been designed yet: {} of your own structures tried, and known shapes with their sizes searched aren't a design. Read what limits the best so far, then try at least {MIN_TAG_TRIES} structures of your own with antenna_tag_try, across at least two different ideas, or say plainly why this job shouldn't have one.", self.tag_own_tries));
        }
        // Calling a design good means nothing with no idea what a known shape does in the same place.
        if self.tag_job && self.tag_own_tries > 0 && !self.tag_known_searched && !self.tag_adjusting && !self.tag_compare_reminded {
            self.tag_compare_reminded = true;
            return Some("Your tag hasn't been compared with anything. Run antenna_tag_design at quick effort with no vary, which keeps your tries on the list, and say how yours stands against the known shapes. Quick under-tunes them, so before saying yours beats one, search that one at normal effort with structures. If one beats yours, change your structure in answer or say so plainly.".into());
        }
        if self.dirty {
            self.dirty = false;
            if self.only_drawn() {
                return Some("The drawing changed and its results haven't been read since. Run it if it hasn't been, read antenna_results and report what they show. Nobody asked for a band or a match, so don't set requirements, don't assess it against any, and don't change the drawing to meet them.".into());
            }
            return Some("The antenna design or its requirements changed and antenna_assess hasn't judged a run since. If no requirements are set yet, set them with antenna_spec from what the user asked for.".into());
        }
        if let Some(p) = self.problem.take() {
            return Some(p);
        }
        if self.mesh_unchecked && !self.mesh_reminded {
            self.mesh_reminded = true;
            return Some("The requirements are met, but on a mesh nobody has checked. Run antenna_converge, or if it would take too long say plainly that the mesh wasn't checked.".into());
        }
        None
    }
}

/// Tracks whether the latest RTLS deployment / simulation meets its accuracy
/// target, so the harness can send the assistant back to improve it.
#[derive(Default)]
struct RtlsCheck {
    /// p95 error target in meters (from the last rtls_auto_deploy).
    target_m: Option<f64>,
    /// Why the target isn't met, if it isn't.
    unmet: Option<String>,
    checks: usize,
}

impl RtlsCheck {
    fn observe(&mut self, tool: &str, input: &serde_json::Value, output: &str) {
        let Ok(o) = serde_json::from_str::<serde_json::Value>(output) else { return };
        match tool {
            "rtls_auto_deploy" => {
                let target = input["target_cm"].as_f64().unwrap_or(30.0) / 100.0;
                self.target_m = Some(target);
                self.unmet = (!o["targets_met"].as_bool().unwrap_or(true)).then(|| {
                    // Null means no result: saying 0 cm here would read as a perfect one.
                    let cm = |v: &serde_json::Value| v.as_f64().map_or("no result".to_string(), |m| format!("{:.0} cm", m * 100.0));
                    let mut why = format!(
                        "target p95 {:.0} cm; predicted p95 {}, simulated CEP95 {}, floor coverage {:.0}%, tags located {}/{}",
                        target * 100.0,
                        cm(&o["predicted_p95_m"]),
                        cm(&o["test"]["cep95_m"]),
                        o["floor_coverage"].as_f64().unwrap_or(0.0) * 100.0,
                        o["test"]["located"],
                        o["test"]["tags"],
                    );
                    if let Some(n) = o["rfid_unread"].as_array().map(Vec::len).filter(|n| *n > 0) {
                        why.push_str(&format!(", {n} RFID tags never read"));
                    }
                    why
                });
            }
            // A later simulation that meets the target clears the complaint.
            "rtls_run" => {
                // The accuracy target is a UWB one; BLE is metre-class by design.
                let cep95 = o["by_tech"]["uwb"]["cep95_m"].as_f64().or_else(|| o["cep95_m"].as_f64());
                if let (Some(t), Some(c)) = (self.target_m, cep95) {
                    self.unmet = (c > t).then(|| format!("target p95 {:.0} cm; latest rtls_run UWB CEP95 {:.0} cm", t * 100.0, c * 100.0));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod rtls_check_tests {
    use super::{AntennaCheck, RtlsCheck};
    use serde_json::json;

    #[test]
    fn antenna_changes_must_be_assessed_and_failures_come_back() {
        let mut c = AntennaCheck::default();
        assert!(c.pending().is_none(), "no antenna work, nothing to check");
        c.observe("antenna_new", &json!({}), "{}");
        c.observe("antenna_run", &json!({}), "{}");
        assert!(c.pending().is_some_and(|w| w.contains("antenna_assess")), "running isn't judging");
        let fail = json!({ "verdict": "not_met", "checks": [{ "name": "match over the band", "pass": false, "measured": "worst S11 -3.0 dB", "required": "-10 dB" }], "diagnosis": ["Too long."], "next": ["Shorten it."] });
        c.observe("antenna_assess", &json!({}), &fail.to_string());
        let why = c.pending().unwrap();
        assert!(why.contains("-3.0 dB") && why.contains("Too long.") && why.contains("Shorten it."), "{why}");
        assert!(c.pending().is_none(), "one report per assessment");
        let bad = json!({ "verdict": "not_trustworthy", "trust_issues": ["Port 1 is not passive."] });
        c.observe("antenna_assess", &json!({}), &bad.to_string());
        assert!(c.pending().unwrap().contains("not passive"));
    }

    #[test]
    fn a_tag_job_is_sent_back_once_until_it_has_tried_structures_of_its_own() {
        let met = json!({ "verdict": "met_mesh_checked" }).to_string();
        let mut c = AntennaCheck::default();
        c.observe("antenna_spec", &json!({ "chip": "ucode9" }), "{}");
        c.observe("antenna_assess", &json!({}), &met);
        let why = c.pending().unwrap();
        assert!(why.contains("antenna_tag_try"), "{why}");
        assert!(c.pending().is_none(), "advice, not a loop");

        // Only searching the known shapes is exactly what this is for.
        let mut d = AntennaCheck::default();
        d.observe("antenna_spec", &json!({ "chip_z": [15, -231] }), "{}");
        d.observe("antenna_tag_design", &json!({}), "{}");
        d.observe("antenna_assess", &json!({}), &met);
        let why = d.pending().unwrap();
        assert!(why.contains("antenna_tag_try") && why.contains("0 of your own"), "{why}");

        // It comes first, so nothing is proved that's about to be replaced.
        let mut first = AntennaCheck::default();
        first.observe("antenna_tag_design", &json!({}), "{}");
        first.observe("antenna_tag_open", &json!({ "id": "option_1" }), "{}");
        assert!(first.pending().unwrap().contains("antenna_tag_try"));
        assert!(first.pending().unwrap().contains("antenna_assess"), "and then the proof is still asked for");

        // One try isn't a comparison of ideas.
        let mut e = AntennaCheck::default();
        e.observe("antenna_tag_try", &json!({}), "{}");
        e.observe("antenna_assess", &json!({}), &met);
        assert!(e.pending().unwrap().contains("1 of your own"));
        // Enough tries, and a drawing tested through the search counts as one, but nothing known to
        // compare them with: a real run called its first try good without ever looking.
        let mut f = AntennaCheck::default();
        f.observe("antenna_tag_try", &json!({}), "{}");
        f.observe("antenna_tag_try", &json!({}), "{}");
        f.observe("antenna_tag_design", &json!({ "only_vary": true, "vary": [{ "name": "L", "min": 0.05, "max": 0.09 }] }), "{}");
        f.observe("antenna_assess", &json!({}), &met);
        let why = f.pending().unwrap();
        assert!(why.contains("hasn't been compared") && why.contains("quick"), "{why}");
        assert!(f.pending().is_none(), "advice, not a loop");
        // Having looked, there's nothing more to say.
        let mut done = AntennaCheck::default();
        for _ in 0..3 {
            done.observe("antenna_tag_try", &json!({}), "{}");
        }
        done.observe("antenna_tag_design", &json!({ "effort": "quick" }), "{}");
        done.observe("antenna_assess", &json!({}), &met);
        assert!(done.pending().is_none());

        // Tuning a tag that already exists asks for nothing new.
        let mut g = AntennaCheck::default();
        g.observe("antenna_spec", &json!({ "chip": "ucode9" }), "{}");
        g.observe("antenna_tag_tune", &json!({ "f_mhz": 902 }), "{}");
        g.observe("antenna_assess", &json!({}), &met);
        assert!(g.pending().is_none());

        // An antenna with no chip isn't a tag.
        let mut h = AntennaCheck::default();
        h.observe("antenna_spec", &json!({ "f_lo_ghz": 2.4 }), "{}");
        h.observe("antenna_assess", &json!({}), &met);
        assert!(h.pending().is_none());

        // Drawing the tag in the user's sketch isn't designing a new one.
        let mut sketch = AntennaCheck::default();
        sketch.observe("antenna_draw", &json!({}), "{}");
        sketch.observe("antenna_spec", &json!({ "chip": "ucode9" }), "{}");
        sketch.observe("antenna_assess", &json!({}), &met);
        assert!(sketch.pending().is_none());
    }

    /// Drawing a sketch and reading its S-parameters is the whole job when nobody named a band.
    #[test]
    fn a_drawing_with_no_requirements_only_needs_its_results_read() {
        let mut c = AntennaCheck::default();
        c.observe("antenna_draw", &json!({}), "{}");
        c.observe("antenna_set", &json!({ "f_lo_ghz": 0.5, "f_hi_ghz": 6.0 }), "{}");
        c.observe("antenna_run", &json!({}), "{}");
        let why = c.pending().unwrap_or_default();
        assert!(why.contains("antenna_results") && why.contains("don't set requirements"), "not until it's looked, and not by making some up: {why}");
        c.observe("antenna_draw", &json!({}), "{}");
        c.observe("antenna_run", &json!({}), "{}");
        c.observe("antenna_results", &json!({}), "{}");
        assert!(c.pending().is_none(), "nothing to judge it against");

        // Putting the design frequency back moves nothing that's drawn.
        c.observe("antenna_set", &json!({ "vars": { "f0": "915MHz" } }), "{}");
        assert!(c.pending().is_none(), "this sent a real run off to make up a band");
        // Moving a dimension does, and so does f0 along with anything else.
        c.observe("antenna_set", &json!({ "vars": { "L": "60mm" } }), "{}");
        assert!(c.pending().is_some());
        c.observe("antenna_results", &json!({}), "{}");
        c.observe("antenna_set", &json!({ "vars": { "f0": "915MHz", "L": "60mm" } }), "{}");
        assert!(c.pending().is_some());

        // With requirements, it's judged as before.
        let mut d = AntennaCheck::default();
        d.observe("antenna_draw", &json!({}), "{}");
        d.observe("antenna_spec", &json!({ "f_lo_ghz": 0.902, "f_hi_ghz": 0.928 }), "{}");
        d.observe("antenna_results", &json!({}), "{}");
        assert!(d.pending().is_some_and(|w| w.contains("antenna_assess")));
    }

    #[test]
    fn drawing_or_new_surroundings_have_to_be_judged_again() {
        let mut c = AntennaCheck::default();
        let met = json!({ "verdict": "met_mesh_checked" }).to_string();
        for tool in ["antenna_draw", "antenna_stack", "antenna_tag_open"] {
            c.observe("antenna_assess", &json!({}), &met);
            assert!(c.pending().is_none(), "{tool}");
            c.observe(tool, &json!({ "preset": "paper_label" }), "{}");
            assert!(c.pending().is_some_and(|w| w.contains("antenna_assess")), "{tool}");
        }
        // Only asking what surroundings there are changes nothing: it sent a real run back to redo
        // a finished answer.
        let mut d = AntennaCheck::default();
        d.observe("antenna_stack", &json!({}), "{}");
        assert!(d.pending().is_none());
    }

    #[test]
    fn antenna_mesh_reminder_goes_out_once_and_a_checked_mesh_is_done() {
        let met = json!({ "verdict": "met_mesh_unchecked" }).to_string();
        let mut c = AntennaCheck::default();
        c.observe("antenna_assess", &json!({}), &met);
        assert!(c.pending().unwrap().contains("antenna_converge"));
        assert!(c.pending().is_none());
        c.observe("antenna_assess", &json!({}), &met);
        assert!(c.pending().is_none(), "advice, not a loop");
        let mut d = AntennaCheck::default();
        d.observe("antenna_set", &json!({}), "{}");
        d.observe("antenna_assess", &json!({}), &json!({ "verdict": "met_mesh_checked" }).to_string());
        assert!(d.pending().is_none());
    }

    #[test]
    fn applying_an_optimizer_result_needs_judging_but_reading_it_does_not() {
        let mut c = AntennaCheck::default();
        c.observe("antenna_optimize_status", &json!({}), "{}");
        assert!(c.pending().is_none());
        c.observe("antenna_optimize_status", &json!({ "apply": true }), "{}");
        assert!(c.pending().is_some());
    }

    #[test]
    fn unmet_deploy_then_fixed_by_run() {
        let mut c = RtlsCheck::default();
        let deploy = json!({ "targets_met": false, "predicted_p95_m": 0.42, "floor_coverage": 0.81,
            "test": { "cep95_m": 0.55, "located": 7, "tags": 9 }, "rfid_unread": ["T1", "T2", "T3"] });
        c.observe("rtls_auto_deploy", &json!({ "target_cm": 30 }), &deploy.to_string());
        let why = c.unmet.clone().expect("target not met");
        assert!(why.contains("55 cm") && why.contains("7/9") && why.contains("3 RFID"), "{why}");
        // No result isn't a perfect result.
        let empty = json!({ "targets_met": false, "predicted_p95_m": null, "test": { "cep95_m": null, "located": 0, "tags": 9 } });
        let mut none = RtlsCheck::default();
        none.observe("rtls_auto_deploy", &json!({ "target_cm": 30 }), &empty.to_string());
        let why = none.unmet.clone().expect("target not met");
        assert!(why.contains("predicted p95 no result") && why.contains("CEP95 no result"), "{why}");
        // Still too coarse.
        c.observe("rtls_run", &json!({}), &json!({ "cep95_m": 0.4 }).to_string());
        assert!(c.unmet.is_some());
        // Within 30 cm: done.
        c.observe("rtls_run", &json!({}), &json!({ "cep95_m": 0.25 }).to_string());
        assert!(c.unmet.is_none());
    }

    #[test]
    fn met_deploy_is_quiet() {
        let mut c = RtlsCheck::default();
        c.observe("rtls_auto_deploy", &json!({}), &json!({ "targets_met": true }).to_string());
        assert!(c.unmet.is_none());
        // Runs without a known target never complain.
        let mut c = RtlsCheck::default();
        c.observe("rtls_run", &json!({}), &json!({ "cep95_m": 5.0 }).to_string());
        assert!(c.unmet.is_none());
    }
}

#[cfg(test)]
mod rtls_check_by_tech {
    use super::RtlsCheck;
    use serde_json::json;

    #[test]
    fn judges_uwb_not_ble() {
        let mut c = RtlsCheck::default();
        c.observe("rtls_auto_deploy", &json!({ "target_cm": 30 }), &json!({ "targets_met": true }).to_string());
        // BLE drags the combined CEP95 to metres; UWB alone meets 30 cm.
        let run = json!({ "cep95_m": 3.7, "by_tech": { "uwb": { "cep95_m": 0.25 }, "ble": { "cep95_m": 10.1 } } });
        c.observe("rtls_run", &json!({}), &run.to_string());
        assert!(c.unmet.is_none());
        let run = json!({ "cep95_m": 3.7, "by_tech": { "uwb": { "cep95_m": 0.48 } } });
        c.observe("rtls_run", &json!({}), &run.to_string());
        assert!(c.unmet.as_deref().unwrap().contains("UWB CEP95 48 cm"));
    }
}

