//! The agent loop: ask the model, run the tools it asks for, and repeat until it stops and the
//! checks let it. What may be called, and what must hold before stopping, come from outside.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, Sender};
use serde_json::Value;

use super::llm::{Block, Message, Provider, ToolDef};
use super::tools::{self, ToolJob, ToolOutput};
use super::{Event, images, toolmaker};
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
}

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
        let msg = match complete_cancellable(provider, &spec.system, &turn.history, &spec.tools, &turn.cancel) {
            Ok(Some(m)) => m,
            Ok(None) => return Some("stopped".to_string()),
            Err(e) => return Some(format!("{e:#}")),
        };
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

        let (tx, mcp, cancel, ev, log) = (&turn.tx, &turn.mcp, &turn.cancel, &turn.ev, &turn.log);
        let run = |name: &str, input: &Value| {
            if cancel.load(Ordering::Relaxed) {
                ToolOutput::err("cancelled by the user")
            } else if let Some(o) = mcp.call(name, input) {
                o
            } else {
                tools::call_logged(tx, name, input.clone(), false, Some(cancel), Some(log))
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
            if name == "perceive_photo" || name == "load_reference_image" {
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
    use serde_json::json;

    use super::*;

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
