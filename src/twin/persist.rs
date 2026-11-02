//! Device positions live in the layout file under a `twin` key, next to the USD objects.
//! Only an ID and a position are stored, so a shared layout carries no addresses or serials.
//! The scene's own save and load code is untouched: we watch its actions and add our part.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bevy::prelude::*;
use serde_json::{Value, json};
use twin_proto::model::DeviceId;

use super::{Placement, TwinScene, TwinSelection};
use crate::scene::{Action, Actions, Status};

#[derive(Resource, Default)]
pub struct LayoutHook {
    save: Vec<PathBuf>,
    load: Vec<PathBuf>,
    clear: bool,
}

pub fn peek_actions(actions: Res<Actions>, mut hook: ResMut<LayoutHook>) {
    for action in &actions.0 {
        match action {
            Action::SaveLayout(path) => hook.save.push(path.clone()),
            Action::LoadLayout(path) => hook.load.push(path.clone()),
            Action::ClearScene => hook.clear = true,
            _ => {}
        }
    }
}

fn to_json(placed: &BTreeMap<DeviceId, Placement>) -> Value {
    let placements: Vec<Value> = placed.iter().map(|(id, place)| json!({ "device": id.0, "position": place.position })).collect();
    json!({ "placements": placements })
}

/// A layout from before the twin has no key and simply yields nothing. A malformed entry is
/// skipped rather than failing the whole layout.
fn from_json(twin: &Value) -> BTreeMap<DeviceId, Placement> {
    let entries = twin["placements"].as_array().into_iter().flatten();
    entries
        .filter_map(|entry| {
            let device = entry["device"].as_str()?;
            let position: [f32; 3] = serde_json::from_value(entry["position"].clone()).ok()?;
            position.iter().all(|v| v.is_finite()).then(|| (DeviceId(device.to_string()), Placement { position }))
        })
        .collect()
}

/// `Status` is taken mutably on purpose. The RTLS and plant hooks rewrite the same file and
/// take it too, which makes Bevy run these read-modify-writes one after another.
pub fn after_actions(mut hook: ResMut<LayoutHook>, mut scene: ResMut<TwinScene>, mut selection: ResMut<TwinSelection>, mut status: ResMut<Status>) {
    for path in std::mem::take(&mut hook.save) {
        if scene.placed.is_empty() {
            continue;
        }
        let saved = (|| -> anyhow::Result<()> {
            let mut layout: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            layout["twin"] = to_json(&scene.placed);
            std::fs::write(&path, serde_json::to_string_pretty(&layout)?)?;
            Ok(())
        })();
        if let Err(e) = saved {
            status.0 = format!("{} (device positions not saved: {e:#})", status.0);
        }
    }
    let loads = std::mem::take(&mut hook.load);
    if std::mem::take(&mut hook.clear) || !loads.is_empty() {
        scene.placed.clear();
        selection.device = None;
        selection.hovered = None;
    }
    for path in loads {
        let loaded = (|| -> anyhow::Result<BTreeMap<DeviceId, Placement>> {
            let layout: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            Ok(from_json(&layout["twin"]))
        })();
        match loaded {
            Ok(placed) => scene.placed = placed,
            Err(e) => status.0 = format!("{} (device positions: {e:#})", status.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip() {
        let mut placed = BTreeMap::new();
        placed.insert(DeviceId("dev_1".into()), Placement { position: [1.5, 0.0, -2.0] });
        placed.insert(DeviceId("dev_2".into()), Placement { position: [3.0, 1.0, 4.0] });
        assert_eq!(from_json(&to_json(&placed)), placed);
    }

    #[test]
    fn a_layout_from_before_the_twin_has_no_positions() {
        let layout = json!({ "format": "sense_sim_layout", "version": 3, "objects": [] });
        assert!(from_json(&layout["twin"]).is_empty());
    }

    #[test]
    fn a_bad_entry_is_skipped_and_the_rest_survive() {
        let twin = json!({ "placements": [
            { "device": "good", "position": [1.0, 0.0, 2.0] },
            { "device": "no position" },
            { "position": [1.0, 0.0, 2.0] },
            { "device": "too short", "position": [1.0] },
            "not an object",
        ] });
        let placed = from_json(&twin);
        assert_eq!(placed.len(), 1);
        assert!(placed.contains_key(&DeviceId("good".into())));
    }

    #[test]
    fn saved_positions_hold_no_addresses_or_serials() {
        let mut placed = BTreeMap::new();
        placed.insert(DeviceId("dev_1".into()), Placement { position: [0.0, 0.0, 0.0] });
        let text = to_json(&placed).to_string();
        assert!(!text.contains("ip") && !text.contains("serial") && !text.contains("mac"), "{text}");
    }
}
