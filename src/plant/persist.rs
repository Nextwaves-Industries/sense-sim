//! Links and plant settings live in the layout file under a `plant` key,
//! added after the scene writes it (the same hook pattern as RTLS). USDA
//! export gains a physics joint per link, so the system arrives in Isaac Sim
//! connected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde_json::{Value, json};

use super::connect::{Link, LinkKind, Links};
use super::sim::{Control, Plant};
use crate::scene::{Action, Actions, PendingSpawns, PlacedObject, Status};

#[derive(Resource, Default)]
pub struct Hook {
    save: Vec<PathBuf>,
    load: Vec<PathBuf>,
    export: Vec<PathBuf>,
    clear: bool,
}

pub fn peek_actions(actions: Res<Actions>, mut hook: ResMut<Hook>) {
    for a in &actions.0 {
        match a {
            Action::SaveLayout(p) => hook.save.push(p.clone()),
            Action::LoadLayout(p) => hook.load.push(p.clone()),
            Action::ExportUsda(p) => hook.export.push(p.clone()),
            Action::ClearScene => hook.clear = true,
            _ => {}
        }
    }
}

pub fn plant_json(links: &Links, plant: &Plant) -> Value {
    let levels: HashMap<&String, f32> = plant.live.volume.iter().map(|(k, v)| (k, *v)).collect();
    json!({
        "links": links.0,
        "controls": plant.controls,
        "running": plant.running,
        "volumes_m3": levels,
    })
}

pub fn parse_plant(v: &Value) -> (Vec<Link>, HashMap<String, Control>, HashMap<String, f32>) {
    let links = v["links"].as_array().into_iter().flatten().filter_map(|l| serde_json::from_value::<Link>(l.clone()).ok()).collect();
    let controls = serde_json::from_value(v["controls"].clone()).unwrap_or_default();
    let volumes = serde_json::from_value(v["volumes_m3"].clone()).unwrap_or_default();
    (links, controls, volumes)
}

#[allow(clippy::too_many_arguments)]
pub fn after_actions(
    mut hook: ResMut<Hook>,
    mut links: ResMut<Links>,
    mut plant: ResMut<Plant>,
    objects: Query<(Entity, &PlacedObject, &Transform)>,
    mut status: ResMut<Status>,
) {
    for path in std::mem::take(&mut hook.save) {
        let extra = plant_json(&links, &plant);
        let res = (|| -> anyhow::Result<()> {
            let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            v["plant"] = extra;
            std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
            Ok(())
        })();
        match res {
            Ok(()) if !links.0.is_empty() => status.0 = format!("{} + {} connections", status.0, links.0.len()),
            Ok(()) => {}
            Err(e) => status.0 = format!("{} (connections not saved: {e:#})", status.0),
        }
    }
    for path in std::mem::take(&mut hook.export) {
        let model = super::sim::Model::build(objects.iter());
        if let Err(e) = append_joints(&path, &model, &links) {
            status.0 = format!("{} (joints not exported: {e:#})", status.0);
        }
    }
    let mut clear = std::mem::take(&mut hook.clear);
    let loads = std::mem::take(&mut hook.load);
    if !loads.is_empty() {
        clear = true;
    }
    if clear {
        links.0.clear();
        plant.clear();
    }
    for path in loads {
        match read(&path) {
            Ok((l, c, v)) => {
                links.0 = l;
                plant.controls = c;
                plant.live.volume = v;
            }
            Err(e) => status.0 = format!("{} (connections: {e:#})", status.0),
        }
    }
}

fn read(path: &Path) -> anyhow::Result<(Vec<Link>, HashMap<String, Control>, HashMap<String, f32>)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(parse_plant(&v["plant"]))
}

/// Same prim names as `scene::export_usda`.
fn prim_name(name: &str) -> String {
    let mut n: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if n.is_empty() || n.chars().next().unwrap().is_ascii_digit() {
        n.insert(0, '_');
    }
    n
}

fn append_joints(path: &Path, model: &super::sim::Model, links: &Links) -> anyhow::Result<()> {
    use std::fmt::Write;
    if links.0.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let Some(end) = text.rfind('}') else { anyhow::bail!("unexpected USDA layout") };
    let mut s = String::new();
    for (i, l) in links.0.iter().enumerate() {
        let (Some(a), Some(b)) = (model.get(&l.a.object), model.get(&l.b.object)) else { continue };
        let Some((wa, _, _)) = model.port_world(&l.a) else { continue };
        // Joint frame: the shared port point, in each body's prim space.
        let local = |o: &super::sim::Obj| (o.t.to_matrix() * o.proto.basis.to_matrix()).inverse().transform_point3(wa);
        let (pa, pb) = (local(a), local(b));
        let kind = match l.kind {
            LinkKind::Belt | LinkKind::Conveyor => "custom",
            _ => "fixed",
        };
        if kind == "custom" {
            writeln!(s, "    def Scope \"plant_link_{i}\" (\n        customData = {{ string kind = \"{}\" string a = \"{}\" string b = \"{}\" }}\n    )\n    {{\n    }}\n", l.kind.name(), l.a.label(), l.b.label())?;
            continue;
        }
        writeln!(s, "    def PhysicsFixedJoint \"plant_link_{i}\" (\n        customData = {{ string kind = \"{}\" }}\n    )\n    {{", l.kind.name())?;
        writeln!(s, "        rel physics:body0 = </World/{}>", prim_name(&a.name))?;
        writeln!(s, "        rel physics:body1 = </World/{}>", prim_name(&b.name))?;
        writeln!(s, "        point3f physics:localPos0 = ({}, {}, {})", pa.x, pa.y, pa.z)?;
        writeln!(s, "        point3f physics:localPos1 = ({}, {}, {})\n    }}\n", pb.x, pb.y, pb.z)?;
    }
    let mut out = text[..end].to_string();
    out.push_str(&s);
    out.push_str(&text[end..]);
    std::fs::write(path, out)?;
    Ok(())
}

/// Keeps links and settings attached to objects through renames, and drops
/// the links of deleted objects.
#[allow(clippy::too_many_arguments)]
pub fn track_names(
    mut known: Local<HashMap<Entity, String>>,
    objects: Query<(Entity, &PlacedObject)>,
    mut removed: RemovedComponents<PlacedObject>,
    pending: Res<PendingSpawns>,
    mut links: ResMut<Links>,
    mut plant: ResMut<Plant>,
    parked: Query<(), With<crate::tabs::ParkedObject>>,
) {
    for (e, o) in &objects {
        match known.get(&e) {
            Some(old) if *old != o.name => {
                let old = old.clone();
                links.rename(&old, &o.name);
                plant.rename(&old, &o.name);
                known.insert(e, o.name.clone());
            }
            Some(_) => {}
            None => {
                known.insert(e, o.name.clone());
            }
        }
    }
    for e in removed.read() {
        let Some(name) = known.remove(&e) else { continue };
        // Parked with its tab, not deleted: its links went with the tab's resources.
        if parked.contains(e) {
            continue;
        }
        let still = objects.iter().any(|(_, o)| o.name == name) || pending.0.iter().any(|p| p.name.as_deref() == Some(name.as_str()));
        if !still {
            links.remove_object(&name);
            plant.controls.remove(&name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plant::connect::PortRef;

    #[test]
    fn plant_section_roundtrips() {
        let mut links = Links::default();
        links.add(Link { a: PortRef::new("tank_1", "outlet"), b: PortRef::new("pipe_1", "a"), kind: LinkKind::Pipe });
        let mut plant = Plant::default();
        plant.controls.insert("pump_1".into(), Control { on: true, rpm: Some(1200.0), ..default() });
        let v = plant_json(&links, &plant);
        let (l, c, _) = parse_plant(&v);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].b.object, "pipe_1");
        assert_eq!(c["pump_1"].rpm, Some(1200.0));
        let (l, c, _) = parse_plant(&Value::Null);
        assert!(l.is_empty() && c.is_empty());
    }
}
