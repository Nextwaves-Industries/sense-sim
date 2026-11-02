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

/// The name a client used to reach us. A web page that rebinds its own DNS name to
/// 127.0.0.1 can reach this port, but its requests still carry that name.
fn host_is_local(host: &str) -> bool {
    let (name, port) = host.split_once(':').unwrap_or((host, "0"));
    (name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1") && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
}

/// Browsers name the page behind a request, and any website can POST here, so only pages
/// served from this machine may pass. Clients that aren't browsers send no `Origin`.
/// `null` is refused because any site can produce it from a sandboxed iframe.
fn origin_allowed(origin: &str) -> bool {
    origin.strip_prefix("http://").is_some_and(host_is_local)
}

/// Twin tools expose real factory data, so they stay out of reach of MCP clients, including
/// the listing. So do tools that aim the app at outside systems: this port has no login, so
/// any local process could otherwise point the plant outputs (webhook, Modbus, UDP, OpenEMS
/// writes) wherever it likes. `tools/call` runs any name it's given, which makes the check
/// here the boundary: this is the only caller that marks a tool call as coming from MCP.
pub(crate) fn mcp_may_list(tool: &str) -> bool {
    !tool.starts_with("twin_") && !matches!(tool, "rtls_configure_outputs")
}

/// As [`mcp_may_list`], for one call: `rtls_openems` may report status but not start a server.
fn mcp_may_call(tool: &str, args: &Value) -> bool {
    mcp_may_list(tool) && !(tool == "rtls_openems" && args["action"] == "start")
}

fn handle(mut req: tiny_http::Request, tx: &Sender<ToolJob>) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if path != "/mcp" && path != "/" {
        return respond(req, 404, None, None);
    }
    if !header(&req, "Host").is_some_and(|h| host_is_local(&h)) || !header(&req, "Origin").is_none_or(|o| origin_allowed(&o)) {
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
                .filter(|t| mcp_may_list(&t.name))
                .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.schema }))
                .collect();
            ok(id, json!({ "tools": list }))
        }
        "tools/call" => {
            let name = m["params"]["name"].as_str().unwrap_or("");
            let args = m["params"].get("arguments").cloned().unwrap_or(json!({}));
            if !mcp_may_call(name, &args) {
                return Some(ok(id, json!({ "content": [{ "type": "text", "text": format!("{name} isn't available over MCP") }], "isError": true })));
            }
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use super::*;

    #[test]
    fn only_this_machines_names_count_as_local() {
        for host in ["localhost", "localhost:8765", "LOCALHOST:8765", "127.0.0.1", "127.0.0.1:8765"] {
            assert!(host_is_local(host), "{host}");
        }
        for host in ["", "evil.example", "evil.example:8765", "localhost.evil.example", "127.0.0.1.nip.io", "localhost:", "localhost:80/x", "localhost@evil.example", "0.0.0.0:8765", "192.168.1.5:8765"] {
            assert!(!host_is_local(host), "{host}");
        }
    }

    #[test]
    fn lookalike_and_sandboxed_origins_are_refused() {
        for origin in ["http://localhost", "http://localhost:3000", "http://127.0.0.1:6274"] {
            assert!(origin_allowed(origin), "{origin}");
        }
        for origin in ["null", "", "http://localhost.evil.example", "http://127.0.0.1.evil.example", "http://localhost:80@evil.example", "https://evil.example", "http://evil.example", "file://"] {
            assert!(!origin_allowed(origin), "{origin}");
        }
    }

    #[test]
    fn twin_tools_are_off_limits_and_others_are_not() {
        assert!(!mcp_may_call("twin_summary", &json!({})));
        assert!(mcp_may_call("place_objects", &json!({})));
    }

    #[test]
    fn outside_systems_cant_be_aimed_at_over_mcp() {
        assert!(!mcp_may_list("rtls_configure_outputs"));
        assert!(!mcp_may_call("rtls_configure_outputs", &json!({ "webhook": "http://x" })));
        assert!(!mcp_may_call("rtls_openems", &json!({ "action": "start" })));
        assert!(mcp_may_call("rtls_openems", &json!({ "action": "status" })));
        assert!(mcp_may_list("rtls_openems"), "status stays readable");
    }

    /// Sends one raw HTTP request to a real server and returns (status, body).
    fn send(port: u16, host: &str, origin: Option<&str>, body: &str) -> (u16, String) {
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        // A tool call that wrongly reaches the queue waits forever, so fail instead of hanging.
        tcp.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
        // HTTP/1.0 so a long reply isn't chunked, which this little reader doesn't undo.
        write!(tcp, "POST /mcp HTTP/1.0\r\nHost: {host}\r\n{origin}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut reply = String::new();
        tcp.read_to_string(&mut reply).unwrap();
        let status = reply.split(' ').nth(1).unwrap().parse().unwrap();
        (status, reply.split_once("\r\n\r\n").unwrap().1.to_string())
    }

    #[test]
    fn the_running_server_enforces_all_of_it() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (tx, jobs) = crossbeam_channel::unbounded();
        start(port, tx).unwrap();
        let local = format!("127.0.0.1:{port}");
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let twin = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"twin_summary"}}"#;

        // Normal clients keep working: no Origin, or a local page.
        assert_eq!(send(port, &local, None, ping).0, 200);
        assert_eq!(send(port, &format!("localhost:{port}"), Some("http://localhost:3000"), ping).0, 200);

        // DNS rebinding carries the attacker's name in Host.
        assert_eq!(send(port, &format!("evil.example:{port}"), None, ping).0, 403);
        // Cross-site pages, including the lookalike and sandboxed ones the old check let in.
        assert_eq!(send(port, &local, Some("http://localhost.evil.example"), ping).0, 403);
        assert_eq!(send(port, &local, Some("null"), ping).0, 403);
        assert_eq!(send(port, &local, Some("https://evil.example"), ping).0, 403);

        // Twin tools are neither listed nor callable, and a refused call never queues a job.
        let (_, listed) = send(port, &local, None, list);
        let listed: Value = serde_json::from_str(&listed).unwrap();
        let names: Vec<&str> = listed["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(!names.is_empty());
        assert!(!names.iter().any(|n| n.starts_with("twin_")), "{names:?}");

        let (status, called) = send(port, &local, None, twin);
        let called: Value = serde_json::from_str(&called).unwrap();
        assert_eq!(status, 200);
        assert_eq!(called["result"]["isError"], true);
        assert!(jobs.is_empty(), "a refused twin tool must not reach the tool queue");
    }
}
