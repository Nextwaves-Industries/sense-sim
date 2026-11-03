//! Studies live in the layout file under a `simulation` key, added after the scene writes it (the
//! same hook as RTLS and the plant).

use std::path::{Path, PathBuf};

use anyhow::Result;
use bevy::prelude::*;
use serde_json::{Value, json};
use sim_core::study::Study;

use super::SimState;
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

/// The section for the layout file: its studies, and the name of a design file an older version saved
/// beside it, so that the file can still be found.
pub fn section(studies: &[Study], older_design: Option<&str>) -> Value {
    let mut v = json!({ "studies": studies });
    if let Some(name) = older_design {
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

fn save(path: &Path, state: &SimState) -> Result<()> {
    let mut v = crate::project_file::read(path)?;
    let older_design = v["simulation"]["design_file"].as_str().map(str::to_string);
    v["simulation"] = section(&state.studies, older_design.as_deref());
    crate::project_file::write(path, &v)
}

/// Reads a layout's section. A design file from an older version can't be opened any more: it stays
/// where it is, and that's said.
fn load(path: &Path, state: &mut SimState) -> Result<String> {
    let v = crate::project_file::read(path)?;
    let section = &v["simulation"];
    let (studies, skipped) = parse_section(section);
    let count = studies.len();
    state.studies = studies;
    state.active = (count > 0).then_some(0);
    state.view = None;
    state.scene_pick = None;
    let mut said = String::new();
    if count > 0 {
        said = format!(" + {count} simulation studies");
    }
    if skipped > 0 {
        said.push_str(&format!(" ({skipped} studies couldn't be read)"));
    }
    if let Some(name) = section["design_file"].as_str() {
        said.push_str(&format!(" (its 3D design, {name}, wasn't opened: this version has no 3D Design)"));
    }
    Ok(said)
}

pub fn after_actions(mut hook: ResMut<Hook>, mut state: ResMut<SimState>, mut status: ResMut<Status>) {
    for path in std::mem::take(&mut hook.save) {
        if let Err(e) = save(&path, &state) {
            status.0 = format!("{} (simulation not saved: {e:#})", status.0);
        }
    }
    let loads = std::mem::take(&mut hook.load);
    if std::mem::take(&mut hook.clear) || !loads.is_empty() {
        state.studies.clear();
        state.active = None;
        state.view = None;
        state.scene_pick = None;
    }
    for path in loads {
        match load(&path, &mut state) {
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

}
