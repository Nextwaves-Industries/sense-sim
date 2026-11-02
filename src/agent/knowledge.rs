//! The assistant's knowledge base: the corpus built into the app plus the
//! user's own folder next to `agent.json`. Logic lives in the `knowledge` crate.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use knowledge::Index;
use knowledge::api::{self, AutoContext};
use serde_json::Value;

use super::config::AgentConfig;
use super::llm::ToolDef;
use super::tools::ToolOutput;

struct State {
    index: Arc<Index>,
    /// Files in the user's folder that could not be used, with the reason.
    skipped: Vec<(String, String)>,
}

static STATE: RwLock<Option<State>> = RwLock::new(None);

/// The user's documents: `.md` or `.txt`, one sub-folder per domain.
pub fn user_dir() -> PathBuf {
    AgentConfig::path().parent().map(PathBuf::from).unwrap_or_default().join("knowledge")
}

fn load() -> State {
    let mut docs = knowledge::builtin_docs();
    let user = knowledge::load_dir(&user_dir());
    docs.extend(user.docs);
    State { index: Arc::new(Index::build(docs)), skipped: user.skipped }
}

pub fn index() -> Arc<Index> {
    if let Some(s) = STATE.read().unwrap().as_ref() {
        return s.index.clone();
    }
    let mut guard = STATE.write().unwrap();
    guard.get_or_insert_with(load).index.clone()
}

/// Re-reads the user's folder.
pub fn reload() {
    let fresh = load();
    *STATE.write().unwrap() = Some(fresh);
}

pub fn skipped() -> Vec<(String, String)> {
    index();
    STATE.read().unwrap().as_ref().map(|s| s.skipped.clone()).unwrap_or_default()
}

pub fn tool_defs() -> Vec<ToolDef> {
    api::tool_specs().into_iter().map(|t| ToolDef { name: t.name.into(), description: t.description.into(), schema: t.schema }).collect()
}

pub fn is_tool(name: &str) -> bool {
    api::TOOL_NAMES.contains(&name)
}

pub fn run(name: &str, input: &Value) -> ToolOutput {
    match api::run_tool(&index(), name, input) {
        Ok(text) => ToolOutput::text(text),
        Err(e) => ToolOutput::err(e),
    }
}

pub fn auto_context(message: &str) -> Option<AutoContext> {
    api::auto_context(&index(), message)
}
