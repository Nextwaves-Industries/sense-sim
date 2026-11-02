//! AI scene assistant: a chat panel driving an LLM (Anthropic, OpenAI, Meta Muse,
//! Gemini, Grok, DeepSeek, OpenRouter or a local OpenAI-compatible server) that builds layouts from
//! instructions or reference photos using the tools in [`tools`]. The same
//! tools are served over MCP, and external MCP servers can add more.

pub mod assetgen;
pub mod catalog;
mod checks;
pub mod config;
pub mod harness;
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

#[cfg(test)]
mod golden_tests;

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
    /// New chat was pressed during a run: what that run still reports belongs to the old chat.
    discard_run: bool,
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
            discard_run: false,
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
        let provider: Arc<dyn llm::Provider> = match llm::make_provider(self.config.provider(), &self.config.effort) {
            Ok(p) => Arc::from(p),
            Err(e) => {
                self.transcript.push(ChatItem::Error(format!("{e:#}")));
                self.attachments = attachments;
                return;
            }
        };
        self.input.clear();
        // A finished checklist has done its job; one with open steps may still be wanted.
        if self.plan.iter().all(|(_, status)| !matches!(status.as_str(), "pending" | "in_progress")) {
            self.plan.clear();
        }
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

        let before = self.history.clone();
        let mut history = before.clone();
        history.push(Message::user(content));
        let mut tool_defs = tools::tool_defs(false);
        tool_defs.extend(self.mcp.tool_defs());
        let (ev_tx, ev_rx) = unbounded();
        self.events = Some(ev_rx);
        self.cancel = Arc::default();
        self.running = true;
        let (tx, mcp, cancel, steps) = (bridge.tx.clone(), self.mcp.clone(), self.cancel.clone(), self.config.max_steps);
        std::thread::spawn(move || {
            let done = ev_tx.clone();
            let run = std::panic::AssertUnwindSafe(move || worker(provider, steps, history, tool_defs, tx, mcp, ev_tx, cancel));
            // Without this the panel would say "Working" for ever, with Send disabled.
            if let Err(panic) = std::panic::catch_unwind(run) {
                let why = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_else(|| "no message".into());
                let _ = done.send(Event::Finished { history: before, error: Some(format!("The assistant stopped on an internal error, and this turn's work isn't in its memory: {why}")) });
            }
        });
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
        // The run ends on its own shortly; its history must not come back into this chat.
        self.discard_run = self.running;
        self.history.clear();
        self.transcript.clear();
        self.questions.clear();
        self.plan.clear();
    }
}

#[allow(clippy::too_many_arguments)]
fn worker(
    provider: Arc<dyn llm::Provider>,
    max_steps: usize,
    history: Vec<Message>,
    tool_defs: Vec<llm::ToolDef>,
    tx: Sender<ToolJob>,
    mcp: Arc<McpHub>,
    ev: Sender<Event>,
    cancel: Arc<AtomicBool>,
) {
    let mut spec = harness::AgentSpec { system: prompt::SYSTEM.to_string(), max_steps, tools: tool_defs };
    let mut turn = harness::Turn { history, log: Default::default(), cancel, tx, mcp, ev };
    let error = harness::run_loop(&provider, &mut spec, &mut turn, &mut checks::SelfChecks::default());
    let _ = turn.ev.send(Event::Finished { history: turn.history, error });
}

fn pump_events(mut agent: ResMut<Agent>) {
    agent.poll_background();
    let Some(rx) = agent.events.clone() else { return };
    for e in rx.try_iter() {
        // New chat was pressed: only the end of that run matters, so Send works again.
        if agent.discard_run {
            if matches!(e, Event::Finished { .. }) {
                agent.discard_run = false;
                agent.running = false;
                agent.events = None;
            }
            continue;
        }
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
mod lifecycle_tests {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use bevy::ecs::system::RunSystemOnce;

    use super::harness::complete_cancellable;
    use super::*;

    struct Slow;
    impl llm::Provider for Slow {
        fn complete(&self, _: &str, _: &[Message], _: &[llm::ToolDef]) -> anyhow::Result<Message> {
            std::thread::sleep(Duration::from_secs(30));
            anyhow::bail!("should have been abandoned")
        }
    }

    struct Panics;
    impl llm::Provider for Panics {
        fn complete(&self, _: &str, _: &[Message], _: &[llm::ToolDef]) -> anyhow::Result<Message> {
            panic!("a parse bug");
        }
    }

    #[test]
    fn stop_interrupts_a_model_call_that_is_still_waiting() {
        let provider: Arc<dyn llm::Provider> = Arc::new(Slow);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            stopper.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        assert!(complete_cancellable(&provider, "s", &[], &[], &cancel).unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[test]
    fn a_panic_in_a_model_call_is_an_error_not_a_hang() {
        let provider: Arc<dyn llm::Provider> = Arc::new(Panics);
        let e = complete_cancellable(&provider, "s", &[], &[], &AtomicBool::new(false)).unwrap_err().to_string();
        assert!(e.contains("without a reply"), "{e}");
    }

    #[test]
    fn new_chat_during_a_run_doesnt_get_the_old_history_back() {
        let mut agent = Agent::new(AgentConfig::default());
        let (tx, rx) = unbounded();
        agent.events = Some(rx);
        agent.running = true;
        agent.plan = vec![("step".into(), "in_progress".into())];
        agent.new_chat();
        assert!(agent.plan.is_empty());

        tx.send(Event::Assistant("from the old chat".into())).unwrap();
        tx.send(Event::Finished { history: vec![Message::user(vec![Block::Text("old".into())])], error: Some("stopped".into()) }).unwrap();
        let mut world = World::new();
        world.insert_resource(agent);
        world.run_system_once(pump_events).unwrap();

        let agent = world.resource::<Agent>();
        assert!(agent.history.is_empty(), "the old run's history came back");
        assert!(agent.transcript.is_empty(), "the old run wrote into the new chat");
        assert!(!agent.running, "Send has to work again");
        assert!(!agent.discard_run);
    }

    #[test]
    fn a_normal_finish_keeps_its_history() {
        let mut agent = Agent::new(AgentConfig::default());
        let (tx, rx) = unbounded();
        agent.events = Some(rx);
        agent.running = true;
        tx.send(Event::Finished { history: vec![Message::user(vec![Block::Text("kept".into())])], error: None }).unwrap();
        let mut world = World::new();
        world.insert_resource(agent);
        world.run_system_once(pump_events).unwrap();
        assert_eq!(world.resource::<Agent>().history.len(), 1);
    }
}
