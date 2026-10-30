//! MCP server (streamable HTTP, JSON responses) exposing the viewer's tools on
//! `http://127.0.0.1:<port>/mcp`, e.g. for
//! `claude mcp add --transport http simready http://127.0.0.1:8765/mcp`.


use base64::Engine;
use crossbeam_channel::Sender;
use serde_json::{Value, json};

use crate::agent::llm::Block;
use crate::agent::tools::{self, ToolJob};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn start(port: u16, tx: Sender<ToolJob>) -> Result<String, String> {
    let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
    std::thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                let tx = tx.clone();
                // Calls such as ask_user can block for a long time.
                std::thread::spawn(move || handle(req, &tx));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(format!("http://127.0.0.1:{port}/mcp"))
}

fn header(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn respond(req: tiny_http::Request, status: u16, body: Option<Value>, session: Option<&str>) {
    let mut resp = match body {
        Some(b) => tiny_http::Response::from_string(b.to_string())
            .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap()),
        None => tiny_http::Response::from_string(String::new()),
    }
    .with_status_code(status);
    if let Some(s) = session
        && let Ok(h) = tiny_http::Header::from_bytes("Mcp-Session-Id", s)
    {
        resp = resp.with_header(h);
    }
    let _ = req.respond(resp);
}

fn handle(mut req: tiny_http::Request, tx: &Sender<ToolJob>) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if path != "/mcp" && path != "/" {
        return respond(req, 404, None, None);
    }
    // Only local pages may talk to us (DNS-rebinding protection).
    if let Some(origin) = header(&req, "Origin")
        && !(origin.starts_with("http://localhost") || origin.starts_with("http://127.0.0.1") || origin == "null")
    {
        return respond(req, 403, None, None);
    }
    match req.method() {
        tiny_http::Method::Post => {}
        tiny_http::Method::Delete => return respond(req, 200, None, None),
        // No server-initiated stream.
        _ => return respond(req, 405, None, None),
    }
    let mut body = String::new();
    if req.as_reader().read_to_string(&mut body).is_err() {
        return respond(req, 400, None, None);
    }
    let Ok(msg) = serde_json::from_str::<Value>(&body) else {
        return respond(req, 400, Some(error(Value::Null, -32700, "parse error")), None);
    };
    let mut session = None;
    let replies: Vec<Value> = match &msg {
        Value::Array(batch) => batch.iter().filter_map(|m| dispatch(m, tx, &mut session)).collect(),
        m => dispatch(m, tx, &mut session).into_iter().collect(),
    };
    let session = session.as_deref();
    match (msg.is_array(), replies.len()) {
        (_, 0) => respond(req, 202, None, session),
        (false, _) => respond(req, 200, replies.into_iter().next(), session),
        (true, _) => respond(req, 200, Some(Value::Array(replies)), session),
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn dispatch(m: &Value, tx: &Sender<ToolJob>, session: &mut Option<String>) -> Option<Value> {
    let id = m.get("id").cloned();
    let method = m["method"].as_str().unwrap_or("");
    let id = id?; // notifications get no reply
    Some(match method {
        "initialize" => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            *session = Some(format!("simready-{nanos:x}"));
            let requested = m["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION);
            let version = if ["2025-03-26", "2025-06-18", "2025-11-25"].contains(&requested) { requested } else { PROTOCOL_VERSION };
            ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "simready-viewer", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Build 3D scenes from NVIDIA SimReady assets in the running viewer. Units are meters, Y up, floor y=0. \
                        Typical flow: search_assets -> place_objects -> screenshot. For a photo: load_reference_image -> estimate_depth -> place_objects.",
                }),
            )
        }
        "ping" => ok(id, json!({})),
        "tools/list" => {
            let list: Vec<Value> = tools::tool_defs(true)
                .into_iter()
                .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.schema }))
                .collect();
            ok(id, json!({ "tools": list }))
        }
        "tools/call" => {
            let name = m["params"]["name"].as_str().unwrap_or("");
            let args = m["params"].get("arguments").cloned().unwrap_or(json!({}));
            let out = tools::call(tx, name, args, true, None);
            ok(id, json!({ "content": out.content.iter().map(content_json).collect::<Vec<_>>(), "isError": out.is_error }))
        }
        "resources/list" => ok(id, json!({ "resources": [] })),
        "prompts/list" => ok(id, json!({ "prompts": [] })),
        _ => error(id, -32601, &format!("method not found: {method}")),
    })
}

pub fn content_json(b: &Block) -> Value {
    match b {
        Block::Text(t) => json!({ "type": "text", "text": t }),
        Block::Image { data, mime } => json!({
            "type": "image",
            "data": base64::engine::general_purpose::STANDARD.encode(data.as_slice()),
            "mimeType": mime,
        }),
        _ => json!({ "type": "text", "text": "" }),
    }
}
