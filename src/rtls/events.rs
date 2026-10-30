//! Events produced by the simulator: the wire format for every output
//! (MQTT, REST webhook, UDP, recordings). JSON, tagged by `type`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RtlsEvent {
    /// A solved tag position. `error_m`/`truth` are simulator ground truth.
    Position {
        ts: f64,
        tag: String,
        tech: String,
        x: f32,
        y: f32,
        z: f32,
        /// 0..1 (from residual and DOP).
        quality: f32,
        anchors: u32,
        dop: f32,
        error_m: Option<f32>,
        truth: Option<[f32; 3]>,
    },
    /// UWB two-way range.
    Range { ts: f64, tag: String, anchor: String, range_m: f32, los: bool, rssi_dbm: f32 },
    /// UWB time difference of arrival vs. `reference`, in metres.
    Tdoa { ts: f64, tag: String, anchor: String, reference: String, diff_m: f32, los: bool },
    /// BLE RSSI between a node and a tag.
    Rssi { ts: f64, tag: String, node: String, rssi_dbm: f32, los: bool },
    RfidRead { ts: f64, reader: String, tag: String, epc: String, rssi_dbm: f32, phase_deg: f32, count: u32 },
    Zone { ts: f64, tag: String, zone: String, enter: bool },
    Alarm { ts: f64, kind: String, source: String, message: String, active: bool },
    DeviceStatus { ts: f64, device: String, kind: String, online: bool, links: u32 },
    Snapshot(Snapshot),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Snapshot {
    pub ts: f64,
    pub running: bool,
    pub speed: f32,
    pub tags_total: u32,
    pub tags_located: u32,
    pub tags_lost: u32,
    pub infra_online: u32,
    pub infra_total: u32,
    pub alarms_active: u32,
    pub rmse_m: f32,
    pub cep95_m: f32,
    pub rfid_reads_per_s: f32,
    pub zones: BTreeMap<String, u32>,
    pub tags: Vec<TagRow>,
    /// RF analysis: signal strength, link quality, NLOS, anchor health, coverage.
    #[serde(default)]
    pub rf: super::rf::RfSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct TagRow {
    pub id: String,
    pub tech: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub quality: f32,
    pub age_s: f32,
    pub lost: bool,
    pub restricted: bool,
}

/// Coarse class used by per-output filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventClass {
    Positions,
    Measurements,
    Rfid,
    Zones,
    Alarms,
    Status,
    Snapshots,
}

impl RtlsEvent {
    pub fn class(&self) -> EventClass {
        match self {
            Self::Position { .. } => EventClass::Positions,
            Self::Range { .. } | Self::Tdoa { .. } | Self::Rssi { .. } => EventClass::Measurements,
            Self::RfidRead { .. } => EventClass::Rfid,
            Self::Zone { .. } => EventClass::Zones,
            Self::Alarm { .. } => EventClass::Alarms,
            Self::DeviceStatus { .. } => EventClass::Status,
            Self::Snapshot(_) => EventClass::Snapshots,
        }
    }

    /// MQTT topic suffix (after the configured prefix).
    pub fn topic(&self) -> String {
        match self {
            Self::Position { tag, .. } => format!("tag/{tag}/position"),
            Self::Range { tag, .. } => format!("tag/{tag}/range"),
            Self::Tdoa { tag, .. } => format!("tag/{tag}/tdoa"),
            Self::Rssi { tag, .. } => format!("tag/{tag}/rssi"),
            Self::RfidRead { reader, .. } => format!("rfid/{reader}/read"),
            Self::Zone { zone, .. } => format!("zone/{zone}/event"),
            Self::Alarm { .. } => "alarm".into(),
            Self::DeviceStatus { device, .. } => format!("device/{device}/status"),
            Self::Snapshot(_) => "snapshot".into(),
        }
    }

    /// Retained on the broker (latest state per topic).
    pub fn retain(&self) -> bool {
        matches!(self, Self::Position { .. } | Self::DeviceStatus { .. } | Self::Snapshot(_))
    }

    /// One CSV line: `type,ts,source,target,value1,value2,value3`.
    pub fn csv(&self) -> String {
        match self {
            Self::Position { ts, tag, x, y, z, .. } => format!("position,{ts:.3},{tag},,{x:.3},{y:.3},{z:.3}"),
            Self::Range { ts, tag, anchor, range_m, los, .. } => format!("range,{ts:.3},{tag},{anchor},{range_m:.3},{},", *los as u8),
            Self::Tdoa { ts, tag, anchor, reference, diff_m, .. } => format!("tdoa,{ts:.3},{tag},{anchor},{diff_m:.3},{reference},"),
            Self::Rssi { ts, tag, node, rssi_dbm, .. } => format!("rssi,{ts:.3},{tag},{node},{rssi_dbm:.1},,"),
            Self::RfidRead { ts, reader, epc, rssi_dbm, count, .. } => format!("rfid,{ts:.3},{reader},{epc},{rssi_dbm:.1},{count},"),
            Self::Zone { ts, tag, zone, enter } => format!("zone,{ts:.3},{tag},{zone},{},,", *enter as u8),
            Self::Alarm { ts, kind, source, active, .. } => format!("alarm,{ts:.3},{source},{kind},{},,", *active as u8),
            Self::DeviceStatus { ts, device, online, links, .. } => format!("status,{ts:.3},{device},,{},{links},", *online as u8),
            Self::Snapshot(s) => format!("snapshot,{:.3},,,{},{},{:.3}", s.ts, s.tags_located, s.tags_lost, s.rmse_m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_is_tagged() {
        let e = RtlsEvent::RfidRead { ts: 1.5, reader: "R1".into(), tag: "E1".into(), epc: "3034".into(), rssi_dbm: -60.0, phase_deg: 12.0, count: 2 };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"rfid_read\""), "{s}");
        let back: RtlsEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);
        let snap = RtlsEvent::Snapshot(Snapshot { ts: 2.0, tags_total: 3, ..Default::default() });
        let s = serde_json::to_string(&snap).unwrap();
        assert!(s.contains("\"type\":\"snapshot\"") && s.contains("\"tags_total\":3"), "{s}");
        assert_eq!(serde_json::from_str::<RtlsEvent>(&s).unwrap(), snap);
    }
}
