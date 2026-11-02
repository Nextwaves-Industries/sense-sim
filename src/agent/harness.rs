//! The agent loop: ask the model, run the tools it asks for, and repeat until it stops and the
//! checks let it. What may be called, and what must hold before stopping, come from outside.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, Sender};
use serde_json::Value;

use super::budget::Budget;
use super::context::{self, Needed};
use super::gate::{CallCtx, Origin};
use super::llm::{Block, Message, Provider, ToolDef};
use super::session::SessionWriter;
use super::tools::{self, ToolJob, ToolOutput};
use super::{Event, Question, images, toolmaker, workspace};
use crate::mcp::client::McpHub;

/// Who made a tool call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Who {
    /// The model of this loop, directly.
    Model,
    /// A step of a `custom_` macro the model called.
    Macro,
}

/// One tool call that actually ran.
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub who: Who,
    pub name: String,
    pub input: Value,
    /// The text of the result; images appear as `[image]`.
    pub output: String,
    pub is_error: bool,
}

/// Every tool call of a turn, however it was made. A check that only saw the model's own calls
/// would miss a macro changing an antenna, so checks read this.
#[derive(Clone, Default)]
pub struct TurnLog(Arc<Mutex<LogState>>);

#[derive(Default)]
struct LogState {
    entries: Vec<LogEntry>,
    /// How many of them the checks have seen.
    seen: usize,
}

impl TurnLog {
    pub fn push(&self, who: Who, name: &str, input: &Value, out: &ToolOutput) {
        let entry = LogEntry { who, name: name.to_string(), input: input.clone(), output: out.summary(), is_error: out.is_error };
        if let Ok(mut log) = self.0.lock() {
            log.entries.push(entry);
        }
    }

    /// The calls logged since the last time this was asked.
    pub fn unseen(&self) -> Vec<LogEntry> {
        let Ok(mut log) = self.0.lock() else { return Vec::new() };
        let new = log.entries[log.seen..].to_vec();
        log.seen = log.entries.len();
        new
    }

    /// Whether a script ran in this turn, so nothing since can count as measured.
    pub fn ran_script(&self) -> bool {
        self.0.lock().map(|l| l.entries.iter().any(|e| e.name == "python_run")).unwrap_or(true)
    }

    #[cfg(test)]
    pub fn entries(&self) -> Vec<LogEntry> {
        self.0.lock().map(|l| l.entries.clone()).unwrap_or_default()
    }
}

/// What one run of the loop is asked to be.
pub(super) struct AgentSpec {
    pub system: String,
    pub max_steps: usize,
    pub tools: Vec<ToolDef>,
}

/// The state of one run.
pub(super) struct Turn {
    pub history: Vec<Message>,
    pub log: TurnLog,
    pub cancel: Arc<AtomicBool>,
    pub tx: Sender<ToolJob>,
    pub mcp: Arc<McpHub>,
    pub ev: Sender<Event>,
    pub budget: Arc<Budget>,
    pub session: Option<SessionWriter>,
    /// How full the provider last said the context was.
    pub context_used: Option<u64>,
    /// What the conversation has to fit in. 0: never compact.
    pub context_limit: u64,
    /// The chat's working folder.
    pub files: Option<PathBuf>,
    save_warned: bool,
}

impl Turn {
    pub fn new(history: Vec<Message>, cancel: Arc<AtomicBool>, tx: Sender<ToolJob>, mcp: Arc<McpHub>, ev: Sender<Event>) -> Self {
        Self { history, log: TurnLog::default(), cancel, tx, mcp, ev, budget: Arc::default(), session: None, context_used: None, context_limit: 0, files: None, save_warned: false }
    }

    /// Writes what's final. Called between rounds, so no unanswered tool call reaches the disk.
    /// A chat that can't be saved shouldn't stop the work, but the user should know.
    pub fn save(&mut self) {
        let Some(session) = &mut self.session else { return };
        if let Err(e) = session.sync(&self.history)
            && !self.save_warned
        {
            self.save_warned = true;
            let _ = self.ev.send(Event::Info(format!("This chat can't be saved, so it won't survive a restart: {e:#}")));
        }
    }
}

/// Keeps the conversation within what the model can hold.
fn compact(provider: &Arc<dyn Provider>, turn: &mut Turn) {
    if turn.context_limit == 0 {
        return;
    }
    let used = turn.context_used.unwrap_or_else(|| context::estimate(&turn.history));
    match context::trim(&mut turn.history, used, turn.context_limit) {
        Needed::Nothing | Needed::Trimmed { images: 0, results: 0 } => return,
        Needed::Trimmed { images, results } => {
            let _ = turn.ev.send(Event::Info(format!("Freed space for the model: removed {images} old images and shortened {results} old tool results.")));
        }
        Needed::Summarize { cut, .. } => {
            let _ = turn.ev.send(Event::Info("The conversation is getting long for this model, so its earlier part is being condensed into notes.".into()));
            let ask = Message::user(vec![Block::Text(context::as_text(&turn.history[..cut]))]);
            let notes = match complete_cancellable(provider, context::SUMMARY_SYSTEM, &[ask], &[], &turn.cancel) {
                Ok(Some(m)) => Some(m.text()).filter(|t| !t.trim().is_empty()),
                _ => None,
            };
            if notes.is_none() {
                let _ = turn.ev.send(Event::Info("Notes on the earlier work couldn't be written, so it may need rechecking. Evidence labels and the plan were kept.".into()));
            }
            context::replace_with_summary(&mut turn.history, cut, notes.as_deref());
        }
    }
    // Until the next reply says otherwise, the old figure is wrong.
    turn.context_used = None;
    if let Some(session) = &mut turn.session {
        let _ = session.rewrite(&turn.history);
    }
}

/// The budget ran out: whether to keep going is the user's decision, and their money.
fn may_continue(turn: &Turn, why: &str) -> bool {
    let (tx, rx) = crossbeam_channel::unbounded();
    let question = Question { text: format!("{why} Keep going?"), options: vec![KEEP_GOING.to_string(), "Stop here".to_string()], candidates: Vec::new(), allow_text: false, input: String::new(), answer: None, reply: Some(tx), approval: true };
    if turn.ev.send(Event::Question(question)).is_err() {
        return false;
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => return answer.summary() == format!("User answered: {KEEP_GOING}"),
            Err(RecvTimeoutError::Disconnected) => return false,
            Err(RecvTimeoutError::Timeout) if turn.cancel.load(Ordering::Relaxed) => return false,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

const KEEP_GOING: &str = "Keep going";

/// What happens when the model wants to stop.
pub(super) enum OnStop {
    Finish,
    /// Not yet: this goes back to the model as a user message.
    SendBack(Vec<Block>),
}

pub(super) trait StopChecks {
    /// The model has asked for these tools, before they run.
    fn before_tools(&mut self, names: &[&str]);
    /// A tool call finished, the model's own or one nested inside it.
    fn observe(&mut self, entry: &LogEntry);
    fn on_stop(&mut self, provider: &dyn Provider, turn: &Turn) -> OnStop;
}

/// A model call that Stop can interrupt. The request can't be cancelled, so it finishes on its
/// own thread and its reply is dropped. `None` means the user stopped.
pub(super) fn complete_cancellable(provider: &Arc<dyn Provider>, system: &str, history: &[Message], tools: &[ToolDef], cancel: &AtomicBool) -> anyhow::Result<Option<Message>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let (p, s, h, t) = (provider.clone(), system.to_string(), history.to_vec(), tools.to_vec());
    std::thread::spawn(move || {
        let _ = tx.send(p.complete(&s, &h, &t));
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(reply) => return reply.map(Some),
            // Its thread panicked: the one place a parse bug would otherwise end the whole run.
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("the model call ended without a reply"),
            Err(RecvTimeoutError::Timeout) if cancel.load(Ordering::Relaxed) => return Ok(None),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Runs until the model stops and `checks` agree, or something ends the turn. Returns why it
/// ended early, if it did. The history is left in `turn`.
pub(super) fn run_loop(provider: &Arc<dyn Provider>, spec: &mut AgentSpec, turn: &mut Turn, checks: &mut dyn StopChecks) -> Option<String> {
    for step in 0.. {
        if turn.cancel.load(Ordering::Relaxed) {
            return Some("stopped".to_string());
        }
        if step >= spec.max_steps {
            return Some(format!("stopped after {} steps (raise max_steps in agent.json)", spec.max_steps));
        }
        if let Some(why) = turn.budget.exceeded() {
            if !may_continue(turn, &why) {
                return Some(format!("{why} Stopped there."));
            }
            turn.budget.extend();
        }
        compact(provider, turn);
        // Better to stop and say so than send something the provider rejects with a riddle.
        if let Err(e) = context::validate(&turn.history) {
            return Some(format!("The conversation got into a state no provider would accept, so the assistant stopped: {e}"));
        }
        turn.save();
        let msg = match complete_cancellable(provider, &spec.system, &turn.history, &spec.tools, &turn.cancel) {
            Ok(Some(m)) => m,
            Ok(None) => return Some("stopped".to_string()),
            Err(e) => return Some(format!("{e:#}")),
        };
        if let Some(u) = msg.usage {
            turn.context_used = Some(u.input);
        }
        let text = msg.text();
        if !text.trim().is_empty() {
            let _ = turn.ev.send(Event::Assistant(text));
        }
        let uses = msg.tool_uses();
        turn.history.push(msg);
        if uses.is_empty() {
            if turn.cancel.load(Ordering::Relaxed) {
                return None;
            }
            match checks.on_stop(provider.as_ref(), turn) {
                OnStop::SendBack(content) => {
                    turn.history.push(Message::user(content));
                    continue;
                }
                OnStop::Finish => return None,
            }
        }
        // A tool made this turn can be called on the next one.
        let tools_changed = uses.iter().any(|(_, n, _)| n == "define_tool" || n == "delete_tool");
        checks.before_tools(&uses.iter().map(|(_, n, _)| n.as_str()).collect::<Vec<_>>());

        let (tx, mcp, cancel, ev, log, files) = (&turn.tx, &turn.mcp, &turn.cancel, &turn.ev, &turn.log, &turn.files);
        // Everything goes through the bridge, where the gate is: external tools too.
        let run = |name: &str, input: &Value| {
            if cancel.load(Ordering::Relaxed) {
                ToolOutput::err("cancelled by the user")
            } else {
                let ctx = CallCtx { origin: Origin::Assistant, log: Some(log.clone()), cancel: Some(cancel.clone()) };
                match files.as_deref().filter(|_| workspace::is_tool(name)) {
                    // These run here, not on the app's main thread, once the gate has agreed.
                    Some(files) => {
                        let input = workspace::resolve(input, files);
                        let allowed = tools::gate_only(tx, name, input.clone(), ctx.clone());
                        if allowed.is_error { allowed } else { workspace::run(name, &input, files, tx, &ctx) }
                    }
                    None => tools::call_ctx(tx, name, input.clone(), ctx),
                }
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
            // What a macro ran is already logged, so the checks see its steps first.
            log.push(Who::Model, &name, &input, &out);
            for entry in log.unseen() {
                checks.observe(&entry);
            }
            if name == "perceive_photo" || name == "load_reference_image" || name == "open_file" {
                for b in &out.content {
                    if let Block::Image { data, mime } = b {
                        let _ = ev.send(Event::Image(images::put(data.clone(), mime)));
                    }
                }
            }
            results.push(Block::ToolResult { id, content: out.content, is_error: out.is_error });
        }
        turn.history.push(Message::user(results));
        if tools_changed {
            spec.tools.retain(|d| !d.name.starts_with(toolmaker::PREFIX));
            spec.tools.extend(toolmaker::defs());
        }
    }
    unreachable!("the step limit ends the loop")
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;
    use serde_json::json;

    use super::*;
    use crate::agent::budget::{Limits, Metered};
    use crate::agent::llm::{Role, Usage};

    /// Replies from a list, each charged the same, and remembers what it was sent.
    struct Script {
        replies: Mutex<Vec<Message>>,
        usage: Usage,
        seen: Mutex<Vec<(String, Vec<Message>)>>,
    }

    impl Provider for Script {
        fn complete(&self, system: &str, messages: &[Message], _: &[ToolDef]) -> anyhow::Result<Message> {
            self.seen.lock().unwrap().push((system.to_string(), messages.to_vec()));
            let mut replies = self.replies.lock().unwrap();
            let mut reply = if replies.is_empty() { text("the end") } else { replies.remove(0) };
            reply.usage = Some(self.usage);
            Ok(reply)
        }
    }

    fn text(t: &str) -> Message {
        Message { role: Role::Assistant, content: vec![Block::Text(t.into())], raw: None, usage: None }
    }
    fn tool(id: &str) -> Message {
        Message { role: Role::Assistant, content: vec![Block::ToolUse { id: id.into(), name: "get_scene".into(), input: json!({}) }], raw: None, usage: None }
    }

    #[derive(Default)]
    struct NoChecks;
    impl StopChecks for NoChecks {
        fn before_tools(&mut self, _: &[&str]) {}
        fn observe(&mut self, _: &LogEntry) {}
        fn on_stop(&mut self, _: &dyn Provider, _: &Turn) -> OnStop {
            OnStop::Finish
        }
    }

    /// Runs the real loop on a script. A thread answers tools, and returns the turn and events.
    fn run(script: Arc<Script>, budget: Limits, context_limit: u64, answer: Option<&'static str>) -> (Turn, Option<String>, Vec<Event>) {
        let (tx, jobs) = unbounded::<ToolJob>();
        std::thread::spawn(move || {
            for job in jobs {
                let _ = job.reply.send(ToolOutput::text("x".repeat(4000)));
            }
        });
        let (ev, events) = unbounded();
        let budget = Arc::new(Budget::new(budget));
        let provider: Arc<dyn Provider> = Arc::new(Metered { inner: script, budget: budget.clone(), ev: ev.clone() });
        let history = vec![Message::user(vec![Block::Text("the user's request".into())])];
        let mut turn = Turn::new(history, Arc::default(), tx, Arc::new(McpHub::default()), ev);
        turn.budget = budget;
        turn.context_limit = context_limit;
        let mut spec = AgentSpec { system: "main".into(), max_steps: 50, tools: Vec::new() };
        // Stands in for the user at the question card.
        let (seen_tx, seen_rx) = unbounded();
        std::thread::spawn(move || {
            for e in events {
                if let Event::Question(mut q) = e {
                    if let Some(a) = answer {
                        q.answer(a.to_string());
                    }
                    let _ = seen_tx.send(Event::Question(q));
                } else {
                    let _ = seen_tx.send(e);
                }
            }
        });
        let error = run_loop(&provider, &mut spec, &mut turn, &mut NoChecks);
        let mut seen: Vec<Event> = Vec::new();
        while let Ok(e) = seen_rx.recv_timeout(Duration::from_millis(50)) {
            seen.push(e);
        }
        (turn, error, seen)
    }

    fn script(replies: Vec<Message>, usage: Usage) -> Arc<Script> {
        Arc::new(Script { replies: Mutex::new(replies), usage, seen: Mutex::default() })
    }

    #[test]
    fn the_budget_stops_a_run_and_says_why() {
        let s = script(vec![tool("a"), tool("b"), tool("c"), text("done")], Usage { input: 600, cached: 0, output: 0 });
        let (_, error, events) = run(s.clone(), Limits { tokens: 1000, ..Default::default() }, 0, Some("Stop here"));
        let why = error.expect("it stopped");
        assert!(why.contains("tokens") && why.contains("Stopped there"), "{why}");
        assert_eq!(s.seen.lock().unwrap().len(), 2, "no more calls once it was over");
        assert!(events.iter().any(|e| matches!(e, Event::Question(q) if q.text.contains("Keep going?"))));
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Usage { .. })).count(), 2);
    }

    #[test]
    fn keep_going_really_keeps_going() {
        let s = script(vec![tool("a"), tool("b"), tool("c"), text("done")], Usage { input: 600, cached: 0, output: 0 });
        let (turn, error, _) = run(s.clone(), Limits { tokens: 1000, ..Default::default() }, 0, Some("Keep going"));
        assert!(error.is_none(), "{error:?}");
        assert_eq!(s.seen.lock().unwrap().len(), 4);
        assert!(matches!(&turn.history.last().unwrap().content[0], Block::Text(t) if t == "done"));
    }

    #[test]
    fn a_long_run_compacts_and_stays_valid() {
        let mut replies: Vec<Message> = (0..30).map(|i| tool(&format!("t{i}"))).collect();
        replies.push(text("done"));
        // Reports a context near the limit, as a long task would.
        let s = script(replies, Usage { input: 9000, cached: 0, output: 10 });
        let (turn, error, events) = run(s.clone(), Limits::default(), 10_000, None);
        assert!(error.is_none(), "{error:?}");
        assert!(context::validate(&turn.history).is_ok());
        assert!(turn.history.len() < 62, "it got shorter: {}", turn.history.len());
        assert!(events.iter().any(|e| matches!(e, Event::Info(t) if t.contains("condensed"))), "the user is told");
        let seen = s.seen.lock().unwrap();
        assert!(seen.iter().any(|(system, _)| system == context::SUMMARY_SYSTEM), "a model wrote the notes");
        for (system, messages) in seen.iter().filter(|(system, _)| system == "main") {
            assert!(context::validate(messages).is_ok(), "{system}: every request sent was valid");
        }
        // The request survives however often it's compacted.
        assert!(matches!(&turn.history[0].content[0], Block::Text(t) if t.contains("the user's request")));
    }

    #[test]
    fn no_limit_means_no_compaction_and_no_extra_calls() {
        let s = script(vec![tool("a"), text("done")], Usage { input: 900_000, cached: 0, output: 1 });
        let (turn, _, _) = run(s.clone(), Limits::default(), 0, None);
        assert_eq!(turn.history.len(), 4);
        assert_eq!(s.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn the_log_hands_out_each_call_once() {
        let log = TurnLog::default();
        log.push(Who::Macro, "antenna_set", &json!({}), &ToolOutput::text("a"));
        log.push(Who::Model, "custom_x", &json!({}), &ToolOutput::err("b"));
        let first = log.unseen();
        assert_eq!(first.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["antenna_set", "custom_x"]);
        assert!(first[1].is_error);
        assert!(log.unseen().is_empty());
        log.push(Who::Model, "get_scene", &json!({}), &ToolOutput::text("c"));
        assert_eq!(log.unseen().len(), 1);
        assert_eq!(log.entries().len(), 3, "the whole record stays");
    }
}
