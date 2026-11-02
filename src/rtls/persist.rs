//! Devices and zones live in the layout file next to the USD objects, under
//! an `rtls` key (older layouts simply have none). The scene's own
//! save/load code is untouched: we watch its actions and add our part.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde_json::{Value, json};

use super::device::{Device, DeviceDoc, Zone};
use super::{RtlsConfig, RtlsSelection, RtlsState};
use crate::scene::{Action, Actions, Status};

#[derive(Resource, Default)]
pub struct LayoutHook {
    save: Vec<PathBuf>,
    load: Vec<PathBuf>,
    clear: bool,
}

pub fn peek_actions(actions: Res<Actions>, mut hook: ResMut<LayoutHook>) {
    for a in &actions.0 {
        match a {
            Action::SaveLayout(p) => hook.save.push(p.clone()),
            Action::LoadLayout(p) => hook.load.push(p.clone()),
            Action::ClearScene => hook.clear = true,
            _ => {}
        }
    }
}

pub fn rtls_json(devices: impl Iterator<Item = (Device, Transform)>, zones: impl Iterator<Item = Zone>) -> Value {
    let devices: Vec<DeviceDoc> = devices
        .map(|(d, t)| DeviceDoc { device: d, translation: t.translation.to_array(), rotation: t.rotation.to_array() })
        .collect();
    let zones: Vec<Zone> = zones.collect();
    json!({ "devices": devices, "zones": zones, "object_materials": super::radio::object_materials_json() })
}

pub fn parse_rtls(v: &Value) -> (Vec<(Device, Transform)>, Vec<Zone>) {
    let devices = v["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| serde_json::from_value::<DeviceDoc>(d.clone()).ok())
        .map(|d| {
            let r = d.rotation;
            let t = Transform::from_translation(Vec3::from(d.translation)).with_rotation(Quat::from_xyzw(r[0], r[1], r[2], r[3]).normalize());
            (d.device, t)
        })
        .collect();
    let zones = v["zones"].as_array().into_iter().flatten().filter_map(|z| serde_json::from_value::<Zone>(z.clone()).ok()).collect();
    (devices, zones)
}

pub fn read_layout_rtls(path: &Path) -> anyhow::Result<(Vec<(Device, Transform)>, Vec<Zone>)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    super::radio::load_object_materials(&v["rtls"]["object_materials"]);
    Ok(parse_rtls(&v["rtls"]))
}

#[allow(clippy::too_many_arguments)]
pub fn after_actions(
    mut commands: Commands,
    mut hook: ResMut<LayoutHook>,
    devices: Query<(Entity, &Device, &Transform)>,
    zones: Query<(Entity, &Zone)>,
    mut status: ResMut<Status>,
    mut state: ResMut<RtlsState>,
    cfg: Res<RtlsConfig>,
    mut rsel: ResMut<RtlsSelection>,
) {
    for path in std::mem::take(&mut hook.save) {
        let extra = rtls_json(devices.iter().map(|(_, d, t)| (d.clone(), *t)), zones.iter().map(|(_, z)| z.clone()));
        let n = devices.iter().count();
        let res = (|| -> anyhow::Result<()> {
            let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            v["rtls"] = extra;
            v["version"] = json!(3);
            std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
            Ok(())
        })();
        match res {
            Ok(()) if n > 0 => status.0 = format!("{} + {n} devices", status.0),
            Ok(()) => {}
            Err(e) => status.0 = format!("{} (devices not saved: {e:#})", status.0),
        }
    }
    let mut clear = std::mem::take(&mut hook.clear);
    let loads = std::mem::take(&mut hook.load);
    if !loads.is_empty() {
        clear = true;
    }
    if clear {
        for (e, _, _) in &devices {
            commands.entity(e).despawn();
        }
        for (e, _) in &zones {
            commands.entity(e).despawn();
        }
        rsel.device = None;
        rsel.zone = None;
        state.restart(cfg.settings.seed);
    }
    for path in loads {
        match read_layout_rtls(&path) {
            Ok((ds, zs)) => {
                for (d, t) in ds {
                    super::view::spawn_device(&mut commands, d, t);
                }
                for z in zs {
                    commands.spawn((Name::new(format!("Zone {}", z.name)), z));
                }
            }
            Err(e) => status.0 = format!("{} (devices: {e:#})", status.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtls::device::{DeviceKind, Motion};

    #[test]
    fn devices_roundtrip_and_old_layouts_load() {
        let d = Device {
            id: "T1".into(),
            kind: DeviceKind::UwbTag,
            profile: "uwb-tag".into(),
            enabled: true,
            motion: Motion::Path { points: vec![[0.0, 1.0, 0.0], [3.0, 1.0, 0.0]], speed: 1.0, ping_pong: true },
            address: String::new(),
            auto: false,
            radio: crate::rtls::device::RadioOverride { uwb_tx_dbm_mhz: Some(-50.0), antenna: Some(crate::rtls::antenna::AntennaChoice::Omni { gain_dbi: 2.0 }), worn: false },
        };
        let z = Zone { name: "Dock".into(), min: [0.0; 3], max: [2.0, 2.0, 2.0], restricted: true, capacity: 0, auto: false };
        let v = rtls_json(std::iter::once((d, Transform::from_xyz(1.0, 1.2, 2.0))), std::iter::once(z));
        let (ds, zs) = parse_rtls(&v);
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].0.id, "T1");
        assert_eq!(ds[0].0.radio.uwb_tx_dbm_mhz, Some(-50.0));
        assert_eq!(ds[0].0.radio.antenna, Some(crate::rtls::antenna::AntennaChoice::Omni { gain_dbi: 2.0 }));
        assert!(matches!(ds[0].0.motion, Motion::Path { ping_pong: true, .. }));
        assert_eq!(ds[0].1.translation, Vec3::new(1.0, 1.2, 2.0));
        assert_eq!(zs[0].name, "Dock");
        // A v2 layout has no rtls key.
        let (ds, zs) = parse_rtls(&Value::Null);
        assert!(ds.is_empty() && zs.is_empty());
    }

    #[test]
    fn templates_parse_completely() {
        for (id, s) in &crate::samples::TEMPLATES {
            let v: Value = serde_json::from_str(s.layout.unwrap()).unwrap();
            let (ds, zs) = parse_rtls(&v["rtls"]);
            assert_eq!(ds.len(), v["rtls"]["devices"].as_array().map_or(0, |a| a.len()), "{id}: a device failed to parse");
            assert_eq!(zs.len(), v["rtls"]["zones"].as_array().map_or(0, |a| a.len()), "{id}: a zone failed to parse");
        }
    }
}
