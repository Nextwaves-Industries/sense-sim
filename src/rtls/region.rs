//! Regulatory regions: which UWB channels and UHF RFID band and power a
//! country allows. A region constrains the device profiles, so the simulation,
//! coverage maps and auto deploy all use legal values. The table is data
//! (`assets/regions/regions.json`), and JSON files in the user's `regions`
//! folder add countries or replace built-in ones with the same id.
//!
//! A rule that could not be confirmed changes nothing and says so: an invented
//! limit would be worse than a visible gap.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::device::{DeviceKind, Profile};

/// Readers using a saved FDTD pattern keep their power: only the pattern knows its peak gain.
pub const PATTERN_NOTE: &str = "RFID readers using a saved FDTD antenna pattern are not capped. Check their EIRP against the limit by hand.";

/// UWB channel centre frequencies (MHz), IEEE 802.15.4.
pub const UWB_CHANNELS: [(u32, f32); 2] = [(5, 6489.6), (9, 7987.2)];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Read in the regulator's own text or an exact reproduction.
    Verified,
    /// From a third-party summary: apply it, but confirm it before relying on it.
    Secondary,
    /// Not confirmed: nothing is applied.
    #[default]
    Unverified,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Secondary => "secondary source",
            Self::Unverified => "not verified",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UwbRule {
    pub status: Status,
    pub channels: Vec<u32>,
    pub max_dbm_mhz: Option<f32>,
    pub note: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RfidRule {
    pub status: Status,
    pub band_mhz: Option<[f32; 2]>,
    pub max_eirp_dbm: Option<f32>,
    pub max_conducted_dbm: Option<f32>,
    pub limit_text: String,
    pub note: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub checked: String,
    pub uwb: UwbRule,
    pub rfid: RfidRule,
}

impl Region {
    /// A UWB frequency this region allows: `freq` if its channel is allowed,
    /// otherwise the first allowed channel the app knows.
    fn uwb_freq(&self, freq: f32) -> f32 {
        let allowed = |f: f32| UWB_CHANNELS.iter().any(|(c, cf)| *cf == f && self.uwb.channels.contains(c));
        if allowed(freq) {
            return freq;
        }
        self.uwb.channels.iter().find_map(|c| UWB_CHANNELS.iter().find(|(k, _)| k == c).map(|(_, f)| *f)).unwrap_or(freq)
    }

    fn rfid_freq(&self, freq: f32) -> f32 {
        match self.rfid.band_mhz {
            Some([lo, hi]) if freq < lo || freq > hi => (lo + hi) / 2.0,
            _ => freq,
        }
    }

    /// Conducted reader power that keeps this antenna gain within the limit.
    pub fn rfid_tx(&self, tx_dbm: f32, gain_dbi: f32) -> f32 {
        let eirp = self.rfid.max_eirp_dbm.map_or(tx_dbm, |limit| tx_dbm.min(limit - gain_dbi));
        self.conducted_limit_dbm().map_or(eirp, |limit| eirp.min(limit))
    }

    /// The conducted limit where the rule has one as well as its EIRP limit, as its
    /// `limit_text` states it. `max_conducted_dbm` in the table overrides this.
    pub fn conducted_limit_dbm(&self) -> Option<f32> {
        if self.rfid.status == Status::Unverified {
            return None;
        }
        self.rfid.max_conducted_dbm.or(match self.id.as_str() {
            "us" | "ca" | "hk" | "mx" => Some(30.0),
            "jp" => Some(24.0),
            _ => None,
        })
    }

    pub fn uwb_tx(&self, dbm_mhz: f32) -> f32 {
        self.uwb.max_dbm_mhz.map_or(dbm_mhz, |limit| dbm_mhz.min(limit))
    }

    /// The profile with this region's rules applied.
    pub fn constrain(&self, p: &Profile) -> Profile {
        let mut out = p.clone();
        match p.kind {
            DeviceKind::UwbAnchor | DeviceKind::UwbTag => {
                out.freq_mhz = self.uwb_freq(p.freq_mhz);
                out.uwb_tx_dbm_mhz = self.uwb_tx(p.uwb_tx_dbm_mhz);
            }
            DeviceKind::RfidReader => {
                out.freq_mhz = self.rfid_freq(p.freq_mhz);
                out.tx_power_dbm = self.rfid_tx(p.tx_power_dbm, p.antenna_gain_dbi);
            }
            _ => {}
        }
        out
    }

    /// What the rules changed in these profiles, for people and the assistant.
    pub fn changes(&self, base: &[Profile]) -> Vec<String> {
        let mut out = Vec::new();
        for p in base {
            let c = self.constrain(p);
            if c.freq_mhz != p.freq_mhz {
                out.push(format!("{}: frequency {:.1} to {:.1} MHz", p.id, p.freq_mhz, c.freq_mhz));
            }
            if c.tx_power_dbm != p.tx_power_dbm {
                out.push(format!(
                    "{}: conducted power {:.1} to {:.2} dBm ({:.2} dBm EIRP with {:.1} dBi)",
                    p.id,
                    p.tx_power_dbm,
                    c.tx_power_dbm,
                    c.tx_power_dbm + p.antenna_gain_dbi,
                    p.antenna_gain_dbi
                ));
            }
            if c.uwb_tx_dbm_mhz != p.uwb_tx_dbm_mhz {
                out.push(format!("{}: UWB power {:.1} to {:.1} dBm/MHz", p.id, p.uwb_tx_dbm_mhz, c.uwb_tx_dbm_mhz));
            }
        }
        out
    }

    /// Everything a user must know before relying on this region's results.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.uwb.status {
            Status::Unverified => out.push(format!("UWB rules for {} are not verified, so UWB values are unchanged. Check them with the regulator.", self.name)),
            Status::Secondary => out.push(format!("UWB rules for {} come from a secondary source. Confirm them with the regulator.", self.name)),
            Status::Verified => {}
        }
        if self.uwb.status != Status::Unverified && self.uwb.max_dbm_mhz.is_none() {
            out.push(format!("No UWB power limit is confirmed for {}, so UWB power is unchanged.", self.name));
        }
        match self.rfid.status {
            Status::Unverified => out.push(format!("RFID rules for {} are not verified, so RFID values are unchanged. Check them with the regulator.", self.name)),
            Status::Secondary => out.push(format!("RFID rules for {} come from a secondary source. Confirm them with the regulator.", self.name)),
            Status::Verified => {}
        }
        out
    }

    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "checked": self.checked,
            "uwb": self.uwb,
            "rfid": self.rfid,
            "warnings": self.warnings(),
        })
    }
}

/// Folders scanned for extra regions (`*.json`, a list or one object).
pub fn user_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        v.push(d.join("regions"));
    }
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    v.push(PathBuf::from(base).join("SenseSim").join("regions"));
    v
}

fn merge(list: &mut Vec<Region>, more: Vec<Region>) {
    for r in more {
        if r.id.is_empty() {
            continue;
        }
        list.retain(|x| x.id != r.id);
        list.push(r);
    }
}

pub fn load() -> Vec<Region> {
    let mut list: Vec<Region> = serde_json::from_str(include_str!("../../assets/regions/regions.json")).unwrap_or_default();
    for dir in user_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Ok(txt) = std::fs::read_to_string(&p) else { continue };
            let parsed: Vec<Region> = serde_json::from_str(&txt).or_else(|_| serde_json::from_str::<Region>(&txt).map(|r| vec![r])).unwrap_or_default();
            merge(&mut list, parsed);
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// The regions and the active one, with the profiles as they were before any rule.
#[derive(Default)]
pub struct RegionState {
    pub regions: Vec<Region>,
    pub active: Option<Region>,
    pub base: Vec<Profile>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Vec<Region> {
        serde_json::from_str(include_str!("../../assets/regions/regions.json")).unwrap()
    }

    fn region(id: &str) -> Region {
        builtin().into_iter().find(|r| r.id == id).unwrap()
    }

    fn reader(tx: f32, gain: f32, freq: f32) -> Profile {
        Profile { id: "r".into(), kind: DeviceKind::RfidReader, tx_power_dbm: tx, antenna_gain_dbi: gain, freq_mhz: freq, ..Default::default() }
    }

    fn anchor(freq: f32) -> Profile {
        Profile { id: "a".into(), kind: DeviceKind::UwbAnchor, freq_mhz: freq, ..Default::default() }
    }

    #[test]
    fn table_parses_and_ids_are_unique() {
        let list = builtin();
        assert!(list.len() >= 10);
        let mut ids: Vec<_> = list.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), list.len());
    }

    /// Every number must carry its origin, and an unverified rule must not carry numbers.
    #[test]
    fn table_is_traceable() {
        for r in builtin() {
            assert!(!r.checked.is_empty(), "{}", r.id);
            if r.uwb.status == Status::Unverified {
                assert!(r.uwb.channels.is_empty() && r.uwb.max_dbm_mhz.is_none(), "{}", r.id);
            } else {
                assert!(!r.uwb.source.is_empty(), "{}", r.id);
            }
            if r.rfid.status == Status::Unverified {
                assert!(r.rfid.band_mhz.is_none() && r.rfid.max_eirp_dbm.is_none(), "{}", r.id);
            } else {
                assert!(!r.rfid.source.is_empty(), "{}", r.id);
            }
        }
    }

    #[test]
    fn vietnam_moves_uwb_to_channel_9_and_caps_the_reader() {
        let vn = region("vn");
        assert_eq!(vn.constrain(&anchor(6489.6)).freq_mhz, 7987.2);
        let r = vn.constrain(&reader(30.0, 9.0, 920.5));
        assert!((r.tx_power_dbm - 20.15).abs() < 0.01, "{}", r.tx_power_dbm);
        assert_eq!(r.freq_mhz, 920.5, "920.5 MHz is already inside the band");
    }

    #[test]
    fn allowed_values_are_left_alone() {
        let us = region("us");
        assert_eq!(us.constrain(&anchor(6489.6)).freq_mhz, 6489.6);
        assert_eq!(us.constrain(&anchor(7987.2)).freq_mhz, 7987.2);
        // 30 dBm + 6 dBi is exactly the FCC budget.
        assert_eq!(us.constrain(&reader(30.0, 6.0, 915.0)).tx_power_dbm, 30.0);
        // The rule above 6 dBi: 9 dBi gets 27 dBm.
        assert_eq!(us.constrain(&reader(30.0, 9.0, 915.0)).tx_power_dbm, 27.0);
        assert!(us.changes(&[anchor(6489.6)]).is_empty());
    }

    #[test]
    fn the_conducted_limit_binds_a_low_gain_reader() {
        // A 0 dBi desktop reader at 33 dBm is inside every EIRP limit that follows, not the conducted ones.
        assert_eq!(region("us").constrain(&reader(33.0, 0.0, 915.0)).tx_power_dbm, 30.0);
        assert_eq!(region("ca").constrain(&reader(33.0, 0.0, 915.0)).tx_power_dbm, 30.0);
        assert_eq!(region("jp").constrain(&reader(33.0, 0.0, 920.5)).tx_power_dbm, 24.0);
        // Where only EIRP is stated, EIRP is all that binds.
        assert_eq!(region("au").constrain(&reader(33.0, 0.0, 920.5)).tx_power_dbm, 33.0);
    }

    #[test]
    fn reader_outside_the_band_moves_to_its_centre() {
        assert_eq!(region("eu").constrain(&reader(20.0, 6.0, 920.5)).freq_mhz, 866.5);
    }

    #[test]
    fn unverified_rules_change_nothing_and_warn() {
        let mx = region("mx");
        let a = anchor(6489.6);
        assert_eq!(mx.constrain(&a).freq_mhz, a.freq_mhz);
        assert_eq!(mx.constrain(&a).uwb_tx_dbm_mhz, a.uwb_tx_dbm_mhz);
        assert!(mx.warnings().iter().any(|w| w.contains("UWB") && w.contains("not verified")), "{:?}", mx.warnings());
    }

    #[test]
    fn verified_regions_have_no_warnings() {
        assert!(region("vn").warnings().is_empty());
        assert!(region("us").warnings().is_empty());
    }

    #[test]
    fn missing_limit_is_not_invented() {
        let cn = region("cn");
        assert_eq!(cn.uwb_tx(-41.3), -41.3);
        assert_eq!(cn.constrain(&anchor(6489.6)).freq_mhz, 7987.2);
        assert!(cn.warnings().iter().any(|w| w.contains("No UWB power limit")));
    }

    #[test]
    fn other_radios_are_untouched() {
        let p = Profile { kind: DeviceKind::BleNode, freq_mhz: 2440.0, tx_power_dbm: 20.0, antenna_gain_dbi: 30.0, ..Default::default() };
        assert_eq!(region("vn").constrain(&p), p);
    }

    #[test]
    fn user_regions_add_and_replace() {
        let mut list = builtin();
        let n = list.len();
        merge(&mut list, vec![Region { id: "vn".into(), name: "Mine".into(), ..Default::default() }, Region { id: "xx".into(), name: "New".into(), ..Default::default() }, Region::default()]);
        assert_eq!(list.len(), n + 1);
        assert_eq!(list.iter().find(|r| r.id == "vn").unwrap().name, "Mine");
    }
}
