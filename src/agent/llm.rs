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
}

impl Message {
    pub fn user(content: Vec<Block>) -> Self {
        Self { role: Role::User, content, raw: None }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n")
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

pub trait Provider: Send + Sync {
    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message>;
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
        if !(200..300).contains(&status) {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string).or_else(|| v["error"].as_str().map(str::to_string)))
                .unwrap_or_else(|| text.chars().take(600).collect());
            bail!("HTTP {status} from {url}: {msg}");
        }
        return serde_json::from_str(&text).with_context(|| format!("bad JSON from {url}"));
    }
}

fn post_json(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
    request_json(url, headers, Some(body))
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
        "max_tokens": 16000,
        "system": [{ "type": "text", "text": system }],
        "messages": msgs,
        // Automatic caching: the breakpoint follows the end of the conversation.
        "cache_control": { "type": "ephemeral" },
    });
    // A call without tools (the reviewer) leaves the field out altogether.
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if !effort.is_empty() {
        body["output_config"] = json!({ "effort": effort });
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
    let raw = v["content"].clone();
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
    Ok(Message { role: Role::Assistant, content, raw: Some((ProviderKind::Anthropic, raw)) })
}

impl Provider for Anthropic {
    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        let body = anthropic_request(&self.model, &self.effort, self.fallbacks, system, messages, tools);
        let mut headers = vec![("x-api-key", self.key.clone()), ("anthropic-version", "2026-10-21".to_string())];
        if body.get("fallbacks").is_some() {
            headers.push(("anthropic-beta", "server-side-fallback-2026-07-01".to_string()));
        }
        let v = post_json(&format!("{}/v1/messages", self.base_url), &headers, &body)?;
        parse_anthropic(&v)
    }
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
    Ok(Message { role: Role::Assistant, content, raw: replay_fields(msg).map(|raw| (kind, raw)) })
}

impl Provider for OpenAiCompat {
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
