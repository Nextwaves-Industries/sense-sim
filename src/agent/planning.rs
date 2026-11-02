//! Plan mode: the assistant may look but not touch, proposes what it would do, and starts only
//! once the user approves. The gate enforces the "not touch" part, so it doesn't rest on the
//! model's good behaviour.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use serde_json::{Value, json};

use super::gate::CallCtx;
use super::harness::{Turn, Who};
use super::llm::ToolDef;
use super::tools::{self, ToolOutput};
use super::{Event, Question};

pub const TOOL: &str = "propose_plan";
pub const APPROVE: &str = "Approve and start";
const CANCEL: &str = "Cancel";

/// Added to the system prompt while plan mode is on.
pub const NOTE: &str = "\n\nPLAN MODE IS ON. Nothing can change until the user approves: tools that change the scene, the project or files are refused, and solvers don't run. \
Look first: read the scene, what is on the user's screen, the knowledge base, and reference designs. Then call propose_plan with what you'd do, the assumptions you made, what would change, and how you'd check it worked. \
Base it on what you found, not on what's usual, and name the real numbers you're aiming for. Keep it short enough to read: a person decides from this. \
If the message only asks a question, answer it and don't propose anything. Once approved, carry the plan out without asking again.";

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL.into(),
        description: "Show the user your plan and wait for their decision. They can approve it, ask for changes, or cancel. Only in plan mode.".into(),
        schema: json!({ "type": "object", "properties": {
            "summary": { "type": "string", "description": "What you'll do and why, in a sentence or two" },
            "steps": { "type": "array", "items": { "type": "string" }, "description": "The steps, in order" },
            "assumptions": { "type": "array", "items": { "type": "string" }, "description": "What you assumed that the user may want to correct" },
            "changes": { "type": "array", "items": { "type": "string" }, "description": "What will change: scene, project, files" },
            "checks": { "type": "array", "items": { "type": "string" }, "description": "How you'll know it worked, with target numbers" }
        }, "required": ["summary", "steps"] }),
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect()
}

/// The card's text. Headings come from here and only the content from the model.
pub fn render(input: &Value) -> String {
    let mut out = format!("Proposed plan\n\n{}\n", input["summary"].as_str().unwrap_or("(no summary given)"));
    let section = |out: &mut String, title: &str, items: Vec<String>, numbered: bool| {
        if items.is_empty() {
            return;
        }
        out.push_str(&format!("\n{title}\n"));
        for (i, item) in items.iter().enumerate() {
            if numbered {
                out.push_str(&format!("{}. {item}\n", i + 1));
            } else {
                out.push_str(&format!("- {item}\n"));
            }
        }
    };
    section(&mut out, "Steps", strings(&input["steps"]), true);
    section(&mut out, "Assuming", strings(&input["assumptions"]), false);
    section(&mut out, "This will change", strings(&input["changes"]), false);
    section(&mut out, "It worked if", strings(&input["checks"]), false);
    out
}

/// Shows the plan and waits for the user. On this thread, which is the worker's, so nothing stalls.
pub fn ask(input: &Value, turn: &Turn) -> ToolOutput {
    let steps = strings(&input["steps"]);
    if steps.is_empty() {
        return ToolOutput::err("A plan needs steps: say what you'd do, in order.");
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    // Typing is for asking for changes. Only the button is consent.
    let question = Question { text: render(input), options: vec![APPROVE.to_string(), CANCEL.to_string()], candidates: Vec::new(), allow_text: true, input: String::new(), answer: None, reply: Some(tx), approval: true };
    if turn.ev.send(Event::Question(question)).is_err() {
        return ToolOutput::err("The app is closing.");
    }
    let said = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => break answer.summary(),
            Err(RecvTimeoutError::Disconnected) => break String::new(),
            Err(RecvTimeoutError::Timeout) if turn.cancel.load(Ordering::Relaxed) => break String::new(),
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    let words = said.strip_prefix("User answered: ").unwrap_or("").trim();
    if words == APPROVE {
        turn.planning.store(false, Ordering::Relaxed);
        let _ = turn.ev.send(Event::PlanApproved);
        // The checklist comes from the approved plan, not from what the model recalls of it, so
        // the harness holds it to every step.
        let plan = json!({ "steps": steps.iter().map(|s| json!({ "step": s, "status": "pending" })).collect::<Vec<_>>() });
        let out = tools::call_ctx(&turn.tx, "update_plan", plan.clone(), CallCtx::harness(turn.cancel.clone()));
        turn.log.push(Who::Model, "update_plan", &plan, &out);
        return ToolOutput::text("The user approved this plan. Carry it out now, step by step, keeping update_plan current. Don't ask for approval again.");
    }
    if words == CANCEL || words.is_empty() {
        return ToolOutput::err("The user didn't approve this plan, so nothing changes. Stop here and say so in a sentence. Don't start any of it.");
    }
    ToolOutput::err(format!("The user wants changes before this goes ahead: \"{words}\". Revise the plan and call propose_plan again."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_reads_as_a_plan_and_shows_only_what_it_was_given() {
        let t = render(&json!({ "summary": "Add a CP reader panel.", "steps": ["Open the reference", "Tune it"], "assumptions": ["US band"], "checks": ["Axial ratio under 3 dB"] }));
        for want in ["Add a CP reader panel.", "1. Open the reference", "2. Tune it", "Assuming", "- US band", "It worked if", "- Axial ratio under 3 dB"] {
            assert!(t.contains(want), "{want} in {t}");
        }
        assert!(!t.contains("This will change"), "an empty section stays out: {t}");
    }

    #[test]
    fn a_plan_with_no_summary_or_odd_input_still_renders() {
        assert!(render(&json!({ "steps": "not a list" })).contains("no summary"));
        assert!(render(&json!(null)).contains("Proposed plan"));
    }
}
