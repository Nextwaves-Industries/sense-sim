//! MCP client: connects to the servers in `agent.json` (stdio commands or
//! streamable-HTTP URLs) and exposes their tools to the assistant as
//! `<server>__<tool>`.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use crossbeam_channel::{Sender, bounded};
use serde_json::{Value, json};

use super::server::PROTOCOL_VERSION;
use crate::agent::config::McpServerCfg;
use crate::agent::llm::{Block, ToolDef};
use crate::agent::tools::ToolOutput;

trait Transport: Send + Sync {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value>;
    fn notify(&self, method: &str, params: Value) -> Result<()>;
}

// ---------------------------------------------------------------------------
// stdio

struct Stdio_ {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, Sender<Value>>>>,
    next: AtomicI64,
    child: Mutex<Child>,
}

fn is_exe_on_path(cmd: &str) -> bool {
    let p = std::path::Path::new(cmd);
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) && (p.is_absolute() || p.exists()) {
        return true;
    }
    let name = if p.extension().is_some() { cmd.to_string() } else { format!("{cmd}.exe") };
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|d| d.join(&name).is_file()))
        .unwrap_or(false)
}

fn write_line(stdin: &Mutex<ChildStdin>, v: &Value) -> Result<()> {
    let mut s = stdin.lock().unwrap();
    writeln!(s, "{v}")?;
    s.flush()?;
    Ok(())
}

impl Stdio_ {
    fn spawn(cfg: &McpServerCfg) -> Result<Self> {
        // On Windows `npx`, `uvx`, … are .cmd shims that only cmd.exe can run.
        let mut cmd = if cfg!(windows) && !is_exe_on_path(&cfg.command) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&cfg.command);
            c
        } else {
            Command::new(&cfg.command)
        };
        cmd.args(&cfg.args).envs(&cfg.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().with_context(|| format!("could not start `{}`", cfg.command))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let pending: Arc<Mutex<HashMap<i64, Sender<Value>>>> = Arc::default();
        let (p, w) = (pending.clone(), stdin.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if msg.get("method").is_some() {
                    // Server -> client request: answer pings, decline the rest.
                    if let Some(id) = msg.get("id") {
                        let reply = if msg["method"] == "ping" {
                            json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                        } else {
                            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported" } })
                        };
                        let _ = write_line(&w, &reply);
                    }
                } else if let Some(id) = msg["id"].as_i64()
                    && let Some(tx) = p.lock().unwrap().remove(&id)
                {
                    let _ = tx.send(msg);
                }
            }
            p.lock().unwrap().clear();
        });
        Ok(Self { stdin, pending, next: AtomicI64::new(1), child: Mutex::new(child) })
    }
}

impl Transport for Stdio_ {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = bounded(1);
        self.pending.lock().unwrap().insert(id, tx);
        write_line(&self.stdin, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let msg = rx.recv_timeout(timeout).map_err(|_| anyhow::anyhow!("no reply to {method}"))?;
        rpc_result(msg)
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        write_line(&self.stdin, &json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }
}

impl Drop for Stdio_ {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

fn rpc_result(msg: Value) -> Result<Value> {
    if let Some(e) = msg.get("error") {
        bail!("{}", e["message"].as_str().unwrap_or("MCP error"));
    }
    Ok(msg["result"].clone())
}

// ---------------------------------------------------------------------------
// streamable HTTP

struct Http {
    url: String,
    session: Mutex<Option<String>>,
    next: AtomicI64,
}

impl Http {
    fn post(&self, body: &Value, timeout: Duration) -> Result<Option<Value>> {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(timeout)).build().into();
        let mut req = agent
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL_VERSION);
        if let Some(s) = self.session.lock().unwrap().clone() {
            req = req.header("mcp-session-id", s);
        }
        let mut resp = req.send(serde_json::to_vec(body)?)?;
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock().unwrap() = Some(s.to_string());
        }
        let status = resp.status().as_u16();
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let text = resp.body_mut().with_config().limit(256 * 1024 * 1024).read_to_string()?;
        if status == 202 || text.trim().is_empty() {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            bail!("HTTP {status}: {}", text.chars().take(300).collect::<String>());
        }
        let id = body.get("id").cloned();
        if ctype.contains("text/event-stream") {
            // Find the response with our id among the SSE events.
            for ev in text.split("\n\n") {
                let data: String = ev.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("\n");
                if let Ok(v) = serde_json::from_str::<Value>(&data)
                    && v.get("id") == id.as_ref()
                    && v.get("method").is_none()
                {
                    return Ok(Some(v));
                }
            }
            bail!("no response in event stream");
        }
        Ok(Some(serde_json::from_str(&text)?))
    }
}

impl Transport for Http {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }), timeout)?;
        rpc_result(msg.context("empty response")?)
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.post(&json!({ "jsonrpc": "2.0", "method": method, "params": params }), Duration::from_secs(30)).map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Hub

struct Server {
    transport: Box<dyn Transport>,
    /// exposed name -> (original name, definition)
    tools: Vec<(String, String, ToolDef)>,
}

#[derive(Default)]
pub struct McpHub {
    servers: Mutex<Vec<Arc<Server>>>,
    /// (server, status) for the settings panel.
    pub status: Mutex<BTreeMap<String, String>>,
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

fn connect(name: &str, cfg: &McpServerCfg) -> Result<Server> {
    let transport: Box<dyn Transport> = if !cfg.url.is_empty() {
        Box::new(Http { url: cfg.url.clone(), session: Mutex::new(None), next: AtomicI64::new(1) })
    } else if !cfg.command.is_empty() {
        Box::new(Stdio_::spawn(cfg)?)
    } else {
        bail!("needs `command` or `url`");
    };
    let t = Duration::from_secs(60);
    transport.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "simready-viewer", "version": env!("CARGO_PKG_VERSION") },
        }),
        t,
    )?;
    transport.notify("notifications/initialized", json!({}))?;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let r = transport.request("tools/list", params, t)?;
        for tool in r["tools"].as_array().into_iter().flatten() {
            let orig = tool["name"].as_str().unwrap_or("").to_string();
            let exposed: String = format!("{}__{}", sanitize(name), sanitize(&orig)).chars().take(64).collect();
            let schema = if tool["inputSchema"].is_object() { tool["inputSchema"].clone() } else { json!({ "type": "object" }) };
            let def = ToolDef {
                name: exposed.clone(),
                description: format!("[{name} MCP] {}", tool["description"].as_str().unwrap_or("")),
                schema,
            };
            tools.push((exposed, orig, def));
        }
        cursor = r["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(Server { transport, tools })
}

impl McpHub {
    /// (Re)connects every enabled server in the background.
    pub fn connect_all(self: &Arc<Self>, servers: &BTreeMap<String, McpServerCfg>) {
        self.servers.lock().unwrap().clear();
        let mut status = self.status.lock().unwrap();
        status.clear();
        for (name, cfg) in servers {
            if !cfg.enabled {
                status.insert(name.clone(), "disabled".into());
                continue;
            }
            status.insert(name.clone(), "connecting…".into());
            let (hub, name, cfg) = (self.clone(), name.clone(), cfg.clone());
            std::thread::spawn(move || {
                let s = match connect(&name, &cfg) {
                    Ok(server) => {
                        let msg = format!("{} tools", server.tools.len());
                        hub.servers.lock().unwrap().push(Arc::new(server));
                        msg
                    }
                    Err(e) => format!("error: {e:#}"),
                };
                hub.status.lock().unwrap().insert(name, s);
            });
        }
    }

    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.servers.lock().unwrap().iter().flat_map(|s| s.tools.iter().map(|t| t.2.clone())).collect()
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.servers.lock().unwrap().iter().any(|s| s.tools.iter().any(|t| t.0 == name))
    }

    /// Runs an MCP tool; `None` if no connected server owns `name`.
    pub fn call(&self, name: &str, input: &Value) -> Option<ToolOutput> {
        let (server, orig) = {
            let servers = self.servers.lock().unwrap();
            let s = servers.iter().find(|s| s.tools.iter().any(|t| t.0 == name))?.clone();
            let orig = s.tools.iter().find(|t| t.0 == name).unwrap().1.clone();
            (s, orig)
        };
        Some(match server.transport.request("tools/call", json!({ "name": orig, "arguments": input }), Duration::from_secs(600)) {
            Ok(r) => result_to_output(&r),
            Err(e) => ToolOutput::err(format!("MCP call failed: {e:#}")),
        })
    }
}

fn result_to_output(r: &Value) -> ToolOutput {
    let mut content = Vec::new();
    for c in r["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => content.push(Block::Text(c["text"].as_str().unwrap_or("").to_string())),
            Some("image") => {
                if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(c["data"].as_str().unwrap_or("")) {
                    content.push(Block::Image { data: Arc::new(data), mime: c["mimeType"].as_str().unwrap_or("image/png").to_string() });
                }
            }
            Some("resource") => content.push(Block::Text(c["resource"]["text"].as_str().map(str::to_string).unwrap_or_else(|| c["resource"].to_string()))),
            _ => content.push(Block::Text(c.to_string())),
        }
    }
    if content.is_empty()
        && let Some(s) = r.get("structuredContent")
    {
        content.push(Block::Text(s.to_string()));
    }
    ToolOutput { content, is_error: r["isError"].as_bool().unwrap_or(false) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our client against our own server: handshake, session header, tools/list.
    #[test]
    fn client_talks_to_server() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let url = crate::mcp::server::start(18765, tx).unwrap();
        let cfg = McpServerCfg { url, enabled: true, ..Default::default() };
        let server = connect("self", &cfg).unwrap();
        assert_eq!(server.tools.len(), crate::agent::tools::tool_defs(true).len());
        assert!(server.tools.iter().any(|t| t.0 == "self__place_objects" && t.1 == "place_objects"));
        let r = server.transport.request("ping", json!({}), Duration::from_secs(5)).unwrap();
        assert!(r.is_object());
        assert!(server.transport.request("nope", json!({}), Duration::from_secs(5)).is_err());
    }

    #[test]
    fn converts_results() {
        let out = result_to_output(&json!({
            "content": [{ "type": "text", "text": "hi" }, { "type": "image", "data": "AQID", "mimeType": "image/png" }],
            "isError": true
        }));
        assert!(out.is_error);
        assert!(matches!(&out.content[1], Block::Image { data, .. } if data.as_slice() == [1, 2, 3]));
    }
}
