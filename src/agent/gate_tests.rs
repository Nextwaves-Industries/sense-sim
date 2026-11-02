//! The gate through the real tool system: `run_tool_jobs`, the question cards and the parked call.
//! A child of `tools`, for its private parts.

use std::sync::atomic::AtomicBool;

use super::*;
use crate::agent::config::AgentConfig;
use crate::agent::gate::Mode;
use crate::agent::harness::{TurnLog, Who};

/// A tool whose handler needs nothing from the world, and says so when it has run.
const TOOL: &str = "delete_tool";
const RAN: &str = "no saved tool";

fn world(mode: Mode) -> World {
    let mut config = AgentConfig::default();
    config.approval = mode;
    let mut w = World::new();
    w.insert_resource(Agent::new(config));
    w.insert_resource(ToolBridge::default());
    w.init_resource::<PendingTools>();
    w
}

fn send(w: &World, ctx: CallCtx) -> Receiver<ToolOutput> {
    let (reply, rx) = unbounded();
    let input = json!({ "name": "no_such_tool_for_the_gate_test" });
    w.resource::<ToolBridge>().tx.send(ToolJob { name: TOOL.into(), input, reply, ctx, check_only: false }).unwrap();
    rx
}

fn answer(w: &mut World, text: &str) {
    w.resource_mut::<Agent>().questions.last_mut().expect("a card").answer(text.into());
}

fn parked(w: &World) -> usize {
    w.resource::<PendingTools>().0.len()
}

/// The first built-in macro, with arguments it accepts.
fn a_macro() -> (String, Value) {
    let def = toolmaker::defs().into_iter().next().expect("the built-in macros");
    let params = def.schema["properties"].as_object().cloned().unwrap_or_default();
    let input: serde_json::Map<String, Value> = params.iter().map(|(k, p)| (k.clone(), p.get("default").cloned().unwrap_or(json!(1)))).collect();
    (def.name, Value::Object(input))
}

#[test]
fn a_call_that_asks_waits_on_a_card_and_runs_only_on_allow() {
    let mut w = world(Mode::Ask);
    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().is_err(), "nothing runs before the answer");
    assert_eq!(parked(&w), 1);
    {
        let agent = w.resource::<Agent>();
        let card = agent.questions.last().unwrap();
        assert!(card.approval && !card.allow_text, "buttons only: typing can't be taken for consent");
        assert_eq!(card.options, ["Allow", "Allow edits for this chat", "Deny"]);
        assert!(card.text.contains(TOOL));
        assert!(matches!(agent.transcript.last(), Some(ChatItem::Question(_))));
        assert!(agent.open, "the panel opens so the card can be seen");
    }
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().is_err(), "it keeps waiting from frame to frame");

    answer(&mut w, "Allow");
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().unwrap().summary().contains(RAN));
    assert_eq!(parked(&w), 0);
}

#[test]
fn deny_and_anything_else_dont_run_it() {
    for said in ["Deny", "allow", "Allow ", "yes", "Allow please"] {
        let mut w = world(Mode::Ask);
        let rx = send(&w, CallCtx::default());
        run_tool_jobs(&mut w);
        answer(&mut w, said);
        run_tool_jobs(&mut w);
        let out = rx.try_recv().unwrap();
        assert!(out.is_error && !out.summary().contains(RAN), "{said}: {}", out.summary());
        assert!(out.summary().contains("did not allow"), "{said}");
    }
}

#[test]
fn allowing_for_the_chat_lasts_until_a_new_chat() {
    let mut w = world(Mode::Ask);
    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    answer(&mut w, "Allow edits for this chat");
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().unwrap().summary().contains(RAN));

    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().unwrap().summary().contains(RAN), "no second card");
    assert_eq!(w.resource::<Agent>().questions.len(), 1);

    w.resource_mut::<Agent>().new_chat();
    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().is_err(), "a new chat starts asking again");
    assert_eq!(parked(&w), 1);
}

#[test]
fn stop_and_new_chat_deny_a_waiting_call() {
    let mut w = world(Mode::Ask);
    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    w.resource_mut::<Agent>().stop();
    run_tool_jobs(&mut w);
    let out = rx.try_recv().unwrap();
    assert!(out.is_error && !out.summary().contains(RAN), "{}", out.summary());

    let mut w = world(Mode::Ask);
    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    w.resource_mut::<Agent>().new_chat();
    run_tool_jobs(&mut w);
    let out = rx.try_recv().unwrap();
    assert!(out.is_error && !out.summary().contains(RAN), "{}", out.summary());
    assert_eq!(parked(&w), 0, "nothing is left waiting");
}

#[test]
fn a_cancelled_run_releases_its_card_and_queued_calls_dont_act() {
    // Cancelled while the card is up, without the card being answered.
    let mut w = world(Mode::Ask);
    let cancel = Arc::new(AtomicBool::new(false));
    let rx = send(&w, CallCtx { cancel: Some(cancel.clone()), ..Default::default() });
    run_tool_jobs(&mut w);
    cancel.store(true, Ordering::Relaxed);
    run_tool_jobs(&mut w);
    assert!(!rx.try_recv().unwrap().summary().contains(RAN));

    // Cancelled while still in the queue.
    let mut w = world(Mode::Edits);
    let cancel = Arc::new(AtomicBool::new(true));
    let rx = send(&w, CallCtx { cancel: Some(cancel), ..Default::default() });
    run_tool_jobs(&mut w);
    let out = rx.try_recv().unwrap();
    assert!(out.is_error && out.summary().contains("cancelled"), "{}", out.summary());
}

#[test]
fn edits_mode_runs_without_a_card_and_mcp_is_refused_not_parked() {
    let mut w = world(Mode::Edits);
    let rx = send(&w, CallCtx::default());
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().unwrap().summary().contains(RAN));
    assert!(w.resource::<Agent>().questions.is_empty());

    let mut w = world(Mode::Ask);
    let rx = send(&w, CallCtx { origin: Origin::Mcp, ..Default::default() });
    run_tool_jobs(&mut w);
    let out = rx.try_recv().unwrap();
    assert!(out.is_error && out.summary().contains("MCP client"), "{}", out.summary());
    assert_eq!(parked(&w), 0);
    assert!(w.resource::<Agent>().questions.is_empty());
}

#[test]
fn the_harness_can_check_the_models_work_in_ask_mode() {
    let mut w = world(Mode::Ask);
    let rx = send(&w, CallCtx::harness(Arc::default()));
    run_tool_jobs(&mut w);
    assert!(rx.try_recv().unwrap().summary().contains(RAN));
}

/// A macro can't launder an MCP client's call: its steps are judged as the client's.
#[test]
fn macro_steps_inherit_who_asked() {
    let (tx, jobs) = unbounded::<ToolJob>();
    let seen = std::thread::spawn(move || {
        let mut origins = Vec::new();
        for job in jobs {
            origins.push(job.ctx.origin);
            let _ = job.reply.send(ToolOutput::text("{}"));
        }
        origins
    });
    let log = TurnLog::default();
    let (name, input) = a_macro();
    let out = toolmaker::run(&name, &input, tx, CallCtx { origin: Origin::Mcp, log: Some(log.clone()), cancel: None });
    assert!(!out.is_error, "{}", out.summary());
    let origins = seen.join().unwrap();
    assert!(!origins.is_empty());
    assert!(origins.iter().all(|o| *o == Origin::Mcp), "{origins:?}");
    let entries = log.entries();
    assert_eq!(entries.len(), origins.len(), "every step is in the turn's record");
    assert!(entries.iter().all(|e| e.who == Who::Macro));
}

#[test]
fn stop_ends_a_macro_between_steps() {
    let (tx, _jobs) = unbounded::<ToolJob>();
    let (name, input) = a_macro();
    let ctx = CallCtx { cancel: Some(Arc::new(AtomicBool::new(true))), ..Default::default() };
    let out = toolmaker::run(&name, &input, tx, ctx);
    assert!(out.is_error && out.summary().contains("stopped before"), "{}", out.summary());
}
