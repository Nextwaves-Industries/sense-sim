//! How an antenna shifts UWB ranges with direction.
//!
//! A UWB chip times the leading edge of the received pulse. The antenna reshapes
//! that pulse differently in each direction, so the range comes out long or short
//! depending on which way the antenna faces. A fixed antenna-delay calibration
//! removes only the average; the part that changes with direction stays, and it
//! ends up as position error. Calibrating harder can't fix it. Antenna design can.
//!
//! This builds the received pulse per direction from the antenna's complex far
//! field across the channel, finds its leading edge the way a first-path detector
//! does, and compares it with the ideal pulse.

use std::f64::consts::{PI, TAU};

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::{mom_far_fields, unwrap};
use crate::farfield::FarField;
use crate::geometry::Geometry;
use crate::mom::MomOptions;
use crate::optimize::{self, Method, Options};
use crate::project::Project;
use crate::{C0, C64, ETA0, linspace};

/// A UWB channel.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub f0: f64,
    pub bw: f64,
}

impl Channel {
    /// IEEE 802.15.4z channel 5.
    pub const CH5: Self = Self { f0: 6.4896e9, bw: 499.2e6 };
    /// IEEE 802.15.4z channel 9.
    pub const CH9: Self = Self { f0: 7.9872e9, bw: 499.2e6 };

    /// The band a pulse with this roll-off occupies around `f0`.
    pub fn half_span(&self) -> f64 {
        0.75 * self.bw
    }

    /// Frequencies to solve for a bias analysis.
    pub fn solve_freqs(&self, n: usize) -> Vec<f64> {
        linspace(self.f0 - self.half_span(), self.f0 + self.half_span(), n.max(3))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BiasOptions {
    pub channel: Channel,
    /// Leading edge: where the pulse first reaches this share of its peak.
    pub edge: f64,
    /// Directions more than this far below the strongest (dB) can't carry a link.
    pub usable_db: f64,
}

impl Default for BiasOptions {
    fn default() -> Self {
        Self { channel: Channel::CH5, edge: 0.25, usable_db: 20.0 }
    }
}

/// Which directions matter: a tag on a person never sees an anchor below its feet.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub theta_min: f64,
    pub theta_max: f64,
}

impl Region {
    pub const ALL: Self = Self { theta_min: 0.0, theta_max: 180.0 };
    /// A vertical antenna seeing anchors around the horizon.
    pub const HORIZON: Self = Self { theta_min: 45.0, theta_max: 135.0 };
}

/// Range shift per direction. `θ` and `φ` follow the far-field grid.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BiasMap {
    pub f0: f64,
    pub theta: Vec<f64>,
    pub phi: Vec<f64>,
    /// Range error (m) with the average removed: what remains after a normal
    /// antenna-delay calibration. `bias[θ][φ]`; positive reads long.
    pub bias_m: Vec<Vec<f64>>,
    /// Mean realised gain over the band, dBi.
    pub gain_dbi: Vec<Vec<f64>>,
    /// Pulse fidelity, 1 is undistorted.
    pub fidelity: Vec<Vec<f64>>,
    /// The removed average (m): the antenna delay a calibration would absorb.
    pub mean_delay_m: f64,
    /// Can carry a link (not in a null).
    pub usable: Vec<Vec<bool>>,
}

/// A single number per concern, over a region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BiasMetrics {
    pub rms_m: f64,
    pub p95_abs_m: f64,
    pub peak_to_peak_m: f64,
    pub fidelity_mean: f64,
    pub fidelity_min: f64,
    pub gain_mean_dbi: f64,
    pub gain_min_dbi: f64,
    /// Share of directions in the region that can carry a link.
    pub usable_share: f64,
}

/// Raised-cosine spectrum (pulse after the receiver's matched filter), `bw` wide.
fn pulse_weight(df: f64, bw: f64) -> f64 {
    let beta = 0.5;
    let t = 1.0 / bw;
    let a = df.abs();
    let (f1, f2) = ((1.0 - beta) / (2.0 * t), (1.0 + beta) / (2.0 * t));
    if a <= f1 {
        1.0
    } else if a >= f2 {
        0.0
    } else {
        0.5 * (1.0 + (PI * t / beta * (a - f1)).cos())
    }
}

/// The far field's polarisation that collects the most energy over the band, as
/// a receiving antenna aligned to it would. Returns the transfer at each sample.
fn matched_transfer(samples: &[(C64, C64)]) -> Vec<C64> {
    // Principal axis of the 2×2 polarisation covariance.
    let (mut a, mut b, mut c) = (0.0, C64::new(0.0, 0.0), 0.0);
    for (t, p) in samples {
        a += t.norm_sqr();
        b += *t * p.conj();
        c += p.norm_sqr();
    }
    if a + c <= 0.0 {
        return vec![C64::new(0.0, 0.0); samples.len()];
    }
    let (ut, up) = if b.norm() < 1e-12 * (a + c) {
        if a >= c { (C64::new(1.0, 0.0), C64::new(0.0, 0.0)) } else { (C64::new(0.0, 0.0), C64::new(1.0, 0.0)) }
    } else {
        let lambda = 0.5 * (a + c) + (0.25 * (a - c).powi(2) + b.norm_sqr()).sqrt();
        let (x, y) = (b, C64::new(lambda - a, 0.0));
        let n = (x.norm_sqr() + y.norm_sqr()).sqrt();
        (x / n, y / n)
    };
    samples.iter().map(|(t, p)| ut.conj() * *t + up.conj() * *p).collect()
}

/// A pulse measured against a reference: leading-edge time (s) and fidelity.
struct Timing {
    edge_s: f64,
    fidelity: f64,
}

struct PulseGrid {
    /// Offsets from f0 and the pulse weight at each.
    df: Vec<f64>,
    w: Vec<f64>,
    /// Evaluation times and, per time, the phasor at each offset.
    times: Vec<f64>,
    edge: f64,
    reference_edge: f64,
}

impl PulseGrid {
    fn new(ch: Channel, edge: f64) -> Self {
        let df = linspace(-ch.half_span(), ch.half_span(), 65);
        let w: Vec<f64> = df.iter().map(|d| pulse_weight(*d, ch.bw)).collect();
        let times = linspace(-8e-9, 8e-9, 801);
        let mut g = Self { df, w, times, edge, reference_edge: 0.0 };
        let ideal = vec![C64::new(1.0, 0.0); g.df.len()];
        g.reference_edge = g.leading_edge(&ideal).unwrap_or(0.0);
        g
    }

    fn envelope(&self, h: &[C64]) -> Vec<f64> {
        self.times
            .iter()
            .map(|t| {
                let mut y = C64::new(0.0, 0.0);
                for k in 0..self.df.len() {
                    let (s, c) = (TAU * self.df[k] * t).sin_cos();
                    y += h[k] * C64::new(c, s) * self.w[k];
                }
                y.norm()
            })
            .collect()
    }

    /// Walks back from the peak to where the envelope first drops below the edge
    /// level, so a side lobe ahead of the pulse isn't taken for the first path.
    fn leading_edge(&self, h: &[C64]) -> Option<f64> {
        let e = self.envelope(h);
        let (peak_i, peak) = e.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, v)| (i, *v))?;
        if !(peak > 0.0) {
            return None;
        }
        let level = self.edge * peak;
        let mut i = peak_i;
        while i > 0 && e[i - 1] >= level {
            i -= 1;
        }
        if i == 0 {
            return Some(self.times[0]);
        }
        let frac = (level - e[i - 1]) / (e[i] - e[i - 1]);
        Some(self.times[i - 1] + frac * (self.times[i] - self.times[i - 1]))
    }

    fn measure(&self, h: &[C64]) -> Option<Timing> {
        let edge = self.leading_edge(h)?;
        // Correlation with the ideal pulse, best alignment: 1 when H is only a delay and a gain.
        let (mut hh, mut rr) = (0.0, 0.0);
        for k in 0..self.df.len() {
            hh += (h[k] * self.w[k]).norm_sqr();
            rr += self.w[k] * self.w[k];
        }
        let norm = (hh * rr).sqrt();
        let fidelity = self
            .times
            .iter()
            .map(|t| {
                let mut y = C64::new(0.0, 0.0);
                for k in 0..self.df.len() {
                    let (s, c) = (TAU * self.df[k] * t).sin_cos();
                    y += h[k] * C64::new(c, s) * (self.w[k] * self.w[k]);
                }
                y.norm()
            })
            .fold(0.0, f64::max);
        Some(Timing { edge_s: edge - self.reference_edge, fidelity: if norm > 0.0 { (fidelity / norm).min(1.0) } else { 0.0 } })
    }
}

/// Resamples a band of complex samples onto `to`, in magnitude and unwrapped phase.
fn resample(from: &[f64], h: &[C64], to: &[f64]) -> Vec<C64> {
    let mag: Vec<f64> = h.iter().map(|v| v.norm()).collect();
    let mut ph: Vec<f64> = h.iter().map(|v| v.arg()).collect();
    unwrap(&mut ph);
    to.iter()
        .map(|f| {
            let i = from.partition_point(|x| x < f).clamp(1, from.len() - 1);
            let t = ((f - from[i - 1]) / (from[i] - from[i - 1])).clamp(0.0, 1.0);
            let m = mag[i - 1] + t * (mag[i] - mag[i - 1]);
            let p = ph[i - 1] + t * (ph[i] - ph[i - 1]);
            C64::from_polar(m, p)
        })
        .collect()
}

/// The range shift per direction, from far fields solved across the channel on
/// one angular grid (1 W incident, so realised gain is in the numbers).
pub fn bias_map(fields: &[FarField], opts: BiasOptions) -> Result<BiasMap> {
    let Some(first) = fields.first() else { bail!("no far fields to analyse") };
    if fields.len() < 3 {
        bail!("a bias analysis needs far fields at 3 or more frequencies across the channel, got {}", fields.len());
    }
    if fields.iter().any(|f| f.theta.len() != first.theta.len() || f.phi.len() != first.phi.len()) {
        bail!("the far fields are on different angular grids");
    }
    let mut freqs: Vec<f64> = fields.iter().map(|f| f.f).collect();
    if freqs.windows(2).any(|w| w[1] <= w[0]) {
        bail!("far fields must be sorted by frequency");
    }
    let span = opts.channel.half_span();
    if freqs[0] > opts.channel.f0 - 0.5 * span || *freqs.last().unwrap() < opts.channel.f0 + 0.5 * span {
        bail!("the far fields cover {:.3}–{:.3} GHz, which doesn't span the {:.3} GHz channel", freqs[0] / 1e9, freqs.last().unwrap() / 1e9, opts.channel.f0 / 1e9);
    }
    freqs.shrink_to_fit();

    let grid = PulseGrid::new(opts.channel, opts.edge);
    let to: Vec<f64> = grid.df.iter().map(|d| opts.channel.f0 + d).collect();
    let (nt, np) = (first.theta.len(), first.phi.len());
    let cell = |ti: usize, pi: usize| -> (f64, f64, f64) {
        let samples: Vec<(C64, C64)> = fields.iter().map(|f| f.at(ti, pi)).collect();
        let gain = samples.iter().map(|(t, p)| (t.norm_sqr() + p.norm_sqr()) / (2.0 * ETA0)).sum::<f64>() / samples.len() as f64 * 4.0 * PI;
        let h = resample(&freqs, &matched_transfer(&samples), &to);
        match grid.measure(&h) {
            Some(t) => (t.edge_s * C0, t.fidelity, 10.0 * gain.max(1e-30).log10()),
            None => (f64::NAN, 0.0, -300.0),
        }
    };
    let rows: Vec<Vec<(f64, f64, f64)>> = (0..nt).into_par_iter().map(|ti| (0..np).map(|pi| cell(ti, pi)).collect()).collect();

    let peak = rows.iter().flatten().map(|c| c.2).fold(f64::MIN, f64::max);
    let usable: Vec<Vec<bool>> = rows.iter().map(|r| r.iter().map(|c| c.0.is_finite() && c.2 >= peak - opts.usable_db).collect()).collect();
    // The average a delay calibration absorbs, over directions that can carry a link.
    let (mut sum, mut weight) = (0.0, 0.0);
    for (ti, r) in rows.iter().enumerate() {
        let w = first.theta[ti].to_radians().sin().max(1e-3);
        for pi in 0..np - 1 {
            if usable[ti][pi] {
                sum += r[pi].0 * w;
                weight += w;
            }
        }
    }
    let mean = if weight > 0.0 { sum / weight } else { 0.0 };
    Ok(BiasMap {
        f0: opts.channel.f0,
        theta: first.theta.clone(),
        phi: first.phi.clone(),
        bias_m: rows.iter().map(|r| r.iter().map(|c| c.0 - mean).collect()).collect(),
        gain_dbi: rows.iter().map(|r| r.iter().map(|c| c.2).collect()).collect(),
        fidelity: rows.iter().map(|r| r.iter().map(|c| c.1).collect()).collect(),
        mean_delay_m: mean,
        usable,
    })
}

impl BiasMap {
    /// Summary over a region. Directions in nulls don't count, because no link uses them.
    pub fn metrics(&self, region: Region) -> BiasMetrics {
        let (mut vals, mut fid, mut gain): (Vec<(f64, f64)>, Vec<(f64, f64)>, Vec<(f64, f64)>) = (Vec::new(), Vec::new(), Vec::new());
        let (mut seen, mut ok) = (0.0, 0.0);
        for (ti, t) in self.theta.iter().enumerate() {
            if *t < region.theta_min - 1e-9 || *t > region.theta_max + 1e-9 {
                continue;
            }
            let w = t.to_radians().sin().max(1e-3);
            for pi in 0..self.phi.len().saturating_sub(1) {
                seen += w;
                if self.usable[ti][pi] {
                    ok += w;
                    vals.push((self.bias_m[ti][pi], w));
                    fid.push((self.fidelity[ti][pi], w));
                    gain.push((self.gain_dbi[ti][pi], w));
                }
            }
        }
        if vals.is_empty() {
            // No direction can carry a link: that's no answer, not zero error.
            let none = f64::NAN;
            return BiasMetrics { rms_m: none, p95_abs_m: none, peak_to_peak_m: none, fidelity_mean: none, fidelity_min: none, gain_mean_dbi: none, gain_min_dbi: none, usable_share: 0.0 };
        }
        let total: f64 = vals.iter().map(|v| v.1).sum();
        let mean = vals.iter().map(|(v, w)| v * w).sum::<f64>() / total;
        let rms = (vals.iter().map(|(v, w)| (v - mean).powi(2) * w).sum::<f64>() / total).sqrt();
        let mut abs: Vec<(f64, f64)> = vals.iter().map(|(v, w)| ((v - mean).abs(), *w)).collect();
        abs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut acc = 0.0;
        let mut p95 = abs.last().map_or(0.0, |a| a.0);
        for (v, w) in &abs {
            acc += w;
            if acc >= 0.95 * total {
                p95 = *v;
                break;
            }
        }
        let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), (v, _)| (a.min(*v), b.max(*v)));
        let avg = |x: &[(f64, f64)]| x.iter().map(|(v, w)| v * w).sum::<f64>() / total;
        BiasMetrics {
            rms_m: rms,
            p95_abs_m: p95,
            peak_to_peak_m: hi - lo,
            fidelity_mean: avg(&fid),
            fidelity_min: fid.iter().map(|v| v.0).fold(1.0, f64::min),
            gain_mean_dbi: avg(&gain),
            gain_min_dbi: gain.iter().map(|v| v.0).fold(f64::MAX, f64::min),
            usable_share: ok / seen.max(1e-12),
        }
    }

    /// The map as the simulator takes it. It has no way to say "no link here", so a null
    /// gets the worst shift of the usable directions, not zero, the kindest value there
    /// is. `None` if no direction is usable.
    pub fn for_simulator(&self) -> Option<Vec<Vec<f32>>> {
        let worst = self.bias_m.iter().flatten().zip(self.usable.iter().flatten()).filter(|(_, ok)| **ok).map(|(v, _)| v.abs()).fold(f64::NAN, f64::max);
        if worst.is_nan() {
            return None;
        }
        Some(self.bias_m.iter().zip(&self.usable).map(|(r, u)| r.iter().zip(u).map(|(v, ok)| if *ok { *v as f32 } else { worst as f32 }).collect()).collect())
    }

    /// Bias (m) towards a direction, nearest grid point; `None` in a null.
    pub fn at(&self, theta: f64, phi: f64) -> Option<f64> {
        let step = |v: &[f64]| if v.len() > 1 { v[1] - v[0] } else { 1.0 };
        let ti = ((theta / step(&self.theta)).round() as usize).min(self.theta.len().saturating_sub(1));
        let pi = ((phi.rem_euclid(360.0) / step(&self.phi)).round() as usize).min(self.phi.len().saturating_sub(1));
        self.usable.get(ti)?.get(pi).copied().unwrap_or(false).then(|| self.bias_m[ti][pi])
    }
}

/// Solves a metal-only geometry across the channel and maps its bias.
pub fn analyse_mom(geom: &Geometry, port: usize, n_freqs: usize, angle_step: f64, opts: BiasOptions, edges_per_lambda: f64) -> Result<BiasMap> {
    let mom = MomOptions { edges_per_lambda, segments_per_lambda: edges_per_lambda.max(16.0), theta_step: angle_step, phi_step: angle_step };
    let fields = mom_far_fields(geom, &opts.channel.solve_freqs(n_freqs), port, mom)?;
    bias_map(&fields, opts)
}

/// Goal of a bias optimisation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BiasGoal {
    pub region: Region,
    /// Peak realised gain floor over the region: stops a design "fixing" bias by
    /// not radiating at all.
    pub min_gain_dbi: f64,
    /// Weight of 1 dB below the floor, in cm of bias.
    pub gain_weight: f64,
}

impl Default for BiasGoal {
    fn default() -> Self {
        Self { region: Region::HORIZON, min_gain_dbi: -3.0, gain_weight: 1.0 }
    }
}

/// Lower is better: RMS bias in cm, plus the gain shortfall and any directions lost to nulls.
pub fn score(m: &BiasMetrics, goal: &BiasGoal) -> f64 {
    m.rms_m * 100.0 + (goal.min_gain_dbi - m.gain_mean_dbi).max(0.0) * goal.gain_weight + (1.0 - m.usable_share) * 20.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BiasDesign {
    pub names: Vec<String>,
    pub start: Vec<f64>,
    pub best: Vec<f64>,
    pub before: BiasMetrics,
    pub after: BiasMetrics,
    pub evals: usize,
}

/// Searches the named variables for the lowest bias (method of moments, so the
/// project must be all metal). `bounds` are absolute values in SI units.
pub fn optimise(project: &Project, vars: &[(String, f64, f64)], opts: BiasOptions, goal: BiasGoal, n_freqs: usize, angle_step: f64, edges_per_lambda: f64, max_evals: usize) -> Result<BiasDesign> {
    let values = project.vars()?;
    let start: Vec<f64> = vars.iter().map(|(n, ..)| values.get(n).copied().ok_or_else(|| anyhow::anyhow!("no variable '{n}' in the design"))).collect::<Result<_>>()?;
    let bounds: Vec<(f64, f64)> = vars.iter().map(|(_, a, b)| (*a, *b)).collect();
    let names: Vec<String> = vars.iter().map(|v| v.0.clone()).collect();
    let eval = |x: &[f64]| -> Result<BiasMetrics> {
        let pairs: Vec<(String, f64)> = names.iter().cloned().zip(x.iter().copied()).collect();
        let g = project.with_values(&pairs).geometry()?;
        Ok(analyse_mom(&g, 0, n_freqs, angle_step, opts, edges_per_lambda)?.metrics(goal.region))
    };
    let before = eval(&start)?;
    let outcome = optimize::minimize(
        |x: &[f64]| eval(x).map_or(f64::NAN, |m| score(&m, &goal)),
        &start,
        &bounds,
        &Options { method: Method::Surrogate, max_evals, seed: 1, population: Some((2 * vars.len() + 2).min(max_evals / 2).max(2)), tol: 1e-3, parallel: 1 },
        |_| true,
    );
    let start_score = score(&before, &goal);
    // The optimiser never reports a design worse than where it started.
    let best = if outcome.value < start_score { outcome.x } else { start.clone() };
    let after = eval(&best)?;
    Ok(BiasDesign { names, start, best, before, after, evals: outcome.evals + 2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{P3, PortDef, Shape, Solid};
    use crate::project::Project;

    /// Far fields of ideal current samples: the field of each with its phase.
    fn fields_of(moments: &[(P3, [f64; 3], f64)], ch: Channel, step: f64) -> Vec<FarField> {
        ch.solve_freqs(9)
            .into_iter()
            .map(|f| {
                let k = TAU * f / C0;
                let mut ff = FarField::grid(f, step, step);
                ff.p_incident = 1.0;
                ff.p_accepted = 1.0;
                for (ti, t) in ff.theta.clone().iter().enumerate() {
                    for (pi, p) in ff.phi.clone().iter().enumerate() {
                        let (st, ct) = t.to_radians().sin_cos();
                        let (sp, cp) = p.to_radians().sin_cos();
                        let r = [st * cp, st * sp, ct];
                        let th = [ct * cp, ct * sp, -st];
                        let ph = [-sp, cp, 0.0];
                        for (pos, j, amp) in moments {
                            let phase = k * (r[0] * pos[0] + r[1] * pos[1] + r[2] * pos[2]);
                            let v = C64::from_polar(*amp, phase);
                            ff.e_theta[ti][pi] += v * (th[0] * j[0] + th[1] * j[1] + th[2] * j[2]);
                            ff.e_phi[ti][pi] += v * (ph[0] * j[0] + ph[1] * j[1] + ph[2] * j[2]);
                        }
                    }
                }
                ff
            })
            .collect()
    }

    fn ring(map: &BiasMap, phi: f64) -> f64 {
        map.at(90.0, phi).expect("usable")
    }

    #[test]
    fn an_element_at_the_origin_has_no_bias() {
        let f = fields_of(&[([0.0; 3], [0.0, 0.0, 1.0], 10.0)], Channel::CH5, 15.0);
        let map = bias_map(&f, BiasOptions::default()).unwrap();
        let m = map.metrics(Region::HORIZON);
        assert!(m.rms_m < 1e-4, "{m:?}");
        assert!(m.fidelity_min > 0.999, "{m:?}");
        assert!(map.mean_delay_m.abs() < 1e-3, "{}", map.mean_delay_m);
    }

    #[test]
    fn an_offset_antenna_reads_short_towards_its_offset_by_exactly_that_distance() {
        // 4 cm off along +x: the wave towards +x starts 4 cm closer, so the range reads
        // 4 cm short, and 4 cm long the other way. This is the geometry check.
        let d = 0.04;
        let f = fields_of(&[([d, 0.0, 0.0], [0.0, 0.0, 1.0], 10.0)], Channel::CH5, 15.0);
        let map = bias_map(&f, BiasOptions::default()).unwrap();
        assert!((ring(&map, 0.0) + d).abs() < 1e-3, "{}", ring(&map, 0.0));
        assert!((ring(&map, 180.0) - d).abs() < 1e-3, "{}", ring(&map, 180.0));
        assert!((ring(&map, 90.0)).abs() < 1e-3, "{}", ring(&map, 90.0));
        let m = map.metrics(Region { theta_min: 90.0, theta_max: 90.0 });
        assert!((m.peak_to_peak_m - 2.0 * d).abs() < 2e-3, "{m:?}");
        // A shift alone doesn't distort the pulse.
        assert!(m.fidelity_min > 0.999, "{m:?}");
    }

    #[test]
    fn two_paths_distort_the_pulse_where_they_interfere() {
        // A strong element and a delayed echo 30 cm behind it: the pulse changes shape.
        let f = fields_of(&[([0.0; 3], [0.0, 0.0, 1.0], 10.0), ([-0.3, 0.0, 0.0], [0.0, 0.0, 1.0], 6.0)], Channel::CH5, 15.0);
        let map = bias_map(&f, BiasOptions::default()).unwrap();
        let m = map.metrics(Region { theta_min: 90.0, theta_max: 90.0 });
        assert!(m.fidelity_min < 0.98, "{m:?}");
        assert!(m.rms_m > 0.002, "{m:?}");
    }

    #[test]
    fn a_polarisation_null_is_not_scored() {
        let f = fields_of(&[([0.0; 3], [0.0, 0.0, 1.0], 10.0)], Channel::CH5, 15.0);
        let map = bias_map(&f, BiasOptions::default()).unwrap();
        // A z dipole radiates nothing along z.
        assert!(map.at(0.0, 0.0).is_none());
        assert!(map.metrics(Region::ALL).usable_share < 1.0);
    }

    // OUT-M2: this returned all zeros, which printed as 0 cm and read as "Likely".
    #[test]
    fn no_usable_direction_is_no_answer_not_zero_error() {
        let f = fields_of(&[([0.0; 3], [0.0, 0.0, 1.0], 10.0)], Channel::CH5, 15.0);
        let map = bias_map(&f, BiasOptions::default()).unwrap();
        let m = map.metrics(Region { theta_min: 0.0, theta_max: 0.0 });
        assert_eq!(m.usable_share, 0.0);
        assert!(m.p95_abs_m.is_nan() && m.rms_m.is_nan(), "{m:?}");
        assert!(score(&m, &BiasGoal::default()).is_nan(), "an optimiser must not prefer it");
    }

    // OUT-M2: nulls went to the simulator as zero error.
    #[test]
    fn nulls_go_to_the_simulator_as_the_worst_shift_not_zero() {
        let d = 0.04;
        let f = fields_of(&[([d, 0.0, 0.0], [0.0, 0.0, 1.0], 10.0)], Channel::CH5, 15.0);
        let map = bias_map(&f, BiasOptions::default()).unwrap();
        let sim = map.for_simulator().unwrap();
        let worst = map.bias_m.iter().flatten().zip(map.usable.iter().flatten()).filter(|(_, ok)| **ok).map(|(v, _)| v.abs()).fold(0.0, f64::max);
        assert!(worst > 0.03, "{worst}");
        assert!((sim[0][0] as f64 - worst).abs() < 1e-6, "a z dipole's null along z: {} of {worst}", sim[0][0]);
        let ti = map.theta.iter().position(|t| *t == 90.0).unwrap();
        assert!((sim[ti][0] as f64 - map.bias_m[ti][0]).abs() < 1e-6, "usable directions keep their own value");
        let mut none = map.clone();
        none.usable.iter_mut().flatten().for_each(|u| *u = false);
        assert!(none.for_simulator().is_none());
    }

    #[test]
    fn bad_input_gets_a_plain_message() {
        assert!(bias_map(&[], BiasOptions::default()).is_err());
        let f = fields_of(&[([0.0; 3], [0.0, 0.0, 1.0], 1.0)], Channel::CH5, 30.0);
        assert!(bias_map(&f[..2], BiasOptions::default()).is_err());
        let wrong = BiasOptions { channel: Channel::CH9, ..Default::default() };
        let err = bias_map(&f, wrong).unwrap_err().to_string();
        assert!(err.contains("doesn't span"), "{err}");
    }

    fn sheet(name: &str, min: P3, max: P3) -> Solid {
        Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min, max } }
    }

    fn strip_dipole(x: f64) -> Vec<Solid> {
        // Half-wave at channel 5 (23 mm less the end effect), 3 mm wide, cut at the feed.
        let (w, l) = (0.0015, 0.0105);
        vec![sheet("up", [x - w, 0.0, 0.0005], [x + w, 0.0, l]), sheet("down", [x - w, 0.0, -l], [x + w, 0.0, -0.0005])]
    }

    fn feed(x: f64) -> PortDef {
        PortDef { name: "feed".into(), a: [x, 0.0, -0.0005], b: [x, 0.0, 0.0005], width: 0.003, z0: 50.0 }
    }

    #[test]
    fn a_real_dipole_is_uniform_and_a_nearby_plate_is_not() {
        let opts = BiasOptions::default();
        let free = Geometry { solids: strip_dipole(0.0), ports: vec![feed(0.0)], ..Default::default() };
        let m_free = analyse_mom(&free, 0, 5, 15.0, opts, 16.0).unwrap().metrics(Region::HORIZON);
        // A small plate beside it, as a battery or a board's ground would be.
        let mut near = free.clone();
        near.solids.push(sheet("plate", [0.012, -0.015, -0.025], [0.012, 0.015, 0.025]));
        let m_near = analyse_mom(&near, 0, 5, 15.0, opts, 16.0).unwrap().metrics(Region::HORIZON);
        println!("free {m_free:?}\nnear {m_near:?}");
        assert!(m_free.rms_m < 0.005, "free dipole {m_free:?}");
        assert!(m_near.rms_m > 2.0 * m_free.rms_m, "plate must matter: {} vs {}", m_near.rms_m, m_free.rms_m);
    }

    #[test]
    fn the_optimiser_never_makes_it_worse() {
        // Bowtie at channel 5, tuning its length: slow-ish but a real solve loop.
        let mut p = Project::from_template("strip_dipole", Some("6.4896GHz")).unwrap();
        p.setup.edges_per_lambda = 12.0;
        let values = p.vars().unwrap();
        let l = values["L"];
        let d = optimise(&p, &[("L".to_string(), 0.8 * l, 1.2 * l)], BiasOptions::default(), BiasGoal::default(), 3, 30.0, 12.0, 6).unwrap();
        assert!(score(&d.after, &BiasGoal::default()) <= score(&d.before, &BiasGoal::default()) + 1e-9, "{d:?}");
    }
}
