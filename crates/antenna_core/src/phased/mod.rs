//! Arrays and what's done with their signals: beamforming on any layout, with coupling from a
//! solved model when there is one; direction finding; the receive chain and the digital
//! processing behind it; and an FMCW MIMO radar. Nothing here has been measured on hardware.

pub mod aperture;
pub mod beam;
pub mod chain;
pub mod doa;
pub mod math;
pub mod pattern;
pub mod radar;
pub mod solved;

use serde::{Deserialize, Serialize};

use crate::geometry::P3;
use crate::{C0, C64};
use aperture::{Layout, unit_dir};
use beam::BeamPlan;
use chain::{Bf, ChainSim, Interferer, Waveform};
use doa::{DfArray, Impair, Method, Scan, Source};
use pattern::ElementKind;
use radar::{Fmcw, Target};

/// A direction-finding study: which array, what's out there and how it's read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DfDesign {
    pub layout: Layout,
    /// Read the array from the latest solved run's ports, coupling and all.
    pub use_ports: bool,
    /// With solved ports: the estimator's table comes from the solve as well. Off, it believes
    /// the textbook phases of the port positions, and the difference shows what coupling costs.
    pub calibrated: bool,
    pub sources: Vec<Source>,
    pub snr_db: f64,
    pub snapshots: usize,
    pub method: Method,
    pub scan: Scan,
    pub step: f64,
    /// How many sources to look for, `None` to count them.
    pub count: Option<usize>,
    pub forward_backward: bool,
    pub smooth_len: usize,
    pub impair: Impair,
    /// Accuracy study: these methods, over this SNR span.
    pub compare: Vec<Method>,
    pub snr_from: f64,
    pub snr_to: f64,
    pub snr_step: f64,
    pub trials: usize,
    pub seed: u64,
}

impl Default for DfDesign {
    fn default() -> Self {
        Self {
            layout: Layout::Line { n: 8, spacing: 0.5 },
            use_ports: false,
            calibrated: true,
            sources: vec![Source { theta: 20.0, phi: 0.0, ..Default::default() }, Source { theta: 35.0, phi: 180.0, ..Default::default() }],
            snr_db: 15.0,
            snapshots: 200,
            method: Method::Music,
            scan: Scan::Plane { phi: 0.0 },
            step: 0.5,
            count: None,
            forward_backward: false,
            smooth_len: 0,
            impair: Impair { backoff_db: 12.0, ..Default::default() },
            compare: vec![Method::Music, Method::Capon, Method::Beamscan],
            snr_from: -10.0,
            snr_to: 30.0,
            snr_step: 5.0,
            trials: 100,
            seed: 1,
        }
    }
}

/// A receive-chain study: the chain, plus where the signal and any jammer come from on a line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChainDesign {
    pub sim: ChainSim,
    /// Channels on a line at half-wave spacing.
    pub channels: usize,
    /// The signal's angle from broadside, degrees.
    pub signal_deg: f64,
    pub jammer: Option<JammerDesign>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct JammerDesign {
    pub angle_deg: f64,
    pub dbm: f64,
    pub waveform: Waveform,
}

impl Default for ChainDesign {
    fn default() -> Self {
        // Its phases are filled in by `resolved`, so a project file needn't carry them.
        Self { sim: ChainSim { a: Vec::new(), ..ChainSim::default() }, channels: 8, signal_deg: 10.0, jammer: None }
    }
}

/// The relative phases on a half-wave line for a wave from `deg` off broadside.
pub fn line_phases(n: usize, deg: f64) -> Vec<C64> {
    let step = std::f64::consts::PI * deg.to_radians().sin();
    (0..n).map(|i| C64::from_polar(1.0, i as f64 * step)).collect()
}

impl ChainDesign {
    /// The simulation as it runs: the channels and the jammer filled in.
    pub fn resolved(&self) -> ChainSim {
        let mut sim = self.sim.clone();
        sim.a = line_phases(self.channels.max(1), self.signal_deg);
        sim.interferers = self.jammer.iter().map(|j| Interferer { waveform: j.waveform, dbm: j.dbm, a: line_phases(self.channels.max(1), j.angle_deg) }).collect();
        if sim.bf == Bf::Single && self.channels > 1 {
            sim.a.truncate(self.channels);
        }
        sim
    }
}

/// A radar study.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RadarDesign {
    pub radar: Fmcw,
    pub n_tx: usize,
    pub n_rx: usize,
    pub targets: Vec<Target>,
}

impl Default for RadarDesign {
    fn default() -> Self {
        // Its elements are laid out by `resolved`, so a project file needn't carry them.
        Self { radar: Fmcw { tx: Vec::new(), rx: Vec::new(), ..Fmcw::default() }, n_tx: 3, n_rx: 4, targets: vec![Target { range_m: 15.0, speed_mps: 4.0, theta: 12.0, phi: 0.0, rcs_dbsm: 5.0 }, Target { range_m: 38.0, speed_mps: -5.0, theta: 25.0, phi: 180.0, rcs_dbsm: 15.0 }, Target { range_m: 52.0, speed_mps: 2.0, theta: 3.0, phi: 0.0, rcs_dbsm: 20.0 }] }
    }
}

impl RadarDesign {
    /// The radar with its elements laid out so the virtual array is one unbroken half-wave
    /// line: receivers at λ/2, transmitters a receiver aperture apart.
    pub fn resolved(&self) -> Fmcw {
        let mut r = self.radar.clone();
        let lambda = C0 / r.f0;
        let (n_tx, n_rx) = (self.n_tx.max(1), self.n_rx.max(1));
        r.rx = (0..n_rx).map(|i| [i as f64 * lambda / 2.0, 0.0, 0.0]).collect();
        r.tx = (0..n_tx).map(|i| [i as f64 * n_rx as f64 * lambda / 2.0, 0.0, 0.0]).collect();
        r
    }
}

/// All of a project's array work, kept with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhasedDesign {
    /// Design frequency, Hz. 0 for the project's.
    pub f0: f64,
    pub layout: Layout,
    pub element: ElementKind,
    pub plan: BeamPlan,
    /// Read the array from the latest solved run's ports, coupling and all.
    pub use_ports: bool,
    /// Each element's output at full drive, dBm, for EIRP.
    pub tx_dbm: f64,
    pub df: DfDesign,
    pub chain: ChainDesign,
    pub radar: RadarDesign,
}

impl Default for PhasedDesign {
    fn default() -> Self {
        Self { f0: 0.0, layout: Layout::default(), element: ElementKind::default(), plan: BeamPlan::default(), use_ports: false, tx_dbm: 20.0, df: DfDesign::default(), chain: ChainDesign::default(), radar: RadarDesign::default() }
    }
}

/// Whether positions are evenly spaced round a circle in the xy plane, starting on +x,
/// counter-clockwise: what ring methods assume.
pub fn is_ring(pos: &[P3]) -> bool {
    let n = pos.len();
    if n < 3 {
        return false;
    }
    let c: P3 = std::array::from_fn(|k| pos.iter().map(|p| p[k]).sum::<f64>() / n as f64);
    let r = ((pos[0][0] - c[0]).powi(2) + (pos[0][1] - c[1]).powi(2)).sqrt();
    if r <= 0.0 {
        return false;
    }
    pos.iter().enumerate().all(|(i, p)| {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        (p[0] - c[0] - r * a.cos()).abs() < 1e-6 * r && (p[1] - c[1] - r * a.sin()).abs() < 1e-6 * r && (p[2] - c[2]).abs() < 1e-6 * r
    })
}

/// Evenly spaced along x, as shift-invariant methods assume. `None` otherwise.
pub fn line_of(pos: &[P3]) -> Option<aperture::LineInfo> {
    if pos.len() < 2 || pos.iter().any(|p| (p[1] - pos[0][1]).abs() > 1e-9 || (p[2] - pos[0][2]).abs() > 1e-9) {
        return None;
    }
    let spacing = pos[1][0] - pos[0][0];
    if spacing <= 0.0 {
        return None;
    }
    pos.iter().enumerate().all(|(i, p)| (p[0] - pos[0][0] - i as f64 * spacing).abs() < 1e-6 * spacing).then(|| aperture::LineInfo { spacing, index: (0..pos.len() as i64).collect() })
}

/// The array as the estimators read it, from a layout.
pub fn df_array_of(layout: &Layout, f0: f64) -> DfArray {
    let pos: Vec<P3> = layout.elements(f0).iter().map(|e| e.pos).collect();
    let ring = matches!(layout, Layout::Ring { .. }) || is_ring(&pos);
    DfArray { line: layout.line(f0), ring, pos, f: f0 }
}

/// The array as the estimators read it, from port positions.
pub fn df_array_of_ports(pos: Vec<P3>, f: f64) -> DfArray {
    DfArray { line: line_of(&pos), ring: is_ring(&pos), pos, f }
}

/// Whether a scan of this kind suits this array: a line only tells one angle, a planar array
/// more, and a ring is at home all the way round.
pub fn natural_scan(array: &DfArray) -> Scan {
    if array.line.is_some() {
        Scan::Plane { phi: 0.0 }
    } else if array.ring {
        Scan::Azimuth { theta: 90.0 }
    } else {
        Scan::Hemisphere { theta_max: 80.0 }
    }
}

/// Where a plane scan's `θ` lands: a signed angle in the plane, as `unit_dir` reads it.
pub fn scan_direction(theta: f64, phi: f64) -> P3 {
    unit_dir(theta, phi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_design_round_trips_through_a_file() {
        let mut d = PhasedDesign::default();
        d.layout = Layout::Panels { faces: 3, nx: 4, ny: 4, dx: 0.5, dy: 0.5, radius: 1.0, tilt_deg: 10.0 };
        d.df.method = Method::Esprit;
        d.chain.jammer = Some(JammerDesign { angle_deg: 40.0, dbm: -50.0, waveform: Waveform::Noise { bw_hz: 20e6 } });
        let text = serde_json::to_string(&d).unwrap();
        let back: PhasedDesign = serde_json::from_str(&text).unwrap();
        // The reader can land a last bit off a float, which an edited file's next save settles.
        assert_eq!(serde_json::to_string(&back.layout).unwrap(), serde_json::to_string(&d.layout).unwrap());
        assert_eq!(back.df.method, d.df.method);
        assert_eq!(back.chain.jammer, d.chain.jammer);
        assert_eq!(back.radar.n_tx, d.radar.n_tx);
        assert_eq!(back.plan, d.plan);
        // A file from before any of this holds nothing, and a part of a design may be missing.
        let empty: PhasedDesign = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, PhasedDesign::default());
        let partial: PhasedDesign = serde_json::from_str(r#"{"tx_dbm": 30.0}"#).unwrap();
        assert_eq!(partial.tx_dbm, 30.0);
    }

    /// Every new project carries one of these, and a project is compared with itself after a save.
    #[test]
    fn a_fresh_design_comes_back_from_a_file_exactly() {
        let d = PhasedDesign::default();
        let back: PhasedDesign = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(d, back);
        // Taking those lists out mustn't have left a study with nothing to run on.
        assert_eq!(d.chain.resolved().a.len(), d.chain.channels);
        assert_eq!(d.radar.resolved().virtual_array().len(), d.radar.n_tx * d.radar.n_rx);
        assert!(chain::simulate(&ChainDesign { sim: ChainSim { samples: 1 << 12, ..d.chain.sim.clone() }, ..d.chain.clone() }.resolved()).is_ok());
    }

    #[test]
    fn rings_and_lines_are_recognised() {
        let f = 3e9;
        let ring: Vec<P3> = Layout::Ring { n: 6, radius: 0.5, radial: false }.elements(f).iter().map(|e| e.pos).collect();
        assert!(is_ring(&ring));
        assert!(line_of(&ring).is_none());
        let line: Vec<P3> = Layout::Line { n: 5, spacing: 0.5 }.elements(f).iter().map(|e| e.pos).collect();
        assert!(!is_ring(&line));
        assert!(line_of(&line).unwrap().is_uniform());
        let mut bent = line.clone();
        bent[2][1] = 0.01;
        assert!(line_of(&bent).is_none());
        assert_eq!(natural_scan(&df_array_of(&Layout::Ring { n: 6, radius: 0.5, radial: false }, f)), Scan::Azimuth { theta: 90.0 });
        assert_eq!(natural_scan(&df_array_of(&Layout::Line { n: 6, spacing: 0.5 }, f)), Scan::Plane { phi: 0.0 });
        assert!(matches!(natural_scan(&df_array_of(&Layout::default(), f)), Scan::Hemisphere { .. }));
    }

    #[test]
    fn a_chain_design_fills_in_its_channels_and_jammer() {
        let d = ChainDesign { channels: 4, signal_deg: 30.0, jammer: Some(JammerDesign { angle_deg: -20.0, dbm: -40.0, waveform: Waveform::Tone { offset_hz: 1e6 } }), ..Default::default() };
        let sim = d.resolved();
        assert_eq!(sim.a.len(), 4);
        assert_eq!(sim.interferers.len(), 1);
        // Half-wave at 30°: π·sin 30° between neighbours.
        assert!((sim.a[1].arg() - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn a_radar_design_makes_an_unbroken_virtual_line() {
        let d = RadarDesign { n_tx: 2, n_rx: 6, ..Default::default() }.resolved();
        let mut x: Vec<f64> = d.virtual_array().iter().map(|p| p[0]).collect();
        x.sort_by(f64::total_cmp);
        let lambda = d.lambda();
        assert_eq!(x.len(), 12);
        for w in x.windows(2) {
            assert!((w[1] - w[0] - lambda / 2.0).abs() < 1e-12, "{x:?}");
        }
    }
}
