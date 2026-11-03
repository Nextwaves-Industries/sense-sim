//! Pins what the harness does, down to every event and every request, on a scripted model and a
//! fake tool bridge. Restructuring the loop must leave `testdata/harness_golden.txt` untouched.
//!
//! When behaviour changes on purpose: `UPDATE_HARNESS_GOLDEN=1 cargo test --bin SenseSim golden`,
//! then read the diff before committing it.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Sender, unbounded};
use serde_json::{Value, json};

use super::llm::{Block, Message, Provider, Role, ToolDef};
use super::tools::{ToolJob, ToolOutput};
use super::{Event, photocheck, prompt, worker};
use crate::mcp::client::McpHub;

fn say(t: &str) -> Message {
    Message { role: Role::Assistant, content: vec![Block::Text(t.into())], raw: None, usage: None }
}

fn calls(uses: &[(&str, Value)]) -> Message {
    let content = uses.iter().enumerate().map(|(i, (name, input))| Block::ToolUse { id: format!("t{i}"), name: (*name).into(), input: input.clone() }).collect();
    Message { role: Role::Assistant, content, raw: None, usage: None }
}

fn render_block(b: &Block) -> String {
    match b {
        Block::Text(t) => format!("text {t:?}"),
        Block::Image { data, mime } => format!("image {} bytes {mime}", data.len()),
        Block::ToolUse { id, name, input } => format!("use {id} {name} {input}"),
        Block::ToolResult { id, content, is_error } => format!("result {id} error={is_error} [{}]", content.iter().map(render_block).collect::<Vec<_>>().join(", ")),
    }
}

fn render_message(m: &Message) -> String {
    let role = if m.role == Role::User { "user" } else { "assistant" };
    format!("{role}: {}", m.content.iter().map(render_block).collect::<Vec<_>>().join(" | "))
}

/// Replies from a script, and writes down everything it was asked.
struct Script {
    replies: Mutex<VecDeque<Result<Message, String>>>,
    log: Mutex<Vec<String>>,
}

impl Provider for Script {
    fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        let who = if system == prompt::SYSTEM {
            "main"
        } else if system == photocheck::REVIEWER_SYSTEM {
            "reviewer"
        } else {
            "other"
        };
        let mut log = self.log.lock().unwrap();
        log.push(format!("  request to {who}, {} tools:", tools.len()));
        log.extend(messages.iter().map(|m| format!("    {}", render_message(m))));
        match self.replies.lock().unwrap().pop_front() {
            Some(Ok(m)) => Ok(m),
            Some(Err(e)) => Err(anyhow!("{e}")),
            None => Ok(say("(the script ran out)")),
        }
    }
}

#[derive(Clone)]
struct Out {
    text: String,
    is_error: bool,
    image: bool,
}

fn ok(text: impl Into<String>) -> Out {
    Out { text: text.into(), is_error: false, image: false }
}
fn fail(text: impl Into<String>) -> Out {
    Out { text: text.into(), is_error: true, image: false }
}
fn pic(text: impl Into<String>) -> Out {
    Out { text: text.into(), is_error: false, image: true }
}

/// Stands in for `run_tool_jobs`: answers each job from a per-tool list, `{}` once that runs out.
fn bridge(answers: Vec<(&str, Vec<Out>)>, log: Arc<Mutex<Vec<String>>>) -> Sender<ToolJob> {
    let (tx, rx) = unbounded::<ToolJob>();
    let mut answers: HashMap<String, VecDeque<Out>> = answers.into_iter().map(|(k, v)| (k.to_string(), v.into())).collect();
    std::thread::spawn(move || {
        for job in rx {
            log.lock().unwrap().push(format!("  tool job {} {}", job.name, job.input));
            let out = answers.get_mut(&job.name).and_then(VecDeque::pop_front).unwrap_or_else(|| ok("{}"));
            let mut content = vec![Block::Text(out.text)];
            if out.image {
                content.push(Block::Image { data: Arc::new(vec![1, 2, 3]), mime: "image/png".into() });
            }
            let _ = job.reply.send(ToolOutput { content, is_error: out.is_error });
        }
    });
    tx
}

fn test_tools() -> Vec<ToolDef> {
    ["a", "b"].iter().map(|n| ToolDef { name: (*n).to_string(), description: String::new(), schema: json!({}) }).collect()
}

struct Case {
    name: &'static str,
    replies: Vec<Result<Message, String>>,
    answers: Vec<(&'static str, Vec<Out>)>,
    max_steps: usize,
    cancelled: bool,
}

impl Case {
    fn new(name: &'static str, replies: Vec<Message>) -> Self {
        Self { name, replies: replies.into_iter().map(Ok).collect(), answers: Vec::new(), max_steps: 150, cancelled: false }
    }
    fn answers(mut self, answers: Vec<(&'static str, Vec<Out>)>) -> Self {
        self.answers = answers;
        self
    }
}

/// Runs a case through the real worker. Tool jobs and model requests are logged in one list, so
/// their relative order is pinned too.
fn run(case: Case) -> String {
    let log = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(Script { replies: Mutex::new(case.replies.into()), log: Mutex::new(Vec::new()) });
    let tx = bridge(case.answers, log.clone());
    let (ev_tx, ev_rx) = unbounded();
    let cancel = Arc::new(AtomicBool::new(case.cancelled));
    let history = vec![Message::user(vec![Block::Text("the user's request".into())])];
    worker(script.clone(), case.max_steps, history, test_tools(), tx, Arc::new(McpHub::default()), ev_tx, cancel, Default::default());

    let mut out = vec![format!("=== {}", case.name)];
    out.push(" model calls:".into());
    out.extend(script.log.lock().unwrap().iter().cloned());
    out.push(" tool jobs:".into());
    // Tools running together arrive in either order.
    let mut jobs = log.lock().unwrap().clone();
    if case.name.contains("together") {
        jobs.sort();
    }
    out.extend(jobs);
    out.push(" events:".into());
    for e in ev_rx.try_iter() {
        out.push(match e {
            Event::Assistant(t) => format!("  assistant {t:?}"),
            Event::ToolStart { name, input } => format!("  tool start {name} {input}"),
            Event::ToolEnd { output, is_error } => format!("  tool end error={is_error} {output:?}"),
            Event::Info(t) => format!("  info {t:?}"),
            // Ids come from a process-wide counter.
            Event::Image(_) => "  image".to_string(),
            // Only sent when a reply carries usage or thinking, or a budget runs out. These cases do none of that.
            Event::Usage { .. } | Event::Thinking(_) | Event::ThinkingVoid | Event::Question(_) | Event::PlanApproved => unreachable!("not in these cases"),
            Event::Finished { history, error } => {
                let mut s = format!("  finished error={error:?}");
                for m in &history {
                    s.push_str(&format!("\n    {}", render_message(m)));
                }
                s
            }
        });
    }
    out.join("\n")
}

fn cases() -> Vec<Case> {
    let issues = json!({ "issue_count": 2, "issues": ["a floats", "b is sunk"] }).to_string();
    let place = || calls(&[("place_objects", json!({ "objects": [] }))]);
    let camera = "{\n\"camera\": { \"eye\": [1, 2, 3], \"target\": [0, 0, 0] }\n}";
    let bad_review = json!({ "score": 4, "differences": [{ "severity": "major", "what": "no ceiling", "where": "top", "fix": "add one" }] }).to_string();
    let good_review = json!({ "score": 8, "differences": [] }).to_string();
    let not_met = json!({ "verdict": "not_met", "checks": [{ "name": "match", "pass": false, "measured": "-3 dB", "required": "-10 dB" }], "diagnosis": ["Too long."], "next": ["Shorten it."] }).to_string();
    let plan = |status: &str| calls(&[("update_plan", json!({ "steps": [{ "step": "do it", "status": status }] }))]);

    vec![
        Case::new("plain answer", vec![say("hello")]),
        Case::new("self-check sends back, then passes", vec![place(), say("done"), calls(&[("update_object", json!({ "name": "a" }))]), say("fixed")]).answers(vec![("verify_scene", vec![fail(issues.clone()), ok("{}")])]),
        Case::new("self-check gives up after its rounds", vec![place(), say("1"), place(), say("2"), place(), say("3"), place(), say("4"), place(), say("5")]).answers(vec![("verify_scene", vec![fail(issues.clone()); 5])]),
        Case::new("rtls target missed, then met", vec![calls(&[("rtls_auto_deploy", json!({ "target_cm": 30 }))]), say("placed"), calls(&[("rtls_run", json!({}))]), say("met")]).answers(vec![
            ("rtls_auto_deploy", vec![ok(json!({ "targets_met": false, "predicted_p95_m": 0.42, "floor_coverage": 0.8, "test": { "cep95_m": 0.55, "located": 7, "tags": 9 } }).to_string())]),
            ("rtls_run", vec![ok(json!({ "cep95_m": 0.25 }).to_string())]),
        ]),
        Case::new("rtls gives up after its rounds", vec![calls(&[("rtls_auto_deploy", json!({ "target_cm": 30 }))]), say("1"), calls(&[("rtls_run", json!({}))]), say("2"), calls(&[("rtls_run", json!({}))]), say("3"), calls(&[("rtls_run", json!({}))]), say("4")]).answers(vec![
            ("rtls_auto_deploy", vec![ok(json!({ "targets_met": false }).to_string())]),
            ("rtls_run", vec![ok(json!({ "cep95_m": 0.9 }).to_string()); 3]),
        ]),
        Case::new("plant changed, run fails, then runs clean", vec![calls(&[("plant_add_part", json!({}))]), say("added"), calls(&[("plant_run", json!({}))]), say("ran"), calls(&[("plant_run", json!({}))]), say("clean")]).answers(vec![(
            "plant_run",
            vec![ok(json!({ "ok": false, "alarms": [{ "alarm": "dry pump" }], "open_pipe_ports": ["P1.out"] }).to_string()), ok(json!({ "ok": true }).to_string())],
        )]),
        Case::new("antenna not judged, not met, mesh reminder", vec![calls(&[("antenna_new", json!({}))]), say("made"), calls(&[("antenna_assess", json!({}))]), say("assessed"), calls(&[("antenna_assess", json!({}))]), say("again"), say("done")]).answers(vec![("antenna_assess", vec![ok(not_met), ok(json!({ "verdict": "met_mesh_unchecked" }).to_string())])]),
        Case::new("plan left open, then finished", vec![plan("in_progress"), say("stopping"), plan("done"), say("finished")]),
        Case::new(
            "photo review fails, then passes",
            vec![
                calls(&[("perceive_photo", json!({ "image_id": "img1" })), ("place_objects", json!({}))]),
                say("rebuilt"),
                say(&bad_review),
                place(),
                say("fixed"),
                say(&good_review),
            ],
        )
        .answers(vec![
            ("perceive_photo", vec![pic("{}")]),
            ("compare_to_photo", vec![pic(camera), pic(camera)]),
            ("screenshot", vec![pic("{}"), pic("{}")]),
            ("check_against_depth", vec![ok(json!({ "disagreements": ["the shelf is further away"] }).to_string()), ok("{}")]),
        ]),
        Case::new("photo review can't be read, so it counts as unreviewed", vec![calls(&[("perceive_photo", json!({ "image_id": "img1" }))]), place(), say("rebuilt"), say("not json")]).answers(vec![("perceive_photo", vec![pic("{}")]), ("compare_to_photo", vec![pic(camera)]), ("screenshot", vec![pic("{}")])]),
        Case::new("lookups run together", vec![calls(&[("get_scene", json!({})), ("search_assets", json!({ "query": "x" }))]), say("found")]).answers(vec![("search_assets", vec![ok("[1]")])]),
        Case::new("a failed tool goes back to the model", vec![calls(&[("get_asset_info", json!({ "id": "x" }))]), say("it failed")]).answers(vec![("get_asset_info", vec![fail("no such asset")])]),
        Case { name: "a provider error ends the turn", replies: vec![Err("HTTP 500".into())], answers: Vec::new(), max_steps: 150, cancelled: false },
        Case { name: "the step limit ends the turn", replies: vec![Ok(place()), Ok(place()), Ok(place())], answers: Vec::new(), max_steps: 2, cancelled: false },
        Case { name: "stopped before the first call", replies: vec![Ok(say("never"))], answers: Vec::new(), max_steps: 150, cancelled: true },
    ]
}

#[test]
fn the_harness_behaves_exactly_as_recorded() {
    let got = cases().into_iter().map(run).collect::<Vec<_>>().join("\n\n") + "\n";
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/agent/testdata/harness_golden.txt");
    if std::env::var_os("UPDATE_HARNESS_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, &got).unwrap();
        return;
    }
    // Git may have checked it out with CRLF.
    let want = std::fs::read_to_string(path).expect("run with UPDATE_HARNESS_GOLDEN=1 to record it").replace("\r\n", "\n");
    if got != want {
        let line = got.lines().zip(want.lines()).position(|(a, b)| a != b).unwrap_or_else(|| got.lines().count().min(want.lines().count()));
        panic!("line {}:\n  got  {:?}\n  want {:?}", line + 1, got.lines().nth(line), want.lines().nth(line));
    }
}

/// Before the log, a macro that changed an antenna or a plant slipped past their checks, because
/// the checks only saw the macro's own name.
#[test]
fn checks_see_what_a_macro_did() {
    use super::harness::{LogEntry, OnStop, StopChecks, Turn, Who};
    use super::checks::SelfChecks;

    let (tx, _rx) = unbounded();
    let (ev, _events) = unbounded();
    let turn = Turn::new(Vec::new(), Arc::default(), tx, Arc::new(McpHub::default()), ev);
    let provider = Script { replies: Mutex::default(), log: Mutex::default() };
    let leaf = |name: &str, is_error: bool| LogEntry { who: Who::Macro, name: name.into(), input: json!({}), output: "{}".into(), is_error };

    // A failed step isn't taken as a finished change.
    let mut checks = SelfChecks::default();
    checks.observe(&leaf("antenna_set", true));
    assert!(matches!(checks.on_stop(&provider, &turn), OnStop::Finish));

    let mut checks = SelfChecks::default();
    checks.observe(&leaf("antenna_set", false));
    let OnStop::SendBack(reason) = checks.on_stop(&provider, &turn) else { panic!("the macro's antenna change went unnoticed") };
    assert!(matches!(&reason[0], Block::Text(t) if t.contains("antenna_assess")));
}

/// A nested scene change has to trigger the same self-check a direct one does.
#[test]
fn a_macro_changing_the_scene_gets_verified() {
    use super::harness::{LogEntry, OnStop, StopChecks, Turn, Who};
    use super::checks::SelfChecks;

    let log = Arc::new(Mutex::new(Vec::new()));
    let issues = json!({ "issue_count": 1 }).to_string();
    let tx = bridge(vec![("verify_scene", vec![fail(issues)])], log.clone());
    let (ev, _events) = unbounded();
    let turn = Turn::new(Vec::new(), Arc::default(), tx, Arc::new(McpHub::default()), ev);
    let provider = Script { replies: Mutex::default(), log: Mutex::default() };
    let mut checks = SelfChecks::default();
    checks.observe(&LogEntry { who: Who::Macro, name: "place_objects".into(), input: json!({}), output: "{}".into(), is_error: false });
    assert!(matches!(checks.on_stop(&provider, &turn), OnStop::SendBack(_)));
    assert!(log.lock().unwrap().iter().any(|l| l.contains("verify_scene")));
}
