//! Keeping a long conversation within what the model can hold.
//!
//! Providers reject a history whose tool calls and results don't pair up, and reasoning has to be
//! replayed exactly as it came. So assistant messages are never edited: compaction only changes
//! user messages, and removes history by replacing everything before an assistant message.

use std::collections::{BTreeSet, HashSet};

use super::llm::{Block, Message, Role};

/// Compaction starts at this share of the model's context, and aims below the next.
const START_AT: f64 = 0.7;
const AIM_FOR: f64 = 0.5;
/// Images, and full tool results, kept at the end of the conversation.
const KEEP_IMAGES: usize = 4;
const KEEP_RESULTS: usize = 6;
const LONG_RESULT: usize = 2000;
/// Rough tokens for an image, when the provider hasn't said.
const IMAGE_TOKENS: u64 = 1500;

const IMAGE_GONE: &str = "[image removed to save space; take a new screenshot if you need to see this]";

pub const SUMMARY_SYSTEM: &str = "You write the working notes that let an engineer pick up a task where a colleague left it, because the conversation so far is about to be discarded. \
Write what was asked, what was done, decisions and why, file paths, object names and ids, what failed, and what is still open. \
Copy every number together with its unit and its evidence label (Estimate, Simulated, Calibrated, Measured, Synthetic): a number without its label can't be used. \
Don't add anything that isn't in the conversation, and don't address the reader. Plain text, as short as it can be without losing those.";

/// Rough tokens in a text.
fn tokens_of(text: &str) -> u64 {
    text.len() as u64 / 4
}

fn block_tokens(b: &Block) -> u64 {
    match b {
        Block::Text(t) => tokens_of(t),
        Block::Image { .. } => IMAGE_TOKENS,
        Block::ToolUse { input, name, .. } => tokens_of(name) + tokens_of(&input.to_string()),
        Block::ToolResult { content, .. } => content.iter().map(block_tokens).sum(),
    }
}

/// Rough tokens in a history, for when the provider hasn't reported any.
pub fn estimate(history: &[Message]) -> u64 {
    history.iter().flat_map(|m| &m.content).map(block_tokens).sum()
}

/// Whether a provider would accept this history: every tool call is answered by the next message,
/// and every result answers a call in the message before it.
pub fn validate(history: &[Message]) -> Result<(), String> {
    let Some(first) = history.first() else { return Ok(()) };
    if first.role != Role::User {
        return Err("the conversation doesn't start with a user message".into());
    }
    for (i, m) in history.iter().enumerate() {
        let calls: Vec<&str> = m.content.iter().filter_map(|b| if let Block::ToolUse { id, .. } = b { Some(id.as_str()) } else { None }).collect();
        let results: Vec<&str> = m.content.iter().filter_map(|b| if let Block::ToolResult { id, .. } = b { Some(id.as_str()) } else { None }).collect();
        match m.role {
            Role::Assistant => {
                if !results.is_empty() {
                    return Err(format!("message {i}: a tool result from the assistant"));
                }
                if !calls.is_empty() {
                    let answers: HashSet<&str> = history.get(i + 1).filter(|n| n.role == Role::User).map(|n| n.content.iter().filter_map(|b| if let Block::ToolResult { id, .. } = b { Some(id.as_str()) } else { None }).collect()).unwrap_or_default();
                    if let Some(lost) = calls.iter().find(|c| !answers.contains(**c)) {
                        return Err(format!("message {i}: tool call {lost} has no result in the next message"));
                    }
                }
            }
            Role::User => {
                if !calls.is_empty() {
                    return Err(format!("message {i}: a tool call from the user"));
                }
                if !results.is_empty() {
                    let asked: HashSet<&str> = i.checked_sub(1).and_then(|p| history.get(p)).filter(|p| p.role == Role::Assistant).map(|p| p.content.iter().filter_map(|b| if let Block::ToolUse { id, .. } = b { Some(id.as_str()) } else { None }).collect()).unwrap_or_default();
                    if let Some(lost) = results.iter().find(|r| !asked.contains(**r)) {
                        return Err(format!("message {i}: tool result {lost} answers no call in the message before it"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Replaces old images in user messages, keeping the newest few. Returns how many went.
fn drop_old_images(history: &mut [Message]) -> usize {
    fn visit(content: &mut [Block], seen: &mut usize, dropped: &mut usize) {
        for b in content.iter_mut().rev() {
            match b {
                Block::Image { .. } => {
                    *seen += 1;
                    if *seen > KEEP_IMAGES {
                        *b = Block::Text(IMAGE_GONE.into());
                        *dropped += 1;
                    }
                }
                Block::ToolResult { content, .. } => visit(content, seen, dropped),
                _ => {}
            }
        }
    }
    let (mut seen, mut dropped) = (0, 0);
    for m in history.iter_mut().rev().filter(|m| m.role == Role::User) {
        visit(&mut m.content, &mut seen, &mut dropped);
    }
    dropped
}

/// The lines that say how far a number can be trusted. They outlive everything around them.
fn evidence_lines(text: &str) -> Vec<String> {
    text.lines().filter(|l| l.contains("\"evidence\"")).map(|l| l.trim().trim_end_matches(',').to_string()).collect()
}

/// Shortens old tool results, keeping the start, the end and any evidence labels.
fn shorten_old_results(history: &mut [Message]) -> usize {
    let mut shortened = 0;
    let keep_from = history.len().saturating_sub(KEEP_RESULTS);
    for m in history[..keep_from].iter_mut().filter(|m| m.role == Role::User) {
        for b in &mut m.content {
            let Block::ToolResult { content, .. } = b else { continue };
            for part in content {
                let Block::Text(t) = part else { continue };
                if t.len() <= LONG_RESULT {
                    continue;
                }
                let chars: Vec<char> = t.chars().collect();
                if chars.len() <= LONG_RESULT {
                    continue;
                }
                let head: String = chars[..800].iter().collect();
                let tail: String = chars[chars.len() - 400..].iter().collect();
                // Only the labels the cut removes: a repeat would look like another result.
                let kept: Vec<String> = evidence_lines(&chars[800..chars.len() - 400].iter().collect::<String>());
                let mut short = format!("{head}\n[… shortened to save space; run the tool again for all of it …]\n{tail}");
                if !kept.is_empty() {
                    short.push_str(&format!("\n[labels from the part removed: {}]", kept.join("; ")));
                }
                *t = short;
                shortened += 1;
            }
        }
    }
    shortened
}

/// Where to cut so that about `keep` tokens remain: the index of an assistant message, so what
/// stays still pairs up. At least two assistant messages stay. `None` if there's nothing to remove.
fn find_cut(history: &[Message], keep: u64) -> Option<usize> {
    let mut kept = 0;
    let mut assistants = 0;
    let mut cut = None;
    for (i, m) in history.iter().enumerate().rev() {
        kept += m.content.iter().map(block_tokens).sum::<u64>();
        if m.role == Role::Assistant {
            assistants += 1;
            if i > 0 {
                cut = Some(i);
            }
            if assistants >= 2 && kept >= keep {
                break;
            }
        }
    }
    // Cutting right after the first message would save nothing.
    cut.filter(|c| *c > 1)
}

/// The old messages as text, for the model that writes the notes.
pub fn as_text(history: &[Message]) -> String {
    let mut out = String::new();
    for m in history {
        out.push_str(if m.role == Role::User { "\nUSER:\n" } else { "\nASSISTANT:\n" });
        for b in &m.content {
            match b {
                Block::Text(t) => out.push_str(t),
                Block::Image { .. } => out.push_str("[image]"),
                Block::ToolUse { name, input, .. } => out.push_str(&format!("[called {name} with {input}]")),
                Block::ToolResult { content, is_error, .. } => {
                    out.push_str(if *is_error { "[tool failed: " } else { "[tool result: " });
                    let text: String = content.iter().map(|c| if let Block::Text(t) = c { t.as_str() } else { "[image]" }).collect::<Vec<_>>().join(" ");
                    let clipped: String = text.chars().take(1500).collect();
                    out.push_str(&clipped);
                    if clipped.len() < text.len() {
                        for line in evidence_lines(&text) {
                            out.push_str(&format!(" {line}"));
                        }
                    }
                    out.push(']');
                }
            }
            out.push('\n');
        }
    }
    out
}

/// Replaces everything before `cut` with one user message: the first message's own words and
/// images, the notes, the latest plan, and every evidence label, word for word. Labels and the
/// plan don't pass through a model, so none can reword them.
pub fn replace_with_summary(history: &mut Vec<Message>, cut: usize, notes: Option<&str>) {
    let removed: Vec<Message> = history.drain(..cut).collect();
    let mut labels = BTreeSet::new();
    let mut plan = None;
    fn collect(content: &[Block], labels: &mut BTreeSet<String>, plan: &mut Option<String>) {
        for b in content {
            match b {
                Block::Text(t) => labels.extend(evidence_lines(t)),
                Block::ToolResult { content, .. } => collect(content, labels, plan),
                Block::ToolUse { name, input, .. } if name == "update_plan" => *plan = Some(input.to_string()),
                _ => {}
            }
        }
    }
    for m in &removed {
        collect(&m.content, &mut labels, &mut plan);
    }
    let mut text = String::from("[Earlier in this conversation, shortened by the app to save space.]\n");
    let mut content = Vec::new();
    if let Some(first) = removed.first() {
        text.push_str("The request that started it:\n");
        for b in &first.content {
            match b {
                Block::Text(t) => {
                    text.push_str(t);
                    text.push('\n');
                }
                // A photo being rebuilt is still the point of the work.
                Block::Image { .. } => content.push(b.clone()),
                _ => {}
            }
        }
    }
    text.push_str("\nNotes on the work so far:\n");
    text.push_str(notes.unwrap_or("(No notes could be made. Read the scene and the results again before relying on earlier work.)"));
    if let Some(p) = plan {
        text.push_str(&format!("\n\nYour plan as last updated: {p}"));
    }
    if !labels.is_empty() {
        text.push_str(&format!("\n\nEvidence labels of earlier results, word for word:\n{}", labels.into_iter().collect::<Vec<_>>().join("\n")));
    }
    content.insert(0, Block::Text(text));
    history.insert(0, Message::user(content));
}

/// What compaction should do now, given how much of the context is in use.
#[derive(Debug, PartialEq, Eq)]
pub enum Needed {
    Nothing,
    /// Images and old results were enough; this many went.
    Trimmed { images: usize, results: usize },
    /// It needs notes from a model, and should cut here.
    Summarize { cut: usize, images: usize, results: usize },
}

/// Does the parts that need no model. `used` is the provider's own count if there is one.
pub fn trim(history: &mut [Message], used: u64, limit: u64) -> Needed {
    if (used as f64) < limit as f64 * START_AT {
        return Needed::Nothing;
    }
    let aim = (limit as f64 * AIM_FOR) as u64;
    let before = estimate(history);
    // What isn't the conversation: the system prompt and the tool list. No cut can shrink those.
    let overhead = used.saturating_sub(before);
    let images = drop_old_images(history);
    let results = shorten_old_results(history);
    let saved = before.saturating_sub(estimate(history));
    if used.saturating_sub(saved) <= aim {
        return Needed::Trimmed { images, results };
    }
    let keep = aim.saturating_sub(overhead) / 2;
    // A summary costs a model call. If it wouldn't free much, it would only run again next step.
    match find_cut(history, keep).filter(|cut| estimate(&history[..*cut]) >= limit / 10) {
        Some(cut) => Needed::Summarize { cut, images, results },
        None => Needed::Trimmed { images, results },
    }
}

/// The same conversation with no tool calls or provider data, for when the model changes: another
/// provider can't replay the old one's reasoning, and some reject a history without their own.
pub fn flatten(history: &[Message]) -> Vec<Message> {
    let text_of = |content: &[Block]| -> String { content.iter().map(|c| if let Block::Text(t) = c { t.as_str() } else { "[image]" }).collect::<Vec<_>>().join(" ") };
    history
        .iter()
        .map(|m| {
            let content = m
                .content
                .iter()
                .map(|b| match b {
                    Block::Text(_) | Block::Image { .. } => b.clone(),
                    Block::ToolUse { name, input, .. } => Block::Text(format!("[called {name} with {input}]")),
                    Block::ToolResult { content, is_error, .. } => Block::Text(format!("[{}: {}]", if *is_error { "tool failed" } else { "tool result" }, text_of(content))),
                })
                .collect();
            Message { role: m.role, content, raw: None, usage: None }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    fn user(t: &str) -> Message {
        Message::user(vec![Block::Text(t.into())])
    }
    fn says(t: &str) -> Message {
        Message { role: Role::Assistant, content: vec![Block::Text(t.into())], raw: None, usage: None }
    }
    fn calls(id: &str) -> Message {
        Message { role: Role::Assistant, content: vec![Block::ToolUse { id: id.into(), name: "get_scene".into(), input: json!({}) }], raw: Some((crate::agent::config::ProviderKind::Anthropic, json!([{ "type": "thinking", "signature": "s" }]))), usage: None }
    }
    fn result(id: &str, text: &str) -> Message {
        Message::user(vec![Block::ToolResult { id: id.into(), content: vec![Block::Text(text.into())], is_error: false }])
    }
    fn pic() -> Block {
        Block::Image { data: Arc::new(vec![1, 2, 3]), mime: "image/png".into() }
    }

    /// A long working conversation: tool rounds with big results.
    fn long_chat(rounds: usize, size: usize) -> Vec<Message> {
        let mut h = vec![Message::user(vec![Block::Text("build the warehouse".into()), pic()])];
        for i in 0..rounds {
            let id = format!("t{i}");
            h.push(calls(&id));
            h.push(result(&id, &"x".repeat(size)));
        }
        h
    }

    #[test]
    fn a_good_history_passes() {
        assert!(validate(&[]).is_ok());
        assert!(validate(&long_chat(5, 10)).is_ok());
        // A self-check's message after a plain reply, and a new request after that.
        assert!(validate(&[user("a"), says("b"), user("[Automatic check]"), says("c"), user("d")]).is_ok());
        // Fallback ids repeat from one turn to the next, which is fine: they pair by position.
        assert!(validate(&[user("a"), calls("call_0"), result("call_0", "r"), calls("call_0"), result("call_0", "r")]).is_ok());
    }

    #[test]
    fn a_broken_history_is_caught() {
        let cases: Vec<(&str, Vec<Message>)> = vec![
            ("starts with the assistant", vec![says("a")]),
            ("a call with no result", vec![user("a"), calls("t1"), user("b")]),
            ("a call at the very end", vec![user("a"), calls("t1")]),
            ("the wrong result", vec![user("a"), calls("t1"), result("t2", "r")]),
            ("a result with no call", vec![user("a"), says("b"), result("t1", "r")]),
            ("a result first", vec![result("t1", "r")]),
        ];
        for (name, h) in cases {
            assert!(validate(&h).is_err(), "{name}");
        }
    }

    #[test]
    fn nothing_happens_below_the_threshold() {
        let mut h = long_chat(3, 100);
        let before = estimate(&h);
        assert_eq!(trim(&mut h, 100, 1000), Needed::Nothing);
        assert_eq!(estimate(&h), before);
    }

    #[test]
    fn only_old_images_go_and_the_newest_stay() {
        let mut h = vec![user("start")];
        for i in 0..8 {
            h.push(Message::user(vec![pic(), Block::Text(format!("shot {i}"))]));
        }
        assert_eq!(drop_old_images(&mut h), 4);
        let count = |m: &Message| m.content.iter().filter(|b| matches!(b, Block::Image { .. })).count();
        assert_eq!(h.iter().map(count).sum::<usize>(), KEEP_IMAGES);
        assert_eq!(count(h.last().unwrap()), 1, "the latest is what the model is working from");
        assert!(matches!(&h[1].content[0], Block::Text(t) if t == IMAGE_GONE));
    }

    #[test]
    fn images_inside_tool_results_count_too() {
        let mut h = vec![user("start")];
        for i in 0..6 {
            let id = format!("t{i}");
            h.push(calls(&id));
            h.push(Message::user(vec![Block::ToolResult { id, content: vec![pic()], is_error: false }]));
        }
        assert_eq!(drop_old_images(&mut h), 2);
        assert!(validate(&h).is_ok());
    }

    #[test]
    fn shortening_keeps_the_ends_and_every_evidence_label() {
        let big = format!("{{\n\"start\": true,\n{}\n\"evidence\": \"Simulated: from the solver\",\n{}\n\"end\": true\n}}", "\"a\": 1,\n".repeat(400), "\"b\": 2,\n".repeat(400));
        let mut h = long_chat(1, 1);
        h[2] = result("t0", &big);
        for i in 1..6 {
            h.push(calls(&format!("t{i}")));
            h.push(result(&format!("t{i}"), "recent"));
        }
        assert_eq!(shorten_old_results(&mut h), 1);
        let Block::ToolResult { content, .. } = &h[2].content[0] else { panic!() };
        let Block::Text(t) = &content[0] else { panic!() };
        assert!(t.len() < big.len() / 2, "{}", t.len());
        assert!(t.contains("\"start\"") && t.contains("\"end\""));
        assert!(t.contains("Simulated: from the solver"), "the label in the removed part survives");
        assert!(validate(&h).is_ok());
    }

    #[test]
    fn recent_results_stay_whole() {
        let mut h = long_chat(2, 5000);
        assert_eq!(shorten_old_results(&mut h), 0);
    }

    #[test]
    fn shortening_doesnt_break_on_text_that_isnt_ascii() {
        let mut h = long_chat(10, 1);
        h[2] = result("t0", &"é°µ".repeat(3000));
        assert_eq!(shorten_old_results(&mut h), 1);
    }

    #[test]
    fn the_cut_always_leaves_a_valid_history() {
        for rounds in [3, 5, 20, 80] {
            let mut h = long_chat(rounds, 1000);
            let cut = find_cut(&h, 500).expect("something to remove");
            assert_eq!(h[cut].role, Role::Assistant);
            replace_with_summary(&mut h, cut, Some("notes"));
            assert!(validate(&h).is_ok(), "{rounds} rounds");
            assert_eq!(h[0].role, Role::User);
            assert!(h.len() >= 3, "the latest rounds stay");
        }
    }

    #[test]
    fn nothing_to_cut_means_no_cut() {
        assert!(find_cut(&[user("a")], 1).is_none());
        assert!(find_cut(&[user("a"), says("b")], 1).is_none());
    }

    #[test]
    fn a_conversation_thats_small_beside_the_tool_list_isnt_summarized_over_and_over() {
        // The context is nearly full, but nearly all of it is the system prompt and tool list.
        let mut h = long_chat(8, 200);
        assert!(matches!(trim(&mut h, 190_000, 200_000), Needed::Trimmed { .. }));
    }

    #[test]
    fn what_stays_leaves_room_for_the_tool_list() {
        // 150k of conversation and 40k of tool list, in 200k: more than half has to go.
        let mut h = long_chat(200, 3000);
        let Needed::Summarize { cut, .. } = trim(&mut h, 190_000, 200_000) else { panic!("should need a summary") };
        let left = estimate(&h[cut..]);
        assert!(left + 40_000 <= 100_000, "{left} left");
        assert!(left > 0);
    }

    #[test]
    fn the_assistants_messages_and_reasoning_arent_touched() {
        let mut h = long_chat(200, 3000);
        let originals: Vec<(String, String)> = h.iter().filter(|m| m.role == Role::Assistant).map(|m| (format!("{:?}", m.content), format!("{:?}", m.raw))).collect();
        let Needed::Summarize { cut, .. } = trim(&mut h, 190_000, 200_000) else { panic!("should need a summary") };
        replace_with_summary(&mut h, cut, Some("notes"));
        let left: Vec<(String, String)> = h.iter().filter(|m| m.role == Role::Assistant).map(|m| (format!("{:?}", m.content), format!("{:?}", m.raw))).collect();
        assert!(!left.is_empty());
        assert!(left.iter().all(|m| originals.contains(m)), "an assistant message changed");
        assert!(validate(&h).is_ok());
    }

    #[test]
    fn the_summary_keeps_the_request_the_photo_the_plan_and_the_labels() {
        let mut h = long_chat(1, 1);
        h.push(Message { role: Role::Assistant, content: vec![Block::ToolUse { id: "p".into(), name: "update_plan".into(), input: json!({ "steps": [{ "step": "finish it" }] }) }], raw: None, usage: None });
        h.push(result("p", "{\n\"evidence\": \"Measured, as entered\"\n}"));
        h.push(calls("z"));
        h.push(result("z", "latest"));
        let cut = h.len() - 2;
        replace_with_summary(&mut h, cut, Some("the model's notes"));
        assert!(matches!(&h[0].content[1], Block::Image { .. }), "the photo is still the point of the work");
        let Block::Text(t) = &h[0].content[0] else { panic!() };
        for want in ["build the warehouse", "the model's notes", "finish it", "\"evidence\": \"Measured, as entered\""] {
            assert!(t.contains(want), "{want} in {t}");
        }
        assert_eq!(h.len(), 3);
        assert!(validate(&h).is_ok());
    }

    #[test]
    fn a_failed_summary_says_so_rather_than_pretending() {
        let mut h = long_chat(3, 10);
        let cut = h.len() - 2;
        replace_with_summary(&mut h, cut, None);
        let Block::Text(t) = &h[0].content[0] else { panic!() };
        assert!(t.contains("No notes could be made"), "{t}");
    }

    #[test]
    fn labels_survive_being_compacted_twice() {
        let mut h = long_chat(1, 1);
        h[2] = result("t0", "{\n\"evidence\": \"Calibrated: site walk\"\n}");
        for i in 1..8 {
            h.push(calls(&format!("t{i}")));
            h.push(result(&format!("t{i}"), "r"));
        }
        let cut = h.len() - 6;
        replace_with_summary(&mut h, cut, Some("first notes"));
        for i in 8..16 {
            h.push(calls(&format!("t{i}")));
            h.push(result(&format!("t{i}"), "r"));
        }
        let cut = h.len() - 4;
        replace_with_summary(&mut h, cut, Some("second notes"));
        let Block::Text(t) = &h[0].content[0] else { panic!() };
        assert!(t.contains("Calibrated: site walk"), "{t}");
        assert!(t.contains("second notes") && t.contains("build the warehouse"));
        assert!(validate(&h).is_ok());
    }

    #[test]
    fn flattening_leaves_no_tool_calls_or_reasoning_and_keeps_the_words() {
        let flat = flatten(&long_chat(3, 10));
        assert!(validate(&flat).is_ok());
        assert!(flat.iter().all(|m| m.raw.is_none()));
        assert!(!flat.iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolUse { .. } | Block::ToolResult { .. })));
        assert!(matches!(&flat[0].content[1], Block::Image { .. }));
        assert!(matches!(&flat[1].content[0], Block::Text(t) if t.contains("get_scene")));
    }
}
