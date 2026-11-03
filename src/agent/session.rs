//! Saving a chat as it goes, so a crash or a restart doesn't lose hours of work. One JSON line per
//! message, with images in their own files. Only complete rounds are written, so what is on disk
//! is always a conversation a provider would accept.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::ProviderKind;
use super::llm::{Block, Message, Role};
use super::{ChatItem, images};

const FILE: &str = "session.jsonl";
const META: &str = "chat.json";

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum SavedBlock {
    Text { text: String },
    Image { sha: String, mime: String },
    Tool { id: String, name: String, input: Value },
    Result { id: String, content: Vec<SavedBlock>, is_error: bool },
}

#[derive(Serialize, Deserialize)]
struct SavedMessage {
    user: bool,
    content: Vec<SavedBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw: Option<(ProviderKind, Value)>,
}

/// Image ids written in the text that have to mean the same thing after a restart.
#[derive(Serialize, Deserialize, Default)]
struct Meta {
    #[serde(default)]
    kind: Option<ProviderKind>,
    #[serde(default)]
    model: String,
    /// (image id, file, mime).
    #[serde(default)]
    images: Vec<(String, String, String)>,
}

/// Where this chat's files live.
pub fn chat_dir(chat_id: &str) -> PathBuf {
    super::gate::workspace_root().join(chat_id)
}

pub fn new_chat_id() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("chat-{ms}")
}

fn save_block(dir: &Path, b: &Block) -> Result<SavedBlock> {
    Ok(match b {
        Block::Text(text) => SavedBlock::Text { text: text.clone() },
        Block::Image { data, mime } => {
            let sha = sim_core::provision::sha256_hex(data);
            let path = dir.join("img").join(&sha);
            if !path.exists() {
                std::fs::create_dir_all(dir.join("img"))?;
                std::fs::write(&path, data.as_slice())?;
            }
            SavedBlock::Image { sha, mime: mime.clone() }
        }
        Block::ToolUse { id, name, input } => SavedBlock::Tool { id: id.clone(), name: name.clone(), input: input.clone() },
        Block::ToolResult { id, content, is_error } => SavedBlock::Result { id: id.clone(), content: content.iter().map(|c| save_block(dir, c)).collect::<Result<_>>()?, is_error: *is_error },
    })
}

fn load_block(dir: &Path, b: SavedBlock) -> Block {
    match b {
        SavedBlock::Text { text } => Block::Text(text),
        SavedBlock::Image { sha, mime } => match std::fs::read(dir.join("img").join(&sha)) {
            Ok(data) => Block::Image { data: Arc::new(data), mime },
            // Cleared by hand. The conversation still has to load.
            Err(_) => Block::Text("[an image from earlier is no longer on disk]".into()),
        },
        SavedBlock::Tool { id, name, input } => Block::ToolUse { id, name, input },
        SavedBlock::Result { id, content, is_error } => Block::ToolResult { id, content: content.into_iter().map(|c| load_block(dir, c)).collect(), is_error },
    }
}

fn line_of(dir: &Path, m: &Message) -> Result<String> {
    let saved = SavedMessage { user: m.role == Role::User, content: m.content.iter().map(|b| save_block(dir, b)).collect::<Result<_>>()?, raw: m.raw.clone() };
    Ok(serde_json::to_string(&saved)?)
}

/// `img12`-style ids in a text.
fn image_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("img") {
        let tail = &rest[at + 3..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            out.push(format!("img{digits}"));
        }
        rest = tail;
    }
    out
}

fn ids_in(content: &[Block], out: &mut HashSet<String>) {
    for b in content {
        match b {
            Block::Text(t) => out.extend(image_ids(t)),
            Block::ToolResult { content, .. } => ids_in(content, out),
            Block::ToolUse { input, .. } => out.extend(image_ids(&input.to_string())),
            Block::Image { .. } => {}
        }
    }
}

/// Writes a chat's messages as they become final.
pub struct SessionWriter {
    dir: PathBuf,
    written: usize,
    meta: Meta,
}

impl SessionWriter {
    /// Starts or continues a chat. `written`: how many messages of the history are on disk.
    pub fn open(chat_id: &str, kind: ProviderKind, model: &str, written: usize) -> Result<Self> {
        let dir = chat_dir(chat_id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut meta: Meta = std::fs::read_to_string(dir.join(META)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        meta.kind = Some(kind);
        meta.model = model.to_string();
        Ok(Self { dir, written, meta })
    }

    fn save_meta(&mut self, history: &[Message]) -> Result<()> {
        let mut ids = HashSet::new();
        for m in history {
            ids_in(&m.content, &mut ids);
        }
        for id in ids {
            if self.meta.images.iter().any(|(known, _, _)| *known == id) {
                continue;
            }
            if let Some((data, mime)) = images::get(&id) {
                let sha = sim_core::provision::sha256_hex(&data);
                let path = self.dir.join("img").join(&sha);
                if !path.exists() {
                    std::fs::create_dir_all(self.dir.join("img"))?;
                    std::fs::write(&path, data.as_slice())?;
                }
                self.meta.images.push((id, sha, mime));
            }
        }
        write_atomic(&self.dir.join(META), serde_json::to_string_pretty(&self.meta)?.as_bytes())
    }

    /// Adds what's new. Call only between rounds: an unanswered tool call must not reach the disk.
    pub fn sync(&mut self, history: &[Message]) -> Result<()> {
        if history.len() < self.written {
            return self.rewrite(history);
        }
        if history.len() == self.written {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join(FILE))?;
        for m in &history[self.written..] {
            writeln!(file, "{}", line_of(&self.dir, m)?)?;
        }
        self.written = history.len();
        self.save_meta(history)
    }

    /// Replaces the file, after compaction changed earlier messages.
    pub fn rewrite(&mut self, history: &[Message]) -> Result<()> {
        let mut text = String::new();
        for m in history {
            text.push_str(&line_of(&self.dir, m)?);
            text.push('\n');
        }
        write_atomic(&self.dir.join(FILE), text.as_bytes())?;
        self.written = history.len();
        self.save_meta(history)
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A saved chat, as far as it can be offered for resuming.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedChat {
    pub id: String,
    /// The first words of the request.
    pub title: String,
    pub modified: std::time::SystemTime,
}

/// Reads a chat back. A torn last line (a crash while writing) is left out, along with anything
/// after the last point the conversation was complete.
pub fn load(chat_id: &str) -> Result<(Vec<Message>, Option<(ProviderKind, String)>)> {
    let dir = chat_dir(chat_id);
    let text = std::fs::read_to_string(dir.join(FILE)).with_context(|| format!("reading the chat in {}", dir.display()))?;
    let mut history = Vec::new();
    for line in text.lines() {
        let Ok(saved) = serde_json::from_str::<SavedMessage>(line) else { break };
        let content = saved.content.into_iter().map(|b| load_block(&dir, b)).collect();
        history.push(Message { role: if saved.user { Role::User } else { Role::Assistant }, content, raw: saved.raw, usage: None });
    }
    while super::context::validate(&history).is_err() && !history.is_empty() {
        history.pop();
    }
    let meta: Meta = std::fs::read_to_string(dir.join(META)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    // The ids in the history's text must find their pictures, and new ones mustn't reuse them.
    for (id, sha, mime) in &meta.images {
        if let Ok(data) = std::fs::read(dir.join("img").join(sha)) {
            images::restore(id, Arc::new(data), mime);
        }
    }
    let model = meta.kind.map(|k| (k, meta.model.clone()));
    Ok((history, model))
}

/// The most recently used chat that has anything in it.
pub fn latest() -> Option<SavedChat> {
    list(&super::gate::workspace_root()).into_iter().find(|c| c.id.starts_with("chat-"))
}

fn list(root: &Path) -> Vec<SavedChat> {
    let mut chats: Vec<SavedChat> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().to_string();
            let file = e.path().join(FILE);
            let modified = std::fs::metadata(&file).ok()?.modified().ok()?;
            let first = std::fs::read_to_string(&file).ok()?.lines().next().map(str::to_string)?;
            let saved: SavedMessage = serde_json::from_str(&first).ok()?;
            let words = saved.content.iter().find_map(|b| if let SavedBlock::Text { text } = b { Some(text.clone()) } else { None })?;
            Some(SavedChat { id, title: words.chars().take(60).collect(), modified })
        })
        .collect();
    chats.sort_by(|a, b| b.modified.cmp(&a.modified));
    chats
}

/// What the panel shows for a loaded conversation. The app's own notes aren't the user's words.
pub fn transcript_of(history: &[Message]) -> Vec<ChatItem> {
    let is_note = |t: &str| t.starts_with("[What the user has on screen") || t.starts_with("[Automatic") || t.starts_with("[Earlier in this conversation") || t.starts_with("[reference image");
    let mut out: Vec<ChatItem> = Vec::new();
    for m in history {
        let thought = m.thinking();
        if !thought.is_empty() {
            out.push(ChatItem::Thinking(thought));
        }
        for b in &m.content {
            match (m.role, b) {
                (Role::User, Block::Text(t)) if !is_note(t) => out.push(ChatItem::User { text: t.clone(), images: Vec::new() }),
                (Role::Assistant, Block::Text(t)) if !t.trim().is_empty() => out.push(ChatItem::Assistant(t.clone())),
                (Role::Assistant, Block::ToolUse { name, input, .. }) => out.push(ChatItem::Tool { name: name.clone(), input: input.to_string(), output: String::new(), is_error: false }),
                (Role::User, Block::ToolResult { content, is_error, .. }) => {
                    // Results come in the order of the calls, each to the first still unanswered.
                    if let Some(ChatItem::Tool { output, is_error: failed, .. }) = out.iter_mut().find(|c| matches!(c, ChatItem::Tool { output, .. } if output.is_empty())) {
                        *output = content.iter().map(|c| if let Block::Text(t) = c { t.as_str() } else { "[image]" }).collect::<Vec<_>>().join(" ");
                        *failed = *is_error;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A chat folder of its own: tests must not touch the user's chats.
    fn fresh(name: &str) -> String {
        let id = format!("test-{name}-{}", std::process::id());
        let _ = std::fs::remove_dir_all(chat_dir(&id));
        id
    }

    fn sample() -> Vec<Message> {
        vec![
            Message::user(vec![Block::Text("build it".into()), Block::Image { data: Arc::new(vec![9, 8, 7]), mime: "image/png".into() }]),
            Message { role: Role::Assistant, content: vec![Block::ToolUse { id: "t1".into(), name: "get_scene".into(), input: json!({ "x": 1 }) }], raw: Some((ProviderKind::Anthropic, json!([{ "type": "thinking", "signature": "sig" }]))), usage: None },
            Message::user(vec![Block::ToolResult { id: "t1".into(), content: vec![Block::Text("{}".into()), Block::Image { data: Arc::new(vec![1, 2]), mime: "image/jpeg".into() }], is_error: true }]),
        ]
    }

    fn cleanup(id: &str) {
        let _ = std::fs::remove_dir_all(chat_dir(id));
    }

    #[test]
    fn a_chat_comes_back_as_it_was() {
        let id = fresh("roundtrip");
        let mut w = SessionWriter::open(&id, ProviderKind::Anthropic, "claude-opus-5-5", 0).unwrap();
        w.sync(&sample()).unwrap();
        let (h, model) = load(&id).unwrap();
        assert_eq!(model, Some((ProviderKind::Anthropic, "claude-opus-5-5".to_string())));
        assert_eq!(h.len(), 3);
        assert!(super::super::context::validate(&h).is_ok());
        assert!(matches!(&h[0].content[1], Block::Image { data, mime } if **data == vec![9, 8, 7] && mime == "image/png"));
        assert_eq!(h[1].raw.as_ref().unwrap().1[0]["signature"], "sig", "reasoning replays exactly");
        let Block::ToolResult { content, is_error, .. } = &h[2].content[0] else { panic!() };
        assert!(*is_error);
        assert!(matches!(&content[1], Block::Image { data, .. } if **data == vec![1, 2]));
        cleanup(&id);
    }

    #[test]
    fn it_appends_only_whats_new_and_rewrites_after_compaction() {
        let id = fresh("append");
        let mut w = SessionWriter::open(&id, ProviderKind::OpenAi, "m", 0).unwrap();
        let mut h = sample();
        w.sync(&h[..1]).unwrap();
        w.sync(&h).unwrap();
        w.sync(&h).unwrap();
        let lines = std::fs::read_to_string(chat_dir(&id).join(FILE)).unwrap();
        assert_eq!(lines.lines().count(), 3, "no message twice");

        h.remove(0);
        h.insert(0, Message::user(vec![Block::Text("short".into())]));
        w.rewrite(&h).unwrap();
        assert_eq!(load(&id).unwrap().0.len(), 3);
        assert!(matches!(&load(&id).unwrap().0[0].content[0], Block::Text(t) if t == "short"));
        cleanup(&id);
    }

    #[test]
    fn a_crash_while_writing_loses_the_last_line_not_the_chat() {
        let id = fresh("torn");
        let mut w = SessionWriter::open(&id, ProviderKind::OpenAi, "m", 0).unwrap();
        w.sync(&sample()).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(chat_dir(&id).join(FILE)).unwrap();
        write!(f, "{{\"user\":false,\"cont").unwrap();
        drop(f);
        assert_eq!(load(&id).unwrap().0.len(), 3);
        cleanup(&id);
    }

    #[test]
    fn a_loaded_history_is_always_one_a_provider_would_accept() {
        let id = fresh("incomplete");
        let mut w = SessionWriter::open(&id, ProviderKind::OpenAi, "m", 0).unwrap();
        // A tool call with no result, as if the app died in the middle of a round.
        w.sync(&sample()[..2]).unwrap();
        let (h, _) = load(&id).unwrap();
        assert_eq!(h.len(), 1, "back to the last complete point");
        cleanup(&id);
    }

    #[test]
    fn a_missing_image_doesnt_stop_the_chat_loading() {
        let id = fresh("noimage");
        let mut w = SessionWriter::open(&id, ProviderKind::OpenAi, "m", 0).unwrap();
        w.sync(&sample()).unwrap();
        std::fs::remove_dir_all(chat_dir(&id).join("img")).unwrap();
        let (h, _) = load(&id).unwrap();
        assert!(matches!(&h[0].content[1], Block::Text(t) if t.contains("no longer on disk")));
        cleanup(&id);
    }

    /// Ids sit in the history's text, and the store restarts empty with its counter at 1. Without
    /// this, "img3" would find nothing, or a later image would take its id.
    #[test]
    fn image_ids_keep_their_meaning_after_a_restart() {
        let id = fresh("ids");
        let image = images::put(Arc::new(vec![4, 5, 6]), "image/png");
        let number: usize = image.trim_start_matches("img").parse().unwrap();
        let h = vec![Message::user(vec![Block::Text(format!("[reference image, image_id: {image}]"))])];
        let mut w = SessionWriter::open(&id, ProviderKind::OpenAi, "m", 0).unwrap();
        w.sync(&h).unwrap();
        images::forget(&image);
        assert!(images::get(&image).is_none());
        load(&id).unwrap();
        assert_eq!(*images::get(&image).unwrap().0, vec![4, 5, 6]);
        let next: usize = images::put(Arc::new(vec![0]), "image/png").trim_start_matches("img").parse().unwrap();
        assert!(next > number, "a new image must not reuse an old id");
        cleanup(&id);
    }

    #[test]
    fn finding_image_ids() {
        assert_eq!(image_ids("see img12 and img3, not imgx or image"), ["img12", "img3"]);
        assert!(image_ids("nothing").is_empty());
    }

    #[test]
    fn the_newest_chat_comes_first_and_empty_folders_are_skipped() {
        let root = std::env::temp_dir().join(format!("sense-sim-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("empty")).unwrap();
        for (name, words) in [("old", "the old one"), ("new", "the new one")] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            let line = serde_json::to_string(&SavedMessage { user: true, content: vec![SavedBlock::Text { text: words.into() }], raw: None }).unwrap();
            std::fs::write(root.join(name).join(FILE), format!("{line}\n")).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let chats = list(&root);
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0].title, "the new one");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_transcript_shows_the_users_words_and_not_the_apps_notes() {
        let h = vec![
            Message::user(vec![Block::Text("[reference image, image_id: img1]".into()), Block::Text("why is this wrong?".into()), Block::Text("[What the user has on screen, added by the app.]".into())]),
            Message { role: Role::Assistant, content: vec![Block::Text("Let me look.".into()), Block::ToolUse { id: "t".into(), name: "get_scene".into(), input: json!({}) }], raw: None, usage: None },
            Message::user(vec![Block::ToolResult { id: "t".into(), content: vec![Block::Text("the scene".into())], is_error: false }]),
            Message::user(vec![Block::Text("[Automatic check] fix it".into())]),
        ];
        let items = transcript_of(&h);
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], ChatItem::User { text, .. } if text == "why is this wrong?"));
        assert!(matches!(&items[1], ChatItem::Assistant(t) if t == "Let me look."));
        assert!(matches!(&items[2], ChatItem::Tool { name, output, .. } if name == "get_scene" && output == "the scene"));
    }

    #[test]
    fn a_reopened_chat_shows_what_the_model_was_thinking() {
        let raw = json!([{ "type": "thinking", "thinking": "Check the loop first.", "signature": "sig" }, { "type": "text", "text": "Done." }]);
        let reply = Message { role: Role::Assistant, content: vec![Block::Text("Done.".into())], raw: Some((ProviderKind::Anthropic, raw)), usage: None };
        let items = transcript_of(&[reply]);
        assert!(matches!(&items[0], ChatItem::Thinking(t) if t == "Check the loop first."));
        assert!(matches!(&items[1], ChatItem::Assistant(t) if t == "Done."));
    }
}
