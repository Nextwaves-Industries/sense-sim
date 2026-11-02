//! Network data: S, Z and Y matrices over frequency, the figures derived from
//! them (VSWR, Q, bandwidth, group delay, active reflection, TARC, correlation,
//! time-domain reflectometry), Touchstone files, fast-sweep interpolation, and
//! the link-budget and small-antenna limits.

use std::f64::consts::PI;

use anyhow::{Result, anyhow, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::farfield::FarField;
use crate::{C0, C64, db10, db20};

/// `s[freq][i][j]`, power waves, real reference impedances `z0` per port.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Network {
    pub f: Vec<f64>,
    pub z0: Vec<f64>,
    pub s: Vec<Vec<Vec<C64>>>,
}

type Matrix = Vec<Vec<C64>>;

/// A finite stand-in for a pivot of zero, so an open circuit reads as a huge
/// impedance instead of a NaN.
const SINGULAR_PIVOT: f64 = 1e-12;

impl Network {
    pub fn ports(&self) -> usize {
        if self.z0.is_empty() { self.s.first().map_or(0, Vec::len) } else { self.z0.len() }
    }

    /// S from Z: `S = G^-½ (Z − G)(Z + G)⁻¹ G^½`, with `G = diag(z0)`.
    pub fn from_z(f: Vec<f64>, z: &[Vec<Vec<C64>>], z0: Vec<f64>) -> Self {
        let s = z
            .iter()
            .map(|zk| {
                let mut plus = zk.clone();
                let mut minus = zk.clone();
                for i in 0..zk.len() {
                    plus[i][i] += z0[i];
                    minus[i][i] -= z0[i];
                }
                let mut m = mat_mul(&minus, &inverse(&plus));
                for (i, row) in m.iter_mut().enumerate() {
                    for (j, v) in row.iter_mut().enumerate() {
                        *v *= (z0[j] / z0[i]).sqrt();
                    }
                }
                m
            })
            .collect();
        Self { f, z0, s }
    }

    /// Z at frequency index `k`: `G^½ (I − S)⁻¹ (I + S) G^½`.
    pub fn z_at(&self, k: usize) -> Vec<Vec<C64>> {
        let (plus, minus) = self.identity_shifted(k);
        let mut z = mat_mul(&inverse(&minus), &plus);
        for (i, row) in z.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v *= (self.z0[i] * self.z0[j]).sqrt();
            }
        }
        z
    }

    /// Y at frequency index `k`: `G^-½ (I + S)⁻¹ (I − S) G^-½`.
    pub fn y_at(&self, k: usize) -> Vec<Vec<C64>> {
        let (plus, minus) = self.identity_shifted(k);
        let mut y = mat_mul(&inverse(&plus), &minus);
        for (i, row) in y.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v /= (self.z0[i] * self.z0[j]).sqrt();
            }
        }
        y
    }

    /// `(I + S, I − S)` at frequency index `k`.
    fn identity_shifted(&self, k: usize) -> (Matrix, Matrix) {
        let n = self.ports();
        let mut plus = self.s[k].clone();
        let mut minus: Matrix = self.s[k].iter().map(|row| row.iter().map(|v| -*v).collect()).collect();
        for i in 0..n {
            plus[i][i] += 1.0;
            minus[i][i] += 1.0;
        }
        (plus, minus)
    }

    fn sij(&self, i: usize, j: usize) -> impl Iterator<Item = C64> + '_ {
        self.s.iter().map(move |m| m[i][j])
    }

    pub fn sij_db(&self, i: usize, j: usize) -> Vec<f64> {
        self.sij(i, j).map(|v| db20(v.norm())).collect()
    }

    /// Unwrapped phase of `S[i][j]`, degrees.
    pub fn sij_phase_deg(&self, i: usize, j: usize) -> Vec<f64> {
        unwrap_phase(self.sij(i, j)).iter().map(|p| p.to_degrees()).collect()
    }

    /// Input impedance with the other ports terminated in their `z0`.
    pub fn zin(&self, port: usize) -> Vec<C64> {
        let z0 = self.z0[port];
        self.sij(port, port).map(|g| reflection_to_z(g, z0)).collect()
    }

    /// VSWR, capped at 1e6 for a total reflection.
    pub fn vswr(&self, port: usize) -> Vec<f64> {
        self.sij(port, port)
            .map(|g| {
                let m = g.norm();
                if m >= 1.0 { 1e6 } else { ((1.0 + m) / (1.0 - m)).min(1e6) }
            })
            .collect()
    }

    /// `−10·log10(1 − |Sii|²)`: positive dB. Infinite when nothing is accepted, which
    /// includes a non-passive `|Sii|` above 1.
    pub fn mismatch_loss_db(&self, port: usize) -> Vec<f64> {
        self.sij(port, port)
            .map(|g| {
                let accepted = 1.0 - g.norm_sqr();
                if accepted > 0.0 { 0.0 - db10(accepted) } else if accepted.is_nan() { f64::NAN } else { f64::INFINITY }
            })
            .collect()
    }

    /// Every local minimum of `|Sii|` below −3 dB as `(f, dB)`. The complex `Sii`
    /// is fitted with a parabola through the three samples around the minimum, so
    /// a sharp, deep notch between samples keeps its true frequency and depth.
    pub fn resonances(&self, port: usize) -> Vec<(f64, f64)> {
        let g: Vec<C64> = self.sij(port, port).collect();
        let m: Vec<f64> = g.iter().map(|v| v.norm()).collect();
        let mut out = Vec::new();
        for k in 1..m.len().saturating_sub(1) {
            if m[k] < m[k - 1] && m[k] <= m[k + 1] {
                let (fk, mk) = refine_minimum([self.f[k - 1], self.f[k], self.f[k + 1]], [g[k - 1], g[k], g[k + 1]]);
                let db = db20(mk);
                if db < -3.0 {
                    out.push((fk, db));
                }
            }
        }
        out
    }

    /// Every contiguous band with `|Sii|` below `limit_db`, as `(f_lo, f_hi)`,
    /// with edges interpolated in dB. A band touching the sweep ends at that end.
    pub fn bandwidths(&self, port: usize, limit_db: f64) -> Vec<(f64, f64)> {
        let db = self.sij_db(port, port);
        let edge = |a: usize, b: usize| {
            let t = (limit_db - db[a]) / (db[b] - db[a]);
            self.f[a] + t * (self.f[b] - self.f[a])
        };
        let mut bands = Vec::new();
        let mut start = None;
        for (k, level) in db.iter().enumerate() {
            let below = *level < limit_db;
            match (below, start) {
                (true, None) => start = Some(if k == 0 { self.f[0] } else { edge(k - 1, k) }),
                (false, Some(lo)) => {
                    bands.push((lo, edge(k - 1, k)));
                    start = None;
                }
                _ => {}
            }
        }
        if let (Some(lo), Some(hi)) = (start, self.f.last()) {
            bands.push((lo, *hi));
        }
        bands
    }

    /// Yaghjian-Best: `Q = ω/(2R)·√(R'² + (X' + |X|/ω)²)`, derivatives in ω.
    pub fn q_factor(&self, port: usize) -> Vec<f64> {
        let z = self.zin(port);
        let w: Vec<f64> = self.f.iter().map(|f| 2.0 * PI * f).collect();
        let r: Vec<f64> = z.iter().map(|v| v.re).collect();
        let x: Vec<f64> = z.iter().map(|v| v.im).collect();
        let (dr, dx) = (derivative(&w, &r), derivative(&w, &x));
        (0..z.len())
            .map(|k| {
                let tuned = dx[k] + x[k].abs() / w[k].max(1e-30);
                w[k] / (2.0 * r[k].abs().max(1e-12)) * (dr[k] * dr[k] + tuned * tuned).sqrt()
            })
            .collect()
    }

    /// Group delay of `S[i][j]`, seconds: `−dφ/dω`.
    pub fn group_delay(&self, i: usize, j: usize) -> Vec<f64> {
        let w: Vec<f64> = self.f.iter().map(|f| 2.0 * PI * f).collect();
        let phase = unwrap_phase(self.sij(i, j));
        derivative(&w, &phase).iter().map(|d| -d).collect()
    }

    /// `Γp = Σⱼ Spj·wⱼ / wp`. An unexcited port gives 0.
    pub fn active_reflection(&self, port: usize, weights: &[C64]) -> Vec<C64> {
        let a = weights[port];
        self.s
            .iter()
            .map(|m| {
                if a.norm_sqr() == 0.0 {
                    return C64::new(0.0, 0.0);
                }
                m[port].iter().zip(weights).map(|(s, w)| s * w).sum::<C64>() / a
            })
            .collect()
    }

    /// Total active reflection coefficient: `‖S·a‖ / ‖a‖`.
    pub fn tarc(&self, weights: &[C64]) -> Vec<f64> {
        let incident: f64 = weights.iter().map(C64::norm_sqr).sum();
        self.s
            .iter()
            .map(|m| {
                let reflected: f64 = m.iter().map(|row| row.iter().zip(weights).map(|(s, w)| s * w).sum::<C64>().norm_sqr()).sum();
                (reflected / incident.max(1e-300)).sqrt()
            })
            .collect()
    }

    /// Envelope correlation from S alone (Blanch), valid for a lossless pair:
    /// `|Sii*Sij + Sji*Sjj|² / ((1 − |Sii|² − |Sji|²)(1 − |Sjj|² − |Sij|²))`.
    /// A pair radiating nothing reads 1.
    pub fn ecc_from_s(&self, i: usize, j: usize) -> Vec<f64> {
        self.s
            .iter()
            .map(|m| {
                let num = (m[i][i].conj() * m[i][j] + m[j][i].conj() * m[j][j]).norm_sqr();
                let a = 1.0 - m[i][i].norm_sqr() - m[j][i].norm_sqr();
                let b = 1.0 - m[j][j].norm_sqr() - m[i][j].norm_sqr();
                if a <= 1e-12 || b <= 1e-12 { 1.0 } else { (num / (a * b)).clamp(0.0, 1.0) }
            })
            .collect()
    }

    /// Low-pass TDR step response of a port, `n` points as
    /// `(round-trip time in seconds, ohms)`. Multiply the time by `v/2` for
    /// distance. Needs a sweep starting near DC with even spacing: `Sii` is resampled
    /// onto harmonics of the mean step, the DC value is extrapolated from the first
    /// two samples, and a Hann window sets a rise time of about `1/f_max`. The
    /// unambiguous window is `1/Δf`.
    pub fn tdr(&self, port: usize, n: usize) -> (Vec<f64>, Vec<f64>) {
        let nf = self.f.len();
        if nf < 2 || n == 0 {
            return (Vec::new(), Vec::new());
        }
        let z0 = self.z0[port];
        let g: Vec<C64> = self.sij(port, port).collect();
        let f_max = self.f[nf - 1];
        let step = (f_max - self.f[0]) / (nf - 1) as f64;
        let m = (f_max / step).round().max(1.0) as usize;
        let df = f_max / m as f64;
        let period = 1.0 / df;

        let dc = if self.f[0] <= 0.0 {
            g[0].re
        } else {
            (g[0] - (g[1] - g[0]) * (self.f[0] / (self.f[1] - self.f[0]))).re.clamp(-1.0, 1.0)
        };
        let mut samples = Vec::with_capacity(nf + 1);
        if self.f[0] > 0.0 {
            samples.push((0.0, C64::new(dc, 0.0)));
        }
        samples.extend(self.f.iter().copied().zip(g.iter().copied()));
        let spectrum: Vec<C64> = (1..=m)
            .map(|k| {
                let w = (PI * k as f64 / (2.0 * (m + 1) as f64)).cos().powi(2);
                interp_linear(&samples, k as f64 * df) * w
            })
            .collect();

        let lead = (4.0 / f_max).min(period / 4.0);
        let times: Vec<f64> = (0..n).map(|i| i as f64 * (period - lead) / n as f64).collect();
        let ohms = times
            .par_iter()
            .map(|t| {
                let mut rho = C64::new(dc * (t + lead), 0.0);
                for (idx, value) in spectrum.iter().enumerate() {
                    let omega = 2.0 * PI * (idx + 1) as f64 * df;
                    let swing = C64::from_polar(1.0, omega * t) - C64::from_polar(1.0, -omega * lead);
                    let term = value * swing / C64::new(0.0, omega);
                    rho += term * 2.0;
                }
                let rho = (rho.re / period).clamp(-1.0, 0.99999);
                z0 * (1.0 + rho) / (1.0 - rho)
            })
            .collect();
        (times, ohms)
    }

    /// Same device against other reference impedances.
    pub fn renormalized(&self, z0: Vec<f64>) -> Self {
        let z: Vec<Matrix> = (0..self.f.len()).map(|k| self.z_at(k)).collect();
        Self::from_z(self.f.clone(), &z, z0)
    }

    /// `.sNp` version 1: GHz, S, RI. Version 1 has one reference impedance, so ports
    /// with differing `z0` are renormalised to the first.
    pub fn to_touchstone(&self) -> String {
        let n = self.ports();
        let r = self.z0.first().copied().unwrap_or(50.0);
        let uniform = self.z0.iter().all(|z| (z - r).abs() <= 1e-9 * r);
        let renormalized;
        let net = if uniform {
            self
        } else {
            renormalized = self.renormalized(vec![r; n]);
            &renormalized
        };
        let mut out = format!("! Sense Sim, {n}-port S parameters\n# GHz S RI R {r}\n");
        let pair = |v: C64| format!(" {:.12e} {:.12e}", v.re, v.im);
        for (k, m) in net.s.iter().enumerate() {
            out.push_str(&format!("{:.12}", net.f[k] / 1e9));
            if n <= 2 {
                for j in 0..n {
                    for row in m.iter().take(n) {
                        out.push_str(&pair(row[j]));
                    }
                }
                out.push('\n');
            } else {
                for row in m {
                    for chunk in row.chunks(4) {
                        for v in chunk {
                            out.push_str(&pair(*v));
                        }
                        out.push('\n');
                    }
                }
            }
        }
        out
    }

    /// Reads Touchstone S data: MA, DB or RI; Hz to GHz; any port count; `!` comments;
    /// noise data is ignored. Version 1 takes its port count from the data layout. A
    /// file with keywords is version 2, including per-port references and Lower or Upper
    /// matrices; mixed-mode data isn't supported.
    pub fn from_touchstone(text: &str) -> Result<Self> {
        if text.lines().any(|l| l.split('!').next().unwrap_or("").trim_start().starts_with('[')) {
            return from_touchstone2(text);
        }
        let mut scale = 1e9;
        let mut format = "MA".to_string();
        let mut r = 50.0;
        let mut seen_options = false;
        let mut lines: Vec<Vec<f64>> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('!').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(options) = line.strip_prefix('#') {
                if !seen_options {
                    seen_options = true;
                    (scale, format, r) = parse_options(options, index + 1)?;
                }
                continue;
            }
            let values: Vec<f64> = line
                .split_whitespace()
                .map(|t| t.parse::<f64>().map_err(|_| anyhow!("line {}: '{t}' is not a number", index + 1)))
                .collect::<Result<_>>()?;
            lines.push(values);
        }
        if lines.is_empty() {
            bail!("no network data found");
        }
        if lines[0].len().is_multiple_of(2) {
            bail!("the first data line has {} values; a record starts with a frequency and then pairs", lines[0].len());
        }

        // A record's first line has an odd count (the frequency); continuations are even.
        let next_record = lines.iter().skip(1).position(|l| l.len() % 2 == 1).map_or(lines.len(), |p| p + 1);
        let record: usize = lines[..next_record].iter().map(Vec::len).sum();
        let n = (((record - 1) / 2) as f64).sqrt().round() as usize;
        if n == 0 || 1 + 2 * n * n != record {
            bail!("a record has {record} values, which is not 1 + 2·n² for any port count n");
        }

        let tokens: Vec<f64> = lines.into_iter().flatten().collect();
        let mut net = Network { f: Vec::new(), z0: vec![r; n], s: Vec::new() };
        let mut at = 0;
        while at < tokens.len() {
            let f = tokens[at] * scale;
            if net.f.last().is_some_and(|last| f <= *last) {
                if n == 2 {
                    break;
                }
                bail!("frequencies must increase: {f} Hz follows {} Hz", net.f[net.f.len() - 1]);
            }
            if at + record > tokens.len() {
                bail!("the data ends in the middle of a record at {f} Hz");
            }
            let mut m = vec![vec![C64::new(0.0, 0.0); n]; n];
            for a in 0..n {
                for b in 0..n {
                    let (i, j) = if n == 2 { (b, a) } else { (a, b) };
                    let offset = at + 1 + 2 * (a * n + b);
                    m[i][j] = decode_pair(&format, tokens[offset], tokens[offset + 1]);
                }
            }
            net.f.push(f);
            net.s.push(m);
            at += record;
        }
        Ok(net)
    }

    /// Fast sweep: Floater-Hormann rational interpolation of order 3 for every S
    /// entry. It has no real poles, so it cannot ring wildly the way a global
    /// polynomial does. Outside the sampled range the end value is held. Give the
    /// narrowest resonance about five samples across its half-power band: with fewer
    /// than three the dB curve can be more than 1 dB off.
    pub fn interpolate(&self, f_out: &[f64]) -> Self {
        let n = self.ports();
        if self.f.is_empty() {
            return Self { f: f_out.to_vec(), z0: self.z0.clone(), s: Vec::new() };
        }
        let (lo, hi) = (self.f[0], self.f[self.f.len() - 1]);
        let span = (hi - lo).max(1e-30);
        let x: Vec<f64> = self.f.iter().map(|f| (f - lo) / span).collect();
        let weights = floater_hormann_weights(&x, 3);
        let s = f_out
            .iter()
            .map(|f| {
                let xo = ((f.clamp(lo, hi)) - lo) / span;
                let terms = barycentric_terms(&x, &weights, xo);
                let mut m = vec![vec![C64::new(0.0, 0.0); n]; n];
                for (i, row) in m.iter_mut().enumerate() {
                    for (j, v) in row.iter_mut().enumerate() {
                        *v = terms.iter().map(|(k, t)| self.s[*k][i][j] * *t).sum();
                    }
                }
                m
            })
            .collect();
        Self { f: f_out.to_vec(), z0: self.z0.clone(), s }
    }
}

/// A frequency sweep solved at more than its even samples where it had to be.
#[derive(Clone, Debug)]
pub struct RefinedSweep {
    pub network: Network,
    pub z: Vec<Matrix>,
    /// Frequencies where the sampling was still in doubt when the budget ran out.
    pub unresolved: Vec<f64>,
}

/// How far a held-out sample may land from the interpolation: about 0.2 dB of its own
/// level, down to −54 dB, which keeps an S11 near −30 dB within about 0.5 dB.
fn held_out_agrees(predicted: C64, solved: C64) -> bool {
    (predicted - solved).norm() <= f64::max(0.002, 0.02 * solved.norm())
}

/// Solves `samples`, then checks the sweep where a resonance could hide: at each local
/// minimum of `|Sii|`, and wherever `Xii` crosses zero going up. Each check solves the
/// middle of an interval and compares it with the interpolation; where they disagree the
/// interval is split and checked again, up to `budget` extra solves. A narrow resonance
/// gets the samples it needs, and a broad one costs a few solves.
pub fn refined_sweep(samples: &[f64], z0: Vec<f64>, budget: usize, mut solve: impl FnMut(f64) -> Result<Vec<Vec<C64>>>) -> Result<RefinedSweep> {
    let mut f = samples.to_vec();
    let mut z = samples.iter().map(|x| solve(*x)).collect::<Result<Vec<_>>>()?;
    let mut settled: Vec<(f64, f64)> = Vec::new();
    let mut unresolved = Vec::new();
    let mut spent = 0;
    // A disagreement can reach past the interval it showed in, so its neighbours get checked too.
    let mut beside: Vec<(f64, f64)> = Vec::new();
    loop {
        let net = Network::from_z(f.clone(), &z, z0.clone());
        let mut candidates = doubtful_intervals(&net, &z);
        candidates.extend(std::mem::take(&mut beside).into_iter().filter(|t| f.windows(2).any(|w| w[0] == t.0 && w[1] == t.1)).map(|t| (t.0, t.1, false)));
        candidates.sort_by(|x, y| x.0.total_cmp(&y.0).then(y.2.cmp(&x.2)));
        candidates.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
        let todo: Vec<(f64, f64, bool)> = candidates.into_iter().filter(|t| !settled.iter().any(|s| s.0 <= t.0 && t.1 <= s.1)).collect();
        if todo.is_empty() {
            break;
        }
        if spent >= budget {
            unresolved = todo.iter().map(|t| 0.5 * (t.0 + t.1)).collect();
            break;
        }
        for (a, b, unseen) in todo {
            if spent >= budget {
                break;
            }
            let mid = 0.5 * (a + b);
            let at_mid = solve(mid)?;
            spent += 1;
            let predicted = net.interpolate(&[mid]);
            let solved = Network::from_z(vec![mid], std::slice::from_ref(&at_mid), z0.clone());
            let agrees = predicted.s[0].iter().zip(&solved.s[0]).flat_map(|(p, q)| p.iter().zip(q)).all(|(p, q)| held_out_agrees(*p, *q));
            let at = f.partition_point(|x| *x < mid);
            if agrees && !unseen {
                settled.push((a, b));
            } else if !agrees {
                if at >= 2 {
                    beside.push((f[at - 2], f[at - 1]));
                }
                if at + 1 < f.len() {
                    beside.push((f[at], f[at + 1]));
                }
            }
            f.insert(at, mid);
            z.insert(at, at_mid);
        }
    }
    Ok(RefinedSweep { network: Network::from_z(f, &z, z0.clone()), z, unresolved })
}

/// Intervals where a resonance could hide. The flag marks an upward zero crossing of X
/// whose reactance is still far above the resistance and `z0` on both sides: S looks the
/// same there whatever sits between them, so agreement proves nothing until it is closed in on.
fn doubtful_intervals(net: &Network, z: &[Matrix]) -> Vec<(f64, f64, bool)> {
    let f = &net.f;
    let n = f.len();
    let mut out: Vec<(f64, f64, bool)> = Vec::new();
    for p in 0..net.ports() {
        let g: Vec<f64> = net.s.iter().map(|m| m[p][p].norm()).collect();
        for k in 1..n.saturating_sub(1) {
            if g[k] < g[k - 1] && g[k] <= g[k + 1] {
                out.push((f[k - 1], f[k], false));
                out.push((f[k], f[k + 1], false));
            }
        }
        for k in 0..n.saturating_sub(1) {
            let (a, b) = (z[k][p][p], z[k + 1][p][p]);
            if a.im < 0.0 && b.im >= 0.0 {
                let scale = 3.0 * net.z0[p].max(a.re.abs()).max(b.re.abs());
                out.push((f[k], f[k + 1], a.im.abs() > scale && b.im.abs() > scale));
            }
        }
    }
    out.sort_by(|x, y| x.0.total_cmp(&y.0).then(y.2.cmp(&x.2)));
    out.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
    out
}

fn reflection_to_z(g: C64, z0: f64) -> C64 {
    let d = C64::new(1.0, 0.0) - g;
    let d = if d.norm() < SINGULAR_PIVOT { C64::new(SINGULAR_PIVOT, 0.0) } else { d };
    (C64::new(1.0, 0.0) + g) / d * z0
}

fn mat_mul(a: &Matrix, b: &Matrix) -> Matrix {
    let n = a.len();
    let mut out = vec![vec![C64::new(0.0, 0.0); n]; n];
    for i in 0..n {
        for k in 0..n {
            let aik = a[i][k];
            for j in 0..n {
                out[i][j] += aik * b[k][j];
            }
        }
    }
    out
}

/// Gauss-Jordan with partial pivoting.
fn inverse(m: &Matrix) -> Matrix {
    let n = m.len();
    let mut a = m.clone();
    let mut inv: Matrix = (0..n).map(|i| (0..n).map(|j| C64::new(if i == j { 1.0 } else { 0.0 }, 0.0)).collect()).collect();
    for col in 0..n {
        let pivot = (col..n).max_by(|x, y| a[*x][col].norm().total_cmp(&a[*y][col].norm())).unwrap_or(col);
        a.swap(col, pivot);
        inv.swap(col, pivot);
        if a[col][col].norm() < SINGULAR_PIVOT {
            a[col][col] = C64::new(SINGULAR_PIVOT, 0.0);
        }
        let d = a[col][col];
        for j in 0..n {
            a[col][j] /= d;
            inv[col][j] /= d;
        }
        for row in 0..n {
            if row != col {
                let factor = a[row][col];
                if factor.norm_sqr() != 0.0 {
                    for j in 0..n {
                        let (ac, ic) = (a[col][j], inv[col][j]);
                        a[row][j] -= factor * ac;
                        inv[row][j] -= factor * ic;
                    }
                }
            }
        }
    }
    inv
}

fn unwrap_phase(values: impl Iterator<Item = C64>) -> Vec<f64> {
    let mut out = Vec::new();
    let mut previous = 0.0;
    let mut offset = 0.0;
    for (k, v) in values.enumerate() {
        let raw = v.arg();
        if k > 0 {
            offset -= 2.0 * PI * ((raw - previous) / (2.0 * PI)).round();
        }
        previous = raw;
        out.push(raw + offset);
    }
    out
}

/// Second-order finite differences on an uneven grid.
fn derivative(x: &[f64], y: &[f64]) -> Vec<f64> {
    let n = x.len();
    if n < 2 {
        return vec![0.0; n];
    }
    if n == 2 {
        let d = (y[1] - y[0]) / (x[1] - x[0]);
        return vec![d, d];
    }
    let parabola = |k: usize, at: usize| {
        let (a, b, c) = (x[k - 1], x[k], x[k + 1]);
        let t = x[at];
        y[k - 1] * (2.0 * t - b - c) / ((a - b) * (a - c))
            + y[k] * (2.0 * t - a - c) / ((b - a) * (b - c))
            + y[k + 1] * (2.0 * t - a - b) / ((c - a) * (c - b))
    };
    (0..n).map(|k| parabola(k.clamp(1, n - 2), k)).collect()
}

/// The frequency and magnitude at the minimum of `|P(f)|` for the complex
/// parabola `P` through three samples, inside their span.
fn refine_minimum(f: [f64; 3], g: [C64; 3]) -> (f64, f64) {
    let parabola = |x: f64| {
        let l0 = (x - f[1]) * (x - f[2]) / ((f[0] - f[1]) * (f[0] - f[2]));
        let l1 = (x - f[0]) * (x - f[2]) / ((f[1] - f[0]) * (f[1] - f[2]));
        let l2 = (x - f[0]) * (x - f[1]) / ((f[2] - f[0]) * (f[2] - f[1]));
        (g[0] * l0 + g[1] * l1 + g[2] * l2).norm()
    };
    let grid = 32;
    let width = (f[2] - f[0]) / grid as f64;
    let mut best = f[1];
    let mut least = g[1].norm();
    for i in 0..=grid {
        let x = f[0] + i as f64 * width;
        let v = parabola(x);
        if v < least {
            least = v;
            best = x;
        }
    }
    let ratio = (5f64.sqrt() - 1.0) / 2.0;
    let (mut a, mut b) = ((best - width).max(f[0]), (best + width).min(f[2]));
    for _ in 0..50 {
        let (c, d) = (b - ratio * (b - a), a + ratio * (b - a));
        if parabola(c) < parabola(d) {
            b = d;
        } else {
            a = c;
        }
    }
    let x = 0.5 * (a + b);
    let v = parabola(x);
    if v < least { (x, v) } else { (best, least) }
}

fn interp_linear(samples: &[(f64, C64)], x: f64) -> C64 {
    let last = samples.len() - 1;
    if x <= samples[0].0 {
        return samples[0].1;
    }
    if x >= samples[last].0 {
        return samples[last].1;
    }
    let hi = samples.partition_point(|s| s.0 < x).clamp(1, last);
    let (a, b) = (samples[hi - 1], samples[hi]);
    a.1 + (b.1 - a.1) * ((x - a.0) / (b.0 - a.0))
}

pub(crate) fn floater_hormann_weights(x: &[f64], order: usize) -> Vec<f64> {
    let n = x.len() - 1;
    let d = order.min(n);
    (0..=n)
        .map(|k| {
            let mut sum = 0.0;
            for i in k.saturating_sub(d)..=k.min(n - d) {
                let mut term = 1.0;
                for j in i..=i + d {
                    if j != k {
                        term /= (x[k] - x[j]).abs();
                    }
                }
                sum += term;
            }
            if (k + d).is_multiple_of(2) { sum } else { -sum }
        })
        .collect()
}

/// `(sample index, coefficient)` pairs of the barycentric formula at `x`.
pub(crate) fn barycentric_terms(x: &[f64], weights: &[f64], at: f64) -> Vec<(usize, f64)> {
    if let Some(k) = x.iter().position(|v| (v - at).abs() < 1e-14) {
        return vec![(k, 1.0)];
    }
    let raw: Vec<f64> = x.iter().zip(weights).map(|(v, w)| w / (at - v)).collect();
    let total: f64 = raw.iter().sum();
    raw.iter().enumerate().map(|(k, r)| (k, r / total)).collect()
}

/// Touchstone 2.0. Keywords are case-insensitive, and data is counted in values, so it
/// can break across lines anywhere. A two-port file without [Two-Port Data Order] is
/// read in the standard's conventional 21_12 order.
fn from_touchstone2(text: &str) -> Result<Network> {
    #[derive(PartialEq)]
    enum Matrix {
        Full,
        Lower,
        Upper,
    }
    let (mut scale, mut format, mut r) = (1e9, "MA".to_string(), 50.0);
    let mut seen_options = false;
    let mut ports: Option<usize> = None;
    let mut frequencies: Option<usize> = None;
    let mut order_12_21 = false;
    let mut matrix = Matrix::Full;
    let mut reference: Vec<f64> = Vec::new();
    let (mut in_information, mut in_reference, mut in_data, mut has_data) = (false, false, false, false);
    let mut data: Vec<f64> = Vec::new();
    let numbers = |s: &str, line: usize| -> Result<Vec<f64>> { s.split_whitespace().map(|t| t.parse::<f64>().map_err(|_| anyhow!("line {line}: '{t}' is not a number"))).collect() };
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('!').next().unwrap_or("").trim();
        let at = index + 1;
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if in_information {
            in_information = !lower.starts_with("[end information]");
            continue;
        }
        if line.starts_with('[') {
            let close = line.find(']').ok_or_else(|| anyhow!("line {at}: a keyword has no closing bracket"))?;
            let key = lower[1..close].trim().to_string();
            let arg = line[close + 1..].trim();
            in_reference = false;
            match key.as_str() {
                k if k.starts_with("version") => {}
                "number of ports" => ports = Some(arg.parse().map_err(|_| anyhow!("line {at}: '{arg}' isn't a port count"))?),
                "number of frequencies" => frequencies = Some(arg.parse().map_err(|_| anyhow!("line {at}: '{arg}' isn't a count"))?),
                "number of noise frequencies" => {}
                "two-port data order" => match arg {
                    "12_21" => order_12_21 = true,
                    "21_12" => order_12_21 = false,
                    other => bail!("line {at}: unknown two-port data order '{other}'"),
                },
                "matrix format" => {
                    matrix = match arg.to_ascii_lowercase().as_str() {
                        "full" => Matrix::Full,
                        "lower" => Matrix::Lower,
                        "upper" => Matrix::Upper,
                        other => bail!("line {at}: unknown matrix format '{other}'"),
                    }
                }
                "reference" => {
                    in_reference = true;
                    reference.extend(numbers(arg, at)?);
                }
                "mixed-mode order" => bail!("line {at}: Touchstone 2 mixed-mode data isn't supported"),
                "begin information" => in_information = true,
                "network data" => {
                    in_data = true;
                    has_data = true;
                }
                // What follows isn't network data.
                "noise data" | "end" => break,
                other => bail!("line {at}: unknown Touchstone 2 keyword '[{other}]'"),
            }
            continue;
        }
        if let Some(options) = line.strip_prefix('#') {
            if !seen_options {
                seen_options = true;
                (scale, format, r) = parse_options(options, at)?;
            }
            continue;
        }
        if in_reference {
            reference.extend(numbers(line, at)?);
        } else if in_data {
            data.extend(numbers(line, at)?);
        } else {
            bail!("line {at}: data before the [Network Data] keyword of a Touchstone 2 file");
        }
    }
    if !has_data {
        bail!("this Touchstone 2 file has no [Network Data]");
    }
    let n = ports.filter(|n| *n > 0).ok_or_else(|| anyhow!("this Touchstone 2 file has no [Number of Ports]"))?;
    let count = frequencies.ok_or_else(|| anyhow!("this Touchstone 2 file has no [Number of Frequencies]"))?;
    let z0 = if reference.is_empty() {
        vec![r; n]
    } else if reference.len() == n {
        reference
    } else {
        bail!("[Reference] gives {} impedances for {n} ports", reference.len());
    };
    let pairs = if matrix == Matrix::Full { n * n } else { n * (n + 1) / 2 };
    let record = 1 + 2 * pairs;
    if data.len() != count * record {
        bail!("[Number of Frequencies] says {count} at {record} values each, but the file has {} values", data.len());
    }
    let mut net = Network { f: Vec::with_capacity(count), z0, s: Vec::with_capacity(count) };
    for values in data.chunks(record) {
        let f = values[0] * scale;
        if let Some(last) = net.f.last()
            && f <= *last
        {
            bail!("frequencies must increase: {f} Hz follows {last} Hz");
        }
        let mut m = vec![vec![C64::new(0.0, 0.0); n]; n];
        let mut next = values[1..].chunks(2).map(|p| decode_pair(&format, p[0], p[1]));
        for a in 0..n {
            for b in 0..n {
                match matrix {
                    Matrix::Full => {
                        // Two ports are N11 N21 N12 N22 unless the file says otherwise.
                        let (i, j) = if n == 2 && !order_12_21 { (b, a) } else { (a, b) };
                        m[i][j] = next.next().unwrap_or_default();
                    }
                    Matrix::Lower if b <= a => {
                        let v = next.next().unwrap_or_default();
                        m[a][b] = v;
                        m[b][a] = v;
                    }
                    Matrix::Upper if b >= a => {
                        let v = next.next().unwrap_or_default();
                        m[a][b] = v;
                        m[b][a] = v;
                    }
                    _ => {}
                }
            }
        }
        net.f.push(f);
        net.s.push(m);
    }
    Ok(net)
}

fn parse_options(options: &str, line: usize) -> Result<(f64, String, f64)> {
    let mut scale = 1e9;
    let mut format = "MA".to_string();
    let mut r = 50.0;
    let mut tokens = options.split_whitespace();
    while let Some(token) = tokens.next() {
        match token.to_ascii_uppercase().as_str() {
            "HZ" => scale = 1.0,
            "KHZ" => scale = 1e3,
            "MHZ" => scale = 1e6,
            "GHZ" => scale = 1e9,
            "S" => {}
            kind @ ("Y" | "Z" | "G" | "H") => bail!("line {line}: only S parameters are supported, this file holds {kind}"),
            fmt @ ("MA" | "DB" | "RI") => format = fmt.to_string(),
            "R" => {
                let value = tokens.next().ok_or_else(|| anyhow!("line {line}: R has no value"))?;
                r = value.parse().map_err(|_| anyhow!("line {line}: reference impedance '{value}' is not a number"))?;
            }
            other => bail!("line {line}: unknown option '{other}'"),
        }
    }
    Ok((scale, format, r))
}

fn decode_pair(format: &str, a: f64, b: f64) -> C64 {
    match format {
        "RI" => C64::new(a, b),
        "DB" => C64::from_polar(10f64.powf(a / 20.0), b.to_radians()),
        _ => C64::from_polar(a, b.to_radians()),
    }
}

/// Envelope correlation between two patterns on the same grid, in an isotropic
/// environment with equal power in both polarisations (XPR = 1):
/// `|∮(Eaθ·Ebθ* + Eaφ·Ebφ*)dΩ|² / (∮|Ea|²dΩ · ∮|Eb|²dΩ)`.
/// NaN when the grids differ.
pub fn ecc_from_far(a: &FarField, b: &FarField) -> f64 {
    let (nt, np) = (a.theta.len(), a.phi.len());
    if nt == 0 || np == 0 || b.theta.len() != nt || b.phi.len() != np {
        return f64::NAN;
    }
    let (dt, dp) = (a.theta_step().to_radians(), a.phi_step().to_radians());
    let mut cross = C64::new(0.0, 0.0);
    let (mut pa, mut pb) = (0.0, 0.0);
    for ti in 0..nt {
        let w = if ti == 0 || ti == nt - 1 { (1.0 - (dt * 0.5).cos()) * dp } else { a.theta[ti].to_radians().sin() * dt * dp };
        for pi in 0..np.saturating_sub(1) {
            let (at, ap) = a.at(ti, pi);
            let (bt, bp) = b.at(ti, pi);
            cross += (at * bt.conj() + ap * bp.conj()) * w;
            pa += (at.norm_sqr() + ap.norm_sqr()) * w;
            pb += (bt.norm_sqr() + bp.norm_sqr()) * w;
        }
    }
    if pa <= 0.0 || pb <= 0.0 {
        return 0.0;
    }
    (cross.norm_sqr() / (pa * pb)).clamp(0.0, 1.0)
}

/// Apparent diversity gain at 1 % outage, `10·√(1 − ecc²)` dB, with `ecc` the
/// envelope correlation as returned by [`ecc_from_far`]. This is the usual MIMO
/// datasheet form, not an effective gain against a reference antenna.
pub fn diversity_gain_db(ecc: f64) -> f64 {
    10.0 * (1.0 - ecc * ecc).max(0.0).sqrt()
}

/// Free-space link budget in dBm, `f` in Hz, `d` in metres.
pub fn friis_received_dbm(tx_dbm: f64, gt_dbi: f64, gr_dbi: f64, f: f64, d: f64) -> f64 {
    tx_dbm + gt_dbi + gr_dbi - db20(4.0 * PI * d * f / C0)
}

/// Free-space distance at which the received power falls to `sens_dbm`.
pub fn friis_range_m(tx_dbm: f64, gt_dbi: f64, gr_dbi: f64, f: f64, sens_dbm: f64) -> f64 {
    C0 / (4.0 * PI * f) * 10f64.powf((tx_dbm + gt_dbi + gr_dbi - sens_dbm) / 20.0)
}

/// Lower bound on the Q of a lossless, linearly polarised antenna inside a
/// sphere of radius `a`, in the exact Chu-McLean form: `1/(ka)³ + 1/(ka)`.
pub fn chu_q_limit(ka: f64) -> f64 {
    1.0 / ka.powi(3) + 1.0 / ka
}

/// Largest fractional bandwidth (Δf/f₀) a lossless single-tuned antenna inside a
/// sphere of radius `a` can have at a VSWR limit: `(s − 1)/(√s·Q)`, with `Q` from
/// [`chu_q_limit`]. Well below ka = 1 it follows Wheeler's `(ka)³` scaling.
pub fn wheeler_bandwidth_limit(ka: f64, vswr: f64) -> f64 {
    (vswr - 1.0) / (vswr.sqrt() * chu_q_limit(ka))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linspace;

    const F0: f64 = 1e9;

    fn c(re: f64, im: f64) -> C64 {
        C64::new(re, im)
    }

    /// A series R-L-C with resonance `f0` and unloaded `q`.
    fn series_rlc(r: f64, f0: f64, q: f64, f: &[f64]) -> Network {
        let w0 = 2.0 * PI * f0;
        let l = q * r / w0;
        let cap = 1.0 / (w0 * w0 * l);
        let z: Vec<Matrix> = f
            .iter()
            .map(|f| {
                let w = 2.0 * PI * f;
                vec![vec![c(r, w * l - 1.0 / (w * cap))]]
            })
            .collect();
        Network::from_z(f.to_vec(), &z, vec![50.0])
    }

    fn delay_line(tau: f64, f: &[f64]) -> Network {
        let s = f
            .iter()
            .map(|f| {
                let t = C64::from_polar(1.0, -2.0 * PI * f * tau);
                vec![vec![c(0.0, 0.0), t], vec![t, c(0.0, 0.0)]]
            })
            .collect();
        Network { f: f.to_vec(), z0: vec![50.0, 50.0], s }
    }

    fn assert_matrix_close(a: &Matrix, b: &Matrix, tol: f64) {
        for (ra, rb) in a.iter().zip(b) {
            for (x, y) in ra.iter().zip(rb) {
                assert!((x - y).norm() <= tol * (1.0 + y.norm()), "{x} vs {y}");
            }
        }
    }

    #[test]
    fn from_z_round_trip_with_unequal_references() {
        let z = vec![vec![
            vec![c(40.0, 12.0), c(5.0, -3.0), c(1.0, 2.0)],
            vec![c(5.0, -3.0), c(75.0, -20.0), c(-4.0, 6.0)],
            vec![c(1.0, 2.0), c(-4.0, 6.0), c(120.0, 33.0)],
        ]];
        let net = Network::from_z(vec![F0], &z, vec![50.0, 75.0, 25.0]);
        assert_eq!(net.ports(), 3);
        assert_matrix_close(&net.z_at(0), &z[0], 1e-10);
        let identity = mat_mul(&net.z_at(0), &net.y_at(0));
        for (i, row) in identity.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((v - c(want, 0.0)).norm() < 1e-10, "{v}");
            }
        }
        // Reciprocal Z stays reciprocal in power-normalised S.
        assert!((net.s[0][0][1] - net.s[0][1][0]).norm() < 1e-12);
    }

    #[test]
    fn known_two_port_values() {
        // A series 50 Ω between 50 Ω ports: S11 = 1/3, S21 = 2/3.
        let big = 1e9;
        let z = vec![vec![vec![c(big + 50.0, 0.0), c(big, 0.0)], vec![c(big, 0.0), c(big, 0.0)]]];
        let net = Network::from_z(vec![F0], &z, vec![50.0, 50.0]);
        assert!((net.s[0][0][0] - c(1.0 / 3.0, 0.0)).norm() < 1e-5, "{}", net.s[0][0][0]);
        assert!((net.s[0][1][0] - c(2.0 / 3.0, 0.0)).norm() < 1e-5, "{}", net.s[0][1][0]);
    }

    #[test]
    fn open_circuit_stays_finite() {
        let net = Network { f: vec![F0], z0: vec![50.0], s: vec![vec![vec![c(1.0, 0.0)]]] };
        assert!(net.z_at(0)[0][0].re.is_finite());
        assert!(net.zin(0)[0].re > 1e6);
        assert_eq!(net.vswr(0)[0], 1e6);
        assert!(net.y_at(0)[0][0].norm() < 1e-12);
    }

    #[test]
    fn a_reflection_above_one_is_not_a_big_mismatch_loss() {
        // |S|² = 1.01 used to read 20 dB through db10's abs.
        let net = Network { f: vec![F0], z0: vec![50.0], s: vec![vec![vec![c(1.01f64.sqrt(), 0.0)]]] };
        let v = net.mismatch_loss_db(0)[0];
        assert!(v.is_infinite() && v > 0.0, "{v}");
    }

    #[test]
    fn scalar_figures() {
        let net = Network { f: vec![F0], z0: vec![50.0], s: vec![vec![vec![c(0.5, 0.0)]]] };
        assert!((net.vswr(0)[0] - 3.0).abs() < 1e-12);
        assert!((net.mismatch_loss_db(0)[0] - 1.2494).abs() < 1e-3);
        assert!((net.sij_db(0, 0)[0] + 6.0206).abs() < 1e-3);
        assert!((net.zin(0)[0] - c(150.0, 0.0)).norm() < 1e-9);
    }

    #[test]
    fn two_port_touchstone_round_trip() {
        let s = vec![
            vec![vec![c(0.1, 0.2), c(0.3, -0.4)], vec![c(-0.5, 0.6), c(0.7, 0.05)]],
            vec![vec![c(-0.15, 0.25), c(0.35, 0.45)], vec![c(0.55, -0.65), c(-0.75, 0.01)]],
        ];
        let net = Network { f: vec![1e9, 1.5e9], z0: vec![50.0, 50.0], s };
        let text = net.to_touchstone();
        assert!(text.contains("# GHz S RI R 50"));
        // File order is S11 S21 S12 S22.
        let first = text.lines().nth(2).unwrap();
        let columns: Vec<f64> = first.split_whitespace().map(|t| t.parse().unwrap()).collect();
        assert_eq!(columns.len(), 9);
        assert!((columns[3] + 0.5).abs() < 1e-9, "S21 re comes third: {first}");
        assert!((columns[5] - 0.3).abs() < 1e-9, "S12 re comes fifth: {first}");
        let back = Network::from_touchstone(&text).unwrap();
        assert_eq!(back.ports(), 2);
        assert_eq!(back.f.len(), 2);
        for k in 0..2 {
            assert!((back.f[k] - net.f[k]).abs() < 1e-3);
            assert_matrix_close(&back.s[k], &net.s[k], 1e-10);
        }
    }

    #[test]
    fn n_port_touchstone_round_trip_and_layout() {
        for n in [1usize, 3, 4, 5, 9] {
            let m: Matrix = (0..n)
                .map(|i| (0..n).map(|j| c(0.01 * (i * n + j) as f64, -0.02 * (j + 1) as f64 + 0.003 * i as f64)).collect())
                .collect();
            let net = Network { f: vec![1e9, 2e9, 3e9], z0: vec![75.0; n], s: vec![m.clone(), m.clone(), m] };
            let text = net.to_touchstone();
            let back = Network::from_touchstone(&text).unwrap();
            assert_eq!(back.ports(), n, "{n}-port");
            assert_eq!(back.f.len(), 3);
            assert_eq!(back.z0, vec![75.0; n]);
            assert_matrix_close(&back.s[1], &net.s[1], 1e-10);
            let data: Vec<&str> = text.lines().filter(|l| !l.starts_with(['!', '#'])).collect();
            let per_record = if n <= 2 { 1 } else { n * n.div_ceil(4) };
            assert_eq!(data.len(), 3 * per_record, "{n}-port line count");
            assert!(data.iter().all(|l| l.split_whitespace().count() <= 9), "at most 4 pairs a line");
        }
    }

    #[test]
    fn unequal_references_are_renormalised_for_touchstone() {
        let z = vec![vec![vec![c(40.0, 12.0), c(5.0, -3.0)], vec![c(5.0, -3.0), c(75.0, -20.0)]]];
        let net = Network::from_z(vec![F0], &z, vec![50.0, 75.0]);
        let back = Network::from_touchstone(&net.to_touchstone()).unwrap();
        assert_eq!(back.z0, vec![50.0, 50.0]);
        assert_matrix_close(&back.z_at(0), &z[0], 1e-9);
    }

    #[test]
    fn parses_hand_written_files() {
        let ma = "! a comment\n! another\n# MHz S MA R 75\n\
                  100  0.5 -90  0.25 45  0.25 45  0.1 180 ! trailing\n\
                  200  0.4 -80  0.2  30  0.2  30  0.3 90\n";
        let net = Network::from_touchstone(ma).unwrap();
        assert_eq!(net.ports(), 2);
        assert_eq!(net.z0, vec![75.0, 75.0]);
        assert_eq!(net.f, vec![100e6, 200e6]);
        assert!((net.s[0][0][0] - c(0.0, -0.5)).norm() < 1e-12);
        assert!((net.s[0][1][1] - c(-0.1, 0.0)).norm() < 1e-12);
        assert!((net.s[1][0][1].norm() - 0.2).abs() < 1e-12);

        let db = "# Hz S DB\n1e9 -20 0\n2e9 -6.0206 90\n";
        let net = Network::from_touchstone(db).unwrap();
        assert_eq!(net.ports(), 1);
        assert_eq!(net.z0, vec![50.0]);
        assert!((net.s[0][0][0] - c(0.1, 0.0)).norm() < 1e-12);
        assert!((net.s[1][0][0] - c(0.0, 0.5)).norm() < 1e-4);

        let ri_khz = "# khz s ri r 100\n1000 0.1 0.2 0.3 0.4 0.5 0.6 0.7 0.8\n";
        let net = Network::from_touchstone(ri_khz).unwrap();
        assert_eq!(net.f, vec![1e6]);
        assert_eq!(net.s[0][1][0], c(0.3, 0.4), "S21 comes first after S11");
        assert_eq!(net.s[0][0][1], c(0.5, 0.6));

        let defaults = "1.0 0.5 90\n";
        let net = Network::from_touchstone(defaults).unwrap();
        assert_eq!(net.f, vec![1e9], "GHz MA R 50 by default");
        assert_eq!(net.z0, vec![50.0]);
        assert!((net.s[0][0][0] - c(0.0, 0.5)).norm() < 1e-12);

        let three = "# GHz S RI R 50\n\
                     1 1 2 3 4 5 6\n  7 8 9 10 11 12\n  13 14 15 16 17 18\n\
                     2 1 2 3 4 5 6\n  7 8 9 10 11 12\n  13 14 15 16 17 18\n";
        let net = Network::from_touchstone(three).unwrap();
        assert_eq!(net.ports(), 3);
        assert_eq!(net.s[1][1][2], c(11.0, 12.0));
        assert_eq!(net.s[1][2][0], c(13.0, 14.0));
    }

    #[test]
    fn two_port_noise_data_is_ignored() {
        let text = "# GHz S RI R 50\n1 .1 .2 .3 .4 .5 .6 .7 .8\n2 .1 .2 .3 .4 .5 .6 .7 .8\n! noise\n1 1.5 .5 90 .3\n2 1.6 .5 90 .3\n";
        let net = Network::from_touchstone(text).unwrap();
        assert_eq!(net.f.len(), 2);
    }

    /// The specification's Examples 5 and 6: the same 4-port data, Full and Lower.
    #[test]
    fn touchstone2_full_and_lower_read_the_same() {
        let head = "[Version] 2.0\n# GHz S MA R 50\n[Number of Ports] 4\n[Number of Frequencies] 1\n";
        let full = format!("{head}[Reference] 50 75 0.01 0.01\n[Matrix Format] Full\n[Network Data]\n5.00000 0.60 161.24 0.40 -42.20 0.42 -66.58 0.53 -79.34 !row 1\n0.40 -42.20 0.60 161.20 0.53 -79.34 0.42 -66.58 !row 2\n0.42 -66.58 0.53 -79.34 0.60 161.24 0.40 -42.20 !row 3\n0.53 -79.34 0.42 -66.58 0.40 -42.20 0.60 161.24 !row 4\n[End]\n");
        let lower = format!("{head}[Reference] 50 75\n0.01 0.01\n[Matrix Format] Lower\n[Network Data]\n5.00000 0.60 161.24 !row 1\n0.40 -42.20 0.60 161.20 !row 2\n0.42 -66.58 0.53 -79.34 0.60 161.24 !row 3\n0.53 -79.34 0.42 -66.58 0.40 -42.20 0.60 161.24 !row 4\n[End]\n");
        let (a, b) = (Network::from_touchstone(&full).unwrap(), Network::from_touchstone(&lower).unwrap());
        assert_eq!(a.z0, vec![50.0, 75.0, 0.01, 0.01]);
        assert_eq!(a.z0, b.z0);
        assert_eq!(a.f, vec![5e9]);
        for i in 0..4 {
            for j in 0..4 {
                assert!((a.s[0][i][j] - b.s[0][i][j]).norm() < 1e-12, "S{}{}", i + 1, j + 1);
            }
        }
        assert!((a.s[0][1][0].norm() - 0.40).abs() < 1e-12);
        assert!((a.s[0][3][0].norm() - 0.53).abs() < 1e-12);
    }

    #[test]
    fn touchstone2_upper_mirrors_the_other_way() {
        let t = "[Version] 2.0\n# GHz S RI\n[Number of Ports] 3\n[Number of Frequencies] 1\n[Matrix Format] Upper\n[Network Data]\n1 0.1 0 0.2 0 0.3 0 0.4 0 0.5 0 0.6 0\n[End]";
        let n = Network::from_touchstone(t).unwrap();
        let s = &n.s[0];
        for (i, j, v) in [(0, 0, 0.1), (0, 1, 0.2), (1, 0, 0.2), (0, 2, 0.3), (2, 0, 0.3), (1, 1, 0.4), (1, 2, 0.5), (2, 1, 0.5), (2, 2, 0.6)] {
            assert!((s[i][j].re - v).abs() < 1e-12, "S{}{}", i + 1, j + 1);
        }
    }

    #[test]
    fn touchstone2_two_port_order_is_honoured() {
        let t = |order: &str| format!("[Version] 2.0\n# GHz S RI R 50\n[Number of Ports] 2\n{order}[Number of Frequencies] 1\n[Network Data]\n1 0.1 0 0.2 0 0.3 0 0.4 0\n");
        let s21 = |text: String| Network::from_touchstone(&text).unwrap().s[0][1][0].re;
        assert_eq!(s21(t("[Two-Port Data Order] 21_12\n")), 0.2);
        assert_eq!(s21(t("[Two-Port Data Order] 12_21\n")), 0.3);
        assert_eq!(s21(t("")), 0.2, "the conventional order when the file doesn't say");
    }

    /// Lower two-port data is 11, 21, 22 only, as the specification says.
    #[test]
    fn touchstone2_lower_two_port_has_three_entries() {
        let t = "[Version] 2.0\n# GHz S RI\n[Number of Ports] 2\n[Number of Frequencies] 2\n[Matrix Format] Lower\n[Network Data]\n1 0.1 0 0.2 0 0.3 0\n2\n0.4 0 0.5 0\n0.6 0\n";
        let n = Network::from_touchstone(t).unwrap();
        assert_eq!(n.f, vec![1e9, 2e9]);
        assert_eq!((n.s[0][1][0].re, n.s[0][0][1].re, n.s[0][1][1].re), (0.2, 0.2, 0.3));
        assert_eq!(n.s[1][1][1].re, 0.6, "values, not lines, separate the frequencies");
    }

    #[test]
    fn touchstone2_skips_information_and_noise_and_ignores_case() {
        let t = "! a VNA export\n[VERSION] 2.0\n# MHz S DB R 75\n[number of ports] 1\n[Number Of Frequencies] 2\n[Number of Noise Frequencies] 1\n[Begin Information]\nthis isn't data 1 2 3\n[Two-Port Data Order] nonsense\n[End Information]\n[Network Data]\n100 -20 0 ! freq\n200 -10 0\n[Noise Data]\n100 1 0.5 0 0.3\n[End]\n";
        let n = Network::from_touchstone(t).unwrap();
        assert_eq!(n.f, vec![100e6, 200e6]);
        assert_eq!(n.z0, vec![75.0]);
        assert!((n.s[1][0][0].norm() - 10f64.powf(-0.5)).abs() < 1e-12);
    }

    #[test]
    fn touchstone2_round_trips_through_the_app() {
        let f = linspace(0.9e9, 1.1e9, 5);
        let a = series_rlc(50.0, 1e9, 10.0, &f);
        let mut text = String::from("[Version] 2.0\n# Hz S RI R 50\n[Number of Ports] 1\n[Number of Frequencies] 5\n[Network Data]\n");
        for (k, x) in f.iter().enumerate() {
            let v = a.s[k][0][0];
            text.push_str(&format!("{x} {:e} {:e}\n", v.re, v.im));
        }
        let b = Network::from_touchstone(&text).unwrap();
        assert!((b.sij_db(0, 0)[2] - a.sij_db(0, 0)[2]).abs() < 1e-9);
    }

    #[test]
    fn bad_touchstone2_is_rejected_with_a_reason() {
        let msg = |t: &str| Network::from_touchstone(t).unwrap_err().to_string();
        let head = "[Version] 2.0\n# GHz S RI\n[Number of Ports] 1\n[Number of Frequencies] 2\n";
        assert!(msg(&format!("{head}[Network Data]\n1 0 0\n")).contains("3 values"));
        assert!(msg(&format!("{head}[Network Data]\n2 0 0\n1 0 0\n")).contains("must increase"));
        assert!(msg(&format!("{head}[Reference] 50 75\n[Network Data]\n1 0 0\n2 0 0\n")).contains("2 impedances for 1 ports"));
        assert!(msg(&format!("{head}[Mixed-Mode Order] D2,3 C2,3\n[Network Data]\n")).contains("mixed-mode"));
        assert!(msg(&format!("{head}[Whatever] 1\n[Network Data]\n")).contains("unknown"));
        assert!(msg(&format!("{head}1 0 0\n[Network Data]\n")).contains("before the [Network Data]"));
        assert!(msg("[Version] 2.0\n# GHz Z RI\n[Number of Ports] 1\n[Number of Frequencies] 1\n[Network Data]\n1 0 0\n").contains("only S parameters"));
        assert!(msg("[Version] 2.0\n[Number of Frequencies] 1\n[Network Data]\n1 0 0\n").contains("Number of Ports"));
    }

    #[test]
    fn bad_touchstone_is_rejected_with_a_reason() {
        let msg = |t: &str| Network::from_touchstone(t).unwrap_err().to_string();
        assert!(msg("").contains("no network data"));
        assert!(msg("# GHz Z RI\n1 2 3\n").contains("only S parameters"));
        assert!(msg("1 2 x\n").contains("not a number"));
        assert!(msg("1 2 3 4 5\n").contains("1 + 2·n²"));
        assert!(msg("[Version] 2.0\n").contains("no [Network Data]"));
        assert!(msg("1 2 3\n0.5 2 3\n").contains("must increase"));
        assert!(msg("1 2 3 4 5 6 7\n 8 9 10 11 12 13\n 14 15 16 17 18 19\n2 3 4\n").contains("middle of a record"));
    }

    #[test]
    fn series_rlc_resonance_bandwidth_and_q() {
        let (r, q) = (50.0, 30.0);
        let f = linspace(0.7e9, 1.3e9, 601);
        let net = series_rlc(r, F0, q, &f);

        let res = net.resonances(0);
        assert_eq!(res.len(), 1);
        assert!((res[0].0 - F0).abs() < 1e-4 * F0, "{}", res[0].0);
        assert!(res[0].1 < -40.0, "a matched resonance is deep: {}", res[0].1);

        // Matched to R: the half-power band of |S11|² is 2·f0/Q wide.
        let bands = net.bandwidths(0, -3.0103);
        assert_eq!(bands.len(), 1);
        let width = bands[0].1 - bands[0].0;
        let q_from_band = 2.0 * F0 / width;
        assert!((q_from_band / q - 1.0).abs() < 0.05, "{q_from_band}");

        let qs = net.q_factor(0);
        let at = (F0 - f[0]) / (f[1] - f[0]);
        let q_ybest = qs[at.round() as usize];
        assert!((q_ybest / q - 1.0).abs() < 0.05, "{q_ybest}");
    }

    #[test]
    fn resonance_between_coarse_samples_is_refined() {
        let f0 = 1.0123e9;
        let f = linspace(0.5e9, 1.5e9, 41);
        let net = series_rlc(30.0, f0, 8.0, &f);
        let res = net.resonances(0);
        assert_eq!(res.len(), 1);
        assert!((res[0].0 - f0).abs() < 0.002 * f0, "{}", res[0].0);
        let want = db20(20.0 / 80.0);
        assert!((res[0].1 - want).abs() < 0.3, "{} vs {want}", res[0].1);
    }

    #[test]
    fn several_resonances_and_bands() {
        let f = linspace(0.5e9, 2.5e9, 2001);
        let z: Vec<Matrix> = f
            .iter()
            .map(|fk| {
                let branch = |f0: f64, q: f64, r: f64| {
                    let w0 = 2.0 * PI * f0;
                    let l = q * r / w0;
                    let cap = 1.0 / (w0 * w0 * l);
                    let w = 2.0 * PI * fk;
                    c(r, w * l - 1.0 / (w * cap))
                };
                let y = branch(1e9, 40.0, 50.0).inv() + branch(2e9, 40.0, 50.0).inv();
                vec![vec![y.inv()]]
            })
            .collect();
        let net = Network::from_z(f, &z, vec![50.0]);
        let res = net.resonances(0);
        assert_eq!(res.len(), 2, "{res:?}");
        assert!((res[0].0 / 1e9 - 1.0).abs() < 0.01);
        assert!((res[1].0 / 2e9 - 1.0).abs() < 0.01);
        let bands = net.bandwidths(0, -10.0);
        assert_eq!(bands.len(), 2, "{bands:?}");
        assert!(bands[0].0 < res[0].0 && res[0].0 < bands[0].1);
        assert!(bands[1].0 < res[1].0 && res[1].0 < bands[1].1);
    }

    #[test]
    fn band_touching_the_sweep_end_stops_there() {
        let f = linspace(0.95e9, 1.05e9, 101);
        let net = series_rlc(50.0, 0.95e9, 20.0, &f);
        let bands = net.bandwidths(0, -3.0);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].0, f[0]);
        let high = series_rlc(50.0, 1.05e9, 20.0, &f);
        assert_eq!(high.bandwidths(0, -3.0)[0].1, f[100]);
    }

    #[test]
    fn delay_line_group_delay_and_phase() {
        let tau = 1.25e-9;
        let f = linspace(1e9, 3e9, 201);
        let net = delay_line(tau, &f);
        for d in net.group_delay(1, 0) {
            assert!((d - tau).abs() < 1e-14, "{d}");
        }
        let phase = net.sij_phase_deg(1, 0);
        let want = -360.0 * (f[200] - f[0]) * tau;
        assert!((phase[200] - phase[0] - want).abs() < 1e-6, "{}", phase[200] - phase[0]);
    }

    #[test]
    fn active_reflection_and_tarc() {
        let s = vec![vec![vec![c(0.2, 0.0), c(0.1, 0.0)], vec![c(0.1, 0.0), c(0.2, 0.0)]]];
        let net = Network { f: vec![F0], z0: vec![50.0, 50.0], s };
        let even = [c(1.0, 0.0), c(1.0, 0.0)];
        assert!((net.active_reflection(0, &even)[0] - c(0.3, 0.0)).norm() < 1e-12);
        let odd = [c(1.0, 0.0), c(-1.0, 0.0)];
        assert!((net.active_reflection(0, &odd)[0] - c(0.1, 0.0)).norm() < 1e-12);
        assert!((net.tarc(&even)[0] - 0.3).abs() < 1e-12);
        assert!((net.tarc(&odd)[0] - 0.1).abs() < 1e-12);
        assert_eq!(net.active_reflection(1, &[c(1.0, 0.0), c(0.0, 0.0)])[0], c(0.0, 0.0));
    }

    #[test]
    fn ecc_from_s_matched_isolated_pair_is_zero() {
        let net = Network { f: vec![F0], z0: vec![50.0, 50.0], s: vec![vec![vec![c(0.0, 0.0); 2]; 2]] };
        assert_eq!(net.ecc_from_s(0, 1), vec![0.0]);
        // A hand-computed coupled lossless-ish pair.
        let s11 = c(0.2, 0.1);
        let s21 = c(0.3, -0.2);
        let m = vec![vec![s11, s21], vec![s21, s11]];
        let net = Network { f: vec![F0], z0: vec![50.0, 50.0], s: vec![m] };
        let num = (s11.conj() * s21 + s21.conj() * s11).norm_sqr();
        let den = (1.0 - s11.norm_sqr() - s21.norm_sqr()).powi(2);
        assert!((net.ecc_from_s(0, 1)[0] - num / den).abs() < 1e-12);
    }

    fn sampled_field(fill: impl Fn(f64, f64) -> (C64, C64)) -> FarField {
        let mut ff = FarField::grid(F0, 3.0, 3.0);
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                let (a, b) = fill(ff.theta[ti].to_radians(), ff.phi[pi].to_radians());
                ff.e_theta[ti][pi] = a;
                ff.e_phi[ti][pi] = b;
            }
        }
        ff
    }

    #[test]
    fn ecc_from_far_identical_and_orthogonal() {
        let a = sampled_field(|t, _| (c(t.sin(), 0.0), c(0.0, 0.0)));
        assert!((ecc_from_far(&a, &a) - 1.0).abs() < 1e-12);
        let scaled = sampled_field(|t, _| (c(0.0, 3.0 * t.sin()), c(0.0, 0.0)));
        assert!((ecc_from_far(&a, &scaled) - 1.0).abs() < 1e-12, "phase and scale do not matter");
        let cross_pol = sampled_field(|t, _| (c(0.0, 0.0), c(t.sin(), 0.0)));
        assert!(ecc_from_far(&a, &cross_pol) < 1e-20);
        // x- and y-directed short dipoles: orthogonal patterns, same polarisation basis.
        let x = sampled_field(|t, p| (c(t.cos() * p.cos(), 0.0), c(-p.sin(), 0.0)));
        let y = sampled_field(|t, p| (c(t.cos() * p.sin(), 0.0), c(p.cos(), 0.0)));
        assert!(ecc_from_far(&x, &y) < 1e-6, "{}", ecc_from_far(&x, &y));
        let other = FarField::grid(F0, 5.0, 5.0);
        assert!(ecc_from_far(&a, &other).is_nan());
    }

    #[test]
    fn diversity_gain() {
        assert!((diversity_gain_db(0.0) - 10.0).abs() < 1e-12);
        assert_eq!(diversity_gain_db(1.0), 0.0);
        assert!((diversity_gain_db(0.5) - 8.660254).abs() < 1e-5);
    }

    #[test]
    fn interpolation_tracks_a_smooth_resonance() {
        let coarse_f = linspace(0.5e9, 1.5e9, 41);
        let dense_f = linspace(0.5e9, 1.5e9, 1001);
        let coarse = series_rlc(30.0, F0, 10.0, &coarse_f);
        let dense = series_rlc(30.0, F0, 10.0, &dense_f);
        let fast = coarse.interpolate(&dense_f);
        let (a, b) = (fast.sij_db(0, 0), dense.sij_db(0, 0));
        let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
        println!("interpolate R=30 Q=10, 41 samples: worst {worst:.5} dB");
        assert!(worst < 0.3, "worst difference {worst} dB");
    }

    #[test]
    fn interpolation_tracks_a_matched_resonance_between_samples() {
        // Matched and off-grid, with five samples across the half-power band. Only the bottom
        // of the notch is excluded, where any tiny error is a large number of dB.
        let f0 = 1.0123e9;
        let coarse_f = linspace(0.5e9, 1.5e9, 76);
        let dense_f = linspace(0.5e9, 1.5e9, 2001);
        let fast = series_rlc(50.0, f0, 30.0, &coarse_f).interpolate(&dense_f);
        let dense = series_rlc(50.0, f0, 30.0, &dense_f);
        let (a, b) = (fast.sij_db(0, 0), dense.sij_db(0, 0));
        let worst = a.iter().zip(&b).filter(|(_, y)| **y > -30.0).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
        println!("interpolate R=50 Q=30, 76 samples: worst {worst:.4} dB above -30 dB");
        assert!(worst < 0.3, "worst difference {worst} dB");
        let (lo, hi) = (fast.resonances(0), dense.resonances(0));
        assert!((lo[0].0 - hi[0].0).abs() < 1e-3 * f0, "{lo:?} vs {hi:?}");
    }

    /// The sweep of a series R-L-C, solved only where the refinement asks.
    fn refined_rlc(q: f64, f0: f64, n: usize) -> (RefinedSweep, usize) {
        let mut solves = 0;
        let w0 = 2.0 * PI * f0;
        let (r, l) = (50.0, q * 50.0 / w0);
        let cap = 1.0 / (w0 * w0 * l);
        let sweep = refined_sweep(&linspace(0.7e9, 1.3e9, n), vec![50.0], n, |f| {
            solves += 1;
            let w = 2.0 * PI * f;
            Ok(vec![vec![c(r, w * l - 1.0 / (w * cap))]])
        })
        .unwrap();
        (sweep, solves)
    }

    /// Q = 200 at 41 samples used to read −6.6 dB with no −10 dB band at all.
    #[test]
    fn a_high_q_resonance_is_found_between_the_samples() {
        for q in [60.0, 100.0, 200.0, 400.0] {
            let f0 = 1.0123e9;
            let (sweep, solves) = refined_rlc(q, f0, 41);
            let fine = linspace(0.7e9, 1.3e9, 4001);
            let (a, b) = (sweep.network.interpolate(&fine), series_rlc(50.0, f0, q, &fine));
            let (da, db) = (a.sij_db(0, 0), b.sij_db(0, 0));
            let worst = da.iter().zip(&db).filter(|(_, y)| **y > -30.0).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
            let (ba, bb) = (a.bandwidths(0, -10.0), b.bandwidths(0, -10.0));
            println!("Q {q}: {solves} solves, worst error {worst:.2} dB above -30 dB, band {:.2} vs {:.2} MHz, unresolved {:?}", (ba[0].1 - ba[0].0) / 1e6, (bb[0].1 - bb[0].0) / 1e6, sweep.unresolved);
            assert!(solves <= 2 * 41, "Q = {q}: {solves}");
            assert!(((ba[0].1 - ba[0].0) / (bb[0].1 - bb[0].0) - 1.0).abs() < 0.03, "Q = {q}: {ba:?} vs {bb:?}");
            // Either it is right, or it says it ran out of solves. Never quietly wrong.
            assert!(worst < 0.6 || !sweep.unresolved.is_empty(), "Q = {q}: {worst} dB off and nothing said");
            if q <= 200.0 {
                assert!(worst < 0.6 && sweep.unresolved.is_empty(), "Q = {q}: {worst} dB off, {:?}", sweep.unresolved);
            }
        }
    }

    /// A smooth, low-Q antenna costs a few extra solves and nothing more.
    #[test]
    fn a_broad_resonance_costs_only_the_check() {
        let (sweep, solves) = refined_rlc(8.0, 1.0123e9, 41);
        assert!(solves <= 41 + 4, "{solves}");
        assert!(sweep.unresolved.is_empty());
    }

    /// When the budget runs out the sweep says where it could not be sure.
    #[test]
    fn an_unresolved_notch_is_reported() {
        let (sweep, _) = refined_rlc(100000.0, 1.0123e9, 9);
        assert!(!sweep.unresolved.is_empty(), "{:?}", sweep.network.f);
        assert!((sweep.unresolved[0] / 1.0123e9 - 1.0).abs() < 0.05, "{:?}", sweep.unresolved);
    }

    #[test]
    fn interpolation_hits_samples_and_does_not_ring() {
        let f = linspace(1e9, 2e9, 21);
        let step: Vec<Matrix> = (0..21).map(|k| vec![vec![c(if k < 10 { 0.0 } else { 1.0 }, 0.0)]]).collect();
        let net = Network { f: f.clone(), z0: vec![50.0], s: step };
        let at_samples = net.interpolate(&f);
        for k in 0..21 {
            assert!((at_samples.s[k][0][0] - net.s[k][0][0]).norm() < 1e-12);
        }
        let fine = linspace(1e9, 2e9, 2001);
        let up = net.interpolate(&fine);
        let (low, high) = up.s.iter().fold((0.0f64, 1.0f64), |(l, h), m| (l.min(m[0][0].re), h.max(m[0][0].re)));
        assert!(low > -0.25 && high < 1.25, "overshoot {low}..{high}");
        let outside = net.interpolate(&[0.5e9, 3e9]);
        assert_eq!(outside.s[0][0][0], c(0.0, 0.0));
        assert_eq!(outside.s[1][0][0], c(1.0, 0.0));
    }

    /// 1-port input reflection of a `zline` line of delay `tau` into `zload`.
    fn line_into(zline: f64, tau: f64, zload: f64, f: &[f64]) -> Network {
        let s = f
            .iter()
            .map(|f| {
                // cos/sin form: exact quarter-wave points would divide by zero with tan.
                let (s, co) = (2.0 * PI * f * tau).sin_cos();
                let z = (c(zload * co, zline * s)) / (c(zline * co, zload * s)) * zline;
                vec![vec![(z - 50.0) / (z + 50.0)]]
            })
            .collect();
        Network { f: f.to_vec(), z0: vec![50.0], s }
    }

    #[test]
    fn tdr_reads_a_line_impedance_and_its_load() {
        let tau = 1e-9;
        let f: Vec<f64> = (1..=2000).map(|k| k as f64 * 5e6).collect();
        let (t, z) = line_into(75.0, tau, 50.0, &f).tdr(0, 4000);
        assert_eq!(t.len(), 4000);
        let at = |x: f64| z[t.partition_point(|v| *v < x)];
        // Round trip 2 ns: 75 Ω on the line, then the first bounce at 50.8 Ω.
        assert!((at(1e-9) - 75.0).abs() < 1.5, "{}", at(1e-9));
        assert!((at(3e-9) - 50.8).abs() < 1.5, "{}", at(3e-9));
        assert!((at(100e-9) - 50.0).abs() < 1.5, "settles at the load: {}", at(100e-9));

        let (t, z) = line_into(50.0, tau, 0.0, &f).tdr(0, 4000);
        let at = |x: f64| z[t.partition_point(|v| *v < x)];
        assert!((at(1e-9) - 50.0).abs() < 1.5, "{}", at(1e-9));
        assert!(at(3e-9) < 2.0, "a short at the end: {}", at(3e-9));

        let (t, z) = line_into(50.0, tau, 200.0, &f).tdr(0, 500);
        let settled = z[t.partition_point(|v| *v < 3e-9)];
        assert!((settled - 200.0).abs() < 8.0, "{settled}");
    }

    #[test]
    fn friis_at_one_kilometre() {
        let received = friis_received_dbm(0.0, 0.0, 0.0, 1e9, 1000.0);
        assert!((received + 92.45).abs() < 0.01, "{received}");
        let d = friis_range_m(10.0, 3.0, 2.0, 2.4e9, -90.0);
        assert!((friis_received_dbm(10.0, 3.0, 2.0, 2.4e9, d) + 90.0).abs() < 1e-9);
    }

    #[test]
    fn small_antenna_limits() {
        assert!((chu_q_limit(1.0) - 2.0).abs() < 1e-12);
        assert!((chu_q_limit(0.5) - 10.0).abs() < 1e-12);
        let fbw = wheeler_bandwidth_limit(0.5, 2.0);
        assert!((fbw - 1.0 / (2f64.sqrt() * 10.0)).abs() < 1e-12);
        assert!(wheeler_bandwidth_limit(0.25, 2.0) < fbw);
    }
}
