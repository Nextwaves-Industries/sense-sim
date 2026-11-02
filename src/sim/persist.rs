//! Studies live in the layout file under a `simulation` key, added after the scene writes it (the
//! same hook as RTLS and the plant), and the design beside it as a `.sc` file of the same name.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use bevy::prelude::*;
use serde_json::{Value, json};
use sim_core::study::Study;

use super::SimState;
use crate::design::{Slot, bridge};
use crate::scene::{Action, Actions, Status};

#[derive(Resource, Default)]
pub struct Hook {
    save: Vec<PathBuf>,
    load: Vec<PathBuf>,
    clear: bool,
}

pub fn peek_actions(actions: Res<Actions>, mut hook: ResMut<Hook>) {
    for a in &actions.0 {
        match a {
            Action::SaveLayout(p) => hook.save.push(p.clone()),
            Action::LoadLayout(p) => hook.load.push(p.clone()),
            Action::ClearScene => hook.clear = true,
            _ => {}
        }
    }
}

/// The design's file beside a layout.
pub fn design_path(layout: &Path) -> PathBuf {
    layout.with_extension("sc")
}

/// The section for the layout file: its studies, and the design's file name if `saved_design`.
pub fn section(studies: &[Study], saved_design: Option<&str>) -> Value {
    let mut v = json!({ "studies": studies });
    if let Some(name) = saved_design {
        v["design_file"] = json!(name);
    }
    v
}

/// The studies in a section. One that can't be read (from a newer version, say) is skipped and counted,
/// so that it can be reported, not silently lost.
pub fn parse_section(v: &Value) -> (Vec<Study>, usize) {
    let mut skipped = 0;
    let mut studies = Vec::new();
    for s in v["studies"].as_array().into_iter().flatten() {
        match serde_json::from_value::<Study>(s.clone()) {
            Ok(study) => studies.push(study),
            Err(_) => skipped += 1,
        }
    }
    (studies, skipped)
}

fn save(path: &Path, state: &SimState, slot: &Slot) -> Result<Option<usize>> {
    let mut design_name = None;
    let mut body_count = None;
    if let Some(host) = &slot.host {
        let session = &host.app.session;
        let bodies = bridge::bodies(session).len();
        if bodies > 0 {
            let bytes = solidcraft_engine::io::write_design_container(&session.design()).map_err(|e| anyhow!("writing the design: {e}"))?;
            let file = design_path(path);
            std::fs::write(&file, bytes).map_err(|e| anyhow!("writing {}: {e}", file.display()))?;
            design_name = file.file_name().map(|n| n.to_string_lossy().to_string());
            body_count = Some(bodies);
        }
    }
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    v["simulation"] = section(&state.studies, design_name.as_deref());
    std::fs::write(path, serde_json::to_string_pretty(&v)?)?;
    Ok(body_count)
}

/// Reads a layout's section. A design with unsaved work open is left alone, and said so.
fn load(path: &Path, state: &mut SimState, slot: &mut Slot) -> Result<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let section = &v["simulation"];
    let (studies, skipped) = parse_section(section);
    let count = studies.len();
    state.studies = studies;
    state.active = (count > 0).then_some(0);
    state.view = None;
    state.picking = None;
    state.scene_pick = None;
    let mut said = String::new();
    if count > 0 {
        said = format!(" + {count} simulation studies");
    }
    if skipped > 0 {
        said.push_str(&format!(" ({skipped} studies couldn't be read)"));
    }
    if let Some(name) = section["design_file"].as_str() {
        let file = path.parent().map_or_else(|| PathBuf::from(name), |dir| dir.join(name));
        if !file.is_file() {
            said.push_str(&format!(" (its design, {name}, is missing)"));
        } else {
            let text = file.to_string_lossy().to_string();
            match slot.host.as_mut() {
                // Not over work that hasn't been saved.
                Some(h) if h.app.session.is_dirty() => said.push_str(&format!(" (the open design has unsaved changes, so {name} wasn't opened)")),
                Some(h) => {
                    h.app.open_path(&text);
                    said.push_str(" + its design");
                }
                None => {
                    slot.open_when_started = Some(file);
                    said.push_str(" + its design");
                }
            }
        }
    }
    Ok(said)
}

pub fn after_actions(mut hook: ResMut<Hook>, mut state: ResMut<SimState>, mut slot: NonSendMut<Slot>, mut status: ResMut<Status>) {
    for path in std::mem::take(&mut hook.save) {
        let safety_save = path == crate::design::scene_safety_path();
        match save(&path, &state, &slot) {
            // The save on the way into Design isn't news. If it failed, that still is.
            Ok(_) if safety_save && status.0.starts_with("Saved ") => status.0.clear(),
            Ok(Some(bodies)) => status.0 = format!("{} + {bodies} design bodies", status.0),
            Ok(None) => {}
            Err(e) => status.0 = format!("{} (simulation not saved: {e:#})", status.0),
        }
    }
    let loads = std::mem::take(&mut hook.load);
    if std::mem::take(&mut hook.clear) || !loads.is_empty() {
        state.studies.clear();
        state.active = None;
        state.view = None;
        state.picking = None;
        state.scene_pick = None;
    }
    for path in loads {
        match load(&path, &mut state, &mut slot) {
            Ok(said) => status.0 = format!("{}{said}", status.0),
            Err(e) => status.0 = format!("{} (simulation: {e:#})", status.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::catalog::AnalysisId;
    use sim_core::study::{BcKind, Condition, FaceRef};

    #[test]
    fn the_section_survives_the_file() {
        let mut s = Study::new("Beam".into(), AnalysisId::Harmonic);
        s.body = Some("scene:Beam_1".into());
        s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![FaceRef { face: 3, point: [1.0, 2.0, 3.0], second: false }] });
        let v = section(std::slice::from_ref(&s), Some("plant.sc"));
        let text = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        let (studies, skipped) = parse_section(&back);
        assert_eq!(studies, [s]);
        assert_eq!(skipped, 0);
        assert_eq!(back["design_file"], "plant.sc");
    }

    #[test]
    fn a_layout_without_a_section_has_no_studies() {
        let (studies, skipped) = parse_section(&Value::Null);
        assert!(studies.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn a_study_that_cant_be_read_is_counted_not_lost_in_silence() {
        let good = serde_json::to_value(Study::new("ok".into(), AnalysisId::Static)).unwrap();
        let v = json!({ "studies": [good, { "name": "from the future", "analysis": "warp-drive" }] });
        let (studies, skipped) = parse_section(&v);
        assert_eq!(studies.len(), 1);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn the_design_goes_beside_the_layout() {
        assert_eq!(design_path(Path::new("C:/x/plant.json")), PathBuf::from("C:/x/plant.sc"));
    }
}
