//! Provider-neutral chat types and the two wire formats: Anthropic Messages
//! and OpenAI-compatible chat completions (OpenAI, Meta Muse, Gemini, Grok, DeepSeek,
//! OpenRouter, Ollama, LM Studio, vLLM).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};

use super::config::{ProviderCfg, ProviderKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub enum Block {
    Text(String),
    Image { data: Arc<Vec<u8>>, mime: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { id: String, content: Vec<Block>, is_error: bool },
}

#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
    /// Provider-native content of an assistant turn (e.g. Anthropic thinking
    /// blocks), replayed verbatim to the same provider.
    pub raw: Option<(ProviderKind, Value)>,
    /// What this reply cost, if the provider said.
    pub usage: Option<Usage>,
}

/// Tokens one model call used.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Usage {
    /// All input, however it was charged: this is also how full the model's context is.
    pub input: u64,
    /// The part of input read from the provider's cache.
    pub cached: u64,
    pub output: u64,
}

impl Usage {
    /// Tokens charged at the full rate.
    pub fn billable(&self) -> u64 {
        self.input.saturating_sub(self.cached) + self.output
    }
}

fn anthropic_usage(v: &Value) -> Option<Usage> {
    let u = &v["usage"];
    let (read, written) = (u["cache_read_input_tokens"].as_u64().unwrap_or(0), u["cache_creation_input_tokens"].as_u64().unwrap_or(0));
    Some(Usage { input: u["input_tokens"].as_u64()? + read + written, cached: read, output: u["output_tokens"].as_u64().unwrap_or(0) })
}

fn openai_usage(v: &Value) -> Option<Usage> {
    let u = &v["usage"];
    let cached = u["prompt_tokens_details"]["cached_tokens"].as_u64().or_else(|| u["prompt_cache_hit_tokens"].as_u64()).unwrap_or(0);
    Some(Usage { input: u["prompt_tokens"].as_u64()?, cached, output: u["completion_tokens"].as_u64().unwrap_or(0) })
}

impl Message {
    pub fn user(content: Vec<Block>) -> Self {
        Self { role: Role::User, content, raw: None, usage: None }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What the model reasoned on the way to this reply, as far as its provider shows it.
    pub fn thinking(&self) -> String {
        let Some((kind, raw)) = &self.raw else { return String::new() };
        let parts: Vec<&str> = if *kind == ProviderKind::Anthropic {
            raw.as_array().into_iter().flatten().filter(|b| b["type"] == "thinking").filter_map(|b| b["thinking"].as_str()).collect()
        } else {
            // Gateways may fill in both names with the same words.
            ["reasoning_content", "reasoning"].iter().filter_map(|k| raw[*k].as_str()).take(1).collect()
        };
        parts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    pub fn tool_uses(&self) -> Vec<(String, String, Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// What a model call reports while it runs.
pub enum Live {
    /// More of what the model is thinking.
    Thinking(String),
    /// The call failed part way and is being sent again, so what it reported so far is void.
    Restart,
}

pub trait Provider: Send + Sync {
    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message>;
    /// The same, reporting to `live` as it goes where the provider can. `live` returns false once
    /// nobody is waiting for the reply, which ends the call at that point.
    fn complete_live(&self, system: &str, messages: &[Message], tools: &[ToolDef], live: &mut dyn FnMut(Live) -> bool) -> Result<Message> {
        let _ = live;
        self.complete(system, messages, tools)
    }
    /// The model it talks to, for accounting.
    fn model(&self) -> String {
        String::new()
    }
}

pub fn make_provider(cfg: &ProviderCfg, effort: &str) -> Result<Box<dyn Provider>> {
    let key = cfg.resolved_key();
    if cfg.kind.needs_key() && key.is_empty() {
        bail!("no API key for {} - set {} or enter one in the assistant settings", cfg.kind.label(), cfg.kind.env_var());
    }
    Ok(match cfg.kind {
        ProviderKind::Anthropic => Box::new(Anthropic {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            key,
            model: anthropic_model_id(&cfg.effective_model()),
            effort: effort.to_string(),
            fallbacks: cfg.anthropic_fallbacks,
        }),
        _ => Box::new(OpenAiCompat {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            key,
            model: cfg.effective_model(),
            kind: cfg.kind,
            effort: effort.to_string(),
        }),
    })
}

/// The native Anthropic API spells versions with hyphens (`claude-sonnet-5-5`), while
/// OpenRouter and most docs use dots (`claude-sonnet-5.5`); accept either.
fn anthropic_model_id(model: &str) -> String {
    let m = model.trim();
    let m = m.strip_prefix("anthropic/").unwrap_or(m);
    if m.starts_with("claude-") { m.replace('.', "-") } else { m.to_string() }
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

/// Statuses worth retrying: rate limits, overload, server errors.
pub fn retryable(status: u16) -> bool {
    status == 408 || status == 409 || status == 429 || status >= 500
}

/// Delay before attempt `n` (1-based retry), honoring `retry-after` seconds.
pub fn backoff(n: u32, retry_after: Option<u64>) -> Duration {
    retry_after.map(|s| Duration::from_secs(s.min(60))).unwrap_or_else(|| Duration::from_millis(1000 * 2u64.pow(n.min(5))))
}

/// JSON request with up to 3 retries on transient failures.
fn request_json(url: &str, headers: &[(&str, String)], body: Option<&Value>) -> Result<Value> {
    let agent = http();
    let mut attempt = 0;
    loop {
        attempt += 1;
        let result = match body {
            Some(b) => {
                let mut req = agent.post(url).header("content-type", "application/json");
                for (k, v) in headers {
                    req = req.header(*k, v.as_str());
                }
                req.send(serde_json::to_vec(b)?)
            }
            None => {
                let mut req = agent.get(url);
                for (k, v) in headers {
                    req = req.header(*k, v.as_str());
                }
                req.call()
            }
        };
        let mut resp = match result {
            Ok(r) => r,
            Err(e) if attempt <= 3 => {
                eprintln!("[assistant] {url}: {e}, retrying");
                std::thread::sleep(backoff(attempt, None));
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("request to {url} failed")),
        };
        let status = resp.status().as_u16();
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let text = resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string()?;
        if retryable(status) && attempt <= 3 {
            std::thread::sleep(backoff(attempt, retry_after));
            continue;
        }
        return json_reply(url, status, &text);
    }
}

/// A whole response: its JSON, or the error it carries.
fn json_reply(url: &str, status: u16, text: &str) -> Result<Value> {
    if !(200..300).contains(&status) {
        let msg = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string).or_else(|| v["error"].as_str().map(str::to_string)))
            .unwrap_or_else(|| text.chars().take(600).collect());
        bail!("HTTP {status} from {url}: {msg}");
    }
    serde_json::from_str(text).with_context(|| format!("bad JSON from {url}"))
}

fn post_json(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
    request_json(url, headers, Some(body))
}

/// How long a stream may stay silent before it counts as dead. The server sends pings while the
/// model works, so this is far longer than a healthy gap.
const STREAM_SILENCE: Duration = Duration::from_secs(300);

/// A streamed reply being put back together into what a whole one would have been.
#[derive(Default)]
struct Assembler {
    message: Value,
    blocks: Vec<Value>,
    /// Tool input as it arrives, by block.
    tool_json: std::collections::HashMap<usize, String>,
    /// The block whose thinking was reported last.
    thought: Option<usize>,
    finished: bool,
}

/// Why a stream gave no reply.
#[derive(Debug, PartialEq)]
enum StreamFail {
    /// Nobody is waiting for it any more.
    Abandoned,
    /// Worth sending again.
    Transient(String),
    Fatal(String),
}

fn push_str(block: &mut Value, key: &str, more: &str) {
    match &mut block[key] {
        Value::String(so_far) => so_far.push_str(more),
        other => *other = json!(more),
    }
}

impl Assembler {
    fn feed(&mut self, event: &Value, live: &mut dyn FnMut(Live) -> bool) -> Result<(), StreamFail> {
        let index = event["index"].as_u64().unwrap_or(0) as usize;
        match event["type"].as_str().unwrap_or("") {
            "message_start" => self.message = event["message"].clone(),
            "content_block_start" => {
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, Value::Null);
                }
                self.blocks[index] = event["content_block"].clone();
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                let Some(block) = self.blocks.get_mut(index) else { return Ok(()) };
                let said = |key: &str| delta[key].as_str().unwrap_or("");
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => push_str(block, "text", said("text")),
                    "signature_delta" => push_str(block, "signature", said("signature")),
                    "input_json_delta" => self.tool_json.entry(index).or_default().push_str(said("partial_json")),
                    "thinking_delta" => {
                        let text = said("thinking");
                        push_str(block, "thinking", text);
                        if !text.is_empty() {
                            let next_block = self.thought.is_some_and(|t| t != index);
                            self.thought = Some(index);
                            let shown = if next_block { format!("\n\n{text}") } else { text.to_string() };
                            if !live(Live::Thinking(shown)) {
                                return Err(StreamFail::Abandoned);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(json), Some(block)) = (self.tool_json.remove(&index), self.blocks.get_mut(index)) {
                    // A reply cut off by its token limit can leave this unfinished: the tool then
                    // says what's wrong and the model tries again.
                    block["input"] = if json.trim().is_empty() { json!({}) } else { serde_json::from_str(&json).unwrap_or_else(|_| json!({ "_invalid_json": json })) };
                }
            }
            "message_delta" => {
                for (k, v) in event["delta"].as_object().into_iter().flatten() {
                    self.message[k] = v.clone();
                }
                for (k, v) in event["usage"].as_object().into_iter().flatten() {
                    self.message["usage"][k] = v.clone();
                }
            }
            "message_stop" => self.finished = true,
            "error" => {
                let why = event["error"]["message"].as_str().unwrap_or("the model's server reported an error").to_string();
                return Err(match event["error"]["type"].as_str().unwrap_or("") {
                    "overloaded_error" | "api_error" | "rate_limit_error" | "timeout_error" => StreamFail::Transient(why),
                    _ => StreamFail::Fatal(why),
                });
            }
            _ => {} // pings, and whatever is added later
        }
        Ok(())
    }

    fn reply(mut self) -> Value {
        self.message["content"] = Value::Array(self.blocks.into_iter().filter(|b| !b.is_null()).collect());
        self.message
    }
}

/// Reads server-sent events until the reply is complete.
fn read_stream(reader: impl std::io::BufRead, live: &mut dyn FnMut(Live) -> bool) -> Result<Value, StreamFail> {
    let mut assembler = Assembler::default();
    for line in reader.lines() {
        let line = line.map_err(|e| StreamFail::Transient(format!("the connection dropped: {e}")))?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let event = serde_json::from_str(data.trim()).map_err(|e| StreamFail::Transient(format!("a garbled reply: {e}")))?;
        assembler.feed(&event, live)?;
        if assembler.finished {
            return Ok(assembler.reply());
        }
    }
    Err(StreamFail::Transient("the reply stopped part way".into()))
}

/// A streaming request with up to 3 retries on transient failures. There's no limit on the whole
/// call, since one that keeps talking is working. A server that answers with a whole reply instead
/// of a stream, as a gateway may, is fine too.
fn post_stream(url: &str, headers: &[(&str, String)], body: &Value, live: &mut dyn FnMut(Live) -> bool) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(STREAM_SILENCE))
        .build()
        .into();
    let bytes = serde_json::to_vec(body)?;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = agent.post(url).header("content-type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, v.as_str());
        }
        let (why, retry_after) = match req.send(&bytes[..]) {
            Err(e) => (e.to_string(), None),
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
                let streaming = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("event-stream"));
                if (200..300).contains(&status) && streaming {
                    match read_stream(std::io::BufReader::new(resp.into_body().into_reader()), live) {
                        Ok(reply) => return Ok(reply),
                        Err(StreamFail::Abandoned) => bail!("nobody is waiting for this reply"),
                        Err(StreamFail::Fatal(why)) => bail!("{why}"),
                        Err(StreamFail::Transient(why)) => (why, None),
                    }
                } else {
                    let text = resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string()?;
                    if !retryable(status) || attempt > 3 {
                        return json_reply(url, status, &text);
                    }
                    (format!("HTTP {status}"), retry_after)
                }
            }
        };
        if attempt > 3 {
            bail!("request to {url} failed: {why}");
        }
        eprintln!("[assistant] {url}: {why}, retrying");
        if !live(Live::Restart) {
            bail!("nobody is waiting for this reply");
        }
        std::thread::sleep(backoff(attempt, retry_after));
    }
}

/// Whether a model on a vendor's list can hold a chat. The lists also carry embedding,
/// image, speech and segmentation models, which a chat request would fail on.
fn is_chat_model(id: &str) -> bool {
    const SKIP: [&str; 19] = [
        "embed", "whisper", "tts", "dall-e", "moderation", "audio", "realtime", "transcribe", "image", "imagine", "search", "davinci", "babbage", "sam-", "veo-", "aqa",
        "lyria", "ocr", "voxtral",
    ];
    !SKIP.iter().any(|s| id.contains(s))
}

/// The id to show and send: Gemini may list its models as `models/<id>`.
fn picker_id(kind: ProviderKind, id: &str) -> String {
    if kind == ProviderKind::Gemini { id.strip_prefix("models/").unwrap_or(id).to_string() } else { id.to_string() }
}

/// Models the provider offers, for the model picker. OpenRouter is filtered
/// to models that take images and tools; the vendors' own APIs to chat models.
pub fn list_models(cfg: &ProviderCfg) -> Result<Vec<String>> {
    let base = cfg.base_url.trim_end_matches('/');
    let key = cfg.resolved_key();
    if cfg.kind.needs_key() && key.is_empty() {
        bail!("set {} or enter an API key first", cfg.kind.env_var());
    }
    let v = match cfg.kind {
        ProviderKind::Anthropic => request_json(
            &format!("{base}/v1/models?limit=1000"),
            &[("x-api-key", key), ("anthropic-version", "2026-10-21".into())],
            None,
        )?,
        _ => {
            let h = if key.is_empty() { vec![] } else { vec![("authorization", format!("Bearer {key}"))] };
            request_json(&format!("{base}/models"), &h, None)?
        }
    };
    let mut out: Vec<String> = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| match cfg.kind {
            ProviderKind::OpenRouter => {
                let has = |k: &str, x: &str| m[k].as_array().into_iter().flatten().any(|v| v == x);
                m["architecture"]["input_modalities"].as_array().into_iter().flatten().any(|v| v == "image") && has("supported_parameters", "tools")
            }
            ProviderKind::OpenAi | ProviderKind::Meta | ProviderKind::Gemini | ProviderKind::Xai | ProviderKind::DeepSeek => is_chat_model(m["id"].as_str().unwrap_or("")),
            _ => true,
        })
        .filter_map(|m| m["id"].as_str().map(|id| picker_id(cfg.kind, id)))
        .collect();
    if cfg.kind != ProviderKind::Anthropic {
        out.sort();
    }
    out.dedup();
    if out.is_empty() {
        bail!("the provider returned no usable models");
    }
    Ok(out)
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

// ---------------------------------------------------------------------------
// Anthropic Messages API

pub struct Anthropic {
    pub base_url: String,
    pub key: String,
    pub model: String,
    pub effort: String,
    pub fallbacks: bool,
}

/// Models that accept the server-side `fallbacks: "default"` parameter.
fn supports_fallbacks(model: &str) -> bool {
    ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"].contains(&model)
}

/// Thinking counts towards this even when it isn't shown, so it has to leave room for the reply.
/// Every model still served takes this much, and a stream can't time out on a long reply.
const MAX_OUTPUT: u64 = 64_000;
/// For the models that take a fixed amount of thinking.
const THINKING_BUDGET: u64 = 16_000;
/// These think only within a fixed budget.
const BUDGET_MODELS: [&str; 3] = ["claude-haiku-4-5", "claude-sonnet-4-5", "claude-opus-4-5"];
/// These answer 400 to an `effort`.
const NO_EFFORT_MODELS: [&str; 2] = ["claude-haiku-4-5", "claude-sonnet-4-5"];

/// Left alone, the newest models think without saying what, and 4.6 to 4.8 don't think at all. Any
/// other name is a gateway's, which may not know the field.
fn anthropic_thinking(model: &str) -> Option<Value> {
    if !model.starts_with("claude-") {
        return None;
    }
    if BUDGET_MODELS.iter().any(|m| model.starts_with(m)) {
        return Some(json!({ "type": "enabled", "budget_tokens": THINKING_BUDGET }));
    }
    // A thinking block is only good for the conversation it was written in, byte for byte, and the
    // app changes that on purpose: compaction edits old messages, Plan mode changes the system
    // prompt and the tools. Unset, the next request after any of those fails with a 400. This way
    // the old blocks go and the work carries on.
    Some(json!({ "type": "adaptive", "display": "summarized", "block_binding": { "prefix_mismatch_behavior": "drop_block" } }))
}

/// What's before a model taking over part way can't be replayed, and those tool calls must not run.
/// Its words stay, since the new model carried on from them.
fn past_the_fallback(content: &Value) -> Value {
    let Some(blocks) = content.as_array() else { return content.clone() };
    let Some(last) = blocks.iter().rposition(|b| b["type"] == "fallback") else { return content.clone() };
    Value::Array(blocks.iter().enumerate().filter(|(i, b)| *i >= last || b["type"] == "text").map(|(_, b)| b.clone()).collect())
}

fn anthropic_block(b: &Block) -> Value {
    match b {
        Block::Text(t) => json!({ "type": "text", "text": t }),
        Block::Image { data, mime } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": mime, "data": b64(data) }
        }),
        Block::ToolUse { id, name, input } => json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
        Block::ToolResult { id, content, is_error } => {
            let inner: Vec<Value> = content.iter().map(anthropic_block).collect();
            json!({ "type": "tool_result", "tool_use_id": id, "content": inner, "is_error": is_error })
        }
    }
}

pub fn anthropic_request(model: &str, effort: &str, fallbacks: bool, system: &str, messages: &[Message], tools: &[ToolDef]) -> Value {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|m| {
            let content = match &m.raw {
                Some((ProviderKind::Anthropic, raw)) => raw.clone(),
                _ => Value::Array(m.content.iter().map(anthropic_block).collect()),
            };
            json!({ "role": if m.role == Role::User { "user" } else { "assistant" }, "content": content })
        })
        .collect();
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema }))
        .collect();
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_OUTPUT,
        "system": [{ "type": "text", "text": system }],
        "messages": msgs,
        // Automatic caching: the breakpoint follows the end of the conversation.
        "cache_control": { "type": "ephemeral" },
    });
    // A call without tools (the reviewer) leaves the field out altogether.
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if !effort.is_empty() && !NO_EFFORT_MODELS.iter().any(|m| model.starts_with(m)) {
        body["output_config"] = json!({ "effort": effort });
    }
    if let Some(thinking) = anthropic_thinking(model) {
        body["thinking"] = thinking;
    }
    if fallbacks && supports_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

pub fn parse_anthropic(v: &Value) -> Result<Message> {
    let stop = v["stop_reason"].as_str().unwrap_or("");
    if stop == "refusal" {
        let why = v["stop_details"]["explanation"].as_str().unwrap_or("declined by the model's safety filter");
        bail!("the model declined this request: {why}");
    }
    let raw = past_the_fallback(&v["content"]);
    let mut content = Vec::new();
    for b in raw.as_array().into_iter().flatten() {
        match b["type"].as_str() {
            Some("text") => content.push(Block::Text(b["text"].as_str().unwrap_or("").to_string())),
            Some("tool_use") => content.push(Block::ToolUse {
                id: b["id"].as_str().unwrap_or("").to_string(),
                name: b["name"].as_str().unwrap_or("").to_string(),
                input: b["input"].clone(),
            }),
            _ => {} // thinking, fallback markers: kept only in `raw`
        }
    }
    if stop == "max_tokens" && content.iter().all(|b| !matches!(b, Block::ToolUse { .. })) {
        content.push(Block::Text("(response cut off at max_tokens)".into()));
    }
    Ok(Message { role: Role::Assistant, content, raw: Some((ProviderKind::Anthropic, raw)), usage: anthropic_usage(v) })
}

impl Provider for Anthropic {
    fn model(&self) -> String {
        self.model.clone()
    }

    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        self.complete_live(system, messages, tools, &mut |_| true)
    }

    fn complete_live(&self, system: &str, messages: &[Message], tools: &[ToolDef], live: &mut dyn FnMut(Live) -> bool) -> Result<Message> {
        let mut body = anthropic_request(&self.model, &self.effort, self.fallbacks, system, messages, tools);
        body["stream"] = json!(true);
        let mut headers = vec![("x-api-key", self.key.clone()), ("anthropic-version", "2026-10-21".to_string())];
        let betas = anthropic_betas(&body);
        if !betas.is_empty() {
            headers.push(("anthropic-beta", betas.join(",")));
        }
        let v = post_stream(&format!("{}/v1/messages", self.base_url), &headers, &body, live)?;
        parse_anthropic(&v)
    }
}

/// What a request has to announce for the optional fields it uses.
fn anthropic_betas(body: &Value) -> Vec<&'static str> {
    let mut betas = Vec::new();
    if body.get("fallbacks").is_some() {
        betas.push("server-side-fallback-2026-07-01");
    }
    if body["thinking"].get("block_binding").is_some() {
        betas.push("thinking-binding-controls-2026-08-01");
    }
    betas
}

// ---------------------------------------------------------------------------
// OpenAI-compatible chat completions

pub struct OpenAiCompat {
    pub base_url: String,
    pub key: String,
    pub model: String,
    pub kind: ProviderKind,
    pub effort: String,
}

fn data_url(data: &[u8], mime: &str) -> String {
    format!("data:{mime};base64,{}", b64(data))
}

/// Reasoning a provider may need back on later requests. DeepSeek answers 400 without its
/// `reasoning_content` once tools are in play; gateways use the other two names.
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "reasoning_details"];

/// Muse takes `reasoning_effort`, but only these models accept `max`; the rest answer 400.
fn muse_effort<'a>(model: &str, effort: &'a str) -> &'a str {
    if effort == "max" && !["muse-wonder-1.0", "muse-spark-1.3"].contains(&model) { "xhigh" } else { effort }
}

pub fn openai_request(kind: ProviderKind, model: &str, effort: &str, system: &str, messages: &[Message], tools: &[ToolDef]) -> Value {
    let mut out = vec![json!({ "role": "system", "content": system })];
    for m in messages {
        match m.role {
            Role::Assistant => {
                let text = m.text();
                // Only the provider that wrote these can read them.
                let sent = m.raw.as_ref().filter(|(k, _)| *k == kind).map(|(_, raw)| raw);
                let sent_calls = sent.and_then(|raw| raw["tool_calls"].as_array());
                let calls: Vec<Value> = m
                    .tool_uses()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (id, name, input))| {
                        let mut call = json!({ "id": id, "type": "function", "function": { "name": name, "arguments": input.to_string() } });
                        // Whatever else the provider put on the call goes back with it: this is
                        // where Gemini carries its thought signature.
                        if let Some(extra) = sent_calls.and_then(|c| c.get(i)).and_then(Value::as_object) {
                            for (k, v) in extra {
                                if !["id", "type", "function", "index"].contains(&k.as_str()) {
                                    call[k] = v.clone();
                                }
                            }
                        }
                        call
                    })
                    .collect();
                let mut msg = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
                if !calls.is_empty() {
                    msg["tool_calls"] = json!(calls);
                }
                for key in REASONING_FIELDS {
                    if let Some(v) = sent.and_then(|raw| raw.get(key)) {
                        msg[key] = v.clone();
                    }
                }
                out.push(msg);
            }
            Role::User => {
                // Tool results become `tool` messages; images they carry follow
                // in one user message because tool messages are text-only.
                let mut parts = Vec::new();
                let mut tool_images = Vec::new();
                for b in &m.content {
                    match b {
                        Block::ToolResult { id, content, is_error } => {
                            let mut text = String::new();
                            if *is_error {
                                text.push_str("ERROR: ");
                            }
                            for c in content {
                                match c {
                                    Block::Text(t) => text.push_str(t),
                                    Block::Image { data, mime } => {
                                        text.push_str("[image attached below]");
                                        tool_images.push(json!({ "type": "image_url", "image_url": { "url": data_url(data, mime) } }));
                                    }
                                    _ => {}
                                }
                            }
                            out.push(json!({ "role": "tool", "tool_call_id": id, "content": text }));
                        }
                        Block::Text(t) => parts.push(json!({ "type": "text", "text": t })),
                        Block::Image { data, mime } => {
                            parts.push(json!({ "type": "image_url", "image_url": { "url": data_url(data, mime) } }))
                        }
                        Block::ToolUse { .. } => {}
                    }
                }
                if !tool_images.is_empty() {
                    let mut c = vec![json!({ "type": "text", "text": "Images returned by the tool calls above:" })];
                    c.extend(tool_images);
                    out.push(json!({ "role": "user", "content": c }));
                }
                if !parts.is_empty() {
                    out.push(json!({ "role": "user", "content": parts }));
                }
            }
        }
    }
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.schema } }))
        .collect();
    let mut body = json!({ "model": model, "messages": out });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if kind == ProviderKind::Meta && !effort.is_empty() {
        body["reasoning_effort"] = json!(muse_effort(model, effort));
    }
    body
}

/// The parts of an assistant reply to send back with it later. Empty values stay out: Muse
/// redacts its reasoning to an empty field and validates requests strictly.
fn replay_fields(msg: &Value) -> Option<Value> {
    let mut keep = serde_json::Map::new();
    for key in REASONING_FIELDS.into_iter().chain(["tool_calls"]) {
        match msg.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() => {}
            Some(Value::Array(a)) if a.is_empty() => {}
            Some(v) => {
                keep.insert(key.to_string(), v.clone());
            }
        }
    }
    // `index` belongs to streaming, and gateways reject it on the way back.
    if let Some(details) = keep.get_mut("reasoning_details").and_then(Value::as_array_mut) {
        for d in details.iter_mut().filter_map(Value::as_object_mut) {
            d.remove("index");
        }
    }
    (!keep.is_empty()).then_some(Value::Object(keep))
}

pub fn parse_openai(kind: ProviderKind, v: &Value) -> Result<Message> {
    let msg = &v["choices"][0]["message"];
    if msg.is_null() {
        bail!("unexpected response: {}", v.to_string().chars().take(400).collect::<String>());
    }
    let mut content = Vec::new();
    if let Some(t) = msg["content"].as_str()
        && !t.trim().is_empty()
    {
        content.push(Block::Text(t.to_string()));
    }
    for (i, c) in msg["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let args = &c["function"]["arguments"];
        let input = match args {
            Value::String(s) if s.trim().is_empty() => json!({}),
            Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({ "_invalid_json": s })),
            other => other.clone(),
        };
        let id = c["id"].as_str().map(str::to_string).unwrap_or_else(|| format!("call_{i}"));
        content.push(Block::ToolUse { id, name: c["function"]["name"].as_str().unwrap_or("").to_string(), input });
    }
    Ok(Message { role: Role::Assistant, content, raw: replay_fields(msg).map(|raw| (kind, raw)), usage: openai_usage(v) })
}

impl Provider for OpenAiCompat {
    fn model(&self) -> String {
        self.model.clone()
    }

    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        let body = openai_request(self.kind, &self.model, &self.effort, system, messages, tools);
        let mut headers = Vec::new();
        if !self.key.is_empty() {
            headers.push(("authorization", format!("Bearer {}", self.key)));
        }
        if self.kind == ProviderKind::OpenRouter {
            headers.push(("http-referer", "https://github.com/nvidia/simready-viewer".to_string()));
            headers.push(("x-title", "SimReady Viewer".to_string()));
        }
        let v = post_json(&format!("{}/chat/completions", self.base_url), &headers, &body).map_err(|e| {
            let s = format!("{e:#}");
            if s.contains("tool") && (s.contains("support") || s.contains("not allowed")) {
                anyhow::anyhow!("{s}\n(this model does not seem to support tool calling - pick a tool-capable model)")
            } else {
                e
            }
        })?;
        parse_openai(self.kind, &v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> Vec<Message> {
        vec![
            Message::user(vec![Block::Text("place a box".into())]),
            Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse { id: "t1".into(), name: "search_assets".into(), input: json!({"query": "box"}) }],
                raw: None,
                usage: None,
            },
            Message::user(vec![Block::ToolResult {
                id: "t1".into(),
                content: vec![Block::Text("[]".into()), Block::Image { data: Arc::new(vec![1, 2, 3]), mime: "image/png".into() }],
                is_error: false,
            }]),
        ]
    }

    fn tools() -> Vec<ToolDef> {
        vec![ToolDef { name: "search_assets".into(), description: "d".into(), schema: json!({"type": "object"}) }]
    }

    #[test]
    fn anthropic_round_trip() {
        let body = anthropic_request("claude-opus-5-5", "high", true, "sys", &conv(), &tools());
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["output_config"]["effort"], "high");
        assert!(body.get("temperature").is_none());
        let m = &body["messages"];
        assert_eq!(m[1]["content"][0]["type"], "tool_use");
        assert_eq!(m[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(m[2]["content"][0]["content"][1]["source"]["data"], "AQID");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");

        let resp = json!({
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "x"},
                {"type": "text", "text": "ok"},
                {"type": "tool_use", "id": "t2", "name": "place_objects", "input": {"objects": []}}
            ]
        });
        let msg = parse_anthropic(&resp).unwrap();
        assert_eq!(msg.text(), "ok");
        assert_eq!(msg.tool_uses()[0].1, "place_objects");
        // Replays the raw content, thinking block included.
        let body = anthropic_request("claude-opus-5-5", "", false, "s", &[msg], &[]);
        assert_eq!(body["messages"][0]["content"][0]["type"], "thinking");
        assert!(body.get("fallbacks").is_none());
    }

    #[test]
    fn every_claude_model_is_asked_to_think_in_a_way_it_accepts() {
        let body = |model: &str| anthropic_request(model, "high", false, "s", &[], &[]);
        // The newest think regardless and hide it unless asked; the 4.6 to 4.8 models don't think unless asked.
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1", "claude-opus-4-8", "claude-sonnet-4-6"] {
            let b = body(model);
            assert_eq!(b["thinking"]["type"], "adaptive", "{model}");
            assert_eq!(b["thinking"]["display"], "summarized", "{model}");
            assert_eq!(b["output_config"]["effort"], "high", "{model}");
            assert_eq!(b["max_tokens"], 64_000, "{model}");
        }
        // Measured: Haiku 4.5 answers 400 to an effort, and takes a budget below max_tokens.
        let old = body("claude-haiku-4-5");
        assert_eq!(old["thinking"]["type"], "enabled");
        assert!(old["thinking"]["budget_tokens"].as_u64().unwrap() < old["max_tokens"].as_u64().unwrap());
        assert!(old.get("output_config").is_none());
        assert!(body("claude-haiku-4-5-20251001").get("output_config").is_none());
        // A gateway's own model name gets nothing it may not know.
        assert!(body("my-gateway-model").get("thinking").is_none());
    }

    #[test]
    fn a_chat_the_app_edited_on_purpose_keeps_going() {
        let body = anthropic_request("claude-opus-5-5", "high", true, "s", &[], &[]);
        assert_eq!(body["thinking"]["block_binding"]["prefix_mismatch_behavior"], "drop_block");
        assert_eq!(anthropic_betas(&body), ["server-side-fallback-2026-07-01", "thinking-binding-controls-2026-08-01"]);
        assert!(anthropic_betas(&anthropic_request("my-gateway-model", "", false, "s", &[], &[])).is_empty());
    }

    /// `events` as a server would send them, with a ping and the `event:` lines a reader skips.
    fn sse(events: &[Value]) -> String {
        let mut out = String::from("event: ping\ndata: {\"type\": \"ping\"}\n\n");
        for e in events {
            out.push_str(&format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()));
        }
        out
    }

    fn start(index: usize, block: Value) -> Value {
        json!({ "type": "content_block_start", "index": index, "content_block": block })
    }
    fn delta(index: usize, delta: Value) -> Value {
        json!({ "type": "content_block_delta", "index": index, "delta": delta })
    }
    fn stop(index: usize) -> Value {
        json!({ "type": "content_block_stop", "index": index })
    }

    /// A thinking block, some words and a tool call, ending normally.
    fn full_reply() -> Vec<Value> {
        vec![
            json!({ "type": "message_start", "message": { "id": "m1", "role": "assistant", "content": [], "stop_reason": null, "usage": { "input_tokens": 100, "cache_read_input_tokens": 40, "output_tokens": 1 } } }),
            start(0, json!({ "type": "thinking", "thinking": "", "signature": "" })),
            delta(0, json!({ "type": "thinking_delta", "thinking": "First, " })),
            delta(0, json!({ "type": "thinking_delta", "thinking": "the loop." })),
            delta(0, json!({ "type": "signature_delta", "signature": "sig" })),
            stop(0),
            start(1, json!({ "type": "thinking", "thinking": "", "signature": "" })),
            delta(1, json!({ "type": "thinking_delta", "thinking": "Then the gap." })),
            delta(1, json!({ "type": "signature_delta", "signature": "sig2" })),
            stop(1),
            start(2, json!({ "type": "text", "text": "" })),
            delta(2, json!({ "type": "text_delta", "text": "Set" })),
            delta(2, json!({ "type": "text_delta", "text": "ting it." })),
            stop(2),
            start(3, json!({ "type": "tool_use", "id": "t1", "name": "antenna_set", "input": {} })),
            delta(3, json!({ "type": "input_json_delta", "partial_json": "{\"a\": " })),
            delta(3, json!({ "type": "input_json_delta", "partial_json": "1}" })),
            stop(3),
            start(4, json!({ "type": "tool_use", "id": "t2", "name": "get_scene", "input": {} })),
            delta(4, json!({ "type": "input_json_delta", "partial_json": "" })),
            stop(4),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 57 } }),
            json!({ "type": "message_stop" }),
        ]
    }

    fn collect(stream: &str) -> (Result<Value, StreamFail>, Vec<String>) {
        let mut said = Vec::new();
        let result = read_stream(stream.as_bytes(), &mut |live| {
            if let Live::Thinking(t) = live {
                said.push(t);
            }
            true
        });
        (result, said)
    }

    #[test]
    fn a_stream_comes_back_as_the_reply_it_stands_for() {
        let (reply, said) = collect(&sse(&full_reply()));
        let msg = parse_anthropic(&reply.unwrap()).unwrap();
        assert_eq!(said, ["First, ", "the loop.", "\n\nThen the gap."]);
        assert_eq!(msg.text(), "Setting it.");
        assert_eq!(msg.tool_uses(), [("t1".to_string(), "antenna_set".to_string(), json!({ "a": 1 })), ("t2".to_string(), "get_scene".to_string(), json!({}))]);
        assert_eq!(msg.usage, Some(Usage { input: 140, cached: 40, output: 57 }));
        assert_eq!(msg.thinking(), "First, the loop.\n\nThen the gap.");
        // What goes back is what came: a signature is only good on the block it was written for.
        let body = anthropic_request("claude-opus-5-5", "", false, "s", &[msg], &[]);
        let sent = &body["messages"][0]["content"];
        assert_eq!(sent[0], json!({ "type": "thinking", "thinking": "First, the loop.", "signature": "sig" }));
        assert_eq!(sent[1]["signature"], "sig2");
        assert_eq!(sent[3]["input"], json!({ "a": 1 }));
    }

    #[test]
    fn thinking_the_model_keeps_to_itself_shows_nothing() {
        let events = [
            json!({ "type": "message_start", "message": { "content": [], "usage": { "input_tokens": 1 } } }),
            start(0, json!({ "type": "thinking", "thinking": "", "signature": "" })),
            delta(0, json!({ "type": "signature_delta", "signature": "sig" })),
            stop(0),
            json!({ "type": "message_stop" }),
        ];
        let (reply, said) = collect(&sse(&events));
        let msg = parse_anthropic(&reply.unwrap()).unwrap();
        assert!(said.is_empty());
        assert_eq!(msg.thinking(), "");
        assert_eq!(msg.raw.unwrap().1[0]["signature"], "sig", "it still goes back");
    }

    #[test]
    fn a_model_taking_over_part_way_doesnt_run_the_first_ones_calls() {
        let reply = json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "partial", "signature": "a" },
                { "type": "text", "text": "So far " },
                { "type": "tool_use", "id": "lost", "name": "place_objects", "input": {} },
                { "type": "fallback", "from": { "model": "x" }, "to": { "model": "y" } },
                { "type": "thinking", "thinking": "theirs", "signature": "b" },
                { "type": "text", "text": "and on." },
            ]
        });
        let msg = parse_anthropic(&reply).unwrap();
        assert!(msg.tool_uses().is_empty());
        assert_eq!(msg.text(), "So far \nand on.");
        assert_eq!(msg.thinking(), "theirs");
        let kinds: Vec<_> = msg.raw.unwrap().1.as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["text", "fallback", "thinking", "text"]);
    }

    #[test]
    fn stream_failures_are_told_apart() {
        let begun = json!({ "type": "message_start", "message": { "content": [] } });
        let error = |kind: &str| json!({ "type": "error", "error": { "type": kind, "message": "it failed" } });
        assert_eq!(collect(&sse(&[begun.clone(), error("overloaded_error")])).0, Err(StreamFail::Transient("it failed".into())));
        assert_eq!(collect(&sse(&[begun.clone(), error("invalid_request_error")])).0, Err(StreamFail::Fatal("it failed".into())));
        assert!(matches!(collect(&sse(std::slice::from_ref(&begun))).0, Err(StreamFail::Transient(_))), "a stream that just ends isn't a reply");
        assert!(matches!(collect("data: {not json\n\n").0, Err(StreamFail::Transient(_))));

        let mut asked = 0;
        let result = read_stream(sse(&full_reply()).as_bytes(), &mut |_| {
            asked += 1;
            false
        });
        assert_eq!(result, Err(StreamFail::Abandoned));
        assert_eq!(asked, 1, "it stops at the first report nobody wants");
    }

    /// Serves one prepared answer per connection, and returns what it was sent.
    fn serve(answers: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for answer in answers {
                let (mut socket, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let (mut head, mut length) = (String::new(), 0);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap();
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                requests.push(format!("{head}\n{}", String::from_utf8_lossy(&body)));
                socket.write_all(answer.as_bytes()).unwrap();
            }
            requests
        });
        (url, server)
    }

    fn http(status: &str, content_type: &str, extra: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    #[test]
    fn a_call_over_http_reports_its_thinking_and_survives_a_failure_part_way() {
        let fails_part_way = sse(&[json!({ "type": "message_start", "message": { "content": [] } }), start(0, json!({ "type": "thinking", "thinking": "", "signature": "" })), delta(0, json!({ "type": "thinking_delta", "thinking": "lost" })), json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } })]);
        let (url, server) = serve(vec![
            http("200 OK", "text/event-stream", "", &fails_part_way),
            http("529 Overloaded", "application/json", "retry-after: 0\r\n", "{\"error\": {\"message\": \"Overloaded\"}}"),
            http("200 OK", "text/event-stream; charset=utf-8", "", &sse(&full_reply())),
        ]);
        let provider = Anthropic { base_url: url, key: "the key".into(), model: "claude-opus-5-5".into(), effort: "high".into(), fallbacks: true };
        let mut log = Vec::new();
        let msg = provider
            .complete_live("sys", &[Message::user(vec![Block::Text("hi".into())])], &[], &mut |live| {
                log.push(match live {
                    Live::Thinking(t) => t,
                    Live::Restart => "<restart>".to_string(),
                });
                true
            })
            .unwrap();
        assert_eq!(log, ["lost", "<restart>", "<restart>", "First, ", "the loop.", "\n\nThen the gap."]);
        assert_eq!(msg.text(), "Setting it.");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        let sent = requests[2].to_lowercase();
        assert!(sent.contains("anthropic-beta: server-side-fallback-2026-07-01,thinking-binding-controls-2026-08-01"), "{sent}");
        assert!(sent.contains("\"stream\":true"), "{sent}");
    }

    #[test]
    fn a_whole_reply_where_a_stream_was_asked_for_is_fine_and_an_error_says_why() {
        let whole = json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": "whole" }] }).to_string();
        let (url, server) = serve(vec![http("200 OK", "application/json", "", &whole), http("400 Bad Request", "application/json", "", "{\"error\": {\"message\": \"no such model\"}}")]);
        let provider = Anthropic { base_url: url, key: "k".into(), model: "gateway".into(), effort: String::new(), fallbacks: false };
        assert_eq!(provider.complete("s", &[], &[]).unwrap().text(), "whole");
        let error = provider.complete("s", &[], &[]).unwrap_err().to_string();
        assert!(error.contains("HTTP 400") && error.contains("no such model"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn other_providers_reasoning_is_shown_when_they_send_it() {
        let reply = |msg: Value| parse_openai(ProviderKind::DeepSeek, &json!({ "choices": [{ "message": msg }] })).unwrap();
        assert_eq!(reply(json!({ "content": "ok", "reasoning_content": "because" })).thinking(), "because");
        assert_eq!(reply(json!({ "content": "ok", "reasoning": "because", "reasoning_content": "because" })).thinking(), "because");
        // Muse redacts to an empty field.
        assert_eq!(reply(json!({ "content": "ok", "reasoning_content": "" })).thinking(), "");
        assert_eq!(reply(json!({ "content": "ok" })).thinking(), "");
    }

    #[test]
    fn anthropic_model_id_normalizes_dots() {
        assert_eq!(anthropic_model_id("claude-sonnet-5.5"), "claude-sonnet-5-5");
        assert_eq!(anthropic_model_id(" anthropic/claude-opus-5.5 "), "claude-opus-5-5");
        assert_eq!(anthropic_model_id("claude-fable-5-1"), "claude-fable-5-1");
    }

    #[test]
    fn anthropic_refusal_is_error() {
        let r = json!({"stop_reason": "refusal", "content": [], "stop_details": {"explanation": "nope"}});
        assert!(parse_anthropic(&r).is_err());
    }

    #[test]
    fn muse_sends_effort_and_nothing_its_chat_route_rejects() {
        let body = openai_request(ProviderKind::Meta, "muse-spark-1.3", "high", "sys", &conv(), &tools());
        assert_eq!(body["reasoning_effort"], "high");
        for rejected in ["temperature", "top_p", "stop", "n", "logprobs", "max_output_tokens", "tool_choice"] {
            assert!(body.get(rejected).is_none(), "{rejected}");
        }
        // One tool message per call, before anything else comes along.
        let m = body["messages"].as_array().unwrap();
        assert_eq!(m[2]["tool_calls"][0]["id"], "t1");
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["tool_call_id"], "t1");

        assert!(openai_request(ProviderKind::Meta, "muse-spark-1.3", "", "s", &[], &[]).get("reasoning_effort").is_none());
        assert!(openai_request(ProviderKind::OpenAi, "gpt", "high", "s", &[], &[]).get("reasoning_effort").is_none());
    }

    #[test]
    fn muse_max_effort_only_goes_to_models_that_take_it() {
        for (model, want) in [
            ("muse-spark-1.3", "max"),
            ("muse-wonder-1.0", "max"),
            ("muse-spark-1.3-contributor", "xhigh"),
            ("muse-spark-1.2", "xhigh"),
        ] {
            assert_eq!(openai_request(ProviderKind::Meta, model, "max", "s", &[], &[])["reasoning_effort"], want, "{model}");
        }
    }

    #[test]
    fn empty_reasoning_is_not_sent_back() {
        let resp = json!({"choices": [{"message": {"content": "ok", "reasoning_content": ""}}]});
        let msg = parse_openai(ProviderKind::Meta, &resp).unwrap();
        assert!(msg.raw.is_none());
        let body = openai_request(ProviderKind::Meta, "m", "", "s", &[msg], &[]);
        assert!(body["messages"][1].get("reasoning_content").is_none());
    }

    #[test]
    fn reasoning_and_call_signatures_go_back_to_the_provider_that_wrote_them() {
        let resp = json!({"choices": [{"message": {
            "content": null,
            "reasoning_content": "thinking",
            "reasoning_details": [{"type": "reasoning.text", "text": "t", "index": 0}],
            "tool_calls": [{
                "id": "c1", "index": 0, "type": "function",
                "function": {"name": "get_scene", "arguments": "{}"},
                "extra_content": {"google": {"thought_signature": "sig"}}
            }]
        }}]});
        let msg = parse_openai(ProviderKind::Gemini, &resp).unwrap();

        let body = openai_request(ProviderKind::Gemini, "m", "", "s", std::slice::from_ref(&msg), &[]);
        let back = &body["messages"][1];
        assert_eq!(back["reasoning_content"], "thinking");
        assert!(back["reasoning_details"][0].get("index").is_none());
        assert_eq!(back["reasoning_details"][0]["text"], "t");
        let call = &back["tool_calls"][0];
        assert_eq!(call["extra_content"]["google"]["thought_signature"], "sig");
        assert_eq!(call["id"], "c1");
        assert_eq!(call["function"]["name"], "get_scene");
        assert!(call.get("index").is_none());

        // Another provider gets the plain turn, and Anthropic doesn't mistake it for its own.
        let body = openai_request(ProviderKind::Xai, "m", "", "s", std::slice::from_ref(&msg), &[]);
        let back = &body["messages"][1];
        assert!(back.get("reasoning_content").is_none());
        assert!(back["tool_calls"][0].get("extra_content").is_none());
        let body = anthropic_request("m", "", false, "s", &[msg], &[]);
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
    }

    #[test]
    fn malformed_tool_calls_do_not_panic() {
        let resp = json!({"choices": [{"message": {"content": "x", "tool_calls": ["junk", {"function": "junk"}]}}]});
        let msg = parse_openai(ProviderKind::Meta, &resp).unwrap();
        let body = openai_request(ProviderKind::Meta, "m", "", "s", &[msg], &[]);
        assert_eq!(body["messages"][1]["tool_calls"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn model_picker_keeps_chat_models_only() {
        for id in ["muse-spark-1.3", "muse-spark-1.3-contributor", "muse-wonder-1.0", "gemini-3.8-flash", "grok-4.7", "deepseek-flash", "deepseek-v4-pro", "gpt-5"] {
            assert!(is_chat_model(id), "{id}");
        }
        for id in ["muse-image-1.0", "muse-voice-transcribe-1.0", "sam-3.1", "gemini-embedding-001", "imagen-4", "veo-3", "grok-imagine-1", "text-embedding-3-small", "whisper-1"] {
            assert!(!is_chat_model(id), "{id}");
        }
        assert_eq!(picker_id(ProviderKind::Gemini, "models/gemini-3.8-flash"), "gemini-3.8-flash");
        assert_eq!(picker_id(ProviderKind::Custom, "models/mine"), "models/mine");
    }

    #[test]
    fn openai_round_trip() {
        let body = openai_request(ProviderKind::OpenAi, "gpt", "", "sys", &conv(), &tools());
        let m = body["messages"].as_array().unwrap();
        assert_eq!(m[0]["role"], "system");
        assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"query\":\"box\"}");
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["tool_call_id"], "t1");
        assert_eq!(m[4]["role"], "user");
        assert!(m[4]["content"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
        assert_eq!(body["tools"][0]["function"]["name"], "search_assets");

        let resp = json!({"choices": [{"message": {"content": null, "tool_calls": [
            {"id": "c1", "type": "function", "function": {"name": "get_scene", "arguments": "{}"}}
        ]}}]});
        let msg = parse_openai(ProviderKind::OpenAi, &resp).unwrap();
        assert_eq!(msg.tool_uses()[0], ("c1".into(), "get_scene".into(), json!({})));
    }
}
