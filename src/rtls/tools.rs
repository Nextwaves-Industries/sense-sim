//! Assistant / MCP tools for the RTLS simulator. Hooked into
//! `agent::tools` (definitions and dispatch on the `rtls_` prefix).

use bevy::prelude::*;
use serde_json::{Value, json};

use super::device::{Device, DeviceKind, Motion, Profiles, Zone, epc_for, next_id};
use super::io::{Hub, OutputsConfig};
use super::planning::{self, Coverage, CoverageRequest, OptimizeRequest, Tech};
use super::{RtlsConfig, RtlsState, sim};
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let vec3 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
    let motion = json!({
        "type": "object",
        "description": "Tag motion. {type:'static'} | {type:'path', points:[[x,y,z],...], speed:m/s, ping_pong:bool} | {type:'wander', center:[x,y,z], radius, speed} | {type:'follow', object:'<scene object name>', offset:[x,y,z]}"
    });
    vec![
        def(
            "rtls_auto_deploy",
            "ONE-SHOT: design and place a complete positioning system for the current scene (the fewest UWB anchors that cover the floor and every zone at the target accuracy, or the best placement within max_anchors, BLE beacons, one RFID reader per zone, UWB tags on vehicles, RFID tags on goods, workers walking the aisles), mounted on ceiling/walls/columns when the scene has them, then simulate it and report accuracy. Replaces devices from a previous auto deploy; keeps manual ones.",
            json!({ "type": "object", "properties": { "uwb": { "type": "boolean" }, "ble": { "type": "boolean" }, "rfid": { "type": "boolean" }, "target_cm": { "type": "number", "description": "p95 position error target, default 30" }, "anchor_tx_dbm_mhz": { "type": "number", "description": "UWB anchor transmit power in dBm/MHz (max -41.3). Lower = shorter range; placement is planned with it." }, "max_anchors": { "type": "integer","description": "Cap on UWB anchors in the scene (existing ones included). Omit for the fewest anchors that meet the target on the floor and in every zone." },"workers": { "type": "integer" }, "test_s": { "type": "number" } } }),
        ),
        def(
            "rtls_openems",
            "Manage the local OpenEMS Edge that Sense Sim installs and runs itself (portable Java + official openems-edge.jar + the Rtls.SenseSim component). action=start installs on first use (~120 MB download), starts it and connects the simulator; stop; status.",
            json!({ "type": "object", "properties": { "action": { "type": "string", "enum": ["start", "stop", "status"] } }, "required": ["action"] }),
        ),
        def(
            "rtls_list_profiles",
            "List the available RTLS device profiles (UWB anchors/tags, BLE beacons/tags, RFID readers/tags) with their radio parameters.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "rtls_add_device",
            "Place a simulated positioning device. Y is up (metres); anchors and readers are usually mounted 2-3 m high, tags about 1-1.2 m. RFID readers face their yaw direction.",
            json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Profile id from rtls_list_profiles, e.g. uwb-anchor, uwb-tag, uwb-tag-tdoa, ble-beacon, ble-tag, rfid-reader, rfid-portal, rfid-tag" },
                    "id": { "type": "string", "description": "Optional device id (default: next free, e.g. A3)" },
                    "position": vec3,
                    "yaw_deg": { "type": "number" },
                    "motion": motion,
                    "address": { "type": "string", "description": "RFID EPC or hardware address" }
                },
                "required": ["profile", "position"]
            }),
        ),
        def(
            "rtls_update_device",
            "Change a device: position, yaw, enabled (false simulates an offline/failed device), profile, motion or address.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "position": vec3,
                    "yaw_deg": { "type": "number" },
                    "enabled": { "type": "boolean" },
                    "profile": { "type": "string" },
                    "motion": motion,
                    "address": { "type": "string" }
                },
                "required": ["id"]
            }),
        ),
        def("rtls_remove_device", "Remove a device by id ('*' removes all devices).", json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] })),
        def("rtls_list_devices", "List all simulated devices and zones with positions, state and the latest tag estimates/errors.", json!({ "type": "object", "properties": {} })),
        def(
            "rtls_add_zone",
            "Add or replace a zone (axis-aligned box) for presence, occupancy and alarms.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "min": vec3,
                    "max": vec3,
                    "restricted": { "type": "boolean", "description": "Entering raises an alarm" },
                    "capacity": { "type": "integer", "description": "Max tags before an alarm (0 = none)" }
                },
                "required": ["name", "min", "max"]
            }),
        ),
        def(
            "rtls_run",
            "Run the simulation for N simulated seconds right now and return accuracy metrics (RMSE, CEP50/95), per-tag fix rates, RFID read rate, alarms and output status.",
            json!({ "type": "object", "properties": { "seconds": { "type": "number" }, "reset": { "type": "boolean", "description": "Restart time and statistics first (default true)" } }, "required": ["seconds"] }),
        ),
        def("rtls_status", "Current live metrics, the latest snapshot sent to outputs, active alarms, output connection status and values read back from OpenEMS/MQTT.", json!({ "type": "object", "properties": {} })),
        def(
            "rtls_control",
            "Run/pause the real-time simulation, change its speed, or change solver settings.",
            json!({
                "type": "object",
                "properties": {
                    "running": { "type": "boolean" },
                    "speed": { "type": "number" },
                    "kalman": { "type": "boolean" },
                    "robust": { "type": "boolean" },
                    "solve_3d": { "type": "boolean" },
                    "seed": { "type": "integer" }
                }
            }),
        ),
        def(
            "rtls_coverage",
            "Compute the coverage heatmap for UWB, BLE or RFID at a tag height: share of the floor with a position fix and predicted error (ray cast against the scene). Shows it in the viewport.",
            json!({ "type": "object", "properties": { "tech": { "type": "string", "enum": ["uwb", "ble", "rfid"] }, "height": { "type": "number" }, "cell": { "type": "number" } }, "required": ["tech"] }),
        ),
        def(
            "rtls_optimize_anchors",
            "Suggest where to add N more anchors/beacons/readers to maximise line-of-sight coverage and geometry. With apply=true they are placed.",
            json!({ "type": "object", "properties": { "tech": { "type": "string", "enum": ["uwb", "ble", "rfid"] }, "count": { "type": "integer", "description": "Most devices to add" }, "height": { "type": "number" }, "target_coverage": { "type": "number", "description": "Stop once this share of the floor is covered (0..1)" }, "mount": { "type": "string", "enum": ["grid", "structure"], "description": "grid (default): room perimeter + grid; structure: ceiling/walls/columns from add_structure" }, "apply": { "type": "boolean" } }, "required": ["tech", "count"] }),
        ),
        def(
            "rtls_configure_outputs",
            "Configure and (re)connect real-time outputs. Pass any subset: {mqtt:{enabled,host,port,username,password,prefix,qos,subscribe}, openems:{enabled,url,username,password,period_s,write:[{metric,address}],read:[address]}, webhook:{enabled,url,headers,batch_ms}, udp:{enabled,target,format}, modbus:{enabled,bind}, recorder:{enabled,path}}. Metrics for OpenEMS: tags_total, tags_located, tags_lost, infra_online, alarms_active, rmse_cm, cep95_cm, rfid_reads_per_s, zone:<name>, tag:<id>:x|y|z|lost.",
            json!({ "type": "object", "properties": { "outputs": { "type": "object" } }, "required": ["outputs"] }),
        ),
    ]
}

fn vec3(v: &Value) -> Option<Vec3> {
    let a = v.as_array()?;
    (a.len() == 3).then(|| Vec3::new(a[0].as_f64().unwrap_or(0.0) as f32, a[1].as_f64().unwrap_or(0.0) as f32, a[2].as_f64().unwrap_or(0.0) as f32))
}

fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

fn find(world: &mut World, id: &str) -> Option<Entity> {
    let mut q = world.query::<(Entity, &Device)>();
    q.iter(world).find(|(_, d)| d.id == id).map(|(e, _)| e)
}

fn parse_motion(v: &Value) -> Result<Motion, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("bad motion: {e}"))
}

fn device_json(world: &World, e: Entity, d: &Device, t: &Transform) -> Value {
    let st = world.resource::<RtlsState>();
    let (yaw, _, _) = t.rotation.to_euler(EulerRot::YXZ);
    let mut v = json!({
        "id": d.id,
        "kind": d.kind.label(),
        "profile": d.profile,
        "enabled": d.enabled,
        "position": [r2(t.translation.x), r2(t.translation.y), r2(t.translation.z)],
        "yaw_deg": r2(yaw.to_degrees()),
        "motion": d.motion,
    });
    if !d.address.is_empty() {
        v["address"] = json!(d.address);
    }
    if let Some(tr) = st.tracks.get(&e) {
        v["estimate"] = json!(tr.est.map(|p| [r2(p.x), r2(p.y), r2(p.z)]));
        v["error_m"] = json!(tr.err.map(r2));
        v["lost"] = json!(tr.lost);
        v["links"] = json!(tr.links.iter().filter(|l| l.value.is_some()).count());
        v["nlos_links"] = json!(tr.links.iter().filter(|l| l.value.is_some() && l.walls > 0).count());
        v["zones"] = json!(tr.zones);
    }
    v
}

fn set_outputs(cfg: &mut OutputsConfig, patch: &Value) -> Result<(), String> {
    let mut cur = serde_json::to_value(&*cfg).map_err(|e| e.to_string())?;
    merge(&mut cur, patch);
    *cfg = serde_json::from_value(cur).map_err(|e| format!("invalid outputs config: {e}"))?;
    Ok(())
}

fn merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (a, b) => *a = b.clone(),
    }
}

pub fn status_json(world: &mut World) -> Value {
    let hub = world.resource::<Hub>();
    let outputs: Vec<Value> = hub
        .sinks
        .iter()
        .map(|s| {
            let st = s.shared.status();
            json!({ "name": s.name, "state": format!("{:?}", st.state), "detail": st.detail, "sent": st.sent, "dropped": st.dropped, "rtt_ms": st.rtt_ms, "last_error": st.last_error })
        })
        .collect();
    let feedback: Vec<Value> = hub.feedback().into_iter().map(|(k, v, age)| json!({ "source": k, "value": v, "age_s": r2(age) })).collect();
    let mut report = sim::report_json(world);
    report["outputs"] = json!(outputs);
    report["read_back"] = json!(feedback);
    report["running"] = json!(world.resource::<RtlsConfig>().settings.running);
    report
}

/// Handles an `rtls_*` tool. `None` if the name is not ours.
pub fn start(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        "rtls_list_profiles" => {
            let p = world.resource::<Profiles>();
            ToolOutput::ok(json!(p.0))
        }
        "rtls_add_device" => {
            let pid = i["profile"].as_str().unwrap_or("");
            let Some(prof) = world.resource::<Profiles>().get(pid).cloned() else {
                let ids: Vec<String> = world.resource::<Profiles>().0.iter().map(|p| p.id.clone()).collect();
                return Some(ToolOutput::err(format!("unknown profile '{pid}'. Available: {}", ids.join(", "))));
            };
            let Some(pos) = vec3(&i["position"]) else { return Some(ToolOutput::err("position [x,y,z] required")) };
            let id = match i["id"].as_str() {
                Some(s) if !s.is_empty() => {
                    if find(world, s).is_some() {
                        return Some(ToolOutput::err(format!("device id '{s}' already exists")));
                    }
                    s.to_string()
                }
                _ => {
                    let mut q = world.query::<&Device>();
                    let ids: Vec<String> = q.iter(world).map(|d| d.id.clone()).collect();
                    next_id(prof.kind, ids.iter().map(String::as_str))
                }
            };
            let motion = match i.get("motion") {
                Some(m) if !m.is_null() => match parse_motion(m) {
                    Ok(m) => m,
                    Err(e) => return Some(ToolOutput::err(e)),
                },
                _ => Motion::Static,
            };
            let address = i["address"].as_str().map(str::to_string).unwrap_or_else(|| if prof.kind == DeviceKind::RfidTag { epc_for(&id) } else { String::new() });
            let yaw = i["yaw_deg"].as_f64().unwrap_or(0.0) as f32;
            let t = Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw.to_radians()));
            let d = Device { id: id.clone(), kind: prof.kind, profile: prof.id.clone(), enabled: true, motion, address, auto: false, radio: Default::default() };
            world.spawn((Name::new(format!("{} {}", prof.kind.label(), id)), d, t));
            ToolOutput::text(format!("Placed {} '{id}' ({}) at [{:.2}, {:.2}, {:.2}]", prof.kind.label(), prof.name, pos.x, pos.y, pos.z))
        }
        "rtls_update_device" => {
            let id = i["id"].as_str().unwrap_or("");
            let Some(e) = find(world, id) else { return Some(ToolOutput::err(format!("no device '{id}'"))) };
            let profile = i["profile"].as_str().map(|p| world.resource::<Profiles>().get(p).cloned());
            let motion = match i.get("motion") {
                Some(m) if !m.is_null() => match parse_motion(m) {
                    Ok(m) => Some(m),
                    Err(e) => return Some(ToolOutput::err(e)),
                },
                _ => None,
            };
            let mut em = world.entity_mut(e);
            if let Some(mut t) = em.get_mut::<Transform>() {
                if let Some(p) = vec3(&i["position"]) {
                    t.translation = p;
                }
                if let Some(y) = i["yaw_deg"].as_f64() {
                    t.rotation = Quat::from_rotation_y((y as f32).to_radians());
                }
            }
            let mut d = em.get_mut::<Device>().unwrap();
            if let Some(b) = i["enabled"].as_bool() {
                d.enabled = b;
            }
            if let Some(a) = i["address"].as_str() {
                d.address = a.into();
            }
            if let Some(m) = motion {
                d.motion = m;
            }
            match profile {
                Some(Some(p)) if p.kind == d.kind => d.profile = p.id,
                Some(Some(p)) => return Some(ToolOutput::err(format!("profile {} is a {}, device is a {}", p.id, p.kind.label(), d.kind.label()))),
                Some(None) => return Some(ToolOutput::err("unknown profile")),
                None => {}
            }
            ToolOutput::text(format!("Updated {id}"))
        }
        "rtls_remove_device" => {
            let id = i["id"].as_str().unwrap_or("");
            let mut q = world.query::<(Entity, &Device)>();
            let es: Vec<Entity> = q.iter(world).filter(|(_, d)| id == "*" || d.id == id).map(|(e, _)| e).collect();
            if es.is_empty() {
                return Some(ToolOutput::err(format!("no device '{id}'")));
            }
            let n = es.len();
            for e in es {
                world.despawn(e);
            }
            ToolOutput::text(format!("Removed {n} device(s)"))
        }
        "rtls_list_devices" => {
            let mut q = world.query::<(Entity, &Device, &Transform)>();
            let list: Vec<(Entity, Device, Transform)> = q.iter(world).map(|(e, d, t)| (e, d.clone(), *t)).collect();
            let mut devices: Vec<Value> = list.iter().map(|(e, d, t)| device_json(world, *e, d, t)).collect();
            devices.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            let mut zq = world.query::<&Zone>();
            let zones: Vec<Value> = zq.iter(world).map(|z| json!(z)).collect();
            ToolOutput::ok(json!({ "devices": devices, "zones": zones }))
        }
        "rtls_add_zone" => {
            let (Some(a), Some(b)) = (vec3(&i["min"]), vec3(&i["max"])) else { return Some(ToolOutput::err("min and max [x,y,z] required")) };
            let name = i["name"].as_str().unwrap_or("Zone").to_string();
            let z = Zone {
                name: name.clone(),
                min: a.min(b).to_array(),
                max: a.max(b).to_array(),
                restricted: i["restricted"].as_bool().unwrap_or(false),
                capacity: i["capacity"].as_u64().unwrap_or(0) as u32,
                auto: false,
            };
            let mut q = world.query::<(Entity, &Zone)>();
            let old: Vec<Entity> = q.iter(world).filter(|(_, z)| z.name == name).map(|(e, _)| e).collect();
            for e in old {
                world.despawn(e);
            }
            world.spawn((Name::new(format!("Zone {name}")), z));
            ToolOutput::text(format!("Zone '{name}' set"))
        }
        "rtls_run" => {
            let secs = i["seconds"].as_f64().unwrap_or(30.0).clamp(0.1, 3600.0);
            if i["reset"].as_bool().unwrap_or(true) {
                let seed = world.resource::<RtlsConfig>().settings.seed;
                world.resource_mut::<RtlsState>().restart(seed);
            }
            let ticks = sim::run_for(world, secs);
            let mut r = sim::report_json(world);
            r["ticks"] = json!(ticks);
            ToolOutput::ok(r)
        }
        "rtls_status" => ToolOutput::ok(status_json(world)),
        "rtls_control" => {
            let mut cfg = world.resource_mut::<RtlsConfig>();
            let s = &mut cfg.settings;
            if let Some(b) = i["running"].as_bool() {
                s.running = b;
            }
            if let Some(x) = i["speed"].as_f64() {
                s.speed = (x as f32).clamp(0.0, 100.0);
            }
            if let Some(b) = i["kalman"].as_bool() {
                s.kalman = b;
            }
            if let Some(b) = i["robust"].as_bool() {
                s.robust = b;
            }
            if let Some(b) = i["solve_3d"].as_bool() {
                s.solve_3d = b;
            }
            if let Some(x) = i["seed"].as_u64() {
                s.seed = x;
            }
            let out = json!(cfg.settings);
            cfg.save();
            ToolOutput::ok(out)
        }
        "rtls_coverage" => {
            let tech = i["tech"].as_str().and_then(Tech::parse).unwrap_or_default();
            let req = CoverageRequest { tech, height: i["height"].as_f64().unwrap_or(1.2) as f32, cell: i["cell"].as_f64().unwrap_or(0.5) as f32 };
            let _ = world.run_system_cached_with(planning::compute, req);
            let cov = world.resource::<Coverage>();
            let (covered, mean, p95) = cov.stats();
            let q = cov.quality;
            ToolOutput::ok(json!({
                "summary": cov.summary, "covered_fraction": covered, "mean_predicted_error_m": mean, "p95_predicted_error_m": p95, "cells": cov.cells.len(), "cell_m": cov.cell,
                // Share of the floor where >= min_share of readings land within target_m.
                "quality": { "pass_fraction": cov.quality_share(), "target_m": q.target_m, "min_share": q.min_share },
            }))
        }
        "rtls_optimize_anchors" => {
            let tech = i["tech"].as_str().and_then(Tech::parse).unwrap_or_default();
            let n = i["count"].as_u64().unwrap_or(4) as usize;
            // mount: "structure" = ceiling/walls/columns of the building; "grid" (default) = perimeter + grid.
            let candidates = match i["mount"].as_str() {
                Some("structure") => {
                    let h = world.resource::<Profiles>().default_for(tech.infra()).map(|p| p.mount_height_m).unwrap_or(2.5);
                    let opts = crate::agent::structures::MountOpts { mount_height_m: h, ceiling: tech != Tech::Rfid, ..Default::default() };
                    let m: Vec<(Vec3, Vec3)> = crate::agent::structures::mount_points(world, &opts).into_iter().map(|m| (m.pos, m.facing)).collect();
                    if m.is_empty() {
                        return Some(ToolOutput::err("mount=structure: the scene has no ceiling, walls or columns (add_structure first) - use mount=grid"));
                    }
                    Some(m)
                }
                _ => None,
            };
            let req = OptimizeRequest {
                tech,
                add: n,
                height: i["height"].as_f64().unwrap_or(1.2) as f32,
                candidates,
                target_coverage: i["target_coverage"].as_f64().map(|x| x as f32),
                ..Default::default()
            };
            let _ = world.run_system_cached_with(planning::optimize, req);
            let (sugg, facing, note) = {
                let c = world.resource::<Coverage>();
                (c.suggestions.clone(), c.suggestion_facing.clone(), c.suggestion_note.clone())
            };
            if i["apply"].as_bool().unwrap_or(false) {
                let prof = world.resource::<Profiles>().default_for(tech.infra()).cloned();
                if let Some(prof) = prof {
                    for (k, p) in sugg.iter().enumerate() {
                        let dir = facing.get(k).copied().unwrap_or(Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
                        let up = if dir.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
                        let mut q = world.query::<&Device>();
                        let ids: Vec<String> = q.iter(world).map(|d| d.id.clone()).collect();
                        let id = next_id(prof.kind, ids.iter().map(String::as_str));
                        world.spawn((Name::new(id.clone()), Device { id, kind: prof.kind, profile: prof.id.clone(), enabled: true, motion: Motion::Static, address: String::new(), auto: false, radio: Default::default() }, Transform::from_translation(*p).looking_to(dir, up)));
                    }
                }
                world.resource_mut::<Coverage>().suggestions.clear();
            }
            ToolOutput::ok(json!({ "note": note, "positions": sugg.iter().map(|p| [r2(p.x), r2(p.y), r2(p.z)]).collect::<Vec<_>>(), "applied": i["apply"].as_bool().unwrap_or(false) }))
        }
        "rtls_configure_outputs" => {
            let patch = i.get("outputs").unwrap_or(i);
            let mut cfg = world.resource::<RtlsConfig>().clone();
            if let Err(e) = set_outputs(&mut cfg.outputs, patch) {
                return Some(ToolOutput::err(e));
            }
            cfg.save();
            world.resource_mut::<Hub>().apply(&cfg.outputs);
            *world.resource_mut::<RtlsConfig>() = cfg;
            // Give connections a moment so the status is meaningful.
            std::thread::sleep(std::time::Duration::from_millis(600));
            ToolOutput::ok(status_json(world)["outputs"].clone())
        }
        "rtls_auto_deploy" => {
            let d = super::deploy::DeployRequest::default();
            let req = super::deploy::DeployRequest {
                uwb: i["uwb"].as_bool().unwrap_or(d.uwb),
                ble: i["ble"].as_bool().unwrap_or(d.ble),
                rfid: i["rfid"].as_bool().unwrap_or(d.rfid),
                target_m: i["target_cm"].as_f64().map(|c| c as f32 / 100.0).unwrap_or(d.target_m),
                workers: i["workers"].as_u64().map(|w| w as usize).unwrap_or(d.workers),
                test_s: i["test_s"].as_f64().unwrap_or(d.test_s),
                max_anchors: i["max_anchors"].as_u64().map(|m| m as usize),
                anchor_tx_dbm_mhz: i["anchor_tx_dbm_mhz"].as_f64().map(|v| v as f32),
                ..d
            };
            let r = super::deploy::auto_deploy(world, &req);
            world.resource_mut::<super::ui::DeployState>().last = Some(r.clone());
            ToolOutput::ok(json!({
                "placed": r.headline(),
                "zones_created": r.zones_created,
                "floor_coverage": r2(r.coverage),
                "zones_fully_covered": format!("{}/{}", r.zones_covered, r.zones_total),
                "predicted_p95_m": r2(r.predicted_p95_m),
                "targets_met": r.targets_met,
                "test": { "uwb_rmse_m": r2(r.uwb_rmse_m), "uwb_cep95_m": r2(r.uwb_cep95_m), "all_tech_rmse_m": r2(r.rmse_m), "located": r.located, "tags": r.total_tags },
                "target_applies_to": "UWB p95 (simulated)",
                "rfid_unread": r.rfid_unread,
                "alarms": r.alarms,
                "notes": r.notes,
            }))
        }
        "rtls_openems" => {
            let action = i["action"].as_str().unwrap_or("status");
            let mut ems = world.resource_mut::<super::openems_local::LocalEms>();
            match action {
                "start" => ems.start(),
                "stop" => ems.stop(),
                _ => {}
            }
            let phase = ems.phase();
            ToolOutput::ok(json!({ "phase": format!("{phase:?}"), "rest": super::openems_local::REST_URL, "console": super::openems_local::CONSOLE_URL, "note": if action == "start" { "Starting runs in the background (first time downloads ~120 MB); call again with action=status." } else { "" } }))
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_outputs_patch() {
        let mut cfg = OutputsConfig::default();
        set_outputs(&mut cfg, &json!({ "mqtt": { "enabled": true, "port": 1884 }, "openems": { "url": "http://ems:8084" } })).unwrap();
        assert!(cfg.mqtt.enabled);
        assert_eq!(cfg.mqtt.port, 1884);
        assert_eq!(cfg.mqtt.host, "127.0.0.1");
        assert_eq!(cfg.openems.url, "http://ems:8084");
        assert!(set_outputs(&mut cfg, &json!({ "mqtt": { "port": "nope" } })).is_err());
    }

    #[test]
    fn motion_json_shapes() {
        let m = parse_motion(&json!({ "type": "path", "points": [[0, 1, 0], [2, 1, 0]], "speed": 1.5, "ping_pong": false })).unwrap();
        assert!(matches!(m, Motion::Path { .. }));
        let m = parse_motion(&json!({ "type": "follow", "object": "forklift_1", "offset": [0, 2, 0] })).unwrap();
        assert!(matches!(m, Motion::Follow { .. }));
    }
}
