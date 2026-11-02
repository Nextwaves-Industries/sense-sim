//! Reconfigurable antennas: switches, varactors and fixed parts across gaps in the
//! metal, and what the antenna does in each state.
//!
//! One full-wave solve answers for every state. Each part's gap is a port of the model,
//! so a run gives the S-matrix of all the ports and a far field per port. Closing a part's
//! port with its impedance is then algebra on those: with the feed ports `f`, the part
//! ports `l` and `Γ` the parts' reflection coefficients,
//!
//! ```text
//! a_l = (I − Γ·S_ll)⁻¹ Γ S_lf a_f     waves the parts send back in
//! S'  = S_ff + S_fl (I − Γ·S_ll)⁻¹ Γ S_lf
//! E   = Σ_f a_f·E_f + Σ_l a_l·E_l     the far field, by superposition
//! ```
//!
//! This holds exactly for linear parts, and it stays well conditioned for a short
//! (`Γ = −1`) and an open (`Γ = +1`), where an impedance form would blow up. The ports
//! are solved with a resistive reference, which damps the antenna, so a smooth basis
//! gives even a sharp state's resonance without resolving it in the solve. Trying a part,
//! a bias or a state costs milliseconds; only moving metal needs a new solve.

use std::f64::consts::PI;

use anyhow::{Result, bail};
use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target as ProofTarget, robust};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::farfield::{FarField, Metrics, Pol, Quantity};
use crate::project::{Setup, SimResult, combine};
use crate::sparam::{Network, barycentric_terms, floater_hormann_weights};
use crate::{C64, db10, db20, linspace};

type Matrix = Vec<Vec<C64>>;

/// Stands in for an infinite impedance: a perfect open reflects `1 − 2e-10`.
const OPEN_OHMS: f64 = 1e12;
/// A varactor's depletion capacitance stops rising here, just short of conducting.
const VARACTOR_CONDUCTS_AT: f64 = 0.6;

/// How one state sets a part.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Setting {
    On,
    Off,
    /// Reverse bias of a varactor, volts.
    Volts(f64),
}

/// A part's circuit, in SI units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Model {
    /// A PIN diode, FET or MEMS switch. On: `Ron + Ls`. Off: `Ls + (Coff ∥ Roff)`;
    /// `r_off` 0 means no parallel loss and `c_off` 0 a perfect open.
    Switch { r_on: f64, c_off: f64, r_off: f64, ls: f64, bias_ma: f64, v_forward: f64, v_reverse: f64 },
    /// `Ls + (Cp ∥ (Rs + Cj(V)))`, with `Cj(V) = Cj0 / (1 + V/Vj)^M`, the SPICE form
    /// varactor makers publish.
    Varactor { cj0: f64, vj: f64, m: f64, cp: f64, rs: f64, ls: f64, v_max: f64 },
    /// A fixed `R + L + C` in series; `c` 0 means no capacitor.
    Fixed { r: f64, l: f64, c: f64 },
}

/// One part as it comes from a datasheet or a guess, and what it can stand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub part: String,
    pub model: Model,
    /// Peak RF volts across it (0: not limited).
    #[serde(default)]
    pub v_limit: f64,
    /// Peak RF amps through it (0: not limited).
    #[serde(default)]
    pub i_limit: f64,
    /// Watts it can dissipate (0: not limited).
    #[serde(default)]
    pub p_limit: f64,
    /// Where the numbers come from, so a user can tell a datasheet from an assumption.
    #[serde(default)]
    pub source: String,
}

fn inverse_or_open(y: C64) -> C64 {
    if y.norm() < 1.0 / OPEN_OHMS { C64::new(OPEN_OHMS, 0.0) } else { y.inv() }
}

/// `1/z`, with a zero impedance standing as a very low one.
fn admittance(z: C64) -> C64 {
    if z.norm() < 1e-9 { C64::new(1e9, 0.0) } else { z.inv() }
}

impl Device {
    /// The parts in the library. Their values are typical figures for a quick first look:
    /// replace them with what your own datasheet says at your bias and frequency.
    pub fn library() -> Vec<Device> {
        const NH: f64 = 1e-9;
        const PF: f64 = 1e-12;
        let switch = |part: &str, r_on, c_off, r_off, ls, ma, vf, limits: (f64, f64, f64), source: &str| Device {
            part: part.into(),
            model: Model::Switch { r_on, c_off, r_off, ls, bias_ma: ma, v_forward: vf, v_reverse: 5.0 },
            v_limit: limits.0,
            i_limit: limits.1,
            p_limit: limits.2,
            source: source.into(),
        };
        vec![
            switch("Ideal switch", 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, (0.0, 0.0, 0.0), "A perfect short or open, for a first look at the geometry."),
            switch("BAR64-02V PIN", 2.1, 0.23 * PF, 5e3, 0.6 * NH, 10.0, 0.9, (150.0, 0.1, 0.25), "Infineon: 2.1 Ω at 10 mA, 0.23 pF at 1 GHz and up, 0.6 nH, 150 V, 100 mA. The off-state 5 kΩ, the 0.9 V and the 250 mW are assumed."),
            switch("SMP1345-079LF PIN", 1.5, 0.15 * PF, 10e3, 0.7 * NH, 10.0, 0.9, (50.0, 0.0, 0.25), "Skyworks SMP1345 series: about 1.5 Ω at 10 mA, 0.15 pF, 50 V, 250 mW. The 0.7 nH, the off-state 10 kΩ and the 0.9 V are assumed."),
            switch("MADP-000907-14020P PIN", 5.2, 0.03 * PF, 100e3, 0.05 * NH, 10.0, 1.3, (45.0, 0.0, 0.1), "MACOM flip chip: 5.2 Ω, 0.030 pF, 45 V, 100 mW. The 0.05 nH, the off-state 100 kΩ and the 1.3 V are assumed."),
            switch("RF switch, generic", 1.5, 0.15 * PF, 0.0, 0.3 * NH, 0.1, 2.5, (20.0, 0.0, 0.0), "Assumed figures for a small SOI or MEMS switch, not any one part. Enter your own."),
            Device {
                part: "SMV1405 varactor".into(),
                model: Model::Varactor { cj0: 2.37 * PF, vj: 0.77, m: 0.5, cp: 0.29 * PF, rs: 0.8, ls: 0.7 * NH, v_max: 30.0 },
                v_limit: 30.0,
                i_limit: 0.0,
                p_limit: 0.0,
                source: "Skyworks SPICE table: Cj0 2.37 pF, Vj 0.77 V, M 0.5, Cp 0.29 pF, Rs 0.8 Ω. The 0.7 nH and the 30 V are assumed: check your package.".into(),
            },
            Device { part: "Fixed capacitor".into(), model: Model::Fixed { r: 0.2, l: 0.4 * NH, c: 1.0 * PF }, v_limit: 0.0, i_limit: 0.0, p_limit: 0.0, source: "A fixed part: set its value.".into() },
            Device { part: "Fixed inductor".into(), model: Model::Fixed { r: 0.5, l: 5.0 * NH, c: 0.0 }, v_limit: 0.0, i_limit: 0.0, p_limit: 0.0, source: "A fixed part: set its value.".into() },
        ]
    }

    pub fn by_part(part: &str) -> Option<Device> {
        Self::library().into_iter().find(|d| d.part.eq_ignore_ascii_case(part))
    }

    /// The setting a new state gives this part.
    pub fn default_setting(&self) -> Setting {
        match self.model {
            Model::Switch { .. } | Model::Fixed { .. } => Setting::Off,
            Model::Varactor { v_max, .. } => Setting::Volts((v_max / 2.0).min(5.0)),
        }
    }

    /// A setting this part can really take, for one left from a part it replaced.
    pub fn normalized(&self, s: Setting) -> Setting {
        match (self.model, s) {
            (Model::Varactor { v_max, .. }, Setting::Volts(v)) => Setting::Volts(v.clamp(0.0, v_max)),
            (Model::Varactor { .. }, Setting::On) => Setting::Volts(0.0),
            (Model::Varactor { v_max, .. }, Setting::Off) => Setting::Volts(v_max),
            (_, Setting::Volts(v)) => {
                if v > 0.0 {
                    Setting::On
                } else {
                    Setting::Off
                }
            }
            (_, other) => other,
        }
    }

    /// The settings a search tries: on and off, or `volts` steps of bias, closer together
    /// at low bias where the capacitance moves fastest.
    pub fn options(&self, volts: usize) -> Vec<Setting> {
        match self.model {
            Model::Switch { .. } => vec![Setting::On, Setting::Off],
            Model::Varactor { v_max, .. } => {
                let n = volts.max(2);
                (0..n).map(|k| Setting::Volts(v_max * (k as f64 / (n - 1) as f64).powi(2))).collect()
            }
            Model::Fixed { .. } => vec![Setting::Off],
        }
    }

    pub fn is_fixed(&self) -> bool {
        matches!(self.model, Model::Fixed { .. })
    }

    /// Junction capacitance of a varactor at this setting, farads.
    pub fn junction_capacitance(&self, s: Setting) -> Option<f64> {
        let Model::Varactor { cj0, vj, m, .. } = self.model else { return None };
        let Setting::Volts(v) = self.normalized(s) else { return None };
        Some(cj0 / (1.0 + v.max(0.0) / vj).powf(m))
    }

    /// The part's impedance at `f`.
    pub fn z(&self, s: Setting, f: f64) -> C64 {
        let w = 2.0 * PI * f;
        let j = C64::new(0.0, 1.0);
        match self.model {
            Model::Switch { r_on, c_off, r_off, ls, .. } => {
                let series = j * (w * ls);
                match self.normalized(s) {
                    Setting::On => series + r_on,
                    _ => {
                        let g = if r_off > 0.0 { 1.0 / r_off } else { 0.0 };
                        series + inverse_or_open(C64::new(g, w * c_off))
                    }
                }
            }
            Model::Varactor { cp, rs, ls, .. } => {
                let cj = self.junction_capacitance(s).unwrap_or(0.0).max(1e-18);
                let branch = C64::new(rs, -1.0 / (w * cj));
                j * (w * ls) + inverse_or_open(admittance(branch) + j * (w * cp))
            }
            Model::Fixed { r, l, c } => {
                let cap = if c > 0.0 { -1.0 / (w * c) } else { 0.0 };
                C64::new(r, w * l + cap)
            }
        }
    }

    /// What this setting draws from the bias supply: `(volts, milliamps)`.
    pub fn dc(&self, s: Setting) -> (f64, f64) {
        match (self.model, self.normalized(s)) {
            (Model::Switch { bias_ma, v_forward, .. }, Setting::On) => (v_forward, bias_ma),
            (Model::Switch { v_reverse, .. }, _) => (-v_reverse, 0.0),
            (Model::Varactor { .. }, Setting::Volts(v)) => (-v, 0.0),
            _ => (0.0, 0.0),
        }
    }

    /// The setting in words.
    pub fn describe(&self, s: Setting) -> String {
        match (self.model, self.normalized(s)) {
            (Model::Fixed { .. }, _) => "Fixed".to_string(),
            (_, Setting::On) => "On".to_string(),
            (_, Setting::Off) => "Off".to_string(),
            (_, Setting::Volts(v)) => format!("{v:.1} V"),
        }
    }

    /// The part's values in one line.
    pub fn summary(&self) -> String {
        match self.model {
            Model::Switch { r_on, c_off, r_off, ls, .. } => {
                let off = if c_off > 0.0 { format!("{:.3} pF", c_off * 1e12) } else { "open".to_string() };
                let rp = if r_off > 0.0 { format!(" and {:.0} kΩ", r_off / 1e3) } else { String::new() };
                format!("{r_on:.1} Ω on, {off}{rp} off, {:.2} nH", ls * 1e9)
            }
            Model::Varactor { cp, rs, ls, v_max, .. } => {
                let (a, b) = (self.junction_capacitance(Setting::Volts(0.0)).unwrap_or(0.0) + cp, self.junction_capacitance(Setting::Volts(v_max)).unwrap_or(0.0) + cp);
                format!("{:.2}–{:.2} pF over 0–{v_max:.0} V, {rs:.1} Ω, {:.2} nH", b * 1e12, a * 1e12, ls * 1e9)
            }
            Model::Fixed { r, l, c } => {
                let mut parts = Vec::new();
                if c > 0.0 {
                    parts.push(format!("{:.2} pF", c * 1e12));
                }
                if l > 0.0 {
                    parts.push(format!("{:.2} nH", l * 1e9));
                }
                parts.push(format!("{r:.1} Ω"));
                parts.join(", ")
            }
        }
    }
}

/// What the board adds around a part. All optional; 0 means absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mount {
    /// A DC block in series with the part, farads.
    #[serde(default)]
    pub c_block: f64,
    /// Layout inductance in series with the part, henries.
    #[serde(default)]
    pub l_series: f64,
    /// The bias choke across the gap, henries.
    #[serde(default)]
    pub l_choke: f64,
    /// The choke's self-resonance, hertz. Above it a choke is a capacitor.
    #[serde(default)]
    pub choke_srf: f64,
    /// The choke's Q.
    #[serde(default)]
    pub choke_q: f64,
    /// Pads and gap across the part, farads.
    #[serde(default)]
    pub c_pad: f64,
}

/// One part mounted on the antenna, across the port of this name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub name: String,
    pub port: String,
    pub device: Device,
    #[serde(default)]
    pub mount: Mount,
}

impl Element {
    /// The part and what is in series with it.
    fn branch(&self, s: Setting, f: f64) -> C64 {
        let w = 2.0 * PI * f;
        let block = if self.mount.c_block > 0.0 { -1.0 / (w * self.mount.c_block) } else { 0.0 };
        self.device.z(s, f) + C64::new(0.0, w * self.mount.l_series + block)
    }

    /// What the antenna sees at the gap.
    pub fn z_load(&self, s: Setting, f: f64) -> C64 {
        let w = 2.0 * PI * f;
        let mut y = admittance(self.branch(s, f)) + C64::new(0.0, w * self.mount.c_pad);
        if self.mount.l_choke > 0.0 {
            let l = self.mount.l_choke;
            let r = if self.mount.choke_q > 0.0 { w * l / self.mount.choke_q } else { 0.0 };
            y += admittance(C64::new(r, w * l));
            if self.mount.choke_srf > 0.0 {
                // The winding's own capacitance, from where it resonates with the inductance.
                let w0 = 2.0 * PI * self.mount.choke_srf;
                y += C64::new(0.0, w / (w0 * w0 * l));
            }
        }
        inverse_or_open(y)
    }
}

/// One way of setting all the parts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TuneState {
    pub name: String,
    /// One per element, in their order.
    pub settings: Vec<Setting>,
    /// The frequency this state is meant for; `None`: where it matches best.
    #[serde(default)]
    pub f_op: Option<f64>,
}

/// A polarisation a state has to give.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolGoal {
    Linear,
    Rhcp,
    Lhcp,
}

impl PolGoal {
    pub const ALL: [PolGoal; 3] = [PolGoal::Linear, PolGoal::Rhcp, PolGoal::Lhcp];
    pub fn label(self) -> &'static str {
        match self {
            PolGoal::Linear => "Linear",
            PolGoal::Rhcp => "RHCP",
            PolGoal::Lhcp => "LHCP",
        }
    }
}

/// Above this a pattern is called linear, and at or below 3 dB circular. The range
/// between is elliptical, which neither goal accepts.
const LINEAR_AR_DB: f64 = 10.0;
const CIRCULAR_AR_DB: f64 = 3.0;

/// One thing the antenna has to do in some state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub name: String,
    pub f_lo: f64,
    pub f_hi: f64,
    /// Worst reflection allowed over the band, dB.
    pub limit_db: f64,
    /// A direction the beam has to reach, `[θ, φ]` in degrees: gain and polarisation are
    /// judged there. `None`: at the pattern's peak.
    #[serde(default)]
    pub beam: Option<[f64; 2]>,
    #[serde(default)]
    pub min_gain_dbi: Option<f64>,
    #[serde(default)]
    pub pol: Option<PolGoal>,
}

impl Target {
    pub fn band(name: &str, f_lo: f64, f_hi: f64) -> Self {
        Self { name: name.into(), f_lo, f_hi, limit_db: -10.0, beam: None, min_gain_dbi: None, pol: None }
    }
    pub fn center(&self) -> f64 {
        0.5 * (self.f_lo + self.f_hi)
    }
}

/// The reconfigurable part of a project: the parts, their states, and what they are for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reconfig {
    pub elements: Vec<Element>,
    pub states: Vec<TuneState>,
    /// The state an ordinary run, sweep or optimisation solves.
    pub active: usize,
    pub targets: Vec<Target>,
    /// Power into the feed, for the RF voltages and currents on the parts.
    pub power_dbm: f64,
}

impl Default for Reconfig {
    fn default() -> Self {
        Self { elements: Vec::new(), states: Vec::new(), active: 0, targets: Vec::new(), power_dbm: 20.0 }
    }
}

/// Which ports are feeds and which carry parts, as indices of the model's ports.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub feeds: Vec<usize>,
    /// The port of each element, in the elements' order.
    pub loads: Vec<usize>,
}

impl Reconfig {
    pub fn is_on(&self) -> bool {
        !self.elements.is_empty()
    }

    /// Matches the elements to the model's ports. An element's port that has gone, or no
    /// feed left, can't be solved and is said in those words.
    pub fn bind(&self, ports: &[String]) -> Result<Binding> {
        let mut loads = Vec::with_capacity(self.elements.len());
        for e in &self.elements {
            let Some(k) = ports.iter().position(|p| *p == e.port) else {
                bail!("'{}' sits across a port called '{}', and the model has no such port.", e.name, e.port);
            };
            if loads.contains(&k) {
                bail!("Two parts sit across port '{}'. A gap takes one: combine them with its mount, or add another gap.", e.port);
            }
            loads.push(k);
        }
        let feeds: Vec<usize> = (0..ports.len()).filter(|k| !loads.contains(k)).collect();
        if feeds.is_empty() {
            bail!("Every port carries a part, so nothing feeds the antenna. Leave one port as the feed.");
        }
        Ok(Binding { feeds, loads })
    }

    /// The state an ordinary run solves.
    pub fn active_state(&self) -> Option<&TuneState> {
        self.states.get(self.active.min(self.states.len().saturating_sub(1)))
    }

    /// Settings with one per element, each one its part can take.
    pub fn settings_of(&self, state: &TuneState) -> Vec<Setting> {
        self.elements.iter().enumerate().map(|(k, e)| e.device.normalized(state.settings.get(k).copied().unwrap_or_else(|| e.device.default_setting()))).collect()
    }

    /// Adds a part, and a setting for it in every state.
    pub fn add_element(&mut self, e: Element) {
        let setting = e.device.default_setting();
        self.elements.push(e);
        let n = self.elements.len();
        for st in &mut self.states {
            st.settings.resize(n, setting);
        }
        if self.states.is_empty() {
            self.states = self.default_states();
        }
    }

    /// Removes a part, and its setting from every state.
    pub fn remove_element(&mut self, k: usize) {
        if k < self.elements.len() {
            self.elements.remove(k);
            for st in &mut self.states {
                if k < st.settings.len() {
                    st.settings.remove(k);
                }
            }
            if self.elements.is_empty() {
                self.states.clear();
                self.active = 0;
            }
        }
    }

    /// A new state with every part at its default.
    pub fn blank_state(&self, name: &str) -> TuneState {
        TuneState { name: name.into(), settings: self.elements.iter().map(|e| e.device.default_setting()).collect(), f_op: None }
    }

    /// The states an unconfigured design starts with: all off, and all on if that means anything.
    pub fn default_states(&self) -> Vec<TuneState> {
        let mut off = self.blank_state("All off");
        for (s, e) in off.settings.iter_mut().zip(&self.elements) {
            if let Model::Varactor { v_max, .. } = e.device.model {
                *s = Setting::Volts(v_max);
            }
        }
        let mut on = self.blank_state("All on");
        let mut differs = false;
        for (s, e) in on.settings.iter_mut().zip(&self.elements) {
            match e.device.model {
                Model::Switch { .. } => {
                    *s = Setting::On;
                    differs = true;
                }
                Model::Varactor { .. } => {
                    *s = Setting::Volts(0.0);
                    differs = true;
                }
                Model::Fixed { .. } => {}
            }
        }
        if differs { vec![off, on] } else { vec![off] }
    }

    /// Every combination of the switches, and `volts` bias steps per varactor. `Err` above
    /// `limit`, with how many there would be.
    pub fn all_combinations(&self, volts: usize, limit: usize) -> Result<Vec<TuneState>> {
        let options: Vec<Vec<Setting>> = self.elements.iter().map(|e| e.device.options(volts)).collect();
        let total = count_combinations(&options);
        if total > limit {
            bail!("That would be {total} states. Keep it to {limit}, or use the search below, which tries them all without listing them.");
        }
        Ok((0..total)
            .map(|i| {
                let settings = nth_combination(&options, i);
                TuneState { name: self.state_name(&settings), settings, f_op: None }
            })
            .collect())
    }

    /// A short name for some settings: which switches are on, and what the bias is.
    pub fn state_name(&self, settings: &[Setting]) -> String {
        let mut on = Vec::new();
        let mut bias = Vec::new();
        let mut switches = 0;
        for (e, s) in self.elements.iter().zip(settings) {
            match (e.device.model, *s) {
                (Model::Switch { .. }, Setting::On) => {
                    on.push(e.name.as_str());
                    switches += 1;
                }
                (Model::Switch { .. }, _) => switches += 1,
                (Model::Varactor { .. }, Setting::Volts(v)) => bias.push(format!("{v:.1} V")),
                _ => {}
            }
        }
        bias.dedup();
        let mut out = Vec::new();
        if switches > 0 {
            out.push(if on.is_empty() {
                "All off".to_string()
            } else if on.len() == switches {
                "All on".to_string()
            } else {
                format!("{} on", on.join(", "))
            });
        }
        if !bias.is_empty() {
            out.push(bias.join(", "));
        }
        if out.is_empty() { "Fixed".to_string() } else { out.join(", ") }
    }

    /// The frequencies a solve needs far fields at, since each state works somewhere else:
    /// the setup's own, the targets' centres and an even spread across the sweep.
    pub fn far_plan(&self, setup: &Setup, samples: usize) -> Vec<f64> {
        let mut v: Vec<f64> = setup.far_freqs.clone();
        v.extend(self.targets.iter().map(Target::center));
        v.extend(self.states.iter().filter_map(|s| s.f_op));
        v.extend(linspace(setup.f_lo, setup.f_hi, samples));
        v.retain(|f| *f >= setup.f_lo && *f <= setup.f_hi);
        v.sort_by(f64::total_cmp);
        let close = (setup.f_hi - setup.f_lo) * 0.005;
        v.dedup_by(|a, b| (*a - *b).abs() <= close);
        v
    }
}

fn count_combinations(options: &[Vec<Setting>]) -> usize {
    options.iter().fold(1usize, |n, o| n.saturating_mul(o.len().max(1)))
}

fn nth_combination(options: &[Vec<Setting>], mut i: usize) -> Vec<Setting> {
    options
        .iter()
        .map(|o| {
            let s = o[i % o.len()];
            i /= o.len();
            s
        })
        .collect()
}

/// Port `k`'s row of a far-field list, `[port][freq]`: every port's pattern at one frequency.
fn patterns_at(basis: &SimResult, k: usize) -> Option<Vec<&FarField>> {
    basis.far.iter().map(|p| p.get(k)).collect()
}

/// One frequency with the parts' ports closed.
#[derive(Clone, Debug)]
pub struct Reduced {
    /// Between the feeds.
    pub s: Matrix,
    /// `a[load][feed]`: the wave the part sends back in, per unit wave at that feed.
    pub a: Matrix,
    /// `b[load][feed]`: the wave arriving at the part.
    pub b: Matrix,
}

/// Solves `A·X = B` in place by Gaussian elimination, `A` an `n × n` row list. `None` if singular.
fn solve_small(a: &mut Matrix, b: &mut Matrix) -> Option<()> {
    let n = a.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|x, y| a[*x][col].norm_sqr().total_cmp(&a[*y][col].norm_sqr()))?;
        if a[pivot][col].norm() < 1e-14 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let k = a[row][col] / a[col][col];
            if k.norm_sqr() == 0.0 {
                continue;
            }
            for j in col..n {
                let v = a[col][j];
                a[row][j] -= k * v;
            }
            for j in 0..b[row].len() {
                let v = b[col][j];
                b[row][j] -= k * v;
            }
        }
    }
    for col in (0..n).rev() {
        for j in 0..b[col].len() {
            let mut v = b[col][j];
            for k in col + 1..n {
                v -= a[col][k] * b[k][j];
            }
            b[col][j] = v / a[col][col];
        }
    }
    Some(())
}

/// Closes the part ports of one S-matrix with these reflection coefficients.
pub fn reduce(s: &[Vec<C64>], bind: &Binding, gamma: &[C64]) -> Option<Reduced> {
    let (nf, nl) = (bind.feeds.len(), bind.loads.len());
    let zero = C64::new(0.0, 0.0);
    let mut a_mat: Matrix = (0..nl).map(|i| (0..nl).map(|j| -gamma[i] * s[bind.loads[i]][bind.loads[j]] + if i == j { 1.0 } else { 0.0 }).collect()).collect();
    let mut x: Matrix = (0..nl).map(|i| (0..nf).map(|j| gamma[i] * s[bind.loads[i]][bind.feeds[j]]).collect()).collect();
    solve_small(&mut a_mat, &mut x)?;
    let mut out = vec![vec![zero; nf]; nf];
    for i in 0..nf {
        for j in 0..nf {
            let mut v = s[bind.feeds[i]][bind.feeds[j]];
            for l in 0..nl {
                v += s[bind.feeds[i]][bind.loads[l]] * x[l][j];
            }
            out[i][j] = v;
        }
    }
    let mut b = vec![vec![zero; nf]; nl];
    for i in 0..nl {
        for j in 0..nf {
            let mut v = s[bind.loads[i]][bind.feeds[j]];
            for l in 0..nl {
                v += s[bind.loads[i]][bind.loads[l]] * x[l][j];
            }
            b[i][j] = v;
        }
    }
    Some(Reduced { s: out, a: x, b })
}

/// What one part goes through in one state, at the state's frequency and the design's power.
#[derive(Clone, Debug)]
pub struct PartLoad {
    pub name: String,
    pub setting: String,
    /// What the antenna sees at the gap.
    pub z_load: C64,
    pub v_peak: f64,
    pub i_peak: f64,
    /// Watts turned to heat in the part itself.
    pub p_device: f64,
    /// Watts lost in the part and its mount.
    pub p_total: f64,
    /// The power at which the first limit is reached, dBm. `None`: the part has no limit here.
    pub max_power_dbm: Option<f64>,
    /// What that first limit is.
    pub limit: Option<String>,
    /// The limit is already passed.
    pub over: bool,
    /// The RF swing is large against the bias, so the capacitance moves with the signal.
    pub nonlinear: bool,
}

/// One state, judged.
#[derive(Clone, Debug, Default)]
pub struct StateReport {
    pub name: String,
    pub settings: Vec<Setting>,
    /// Feed 1's reflection, dB, over the sweep.
    pub curve: Vec<(f64, f64)>,
    /// The frequency the figures below are for.
    pub f_op: f64,
    pub s_op_db: f64,
    pub resonances: Vec<(f64, f64)>,
    pub bands: Vec<(f64, f64)>,
    /// The far-field frequency nearest `f_op`, and that pattern's figures.
    pub far_f: Option<f64>,
    pub metrics: Option<Metrics>,
    /// Polarisation in words, at the pattern's peak.
    pub polarisation: String,
    /// Share of the accepted power lost in the parts and mounts, 0..1.
    pub part_loss: f64,
    pub parts: Vec<PartLoad>,
    pub dc_ma: f64,
    pub dc_mw: f64,
}

/// How well one set of settings does one target.
#[derive(Clone, Debug, Default)]
pub struct Score {
    pub worst_s_db: f64,
    /// Realized gain where the target judges it, at the far-field frequency used.
    pub gain_dbi: Option<f64>,
    pub axial_ratio_db: Option<f64>,
    pub rhcp: bool,
    pub far_f: Option<f64>,
    /// Peak direction `[θ, φ]` of that pattern.
    pub peak: Option<[f64; 2]>,
    /// 0 when met; otherwise roughly how many dB it misses by.
    pub penalty: f64,
    pub met: bool,
}

/// A target, and the best settings found for it.
#[derive(Clone, Debug)]
pub struct Found {
    pub target: String,
    pub settings: Vec<Setting>,
    pub score: Score,
    pub tried: usize,
    /// Every combination was tried, rather than searching from several starts.
    pub exhaustive: bool,
}

/// One bias of a tuning curve.
#[derive(Clone, Copy, Debug)]
pub struct TunePoint {
    pub volts: f64,
    /// Junction plus package capacitance of the first varactor, farads.
    pub capacitance: f64,
    pub f_res: Option<f64>,
    pub depth_db: f64,
    pub band: Option<(f64, f64)>,
    /// Share of the accepted power not lost in the parts, at resonance.
    pub part_efficiency: f64,
}

/// Every state, and what they add up to.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub states: Vec<StateReport>,
    /// The best reflection any state reaches, over the sweep: what tuning can cover.
    pub envelope: Vec<(f64, f64)>,
    /// The lowest and highest resonance of any state.
    pub tuning: Option<(f64, f64)>,
    /// The frequencies under −10 dB in some state.
    pub coverage: Vec<(f64, f64)>,
    /// The largest angle between two states' peak directions, degrees.
    pub beam_span_deg: f64,
    /// Each target with the states that meet it: `(target, state indices)`.
    pub met_by: Vec<(String, Vec<usize>)>,
    pub warnings: Vec<String>,
}

const COVERAGE_LIMIT_DB: f64 = -10.0;

/// Solved patterns across a sweep. A state's narrow resonance is judged on a pattern
/// interpolated between them: over a 2.8:1 sweep of a switched dipole, 9 left 8.7 % of the field
/// wrong and 0.8 dB of gain, 17 left 0.5 % and 0.04 dB. FDTD pays for each in run time, so it
/// takes fewer, and [`Solved::report`] says so if the power stops adding up.
pub const FAR_SAMPLES_MOM: usize = 17;
pub const FAR_SAMPLES_FDTD: usize = 9;

/// Above this many combinations the search stops listing them and hill-climbs instead.
const MAX_EXHAUSTIVE: usize = 60_000;
/// How many candidates get their pattern worked out, ranked by match.
const PATTERNS_JUDGED: usize = 256;

/// A solved run, ready to be closed with any parts in any state. Works on a basis whose
/// far fields are phase-referenced to their incident waves, as `SimResult`s are.
pub struct Solved<'a> {
    pub basis: &'a SimResult,
    pub bind: Binding,
    /// The basis S at each far-field frequency.
    far_s: Vec<Matrix>,
}

impl<'a> Solved<'a> {
    /// `ports`: the model's port names, in the order they were solved.
    pub fn new(basis: &'a SimResult, ports: &[String], rc: &Reconfig) -> Result<Self> {
        let bind = rc.bind(ports)?;
        if basis.network.ports() != ports.len() {
            bail!("This solve has {} ports and the model has {}: solve it again.", basis.network.ports(), ports.len());
        }
        let far_s = if basis.far_f.is_empty() || basis.network.s.is_empty() { Vec::new() } else { basis.network.interpolate(&basis.far_f).s };
        Ok(Self { basis, bind, far_s })
    }

    fn gammas(&self, rc: &Reconfig, settings: &[Setting], f: f64) -> Vec<C64> {
        rc.elements
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let z = e.z_load(settings[i], f);
                let z0 = self.basis.network.z0[self.bind.loads[i]];
                (z - z0) / (z + z0)
            })
            .collect()
    }

    /// The feeds' network in these settings.
    pub fn network(&self, rc: &Reconfig, settings: &[Setting]) -> Result<Network> {
        let net = &self.basis.network;
        let s: Option<Vec<Matrix>> = net.f.par_iter().zip(&net.s).map(|(f, m)| reduce(m, &self.bind, &self.gammas(rc, settings, *f)).map(|r| r.s)).collect();
        let Some(s) = s else { bail!("These settings make the parts and the antenna resonate without loss, which has no finite answer.") };
        Ok(Network { f: net.f.clone(), z0: self.bind.feeds.iter().map(|k| net.z0[*k]).collect(), s })
    }

    /// The reduction at any frequency in the sweep.
    fn at(&self, rc: &Reconfig, settings: &[Setting], f: f64) -> Option<Reduced> {
        let s = self.basis.network.interpolate(&[f]).s.swap_remove(0);
        reduce(&s, &self.bind, &self.gammas(rc, settings, f))
    }

    /// The pattern at `far_f[k]` with `feed` driven: 1 W incident, the other feeds matched.
    pub fn far(&self, rc: &Reconfig, settings: &[Setting], feed: usize, k: usize) -> Option<FarField> {
        let f = *self.basis.far_f.get(k)?;
        let list = patterns_at(self.basis, k)?;
        let r = reduce(self.far_s.get(k)?, &self.bind, &self.gammas(rc, settings, f))?;
        let mut w = vec![C64::new(0.0, 0.0); list.len()];
        w[self.bind.feeds[feed]] = C64::new(1.0, 0.0);
        for (l, port) in self.bind.loads.iter().enumerate() {
            w[*port] = r.a[l][feed];
        }
        let mut ff = combine(&list, &w)?;
        ff.p_incident = 1.0;
        ff.p_accepted = 1.0 - r.s[feed][feed].norm_sqr();
        Some(ff)
    }

    /// The pattern at any frequency the solved patterns span, with `feed` driven, and the
    /// frequency it really is for. Each port's own pattern is damped by the others' reference
    /// resistors, so it moves smoothly with frequency and interpolates well, even where a
    /// state's resonance is far narrower than the spacing of the solved patterns. Outside
    /// that span, or with a single solved pattern, it's the nearest one.
    pub fn far_at(&self, rc: &Reconfig, settings: &[Setting], feed: usize, f: f64) -> Option<(FarField, f64)> {
        let fs = &self.basis.far_f;
        let k = self.basis.far_index(f)?;
        let (lo, hi) = (*fs.first()?, *fs.last()?);
        if fs.len() < 2 || f < lo || f > hi || (fs[k] - f).abs() <= 1e-9 * f {
            return self.far(rc, settings, feed, k).map(|ff| (ff, fs[k]));
        }
        let x: Vec<f64> = fs.iter().map(|v| (v - lo) / (hi - lo)).collect();
        let terms = barycentric_terms(&x, &floater_hormann_weights(&x, 3), (f - lo) / (hi - lo));
        let s = self.basis.network.interpolate(&[f]).s.swap_remove(0);
        let r = reduce(&s, &self.bind, &self.gammas(rc, settings, f))?;
        let mut w = vec![C64::new(0.0, 0.0); self.basis.far.len()];
        w[self.bind.feeds[feed]] = C64::new(1.0, 0.0);
        for (l, port) in self.bind.loads.iter().enumerate() {
            w[*port] = r.a[l][feed];
        }
        let mut out: Option<FarField> = None;
        for (i, t) in terms {
            let c = combine(&patterns_at(self.basis, i)?, &w)?;
            match out.as_mut() {
                None => {
                    let mut first = c;
                    first.e_theta.iter_mut().chain(first.e_phi.iter_mut()).flatten().for_each(|v| *v *= t);
                    out = Some(first);
                }
                Some(sum) => {
                    for (a, b) in sum.e_theta.iter_mut().chain(sum.e_phi.iter_mut()).flatten().zip(c.e_theta.iter().chain(c.e_phi.iter()).flatten()) {
                        *a += *b * t;
                    }
                }
            }
        }
        let mut ff = out?;
        ff.f = f;
        ff.p_incident = 1.0;
        ff.p_accepted = 1.0 - r.s[feed][feed].norm_sqr();
        Some((ff, f))
    }

    /// Where a state is judged: the frequency it's meant for, or where it matches best.
    fn operating_frequency(state: &TuneState, network: &Network) -> f64 {
        let deepest = network.resonances(0).into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        let db = network.sij_db(0, 0);
        let least = network.f.iter().zip(&db).filter(|v| v.1.is_finite()).min_by(|a, b| a.1.total_cmp(b.1)).map(|x| *x.0);
        state.f_op.or(deepest).or(least).unwrap_or(network.f.first().copied().unwrap_or(1e9))
    }

    /// What a state does as an ordinary result, feeds only: every result page, the array,
    /// matching and link pages and the exports can then take it like any other run.
    pub fn result(&self, rc: &Reconfig, state: &TuneState) -> Result<SimResult> {
        let settings = rc.settings_of(state);
        let network = self.network(rc, &settings)?;
        let basis = self.basis;
        // Its own frequency among the patterns, so a page can show it where it works.
        let mut far_f = basis.far_f.clone();
        let f_op = Self::operating_frequency(state, &network);
        if far_f.len() >= 2 && f_op > far_f[0] && f_op < far_f[far_f.len() - 1] && !far_f.iter().any(|f| (f - f_op).abs() <= 1e-9 * f_op) {
            far_f.push(f_op);
            far_f.sort_by(f64::total_cmp);
        }
        let far: Vec<Vec<FarField>> = (0..self.bind.feeds.len()).map(|feed| far_f.iter().filter_map(|f| self.far_at(rc, &settings, feed, *f).map(|x| x.0)).collect()).collect();
        let lossy = rc.elements.iter().zip(&settings).any(|(e, s)| basis.far_f.iter().chain(basis.network.f.first()).any(|f| e.z_load(*s, *f).re > 1e-9));
        let lossless = basis.lossless && !lossy;
        let mut warnings = basis.warnings.clone();
        warnings.extend(crate::checks::balance_warnings(&network, &far, lossless, "The parts or the far-field grid don't add up here: refine the mesh or check the part values."));
        warnings.dedup();
        Ok(SimResult {
            label: format!("{} · {}", basis.label, state.name),
            solver: basis.solver.clone(),
            network,
            far,
            far_f,
            // A field plane or current picture of the basis shows port 1 with every other port
            // matched, which isn't this state.
            planes: Vec::new(),
            currents: None,
            size: basis.size,
            seconds: basis.seconds,
            dir: basis.dir.clone(),
            vars: basis.vars.clone(),
            warnings,
            density: basis.density,
            density_unit: basis.density_unit.clone(),
            end_db: basis.end_db,
            metal: basis.metal.clone(),
            lossless,
            convergence: basis.convergence,
        })
    }

    /// The incident waves on every port for 1 W at feed 1, at `f`: what a solver needs to
    /// redraw this state's currents.
    pub fn waves(&self, rc: &Reconfig, settings: &[Setting], f: f64) -> Option<Vec<C64>> {
        let r = self.at(rc, settings, f)?;
        let n = self.basis.network.ports();
        let mut w = vec![C64::new(0.0, 0.0); n];
        w[*self.bind.feeds.first()?] = C64::new(1.0, 0.0);
        for (l, port) in self.bind.loads.iter().enumerate() {
            w[*port] = r.a[l][0];
        }
        Some(w)
    }

    fn part_loads(&self, rc: &Reconfig, settings: &[Setting], f: f64, r: &Reduced) -> Vec<PartLoad> {
        let watts = 1e-3 * 10f64.powf(rc.power_dbm / 10.0);
        // Peak waves for this much power per unit wave at the feed.
        let scale = (2.0 * watts).sqrt();
        rc.elements
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let z0 = self.basis.network.z0[self.bind.loads[i]];
                let (a, b) = (r.a[i][0] * scale, r.b[i][0] * scale);
                let v = (a + b) * z0.sqrt();
                let setting = settings[i];
                let z_device = e.device.z(setting, f);
                let i_branch = v * admittance(e.branch(setting, f));
                let v_device = i_branch * z_device;
                let (v_peak, i_peak) = (v_device.norm(), i_branch.norm());
                let p_device = 0.5 * i_branch.norm_sqr() * z_device.re.max(0.0);
                let p_total = (0.5 * (b.norm_sqr() - a.norm_sqr())).max(0.0);
                let (mut max_w, mut limit, mut over) = (f64::INFINITY, None, false);
                let mut consider = |ratio: f64, what: String, sq: bool| {
                    // `ratio` is what's happening over what's allowed, and scales with √P or P.
                    if ratio > 0.0 {
                        let at = watts / if sq { ratio * ratio } else { ratio };
                        if at < max_w {
                            max_w = at;
                            limit = Some(what);
                        }
                        over |= ratio > 1.0;
                    }
                };
                let mut nonlinear = false;
                match (e.device.model, e.device.normalized(setting)) {
                    (Model::Varactor { vj, v_max, .. }, Setting::Volts(bias)) => {
                        consider(v_peak / (bias + VARACTOR_CONDUCTS_AT).max(1e-6), format!("the RF swing passing the {bias:.1} V bias, which makes it conduct"), true);
                        if v_max > bias {
                            consider(v_peak / (v_max - bias), format!("the {v_max:.0} V reverse limit"), true);
                        }
                        nonlinear = v_peak > 0.25 * (bias + vj);
                    }
                    (Model::Switch { .. }, Setting::On) => {
                        if e.device.i_limit > 0.0 {
                            consider(i_peak / e.device.i_limit, format!("{:.0} mA of RF current", e.device.i_limit * 1e3), true);
                        }
                    }
                    (Model::Switch { .. }, _) => {
                        if e.device.v_limit > 0.0 {
                            consider(v_peak / e.device.v_limit, format!("{:.0} V of RF across it", e.device.v_limit), true);
                        }
                    }
                    _ => {}
                }
                if e.device.p_limit > 0.0 {
                    consider(p_device / e.device.p_limit, format!("{:.0} mW dissipated", e.device.p_limit * 1e3), false);
                }
                let max_power_dbm = max_w.is_finite().then(|| db10(max_w * 1e3));
                PartLoad { name: e.name.clone(), setting: e.device.describe(setting), z_load: e.z_load(setting, f), v_peak, i_peak, p_device, p_total, max_power_dbm, limit, over, nonlinear }
            })
            .collect()
    }

    /// Judges a state at its frequency, or wherever it matches best.
    pub fn state_report(&self, rc: &Reconfig, state: &TuneState) -> Result<StateReport> {
        let settings = rc.settings_of(state);
        let network = self.network(rc, &settings)?;
        let db = network.sij_db(0, 0);
        let curve: Vec<(f64, f64)> = network.f.iter().copied().zip(db.iter().copied()).collect();
        let resonances = network.resonances(0);
        let bands = network.bandwidths(0, COVERAGE_LIMIT_DB);
        let f_op = Self::operating_frequency(state, &network);
        let reduced = self.at(rc, &settings, f_op);
        let s_op_db = reduced.as_ref().map_or(f64::NAN, |r| db20(r.s[0][0].norm()));
        let parts = reduced.as_ref().map_or(Vec::new(), |r| self.part_loads(rc, &settings, f_op, r));
        // Lost per unit incident, over what the antenna accepted.
        let part_loss = reduced.as_ref().map_or(0.0, |r| {
            let accepted = 1.0 - r.s[0][0].norm_sqr();
            let lost: f64 = (0..rc.elements.len()).map(|l| (r.b[l][0].norm_sqr() - r.a[l][0].norm_sqr()).max(0.0)).sum();
            if accepted > 1e-9 { (lost / accepted).clamp(0.0, 1.0) } else { 0.0 }
        });
        let far = self.far_at(rc, &settings, 0, f_op);
        let metrics = far.as_ref().map(|x| x.0.metrics());
        let polarisation = metrics.as_ref().map_or_else(|| "–".to_string(), polarisation_words);
        let mut dc_ma = 0.0;
        let mut dc_mw = 0.0;
        for (e, s) in rc.elements.iter().zip(&settings) {
            let (v, ma) = e.device.dc(*s);
            dc_ma += ma;
            dc_mw += v.abs() * ma;
        }
        Ok(StateReport { name: state.name.clone(), settings, curve, f_op, s_op_db, resonances, bands, far_f: far.map(|x| x.1), metrics, polarisation, part_loss, parts, dc_ma, dc_mw })
    }

    /// All the states, the figures across them, and which meet the targets.
    pub fn report(&self, rc: &Reconfig) -> Result<Report> {
        let states: Vec<StateReport> = rc.states.par_iter().map(|s| self.state_report(rc, s)).collect::<Result<_>>()?;
        let mut envelope = Vec::new();
        if let Some(first) = states.first() {
            envelope = first.curve.clone();
            for s in &states[1..] {
                for (e, c) in envelope.iter_mut().zip(&s.curve) {
                    if c.1 < e.1 {
                        e.1 = c.1;
                    }
                }
            }
        }
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for s in &states {
            for r in &s.resonances {
                lo = lo.min(r.0);
                hi = hi.max(r.0);
            }
        }
        let coverage = bands_under(&envelope, COVERAGE_LIMIT_DB);
        let mut beam_span_deg: f64 = 0.0;
        for (i, a) in states.iter().enumerate() {
            for b in &states[i + 1..] {
                if let (Some(ma), Some(mb)) = (&a.metrics, &b.metrics) {
                    beam_span_deg = beam_span_deg.max(angle_between([ma.peak_theta, ma.peak_phi], [mb.peak_theta, mb.peak_phi]));
                }
            }
        }
        let met_by = rc
            .targets
            .iter()
            .map(|t| {
                let list = rc.states.iter().enumerate().filter(|(_, st)| self.score(rc, &rc.settings_of(st), t).met).map(|(i, _)| i).collect();
                (t.name.clone(), list)
            })
            .collect();
        let mut warnings = Vec::new();
        let sweep = (self.basis.network.f.first().copied().unwrap_or(0.0), self.basis.network.f.last().copied().unwrap_or(0.0));
        for (t, list) in rc.targets.iter().zip(&met_by) {
            let list: &(String, Vec<usize>) = list;
            if t.f_lo < sweep.0 || t.f_hi > sweep.1 {
                warnings.push(format!("'{}' reaches outside the solved sweep, {:.3}–{:.3} GHz: widen the sweep and solve again.", t.name, sweep.0 / 1e9, sweep.1 / 1e9));
            } else if list.1.is_empty() {
                warnings.push(format!("No state meets '{}' yet. Search for one below, or add a state.", t.name));
            }
            // Between solved patterns it interpolates; beyond them it can only take the nearest.
            let needs_far = t.beam.is_some() || t.min_gain_dbi.is_some() || t.pol.is_some();
            let fs = &self.basis.far_f;
            let spanned = fs.len() >= 2 && t.center() >= fs[0] && t.center() <= fs[fs.len() - 1];
            if needs_far && !spanned {
                let near = self.basis.far_index(t.center()).and_then(|k| fs.get(k).copied()).unwrap_or(0.0);
                warnings.push(format!("No pattern was solved around '{}': its gain and polarisation are from {:.3} GHz. Solve again to add one.", t.name, near / 1e9));
            }
        }
        let quoted = |list: Vec<&StateReport>| list.iter().map(|s| format!("'{}'", s.name)).collect::<Vec<_>>().join(", ");
        let over: Vec<&StateReport> = states.iter().filter(|s| s.parts.iter().any(|p| p.over)).collect();
        if !over.is_empty() {
            warnings.push(format!("At {:.0} dBm a part passes its limit in {}. The parts' table says which, and the power it can take.", rc.power_dbm, quoted(over)));
        }
        // A pattern between solved ones has to carry the power the network says it does.
        if self.basis.lossless {
            let off: Vec<&StateReport> = states.iter().filter(|s| s.metrics.as_ref().is_some_and(|m| s.s_op_db < -3.0 && (m.rad_efficiency + s.part_loss - 1.0).abs() > 0.06)).collect();
            if !off.is_empty() {
                warnings.push(format!("The power doesn't add up in {}: the radiated pattern and the parts' loss disagree with the network, so gain there can't be trusted. Solving again with more far-field frequencies, or on a finer mesh, should settle it.", quoted(off)));
            }
        }
        let moved: Vec<&StateReport> = states.iter().filter(|s| s.parts.iter().any(|p| p.nonlinear && !p.over)).collect();
        if !moved.is_empty() {
            warnings.push(format!("At {:.0} dBm the RF swing is large against a varactor's bias in {}. Its capacitance moves with the signal, which this linear model doesn't include: expect detuning and harmonics.", rc.power_dbm, quoted(moved)));
        }
        let tuning = (lo <= hi).then_some((lo, hi));
        Ok(Report { states, envelope, tuning, coverage, beam_span_deg, met_by, warnings })
    }

    /// Worst feed reflection over a band, dB, on the band's own samples.
    fn worst_in_band(&self, rc: &Reconfig, settings: &[Setting], band: &Network) -> f64 {
        let mut worst = f64::NEG_INFINITY;
        for (f, m) in band.f.iter().zip(&band.s) {
            let Some(r) = reduce(m, &self.bind, &self.gammas(rc, settings, *f)) else { return f64::INFINITY };
            for k in 0..r.s.len() {
                worst = worst.max(db20(r.s[k][k].norm()));
            }
        }
        worst
    }

    fn band_samples(&self, t: &Target) -> Network {
        self.basis.network.interpolate(&linspace(t.f_lo, t.f_hi, 9))
    }

    /// How these settings do against a target.
    pub fn score(&self, rc: &Reconfig, settings: &[Setting], t: &Target) -> Score {
        let band = self.band_samples(t);
        let worst = self.worst_in_band(rc, settings, &band);
        self.score_with(rc, settings, t, worst)
    }

    fn score_with(&self, rc: &Reconfig, settings: &[Setting], t: &Target, worst_s_db: f64) -> Score {
        let mut penalty = (worst_s_db - t.limit_db).max(0.0);
        if worst_s_db.is_infinite() {
            penalty = 60.0;
        }
        let needs_far = t.beam.is_some() || t.min_gain_dbi.is_some() || t.pol.is_some();
        let far = self.far_at(rc, settings, 0, t.center());
        let mut out = Score { worst_s_db, far_f: far.as_ref().map(|x| x.1), ..Default::default() };
        match &far {
            Some((ff, _)) => {
                let m = ff.metrics();
                out.peak = Some([m.peak_theta, m.peak_phi]);
                let (gain, ar, rhcp) = match t.beam {
                    Some([theta, phi]) => {
                        let ti = nearest(&ff.theta, theta);
                        let pi = nearest(&ff.phi, phi.rem_euclid(360.0));
                        let (r, l) = ff.circular(ti, pi);
                        (ff.value_db(ti, pi, Quantity::RealizedGain, Pol::Total), ff.axial_ratio_db(ti, pi), r.norm() >= l.norm())
                    }
                    None => (m.peak_realized_dbi, m.axial_ratio_db, m.rhcp_at_peak),
                };
                out.gain_dbi = Some(gain);
                out.axial_ratio_db = Some(ar);
                out.rhcp = rhcp;
                if let Some(min) = t.min_gain_dbi {
                    penalty += (min - gain).max(0.0);
                }
                if let Some(p) = t.pol {
                    penalty += match p {
                        PolGoal::Linear => (LINEAR_AR_DB - ar).max(0.0),
                        PolGoal::Rhcp | PolGoal::Lhcp => {
                            let right_hand = (p == PolGoal::Rhcp) == rhcp;
                            (ar - CIRCULAR_AR_DB).max(0.0) + if right_hand { 0.0 } else { 20.0 }
                        }
                    };
                }
            }
            None if needs_far => penalty += 40.0,
            None => {}
        }
        out.penalty = penalty;
        out.met = penalty <= 0.0 && worst_s_db.is_finite();
        out
    }

    /// The best settings for each target, on this solve. Every combination if there are few
    /// enough, otherwise a hill climb from several starts.
    pub fn search(&self, rc: &Reconfig, volts: usize) -> Vec<Found> {
        let options: Vec<Vec<Setting>> = rc.elements.iter().map(|e| e.device.options(volts)).collect();
        let total = count_combinations(&options);
        let exhaustive = total <= MAX_EXHAUSTIVE;
        rc.targets
            .iter()
            .map(|t| {
                let band = self.band_samples(t);
                let mut ranked: Vec<(f64, Vec<Setting>)> = if exhaustive {
                    (0..total)
                        .into_par_iter()
                        .map(|i| {
                            let s = nth_combination(&options, i);
                            (self.worst_in_band(rc, &s, &band), s)
                        })
                        .collect()
                } else {
                    self.climb(rc, &options, &band)
                };
                let tried = if exhaustive { total } else { ranked.len() };
                ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
                ranked.truncate(PATTERNS_JUDGED);
                let judged: Vec<(Score, Vec<Setting>)> = ranked.into_par_iter().map(|(w, s)| (self.score_with(rc, &s, t, w), s)).collect();
                let best = judged.into_iter().min_by(|a, b| a.0.penalty.total_cmp(&b.0.penalty).then_with(|| b.0.gain_dbi.unwrap_or(f64::NEG_INFINITY).total_cmp(&a.0.gain_dbi.unwrap_or(f64::NEG_INFINITY))).then_with(|| a.0.worst_s_db.total_cmp(&b.0.worst_s_db)));
                let (score, settings) = best.unwrap_or_default();
                Found { target: t.name.clone(), settings, score, tried, exhaustive }
            })
            .collect()
    }

    /// Hill climbs over the settings by match alone, one part at a time, from spread-out starts.
    fn climb(&self, rc: &Reconfig, options: &[Vec<Setting>], band: &Network) -> Vec<(f64, Vec<Setting>)> {
        (0..16usize)
            .into_par_iter()
            .map(|start| {
                let mut rng = proof::Rng::new(start as u64 + 1);
                let mut s: Vec<Setting> = options.iter().map(|o| o[(rng.uniform() * o.len() as f64) as usize % o.len()]).collect();
                let mut value = self.worst_in_band(rc, &s, band);
                for _ in 0..4 {
                    let mut moved = false;
                    for (i, o) in options.iter().enumerate() {
                        for choice in o {
                            let keep = s[i];
                            s[i] = *choice;
                            let v = self.worst_in_band(rc, &s, band);
                            if v < value {
                                value = v;
                                moved = true;
                            } else {
                                s[i] = keep;
                            }
                        }
                    }
                    if !moved {
                        break;
                    }
                }
                (value, s)
            })
            .collect()
    }

    /// Resonance against bias from a state's settings. `which`: one element, or every varactor
    /// together when `None`, as on a shared bias line.
    pub fn tuning(&self, rc: &Reconfig, base: &TuneState, which: Option<usize>, n: usize) -> Vec<TunePoint> {
        let varactors: Vec<usize> = rc.elements.iter().enumerate().filter(|(i, e)| matches!(e.device.model, Model::Varactor { .. }) && which.is_none_or(|w| w == *i)).map(|(i, _)| i).collect();
        let Some(&first) = varactors.first() else { return Vec::new() };
        let Model::Varactor { v_max, cp, .. } = rc.elements[first].device.model else { return Vec::new() };
        let n = n.max(2);
        let settings = rc.settings_of(base);
        (0..n)
            .into_par_iter()
            .filter_map(|k| {
                let volts = v_max * (k as f64 / (n - 1) as f64).powi(2);
                let mut s = settings.clone();
                for i in &varactors {
                    s[*i] = Setting::Volts(volts);
                }
                let net = self.network(rc, &s).ok()?;
                let res = net.resonances(0);
                let best = res.iter().min_by(|a, b| a.1.total_cmp(&b.1)).copied();
                let band = best.and_then(|(f, _)| net.bandwidths(0, COVERAGE_LIMIT_DB).into_iter().find(|b| b.0 <= f && f <= b.1));
                let part_efficiency = best.and_then(|(f, _)| self.at(rc, &s, f)).map_or(f64::NAN, |r| {
                    let accepted = 1.0 - r.s[0][0].norm_sqr();
                    let lost: f64 = (0..rc.elements.len()).map(|l| (r.b[l][0].norm_sqr() - r.a[l][0].norm_sqr()).max(0.0)).sum();
                    if accepted > 1e-9 { (1.0 - lost / accepted).clamp(0.0, 1.0) } else { f64::NAN }
                });
                let cap = rc.elements[first].device.junction_capacitance(Setting::Volts(volts)).unwrap_or(0.0) + cp;
                Some(TunePoint { volts, capacitance: cap, f_res: best.map(|b| b.0), depth_db: best.map_or(f64::NAN, |b| b.1), band, part_efficiency })
            })
            .collect()
    }

    /// The chance a build still meets a target in this state, when each part's values spread:
    /// `spread` is the relative 1σ of capacitance, resistance and inductance.
    pub fn robust(&self, rc: &Reconfig, state: &TuneState, t: &Target, spread: f64, seed: u64) -> RobustReport {
        let settings = rc.settings_of(state);
        let band = self.band_samples(t);
        let mut factors = Vec::with_capacity(rc.elements.len());
        for e in &rc.elements {
            factors.push(Factor::new(&format!("{} value", e.name), "×", Dist::LogNormal { median: 1.0, sigma: spread }, "Assumed part-to-part spread"));
        }
        robust("Worst reflection in band", "dB", &factors, Some(ProofTarget::at_most(t.limit_db)), Evidence::Simulated, RobustOptions { samples: 200, seed }, |x| {
            let mut varied = rc.clone();
            for (e, k) in varied.elements.iter_mut().zip(x) {
                e.device.scale_values(*k);
            }
            self.worst_in_band(&varied, &settings, &band)
        })
    }
}

impl Device {
    /// Every value that sets its impedance, multiplied by `k`: how one part differs from the next.
    fn scale_values(&mut self, k: f64) {
        match &mut self.model {
            Model::Switch { r_on, c_off, r_off, ls, .. } => {
                *r_on *= k;
                *c_off *= k;
                *r_off /= k;
                *ls *= k;
            }
            Model::Varactor { cj0, cp, rs, ls, .. } => {
                *cj0 *= k;
                *cp *= k;
                *rs *= k;
                *ls *= k;
            }
            Model::Fixed { r, l, c } => {
                *r *= k;
                *l *= k;
                *c *= k;
            }
        }
    }
}

fn nearest(values: &[f64], v: f64) -> usize {
    values.iter().enumerate().min_by(|a, b| (a.1 - v).abs().total_cmp(&(b.1 - v).abs())).map_or(0, |x| x.0)
}

fn polarisation_words(m: &Metrics) -> String {
    if m.axial_ratio_db > LINEAR_AR_DB {
        "Linear".to_string()
    } else if m.axial_ratio_db <= CIRCULAR_AR_DB {
        format!("{} ({:.1} dB)", if m.rhcp_at_peak { "RHCP" } else { "LHCP" }, m.axial_ratio_db)
    } else {
        format!("Elliptical ({:.1} dB)", m.axial_ratio_db)
    }
}

/// Angle between two directions given as `[θ, φ]`, degrees.
pub fn angle_between(a: [f64; 2], b: [f64; 2]) -> f64 {
    let v = |d: [f64; 2]| {
        let (st, ct) = d[0].to_radians().sin_cos();
        let (sp, cp) = d[1].to_radians().sin_cos();
        [st * cp, st * sp, ct]
    };
    let (x, y) = (v(a), v(b));
    (x[0] * y[0] + x[1] * y[1] + x[2] * y[2]).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Contiguous stretches of a curve under `limit`, edges interpolated.
fn bands_under(curve: &[(f64, f64)], limit: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = None;
    for k in 0..curve.len() {
        let below = curve[k].1 < limit;
        let edge = |a: (f64, f64), b: (f64, f64)| a.0 + (limit - a.1) / (b.1 - a.1) * (b.0 - a.0);
        match (below, start) {
            (true, None) => start = Some(if k == 0 { curve[0].0 } else { edge(curve[k - 1], curve[k]) }),
            (false, Some(lo)) => {
                out.push((lo, edge(curve[k - 1], curve[k])));
                start = None;
            }
            _ => {}
        }
    }
    if let (Some(lo), Some(last)) = (start, curve.last()) {
        out.push((lo, last.0));
    }
    out
}

/// What the numbers rest on, in plain words, for the page and the assistant.
pub fn assumptions(rc: &Reconfig, basis: &SimResult) -> Vec<String> {
    let mut out = vec![
        format!("Evidence: {}. Solved with {}; the parts are circuit models, and nothing here has been measured.", Evidence::Simulated.label(), basis.solver),
        "Parts are linear: a fixed impedance at each frequency. Harmonics, intermodulation and the RF swing moving a varactor's capacitance are not modelled.".to_string(),
        "Bias lines are not in the model unless you draw them as parts. A real line can detune or radiate, so draw it once the layout is known, and solve again.".to_string(),
        "Each state is a steady one. Switching time, and settling after a change of bias, aren't simulated.".to_string(),
    ];
    if rc.elements.iter().any(|e| !e.device.source.is_empty()) {
        out.push("Part values are typical datasheet figures, some of them assumed: see each part's note. Your bias point, package and frequency change them, so check against your own datasheet.".to_string());
    }
    if basis.solver.contains("MoM") {
        out.push("The method of moments treats a gap as a point in the metal, with what capacitance the gap's shape has. Pads and the part's body add more: set it as the mount's pad capacitance.".to_string());
    }
    out
}

/// The state table: the bias truth table a firmware engineer needs, and each state's figures.
pub fn truth_table_csv(rc: &Reconfig, report: &Report) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("state");
    for e in &rc.elements {
        out.push_str(&format!(",{}", quote(&format!("{} ({})", e.name, e.device.part))));
    }
    out.push_str(",bias_ma,bias_mw,f_op_ghz,s11_db,band_lo_ghz,band_hi_ghz,realized_gain_dbi,peak_theta_deg,peak_phi_deg,polarisation,part_loss_percent,total_efficiency_percent\n");
    for s in &report.states {
        out.push_str(&quote(&s.name));
        for (e, v) in rc.elements.iter().zip(&s.settings) {
            let (volts, ma) = e.device.dc(*v);
            let word = e.device.describe(*v);
            out.push_str(&format!(",{}", quote(&if ma > 0.0 { format!("{word} {ma:.0} mA") } else if volts != 0.0 { format!("{word} {volts:+.1} V") } else { word })));
        }
        let band = s.bands.iter().find(|b| b.0 <= s.f_op && s.f_op <= b.1).copied();
        let num = |v: Option<f64>, d: usize| v.filter(|x| x.is_finite()).map_or(String::new(), |x| format!("{x:.d$}"));
        out.push_str(&format!(
            ",{:.3},{:.3},{:.6},{:.2},{},{},{},{},{},{},{:.2},{}\n",
            s.dc_ma,
            s.dc_mw,
            s.f_op / 1e9,
            s.s_op_db,
            num(band.map(|b| b.0 / 1e9), 6),
            num(band.map(|b| b.1 / 1e9), 6),
            num(s.metrics.as_ref().map(|m| m.peak_realized_dbi), 2),
            num(s.metrics.as_ref().map(|m| m.peak_theta), 1),
            num(s.metrics.as_ref().map(|m| m.peak_phi), 1),
            quote(&s.polarisation),
            s.part_loss * 100.0,
            num(s.metrics.as_ref().map(|m| m.total_efficiency * 100.0), 1),
        ));
    }
    out
}

/// The parts and states a reconfigurable template starts with. The port names are the
/// template's own.
pub fn for_template(id: &str, f0: f64) -> Option<Reconfig> {
    let device = |part: &str| Device::by_part(part).expect("a part in the library");
    let element = |name: &str, port: &str, part: &str| Element { name: name.into(), port: port.into(), device: device(part), mount: Mount::default() };
    let state = |name: &str, settings: Vec<Setting>, f_op: Option<f64>| TuneState { name: name.into(), settings, f_op };
    use Setting::{Off, On, Volts};
    // At 2.45 GHz a low-capacitance flip chip: a 0.23 pF package diode barely opens there,
    // and the outer sections keep working through it.
    const MADP: &str = "MADP-000907-14020P PIN";
    let rc = match id {
        "reconfig_dipole" => Reconfig {
            elements: vec![element("D1 up", "D1 up", MADP), element("D1 down", "D1 down", MADP), element("D2 up", "D2 up", MADP), element("D2 down", "D2 down", MADP)],
            states: vec![state("Short: all off", vec![Off; 4], None), state("Middle: D1 on", vec![On, On, Off, Off], None), state("Long: all on", vec![On; 4], None)],
            ..Default::default()
        },
        "reconfig_pifa" => Reconfig {
            elements: vec![element("Tuner", "tuner", "SMV1405 varactor")],
            states: [0.0, 1.0, 3.0, 8.0, 15.0, 30.0].iter().map(|v| state(&format!("{v:.0} V"), vec![Volts(*v)], None)).collect(),
            ..Default::default()
        },
        "reconfig_parasitic" => {
            // A shorted parasitic reflects and an open one all but vanishes, so the beam
            // leaves through the open one.
            let on_except = |k: usize| (0..4).map(|i| if i == k { Off } else { On }).collect::<Vec<_>>();
            Reconfig {
                elements: vec![element("D +x", "D +x", "SMP1345-079LF PIN"), element("D +y", "D +y", "SMP1345-079LF PIN"), element("D -x", "D -x", "SMP1345-079LF PIN"), element("D -y", "D -y", "SMP1345-079LF PIN")],
                states: vec![state("Beam +x", on_except(0), Some(f0)), state("Beam +y", on_except(1), Some(f0)), state("Beam -x", on_except(2), Some(f0)), state("Beam -y", on_except(3), Some(f0)), state("All round: all off", vec![Off; 4], Some(f0))],
                targets: vec![Target { beam: Some([90.0, 0.0]), min_gain_dbi: Some(7.0), ..Target::band("Beam towards +x", f0 * 0.98, f0 * 1.02) }, Target { beam: Some([90.0, 180.0]), min_gain_dbi: Some(7.0), ..Target::band("Beam towards -x", f0 * 0.98, f0 * 1.02) }],
                ..Default::default()
            }
        }
        "reconfig_pol_patch" => Reconfig {
            elements: vec![element("A1", "A1", MADP), element("A2", "A2", MADP), element("B1", "B1", MADP), element("B2", "B2", MADP)],
            states: vec![state("Linear: all on", vec![On; 4], Some(f0)), state("Right-hand: A on", vec![On, On, Off, Off], Some(f0)), state("Left-hand: B on", vec![Off, Off, On, On], Some(f0))],
            targets: vec![Target { pol: Some(PolGoal::Rhcp), limit_db: -6.0, ..Target::band("Right-hand circular", f0 * 0.995, f0 * 1.005) }, Target { pol: Some(PolGoal::Lhcp), limit_db: -6.0, ..Target::band("Left-hand circular", f0 * 0.995, f0 * 1.005) }],
            ..Default::default()
        },
        _ => return None,
    };
    Some(rc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Geometry, PortDef, Shape, Solid};
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::Project;

    fn c(re: f64, im: f64) -> C64 {
        C64::new(re, im)
    }

    fn ideal() -> Device {
        Device::by_part("Ideal switch").unwrap()
    }

    fn close(a: C64, b: C64, tol: f64) {
        assert!((a - b).norm() <= tol, "{a} vs {b}");
    }

    /// A line of phase θ, closed by a short or an open at its far end.
    #[test]
    fn a_line_closed_by_a_short_or_an_open_gives_the_textbook_reflection() {
        let t = C64::from_polar(1.0, -0.7);
        let s = vec![vec![c(0.0, 0.0), t], vec![t, c(0.0, 0.0)]];
        let bind = Binding { feeds: vec![0], loads: vec![1] };
        let short = reduce(&s, &bind, &[c(-1.0, 0.0)]).unwrap();
        close(short.s[0][0], -(t * t), 1e-12);
        let open = reduce(&s, &bind, &[c(1.0, 0.0)]).unwrap();
        close(open.s[0][0], t * t, 1e-12);
        // Matched: nothing comes back.
        let matched = reduce(&s, &bind, &[c(0.0, 0.0)]).unwrap();
        close(matched.s[0][0], c(0.0, 0.0), 1e-12);
    }

    /// The closed form against the impedance form `Zff − Zfl (Zll + ZL)⁻¹ Zlf`, on a matrix
    /// with nothing special about it.
    #[test]
    fn closing_ports_agrees_with_the_impedance_formula() {
        let z = vec![
            vec![c(40.0, 12.0), c(5.0, -3.0), c(1.0, 2.0), c(-2.0, 4.0)],
            vec![c(5.0, -3.0), c(75.0, -20.0), c(-4.0, 6.0), c(3.0, 1.0)],
            vec![c(1.0, 2.0), c(-4.0, 6.0), c(120.0, 33.0), c(7.0, -5.0)],
            vec![c(-2.0, 4.0), c(3.0, 1.0), c(7.0, -5.0), c(60.0, 25.0)],
        ];
        let z0 = vec![50.0, 75.0, 50.0, 100.0];
        let net = Network::from_z(vec![1e9], std::slice::from_ref(&z), z0.clone());
        let bind = Binding { feeds: vec![0, 3], loads: vec![1, 2] };
        let zl = [c(10.0, 30.0), c(2.0, -80.0)];
        let gamma: Vec<C64> = zl.iter().zip([z0[1], z0[2]]).map(|(z, r)| (*z - r) / (*z + r)).collect();
        let r = reduce(&net.s[0], &bind, &gamma).unwrap();
        let (l, f) = ([1usize, 2], [0usize, 3]);
        let mut zll = vec![vec![z[1][1] + zl[0], z[1][2]], vec![z[2][1], z[2][2] + zl[1]]];
        let mut rhs: Matrix = l.iter().map(|i| f.iter().map(|j| z[*i][*j]).collect()).collect();
        solve_small(&mut zll, &mut rhs).unwrap();
        let reduced_z: Vec<Matrix> = vec![
            f.iter()
                .map(|i| {
                    f.iter()
                        .enumerate()
                        .map(|(jx, j)| z[*i][*j] - l.iter().enumerate().map(|(lx, m)| z[*i][*m] * rhs[lx][jx]).sum::<C64>())
                        .collect()
                })
                .collect(),
        ];
        let want = Network::from_z(vec![1e9], &reduced_z, vec![z0[0], z0[3]]);
        for i in 0..2 {
            for j in 0..2 {
                close(r.s[i][j], want.s[0][i][j], 1e-10);
            }
        }
    }

    #[test]
    fn a_pin_diode_is_a_small_resistor_on_and_a_small_capacitor_off() {
        let d = Device::by_part("BAR64-02V PIN").unwrap();
        let f = 2.45e9;
        let w = 2.0 * PI * f;
        let on = d.z(Setting::On, f);
        assert!((on.re - 2.1).abs() < 1e-12);
        assert!((on.im - w * 0.6e-9).abs() < 1e-6, "{on}");
        let off = d.z(Setting::Off, f);
        // 0.23 pF is about −282 Ω here, with 5 kΩ across it barely mattering.
        assert!((off.im + 1.0 / (w * 0.23e-12) - w * 0.6e-9).abs() < 2.0, "{off}");
        assert!(off.re > 5.0 && off.re < 25.0, "{off}");
    }

    #[test]
    fn the_varactor_follows_its_published_curve() {
        let d = Device::by_part("SMV1405 varactor").unwrap();
        let cj = |v: f64| d.junction_capacitance(Setting::Volts(v)).unwrap();
        assert!((cj(0.0) - 2.37e-12).abs() < 1e-18);
        let want = 2.37e-12 / (1.0 + 4.0 / 0.77f64).powf(0.5);
        assert!((cj(4.0) - want).abs() < 1e-18);
        assert!(cj(30.0) < cj(4.0) && cj(4.0) < cj(0.0));
        // Well below self-resonance it's a capacitor of Cj + Cp.
        let z = d.z(Setting::Volts(4.0), 50e6);
        let c_seen = -1.0 / (2.0 * PI * 50e6 * z.im);
        assert!((c_seen - (want + 0.29e-12)).abs() < 2e-14, "{c_seen}");
        assert!(z.re > 0.0);
    }

    #[test]
    fn perfect_parts_really_are_a_short_and_an_open() {
        let d = ideal();
        assert!(d.z(Setting::On, 1e9).norm() < 1e-9);
        assert!(d.z(Setting::Off, 1e9).norm() >= OPEN_OHMS * 0.99);
    }

    #[test]
    fn a_mount_adds_up() {
        let f = 1e9;
        let w = 2.0 * PI * f;
        let mut e = Element { name: "x".into(), port: "p".into(), device: ideal(), mount: Mount::default() };
        // A DC block turns the short into a capacitor.
        e.mount.c_block = 10e-12;
        let z = e.z_load(Setting::On, f);
        assert!((z.im + 1.0 / (w * 10e-12)).abs() < 1e-6, "{z}");
        // A choke across an open is the choke.
        e.mount = Mount { l_choke: 47e-9, ..Default::default() };
        let z = e.z_load(Setting::Off, f);
        assert!((z.im - w * 47e-9).abs() < 1e-3, "{z}");
        // Past its self-resonance a choke looks capacitive.
        e.mount.choke_srf = 0.5e9;
        assert!(e.z_load(Setting::Off, f).im < 0.0);
        e.mount.choke_srf = 5e9;
        assert!(e.z_load(Setting::Off, f).im > 0.0);
    }

    fn switched_wire(l: f64, ports: Vec<PortDef>) -> Geometry {
        Geometry { solids: vec![Solid { name: "wire".into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[0.0, 0.0, -l / 2.0], [0.0, 0.0, l / 2.0]], r: 0.3e-3 } }], ports, ..Default::default() }
    }

    fn gap(name: &str, z: f64, z0: f64) -> PortDef {
        PortDef { name: name.into(), a: [0.0, 0.0, z - 0.5e-3], b: [0.0, 0.0, z + 0.5e-3], width: 0.0, z0 }
    }

    /// A far field on the app's route: 1 W incident per port, every other port matched.
    fn mom_basis(g: &Geometry, freqs: &[f64], far: &[f64]) -> SimResult {
        let opts = MomOptions { segments_per_lambda: 60.0, theta_step: 5.0, phi_step: 5.0, ..Default::default() };
        let p = MomProblem::new(g, *freqs.last().unwrap(), opts).unwrap();
        let z: Vec<_> = freqs.iter().map(|f| p.solve(*f, false).unwrap().z).collect();
        let network = Network::from_z(freqs.to_vec(), &z, g.ports.iter().map(|x| x.z0).collect());
        let mut per_port: Vec<Vec<FarField>> = vec![Vec::new(); g.ports.len()];
        for f in far {
            for (port, ff) in p.solve(*f, true).unwrap().far.into_iter().enumerate() {
                per_port[port].push(ff);
            }
        }
        SimResult { label: "t".into(), solver: "MoM".into(), network, far: per_port, far_f: far.to_vec(), lossless: true, ..Default::default() }
    }

    fn names(g: &Geometry) -> Vec<String> {
        g.ports.iter().map(|p| p.name.clone()).collect()
    }

    fn one_part(port: &str, device: Device) -> Reconfig {
        Reconfig { elements: vec![Element { name: port.into(), port: port.into(), device, mount: Mount::default() }], ..Default::default() }
    }

    /// The method's real test, against a solve that never heard of it: closing a port
    /// with resistor R must match the same model solved with R as that port's own reference,
    /// where the solver terminates it directly. Complex far field included.
    #[test]
    fn closing_a_port_matches_solving_with_that_resistor() {
        let freqs = crate::linspace(0.7e9, 1.3e9, 13);
        let far = [1.0e9];
        let make = |r: f64| switched_wire(0.15, vec![gap("feed", 0.0, 50.0), gap("load", 0.04, r)]);
        let (a, b) = (make(50.0), make(20.0));
        let (basis, direct) = (mom_basis(&a, &freqs, &far), mom_basis(&b, &freqs, &far));
        let device = Device { part: "R".into(), model: Model::Fixed { r: 20.0, l: 0.0, c: 0.0 }, v_limit: 0.0, i_limit: 0.0, p_limit: 0.0, source: String::new() };
        let rc = one_part("load", device);
        let sv = Solved::new(&basis, &names(&a), &rc).unwrap();
        let settings = [Setting::Off];
        let net = sv.network(&rc, &settings).unwrap();
        for k in 0..net.f.len() {
            close(net.s[k][0][0], direct.network.s[k][0][0], 1e-9);
        }
        let ff = sv.far(&rc, &settings, 0, 0).unwrap();
        let want = &direct.far[0][0];
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                close(ff.e_theta[ti][pi], want.e_theta[ti][pi], 1e-6);
                close(ff.e_phi[ti][pi], want.e_phi[ti][pi], 1e-6);
            }
        }
        assert!((ff.p_accepted - want.p_accepted).abs() < 1e-9, "{} vs {}", ff.p_accepted, want.p_accepted);
    }

    /// Against different metal: a closed switch must act like an unbroken wire, and an open
    /// one like a wire cut there. Independent geometry, so a modelling error shows.
    #[test]
    fn a_closed_switch_is_unbroken_wire_and_an_open_one_is_a_cut() {
        let (long, short) = (0.17, 0.11);
        let cut = short / 2.0;
        let freqs = crate::linspace(0.6e9, 1.8e9, 49);
        let g = switched_wire(long, vec![gap("feed", 0.0, 50.0), gap("up", cut, 50.0), gap("down", -cut, 50.0)]);
        let basis = mom_basis(&g, &freqs, &[]);
        let mut rc = one_part("up", ideal());
        rc.elements.push(Element { name: "down".into(), port: "down".into(), device: ideal(), mount: Mount::default() });
        let sv = Solved::new(&basis, &names(&g), &rc).unwrap();

        let direct = |l: f64| {
            let g = switched_wire(l, vec![gap("feed", 0.0, 50.0)]);
            mom_basis(&g, &freqs, &[]).network
        };
        let res = |n: &Network| n.resonances(0).iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0;
        let (on, off) = (sv.network(&rc, &[Setting::On, Setting::On]).unwrap(), sv.network(&rc, &[Setting::Off, Setting::Off]).unwrap());
        let (want_on, want_off) = (res(&direct(long)), res(&direct(short)));
        println!("on {} vs {want_on}, off {} vs {want_off}", res(&on), res(&off));
        assert!((res(&on) / want_on - 1.0).abs() < 0.005, "{} vs {want_on}", res(&on));
        // The cut wire stops a little past the cut, by the gap's half width.
        assert!((res(&off) / want_off - 1.0).abs() < 0.03, "{} vs {want_off}", res(&off));
        assert!(res(&off) > 1.3 * res(&on));
    }

    /// Power in has to be radiated or lost in the part. A figure from S and one from the far
    /// field have to agree.
    #[test]
    fn power_in_is_radiated_or_lost_in_the_parts() {
        let freqs = crate::linspace(0.7e9, 1.3e9, 25);
        let g = switched_wire(0.15, vec![gap("feed", 0.0, 50.0), gap("load", 0.03, 50.0)]);
        let basis = mom_basis(&g, &freqs, &[1e9]);
        let device = Device { part: "R".into(), model: Model::Fixed { r: 25.0, l: 0.0, c: 0.0 }, v_limit: 0.0, i_limit: 0.0, p_limit: 0.0, source: String::new() };
        let rc = Reconfig { states: vec![TuneState { name: "s".into(), settings: vec![Setting::Off], f_op: Some(1e9) }], power_dbm: 30.0, ..one_part("load", device) };
        let sv = Solved::new(&basis, &names(&g), &rc).unwrap();
        let r = sv.state_report(&rc, &rc.states[0]).unwrap();
        let m = r.metrics.as_ref().unwrap();
        let rad_eff = m.rad_efficiency;
        println!("radiation efficiency {rad_eff}, part loss {}", r.part_loss);
        assert!(r.part_loss > 0.05, "{}", r.part_loss);
        assert!((rad_eff + r.part_loss - 1.0).abs() < 0.03, "radiated {rad_eff} + lost {} ≠ 1", r.part_loss);
        // 1 W: the same loss from the wave figures and from I²R.
        let p = &r.parts[0];
        assert!((p.p_device - p.p_total).abs() < 1e-3 * p.p_total.max(1e-6), "{} vs {}", p.p_device, p.p_total);
        let accepted = 1.0 - 10f64.powf(r.s_op_db / 10.0);
        assert!((p.p_total - r.part_loss * accepted).abs() < 1e-3, "{} vs {}", p.p_total, r.part_loss * accepted);
    }

    fn template_basis(id: &str) -> (Project, Geometry, SimResult) {
        let mut p = Project::from_template(id, None).unwrap();
        p.setup.far_step = 5.0;
        let g = p.geometry().unwrap();
        let opts = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, theta_step: 5.0, phi_step: 5.0 };
        let problem = MomProblem::new(&g, p.setup.f_hi, opts).unwrap();
        let fs = crate::linspace(p.setup.f_lo, p.setup.f_hi, p.setup.mom_samples);
        let z: Vec<_> = fs.iter().map(|f| problem.solve(*f, false).unwrap().z).collect();
        let net = Network::from_z(fs, &z, g.ports.iter().map(|x| x.z0).collect());
        let network = net.interpolate(&crate::linspace(p.setup.f_lo, p.setup.f_hi, p.setup.points));
        let far_f = p.reconfig.far_plan(&p.setup, FAR_SAMPLES_MOM);
        let mut far: Vec<Vec<FarField>> = vec![Vec::new(); g.ports.len()];
        for f in &far_f {
            for (port, ff) in problem.solve(*f, true).unwrap().far.into_iter().enumerate() {
                far[port].push(ff);
            }
        }
        (p, g, SimResult { label: id.into(), solver: "MoM".into(), network, far, far_f, lossless: true, ..Default::default() })
    }

    /// The templates say what each state does, so they have to be held to it.
    #[test]
    fn the_switched_dipole_steps_down_a_band_per_state() {
        let (p, g, basis) = template_basis("reconfig_dipole");
        let sv = Solved::new(&basis, &names(&g), &p.reconfig).unwrap();
        let r = sv.report(&p.reconfig).unwrap();
        let f: Vec<f64> = r.states.iter().map(|s| s.f_op).collect();
        for s in &r.states {
            println!("{}: {:.3} GHz, {:.1} dB, {:.2} dBi, θ {:.0}, loss {:.1} %", s.name, s.f_op / 1e9, s.s_op_db, s.metrics.as_ref().unwrap().peak_realized_dbi, s.metrics.as_ref().unwrap().peak_theta, s.part_loss * 100.0);
        }
        assert!(f[0] > 1.2 * f[1] && f[1] > 1.2 * f[2], "{f:?}");
        for s in &r.states {
            assert!(s.s_op_db < -10.0, "{}: {}", s.name, s.s_op_db);
        }
    }

    #[test]
    fn the_parasitic_array_turns_its_beam_with_the_switches() {
        let (p, g, basis) = template_basis("reconfig_parasitic");
        let sv = Solved::new(&basis, &names(&g), &p.reconfig).unwrap();
        let r = sv.report(&p.reconfig).unwrap();
        for s in &r.states {
            let m = s.metrics.as_ref().unwrap();
            println!("{}: S11 {:.1} dB, {:.2} dBi at θ {:.0} φ {:.0}, loss {:.1} %", s.name, s.s_op_db, m.peak_realized_dbi, m.peak_theta, m.peak_phi, s.part_loss * 100.0);
        }
        for (k, phi) in [(0usize, 0.0), (1, 90.0), (2, 180.0), (3, 270.0)] {
            let m = r.states[k].metrics.as_ref().unwrap();
            let off = ((m.peak_phi - phi + 180.0).rem_euclid(360.0) - 180.0).abs();
            assert!(off < 15.0, "{}: φ {} wanted {phi}", r.states[k].name, m.peak_phi);
            assert!(m.peak_realized_dbi > 5.0, "{}: {}", r.states[k].name, m.peak_realized_dbi);
            assert!(r.states[k].s_op_db < -6.0, "{}: {}", r.states[k].name, r.states[k].s_op_db);
        }
        assert!(r.beam_span_deg > 90.0, "{}", r.beam_span_deg);
        // The search has to pick the state that points the right way, by itself.
        let found = sv.search(&p.reconfig, 5);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|x| x.exhaustive && x.tried == 16));
        assert!(found[0].score.met, "{:?}", found[0].score);
        assert_eq!(found[0].settings, vec![Setting::Off, Setting::On, Setting::On, Setting::On], "{:?}", found[0].score);
        assert_eq!(found[1].settings, vec![Setting::On, Setting::On, Setting::Off, Setting::On], "{:?}", found[1].score);
    }

    #[test]
    fn the_varactor_pifa_tunes_down_as_the_bias_falls() {
        let (p, g, basis) = template_basis("reconfig_pifa");
        let sv = Solved::new(&basis, &names(&g), &p.reconfig).unwrap();
        let curve = sv.tuning(&p.reconfig, &p.reconfig.states[0], None, 9);
        for c in &curve {
            println!("{:5.1} V, {:.2} pF: {:?} GHz, {:.1} dB, parts pass {:.1} %", c.volts, c.capacitance * 1e12, c.f_res.map(|f| f / 1e9), c.depth_db, c.part_efficiency * 100.0);
        }
        let f: Vec<f64> = curve.iter().filter_map(|c| c.f_res).collect();
        assert_eq!(f.len(), curve.len(), "a resonance at every bias");
        assert!(f.windows(2).all(|w| w[1] >= w[0] * 0.999), "{f:?}");
        assert!(f[f.len() - 1] > 1.15 * f[0], "{f:?}");
        // More capacitance holds more current, so low bias loses more in the varactor's Rs.
        assert!(curve[0].part_efficiency < curve[curve.len() - 1].part_efficiency, "{:?}", curve.iter().map(|c| c.part_efficiency).collect::<Vec<_>>());
    }

    #[test]
    fn the_polarisation_patch_gives_both_hands_and_linear() {
        let (p, g, basis) = template_basis("reconfig_pol_patch");
        let rc = &p.reconfig;
        let sv = Solved::new(&basis, &names(&g), rc).unwrap();
        let r = sv.report(rc).unwrap();
        for s in &r.states {
            let m = s.metrics.as_ref().unwrap();
            println!("{}: S11 {:.1} dB, {:.2} dBi θ {:.0}, AR {:.1} dB, {}", s.name, s.s_op_db, m.peak_realized_dbi, m.peak_theta, m.axial_ratio_db, s.polarisation);
        }
        let m = |k: usize| r.states[k].metrics.as_ref().unwrap();
        assert!(m(0).axial_ratio_db > 10.0, "all on is linear: {}", m(0).axial_ratio_db);
        assert!(m(1).axial_ratio_db < 3.0 && m(2).axial_ratio_db < 3.0, "{} {}", m(1).axial_ratio_db, m(2).axial_ratio_db);
        assert_ne!(m(1).rhcp_at_peak, m(2).rhcp_at_peak, "the two diagonals turn opposite ways");
        for s in &r.states {
            assert!(s.s_op_db < -6.0, "{}: {}", s.name, s.s_op_db);
        }
    }

    /// A state's narrow resonance falls between the solved patterns, so it's judged on an
    /// interpolated one, which has to match a pattern really solved at that frequency.
    #[test]
    fn a_pattern_between_the_solved_ones_matches_one_solved_there() {
        for id in ["reconfig_pifa", "reconfig_dipole"] {
            let (p, g, basis) = template_basis(id);
            let rc = &p.reconfig;
            let sv = Solved::new(&basis, &names(&g), rc).unwrap();
            let opts = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, theta_step: 5.0, phi_step: 5.0 };
            let problem = MomProblem::new(&g, p.setup.f_hi, opts).unwrap();
            for state in &rc.states {
                let settings = rc.settings_of(state);
                let report = sv.state_report(rc, state).unwrap();
                let f = report.f_op;
                let (ff, at) = sv.far_at(rc, &settings, 0, f).unwrap();
                assert!((at - f).abs() < 1.0, "judged at {at}, not {f}");
                let solved_there = problem.solve(f, true).unwrap().far;
                let direct = SimResult { network: basis.network.clone(), far: solved_there.into_iter().map(|x| vec![x]).collect(), far_f: vec![f], lossless: true, ..Default::default() };
                let want = Solved::new(&direct, &names(&g), rc).unwrap().far(rc, &settings, 0, 0).unwrap();
                let (mut num, mut den) = (0.0, 0.0);
                for ti in 0..ff.theta.len() {
                    for pi in 0..ff.phi.len() {
                        num += (ff.e_theta[ti][pi] - want.e_theta[ti][pi]).norm_sqr() + (ff.e_phi[ti][pi] - want.e_phi[ti][pi]).norm_sqr();
                        den += want.e_theta[ti][pi].norm_sqr() + want.e_phi[ti][pi].norm_sqr();
                    }
                }
                let rel = (num / den).sqrt();
                let (a, b) = (ff.metrics(), want.metrics());
                println!("{id} {:18} {:.3} GHz: field off by {:.2} %, gain {:.2} vs {:.2} dBi, radiated {:.3} + lost {:.3}", state.name, f / 1e9, rel * 100.0, a.peak_realized_dbi, b.peak_realized_dbi, a.rad_efficiency, report.part_loss);
                assert!(rel < 0.03, "{id} {}: field off by {:.1} %", state.name, rel * 100.0);
                assert!((a.peak_realized_dbi - b.peak_realized_dbi).abs() < 0.2, "{id} {}: {} vs {}", state.name, a.peak_realized_dbi, b.peak_realized_dbi);
                assert!((a.rad_efficiency + report.part_loss - 1.0).abs() < 0.05, "{id} {}: radiated {} + lost {}", state.name, a.rad_efficiency, report.part_loss);
            }
        }
    }

    /// Too few patterns has to show, not read as a confident number.
    #[test]
    fn a_pattern_too_far_from_any_solved_one_is_called_out() {
        let (p, g, mut basis) = template_basis("reconfig_dipole");
        let rc = &p.reconfig;
        let clean = Solved::new(&basis, &names(&g), rc).unwrap().report(rc).unwrap();
        assert!(!clean.warnings.iter().any(|w| w.contains("add up")), "{:?}", clean.warnings);
        // Every other solved pattern, and every other again.
        for port in &mut basis.far {
            *port = port.iter().step_by(2).cloned().collect();
        }
        basis.far_f = basis.far_f.iter().step_by(2).copied().collect();
        let coarse = Solved::new(&basis, &names(&g), rc).unwrap().report(rc).unwrap();
        assert!(coarse.warnings.iter().any(|w| w.contains("add up") && w.contains("Long")), "{:?}", coarse.warnings);
    }

    #[test]
    fn states_are_named_and_listed() {
        let mut rc = one_part("a", ideal());
        rc.elements.push(Element { name: "b".into(), port: "b".into(), device: ideal(), mount: Mount::default() });
        rc.elements.push(Element { name: "v".into(), port: "v".into(), device: Device::by_part("SMV1405 varactor").unwrap(), mount: Mount::default() });
        let all = rc.all_combinations(3, 64).unwrap();
        assert_eq!(all.len(), 12);
        assert!(all.iter().any(|s| s.name == "All on, 0.0 V"), "{:?}", all.iter().map(|s| &s.name).collect::<Vec<_>>());
        assert!(all.iter().any(|s| s.name == "a on, 30.0 V"), "{:?}", all.iter().map(|s| &s.name).collect::<Vec<_>>());
        assert!(rc.all_combinations(3, 10).unwrap_err().to_string().contains("12"));
    }

    #[test]
    fn a_part_on_a_missing_port_or_no_feed_is_said_plainly() {
        let names = vec!["feed".to_string(), "gap".to_string()];
        assert!(one_part("nope", ideal()).bind(&names).unwrap_err().to_string().contains("nope"));
        let mut both = one_part("feed", ideal());
        both.elements.push(Element { name: "g".into(), port: "gap".into(), device: ideal(), mount: Mount::default() });
        assert!(both.bind(&names).unwrap_err().to_string().contains("feed"));
        assert_eq!(one_part("gap", ideal()).bind(&names).unwrap(), Binding { feeds: vec![0], loads: vec![1] });
    }

    #[test]
    fn a_setting_left_from_another_part_is_made_to_fit() {
        let v = Device::by_part("SMV1405 varactor").unwrap();
        assert_eq!(v.normalized(Setting::On), Setting::Volts(0.0));
        assert_eq!(v.normalized(Setting::Off), Setting::Volts(30.0));
        assert_eq!(v.normalized(Setting::Volts(99.0)), Setting::Volts(30.0));
        let d = ideal();
        assert_eq!(d.normalized(Setting::Volts(3.0)), Setting::On);
        assert_eq!(d.normalized(Setting::Volts(0.0)), Setting::Off);
    }

    #[test]
    fn a_varactor_is_warned_of_when_the_signal_passes_its_bias() {
        let freqs = crate::linspace(0.8e9, 1.2e9, 9);
        let g = switched_wire(0.15, vec![gap("feed", 0.0, 50.0), gap("v", 0.05, 50.0)]);
        let basis = mom_basis(&g, &freqs, &[1e9]);
        let mut rc = one_part("v", Device::by_part("SMV1405 varactor").unwrap());
        rc.states = vec![TuneState { name: "low bias".into(), settings: vec![Setting::Volts(0.5)], f_op: Some(1e9) }];
        rc.power_dbm = 40.0;
        let sv = Solved::new(&basis, &names(&g), &rc).unwrap();
        let hot = sv.state_report(&rc, &rc.states[0]).unwrap();
        assert!(hot.parts[0].over, "{:?}", hot.parts[0]);
        let limit = hot.parts[0].max_power_dbm.unwrap();
        assert!(limit < 40.0);
        // At exactly that power it sits on the limit.
        rc.power_dbm = limit - 0.01;
        assert!(!sv.state_report(&rc, &rc.states[0]).unwrap().parts[0].over);
        let report = {
            rc.power_dbm = 40.0;
            sv.report(&rc).unwrap()
        };
        assert!(report.warnings.iter().any(|w| w.contains("passes its limit")), "{:?}", report.warnings);
    }

    #[test]
    fn a_state_table_lists_bias_and_figures() {
        let freqs = crate::linspace(0.8e9, 1.2e9, 9);
        let g = switched_wire(0.15, vec![gap("feed", 0.0, 50.0), gap("d", 0.05, 50.0)]);
        let basis = mom_basis(&g, &freqs, &[1e9]);
        let mut rc = one_part("d", Device::by_part("BAR64-02V PIN").unwrap());
        rc.states = rc.default_states();
        let sv = Solved::new(&basis, &names(&g), &rc).unwrap();
        let report = sv.report(&rc).unwrap();
        let csv = truth_table_csv(&rc, &report);
        assert!(csv.lines().count() == 3, "{csv}");
        assert!(csv.contains("On 10 mA"), "{csv}");
        assert!(csv.contains("Off -5.0 V"), "{csv}");
        let mut p = Project::from_template("dipole", None).unwrap();
        p.reconfig = rc.clone();
        let html = crate::report::reconfig_html(&p, &report, &assumptions(&rc, &basis));
        for piece in ["All on", "BAR64-02V", "10.0 mA", "<svg", "What this assumes", "not measured"] {
            assert!(html.contains(piece), "no {piece}: {html}");
        }
        // Each state's Touchstone reads back as the state it was written from.
        let net = sv.network(&rc, &rc.settings_of(&rc.states[1])).unwrap();
        let back = Network::from_touchstone(&net.to_touchstone()).unwrap();
        assert_eq!(back.ports(), 1);
        assert!((back.s[3][0][0] - net.s[3][0][0]).norm() < 1e-9);
    }

    #[test]
    fn a_tolerance_run_names_the_part_that_matters() {
        let freqs = crate::linspace(0.7e9, 1.3e9, 25);
        let g = switched_wire(0.15, vec![gap("feed", 0.0, 50.0), gap("strong", 0.01, 50.0), gap("weak", 0.07, 50.0)]);
        let basis = mom_basis(&g, &freqs, &[]);
        let cap = Device { part: "C".into(), model: Model::Fixed { r: 0.1, l: 0.0, c: 2e-12 }, v_limit: 0.0, i_limit: 0.0, p_limit: 0.0, source: String::new() };
        let mut rc = one_part("strong", cap.clone());
        rc.elements.push(Element { name: "weak".into(), port: "weak".into(), device: cap, mount: Mount::default() });
        rc.states = vec![TuneState { name: "s".into(), settings: vec![Setting::Off, Setting::Off], f_op: None }];
        let sv = Solved::new(&basis, &names(&g), &rc).unwrap();
        let best = sv.state_report(&rc, &rc.states[0]).unwrap().f_op;
        let target = Target::band("t", best * 0.99, best * 1.01);
        let r = sv.robust(&rc, &rc.states[0], &target, 0.1, 3);
        assert!(r.prediction.high > r.prediction.low, "{:?}", r.prediction);
        let d = r.main_driver().unwrap();
        assert!(d.factor.contains("strong"), "{:?}", r.swings);
    }

    #[test]
    fn a_project_with_parts_survives_a_file_and_an_old_file_opens() {
        let p = Project::from_template("reconfig_dipole", None).unwrap();
        assert_eq!(p.reconfig.elements.len(), 4);
        let q = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(p.reconfig.states, q.reconfig.states);
        assert_eq!(q.reconfig.elements.len(), 4);
        assert_eq!(p.reconfig.elements[0].port, q.reconfig.elements[0].port);
        let (a, b) = (p.reconfig.elements[0].device.z(Setting::Off, 2e9), q.reconfig.elements[0].device.z(Setting::Off, 2e9));
        assert!((a - b).norm() < 1e-9 * a.norm(), "{a} vs {b}");
        let mut old: serde_json::Value = serde_json::from_str(&Project::from_template("dipole", None).unwrap().to_json().unwrap()).unwrap();
        old.as_object_mut().unwrap().remove("reconfig");
        let opened = Project::from_json(&old.to_string()).unwrap();
        assert!(!opened.reconfig.is_on());
    }

    #[test]
    fn the_far_plan_covers_targets_and_stays_in_the_sweep() {
        let p = Project::from_template("reconfig_dipole", None).unwrap();
        let mut rc = p.reconfig.clone();
        rc.targets = vec![Target::band("t", 1.6e9, 1.7e9), Target::band("outside", 9e9, 10e9)];
        let plan = rc.far_plan(&p.setup, 9);
        assert!(plan.iter().all(|f| (p.setup.f_lo..=p.setup.f_hi).contains(f)), "{plan:?}");
        assert!(plan.iter().any(|f| (f - 1.65e9).abs() < 0.01 * 1.65e9), "{plan:?}");
        assert!(plan.len() >= 7, "{plan:?}");
        assert!(plan.windows(2).all(|w| w[0] < w[1]), "{plan:?}");
    }

    #[test]
    fn angles_between_directions() {
        assert!((angle_between([90.0, 0.0], [90.0, 180.0]) - 180.0).abs() < 1e-9);
        assert!((angle_between([90.0, 0.0], [90.0, 90.0]) - 90.0).abs() < 1e-9);
        assert!(angle_between([0.0, 0.0], [0.0, 120.0]) < 1e-6, "the pole has no φ");
    }

    #[test]
    fn stretches_under_a_limit() {
        let c = [(1.0, -5.0), (2.0, -15.0), (3.0, -15.0), (4.0, -5.0), (5.0, -12.0)];
        let b = bands_under(&c, -10.0);
        assert_eq!(b.len(), 2);
        assert!((b[0].0 - 1.5).abs() < 1e-9 && (b[0].1 - 3.5).abs() < 1e-9, "{b:?}");
        assert!((b[1].1 - 5.0).abs() < 1e-9, "{b:?}");
    }
}

/// Surveys for setting the templates' numbers, not pass/fail:
/// `SURVEY="reconfig_pol_patch:t=0.08*lambda0,L=0.44*lambda0;..." cargo test -p antenna_core --release reconfig::survey -- --ignored --nocapture`
#[cfg(test)]
mod survey {
    use super::*;
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::Project;

    #[test]
    #[ignore]
    fn survey() {
        let list = std::env::var("SURVEY").unwrap_or_default();
        let (lo, hi, nf) = (std::env::var("LO").ok().and_then(|v| v.parse().ok()).unwrap_or(0.92), std::env::var("HI").ok().and_then(|v| v.parse().ok()).unwrap_or(1.08), std::env::var("NF").ok().and_then(|v| v.parse().ok()).unwrap_or(9usize));
        for case in list.split(';').filter(|s| !s.is_empty()) {
            let (id, sets) = case.split_once(':').unwrap_or((case, ""));
            let mut p = Project::from_template(id, None).unwrap();
            for set in sets.split(',').filter(|s| !s.is_empty()) {
                let (k, v) = set.split_once('=').unwrap();
                if k == "part" {
                    for e in &mut p.reconfig.elements {
                        e.device = Device::by_part(v).unwrap();
                    }
                } else {
                    p.params.iter_mut().find(|r| r.name == k).unwrap_or_else(|| panic!("{k}")).expr = v.to_string();
                }
            }
            let g = p.geometry().unwrap();
            let f0 = p.f0();
            let opts = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, theta_step: 5.0, phi_step: 5.0 };
            let problem = MomProblem::new(&g, p.setup.f_hi, opts).unwrap();
            let fs = crate::linspace(p.setup.f_lo, p.setup.f_hi, p.setup.mom_samples);
            let z: Vec<_> = fs.iter().map(|f| problem.solve(*f, false).unwrap().z).collect();
            let network = Network::from_z(fs, &z, g.ports.iter().map(|x| x.z0).collect()).interpolate(&crate::linspace(p.setup.f_lo, p.setup.f_hi, p.setup.points));
            let far_f = crate::linspace(f0 * lo, f0 * hi, nf);
            let mut far: Vec<Vec<FarField>> = vec![Vec::new(); g.ports.len()];
            for f in &far_f {
                for (port, ff) in problem.solve(*f, true).unwrap().far.into_iter().enumerate() {
                    far[port].push(ff);
                }
            }
            let basis = SimResult { label: id.into(), solver: "MoM".into(), network, far, far_f: far_f.clone(), lossless: true, ..Default::default() };
            let names: Vec<String> = g.ports.iter().map(|x| x.name.clone()).collect();
            let sv = Solved::new(&basis, &names, &p.reconfig).unwrap();
            println!("=== {case}: {} unknowns", problem.unknowns());
            for st in &p.reconfig.states {
                let s = p.reconfig.settings_of(st);
                let net = sv.network(&p.reconfig, &s).unwrap();
                let res: Vec<String> = net.resonances(0).iter().map(|r| format!("{:.3}:{:.1}", r.0 / f0, r.1)).collect();
                print!("{:22} res/f0 {:?}\n   ", st.name, res);
                for (k, f) in far_f.iter().enumerate() {
                    let ff = sv.far(&p.reconfig, &s, 0, k).unwrap();
                    let m = ff.metrics();
                    let db = net.interpolate(&[*f]).sij_db(0, 0)[0];
                    print!(" [{:.3} S{:.1} G{:.1} AR{:.1}{} θ{:.0}φ{:.0}]", f / f0, db, m.peak_realized_dbi, m.axial_ratio_db, if m.rhcp_at_peak { "R" } else { "L" }, m.peak_theta, m.peak_phi);
                }
                println!();
            }
        }
    }
}

/// How many solved patterns it takes for a state between them to be right. A survey:
/// `cargo test -p antenna_core --release far_density -- --ignored --nocapture`
#[cfg(test)]
mod far_density {
    use super::*;
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::Project;

    #[test]
    #[ignore]
    fn far_density() {
        for id in ["reconfig_dipole", "reconfig_pifa", "reconfig_parasitic", "reconfig_pol_patch"] {
            let p = Project::from_template(id, None).unwrap();
            let g = p.geometry().unwrap();
            let rc = &p.reconfig;
            let names: Vec<String> = g.ports.iter().map(|x| x.name.clone()).collect();
            let opts = MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, theta_step: 5.0, phi_step: 5.0 };
            let problem = MomProblem::new(&g, p.setup.f_hi, opts).unwrap();
            let fs = crate::linspace(p.setup.f_lo, p.setup.f_hi, p.setup.mom_samples);
            let z: Vec<_> = fs.iter().map(|f| problem.solve(*f, false).unwrap().z).collect();
            let network = Network::from_z(fs, &z, g.ports.iter().map(|x| x.z0).collect()).interpolate(&crate::linspace(p.setup.f_lo, p.setup.f_hi, p.setup.points));
            for n in [9usize, 13, 17, 25] {
                let far_f = crate::linspace(p.setup.f_lo, p.setup.f_hi, n);
                let mut far: Vec<Vec<FarField>> = vec![Vec::new(); g.ports.len()];
                for f in &far_f {
                    for (port, ff) in problem.solve(*f, true).unwrap().far.into_iter().enumerate() {
                        far[port].push(ff);
                    }
                }
                let basis = SimResult { network: network.clone(), far, far_f, lossless: true, ..Default::default() };
                let sv = Solved::new(&basis, &names, rc).unwrap();
                let (mut worst, mut worst_gain) = (0.0f64, 0.0f64);
                for st in &rc.states {
                    let s = rc.settings_of(st);
                    let f = sv.state_report(rc, st).unwrap().f_op;
                    let (ff, _) = sv.far_at(rc, &s, 0, f).unwrap();
                    let direct = SimResult { network: network.clone(), far: problem.solve(f, true).unwrap().far.into_iter().map(|x| vec![x]).collect(), far_f: vec![f], lossless: true, ..Default::default() };
                    let want = Solved::new(&direct, &names, rc).unwrap().far(rc, &s, 0, 0).unwrap();
                    let (mut num, mut den) = (0.0, 0.0);
                    for ti in 0..ff.theta.len() {
                        for pi in 0..ff.phi.len() {
                            num += (ff.e_theta[ti][pi] - want.e_theta[ti][pi]).norm_sqr() + (ff.e_phi[ti][pi] - want.e_phi[ti][pi]).norm_sqr();
                            den += want.e_theta[ti][pi].norm_sqr() + want.e_phi[ti][pi].norm_sqr();
                        }
                    }
                    worst = worst.max((num / den).sqrt());
                    worst_gain = worst_gain.max((ff.metrics().peak_realized_dbi - want.metrics().peak_realized_dbi).abs());
                }
                println!("{id:20} {n:3} patterns: worst field error {:6.2} %, worst gain error {worst_gain:.2} dB", worst * 100.0);
            }
        }
    }
}
