//! OpenEMS Edge REST (Controller.Api.Rest.ReadWrite):
//! `POST /rest/channel/<Component>/<Channel>` with `{"value": …}` and
//! `GET` of the same path to read values back. Basic auth (OpenEMS ignores
//! the user name; the password selects the role, e.g. `x:admin`).

use std::time::{Duration, Instant};

use base64::Engine;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use serde_json::{Value, json};

use super::{Control, Feedback, OpenEmsCfg, Outgoing, Shared};
use crate::rtls::events::{RtlsEvent, Snapshot};

/// Value of a named simulator metric. Names: `tags_total`, `tags_located`,
/// `tags_lost`, `infra_online`, `infra_total`, `alarms_active`, `rmse_cm`,
/// `cep95_cm`, `rfid_reads_per_s`, `running`, `zone:<name>` (occupancy),
/// `tag:<id>:x|y|z|lost|quality`.
pub fn metric(s: &Snapshot, name: &str) -> Option<f64> {
    let n = name.trim();
    // `zone#2`: occupancy of the 2nd zone by name (OpenEMS channels are numbered).
    if let Some(k) = n.strip_prefix("zone#") {
        let k: usize = k.parse().ok()?;
        return Some(s.zones.values().nth(k.checked_sub(1)?).copied().unwrap_or(0) as f64);
    }
    if let Some(z) = n.strip_prefix("zone:") {
        return Some(*s.zones.get(z).unwrap_or(&0) as f64);
    }
    // `anchor#3:success`: RF health of the 3rd infrastructure device (by id);
    // `anchor:A3:dbm` by id. Fields: heard, in_range, success, dbm, min_dbm, nlos, outliers, bias_cm.
    let anchor = if let Some(rest) = n.strip_prefix("anchor#") {
        let (k, field) = rest.split_once(':')?;
        let k: usize = k.parse().ok()?;
        Some((s.rf.anchors.get(k.checked_sub(1)?), field))
    } else if let Some(rest) = n.strip_prefix("anchor:") {
        let (id, field) = rest.rsplit_once(':')?;
        Some((s.rf.anchors.iter().find(|a| a.id == id), field))
    } else {
        None
    };
    if let Some((row, field)) = anchor {
        // A missing anchor reads as 0 so numbered channels stay defined.
        let Some(a) = row else { return Some(0.0) };
        return Some(match field {
            "heard" => a.heard as f64,
            "in_range" => a.in_range as f64,
            "success" => a.success_pct.round() as f64,
            "dbm" => a.avg_dbm.round() as f64,
            "min_dbm" => a.min_dbm.round() as f64,
            "nlos" => a.nlos_pct.round() as f64,
            "outliers" => a.outliers as f64,
            "bias_cm" => a.bias_cm.round() as f64,
            _ => return None,
        });
    }
    let rf = &s.rf;
    let rf_metric = match n {
        "rf_links" => Some(rf.links as f64),
        "rf_success_pct" => Some(rf.success_pct.round() as f64),
        "rf_avg_dbm" => Some(rf.avg_dbm.round() as f64),
        "rf_min_dbm" => Some(rf.min_dbm.round() as f64),
        "rf_weak_links" => Some(rf.weak_links as f64),
        "rf_nlos_pct" => Some(rf.nlos_pct.round() as f64),
        "rf_blocked_links" => Some(rf.blocked_links as f64),
        "rf_nlos_bias_cm" => Some(rf.nlos_bias_cm.round() as f64),
        "rf_tags_under3" => Some(rf.tags_under3 as f64),
        "rf_anchors_weak" => Some(rf.anchors_weak as f64),
        // -1 until a coverage scan ran.
        "rf_covered_pct" => Some(rf.covered_pct.map_or(-1.0, |v| v.round() as f64)),
        "rf_dead_area_m2" => Some(rf.dead_area_m2.map_or(-1.0, |v| v.round() as f64)),
        _ => None,
    };
    if rf_metric.is_some() {
        return rf_metric;
    }
    if let Some(rest) = n.strip_prefix("tag:") {
        let (id, field) = rest.rsplit_once(':')?;
        let t = s.tags.iter().find(|t| t.id == id)?;
        return Some(match field {
            "x" => t.x as f64,
            "y" => t.y as f64,
            "z" => t.z as f64,
            "lost" => t.lost as u8 as f64,
            "quality" => t.quality as f64,
            _ => return None,
        });
    }
    Some(match n {
        "tags_total" => s.tags_total as f64,
        "tags_located" => s.tags_located as f64,
        "tags_lost" => s.tags_lost as f64,
        "infra_online" | "anchors_online" => s.infra_online as f64,
        "infra_total" => s.infra_total as f64,
        "alarms_active" => s.alarms_active as f64,
        "rmse_cm" => (s.rmse_m * 100.0).round() as f64,
        "rmse_m" => s.rmse_m as f64,
        "cep95_cm" => (s.cep95_m * 100.0).round() as f64,
        "rfid_reads_per_s" => s.rfid_reads_per_s as f64,
        "running" => s.running as u8 as f64,
        _ => return None,
    })
}

pub fn channel_url(base: &str, address: &str) -> String {
    format!("{}/rest/channel/{}", base.trim().trim_end_matches('/'), address.trim().trim_start_matches('/'))
}

pub fn auth_header(user: &str, pass: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}")))
}

pub fn run(cfg: OpenEmsCfg, rx: Receiver<Outgoing>, shared: Shared) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .http_status_as_error(false)
        .build()
        .into();
    let auth = auth_header(&cfg.username, &cfg.password);
    let period = Duration::from_secs_f32(cfg.period_s.max(0.1));
    let mut latest: Option<Snapshot> = None;
    // Commands written in OpenEMS (rtls0/SimRunning, SimSpeedX10) act when they change.
    let mut commands: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    // Last value written per channel, and when.
    let mut written: std::collections::HashMap<String, (f64, Instant)> = std::collections::HashMap::new();
    let mut next = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => {
                if let RtlsEvent::Snapshot(s) = &m.event {
                    latest = Some(s.clone());
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(_) => {}
        }
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + period;
        let mut errors = Vec::new();
        let mut ok = 0;
        let t0 = Instant::now();
        if let Some(s) = &latest {
            // OpenEMS applies a REST write on its next cycle (~1 s) and the
            // request blocks until then, so: only send what changed (plus a
            // refresh every 30 s), and send in parallel.
            let mut pending: Vec<(String, f64)> = Vec::new();
            for m in &cfg.write {
                let Some(v) = metric(s, &m.metric) else {
                    errors.push(format!("unknown metric '{}'", m.metric));
                    continue;
                };
                let stale = written.get(&m.address).is_none_or(|(old, at)| *old != v || at.elapsed() > Duration::from_secs(30));
                if stale {
                    pending.push((m.address.clone(), v));
                }
            }
            for chunk in pending.chunks(6) {
                let results: Vec<(String, f64, Result<u16, String>)> = std::thread::scope(|sc| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|(address, v)| {
                            let (agent, auth) = (&agent, &auth);
                            let url = channel_url(&cfg.url, address);
                            let body = json!({ "value": v }).to_string();
                            sc.spawn(move || {
                                let r = agent.post(&url).header("authorization", auth).header("content-type", "application/json").send(body.as_bytes());
                                (address.clone(), *v, r.map(|r| r.status().as_u16()).map_err(|e| e.to_string()))
                            })
                        })
                        .collect();
                    handles.into_iter().filter_map(|h| h.join().ok()).collect()
                });
                let mut unreachable = false;
                for (address, v, r) in results {
                    match r {
                        Ok(code) if (200..300).contains(&code) => {
                            ok += 1;
                            shared.sent(1);
                            written.insert(address, (v, Instant::now()));
                        }
                        Ok(code) => {
                            shared.dropped(1);
                            errors.push(format!("write {address} → HTTP {code}"));
                        }
                        Err(e) => {
                            shared.dropped(1);
                            unreachable |= e.contains("onnection");
                            errors.push(format!("write {address}: {e}"));
                        }
                    }
                }
                if unreachable {
                    break; // don't hammer every channel
                }
            }
        }
        for address in &cfg.read {
            let url = channel_url(&cfg.url, address);
            match agent.get(&url).header("authorization", &auth).call() {
                Ok(mut r) if r.status().is_success() => {
                    ok += 1;
                    let text = r.body_mut().read_to_string().unwrap_or_default();
                    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    // A regex address (rtls0/Actuator.*) answers with a list.
                    let items: Vec<Value> = match v {
                        Value::Array(a) => a,
                        x => vec![x],
                    };
                    for it in &items {
                        let addr = it["address"].as_str().map(str::to_string).unwrap_or_else(|| address.clone());
                        let shown = match &it["value"] {
                            Value::Null if it.get("address").is_none() => text.chars().take(120).collect(),
                            Value::Null => "–".into(),
                            x => format!("{x}{}", it["unit"].as_str().filter(|u| !u.is_empty()).map(|u| format!(" {u}")).unwrap_or_default()),
                        };
                        shared.feedback(Feedback { source: format!("openems:{addr}"), value: shown });
                        if let Some(x) = it["value"].as_f64() {
                            let old = commands.insert(addr.clone(), x);
                            let changed = old.is_some_and(|o| o != x);
                            if changed && addr.ends_with("/SimRunning") {
                                shared.control(Control::Run(x != 0.0));
                            } else if changed && addr.ends_with("/SimSpeedX10") && x > 0.0 {
                                shared.control(Control::Speed(x as f32 / 10.0));
                            } else if (changed || old.is_none())
                                && let Some(n) = addr.rsplit_once("/Actuator").and_then(|(_, n)| n.parse::<u8>().ok())
                            {
                                // Actuators are state, not commands: apply the first value too.
                                shared.control(Control::Actuator(n, x as f32));
                            }
                        }
                    }
                }
                Ok(r) => errors.push(format!("read {address} → HTTP {}", r.status().as_u16())),
                Err(e) => {
                    errors.push(format!("read {address}: {e}"));
                    break;
                }
            }
        }
        if errors.is_empty() {
            if ok > 0 {
                shared.set_rtt(t0.elapsed().as_secs_f32() * 1000.0 / ok as f32);
            }
            shared.set_connected(format!("{} ok ({} writes, {} reads per {:.1}s)", cfg.url, cfg.write.len(), cfg.read.len(), cfg.period_s));
        } else {
            let hint = if errors.iter().any(|e| e.contains("401")) {
                " - check the password/role"
            } else if errors.iter().any(|e| e.contains("404")) {
                " - component/channel not found (or not writable) in OpenEMS"
            } else {
                ""
            };
            shared.set_error(format!("{}{hint}", errors.join("; ")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtls::events::TagRow;

    #[test]
    fn metrics_and_urls() {
        let mut s = Snapshot { tags_located: 4, rmse_m: 0.234, ..Default::default() };
        s.zones.insert("Dock".into(), 2);
        s.tags.push(TagRow { id: "T1".into(), x: 1.5, ..Default::default() });
        assert_eq!(metric(&s, "tags_located"), Some(4.0));
        assert_eq!(metric(&s, "rmse_cm"), Some(23.0));
        assert_eq!(metric(&s, "zone:Dock"), Some(2.0));
        assert_eq!(metric(&s, "tag:T1:x"), Some(1.5));
        assert_eq!(metric(&s, "nope"), None);
        assert_eq!(channel_url("http://h:8084/", "/rtls0/TagsLost"), "http://h:8084/rest/channel/rtls0/TagsLost");
        assert_eq!(auth_header("x", "admin"), "Basic eDphZG1pbg==");
    }

    /// End-to-end against a fake OpenEMS REST endpoint.
    #[test]
    fn writes_and_reads_back() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let (seen_tx, seen_rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let auth = req.headers().iter().find(|h| h.field.equiv("authorization")).map(|h| h.value.to_string()).unwrap_or_default();
                let _ = seen_tx.send((req.method().to_string(), req.url().to_string(), body, auth));
                let resp = if req.method() == &tiny_http::Method::Get {
                    tiny_http::Response::from_string(r#"{"address":"_sum/EssSoc","value":57,"unit":"%"}"#)
                } else {
                    tiny_http::Response::from_string("{}")
                };
                let _ = req.respond(resp);
            }
        });
        let cfg = OpenEmsCfg {
            enabled: true,
            url: format!("http://127.0.0.1:{port}"),
            period_s: 0.1,
            write: vec![super::super::ChannelMap { metric: "tags_lost".into(), address: "rtls0/TagsLost".into() }],
            read: vec!["_sum/EssSoc".into()],
            ..Default::default()
        };
        let (tx, rx) = crossbeam_channel::bounded(10);
        let (ctl, _ctl_rx) = crossbeam_channel::unbounded();
        let shared = Shared::new(ctl, "test");
        let sh = shared.clone();
        let h = std::thread::spawn(move || run(cfg, rx, sh));
        let snap = Snapshot { tags_lost: 3, ..Default::default() };
        let ev = RtlsEvent::Snapshot(snap);
        tx.send(std::sync::Arc::new(super::super::Message { topic: ev.topic(), payload: String::new(), retain: false, event: ev })).unwrap();
        let mut got_post = false;
        let mut got_get = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(got_post && got_get) {
            if let Ok((m, url, body, auth)) = seen_rx.recv_timeout(Duration::from_millis(200)) {
                assert_eq!(auth, "Basic eDphZG1pbg==");
                if m == "POST" && url == "/rest/channel/rtls0/TagsLost" {
                    assert!(body.contains("\"value\":3"), "{body}");
                    got_post = true;
                }
                if m == "GET" && url == "/rest/channel/_sum/EssSoc" {
                    got_get = true;
                }
            }
        }
        assert!(got_post && got_get);
        std::thread::sleep(Duration::from_millis(200));
        let fb = shared.0.feedback.lock().unwrap().get("openems:_sum/EssSoc").map(|(v, _)| v.clone());
        assert_eq!(fb.as_deref(), Some("57 %"));
        drop(tx);
        h.join().unwrap();
    }
}
