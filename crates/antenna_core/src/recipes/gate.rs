//! An RFID security gate, judged by what a gate is for: read every tag that walks
//! through, in any orientation, and none of the ones on display nearby.
//!
//! An aisle is well inside the near field of its own antennas: a panel 1.5 m tall
//! at 915 MHz has its far field beyond 2D²/λ ≈ 14 m. So a pattern and an array factor
//! can't answer this. The field is solved per port and sampled where tags are, then
//! the ports are combined with each excitation state: a state is what a phase network
//! or a switch sets at one moment, so several model a reader cycling through them.
//!
//! The ports' fields are kept, so changing phases, power or a tag costs no new solve.

use std::f64::consts::PI;

use anyhow::{Result, bail};
use proof::{Dist, Evidence, Factor, RobustOptions, RobustReport, Target, robust};
use serde::{Deserialize, Serialize};

use crate::farfield::FarField;
use crate::geometry::{Geometry, P3};
use crate::mom::{MomOptions, MomProblem};
use crate::optimize::{self, Method, Options};
use crate::project::{Drive, DriveState, Project, combine};
use crate::sparam::Network;
use crate::{C0, C64, ETA0, db20, material};

/// How tags can point: a dipole's axis, so opposite directions count once.
pub const ORIENTATIONS: [(&str, P3); 9] = [
    ("x", [1.0, 0.0, 0.0]),
    ("y", [0.0, 1.0, 0.0]),
    ("z", [0.0, 0.0, 1.0]),
    ("x+y", [0.70711, 0.70711, 0.0]),
    ("x−y", [0.70711, -0.70711, 0.0]),
    ("x+z", [0.70711, 0.0, 0.70711]),
    ("x−z", [0.70711, 0.0, -0.70711]),
    ("y+z", [0.0, 0.70711, 0.70711]),
    ("y−z", [0.0, 0.70711, -0.70711]),
];

/// Where the aisle is, in the model's axes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aisle {
    /// Axis across the aisle, away from the panel.
    pub across: usize,
    /// Axis tags walk along.
    pub walk: usize,
    /// The panel's front on the `across` axis, and which way the aisle lies from it.
    pub start: f64,
    pub sign: f64,
    /// Aisle width from `start`.
    pub width: f64,
    /// Length of the read zone along `walk`, centred on `center`.
    pub length: f64,
    pub center: f64,
    /// Tag heights on the remaining axis.
    pub low: f64,
    pub high: f64,
    /// Where "must not read" starts, measured from the zone's edges, and how deep it runs.
    pub stray_from: f64,
    pub stray_depth: f64,
}

impl Aisle {
    pub fn up(&self) -> usize {
        3 - self.across - self.walk
    }

    fn across_range(&self, from: f64, to: f64) -> (f64, f64) {
        let (a, b) = (self.start + self.sign * from, self.start + self.sign * to);
        (a.min(b), a.max(b))
    }

    fn box_of(&self, across: (f64, f64), walk: (f64, f64)) -> (P3, P3) {
        let (mut lo, mut hi) = ([0.0; 3], [0.0; 3]);
        lo[self.across] = across.0;
        hi[self.across] = across.1;
        lo[self.walk] = walk.0;
        hi[self.walk] = walk.1;
        lo[self.up()] = self.low;
        hi[self.up()] = self.high;
        (lo, hi)
    }

    /// The box tags walk through.
    pub fn zone(&self) -> (P3, P3) {
        self.box_of(self.across_range(0.0, self.width), (self.center - self.length / 2.0, self.center + self.length / 2.0))
    }

    /// Where tags are on display and shouldn't read: ahead, behind, and past the panel.
    pub fn stray_boxes(&self) -> Vec<(P3, P3)> {
        let across = self.across_range(0.0, self.width);
        let (front, back) = (self.center + self.length / 2.0 + self.stray_from, self.center - self.length / 2.0 - self.stray_from);
        vec![
            self.box_of(across, (front, front + self.stray_depth)),
            self.box_of(across, (back - self.stray_depth, back)),
            self.box_of(self.across_range(-self.stray_from - self.stray_depth, -self.stray_from), (self.center - self.length / 2.0, self.center + self.length / 2.0)),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateSpec {
    pub f: f64,
    pub aisle: Aisle,
    /// Sample spacing through the zone, metres.
    pub spacing: f64,
    /// No samples closer to metal than this: the field isn't reliable there.
    pub clear: f64,
    /// Reader power at the reader's connector, dBm.
    pub tx_dbm: f64,
    /// Cable, connectors and any excess splitter loss, one way, dB.
    pub feed_loss_db: f64,
    /// The chip's read threshold, dBm.
    pub tag_sensitivity_dbm: f64,
    pub tag_gain_dbi: f64,
    /// Loss from what the tag is on, nominal and spread, dB.
    pub product_loss_db: (f64, f64),
    pub reader_sensitivity_dbm: f64,
    /// Power returned by the tag's modulation relative to what reached it, dB.
    pub backscatter_db: f64,
    /// Walking speed, m/s.
    pub walk_speed: f64,
    /// Time the reader spends in one state, seconds.
    pub dwell_s: f64,
    pub min_read: f64,
    pub max_stray: f64,
    /// The limit on radiated power for the region, dBm EIRP.
    pub eirp_limit_dbm: f64,
}

impl GateSpec {
    /// A 1 m aisle in front of the model's panel. Which axis is which comes from the
    /// model's shape: across its thinnest side, up along z when z isn't the thin one.
    pub fn for_model(geom: &Geometry, f: f64) -> Self {
        let (lo, hi) = geom.bounds();
        let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        let across = (0..3).min_by(|a, b| size[*a].total_cmp(&size[*b])).unwrap_or(0);
        let up = if across == 2 { 1 } else { 2 };
        let walk = 3 - across - up;
        let center = (lo[walk] + hi[walk]) / 2.0;
        let (low, high) = if up == 2 && lo[2] >= -1e-9 && hi[2] > 0.5 { (0.2, 1.7) } else { (lo[up], hi[up]) };
        let aisle = Aisle { across, walk, start: hi[across], sign: 1.0, width: 1.0, length: 1.2, center, low, high, stray_from: 1.0, stray_depth: 1.5 };
        Self::with_aisle(aisle, f)
    }

    /// The aisle a project describes: a gate template knows its own, between its panels.
    pub fn for_project(p: &Project, geom: &Geometry) -> Self {
        let f = p.f0();
        let mut s = Self::for_model(geom, f);
        if p.template == "rfid_gate"
            && let Ok(v) = p.vars()
        {
            let two = v.get("sides").is_some_and(|n| *n >= 1.5);
            s.aisle.across = 0;
            s.aisle.walk = 1;
            s.aisle.start = 0.0;
            s.aisle.sign = 1.0;
            s.aisle.center = 0.0;
            s.aisle.width = if two { v.get("A").copied().unwrap_or(1.0) } else { 1.0 };
            s.aisle.low = 0.2;
            s.aisle.high = 1.7;
        }
        s
    }

    fn with_aisle(aisle: Aisle, f: f64) -> Self {
        Self {
            f,
            aisle,
            spacing: 0.1,
            clear: 0.05,
            tx_dbm: 30.0,
            feed_loss_db: 1.5,
            tag_sensitivity_dbm: -20.0,
            tag_gain_dbi: 1.0,
            product_loss_db: (3.0, 2.0),
            reader_sensitivity_dbm: -80.0,
            backscatter_db: -6.0,
            walk_speed: 1.5,
            dwell_s: 0.05,
            min_read: 0.99,
            max_stray: 0.01,
            eirp_limit_dbm: 36.0,
        }
    }

    /// The part that decides where the field is sampled: change it and the field must be solved again.
    pub fn same_samples(&self, o: &GateSpec) -> bool {
        self.f == o.f && self.aisle == o.aisle && self.spacing == o.spacing && self.clear == o.clear
    }

    /// Path gain a read needs at this power, loss and tag, dB. Returns the gain and
    /// whether the reader's receive side, not the tag's chip, is what limits.
    pub fn needed_gain(&self, tx_dbm: f64, loss_db: f64, tag_sensitivity_dbm: f64) -> (f64, bool) {
        let forward = tag_sensitivity_dbm - tx_dbm + self.feed_loss_db + loss_db;
        // By reciprocity, the way back through the same feed has the same path gain.
        let reverse = (self.reader_sensitivity_dbm - tx_dbm - self.backscatter_db) / 2.0 + self.feed_loss_db + loss_db;
        (forward.max(reverse), reverse > forward)
    }
}

/// Where the field is sampled.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Samples {
    pub axes: [Vec<f64>; 3],
    /// Index into `points` for each grid node, `x + nx·(y + ny·z)`; `None` is too close to metal.
    pub nodes: Vec<Option<u32>>,
    pub points: Vec<P3>,
    /// The points in "must not read" boxes come after the zone's.
    pub stray_from: usize,
    pub excluded: usize,
}

impl Samples {
    fn node(&self, ix: [usize; 3]) -> Option<usize> {
        self.nodes[ix[0] + self.axes[0].len() * (ix[1] + self.axes[1].len() * ix[2])].map(|v| v as usize)
    }
}

fn grid(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let n = ((hi - lo) / step).ceil().max(0.0) as usize;
    crate::linspace(lo, hi, n + 1)
}

/// The sample points for a model. `keep_out` adds boxes the field can't be known in.
pub fn samples(geom: &Geometry, spec: &GateSpec, keep_out: &[(P3, P3)]) -> Result<Samples> {
    let a = &spec.aisle;
    if a.across == a.walk || a.across > 2 || a.walk > 2 {
        bail!("the aisle's across and walking axes must be two different axes");
    }
    if !(spec.spacing > 0.0) || !(a.width > 0.0) || !(a.length > 0.0) || !(a.high >= a.low) {
        bail!("the read zone needs a positive size and spacing");
    }
    let lib = material::library();
    let mut solid: Vec<(P3, P3)> = geom
        .solids
        .iter()
        .filter(|s| lib.iter().find(|m| m.name == s.material).is_none_or(|m| m.is_conductor() || m.eps_r() > 1.0))
        .map(|s| {
            let (lo, hi) = s.shape.bounds();
            (std::array::from_fn(|k| lo[k] - spec.clear), std::array::from_fn(|k| hi[k] + spec.clear))
        })
        .collect();
    solid.extend_from_slice(keep_out);
    let blocked = |p: P3| (geom.ground && p[2] < spec.clear) || solid.iter().any(|(lo, hi)| (0..3).all(|k| p[k] >= lo[k] && p[k] <= hi[k]));

    let (lo, hi) = a.zone();
    let axes: [Vec<f64>; 3] = std::array::from_fn(|k| grid(lo[k], hi[k], spec.spacing));
    let total = axes[0].len() * axes[1].len() * axes[2].len();
    if total > 60_000 {
        bail!("{total} sample points is too many: widen the spacing");
    }
    let mut nodes = Vec::with_capacity(total);
    let mut points = Vec::with_capacity(total);
    let mut excluded = 0;
    for &z in &axes[2] {
        for &y in &axes[1] {
            for &x in &axes[0] {
                let p = [x, y, z];
                if blocked(p) {
                    nodes.push(None);
                    excluded += 1;
                } else {
                    nodes.push(Some(points.len() as u32));
                    points.push(p);
                }
            }
        }
    }
    let stray_from = points.len();
    // Coarser: this only has to say how much reads out there, not map it.
    for (blo, bhi) in a.stray_boxes() {
        let g: [Vec<f64>; 3] = std::array::from_fn(|k| grid(blo[k], bhi[k], spec.spacing * 2.0));
        for &z in &g[2] {
            for &y in &g[1] {
                for &x in &g[0] {
                    if !blocked([x, y, z]) {
                        points.push([x, y, z]);
                    }
                }
            }
        }
    }
    if points.len() == stray_from {
        bail!("every sample in the read zone is inside or against the model: move the zone");
    }
    Ok(Samples { axes, nodes, points, stray_from, excluded })
}

/// The solved model at one frequency: enough to judge any excitation without a new solve.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GateField {
    pub f: f64,
    pub solver: String,
    /// MoM unknowns, or FDTD cells.
    pub size: u64,
    pub seconds: f64,
    pub samples: Samples,
    /// `e[port][point]`: the field in V/m, peak, with 1 W incident on that port and
    /// the others terminated.
    pub e: Vec<Vec<[C64; 3]>>,
    /// One frequency: the port coupling.
    pub network: Network,
    /// Each port's far field, for what the gate radiates in total.
    pub far: Vec<FarField>,
    pub ports: Vec<String>,
}

/// Solves a metal model with the method of moments.
pub fn solve_mom(geom: &Geometry, spec: &GateSpec, opts: MomOptions) -> Result<GateField> {
    let t0 = std::time::Instant::now();
    let samples = samples(geom, spec, &[])?;
    let problem = MomProblem::new(geom, spec.f, opts)?;
    let mut pt = problem.solve(spec.f, true)?;
    let e = problem.near_fields(spec.f, &samples.points)?;
    let z0: Vec<f64> = geom.ports.iter().map(|p| p.z0).collect();
    let network = Network::from_z(vec![spec.f], std::slice::from_ref(&pt.z), z0);
    Ok(GateField {
        f: spec.f,
        solver: "Method of moments".into(),
        size: problem.unknowns() as u64,
        seconds: t0.elapsed().as_secs_f64(),
        samples,
        e,
        network,
        far: std::mem::take(&mut pt.far),
        ports: geom.ports.iter().map(|p| p.name.clone()).collect(),
    })
}

/// Ready-made ways of driving `n` ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Everything in phase, one state.
    InPhase,
    /// Equal phase steps from port to port, one state. 360/n turns once along the panel.
    Progressive(i32),
    /// Neighbours in opposite phase, one state.
    Alternating,
    /// One element at a time, as a switch would.
    OneAtATime,
    /// A phase ramp in several steps, sweeping the beam.
    Sweep,
}

impl Preset {
    pub fn label(self) -> String {
        match self {
            Preset::InPhase => "All in phase".into(),
            Preset::Progressive(d) => format!("{d}° per element"),
            Preset::Alternating => "Alternating 0°, 180°".into(),
            Preset::OneAtATime => "One element at a time".into(),
            Preset::Sweep => "Beam sweep, five steps".into(),
        }
    }
}

fn ramp(name: String, n: usize, step: f64) -> DriveState {
    DriveState { name, drive: (0..n).map(|k| Drive { amp: 1.0, phase: (k as f64 * step + 180.0).rem_euclid(360.0) - 180.0 }).collect() }
}

pub fn preset_states(p: Preset, n: usize) -> Vec<DriveState> {
    match p {
        Preset::InPhase => vec![ramp("In phase".into(), n, 0.0)],
        Preset::Progressive(d) => vec![ramp(format!("{d}° per element"), n, d as f64)],
        Preset::Alternating => vec![ramp("Alternating".into(), n, 180.0)],
        Preset::OneAtATime => (0..n).map(|k| DriveState { name: format!("Element {} alone", k + 1), drive: (0..n).map(|j| Drive { amp: if j == k { 1.0 } else { 0.0 }, phase: 0.0 }).collect() }).collect(),
        Preset::Sweep => [-90.0, -45.0, 0.0, 45.0, 90.0].iter().map(|d: &f64| ramp(format!("{d:+.0}° per element"), n, *d)).collect(),
    }
}

/// What a gate starts with: two ways of driving it that the reader alternates between.
/// More isn't simply better: each extra state lengthens the trip round them and adds
/// its own stray reads, as `gate_states_audit` below shows. In free space a single good
/// state can match this; two keep some diversity for what the model leaves out.
pub fn default_states(n: usize) -> Vec<DriveState> {
    vec![ramp("In phase".into(), n, 0.0), ramp("90° per element".into(), n, 90.0)]
}

/// The reader power that keeps every state within the EIRP limit, at most `tx_dbm`.
fn legal_power(far: &[&FarField], weights: &[Vec<C64>], spec: &GateSpec) -> f64 {
    let loudest = weights.iter().filter_map(|w| combine(far, w)).map(|ff| ff.metrics().peak_realized_dbi + spec.tx_dbm - spec.feed_loss_db).fold(f64::NEG_INFINITY, f64::max);
    if loudest > spec.eirp_limit_dbm { spec.tx_dbm - (loudest - spec.eirp_limit_dbm) } else { spec.tx_dbm }
}

/// A grid of margins for a chart. NaN where nothing was sampled.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeatMap {
    /// Axis index across and up the chart.
    pub axes: (usize, usize),
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    /// `u + nu·v`, dB above what a read needs.
    pub db: Vec<f64>,
}

/// One excitation state on its own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StateScore {
    pub name: String,
    /// Share of walks this state reads alone, if it stayed on.
    pub read: f64,
    pub stray: f64,
    /// Total reflected over total incident at the feed, dB.
    pub tarc_db: f64,
    pub eirp_dbm: Option<f64>,
    /// Largest field in the zone, V/m rms.
    pub peak_field_vm: f64,
}

/// How a gate does with a set of states, at one power.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GateReport {
    pub f: f64,
    /// Share of walks read while the reader cycles through the states.
    pub read: f64,
    /// Share of display positions read.
    pub stray: f64,
    /// (orientation, share of walks read), worst first.
    pub by_orientation: Vec<(String, f64)>,
    pub states: Vec<StateScore>,
    /// Path gain a read needs, dB.
    pub needed_db: f64,
    /// The reader's receive side limits, not the tag's chip.
    pub reverse_limited: bool,
    /// Samples a tag must stay readable for: one trip round the states.
    pub window: usize,
    pub lanes: usize,
    /// Lowest reader power that meets the read goal, and its stray share.
    pub power_for_goal: Option<(f64, f64)>,
    /// When a state breaks the EIRP limit: the reader power that just keeps to it,
    /// with the read and stray shares there.
    pub legal: Option<(f64, f64, f64)>,
    /// Walks read and not read: per lane, per orientation.
    pub lane_map: HeatMap,
    /// The worst orientation through the zone at mid height.
    pub plane_map: HeatMap,
    /// Per walk and orientation, best sustained path gain, dB.
    pub walk_table: Vec<f64>,
    /// Per display position and orientation, best path gain, dB.
    pub stray_table: Vec<f64>,
    pub warnings: Vec<String>,
}

fn share_at_or_above(table: &[f64], needed: f64) -> f64 {
    if table.is_empty() {
        return 0.0;
    }
    table.iter().filter(|v| **v >= needed).count() as f64 / table.len() as f64
}

/// Path gain from the feed to a matched tag at each point and orientation, dB,
/// for one state. The state's weights carry 1 W in total.
fn state_gains(field: &GateField, w: &[C64], tag_gain_dbi: f64) -> Vec<f32> {
    let lambda = C0 / field.f;
    let aperture = 10f64.powf(tag_gain_dbi / 10.0) * lambda * lambda / (4.0 * PI);
    let n = field.samples.points.len();
    let mut out = Vec::with_capacity(n * ORIENTATIONS.len());
    for p in 0..n {
        let mut e = [C64::new(0.0, 0.0); 3];
        for (port, wk) in field.e.iter().zip(w) {
            for k in 0..3 {
                e[k] += port[p][k] * *wk;
            }
        }
        for (_, o) in ORIENTATIONS {
            let along = e[0] * o[0] + e[1] * o[1] + e[2] * o[2];
            let watts = along.norm_sqr() / (2.0 * ETA0) * aperture;
            out.push((10.0 * watts.max(1e-25).log10()) as f32);
        }
    }
    out
}

/// The most that a tag can be held at for `window` samples in a row, along one lane.
fn best_window(series: &[f64], window: usize) -> f64 {
    let w = window.clamp(1, series.len().max(1));
    series.windows(w).map(|s| s.iter().copied().fold(f64::INFINITY, f64::min)).fold(f64::NEG_INFINITY, f64::max)
}

/// Each state's gains, weights and name; the states that drive nothing are dropped with a warning.
struct Driven {
    names: Vec<String>,
    weights: Vec<Vec<C64>>,
    gains: Vec<Vec<f32>>,
}

fn drive(field: &GateField, spec: &GateSpec, states: &[DriveState], warnings: &mut Vec<String>) -> Driven {
    let ports = field.e.len();
    let mut d = Driven { names: Vec::new(), weights: Vec::new(), gains: Vec::new() };
    for s in states {
        if s.drive.len() != ports {
            warnings.push(format!("State '{}' sets {} ports and the model has {ports}.", s.name, s.drive.len()));
        }
        match s.weights(ports) {
            Some(w) => {
                d.gains.push(state_gains(field, &w, spec.tag_gain_dbi));
                d.weights.push(w);
                d.names.push(s.name.clone());
            }
            None => warnings.push(format!("State '{}' drives nothing, so it was left out.", s.name)),
        }
    }
    d
}

/// For each lane and orientation, the best sustained gain over these states.
fn walk_table(field: &GateField, spec: &GateSpec, gains: &[&[f32]], window: usize) -> Vec<f64> {
    let s = &field.samples;
    let a = &spec.aisle;
    let (walk, up, across) = (a.walk, a.up(), a.across);
    let nw = s.axes[walk].len();
    let no = ORIENTATIONS.len();
    let mut out = Vec::with_capacity(s.axes[up].len() * s.axes[across].len() * no);
    let mut series = vec![0.0; nw];
    for iv in 0..s.axes[up].len() {
        for iu in 0..s.axes[across].len() {
            for o in 0..no {
                let mut best = f64::NEG_INFINITY;
                for g in gains {
                    for (iw, value) in series.iter_mut().enumerate() {
                        let mut ix = [0; 3];
                        ix[walk] = iw;
                        ix[up] = iv;
                        ix[across] = iu;
                        // Too close to metal to know: nothing is claimed for that sample.
                        *value = s.node(ix).map_or(f64::NEG_INFINITY, |p| g[p * no + o] as f64);
                    }
                    best = best.max(best_window(&series, window));
                }
                out.push(best);
            }
        }
    }
    out
}

/// One trip through the states, in samples along the walk.
fn window_for(field: &GateField, spec: &GateSpec, states: usize) -> usize {
    let axis = &field.samples.axes[spec.aisle.walk];
    let step = if axis.len() > 1 { (axis[axis.len() - 1] - axis[0]) / (axis.len() - 1) as f64 } else { 1.0 };
    let cycle = states.max(1) as f64 * spec.dwell_s;
    ((cycle * spec.walk_speed / step).ceil() as usize).max(1)
}

/// Judges one set of states.
pub fn evaluate(field: &GateField, spec: &GateSpec, states: &[DriveState]) -> GateReport {
    let mut warnings = Vec::new();
    let d = drive(field, spec, states, &mut warnings);
    let (needed, reverse) = spec.needed_gain(spec.tx_dbm, spec.product_loss_db.0, spec.tag_sensitivity_dbm);
    let mut report = GateReport { f: field.f, needed_db: needed, reverse_limited: reverse, warnings, ..Default::default() };
    if d.gains.is_empty() || field.e.is_empty() {
        report.warnings.push("No state drives any port.".into());
        return report;
    }
    let s = &field.samples;
    let no = ORIENTATIONS.len();
    let refs: Vec<&[f32]> = d.gains.iter().map(Vec::as_slice).collect();
    let window = window_for(field, spec, refs.len());
    let walks = walk_table(field, spec, &refs, window);
    let stray_points = s.points.len() - s.stray_from;
    let mut stray = vec![f64::NEG_INFINITY; stray_points * no];
    for g in &refs {
        for (i, v) in stray.iter_mut().enumerate() {
            *v = v.max(g[(s.stray_from * no) + i] as f64);
        }
    }
    report.read = share_at_or_above(&walks, needed);
    report.stray = share_at_or_above(&stray, needed);
    report.window = window;
    report.lanes = walks.len() / no;
    let mut by: Vec<(String, f64)> = (0..no)
        .map(|o| {
            let column: Vec<f64> = walks.iter().skip(o).step_by(no).copied().collect();
            (ORIENTATIONS[o].0.to_string(), share_at_or_above(&column, needed))
        })
        .collect();
    by.sort_by(|a, b| a.1.total_cmp(&b.1));
    report.by_orientation = by;
    report.lane_map = lane_map(field, spec, &walks, needed);
    report.plane_map = plane_map(field, spec, &refs, needed);

    let far: Vec<&FarField> = field.far.iter().collect();
    for (k, w) in d.weights.iter().enumerate() {
        let alone = walk_table(field, spec, &[d.gains[k].as_slice()], 1);
        let mut peak = 0.0f64;
        for p in 0..s.stray_from {
            let mut e2 = 0.0;
            for c in 0..3 {
                let v: C64 = field.e.iter().zip(w).map(|(port, wk)| port[p][c] * *wk).sum();
                e2 += v.norm_sqr();
            }
            peak = peak.max(e2);
        }
        let power = 10f64.powf((spec.tx_dbm - spec.feed_loss_db - 30.0) / 10.0);
        let tarc = field.network.tarc(w).first().copied().unwrap_or(0.0);
        let eirp = combine(&far, w).map(|ff| ff.metrics().peak_realized_dbi + spec.tx_dbm - spec.feed_loss_db);
        let stray_alone = d.gains[k][s.stray_from * no..].iter().filter(|v| **v as f64 >= needed).count() as f64 / (stray_points * no).max(1) as f64;
        report.states.push(StateScore { name: d.names[k].clone(), read: share_at_or_above(&alone, needed), stray: stray_alone, tarc_db: db20(tarc), eirp_dbm: eirp, peak_field_vm: (peak * power / 2.0).sqrt() });
    }
    let loudest = report.states.iter().filter_map(|s| s.eirp_dbm).fold(f64::NEG_INFINITY, f64::max);
    if loudest > spec.eirp_limit_dbm {
        let tx = spec.tx_dbm - (loudest - spec.eirp_limit_dbm);
        let (n, _) = spec.needed_gain(tx, spec.product_loss_db.0, spec.tag_sensitivity_dbm);
        report.legal = Some((tx, share_at_or_above(&walks, n), share_at_or_above(&stray, n)));
    }
    // The lowest power that meets the goal, by the same tables.
    let mut tx = 0.0;
    while tx <= 40.0 {
        let (n, _) = spec.needed_gain(tx, spec.product_loss_db.0, spec.tag_sensitivity_dbm);
        if share_at_or_above(&walks, n) >= spec.min_read {
            report.power_for_goal = Some((tx, share_at_or_above(&stray, n)));
            break;
        }
        tx += 0.5;
    }
    report.walk_table = walks;
    report.stray_table = stray;
    report
}

fn lane_map(field: &GateField, spec: &GateSpec, walks: &[f64], needed: f64) -> HeatMap {
    let s = &field.samples;
    let (up, across) = (spec.aisle.up(), spec.aisle.across);
    let (nu, nv) = (s.axes[across].len(), s.axes[up].len());
    let no = ORIENTATIONS.len();
    let mut db = vec![f64::NAN; nu * nv];
    for iv in 0..nv {
        for iu in 0..nu {
            let at = (iu + nu * iv) * no;
            let worst = walks[at..at + no].iter().copied().fold(f64::INFINITY, f64::min);
            db[iu + nu * iv] = if worst.is_finite() { worst - needed } else { f64::NAN };
        }
    }
    HeatMap { axes: (across, up), u: s.axes[across].clone(), v: s.axes[up].clone(), db }
}

fn plane_map(field: &GateField, spec: &GateSpec, gains: &[&[f32]], needed: f64) -> HeatMap {
    let s = &field.samples;
    let (up, walk, across) = (spec.aisle.up(), spec.aisle.walk, spec.aisle.across);
    let iv = s.axes[up].len() / 2;
    let (nu, nv) = (s.axes[across].len(), s.axes[walk].len());
    let no = ORIENTATIONS.len();
    let mut db = vec![f64::NAN; nu * nv];
    for jw in 0..nv {
        for iu in 0..nu {
            let mut ix = [0; 3];
            ix[up] = iv;
            ix[walk] = jw;
            ix[across] = iu;
            let Some(p) = s.node(ix) else { continue };
            let worst = (0..no).map(|o| gains.iter().map(|g| g[p * no + o] as f64).fold(f64::NEG_INFINITY, f64::max)).fold(f64::INFINITY, f64::min);
            db[iu + nu * jw] = worst - needed;
        }
    }
    HeatMap { axes: (across, walk), u: s.axes[across].clone(), v: s.axes[walk].clone(), db }
}

/// Searches the phases of `n_states` states for the most walks read and the fewest
/// strays, at the power each candidate may legally use: a phase that piles the beam
/// up has to turn the reader down. No new solve: only the combination changes.
pub fn search_phases(field: &GateField, spec: &GateSpec, n_states: usize, max_evals: usize, seed: u64) -> Vec<DriveState> {
    let ports = field.e.len();
    if ports == 0 || n_states == 0 {
        return Vec::new();
    }
    let dims = n_states * ports.saturating_sub(1);
    let build = |x: &[f64]| -> Vec<DriveState> {
        (0..n_states)
            .map(|s| {
                let mut drive = Vec::with_capacity(ports);
                drive.push(Drive { amp: 1.0, phase: 0.0 });
                for k in 0..ports - 1 {
                    drive.push(Drive { amp: 1.0, phase: x[s * (ports - 1) + k] });
                }
                DriveState { name: format!("Searched {}", s + 1), drive }
            })
            .collect()
    };
    let no = ORIENTATIONS.len();
    let window = window_for(field, spec, n_states);
    let far: Vec<&FarField> = field.far.iter().collect();
    let cost = |x: &[f64]| -> f64 {
        let states = build(x);
        let weights: Vec<Vec<C64>> = states.iter().filter_map(|s| s.weights(ports)).collect();
        let (needed, _) = spec.needed_gain(legal_power(&far, &weights, spec), spec.product_loss_db.0, spec.tag_sensitivity_dbm);
        let gains: Vec<Vec<f32>> = weights.iter().map(|w| state_gains(field, w, spec.tag_gain_dbi)).collect();
        let refs: Vec<&[f32]> = gains.iter().map(Vec::as_slice).collect();
        let walks = walk_table(field, spec, &refs, window);
        let from = field.samples.stray_from * no;
        let stray = (0..field.samples.points.len() - field.samples.stray_from).flat_map(|p| (0..no).map(move |o| (p, o))).filter(|(p, o)| refs.iter().any(|g| g[from + p * no + o] as f64 >= needed)).count() as f64 / ((field.samples.points.len() - field.samples.stray_from) * no).max(1) as f64;
        // A share alone is flat between reads; the margin gives the search a slope.
        let soft = walks.iter().map(|v| ((v - needed) / 6.0).clamp(-1.0, 1.0)).sum::<f64>() / walks.len().max(1) as f64;
        -share_at_or_above(&walks, needed) - 0.1 * soft + (stray - spec.max_stray).max(0.0)
    };
    if dims == 0 {
        return build(&[]);
    }
    let start = vec![0.0; dims];
    let bounds = vec![(-180.0, 180.0); dims];
    let opts = Options { method: Method::DifferentialEvolution, max_evals, seed, population: Some((4 * dims).clamp(12, 40)), tol: 1e-4, parallel: 1 };
    let outcome = optimize::minimize(cost, &start, &bounds, &opts, |_| true);
    build(&outcome.x)
}

/// Read rate and stray rate across what isn't known about tags and the reader.
pub fn predict(report: &GateReport, spec: &GateSpec, samples: usize) -> (RobustReport, RobustReport) {
    let factors = vec![
        Factor::new("Tag sensitivity", "dBm", Dist::Normal { mean: spec.tag_sensitivity_dbm, sigma: 1.5 }, "Spread between tag batches, assumed"),
        Factor::new("Tag gain on product", "dB", Dist::Normal { mean: 0.0, sigma: 1.5 }, "Assumed"),
        Factor::new("Product loss", "dB", Dist::Normal { mean: spec.product_loss_db.0, sigma: spec.product_loss_db.1 }, "Given for this gate"),
        Factor::new("Reader power", "dBm", Dist::Normal { mean: spec.tx_dbm, sigma: 0.5 }, "Reader and cable tolerance, assumed"),
    ];
    let needed = |v: &[f64]| spec.needed_gain(v[3], v[2] - v[1], v[0]).0;
    let opts = RobustOptions { samples, seed: 13 };
    let read = robust("Tags read walking through", "share", &factors, Some(Target::at_least(spec.min_read)), Evidence::Estimate, opts, |v| share_at_or_above(&report.walk_table, needed(v)));
    let stray = robust("Tags on display read", "share", &factors, Some(Target::at_most(spec.max_stray)), Evidence::Estimate, opts, |v| share_at_or_above(&report.stray_table, needed(v)));
    (read, stray)
}

/// What a report on this recipe assumes, for the claim ledger.
pub fn assumptions(field: &GateField, spec: &GateSpec) -> Vec<String> {
    vec![
        format!("Solved by {}, without people, products, housings or building, except what the model holds", field.solver.to_lowercase()),
        "Tags are dipoles that don't disturb the field; they read when both links close, without tag collisions or interference from other readers".into(),
        format!("Walking at {:.1} m/s, a tag counts as read if some state covers it for one full trip round the states ({:.0} ms per state)", spec.walk_speed, spec.dwell_s * 1e3),
        "Products enter as a loss range; a tag against liquid, foil or a body can do far worse".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{PortDef, Shape, Solid};

    fn sheet(name: &str, min: P3, max: P3) -> Solid {
        Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min, max } }
    }

    /// `n` vertical strip dipoles in a row along y, in free space.
    fn row(n: usize, pitch: f64, f: f64) -> Geometry {
        let lambda = C0 / f;
        let (l, w, g) = (0.46 * lambda, lambda / 40.0, lambda / 50.0);
        let mut solids = Vec::new();
        let mut ports = Vec::new();
        for k in 0..n {
            let y = (k as f64 - (n as f64 - 1.0) / 2.0) * pitch;
            solids.push(sheet(&format!("up {k}"), [0.0, y - w / 2.0, g / 2.0], [0.0, y + w / 2.0, l / 2.0]));
            solids.push(sheet(&format!("down {k}"), [0.0, y - w / 2.0, -l / 2.0], [0.0, y + w / 2.0, -g / 2.0]));
            ports.push(PortDef { name: format!("P{}", k + 1), a: [0.0, y, -g / 2.0], b: [0.0, y, g / 2.0], width: w, z0: 73.0 });
        }
        Geometry { solids, ports, ..Default::default() }
    }

    fn spec_for(geom: &Geometry, f: f64) -> GateSpec {
        let mut s = GateSpec::for_model(geom, f);
        s.aisle = Aisle { across: 0, walk: 1, start: 0.0, sign: 1.0, width: 1.0, length: 0.6, center: 0.0, low: -0.3, high: 0.3, stray_from: 1.0, stray_depth: 0.6 };
        s.spacing = 0.15;
        s
    }

    fn opts() -> MomOptions {
        MomOptions { edges_per_lambda: 14.0, theta_step: 5.0, phi_step: 5.0, ..Default::default() }
    }

    /// The field's size in a direction, far away, against the pattern the solver reports.
    #[test]
    fn near_field_far_away_agrees_with_the_combined_pattern() {
        let f = 915e6;
        let g = row(3, 0.5 * C0 / f, f);
        let p = MomProblem::new(&g, f, opts()).unwrap();
        let pt = p.solve(f, true).unwrap();
        let w: Vec<C64> = [(1.0f64, 0.0f64), (1.0, 40.0), (1.0, 80.0)].iter().map(|(a, d)| C64::from_polar(*a / 3f64.sqrt(), d.to_radians())).collect();
        let list: Vec<&FarField> = pt.far.iter().collect();
        let far = combine(&list, &w).unwrap();
        let r = 2000.0 * C0 / f;
        let (ti, pi) = (far.theta.iter().position(|t| (*t - 90.0).abs() < 1e-6).unwrap(), far.phi.iter().position(|t| (*t - 60.0).abs() < 1e-6).unwrap());
        let (st, ct) = 90f64.to_radians().sin_cos();
        let (sp, cp) = 60f64.to_radians().sin_cos();
        let e = p.near_fields(f, &[[r * st * cp, r * st * sp, r * ct]]).unwrap();
        let mut sum = [C64::new(0.0, 0.0); 3];
        for (port, wk) in e.iter().zip(&w) {
            for k in 0..3 {
                sum[k] += port[0][k] * *wk;
            }
        }
        let near = (sum[0].norm_sqr() + sum[1].norm_sqr() + sum[2].norm_sqr()).sqrt() * r;
        let pattern = (far.e_theta[ti][pi].norm_sqr() + far.e_phi[ti][pi].norm_sqr()).sqrt();
        assert!((near - pattern).abs() / pattern < 0.01, "near {near} vs pattern {pattern}");
    }

    /// With one element and a far tag, the budget has to be Friis.
    #[test]
    fn far_away_the_read_budget_is_the_friis_equation() {
        let f = 915e6;
        let g = row(1, 0.0, f);
        let mut spec = spec_for(&g, f);
        // A thin line of samples 20 wavelengths out, along x from the dipole.
        let lambda = C0 / f;
        spec.aisle = Aisle { across: 0, walk: 1, start: 20.0 * lambda, sign: 1.0, width: 0.1, length: 0.0001, center: 0.0, low: 0.0, high: 0.0, stray_from: 100.0, stray_depth: 0.1 };
        spec.spacing = 0.1;
        let field = solve_mom(&g, &spec, opts()).unwrap();
        let w = [C64::new(1.0, 0.0)];
        let gains = state_gains(&field, &w, spec.tag_gain_dbi);
        let d = 20.0 * lambda;
        let far = &field.far[0];
        let (ti, pi) = (far.theta.iter().position(|t| (*t - 90.0).abs() < 1e-6).unwrap(), 0);
        // Realised gain in that direction, from the same far field.
        let rg = far.grid_db(crate::farfield::Quantity::RealizedGain, crate::farfield::Pol::Total)[ti][pi];
        let friis = rg + spec.tag_gain_dbi + 20.0 * (lambda / (4.0 * PI * d)).log10();
        let z_axis = gains[2] as f64;
        println!("Friis {friis:.2} dB, solved {z_axis:.2} dB");
        assert!((z_axis - friis).abs() < 0.3, "{z_axis} vs {friis}");
        // A tag across the field reads nothing.
        assert!(gains[0] < gains[2] - 30.0, "{} {}", gains[0], gains[2]);
    }

    #[test]
    fn a_phase_ramp_steers_as_theory_says() {
        let f = 915e6;
        let lambda = C0 / f;
        let pitch = 0.5 * lambda;
        let g = row(5, pitch, f);
        let p = MomProblem::new(&g, f, opts()).unwrap();
        let pt = p.solve(f, true).unwrap();
        let list: Vec<&FarField> = pt.far.iter().collect();
        for step in [30.0f64, 60.0] {
            let w: Vec<C64> = (0..5).map(|k| C64::from_polar(1.0, (k as f64 * step).to_radians())).collect();
            let far = combine(&list, &w).unwrap();
            let total = far.grid_db(crate::farfield::Quantity::Directivity, crate::farfield::Pol::Total);
            let ti = far.theta.iter().position(|t| (*t - 90.0).abs() < 1e-6).unwrap();
            // Broadside to the row on the +x side: φ from −90° to 90°.
            let (best, _) = far.phi.iter().enumerate().filter(|(_, ph)| **ph <= 90.0 || **ph >= 270.0).max_by(|a, b| total[ti][a.0].total_cmp(&total[ti][b.0])).unwrap();
            let got = far.phi[best];
            let got = if got > 180.0 { got - 360.0 } else { got };
            // With exp(+jωt), a phase that grows along +y leans the beam towards −y.
            let want = -(step.to_radians() / (2.0 * PI * pitch / lambda)).asin().to_degrees();
            println!("step {step}°: beam {got:.0}°, theory {want:.1}°");
            assert!((got - want).abs() <= 6.0, "step {step}: {got} vs {want}");
        }
    }

    #[test]
    fn coupling_is_in_the_numbers() {
        // Two close dipoles couple strongly. Each port's field already holds the other
        // element's re-radiation, so the combined accepted power must follow TARC.
        let f = 915e6;
        let g = row(2, 0.2 * C0 / f, f);
        let spec = spec_for(&g, f);
        let field = solve_mom(&g, &spec, opts()).unwrap();
        let s21 = field.network.s[0][1][0].norm();
        assert!(s21 > 0.2, "these two should couple: |S21| = {s21}");
        for phase in [0.0f64, 90.0, 180.0] {
            let st = DriveState { name: "t".into(), drive: vec![Drive { amp: 1.0, phase: 0.0 }, Drive { amp: 1.0, phase }] };
            let w = st.weights(2).unwrap();
            let far = combine(&field.far.iter().collect::<Vec<_>>(), &w).unwrap();
            let m = far.metrics();
            let tarc = field.network.tarc(&w)[0];
            // Radiated power from the pattern: gain over directivity, with 1 W incident in all.
            let radiated = m.total_efficiency;
            let accepted = 1.0 - tarc * tarc;
            println!("phase {phase}: radiated {radiated:.3}, 1 − TARC² {accepted:.3}");
            assert!((radiated - accepted).abs() < 0.03, "phase {phase}: {radiated} vs {accepted}");
        }
    }

    fn fake_field(field_at: impl Fn(P3) -> [C64; 3], spec: &GateSpec, geom: &Geometry) -> GateField {
        let s = samples(geom, spec, &[]).unwrap();
        let e = vec![s.points.iter().map(|p| field_at(*p)).collect()];
        GateField { f: spec.f, solver: "fake".into(), samples: s, e, network: Network { f: vec![spec.f], z0: vec![50.0], s: vec![vec![vec![C64::new(0.0, 0.0)]]] }, ports: vec!["P".into()], ..Default::default() }
    }

    fn lonely_geom() -> Geometry {
        Geometry { solids: vec![sheet("far away", [-50.0, -50.0, 50.0], [-49.0, -49.0, 50.0])], ports: Vec::new(), ..Default::default() }
    }

    #[test]
    fn a_uniform_strong_field_reads_everything_in_its_polarisation_only() {
        let g = lonely_geom();
        let mut spec = spec_for(&g, 915e6);
        spec.aisle.stray_from = 3.0;
        // Along z only: every tag with a z component reads; those at right angles can't.
        let f = fake_field(|_| [C64::new(0.0, 0.0), C64::new(0.0, 0.0), C64::new(100.0, 0.0)], &spec, &g);
        let st = DriveState { name: "on".into(), drive: vec![Drive { amp: 1.0, phase: 0.0 }] };
        let r = evaluate(&f, &spec, &[st]);
        assert_eq!(r.by_orientation[0].1, 0.0, "x, y and their diagonals see nothing: {:?}", r.by_orientation);
        let z = r.by_orientation.iter().find(|o| o.0 == "z").unwrap().1;
        assert_eq!(z, 1.0);
        assert!((r.read - 5.0 / 9.0).abs() < 1e-9, "{}", r.read);
    }

    #[test]
    fn a_short_passage_can_miss_a_state_that_cycling_needs() {
        let g = lonely_geom();
        let mut spec = spec_for(&g, 915e6);
        spec.aisle.length = 0.6;
        spec.spacing = 0.1;
        // Only the two samples nearest the gate plane have any field.
        let f = fake_field(|p| if p[1].abs() < 0.06 { [C64::new(0.0, 0.0), C64::new(0.0, 0.0), C64::new(100.0, 0.0)] } else { [C64::new(0.0, 0.0); 3] }, &spec, &g);
        let on = |name: &str| DriveState { name: name.into(), drive: vec![Drive { amp: 1.0, phase: 0.0 }] };
        spec.dwell_s = 0.05;
        let one = evaluate(&f, &spec, &[on("a")]);
        // Ten states at 50 ms: half a second per trip, 75 cm at walking pace, longer
        // than the 20 cm the field covers. Nothing is sure to read any more.
        let ten: Vec<DriveState> = (0..10).map(|k| on(&format!("s{k}"))).collect();
        let many = evaluate(&f, &spec, &ten);
        assert!(one.read > 0.5 && many.read == 0.0, "{} {}", one.read, many.read);
        assert!(many.window > one.window);
    }

    #[test]
    fn the_budget_says_which_link_limits() {
        let g = lonely_geom();
        let spec = spec_for(&g, 915e6);
        // A −20 dBm chip at 30 dBm needs about −45.5 dB one way; −80 dBm back needs −50.5.
        let (n, reverse) = spec.needed_gain(30.0, 3.0, -20.0);
        assert!((n - (-45.5)).abs() < 1e-9, "{n}");
        assert!(!reverse);
        // A deaf reader: the way back limits.
        let deaf = GateSpec { reader_sensitivity_dbm: -60.0, ..spec };
        let (n, reverse) = deaf.needed_gain(30.0, 3.0, -20.0);
        assert!(reverse && (n - (-37.5)).abs() < 1e-9, "{n}");
    }

    #[test]
    fn samples_stay_off_the_metal_and_stray_stays_out_of_the_zone() {
        let f = 915e6;
        let g = row(1, 0.0, f);
        let spec = spec_for(&g, f);
        let s = samples(&g, &spec, &[]).unwrap();
        assert!(s.excluded > 0, "the zone starts at the dipole");
        for p in &s.points {
            let (lo, hi) = g.bounds();
            assert!((0..3).any(|k| p[k] < lo[k] - spec.clear || p[k] > hi[k] + spec.clear), "{p:?}");
        }
        let (zl, zh) = spec.aisle.zone();
        for p in &s.points[s.stray_from..] {
            assert!((0..3).any(|k| p[k] < zl[k] || p[k] > zh[k]), "{p:?}");
        }
    }

    #[test]
    fn states_put_one_watt_in_the_feed_whatever_their_amplitudes() {
        let st = DriveState { name: "t".into(), drive: vec![Drive { amp: 3.0, phase: 10.0 }, Drive { amp: 4.0, phase: 70.0 }] };
        let w = st.weights(2).unwrap();
        let total: f64 = w.iter().map(C64::norm_sqr).sum();
        assert!((total - 1.0).abs() < 1e-12);
        assert!((w[1].arg() - w[0].arg() - 60f64.to_radians()).abs() < 1e-12);
        assert!(DriveState { name: "off".into(), drive: vec![Drive { amp: 0.0, phase: 0.0 }] }.weights(1).is_none());
        // One switch at a time: as many states as elements, each driving one.
        let mux = preset_states(Preset::OneAtATime, 5);
        assert_eq!(mux.len(), 5);
        assert_eq!(mux[2].weights(5).unwrap().iter().filter(|w| w.norm() > 0.0).count(), 1);
        assert_eq!(preset_states(Preset::Sweep, 5).len(), 5);
    }

    #[test]
    fn a_range_comes_with_the_prediction() {
        let g = lonely_geom();
        let spec = spec_for(&g, 915e6);
        let report = GateReport { walk_table: (0..600).map(|i| -60.0 + 30.0 * i as f64 / 600.0).collect(), stray_table: (0..600).map(|i| -95.0 + 30.0 * i as f64 / 600.0).collect(), ..Default::default() };
        let (read, stray) = predict(&report, &spec, 300);
        assert!(read.prediction.low < read.prediction.high, "{:?}", read.prediction);
        assert_eq!(read.prediction.evidence, Evidence::Estimate);
        assert!(stray.prediction.median < read.prediction.median);
    }

    /// How the answer settles as the mesh refines:
    /// `cargo test -p antenna_core --release gate_mesh_audit -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gate_mesh_audit() {
        let p = Project::from_template("rfid_gate", None).unwrap();
        let g = p.geometry().unwrap();
        let spec = GateSpec::for_project(&p, &g);
        let solo: Vec<DriveState> = preset_states(Preset::InPhase, 5);
        for edges in [6.0, 8.0, 10.0, 12.0, 16.0, 20.0] {
            let opts = MomOptions { edges_per_lambda: edges, theta_step: 5.0, phi_step: 5.0, ..Default::default() };
            let t = std::time::Instant::now();
            let field = solve_mom(&g, &spec, opts).unwrap();
            let r = evaluate(&field, &spec, &p.states);
            let one = evaluate(&field, &spec, &solo);
            let walk_mean = r.walk_table.iter().sum::<f64>() / r.walk_table.len() as f64;
            println!(
                "{edges:4} edges/λ {:6} unknowns {:6.1} s  S11 {:6.2} dB  read {:.3}  stray {:.3}  mean gain {:7.3} dB  in phase: read {:.3} EIRP {:.2} dBm",
                field.size,
                t.elapsed().as_secs_f64(),
                db20(field.network.s[0][0][0].norm()),
                r.read,
                r.stray,
                walk_mean,
                one.read,
                one.states[0].eirp_dbm.unwrap_or(0.0)
            );
        }
    }

    /// What each way of driving the gate does:
    /// `cargo test -p antenna_core --release gate_states_audit -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gate_states_audit() {
        let p = Project::from_template("rfid_gate", None).unwrap();
        let g = p.geometry().unwrap();
        let spec = GateSpec::for_project(&p, &g);
        let opts = MomOptions { edges_per_lambda: 10.0, theta_step: 5.0, phi_step: 5.0, ..Default::default() };
        let field = solve_mom(&g, &spec, opts).unwrap();
        println!("{} unknowns, {} samples, {} excluded, in {:.1} s", field.size, field.samples.points.len(), field.samples.excluded, field.seconds);
        for preset in [Preset::InPhase, Preset::Progressive(72), Preset::Progressive(90), Preset::Alternating, Preset::OneAtATime, Preset::Sweep] {
            let st = preset_states(preset, 5);
            let r = evaluate(&field, &spec, &st);
            let loud = r.states.iter().filter_map(|s| s.eirp_dbm).fold(f64::MIN, f64::max);
            println!("{:28} {} states  read {:.3}  worst {} {:.3}  stray {:.3}  EIRP {:.1}  power for goal {:?}  legal {:?}", preset.label(), st.len(), r.read, r.by_orientation[0].0, r.by_orientation[0].1, r.stray, loud, r.power_for_goal, r.legal);
        }
        for (name, set) in [("0° and 90°", default_states(5)), ("0°, 90°, −90°", vec![ramp("a".into(), 5, 0.0), ramp("b".into(), 5, 90.0), ramp("c".into(), 5, -90.0)]), ("four states", vec![ramp("a".into(), 5, 0.0), ramp("b".into(), 5, 90.0), ramp("c".into(), 5, -90.0), ramp("d".into(), 5, 180.0)])] {
            let r = evaluate(&field, &spec, &set);
            println!("{name:28} {} states  read {:.3}  worst {} {:.3}  stray {:.3}  power for goal {:?}  legal {:?}", set.len(), r.read, r.by_orientation[0].0, r.by_orientation[0].1, r.stray, r.power_for_goal, r.legal);
        }
        for n in [1, 2, 3] {
            let t = std::time::Instant::now();
            let found = search_phases(&field, &spec, n, 800, 7);
            let f = evaluate(&field, &spec, &found);
            println!("searched {n} states in {:.1} s: read {:.3} stray {:.3} worst {:?} legal {:?}", t.elapsed().as_secs_f64(), f.read, f.stray, f.by_orientation[0], f.legal);
            for s in &found {
                println!("  {:?}", s.drive.iter().map(|d| d.phase.round()).collect::<Vec<_>>());
            }
        }
    }

    /// The whole test on the catalogue's gate.
    #[test]
    fn the_five_element_gate_template_runs_end_to_end() {
        let p = Project::from_template("rfid_gate", None).unwrap();
        let g = p.geometry().unwrap();
        assert_eq!(g.ports.len(), 5);
        assert!(g.solver_fit().surface_mom);
        let mut spec = GateSpec::for_project(&p, &g);
        spec.spacing = 0.2;
        let opts = MomOptions { edges_per_lambda: 8.0, theta_step: 10.0, phi_step: 10.0, ..Default::default() };
        let t = std::time::Instant::now();
        let field = solve_mom(&g, &spec, opts).unwrap();
        println!("{} unknowns, {} samples, {:.1} s", field.size, field.samples.points.len(), t.elapsed().as_secs_f64());
        for i in 0..5 {
            println!("S{0}{0} {1:.1} dB", i + 1, db20(field.network.s[0][i][i].norm()));
        }
        let r = evaluate(&field, &spec, &p.states);
        println!("read {:.3}, stray {:.3}, needed {:.1} dB, window {}", r.read, r.stray, r.needed_db, r.window);
        println!("by orientation {:?}", r.by_orientation);
        for s in &r.states {
            println!("{s:?}");
        }
        assert_eq!(r.states.len(), p.states.len());
        assert!(r.read > 0.0 && r.read <= 1.0);
        // Power buys reads, and the way the gate is driven matters.
        let low = evaluate(&field, &GateSpec { tx_dbm: 15.0, ..spec }, &p.states);
        assert!(r.read >= low.read);
        let many = r.states.iter().map(|s| s.read).fold(0.0, f64::max);
        println!("best single state {many:.3}");
        assert!(r.states.iter().any(|s| (s.read - many).abs() > 1e-6), "states must differ");
    }
}
