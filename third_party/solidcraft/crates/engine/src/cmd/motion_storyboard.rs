//! Storyboards: motion studies and exploded views in the order they are shown, for
//! `motion.export_animation` to draw one after another. See `solidcraft_doc::motion` for where
//! they are kept.

use serde_json::{Value, json};
use solidcraft_doc::metadata::{Owner, check_text};
use solidcraft_doc::motion::{STORYBOARDS_KEY, Storyboard, StoryboardClip, storyboards};

use super::{study_index, view_index};
use crate::params::{bad, str_};
use crate::{Result, Session};

/// Most storyboards in a design, and clips in one.
const MAX_BOARDS: usize = 64;
const MAX_CLIPS: usize = 64;

pub(super) fn has_something_to_play(s: &Session) -> std::result::Result<(), String> {
    let a = &s.doc.assembly;
    if a.studies.is_empty() && a.exploded.is_empty() {
        Err("A storyboard plays motion studies and exploded views, so the design needs one of them first.".into())
    } else {
        Ok(())
    }
}

fn store(s: &mut Session, boards: &[Storyboard], cmd: &str) -> Result<()> {
    let value = if boards.is_empty() { Value::Null } else { json!({ "boards": boards }) };
    let mut next = s.doc.metadata.clone();
    next.set(Owner::Design, STORYBOARDS_KEY, value).map_err(|e| bad(cmd, e.to_string()))?;
    s.doc_mut().metadata = next;
    Ok(())
}

/// The clip `c` names, by the stable id of what it plays.
fn clip(s: &Session, c: &Value, cmd: &str) -> Result<StoryboardClip> {
    let a = &s.doc.assembly;
    match (c.get("study"), c.get("view")) {
        (Some(_), None) => {
            let study = a.studies.get(study_index(s, c, cmd)?).ok_or_else(|| bad(cmd, "study"))?.id;
            Ok(StoryboardClip::Study { study })
        }
        (None, Some(_)) => {
            let view = a.exploded.get(view_index(s, c, cmd)?).ok_or_else(|| bad(cmd, "view"))?.id;
            let back = match c.get("back") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(bad(cmd, "`back` must be true or false")),
            };
            Ok(StoryboardClip::View { view, back })
        }
        _ => Err(bad(cmd, "each clip is {study} or {view, back?}")),
    }
}

pub(super) fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "storyboard.create";
    let given = p.get("clips").and_then(Value::as_array).ok_or_else(|| bad(cmd, "`clips` must be a list of {study} or {view, back?}"))?;
    if given.is_empty() || given.len() > MAX_CLIPS {
        return Err(bad(cmd, format!("a storyboard has 1…{MAX_CLIPS} clips")));
    }
    let clips = given.iter().map(|c| clip(s, c, cmd)).collect::<Result<Vec<_>>>()?;
    let mut boards = storyboards(&s.doc.metadata);
    let editing = match str_(p, "storyboard") {
        Some(name) => Some(boards.iter().position(|b| b.name == name).ok_or_else(|| bad(cmd, format!("no storyboard `{name}`")))?),
        None => None,
    };
    let name = match str_(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None => match editing.and_then(|i| boards.get(i)) {
            Some(b) => b.name.clone(),
            None => (1..).map(|k| format!("Storyboard{k}")).find(|n| !boards.iter().any(|b| b.name == *n)).unwrap_or_default(),
        },
    };
    check_text("the name", &name).map_err(|e| bad(cmd, e.to_string()))?;
    if boards.iter().enumerate().any(|(i, b)| b.name == name && Some(i) != editing) {
        return Err(bad(cmd, format!("there is already a storyboard `{name}`")));
    }
    let count = clips.len();
    if editing.is_none() && boards.len() >= MAX_BOARDS {
        return Err(bad(cmd, format!("a design has at most {MAX_BOARDS} storyboards")));
    }
    let board = Storyboard { name: name.clone(), clips };
    match editing.and_then(|i| boards.get_mut(i)) {
        Some(b) => *b = board,
        None => boards.push(board),
    }
    store(s, &boards, cmd)?;
    Ok(json!({"storyboard": name, "clips": count}))
}

pub(super) fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "storyboard.delete";
    let name = str_(p, "storyboard").ok_or_else(|| bad(cmd, "`storyboard` must be a name"))?;
    let mut boards = storyboards(&s.doc.metadata);
    let before = boards.len();
    boards.retain(|b| b.name != name);
    if boards.len() == before {
        return Err(bad(cmd, format!("no storyboard `{name}`")));
    }
    store(s, &boards, cmd)?;
    Ok(json!({"deleted": name}))
}

pub(super) fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let a = &s.doc.assembly;
    let row = |c: &StoryboardClip| match c {
        StoryboardClip::Study { study } => {
            let name = a.studies.iter().find(|m| m.id == *study).map(|m| m.name.as_str());
            json!({"study": study, "name": name, "missing": name.is_none()})
        }
        StoryboardClip::View { view, back } => {
            let name = a.exploded.iter().find(|v| v.id == *view).map(|v| v.name.as_str());
            json!({"view": view, "back": back, "name": name, "missing": name.is_none()})
        }
    };
    let boards = storyboards(&s.doc.metadata);
    let rows: Vec<Value> = boards.iter().map(|b| json!({"name": b.name, "clips": b.clips.iter().map(row).collect::<Vec<_>>()})).collect();
    Ok(json!({"storyboards": rows}))
}

/// The parameters of each clip of the storyboard `p` names, as the exporter takes one clip's.
pub(super) fn clips(s: &Session, p: &Value, cmd: &str) -> Result<Vec<Value>> {
    let name = str_(p, "storyboard").ok_or_else(|| bad(cmd, "`storyboard` must be a name"))?;
    let boards = storyboards(&s.doc.metadata);
    let board = boards.iter().find(|b| b.name == name).ok_or_else(|| bad(cmd, format!("no storyboard `{name}`")))?;
    if board.clips.is_empty() {
        return Err(bad(cmd, format!("storyboard `{name}` has no clips")));
    }
    Ok(board.clips.iter().map(|c| json!(c)).collect())
}
