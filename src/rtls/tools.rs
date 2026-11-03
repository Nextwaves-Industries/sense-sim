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
    #[allow(unused_mut)]
    let mut all = vec![
        def(
            "rtls_auto_deploy",
            "ONE-SHOT: design and place a complete positioning system for the current scene (the fewest UWB anchors that cover the floor and every zone at the target accuracy, or the best placement within max_anchors, BLE beacons, BLE AoA locators, WiFi APs, RFID readers aimed at shelves and zones with portal pairs on doorways, UWB tags on vehicles, RFID tags on goods, workers walking the aisles), mounted on the ceiling, walls, columns and racks, then simulate it and report accuracy. Never in mid-air: a scene with none of those gets an error, so add_structure first. Replaces devices from a previous auto deploy; keeps manual ones.",
            json!({ "type": "object", "properties": { "uwb": { "type": "boolean" }, "ble": { "type": "boolean" }, "rfid": { "type": "boolean" }, "ble_aoa": { "type": "boolean", "description": "BLE with direction-finding locators instead of RSSI beacons" }, "wifi": { "type": "boolean", "description": "WiFi access points with RTT" }, "heights": { "type": "array", "items": { "type": "number" }, "description": "More tag heights to plan for (m), e.g. [0.3, 1.8]" }, "dim3": { "type": "boolean", "description": "Plan for full 3D fixes" }, "redundancy": { "type": "boolean", "description": "Keep the target with any one device failed" }, "min_spacing_m": { "type": "number" }, "cost_weight": { "type": "number", "description": "Weight of install cost against coverage (default 0.0005)" }, "head_end": vec3, "refine": { "type": "boolean", "description": "Fine-tune positions along the mounting surface (default true)" }, "colocate": { "type": "boolean", "description": "Let technologies share a mount at lower cost" }, "target_cm": { "type": "number", "description": "p95 position error target, default 30" }, "anchor_tx_dbm_mhz": { "type": "number", "description": "UWB anchor transmit power in dBm/MHz (max -41.3). Lower = shorter range; placement is planned with it." }, "max_anchors": { "type": "integer","description": "Cap on UWB anchors in the scene (existing ones included). Omit for the fewest anchors that meet the target on the floor and in every zone." },"workers": { "type": "integer" }, "test_s": { "type": "number" } } }),
        ),
        def(
            "rtls_openems",
            "Manage the local OpenEMS Edge that Sense Sim installs and runs itself (portable Java + official openems-edge.jar + the Rtls.SenseSim component). action=start installs on first use (~120 MB download), starts it and connects the simulator; stop; status.",
            json!({ "type": "object", "properties": { "action": { "type": "string", "enum": ["start", "stop", "status"] } }, "required": ["action"] }),
        ),
        def(
            "rtls_list_regions",
            "List the regulatory regions: for each country the legal UWB channels and power and the UHF RFID band and power, how well each rule is verified, and the active region.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "rtls_set_region",
            "Set the regulatory region of the deployment. Profiles, simulation, coverage and auto deploy then use legal values: UWB moves to an allowed channel and RFID readers are capped to the EIRP limit. Set it before planning. Returns what changed and any warnings. Rules not verified for a country change nothing and are reported.",
            json!({ "type": "object", "properties": { "region": { "type": "string", "description": "Region id from rtls_list_regions, e.g. 'vn'; 'none' removes it" } }, "required": ["region"] }),
        ),
        def(
            "rtls_list_profiles",
            "List the available RTLS device profiles (UWB anchors/tags, BLE beacons/tags, RFID readers/tags) with their radio parameters.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "rtls_add_device",
            "Place a simulated positioning device. Y is up (metres). Anchors, beacons, readers, access points and gateways are fixed to something: a position with nothing holding it goes to the nearest ceiling, wall, column or rack and faces out of it, and the reply says where it landed. Check that, it can be far from what you asked. With nothing to mount on it is an error: add_structure first. Tags are worn or carried, about 1-1.2 m, and stay where you put them. RFID readers face their yaw direction, aimed up or down by tilt_deg; give angles to aim one on its mount.",
            json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Profile id from rtls_list_profiles, e.g. uwb-anchor, uwb-tag, uwb-tag-tdoa, ble-beacon, ble-tag, rfid-reader, rfid-portal, rfid-tag" },
                    "id": { "type": "string", "description": "Optional device id (default: next free, e.g. A3)" },
                    "position": vec3,
                    "yaw_deg": { "type": "number" },
                    "tilt_deg": { "type": "number", "description": "Aim up (+) or down (-); -90 points straight down" },
                    "roll_deg": { "type": "number", "description": "Turn around the facing direction" },
                    "motion": motion,
                    "address": { "type": "string", "description": "RFID EPC or hardware address" }
                },
                "required": ["profile", "position"]
            }),
        ),
        def(
            "rtls_update_device",
            "Change a device: position, yaw, tilt, roll (angles not given are kept), enabled (false simulates an offline/failed device), profile, motion or address. A new position for a fixed device goes on the nearest ceiling, wall, column or rack if nothing holds it there, as in rtls_add_device.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "position": vec3,
                    "yaw_deg": { "type": "number" },
                    "tilt_deg": { "type": "number", "description": "Aim up (+) or down (-); -90 points straight down" },
                    "roll_deg": { "type": "number", "description": "Turn around the facing direction" },
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
            "Run the simulation for N simulated seconds right now and return accuracy metrics over the whole run (RMSE, CEP50/95, sample count, availability; moving and parked tags apart; null with no fixes), per-tag fix rates, RFID reads and read chance, alarms and output status. Simulated results, not measurements.",
            json!({ "type": "object", "properties": { "seconds": { "type": "number" }, "reset": { "type": "boolean", "description": "Restart time and statistics first, and put the tags back where they started afterwards, so the same seed reproduces the run (default true)" } }, "required": ["seconds"] }),
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
            "Compute the coverage heatmap for UWB, BLE, RFID or WiFi at a tag height: share of the floor with a position fix and predicted RMS error (ray cast against the scene; 95 % of readings land within 1.73 times that). RFID has no position: its error fields are null and read_margin_db holds the link margin. Shows it in the viewport.",
            json!({ "type": "object", "properties": { "tech": { "type": "string", "enum": ["uwb", "ble", "rfid", "wifi"] }, "height": { "type": "number" }, "cell": { "type": "number" } }, "required": ["tech"] }),
        ),
        def(
            "rtls_optimize_anchors",
            "Suggest where to add N more anchors/beacons/readers/access points on the scene's ceiling, walls, columns and racks, scored with the simulation's link model (noise, NLOS, dropouts, read chance, geometry) and install cost, with aims for directional readers. With apply=true they are placed. A scene with nothing to mount on gets an error: add_structure first.",
            json!({ "type": "object", "properties": { "profile": { "type": "string", "description": "Infrastructure profile, e.g. ble-aoa-locator, wifi-ap-rssi" }, "heights": { "type": "array", "items": { "type": "number" } }, "dim3": { "type": "boolean" }, "redundancy": { "type": "boolean" }, "min_spacing_m": { "type": "number" }, "cost_weight": { "type": "number" }, "head_end": vec3, "refine": { "type": "boolean" }, "target_p95_m": { "type": "number" }, "tech": { "type": "string", "enum": ["uwb", "ble", "rfid", "wifi"] }, "count": { "type": "integer", "description": "Most devices to add" }, "height": { "type": "number" }, "target_coverage": { "type": "number", "description": "Stop once this share of the floor is covered (0..1)" }, "apply": { "type": "boolean" } }, "required": ["tech", "count"] }),
        ),
        def(
            "rtls_configure_outputs",
            "Configure and (re)connect real-time outputs. Pass any subset: {mqtt:{enabled,host,port,username,password,prefix,qos,subscribe}, openems:{enabled,url,username,password,period_s,write:[{metric,address}],read:[address]}, webhook:{enabled,url,headers,batch_ms}, udp:{enabled,target,format}, modbus:{enabled,bind}, recorder:{enabled,path}}. Metrics for OpenEMS: tags_total, tags_located, tags_lost, infra_online, alarms_active, rmse_cm, cep95_cm, rfid_reads_per_s, zone:<name>, tag:<id>:x|y|z|lost.",
            json!({ "type": "object", "properties": { "outputs": { "type": "object" } }, "required": ["outputs"] }),
        ),
    ];
    all.push(def(
        "rtls_smart_plan",
        "Find UWB plans a site can live with, rather than one layout. Works out from the layout where tags really go and judges plans there, searches for anchors that hold on a busy shift, with materials 50 % worse and with racks half full (taking the worst, not the average), then simulates the choices (Lean, Balanced, Bulletproof), each on every day and with its most relied-on anchor dead, and runs plain Auto deploy on the same tests to compare. Puts the recommended plan on the floor. Also returns why each anchor is there, what each extra anchor buys, and an antenna brief: the cone the anchors really serve and how much range shift it can take, ready for design_uwb_bias theta_min/theta_max. Slow: minutes on a big site. Use it when the user wants to choose a plan or asks whether one will hold; rtls_auto_deploy is for one quick answer and for other technologies.",
        json!({ "type": "object", "properties": { "target_cm": { "type": "number", "description": "95th-percentile position error to hold, default 30" }, "max_anchors": { "type": "integer", "description": "Cap on anchors in the scene, yours included" }, "test_s": { "type": "number", "description": "Simulated seconds per test, default 20" }, "anchor_price": { "type": "number" }, "fit_price": { "type": "number" } } }),
    ));
    all
}

fn vec3(v: &Value) -> Option<Vec3> {
    let a = v.as_array()?;
    (a.len() == 3).then(|| Vec3::new(a[0].as_f64().unwrap_or(0.0) as f32, a[1].as_f64().unwrap_or(0.0) as f32, a[2].as_f64().unwrap_or(0.0) as f32))
}

/// What a reply adds about where a fixed device went, so the caller knows it moved.
fn mount_note(m: &sim::Mounted, asked: Vec3) -> String {
    if m.moved_m > 0.0 {
        format!(" on {}. Moved {:.2} m: [{:.2}, {:.2}, {:.2}] was in mid-air", m.on, m.moved_m, asked.x, asked.y, asked.z)
    } else if m.on.is_empty() {
        String::new()
    } else {
        format!(" on {}", m.on)
    }
}

fn no_mount_error(what: &str, at: Vec3) -> String {
    format!("{what} not placed: nothing to mount it on near [{:.2}, {:.2}, {:.2}]. The scene has no ceiling, wall, column or rack there: add_structure first (in the open, a column makes a pole).", at.x, at.y, at.z)
}

/// A JSON array of numbers (empty when it isn't one).
fn floats(v: &Value) -> Vec<f32> {
    v.as_array().map_or(Vec::new(), |a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
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
    let (yaw, tilt, roll) = t.rotation.to_euler(EulerRot::YXZ);
    let mut v = json!({
        "id": d.id,
        "kind": d.kind.label(),
        "profile": d.profile,
        "enabled": d.enabled,
        "position": [r2(t.translation.x), r2(t.translation.y), r2(t.translation.z)],
        "yaw_deg": r2(yaw.to_degrees()),
        "tilt_deg": r2(tilt.to_degrees()),
        "roll_deg": r2(roll.to_degrees()),
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

/// The active region, or a reminder that results assume no local limits.
fn region_json(world: &World) -> Value {
    match &world.resource::<Profiles>().1.active {
        Some(r) => json!({ "id": r.id, "name": r.name, "warnings": r.warnings() }),
        None => json!({ "id": Value::Null, "warning": "No region set: profile values may exceed local limits. Use rtls_set_region." }),
    }
}

/// A figure, or null where it isn't one (no position, or not enough of them).
fn finite(v: f32) -> Value {
    if v.is_finite() { json!(r2(v)) } else { Value::Null }
}

fn class_json(c: &super::deploy::ClassResult) -> Value {
    json!({
        "tags": c.tags,
        "updates": c.attempts,
        "with_a_fix": c.fixes,
        "availability": c.availability().map(r2),
        "rmse_m": c.rmse_m.map(r2),
        "p95_counting_no_fix_m": c.p95_m.map(finite),
        "p95_of_fixes_m": c.fix_p95_m.map(r2),
    })
}

/// What a coverage scan found. RFID has no position to predict an error for: its cells carry a
/// link margin in dB, reported under its own name.
fn coverage_json(cov: &Coverage) -> Value {
    let (covered, mean, p95) = cov.stats();
    let q = cov.quality;
    let (near_only, worst) = cov.proximity_only();
    let mut v = json!({
        "summary": cov.summary, "covered_fraction": covered, "cells": cov.cells.len(), "cell_m": cov.cell,
        // Share of the floor heard by too few for a position: no fix, however close.
        "proximity_only_fraction": near_only, "proximity_only_worst_m": worst,
        "mean_predicted_error_m": Value::Null, "p95_predicted_error_m": Value::Null,
        // Share of the floor where >= min_share of readings land within target_m.
        "quality": { "pass_fraction": Value::Null, "target_m": q.target_m, "min_share": q.min_share },
    });
    if cov.tech == Tech::Rfid {
        v["read_margin_db"] = json!({ "mean": mean, "p95": p95, "note": "Best link margin where the read chance per round is over 90 %" });
    } else if covered > 0.0 {
        // These are RMS: 95 % of readings land within 1.73 times as far.
        v["mean_predicted_error_m"] = json!(mean);
        v["p95_predicted_error_m"] = json!(p95);
        v["quality"]["pass_fraction"] = json!(cov.quality_share());
    } else {
        v["quality"]["pass_fraction"] = json!(0.0);
    }
    v
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
    report["region"] = region_json(world);
    report
}

/// Handles an `rtls_*` tool. `None` if the name is not ours.
pub fn start(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        "rtls_list_regions" => {
            let p = world.resource::<Profiles>();
            let list: Vec<Value> = p.1.regions.iter().map(|r| r.json()).collect();
            ToolOutput::ok(json!({ "active": p.1.active.as_ref().map(|r| r.id.clone()), "regions": list, "note": "Design inputs, not legal advice: confirm with the national regulator. Countries not listed can be added as JSON in the regions folder." }))
        }
        "rtls_set_region" => {
            let id = i["region"].as_str().unwrap_or("");
            let mut p = world.resource_mut::<Profiles>();
            if let Err(e) = p.set_region(id) {
                return Some(ToolOutput::err(e));
            }
            let out = match &p.1.active {
                Some(r) => json!({ "region": r.json(), "changes": r.changes(&p.1.base), "antenna_note": super::region::PATTERN_NOTE, "next": "Existing devices now use these values. Re-run rtls_coverage or rtls_auto_deploy: the layout may no longer meet its target." }),
                None => json!({ "region": Value::Null, "next": "Profile values restored." }),
            };
            let stored = p.1.active.as_ref().map(|r| r.id.clone()).unwrap_or_default();
            let mut cfg = world.resource_mut::<RtlsConfig>();
            cfg.settings.region = stored;
            cfg.save();
            ToolOutput::ok(out)
        }
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
            let mounted = if prof.kind.is_mobile() || !matches!(motion, Motion::Static) {
                None
            } else {
                match sim::Occluders::of_world(world).mount_for(pos) {
                    Some(m) => Some(m),
                    None => return Some(ToolOutput::err(no_mount_error(prof.kind.label(), pos))),
                }
            };
            let address = i["address"].as_str().map(str::to_string).unwrap_or_else(|| if prof.kind == DeviceKind::RfidTag { epc_for(&id) } else { String::new() });
            let angle = |key: &str| (i[key].as_f64().unwrap_or(0.0) as f32).to_radians();
            let rotation = Quat::from_euler(EulerRot::YXZ, angle("yaw_deg"), angle("tilt_deg"), angle("roll_deg"));
            let mut t = Transform::from_translation(mounted.as_ref().map_or(pos, |m| m.pos)).with_rotation(rotation);
            // Out of its surface, unless told how to aim.
            if !["yaw_deg", "tilt_deg", "roll_deg"].iter().any(|k| i[*k].is_number())
                && let Some(f) = mounted.as_ref().and_then(|m| m.facing)
            {
                t.look_to(f, if f.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y });
            }
            let at = t.translation;
            let d = Device { id: id.clone(), kind: prof.kind, profile: prof.id.clone(), enabled: true, motion, address, auto: false, radio: Default::default() };
            world.spawn((Name::new(format!("{} {}", prof.kind.label(), id)), d, t));
            ToolOutput::text(format!("Placed {} '{id}' ({}) at [{:.2}, {:.2}, {:.2}]{}", prof.kind.label(), prof.name, at.x, at.y, at.z, mounted.as_ref().map_or_else(String::new, |m| mount_note(m, pos))))
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
            let asked = vec3(&i["position"]);
            let (fixed, label) = {
                let d = world.entity(e).get::<Device>().unwrap();
                (!d.kind.is_mobile() && matches!(motion.as_ref().unwrap_or(&d.motion), Motion::Static), d.kind.label())
            };
            let mounted = match asked.filter(|_| fixed) {
                Some(p) => match sim::Occluders::of_world(world).mount_for(p) {
                    Some(m) => Some(m),
                    None => return Some(ToolOutput::err(no_mount_error(label, p))),
                },
                None => None,
            };
            let mut em = world.entity_mut(e);
            if let Some(mut t) = em.get_mut::<Transform>() {
                if let Some(p) = asked {
                    t.translation = mounted.as_ref().map_or(p, |m| m.pos);
                }
                // Moved onto another surface and given no aim: out of that surface.
                if let Some(f) = mounted.as_ref().filter(|m| m.moved_m > 0.0).and_then(|m| m.facing) {
                    t.look_to(f, if f.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y });
                }
                // Turn by the difference, so setting one angle keeps the others.
                let (yaw, tilt, roll) = t.rotation.to_euler(EulerRot::YXZ);
                let delta = |key: &str, now: f32| i[key].as_f64().map(|v| (v as f32).to_radians() - now);
                if let Some(d) = delta("yaw_deg", yaw) {
                    t.rotate_y(d);
                }
                if let Some(d) = delta("tilt_deg", tilt) {
                    t.rotate_local_x(d);
                }
                if let Some(d) = delta("roll_deg", roll) {
                    t.rotate_local_z(d);
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
            match (&mounted, asked) {
                (Some(m), Some(p)) => ToolOutput::text(format!("Updated {id}: at [{:.2}, {:.2}, {:.2}]{}", m.pos.x, m.pos.y, m.pos.z, mount_note(m, p))),
                _ => ToolOutput::text(format!("Updated {id}")),
            }
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
            let reset = i["reset"].as_bool().unwrap_or(true);
            let start = reset.then(|| sim::RunStart::take(world));
            if reset {
                let seed = world.resource::<RtlsConfig>().settings.seed;
                world.resource_mut::<RtlsState>().restart(seed);
            }
            let ticks = sim::run_for(world, secs);
            let mut r = sim::report_json(world);
            r["ticks"] = json!(ticks);
            if let Some(start) = start {
                start.restore(world);
            }
            if world.resource::<RtlsConfig>().settings.running {
                r["note"] = json!("The live simulation is running, so moving tags start wherever it has taken them. Pause it to reproduce a run exactly.");
            }
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
            let mut out = coverage_json(world.resource::<Coverage>());
            out["region"] = region_json(world);
            ToolOutput::ok(out)
        }
        "rtls_optimize_anchors" => {
            let tech = i["tech"].as_str().and_then(Tech::parse).unwrap_or_default();
            let n = i["count"].as_u64().unwrap_or(4) as usize;
            let mut req = OptimizeRequest {
                tech,
                add: n,
                height: i["height"].as_f64().unwrap_or(1.2) as f32,
                target_coverage: i["target_coverage"].as_f64().map(|x| x as f32),
                target_p95_m: i["target_p95_m"].as_f64().map(|x| x as f32),
                profile: i["profile"].as_str().map(str::to_string),
                heights: floats(&i["heights"]),
                dim3: i["dim3"].as_bool().unwrap_or(false),
                redundancy: i["redundancy"].as_bool().unwrap_or(false),
                min_spacing_m: i["min_spacing_m"].as_f64().map(|x| x as f32),
                cost_weight: i["cost_weight"].as_f64().map(|x| x as f32),
                head_end: vec3(&i["head_end"]),
                refine: i["refine"].as_bool().unwrap_or(true),
                expand_heights: true,
                ..Default::default()
            };
            super::deploy::fill_mounts(world, &mut req);
            if req.candidates.as_ref().is_none_or(Vec::is_empty) {
                return Some(ToolOutput::err("the scene has no ceiling, walls, columns or racks to mount devices on: add_structure first"));
            }
            let profile = req.profile.clone();
            let result = world.run_system_cached_with(planning::optimize, req).unwrap_or_default();
            let (sugg, facing, note) = {
                let c = world.resource::<Coverage>();
                (c.suggestions.clone(), c.suggestion_facing.clone(), c.suggestion_note.clone())
            };
            if i["apply"].as_bool().unwrap_or(false) {
                let prof = {
                    let all = world.resource::<Profiles>();
                    profile.as_deref().and_then(|id| all.get(id)).filter(|p| p.kind == tech.infra()).or_else(|| all.default_for(tech.infra())).cloned()
                };
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
            ToolOutput::ok(json!({ "note": note, "positions": sugg.iter().map(|p| [r2(p.x), r2(p.y), r2(p.z)]).collect::<Vec<_>>(), "applied": i["apply"].as_bool().unwrap_or(false), "targets_met": result.targets_met, "floor_coverage": r2(result.coverage_after), "reachable_coverage": r2(result.reachable_coverage) }))
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
                ble_aoa: i["ble_aoa"].as_bool().unwrap_or(d.ble_aoa),
                wifi: i["wifi"].as_bool().unwrap_or(d.wifi),
                heights: floats(&i["heights"]),
                dim3: i["dim3"].as_bool().unwrap_or(d.dim3),
                redundancy: i["redundancy"].as_bool().unwrap_or(d.redundancy),
                min_spacing_m: i["min_spacing_m"].as_f64().map(|v| v as f32),
                cost_weight: i["cost_weight"].as_f64().map(|v| v as f32),
                head_end: vec3(&i["head_end"]),
                refine: i["refine"].as_bool().unwrap_or(d.refine),
                colocate: i["colocate"].as_bool().unwrap_or(d.colocate),
                target_m: i["target_cm"].as_f64().map(|c| c as f32 / 100.0).unwrap_or(d.target_m),
                workers: i["workers"].as_u64().map(|w| w as usize).unwrap_or(d.workers),
                test_s: i["test_s"].as_f64().unwrap_or(d.test_s),
                max_anchors: i["max_anchors"].as_u64().map(|m| m as usize),
                anchor_tx_dbm_mhz: i["anchor_tx_dbm_mhz"].as_f64().map(|v| v as f32),
                ..d
            };
            let r = super::deploy::auto_deploy(world, &req);
            world.resource_mut::<super::ui::DeployState>().last = Some(r.clone());
            world.resource_mut::<super::smart::SmartPlan>().floor_replaced();
            if r.no_mounts {
                return Some(ToolOutput::err(format!("{} Build the shell with add_structure and call this again.", super::deploy::NO_MOUNTS)));
            }
            let (_, verdict) = r.verdict();
            ToolOutput::ok(json!({
                "placed": r.headline(),
                "verdict": verdict,
                "zones_created": r.zones_created,
                "floor_coverage": r2(r.coverage),
                "reachable_coverage": r2(r.reachable_coverage),
                "zones_meeting_coverage": format!("{}/{}", r.zones_covered, r.zones_total),
                "predicted_p95_m": finite(r.predicted_p95_m),
                "targets_met": r.targets_met,
                "plans": r.plans.iter().map(|p| json!({ "tech": p.tech, "devices": p.devices, "coverage": r2(p.coverage), "reachable_coverage": r2(p.reachable_coverage), "predicted_p95_m": finite(p.p95_m), "install_cost": r2(p.cost), "targets_met": p.targets_met })).collect::<Vec<_>>(),
                "feedback_added": r.feedback_added,
                "test": {
                    "simulated": true,
                    "seconds": r.test_s,
                    "uwb_rmse_m": r2(r.uwb_rmse_m),
                    // Null when more than 5 % of updates had no fix.
                    "uwb_cep95_m": finite(r.uwb_cep95_m),
                    "cep95_m": finite(r.cep95_m),
                    "all_tech_rmse_m": r2(r.rmse_m),
                    "found": r.found,
                    "located": r.located,
                    "tags": r.total_tags,
                    "accuracy": r.accuracy.iter().map(|a| json!({ "tech": a.tech, "target_m": a.target_m, "passed": a.passed(), "moving": class_json(&a.moving), "parked": class_json(&a.parked) })).collect::<Vec<_>>(),
                    "rfid_tags": r.rfid_tags,
                    "rfid_mean_read_chance_per_round": r2(r.rfid_read_chance),
                },
                "target_applies_to": "each technology's simulated p95, moving and parked tags apart, an update with no fix counting as a miss",
                "rfid_unread": r.rfid_unread,
                "alarms": r.alarms,
                "notes": r.notes,
                "region": region_json(world),
            }))
        }
        "rtls_smart_plan" => {
            let d = super::smart::SmartRequest::default();
            let costing = world.resource::<super::smart::SmartPlan>().costing;
            let given = |k: &str| i[k].as_f64();
            let mut edited = costing.edited;
            edited[0] |= given("anchor_price").is_some();
            edited[1] |= given("fit_price").is_some();
            let costing = super::smart::Costing { anchor: given("anchor_price").unwrap_or(costing.anchor), fit: given("fit_price").unwrap_or(costing.fit), edited, ..costing };
            // A test too short to see anything isn't a test.
            let test_s = i["test_s"].as_f64().unwrap_or(d.test_s).max(5.0);
            let req = super::smart::SmartRequest { target_m: i["target_cm"].as_f64().map_or(d.target_m, |c| c as f32 / 100.0), max_anchors: i["max_anchors"].as_u64().map(|m| m as usize), test_s, costing, ..d };
            let r = super::smart::run_to_end(world, req);
            if let Some(why) = &r.failed {
                return Some(ToolOutput::err(why.clone()));
            }
            let mut v = serde_json::to_value(&r).unwrap_or_default();
            v["brief"] = serde_json::to_value(&r.brief).unwrap_or_default();
            v["brief"]["theta_range_deg_upright_wire"] = json!(r.brief.theta_range(false));
            v["brief"]["theta_range_deg_patch"] = json!(r.brief.theta_range(true));
            v["evidence"] = json!("Simulated: not validated on hardware. Traffic is worked out from the layout, not measured");
            ToolOutput::ok(v)
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
