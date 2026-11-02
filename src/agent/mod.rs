//! AI scene assistant: a chat panel driving an LLM (Anthropic, OpenAI,
//! OpenRouter or a local OpenAI-compatible server) that builds layouts from
//! instructions or reference photos using the tools in [`tools`]. The same
//! tools are served over MCP, and external MCP servers can add more.

pub mod assetgen;
pub mod catalog;
pub mod config;
pub mod knowledge;
pub mod llm;
pub mod markdown;
pub mod panel;
pub mod photocheck;
pub mod placer;
pub mod prompt;
pub mod structures;
pub mod toolmaker;
pub mod tools;
pub mod verify;
#[path = "../vision/mod.rs"]
pub mod vision;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bevy::prelude::*;
use bevy::window::FileDragAndDrop;
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::mcp::client::McpHub;
use config::AgentConfig;
use llm::{Block, Message};
use tools::{PendingTools, ToolBridge, ToolJob, ToolOutput};

pub enum ChatItem {
    User { text: String, images: Vec<String> },
    Assistant(String),
    Tool { name: String, input: String, output: String, is_error: bool },
    /// Index into `Agent::questions`.
    Question(usize),
    Image(String),
    Info(String),
    Error(String),
}

pub struct Question {
    pub text: String,
    pub options: Vec<String>,
    /// Library entry indices to show as clickable thumbnails.
    pub candidates: Vec<usize>,
    pub allow_text: bool,
    pub input: String,
    pub answer: Option<String>,
    pub reply: Option<Sender<ToolOutput>>,
}

impl Question {
    pub fn answer(&mut self, text: String) {
        if let Some(r) = self.reply.take() {
            let _ = r.send(ToolOutput::text(format!("User answered: {text}")));
        }
        self.answer = Some(text);
    }
}

enum Event {
    Assistant(String),
    ToolStart { name: String, input: String },
    ToolEnd { output: String, is_error: bool },
    Finished { history: Vec<Message>, error: Option<String> },
    Info(String),
    Image(String),
}

#[derive(Resource)]
pub struct Agent {
    pub config: AgentConfig,
    pub open: bool,
    pub show_settings: bool,
    pub transcript: Vec<ChatItem>,
    pub questions: Vec<Question>,
    pub input: String,
    /// Image ids attached to the next message.
    pub attachments: Vec<String>,
    pub history: Vec<Message>,
    pub running: bool,
    pub mcp: Arc<McpHub>,
    pub mcp_server_status: String,
    /// egui textures for chat images, by image id.
    pub textures: std::collections::HashMap<String, bevy_egui::egui::TextureHandle>,
    /// Model lists fetched from each provider, for the picker.
    pub models: std::collections::HashMap<config::ProviderKind, Vec<String>>,
    pub models_status: String,
    models_rx: Option<(config::ProviderKind, Receiver<Result<Vec<String>, String>>)>,
    /// Result of "Test connection": Ok(reply) or Err(message).
    pub test_status: Option<Result<String, String>>,
    test_rx: Option<Receiver<Result<String, String>>>,
    /// Checklist maintained by the `update_plan` tool.
    pub plan: Vec<(String, String)>,
    cancel: Arc<AtomicBool>,
    events: Option<Receiver<Event>>,
}

impl Agent {
    fn new(config: AgentConfig) -> Self {
        let mcp = Arc::new(McpHub::default());
        mcp.connect_all(&config.mcp_servers);
        Self {
            config,
            open: false,
            show_settings: false,
            transcript: Vec::new(),
            questions: Vec::new(),
            input: String::new(),
            attachments: Vec::new(),
            history: Vec::new(),
            running: false,
            mcp,
            mcp_server_status: String::new(),
            textures: Default::default(),
            models: Default::default(),
            models_status: String::new(),
            models_rx: None,
            test_status: None,
            test_rx: None,
            plan: Vec::new(),
            cancel: Arc::default(),
            events: None,
        }
    }

    /// Fetches the active provider's model list in the background.
    pub fn refresh_models(&mut self) {
        let cfg = self.config.provider().clone();
        let kind = cfg.kind;
        let (tx, rx) = unbounded();
        std::thread::spawn(move || {
            let _ = tx.send(llm::list_models(&cfg).map_err(|e| format!("{e:#}")));
        });
        self.models_status = "loading models…".into();
        self.models_rx = Some((kind, rx));
    }

    /// Sends a one-line request to check the key, model and endpoint.
    pub fn test_connection(&mut self) {
        let provider = match llm::make_provider(self.config.provider(), "low") {
            Ok(p) => p,
            Err(e) => {
                self.test_status = Some(Err(format!("{e:#}")));
                return;
            }
        };
        let (tx, rx) = unbounded();
        std::thread::spawn(move || {
            let msg = Message::user(vec![Block::Text("Reply with the single word OK.".into())]);
            let r = provider.complete("You are a connection test.", &[msg], &[]).map(|m| m.text()).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
        self.test_status = Some(Ok("testing…".into()));
        self.test_rx = Some(rx);
    }

    /// Why the assistant can't run right now, if it can't.
    pub fn blocker(&self) -> Option<String> {
        let p = self.config.provider();
        if p.kind.needs_key() && p.resolved_key().is_empty() {
            return Some(format!("Add an API key for {} (or set {}) in Settings → AI assistant.", p.kind.label(), p.kind.env_var()));
        }
        None
    }

    fn poll_background(&mut self) {
        if let Some((kind, rx)) = &self.models_rx
            && let Ok(r) = rx.try_recv()
        {
            let kind = *kind;
            match r {
                Ok(list) => {
                    self.models_status = format!("{} models", list.len());
                    self.models.insert(kind, list);
                }
                Err(e) => self.models_status = e,
            }
            self.models_rx = None;
        }
        if let Some(rx) = &self.test_rx
            && let Ok(r) = rx.try_recv()
        {
            self.test_status = Some(r.map(|t| format!("OK - {} replied: {}", self.config.provider().effective_model(), t.trim())));
            self.test_rx = None;
        }
    }

    pub fn attach_file(&mut self, path: &std::path::Path) {
        match images::load_file(path) {
            Ok((id, _, _)) => {
                self.attachments.push(id);
                self.open = true;
            }
            Err(e) => self.transcript.push(ChatItem::Error(e)),
        }
    }

    /// Sends the composed message and starts a worker for this turn.
    pub fn send(&mut self, bridge: &ToolBridge) {
        if self.running {
            return;
        }
        let text = self.input.trim().to_string();
        let attachments = std::mem::take(&mut self.attachments);
        if text.is_empty() && attachments.is_empty() {
            return;
        }
        let provider = match llm::make_provider(self.config.provider(), &self.config.effort) {
            Ok(p) => p,
            Err(e) => {
                self.transcript.push(ChatItem::Error(format!("{e:#}")));
                self.attachments = attachments;
                return;
            }
        };
        self.input.clear();
        let mut content = Vec::new();
        for id in &attachments {
            if let Some((data, mime)) = images::get(id) {
                content.push(Block::Text(format!("[reference image, image_id: {id}]")));
                content.push(Block::Image { data, mime });
            }
        }
        let shown = if text.is_empty() { "Rebuild this scene with SimReady assets.".to_string() } else { text };
        content.push(Block::Text(shown.clone()));
        let context = if self.config.knowledge_auto { knowledge::auto_context(&shown) } else { None };
        self.transcript.push(ChatItem::User { text: shown, images: attachments });
        if let Some(c) = context {
            content.push(Block::Text(c.text));
            self.transcript.push(ChatItem::Info(format!("Knowledge attached: {}", c.headings.join("; "))));
        }

        let mut history = self.history.clone();
        history.push(Message::user(content));
        let mut tool_defs = tools::tool_defs(false);
        tool_defs.extend(self.mcp.tool_defs());
        let (ev_tx, ev_rx) = unbounded();
        self.events = Some(ev_rx);
        self.cancel = Arc::default();
        self.running = true;
        let (tx, mcp, cancel, steps) = (bridge.tx.clone(), self.mcp.clone(), self.cancel.clone(), self.config.max_steps);
        std::thread::spawn(move || worker(provider, steps, history, tool_defs, tx, mcp, ev_tx, cancel));
    }

    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Unblock questions the worker is waiting on.
        for q in &mut self.questions {
            if q.answer.is_none() && q.reply.is_some() {
                q.answer("(no answer - the user stopped the assistant)".into());
            }
        }
    }

    pub fn new_chat(&mut self) {
        self.stop();
        self.history.clear();
        self.transcript.clear();
        self.questions.clear();
    }
}

#[allow(clippy::too_many_arguments)]
fn worker(
    provider: Box<dyn llm::Provider>,
    max_steps: usize,
    mut history: Vec<Message>,
    mut tool_defs: Vec<llm::ToolDef>,
    tx: Sender<ToolJob>,
    mcp: Arc<McpHub>,
    ev: Sender<Event>,
    cancel: Arc<AtomicBool>,
) {
    let mut error = None;
    // Self-check harness: when the model wants to stop after changing the
    // scene, verify it and send it back to work while problems remain.
    let mut changed_scene = false;
    let mut checks = 0;
    const MAX_CHECKS: usize = 4;
    let mut rtls = RtlsCheck::default();
    const MAX_RTLS_CHECKS: usize = 3;
    let mut plant = PlantCheck::default();
    const MAX_PLANT_CHECKS: usize = 3;
    let mut antenna = AntennaCheck::default();
    const MAX_ANTENNA_CHECKS: usize = 4;
    let mut plan = PlanCheck::default();
    let mut photo = photocheck::PhotoCheck::default();
    const MAX_PLAN_CHECKS: usize = 4;
    for step in 0.. {
        if cancel.load(Ordering::Relaxed) {
            error = Some("stopped".to_string());
            break;
        }
        if step >= max_steps {
            error = Some(format!("stopped after {max_steps} steps (raise max_steps in agent.json)"));
            break;
        }
        let msg = match provider.complete(prompt::SYSTEM, &history, &tool_defs) {
            Ok(m) => m,
            Err(e) => {
                error = Some(format!("{e:#}"));
                break;
            }
        };
        let text = msg.text();
        if !text.trim().is_empty() {
            let _ = ev.send(Event::Assistant(text));
        }
        let uses = msg.tool_uses();
        history.push(msg);
        if uses.is_empty() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if changed_scene {
                changed_scene = false;
                let out = tools::call(&tx, "verify_scene", serde_json::json!({}), false, Some(&cancel));
                let report = out.summary();
                if out.is_error {
                    checks += 1;
                    let count = serde_json::from_str::<serde_json::Value>(&report).ok().and_then(|v| v["issue_count"].as_u64()).unwrap_or(0);
                    if checks <= MAX_CHECKS {
                        let _ = ev.send(Event::Info(format!("Self-check found {count} issue(s); sending the assistant back to fix them (round {checks}/{MAX_CHECKS}).")));
                        history.push(Message::user(vec![Block::Text(format!(
                            "[Automatic self-check] Before finishing, verify_scene measured these problems in the scene:\n{report}\n\
                             Fix every issue (move, re-stack or delete objects; use get_scene for exact positions), then call verify_scene to confirm. \
                             Only leave an issue if it is intentional, and say why."
                        ))]));
                        continue;
                    }
                    let _ = ev.send(Event::Info(format!("Self-check still reports {count} issue(s) after {MAX_CHECKS} rounds; stopping here.")));
                } else {
                    let _ = ev.send(Event::Info("Self-check passed: nothing floating, sunk or intersecting, and every connection lines up.".into()));
                }
            }
            // Positioning targets from the last RTLS deployment / simulation.
            if let Some(why) = rtls.unmet.take() {
                rtls.checks += 1;
                if rtls.checks <= MAX_RTLS_CHECKS {
                    let _ = ev.send(Event::Info(format!(
                        "RTLS accuracy target not met; sending the assistant back to improve the deployment (round {}/{MAX_RTLS_CHECKS}).",
                        rtls.checks
                    )));
                    history.push(Message::user(vec![Block::Text(format!(
                        "[Automatic check] The positioning system does not meet its target yet: {why}\n\
                         Improve it before finishing: add or move UWB anchors / BLE beacons / RFID readers where coverage is weak \
                         (rtls_coverage shows where; rtls_optimize_anchors with mount \"structure\" and target_coverage helps; search_knowledge has placement rules and troubleshooting), then rtls_run again. \
                         If the target is physically unreachable, explain why and what would fix it."
                    ))]));
                    continue;
                }
                let _ = ev.send(Event::Info("RTLS target still not met after several rounds; stopping here.".into()));
            }
            // Connected systems must be shown to work, not just placed.
            if let Some(why) = plant.pending() {
                plant.checks += 1;
                if plant.checks <= MAX_PLANT_CHECKS {
                    let _ = ev.send(Event::Info(format!(
                        "Connected system not proven yet; sending the assistant back to run and fix it (round {}/{MAX_PLANT_CHECKS}).",
                        plant.checks
                    )));
                    history.push(Message::user(vec![Block::Text(format!(
                        "[Automatic check] {why}\n\
                         Make it work before finishing: call plant_run, then fix every leak, open pipe end, undriven pump, dry pump, \
                         closed path or misaligned link it reports (plant_route_pipe, plant_connect, plant_add_part, plant_set), and run again until ok is true. \
                         If something is intentional (e.g. a valve kept closed), say so."
                    ))]));
                    continue;
                }
                let _ = ev.send(Event::Info("Connected system still reports problems after several rounds; stopping here.".into()));
            }
            // A photo rebuild is judged against the photo by someone who didn't build it.
            if let Some(image) = photo.due() {
                let cmp = tools::call(&tx, "compare_to_photo", serde_json::json!({ "image_id": image }), false, Some(&cancel));
                let shot = cmp.content.iter().find_map(|b| if let Block::Image { data, mime } = b { Some((data.clone(), mime.clone())) } else { None });
                // The second view can't be flattered by pictures that line up with the photo.
                let side = photocheck::side_view(&cmp.summary()).and_then(|view| {
                    tools::call(&tx, "set_camera", view, false, Some(&cancel));
                    let s = tools::call(&tx, "screenshot", serde_json::json!({}), false, Some(&cancel));
                    s.content.iter().find_map(|b| if let Block::Image { data, mime } = b { Some((data.clone(), mime.clone())) } else { None })
                });
                let verdict = match shot.as_ref().filter(|_| !cmp.is_error) {
                    Some((data, mime)) => photocheck::review(provider.as_ref(), data, mime, side.as_ref()),
                    None => Err(anyhow::anyhow!("{}", cmp.summary())),
                };
                // Depth is one more witness for the review: it places nothing.
                let depth = tools::call(&tx, "check_against_depth", serde_json::json!({ "image_id": image }), false, Some(&cancel));
                let depth_note = (!depth.is_error).then(|| photocheck::depth_note(&depth.summary())).flatten();
                match verdict {
                    Ok(c) if c.passes() => {
                        let _ = ev.send(Event::Info(format!("Independent review passed: {:.1}/10 against the photo.", c.score)));
                    }
                    Ok(c) if photo.can_send_back() => {
                        let _ = ev.send(Event::Info(format!(
                            "Independent review scored {:.1}/10 against the photo; sending the assistant back to fix it (round {}/{}).",
                            c.score,
                            photo.reviews,
                            photocheck::MAX_REVIEWS
                        )));
                        let mut content = vec![Block::Text(c.report())];
                        if let Some(n) = &depth_note {
                            content.push(Block::Text(n.clone()));
                        }
                        content.extend(cmp.content.iter().filter(|b| matches!(b, Block::Image { .. })).cloned());
                        if let Some((data, mime)) = &side {
                            content.push(Block::Text("The same rebuild from a nearby viewpoint:".into()));
                            content.push(Block::Image { data: data.clone(), mime: mime.clone() });
                        }
                        history.push(Message::user(content));
                        continue;
                    }
                    Ok(c) => {
                        let _ = ev.send(Event::Info(format!("Independent review still scores {:.1}/10 after {} rounds; stopping here. Remaining differences:\n{}", c.score, photo.reviews, c.outstanding())));
                    }
                    // Never counted as a pass: the user is told the check didn't happen.
                    Err(e) => {
                        let _ = ev.send(Event::Info(format!("Independent review could not run, so this rebuild is unreviewed: {e:#}")));
                    }
                }
            }
            // An antenna is judged by the assessor, not by how the design looks.
            if let Some(why) = antenna.pending() {
                antenna.checks += 1;
                if antenna.checks <= MAX_ANTENNA_CHECKS {
                    let _ = ev.send(Event::Info(format!(
                        "Antenna not proven yet; sending the assistant back to check it (round {}/{MAX_ANTENNA_CHECKS}).",
                        antenna.checks
                    )));
                    history.push(Message::user(vec![Block::Text(format!(
                        "[Automatic check] {why}\n\
                         Before finishing: follow the antenna_assess diagnosis and next steps, change one thing at a time, then antenna_run, antenna_wait and antenna_assess again. \
                         If the requirements can't be reached, stop trying and say so: name the limit (size, Q, material loss), the best result you reached, and what would have to change. \
                         Never present a number from a run that assess calls not_trustworthy as a result."
                    ))]));
                    continue;
                }
                let _ = ev.send(Event::Info("The antenna still isn't proven after several rounds; stopping here.".into()));
            }
            // The model's own checklist: don't stop with steps still open (e.g. to ask
            // something in plain text, which ends the turn instead of waiting).
            if let Some(open) = plan.open() {
                plan.checks += 1;
                if plan.checks <= MAX_PLAN_CHECKS {
                    let _ = ev.send(Event::Info(format!(
                        "Plan not finished ({open}); sending the assistant back to complete it (round {}/{MAX_PLAN_CHECKS}).",
                        plan.checks
                    )));
                    history.push(Message::user(vec![Block::Text(format!(
                        "[Automatic check] You stopped, but your plan still has unfinished steps: {open}.\n\
                         Keep working on them now. Do not end your turn to ask something in plain text: if you really need the user's input, \
                         call ask_user (it waits for the answer and you continue). When a step is done or no longer applies, call update_plan \
                         and mark it done (or failed, saying why in the step text). Finish only when every step is done or failed."
                    ))]));
                    continue;
                }
                let _ = ev.send(Event::Info(format!("Plan still has open steps after {MAX_PLAN_CHECKS} rounds; stopping here.")));
            }
            break;
        }
        // A tool made this turn can be called on the next one.
        let tools_changed = uses.iter().any(|(_, n, _)| n == "define_tool" || n == "delete_tool");
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
        if uses.iter().any(|(_, n, _)| MUTATING.contains(&n.as_str()) || n.starts_with(toolmaker::PREFIX)) {
            changed_scene = true;
            photo.scene_changed();
        }
        let run = |name: &str, input: &serde_json::Value| {
            if cancel.load(Ordering::Relaxed) {
                ToolOutput::err("cancelled by the user")
            } else if let Some(o) = mcp.call(name, input) {
                o
            } else {
                tools::call(&tx, name, input.clone(), false, Some(&cancel))
            }
        };
        // Lookups, downloads and external MCP tools don't touch the scene:
        // a turn made only of those runs them all at once (one thread each).
        const PARALLEL_SAFE: [&str; 13] = ["search_assets", "get_asset_info", "download_assets", "list_asset_categories", "get_scene", "perceive_photo", "search_knowledge", "read_knowledge", "list_knowledge", "antenna_calc", "antenna_get", "antenna_list_templates", "antenna_results"];
        let parallel = uses.len() > 1 && uses.iter().all(|(_, n, _)| PARALLEL_SAFE.contains(&n.as_str()) || mcp.has_tool(n));
        let mut outs: Vec<Option<ToolOutput>> = if parallel {
            let _ = ev.send(Event::Info(format!("Running {} tool calls in parallel…", uses.len())));
            std::thread::scope(|s| {
                let handles: Vec<_> = uses.iter().map(|(_, name, input)| s.spawn(|| run(name, input))).collect();
                handles.into_iter().map(|h| Some(h.join().unwrap_or_else(|_| ToolOutput::err("tool thread panicked")))).collect()
            })
        } else {
            uses.iter().map(|_| None).collect()
        };
        // All results go back in one user message (keeps parallel tool use working).
        let mut results = Vec::new();
        for (k, (id, name, input)) in uses.into_iter().enumerate() {
            let _ = ev.send(Event::ToolStart { name: name.clone(), input: input.to_string() });
            let out = outs[k].take().unwrap_or_else(|| run(&name, &input));
            let _ = ev.send(Event::ToolEnd { output: out.summary(), is_error: out.is_error });
            if !out.is_error {
                if name == "perceive_photo"
                    && let Some(id) = input["image_id"].as_str()
                {
                    photo.perceived(id);
                }
                rtls.observe(&name, &input, &out.summary());
                plant.observe(&name, &out.summary());
                antenna.observe(&name, &input, &out.summary());
                plan.observe(&name, &input);
            }
            if name == "perceive_photo" || name == "load_reference_image" {
                for b in &out.content {
                    if let Block::Image { data, mime } = b {
                        let _ = ev.send(Event::Image(images::put(data.clone(), mime)));
                    }
                }
            }
            results.push(Block::ToolResult { id, content: out.content, is_error: out.is_error });
        }
        history.push(Message::user(results));
        if tools_changed {
            tool_defs.retain(|d| !d.name.starts_with(toolmaker::PREFIX));
            tool_defs.extend(toolmaker::defs());
        }
    }
    let _ = ev.send(Event::Finished { history, error });
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
                    let mut why = format!(
                        "target p95 {:.0} cm; predicted p95 {:.0} cm, measured CEP95 {:.0} cm, floor coverage {:.0}%, tags located {}/{}",
                        target * 100.0,
                        o["predicted_p95_m"].as_f64().unwrap_or(0.0) * 100.0,
                        o["test"]["cep95_m"].as_f64().unwrap_or(0.0) * 100.0,
                        o["floor_coverage"].as_f64().unwrap_or(0.0) * 100.0,
                        o["test"]["located"],
                        o["test"]["tags"],
                    );
                    if let Some(n) = o["rfid_unread"].as_u64().filter(|n| *n > 0) {
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

fn pump_events(mut agent: ResMut<Agent>) {
    agent.poll_background();
    let Some(rx) = agent.events.clone() else { return };
    for e in rx.try_iter() {
        match e {
            Event::Assistant(t) => agent.transcript.push(ChatItem::Assistant(t)),
            Event::ToolStart { name, input } => {
                agent.transcript.push(ChatItem::Tool { name, input, output: String::new(), is_error: false })
            }
            Event::ToolEnd { output: o, is_error: err } => {
                if let Some(ChatItem::Tool { output, is_error, .. }) =
                    agent.transcript.iter_mut().rev().find(|c| matches!(c, ChatItem::Tool { .. }))
                {
                    *output = o;
                    *is_error = err;
                }
            }
            Event::Info(t) => agent.transcript.push(ChatItem::Info(t)),
            Event::Image(id) => agent.transcript.push(ChatItem::Image(id)),
            Event::Finished { history, error } => {
                agent.history = history;
                agent.running = false;
                agent.events = None;
                if let Some(e) = error {
                    agent.transcript.push(ChatItem::Error(e));
                }
            }
        }
    }
}

fn file_drop(mut drops: MessageReader<FileDragAndDrop>, mut agent: ResMut<Agent>) {
    for d in drops.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = d {
            let ext = path_buf.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp") {
                agent.attach_file(path_buf);
            }
        }
    }
}

pub struct AgentPlugin {
    /// `None` disables the MCP server.
    pub mcp_port: Option<u16>,
}

impl Plugin for AgentPlugin {
    fn build(&self, app: &mut App) {
        let config = AgentConfig::load();
        let mut agent = Agent::new(config.clone());
        let bridge = ToolBridge::default();
        let port = self.mcp_port.unwrap_or(config.mcp_port);
        agent.mcp_server_status = if config.mcp_server && self.mcp_port != Some(0) {
            match crate::mcp::server::start(port, bridge.tx.clone()) {
                Ok(url) => format!("MCP server: {url}"),
                Err(e) => format!("MCP server off: {e}"),
            }
        } else {
            "MCP server disabled".into()
        };
        app.insert_resource(agent)
            .insert_resource(bridge)
            .init_resource::<PendingTools>()
            .init_resource::<catalog::Catalog>()
            .add_systems(
                Update,
                (
                    // Actions a tool queues (load, clear) must be seen by the RTLS / plant hooks.
                    tools::run_tool_jobs.before(crate::rtls::persist::peek_actions).before(crate::plant::persist::peek_actions),
                    pump_events,
                    catalog::update_catalog,
                    file_drop,
                ),
            );
    }
}

/// Images the assistant can refer to by id (attachments, screenshots).
pub mod images {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, LazyLock, Mutex};

    type Entry = (Arc<Vec<u8>>, String);
    static STORE: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(Default::default);
    static NEXT: AtomicUsize = AtomicUsize::new(1);

    pub fn put(data: Arc<Vec<u8>>, mime: &str) -> String {
        let id = format!("img{}", NEXT.fetch_add(1, Ordering::Relaxed));
        STORE.lock().unwrap().insert(id.clone(), (data, mime.to_string()));
        id
    }

    pub fn get(id: &str) -> Option<Entry> {
        STORE.lock().unwrap().get(id).cloned()
    }

    /// Loads a photo, downsized to at most 1600 px (enough for vision models
    /// and depth), re-encoded as JPEG.
    pub fn load_file(path: &Path) -> Result<(String, Arc<Vec<u8>>, String), String> {
        let img = image::open(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
        let img = if img.width().max(img.height()) > 1600 { img.resize(1600, 1600, image::imageops::FilterType::Lanczos3) } else { img };
        // JPEG has no alpha: flatten transparent PNG/WebP onto white instead of
        // letting transparent pixels turn black.
        let rgb = if img.color().has_alpha() {
            let rgba = img.to_rgba8();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                let [r, g, b, a] = rgba.get_pixel(x, y).0;
                let mix = |c: u8| ((c as u16 * a as u16 + 255 * (255 - a as u16)) / 255) as u8;
                image::Rgb([mix(r), mix(g), mix(b)])
            }))
        } else {
            image::DynamicImage::ImageRgb8(img.to_rgb8())
        };
        let mut out = std::io::Cursor::new(Vec::new());
        rgb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)).map_err(|e| e.to_string())?;
        let data = Arc::new(out.into_inner());
        let id = put(data.clone(), "image/jpeg");
        Ok((id, data, "image/jpeg".into()))
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
            "test": { "cep95_m": 0.55, "located": 7, "tags": 9 }, "rfid_unread": 3 });
        c.observe("rtls_auto_deploy", &json!({ "target_cm": 30 }), &deploy.to_string());
        let why = c.unmet.clone().expect("target not met");
        assert!(why.contains("55 cm") && why.contains("7/9") && why.contains("3 RFID"), "{why}");
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
