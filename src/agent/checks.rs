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
}

impl AntennaCheck {
    fn observe(&mut self, tool: &str, input: &serde_json::Value, output: &str) {
        let changed = matches!(tool, "antenna_new" | "antenna_set" | "antenna_import_cad" | "antenna_spec") || (tool == "antenna_optimize_status" && input["apply"].as_bool().unwrap_or(false));
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
        if self.dirty {
            self.dirty = false;
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

