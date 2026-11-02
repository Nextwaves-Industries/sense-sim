//! Outputs: every sink runs on its own thread, fed through a bounded channel
//! so a slow or offline target never stalls the simulation. Sinks report
//! status and any values read back from the target (the EMS "reaction").

pub mod modbus;
pub mod mqtt;
pub mod openems;

use std::collections::BTreeMap;
use std::io::Write;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::prelude::Resource;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded, unbounded};
use serde::{Deserialize, Serialize};

use super::events::{EventClass, RtlsEvent};

// ---------------------------------------------------------------------------
// Configuration

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EventFilter {
    pub positions: bool,
    pub measurements: bool,
    pub rfid: bool,
    pub zones: bool,
    pub alarms: bool,
    pub status: bool,
    pub snapshots: bool,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self { positions: true, measurements: false, rfid: true, zones: true, alarms: true, status: true, snapshots: true }
    }
}

impl EventFilter {
    pub fn all() -> Self {
        Self { positions: true, measurements: true, rfid: true, zones: true, alarms: true, status: true, snapshots: true }
    }
    pub fn allows(&self, c: EventClass) -> bool {
        match c {
            EventClass::Positions => self.positions,
            EventClass::Measurements => self.measurements,
            EventClass::Rfid => self.rfid,
            EventClass::Zones => self.zones,
            EventClass::Alarms => self.alarms,
            EventClass::Status => self.status,
            EventClass::Snapshots => self.snapshots,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MqttCfg {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub username: String,
    pub password: String,
    /// Topics are `<prefix>/tag/<id>/position`, `<prefix>/snapshot`, …
    pub prefix: String,
    pub qos: u8,
    pub retain: bool,
    /// Extra topic filter to listen to (e.g. what OpenEMS publishes back).
    pub subscribe: String,
    pub filter: EventFilter,
}

impl Default for MqttCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 1883,
            client_id: String::new(),
            username: String::new(),
            password: String::new(),
            prefix: "nextwaves/rtls/site1".into(),
            qos: 0,
            retain: true,
            subscribe: "openems/#".into(),
            filter: EventFilter::default(),
        }
    }
}

impl MqttCfg {
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host.trim(), self.port)
    }
}

/// One simulator metric written to one OpenEMS channel (`Component/Channel`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelMap {
    pub metric: String,
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OpenEmsCfg {
    pub enabled: bool,
    /// OpenEMS Edge with Controller.Api.Rest.ReadWrite, e.g. http://192.168.1.10:8084
    pub url: String,
    pub username: String,
    pub password: String,
    pub period_s: f32,
    pub write: Vec<ChannelMap>,
    /// Channels read back every period and shown as the EMS reaction.
    pub read: Vec<String>,
}

impl Default for OpenEmsCfg {
    fn default() -> Self {
        let m = |metric: &str, ch: &str| ChannelMap { metric: metric.into(), address: format!("rtls0/{ch}") };
        Self {
            enabled: false,
            url: "http://127.0.0.1:8084".into(),
            username: "x".into(),
            password: "admin".into(),
            period_s: 1.0,
            write: vec![
                m("tags_located", "TagsLocated"),
                m("tags_lost", "TagsLost"),
                m("alarms_active", "AlarmsActive"),
                m("infra_online", "AnchorsOnline"),
                m("rmse_cm", "RmseCm"),
            ],
            read: vec!["_sum/State".into(), "_sum/GridActivePower".into(), "_sum/EssSoc".into()],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebhookCfg {
    pub enabled: bool,
    pub url: String,
    /// `Header: value` per line.
    pub headers: String,
    pub batch_ms: u64,
    pub filter: EventFilter,
}

impl Default for WebhookCfg {
    fn default() -> Self {
        Self { enabled: false, url: "http://127.0.0.1:8080/rtls/events".into(), headers: String::new(), batch_ms: 500, filter: EventFilter::default() }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UdpFormat {
    #[default]
    Json,
    Csv,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UdpCfg {
    pub enabled: bool,
    pub target: String,
    pub format: UdpFormat,
    pub filter: EventFilter,
}

impl Default for UdpCfg {
    fn default() -> Self {
        Self { enabled: false, target: "127.0.0.1:9100".into(), format: UdpFormat::Json, filter: EventFilter::all() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModbusCfg {
    pub enabled: bool,
    pub bind: String,
}

impl Default for ModbusCfg {
    fn default() -> Self {
        Self { enabled: false, bind: "0.0.0.0:5020".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RecorderCfg {
    pub enabled: bool,
    pub path: String,
    pub filter: EventFilter,
}

impl Default for RecorderCfg {
    fn default() -> Self {
        let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        Self { enabled: false, path: format!("{}/SenseSim/rtls-recording.jsonl", base.replace('\\', "/")), filter: EventFilter::all() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct OutputsConfig {
    pub mqtt: MqttCfg,
    pub openems: OpenEmsCfg,
    pub webhook: WebhookCfg,
    pub udp: UdpCfg,
    pub modbus: ModbusCfg,
    pub recorder: RecorderCfg,
}

// ---------------------------------------------------------------------------
// Messages, status, feedback, control

pub struct Message {
    pub topic: String,
    pub payload: String,
    pub retain: bool,
    pub event: RtlsEvent,
}

pub type Outgoing = Arc<Message>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Connected,
    Error,
}

#[derive(Clone, Debug)]
pub struct SinkStatus {
    pub state: LinkState,
    pub detail: String,
    pub sent: u64,
    pub dropped: u64,
    pub last_error: String,
    pub rtt_ms: Option<f32>,
}

impl SinkStatus {
    pub fn connecting(target: &str) -> Self {
        Self { state: LinkState::Connecting, detail: format!("connecting to {target}…"), sent: 0, dropped: 0, last_error: String::new(), rtt_ms: None }
    }
}

#[derive(Clone, Debug)]
pub struct Feedback {
    pub source: String,
    pub value: String,
}

/// Commands an external system may send back (MQTT `<prefix>/control`,
/// Modbus control registers).
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Run(bool),
    Speed(f32),
    ResetMetrics,
    /// OpenEMS actuator `n` (1..8, zone n) set to 0..100 %.
    Actuator(u8, f32),
}

pub struct SharedInner {
    pub status: Mutex<SinkStatus>,
    pub feedback: Mutex<BTreeMap<String, (String, Instant)>>,
    control: Sender<Control>,
}

#[derive(Clone)]
pub struct Shared(pub Arc<SharedInner>);

impl Shared {
    fn new(control: Sender<Control>, target: &str) -> Self {
        Self(Arc::new(SharedInner {
            status: Mutex::new(SinkStatus::connecting(target)),
            feedback: Mutex::new(BTreeMap::new()),
            control,
        }))
    }
    pub fn status(&self) -> SinkStatus {
        self.0.status.lock().unwrap().clone()
    }
    pub fn set_state(&self, s: SinkStatus) {
        let mut st = self.0.status.lock().unwrap();
        let (sent, dropped, err) = (st.sent, st.dropped, st.last_error.clone());
        *st = SinkStatus { sent, dropped, last_error: err, ..s };
    }
    pub fn set_connected(&self, detail: String) {
        let mut st = self.0.status.lock().unwrap();
        st.state = LinkState::Connected;
        st.detail = detail;
    }
    pub fn set_error(&self, e: String) {
        let mut st = self.0.status.lock().unwrap();
        st.state = LinkState::Error;
        st.detail = e.clone();
        st.last_error = e;
    }
    pub fn set_rtt(&self, ms: f32) {
        self.0.status.lock().unwrap().rtt_ms = Some(ms);
    }
    pub fn sent(&self, n: u64) {
        self.0.status.lock().unwrap().sent += n;
    }
    pub fn dropped(&self, n: u64) {
        self.0.status.lock().unwrap().dropped += n;
    }
    pub fn feedback(&self, f: Feedback) {
        let mut m = self.0.feedback.lock().unwrap();
        if m.len() > 200 && !m.contains_key(&f.source) {
            return;
        }
        m.insert(f.source, (f.value, Instant::now()));
    }
    pub fn control(&self, c: Control) {
        let _ = self.0.control.send(c);
    }
    /// `{"cmd":"pause"}`, `{"cmd":"run"}`, `{"cmd":"speed","value":2}`, `{"cmd":"reset"}`.
    pub fn control_json(&self, text: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
        match v["cmd"].as_str().unwrap_or("") {
            "run" | "start" | "play" => self.control(Control::Run(true)),
            "pause" | "stop" => self.control(Control::Run(false)),
            "speed" => {
                if let Some(s) = v["value"].as_f64() {
                    self.control(Control::Speed(s as f32));
                }
            }
            "reset" => self.control(Control::ResetMetrics),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Hub

pub struct Sink {
    pub name: &'static str,
    tx: Sender<Outgoing>,
    filter: Option<EventFilter>,
    pub shared: Shared,
}

#[derive(Resource)]
pub struct Hub {
    pub sinks: Vec<Sink>,
    applied: Option<OutputsConfig>,
    control_tx: Sender<Control>,
    pub control_rx: Receiver<Control>,
    pub published: u64,
    /// Nothing goes out: a test run isn't the live site.
    pub muted: bool,
}

impl Default for Hub {
    fn default() -> Self {
        let (control_tx, control_rx) = unbounded();
        Self { sinks: Vec::new(), applied: None, control_tx, control_rx, published: 0, muted: false }
    }
}

impl Hub {
    /// Reconnects every sink even if the configuration is unchanged.
    pub fn restart(&mut self, cfg: &OutputsConfig) {
        self.applied = None;
        self.apply(cfg);
    }

    /// (Re)starts the sinks when the configuration changed.
    pub fn apply(&mut self, cfg: &OutputsConfig) {
        if self.applied.as_ref() == Some(cfg) {
            return;
        }
        // Dropping the senders ends the old threads.
        self.sinks.clear();
        self.applied = Some(cfg.clone());
        let ctl = self.control_tx.clone();
        let mut add = |name: &'static str, target: &str, filter: Option<EventFilter>, run: Box<dyn FnOnce(Receiver<Outgoing>, Shared) + Send>| {
            let (tx, rx) = bounded(20_000);
            let shared = Shared::new(ctl.clone(), target);
            let s2 = shared.clone();
            let _ = std::thread::Builder::new().name(format!("rtls-{name}")).spawn(move || run(rx, s2));
            self.sinks.push(Sink { name, tx, filter, shared });
        };
        if cfg.mqtt.enabled {
            let c = cfg.mqtt.clone();
            add("MQTT", &c.host_port(), Some(c.filter.clone()), Box::new(move |rx, sh| mqtt::run(c, rx, sh)));
        }
        if cfg.openems.enabled {
            let c = cfg.openems.clone();
            let f = EventFilter { snapshots: true, ..none() };
            add("OpenEMS REST", &c.url.clone(), Some(f), Box::new(move |rx, sh| openems::run(c, rx, sh)));
        }
        if cfg.modbus.enabled {
            let c = cfg.modbus.clone();
            let f = EventFilter { snapshots: true, ..none() };
            add("Modbus/TCP", &c.bind.clone(), Some(f), Box::new(move |rx, sh| modbus::run(c, rx, sh)));
        }
        if cfg.webhook.enabled {
            let c = cfg.webhook.clone();
            add("Webhook", &c.url.clone(), Some(c.filter.clone()), Box::new(move |rx, sh| run_webhook(c, rx, sh)));
        }
        if cfg.udp.enabled {
            let c = cfg.udp.clone();
            add("UDP", &c.target.clone(), Some(c.filter.clone()), Box::new(move |rx, sh| run_udp(c, rx, sh)));
        }
        if cfg.recorder.enabled {
            let c = cfg.recorder.clone();
            add("Recorder", &c.path.clone(), Some(c.filter.clone()), Box::new(move |rx, sh| run_recorder(c, rx, sh)));
        }
    }

    pub fn publish(&mut self, event: &RtlsEvent) {
        if self.muted || self.sinks.is_empty() {
            return;
        }
        let class = event.class();
        let wanted: Vec<&Sink> = self.sinks.iter().filter(|s| s.filter.as_ref().is_none_or(|f| f.allows(class))).collect();
        if wanted.is_empty() {
            return;
        }
        let msg = Arc::new(Message {
            topic: event.topic(),
            payload: serde_json::to_string(event).unwrap_or_default(),
            retain: event.retain(),
            event: event.clone(),
        });
        self.published += 1;
        for s in wanted {
            if let Err(TrySendError::Full(_)) = s.tx.try_send(msg.clone()) {
                s.shared.dropped(1);
            }
        }
    }

    /// Latest values every sink has read back, newest first per source.
    pub fn feedback(&self) -> Vec<(String, String, f32)> {
        let mut v = Vec::new();
        for s in &self.sinks {
            for (k, (val, t)) in s.shared.0.feedback.lock().unwrap().iter() {
                v.push((k.clone(), val.clone(), t.elapsed().as_secs_f32()));
            }
        }
        v
    }
}

fn none() -> EventFilter {
    EventFilter { positions: false, measurements: false, rfid: false, zones: false, alarms: false, status: false, snapshots: false }
}

// ---------------------------------------------------------------------------
// Simple sinks

fn run_udp(cfg: UdpCfg, rx: Receiver<Outgoing>, shared: Shared) {
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => return shared.set_error(format!("UDP bind failed: {e}")),
    };
    if let Err(e) = sock.connect(cfg.target.trim()) {
        return shared.set_error(format!("UDP target {}: {e}", cfg.target));
    }
    shared.set_connected(format!("sending to {}", cfg.target));
    for m in rx.iter() {
        let line = match cfg.format {
            UdpFormat::Json => m.payload.clone(),
            UdpFormat::Csv => m.event.csv(),
        };
        match sock.send(line.as_bytes()) {
            Ok(_) => shared.sent(1),
            // Nobody listening (ICMP port unreachable) is not fatal for UDP.
            Err(e) => {
                shared.dropped(1);
                shared.0.status.lock().unwrap().last_error = e.to_string();
            }
        }
    }
}

fn run_recorder(cfg: RecorderCfg, rx: Receiver<Outgoing>, shared: Shared) {
    let path = std::path::PathBuf::from(cfg.path.trim());
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let file = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(e) => return shared.set_error(format!("cannot open {}: {e}", path.display())),
    };
    let mut w = std::io::BufWriter::new(file);
    shared.set_connected(format!("recording to {}", path.display()));
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => {
                if writeln!(w, "{}", m.payload).is_ok() {
                    shared.sent(1);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = w.flush();
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = w.flush();
                return;
            }
        }
    }
}

fn run_webhook(cfg: WebhookCfg, rx: Receiver<Outgoing>, shared: Shared) {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).http_status_as_error(false).build().into();
    let headers: Vec<(String, String)> = cfg
        .headers
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    shared.set_connected(format!("posting to {}", cfg.url));
    let batch_window = Duration::from_millis(cfg.batch_ms.max(10));
    loop {
        let first = match rx.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut batch = vec![first];
        let until = Instant::now() + batch_window;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(m) => batch.push(m),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if batch.len() >= 1000 {
                break;
            }
        }
        let body = format!("[{}]", batch.iter().map(|m| m.payload.as_str()).collect::<Vec<_>>().join(","));
        let mut req = agent.post(cfg.url.trim()).header("content-type", "application/json");
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let t0 = Instant::now();
        match req.send(body.as_bytes()) {
            Ok(resp) if resp.status().is_success() => {
                shared.sent(batch.len() as u64);
                shared.set_rtt(t0.elapsed().as_secs_f32() * 1000.0);
                shared.set_connected(format!("posting to {} (HTTP {})", cfg.url, resp.status().as_u16()));
            }
            Ok(resp) => {
                shared.dropped(batch.len() as u64);
                shared.set_error(format!("HTTP {} from {}", resp.status().as_u16(), cfg.url));
            }
            Err(e) => {
                shared.dropped(batch.len() as u64);
                shared.set_error(format!("{}: {e}", cfg.url));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use super::*;
    use crate::rtls::events::{Snapshot, TagRow};

    fn read_packet(s: &mut TcpStream, buf: &mut Vec<u8>) -> (u8, Vec<u8>) {
        let mut tmp = [0u8; 1024];
        loop {
            if let Some((first, body, used)) = mqtt::parse_frame(buf) {
                let out = (first, body.to_vec());
                buf.drain(..used);
                return out;
            }
            let n = s.read(&mut tmp).unwrap();
            assert!(n > 0, "client closed");
            buf.extend_from_slice(&tmp[..n]);
        }
    }

    /// The MQTT sink against a tiny fake broker: connect, subscribe,
    /// publish, and a control message coming back.
    #[test]
    fn mqtt_end_to_end_with_fake_broker() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut hub = Hub::default();
        let cfg = OutputsConfig {
            mqtt: MqttCfg { enabled: true, port, prefix: "nw/site".into(), subscribe: "openems/#".into(), username: "u".into(), password: "p".into(), ..Default::default() },
            ..Default::default()
        };
        hub.apply(&cfg);
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = Vec::new();
        let (first, _) = read_packet(&mut s, &mut buf);
        assert_eq!(first, 0x10, "CONNECT");
        s.write_all(&[0x20, 2, 0, 0]).unwrap();
        let mut filters = Vec::new();
        for _ in 0..2 {
            let (first, body) = read_packet(&mut s, &mut buf);
            assert_eq!(first, 0x82, "SUBSCRIBE");
            let l = u16::from_be_bytes([body[2], body[3]]) as usize;
            filters.push(String::from_utf8(body[4..4 + l].to_vec()).unwrap());
        }
        assert!(filters.contains(&"openems/#".to_string()) && filters.contains(&"nw/site/control".to_string()), "{filters:?}");
        // Wait until the sink reports connected, then publish.
        let deadline = Instant::now() + Duration::from_secs(3);
        while hub.sinks[0].shared.status().state != LinkState::Connected && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let snap = Snapshot { tags_total: 2, tags: vec![TagRow { id: "T1".into(), ..Default::default() }], ..Default::default() };
        hub.publish(&RtlsEvent::Snapshot(snap));
        let (first, body) = read_packet(&mut s, &mut buf);
        assert_eq!(first >> 4, 3, "PUBLISH");
        let (topic, payload, _) = mqtt::parse_publish(first, &body).unwrap();
        assert_eq!(topic, "nw/site/snapshot");
        assert_eq!(first & 1, 1, "snapshot is retained");
        let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(v["type"], "snapshot");
        assert_eq!(v["tags_total"], 2);
        // The EMS answers on the control topic and on its own topic.
        s.write_all(&mqtt::publish_packet("nw/site/control", br#"{"cmd":"speed","value":4}"#, 0, false, 0)).unwrap();
        s.write_all(&mqtt::publish_packet("openems/_sum/EssSoc", b"57", 0, false, 0)).unwrap();
        let c = hub.control_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(c, Control::Speed(4.0));
        std::thread::sleep(Duration::from_millis(200));
        assert!(hub.feedback().iter().any(|(k, v, _)| k == "mqtt:openems/_sum/EssSoc" && v == "57"));
        // Measurements are filtered out by default.
        hub.publish(&RtlsEvent::Range { ts: 0.0, tag: "T1".into(), anchor: "A1".into(), range_m: 1.0, los: true, rssi_dbm: -80.0 });
        hub.publish(&RtlsEvent::Alarm { ts: 1.0, kind: "tag_lost".into(), source: "T1".into(), message: "x".into(), active: true });
        let (first, body) = read_packet(&mut s, &mut buf);
        let (topic, _, _) = mqtt::parse_publish(first, &body).unwrap();
        assert_eq!(topic, "nw/site/alarm");
    }

    #[test]
    fn modbus_server_over_tcp() {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let mut hub = Hub::default();
        hub.apply(&OutputsConfig { modbus: ModbusCfg { enabled: true, bind: format!("127.0.0.1:{port}") }, ..Default::default() });
        hub.publish(&RtlsEvent::Snapshot(Snapshot { tags_total: 7, tags_lost: 2, ..Default::default() }));
        let mut s = None;
        for _ in 0..50 {
            if let Ok(c) = TcpStream::connect(("127.0.0.1", port)) {
                s = Some(c);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut s = s.expect("modbus server not listening");
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        s.write_all(&[0, 1, 0, 0, 0, 6, 1, 3, 0, 0, 0, 4]).unwrap();
        let mut r = [0u8; 17];
        s.read_exact(&mut r).unwrap();
        assert_eq!(r[7], 3);
        assert_eq!(u16::from_be_bytes([r[11], r[12]]), 7, "tags total");
        assert_eq!(u16::from_be_bytes([r[15], r[16]]), 2, "tags lost");
        // Pause the simulation from the PLC side.
        s.write_all(&[0, 2, 0, 0, 0, 6, 1, 6, 0x03, 0xE8, 0, 0]).unwrap();
        let mut r = [0u8; 12];
        s.read_exact(&mut r).unwrap();
        assert_eq!(hub.control_rx.recv_timeout(Duration::from_secs(2)).unwrap(), Control::Run(false));
    }

    #[test]
    fn udp_csv_stream() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut hub = Hub::default();
        hub.apply(&OutputsConfig {
            udp: UdpCfg { enabled: true, target: rx.local_addr().unwrap().to_string(), format: UdpFormat::Csv, filter: EventFilter::all() },
            ..Default::default()
        });
        std::thread::sleep(Duration::from_millis(200));
        hub.publish(&RtlsEvent::Range { ts: 1.5, tag: "T1".into(), anchor: "A2".into(), range_m: 4.25, los: false, rssi_dbm: -85.0 });
        let mut b = [0u8; 512];
        let n = rx.recv(&mut b).unwrap();
        assert_eq!(std::str::from_utf8(&b[..n]).unwrap(), "range,1.500,T1,A2,4.250,0,");
    }
}
