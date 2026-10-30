//! Antennas on anchors and tags: per-device choices (omni, directional, or a
//! pattern simulated with FDTD and saved to the antenna library), and the one
//! gain lookup the simulation, the floor map and the optimiser all use.

use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::device::Profile;
use super::planning::{AppliedAntenna, FdtdAntenna, Tech};
use super::radio;

/// A device's own antenna (overrides the profile's).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AntennaChoice {
    /// The same gain in every direction.
    Omni { gain_dbi: f32 },
    /// Aimed where the device faces: peak gain and half-power beam width.
    Directional { gain_dbi: f32, beam_deg: f32 },
    /// A pattern saved from the RF solver (FDTD), by name.
    Library { name: String },
}

impl AntennaChoice {
    pub fn label(&self) -> String {
        match self {
            Self::Omni { gain_dbi } => format!("Omni {gain_dbi:+.1} dBi"),
            Self::Directional { gain_dbi, beam_deg } => format!("Directional {gain_dbi:+.1} dBi · {beam_deg:.0}°"),
            Self::Library { name } => name.clone(),
        }
    }
}

/// Antenna patterns saved from FDTD runs, one JSON file each in
/// `%LOCALAPPDATA%/SenseSim/antennas`.
#[derive(Resource, Default)]
pub struct AntennaLibrary(pub Vec<FdtdAntenna>);

impl AntennaLibrary {
    pub fn dir() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(base).join("SenseSim").join("antennas")
    }

    pub fn load() -> Self {
        let mut v: Vec<FdtdAntenna> = std::fs::read_dir(Self::dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
            .collect();
        v.sort_by(|a, b| a.label.cmp(&b.label));
        Self(v)
    }

    pub fn get(&self, name: &str) -> Option<&FdtdAntenna> {
        self.0.iter().find(|a| a.label == name)
    }

    /// Adds (or replaces, by name) a pattern and writes it to disk.
    pub fn save(&mut self, a: FdtdAntenna) -> std::io::Result<()> {
        let dir = Self::dir();
        std::fs::create_dir_all(&dir)?;
        let file: String = a.label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        std::fs::write(dir.join(format!("{file}.json")), serde_json::to_string(&a).unwrap_or_default())?;
        self.0.retain(|x| x.label != a.label);
        self.0.push(a);
        self.0.sort_by(|a, b| a.label.cmp(&b.label));
        Ok(())
    }
}

/// Gain lookup: a device's own antenna, else the FDTD antenna applied to its
/// technology's infrastructure ("Use in the floor map"), else its profile's.
#[derive(Clone, Copy)]
pub struct Antennas<'a> {
    pub library: &'a AntennaLibrary,
    pub applied: &'a AppliedAntenna,
}

impl Antennas<'_> {
    /// Gain (dBi) of a device facing `fwd` towards `dir`. `infra`: an anchor /
    /// beacon / reader (the applied FDTD antenna is for those).
    pub fn gain(&self, choice: Option<&AntennaChoice>, tech: Tech, infra: bool, prof: &Profile, fwd: Vec3, dir: Vec3) -> f32 {
        let angle = || fwd.angle_between(dir);
        match choice {
            Some(AntennaChoice::Omni { gain_dbi }) => *gain_dbi,
            Some(AntennaChoice::Directional { gain_dbi, beam_deg }) => radio::antenna_gain(*gain_dbi, *beam_deg, angle()),
            // A pattern missing from the library (deleted file): isotropic.
            Some(AntennaChoice::Library { name }) => self.library.get(name).map_or(0.0, |a| a.gain(fwd, dir)),
            None => match self.applied.0.as_ref().filter(|a| infra && a.tech == tech) {
                Some(a) => a.gain(fwd, dir),
                None => radio::antenna_gain(prof.antenna_gain_dbi, prof.beam_deg, angle()),
            },
        }
    }

    /// Gain of a tag whose orientation the floor map doesn't know: an omni
    /// tag's gain, otherwise isotropic (0 dBi).
    pub fn tag_gain_unaimed(&self, choice: Option<&AntennaChoice>, prof: &Profile) -> f32 {
        match choice {
            Some(AntennaChoice::Omni { gain_dbi }) => *gain_dbi,
            Some(_) => 0.0,
            None if prof.beam_deg >= 359.0 => prof.antenna_gain_dbi,
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_resolve_in_order() {
        let lib = AntennaLibrary::default();
        let applied = AppliedAntenna::default();
        let a = Antennas { library: &lib, applied: &applied };
        let prof = Profile::default();
        let (fwd, ahead, behind) = (Vec3::X, Vec3::X, Vec3::NEG_X);
        // Profile default: omni 0 dBi.
        assert_eq!(a.gain(None, Tech::Uwb, true, &prof, fwd, behind), 0.0);
        assert_eq!(a.gain(Some(&AntennaChoice::Omni { gain_dbi: 3.0 }), Tech::Uwb, true, &prof, fwd, behind), 3.0);
        let dir = AntennaChoice::Directional { gain_dbi: 8.0, beam_deg: 60.0 };
        assert_eq!(a.gain(Some(&dir), Tech::Uwb, true, &prof, fwd, ahead), 8.0);
        assert!(a.gain(Some(&dir), Tech::Uwb, true, &prof, fwd, behind) < -10.0);
        // Unknown library pattern: isotropic rather than a crash.
        assert_eq!(a.gain(Some(&AntennaChoice::Library { name: "gone".into() }), Tech::Uwb, true, &prof, fwd, ahead), 0.0);
    }

    #[test]
    fn choice_json_is_readable() {
        let c = AntennaChoice::Directional { gain_dbi: 6.0, beam_deg: 90.0 };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, r#"{"type":"directional","gain_dbi":6.0,"beam_deg":90.0}"#);
        assert_eq!(serde_json::from_str::<AntennaChoice>(&s).unwrap(), c);
    }
}
