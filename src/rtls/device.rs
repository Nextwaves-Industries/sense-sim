//! Device profiles (data-driven, JSON), the `Device` component, tag motion
//! and zones.

use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    UwbAnchor,
    UwbTag,
    /// Fixed BLE beacon or gateway: a reference node for RSSI positioning.
    BleNode,
    /// Mobile BLE tag / phone.
    BleTag,
    RfidReader,
    RfidTag,
}

impl DeviceKind {
    pub const ALL: [DeviceKind; 6] =
        [Self::UwbAnchor, Self::UwbTag, Self::BleNode, Self::BleTag, Self::RfidReader, Self::RfidTag];

    pub fn label(self) -> &'static str {
        match self {
            Self::UwbAnchor => "UWB anchor",
            Self::UwbTag => "UWB tag",
            Self::BleNode => "BLE beacon/gateway",
            Self::BleTag => "BLE tag",
            Self::RfidReader => "RFID reader",
            Self::RfidTag => "RFID tag",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::UwbAnchor => "A",
            Self::UwbTag => "T",
            Self::BleNode => "B",
            Self::BleTag => "BT",
            Self::RfidReader => "R",
            Self::RfidTag => "E",
        }
    }

    /// Infrastructure (fixed) vs things being tracked.
    pub fn is_mobile(self) -> bool {
        matches!(self, Self::UwbTag | Self::BleTag | Self::RfidTag)
    }

    pub fn tech(self) -> &'static str {
        match self {
            Self::UwbAnchor | Self::UwbTag => "uwb",
            Self::BleNode | Self::BleTag => "ble",
            Self::RfidReader | Self::RfidTag => "rfid",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Self::UwbAnchor => Color::srgb(1.0, 0.69, 0.13),
            Self::UwbTag => Color::srgb(0.35, 0.85, 1.0),
            Self::BleNode => Color::srgb(0.55, 0.45, 1.0),
            Self::BleTag => Color::srgb(0.8, 0.6, 1.0),
            Self::RfidReader => Color::srgb(0.25, 0.85, 0.45),
            Self::RfidTag => Color::srgb(0.75, 1.0, 0.5),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UwbMode {
    #[default]
    Twr,
    Tdoa,
}

/// Radio and hardware parameters. Every field has a sensible default so a
/// vendor profile only needs to list what differs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub vendor: String,
    pub mount_height_m: f32,
    pub max_range_m: f32,
    /// Two-way ranging noise (LOS), metres.
    pub range_sigma_m: f32,
    /// Extra positive range bias per obstructing object, metres.
    pub nlos_bias_m: f32,
    /// Spread of the NLOS excess (exponential mean), metres.
    pub nlos_sigma_m: f32,
    /// Ranging fails behind more than this many obstructions.
    pub max_walls: u32,
    /// Probability that a single measurement is lost.
    pub dropout: f32,
    pub clock_ppm: f32,
    /// TDoA anchor synchronisation error, nanoseconds (1 ns ≈ 0.3 m).
    pub sync_sigma_ns: f32,
    pub update_hz: f32,
    pub mode: UwbMode,
    pub tx_power_dbm: f32,
    /// UWB transmit power as spectral density, dBm/MHz (−41.3 = the FCC/ETSI
    /// limit, what chips like the DW3000 run at full power). Lower it to
    /// shorten the link range; the planner and the simulation both follow it.
    pub uwb_tx_dbm_mhz: f32,
    pub rssi_at_1m: f32,
    pub path_loss_exp: f32,
    pub shadow_sigma_db: f32,
    pub wall_loss_db: f32,
    pub sensitivity_dbm: f32,
    pub antenna_gain_dbi: f32,
    pub beam_deg: f32,
    pub freq_mhz: f32,
    pub tag_sensitivity_dbm: f32,
    /// Max singulated tags per second (RFID).
    pub read_rate_hz: f32,
    pub battery_mah: f32,
    pub current_ma: f32,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: DeviceKind::UwbAnchor,
            vendor: "generic".into(),
            mount_height_m: 2.5,
            max_range_m: 40.0,
            range_sigma_m: 0.1,
            nlos_bias_m: 0.35,
            nlos_sigma_m: 0.25,
            max_walls: 2,
            dropout: 0.01,
            clock_ppm: 2.0,
            sync_sigma_ns: 0.15,
            update_hz: 10.0,
            mode: UwbMode::Twr,
            tx_power_dbm: 0.0,
            uwb_tx_dbm_mhz: super::radio::UWB_MAX_DBM_MHZ,
            rssi_at_1m: -59.0,
            path_loss_exp: 2.2,
            shadow_sigma_db: 4.0,
            wall_loss_db: 6.0,
            sensitivity_dbm: -95.0,
            antenna_gain_dbi: 0.0,
            beam_deg: 360.0,
            freq_mhz: 6489.6,
            tag_sensitivity_dbm: -18.0,
            read_rate_hz: 400.0,
            battery_mah: 0.0,
            current_ma: 0.0,
        }
    }
}

#[derive(Resource)]
pub struct Profiles(pub Vec<Profile>);

impl Profiles {
    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.0.iter().find(|p| p.id == id)
    }

    /// Profile by id, falling back to the first of that kind.
    pub fn resolve(&self, id: &str, kind: DeviceKind) -> Profile {
        self.get(id)
            .or_else(|| self.0.iter().find(|p| p.kind == kind))
            .cloned()
            .unwrap_or_else(|| Profile { kind, ..default() })
    }

    /// The device's profile with its own radio settings applied on top.
    pub fn for_device(&self, d: &Device) -> Profile {
        let mut p = self.resolve(&d.profile, d.kind);
        if let Some(tx) = d.radio.uwb_tx_dbm_mhz {
            p.uwb_tx_dbm_mhz = tx.min(super::radio::UWB_MAX_DBM_MHZ);
        }
        p
    }

    pub fn default_for(&self, kind: DeviceKind) -> Option<&Profile> {
        self.0.iter().find(|p| p.kind == kind)
    }

    /// Folders scanned for extra vendor profiles (`*.json`, a list or one object).
    pub fn user_dirs() -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Ok(exe) = std::env::current_exe()
            && let Some(d) = exe.parent()
        {
            v.push(d.join("devices"));
        }
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        v.push(PathBuf::from(base).join("SenseSim").join("devices"));
        v
    }

    pub fn load() -> Self {
        let mut list: Vec<Profile> = serde_json::from_str(include_str!("../../assets/devices/generic.json")).unwrap_or_default();
        for dir in Self::user_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "json") {
                    continue;
                }
                let Ok(txt) = std::fs::read_to_string(&p) else { continue };
                let parsed: Vec<Profile> = serde_json::from_str(&txt)
                    .or_else(|_| serde_json::from_str::<Profile>(&txt).map(|p| vec![p]))
                    .unwrap_or_default();
                for prof in parsed {
                    // User profiles override built-ins with the same id.
                    list.retain(|x| x.id != prof.id);
                    list.push(prof);
                }
            }
        }
        Self(list)
    }
}

/// How a mobile device moves during the simulation.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Motion {
    #[default]
    Static,
    /// Walks the waypoints at `speed` m/s; loops back to the start or ping-pongs.
    Path { points: Vec<[f32; 3]>, speed: f32, ping_pong: bool },
    /// Random waypoints inside a circle.
    Wander { center: [f32; 3], radius: f32, speed: f32 },
    /// Rides on a placed scene object (forklift, cart, pallet…).
    Follow { object: String, offset: [f32; 3] },
}

impl Motion {
    pub fn label(&self) -> &'static str {
        match self {
            Motion::Static => "Static",
            Motion::Path { .. } => "Path",
            Motion::Wander { .. } => "Wander",
            Motion::Follow { .. } => "Follow object",
        }
    }
}

/// A simulated radio device. Its pose is the entity's `Transform` (antennas
/// face the local -Z axis, Bevy's forward).
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
#[require(MotionState)]
pub struct Device {
    pub id: String,
    pub kind: DeviceKind,
    pub profile: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub motion: Motion,
    /// RFID: the EPC; others: an optional hardware address / serial.
    #[serde(default)]
    pub address: String,
    /// Placed by auto deploy (replaced when it runs again).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
    /// Radio settings for this device only, on top of its profile.
    #[serde(default, skip_serializing_if = "RadioOverride::is_empty")]
    pub radio: RadioOverride,
}

/// Per-device radio settings; `None` = the profile's value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RadioOverride {
    /// UWB transmit power, dBm/MHz (at most −41.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uwb_tx_dbm_mhz: Option<f32>,
    /// Its own antenna (`None` = the profile's, or the FDTD antenna applied to its technology).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub antenna: Option<super::antenna::AntennaChoice>,
}

impl RadioOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn yes() -> bool {
    true
}

/// Runtime motion state (not saved).
#[derive(Component, Default, Clone)]
pub struct MotionState {
    pub leg: usize,
    pub forward: bool,
    pub target: Option<Vec3>,
}

/// An axis-aligned area used for presence, occupancy and alarms.
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Zone {
    pub name: String,
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Entering raises an alarm.
    #[serde(default)]
    pub restricted: bool,
    /// Occupancy above this raises an alarm (0 = no limit).
    #[serde(default)]
    pub capacity: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

impl Zone {
    pub fn contains(&self, p: Vec3) -> bool {
        p.x >= self.min[0] && p.x <= self.max[0] && p.z >= self.min[2] && p.z <= self.max[2] && p.y >= self.min[1] - 0.5 && p.y <= self.max[1] + 0.5
    }
}

/// Saved form of a device (pose + config).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceDoc {
    #[serde(flatten)]
    pub device: Device,
    pub translation: [f32; 3],
    #[serde(default = "identity")]
    pub rotation: [f32; 4],
}

fn identity() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

/// A fresh, unused id like `A3`.
pub fn next_id<'a>(kind: DeviceKind, existing: impl Iterator<Item = &'a str>) -> String {
    let taken: std::collections::HashSet<&str> = existing.collect();
    (1..).map(|n| format!("{}{n}", kind.prefix())).find(|s| !taken.contains(s.as_str())).unwrap()
}

/// A plausible 96-bit EPC (SGTIN-ish hex) derived from the id.
pub fn epc_for(id: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("3034{:016X}{:04X}", h, (h >> 48) as u16)
}
