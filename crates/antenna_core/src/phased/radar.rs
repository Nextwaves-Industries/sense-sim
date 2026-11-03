//! An FMCW MIMO radar, end to end: the chirps' beat signal for each transmitter and receiver,
//! then range and Doppler FFTs, cell-averaging CFAR, peak fitting, the time-division Doppler
//! correction, and angle from the virtual array. Sized by the radar equation.

use std::f64::consts::{PI, TAU};

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::aperture::unit_dir;
use super::doa::{self, DfArray, DfOptions, Method, Scan, Table};
use super::math::{ZERO, fft, fft_shift, hann, parabolic_peak};
use crate::geometry::{P3, dot};
use crate::{C0, C64, db10};

const BOLTZMANN: f64 = 1.380_649e-23;

/// What the radar is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fmcw {
    pub f0: f64,
    /// Sweep, Hz.
    pub bandwidth: f64,
    /// Sweep time of one chirp, s.
    pub chirp_s: f64,
    /// Time between chirps from one transmitter to the next, s.
    pub repeat_s: f64,
    /// Samples per chirp.
    pub samples: usize,
    /// Chirps per transmitter.
    pub loops: usize,
    /// Transmit and receive element positions, metres. With more than one transmitter they
    /// take turns, chirp by chirp.
    pub tx: Vec<P3>,
    pub rx: Vec<P3>,
    /// Each transmitter's output, dBm.
    pub tx_dbm: f64,
    pub tx_gain_dbi: f64,
    pub rx_gain_dbi: f64,
    pub nf_db: f64,
    /// CFAR false-alarm chance per cell.
    pub pfa: f64,
    pub seed: u64,
}

impl Default for Fmcw {
    /// A 77 GHz automotive sensor, 3 transmitters and 4 receivers.
    fn default() -> Self {
        let lambda = C0 / 77e9;
        let rx: Vec<P3> = (0..4).map(|i| [i as f64 * lambda / 2.0, 0.0, 0.0]).collect();
        let tx: Vec<P3> = (0..3).map(|i| [i as f64 * 2.0 * lambda, 0.0, 0.0]).collect();
        Self { f0: 77e9, bandwidth: 300e6, chirp_s: 40e-6, repeat_s: 50e-6, samples: 256, loops: 64, tx, rx, tx_dbm: 12.0, tx_gain_dbi: 10.0, rx_gain_dbi: 10.0, nf_db: 12.0, pfa: 1e-6, seed: 1 }
    }
}

/// A point target.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub range_m: f64,
    /// Towards positive range, m/s: a receding target is positive.
    pub speed_mps: f64,
    /// θ and φ in the array's frame: broadside is θ = 0, and +x is φ = 0.
    pub theta: f64,
    pub phi: f64,
    pub rcs_dbsm: f64,
}

impl Default for Target {
    fn default() -> Self {
        Self { range_m: 30.0, speed_mps: 5.0, theta: 15.0, phi: 0.0, rcs_dbsm: 10.0 }
    }
}

/// What the waveform allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub range_resolution_m: f64,
    pub max_range_m: f64,
    pub speed_resolution_mps: f64,
    pub max_speed_mps: f64,
    pub angle_resolution_deg: f64,
    pub virtual_elements: usize,
    pub sample_rate_hz: f64,
}

impl Fmcw {
    pub fn lambda(&self) -> f64 {
        C0 / self.f0
    }

    pub fn slope(&self) -> f64 {
        self.bandwidth / self.chirp_s
    }

    pub fn sample_rate(&self) -> f64 {
        self.samples as f64 / self.chirp_s
    }

    /// Every transmit and receive pair's phase centre, in the order the data cube has them:
    /// `tx·rx.len() + rx`. Each virtual element sits at `(r_tx + r_rx)` and sees the round-trip
    /// path, equal to one element of a real array of that shape.
    pub fn virtual_array(&self) -> Vec<P3> {
        let mut out = Vec::with_capacity(self.tx.len() * self.rx.len());
        for t in &self.tx {
            for r in &self.rx {
                out.push(std::array::from_fn(|k| t[k] + r[k]));
            }
        }
        out
    }

    pub fn limits(&self) -> Limits {
        let fs = self.sample_rate();
        let lambda = self.lambda();
        let pri = self.repeat_s * self.tx.len().max(1) as f64;
        let positions = self.virtual_array();
        let extent = positions.iter().map(|p| p[0]).fold(f64::MIN, f64::max) - positions.iter().map(|p| p[0]).fold(f64::MAX, f64::min);
        Limits {
            range_resolution_m: C0 / (2.0 * self.bandwidth),
            max_range_m: fs / 2.0 * C0 / (2.0 * self.slope()),
            speed_resolution_mps: lambda / (2.0 * self.loops as f64 * pri),
            max_speed_mps: lambda / (4.0 * pri),
            angle_resolution_deg: (lambda / (extent + lambda / 2.0)).min(1.0).asin().to_degrees(),
            virtual_elements: positions.len(),
            sample_rate_hz: fs,
        }
    }

    /// Radar equation: a target's received power per sample against the noise in a sample, dB.
    pub fn sample_snr_db(&self, t: &Target) -> f64 {
        let lambda = self.lambda();
        let pr_dbm = self.tx_dbm + self.tx_gain_dbi + self.rx_gain_dbi + 2.0 * db10(lambda) + t.rcs_dbsm - 3.0 * db10(4.0 * PI) - 4.0 * db10(t.range_m.max(1e-3));
        let noise_dbm = db10(BOLTZMANN * 290.0 * self.sample_rate()) + 30.0 + self.nf_db;
        pr_dbm - noise_dbm
    }

    /// The SNR after both FFTs and the Hann windows: samples times loops, less 1.76 dB per window.
    pub fn integrated_snr_db(&self, t: &Target) -> f64 {
        self.sample_snr_db(t) + db10((self.samples * self.loops) as f64) - 2.0 * 1.76
    }

    /// The range at which `t` integrates to `snr_db`, m.
    pub fn range_for_snr(&self, t: &Target, snr_db: f64) -> f64 {
        t.range_m * 10f64.powf((self.integrated_snr_db(t) - snr_db) / 40.0)
    }
}

/// `cube[virtual element][loop][sample]`, beat signal in a sample's volts.
pub type Cube = Vec<Vec<Vec<C64>>>;

/// Mixes each target down, and adds receiver noise. Beat phase is `2π·f₀·τ + 2π·S·τ·t`, with `τ`
/// the round trip at that moment and path.
pub fn simulate(c: &Fmcw, targets: &[Target]) -> Result<Cube> {
    if c.tx.is_empty() || c.rx.is_empty() {
        bail!("the radar needs a transmitter and a receiver");
    }
    if c.samples < 4 || c.loops < 2 {
        bail!("too few samples or chirps to transform");
    }
    let fs = c.sample_rate();
    let slope = c.slope();
    let lambda = c.lambda();
    let k = TAU / lambda;
    let noise_var = BOLTZMANN * 290.0 * fs * 10f64.powf(c.nf_db / 10.0);
    let n_virtual = c.tx.len() * c.rx.len();
    let paths = c.virtual_array();
    let amps: Vec<f64> = targets
        .iter()
        .map(|t| {
            let pr_dbm = c.tx_dbm + c.tx_gain_dbi + c.rx_gain_dbi + 2.0 * db10(lambda) + t.rcs_dbsm - 3.0 * db10(4.0 * PI) - 4.0 * db10(t.range_m.max(1e-3));
            10f64.powf((pr_dbm - 30.0) / 20.0)
        })
        .collect();
    let seeds: Vec<u64> = {
        let mut r = proof::Rng::new(c.seed);
        (0..n_virtual * c.loops).map(|_| r.next_u64()).collect()
    };
    let cube: Cube = (0..n_virtual)
        .into_par_iter()
        .map(|v| {
            let tx_index = v / c.rx.len();
            (0..c.loops)
                .map(|l| {
                    let mut r = proof::Rng::new(seeds[v * c.loops + l]);
                    let start = (l * c.tx.len() + tx_index) as f64 * c.repeat_s;
                    let mut chirp = vec![ZERO; c.samples];
                    for (i, s) in chirp.iter_mut().enumerate() {
                        let fast = i as f64 / fs;
                        let mut sum = ZERO;
                        for (t, a) in targets.iter().zip(&amps) {
                            let d = unit_dir(t.theta, t.phi);
                            let moved = t.range_m + t.speed_mps * (start + fast);
                            // The element's lead on the wave shortens the path.
                            let path = 2.0 * moved - dot(d, paths[v]);
                            let delay = path / C0;
                            sum += C64::from_polar(*a, k * path + TAU * slope * delay * fast);
                        }
                        *s = sum + C64::new(r.gauss(), r.gauss()) * (noise_var / 2.0).sqrt();
                    }
                    chirp
                })
                .collect()
        })
        .collect();
    Ok(cube)
}

/// One target found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub range_m: f64,
    pub speed_mps: f64,
    /// θ and φ, on the plane through x when the virtual array is a line.
    pub theta: f64,
    pub phi: f64,
    pub snr_db: f64,
    /// False with one virtual element, which can't tell an angle.
    pub has_angle: bool,
    /// How many times its speed was unfolded past the Doppler limit, from the transmitters' phases.
    pub folds: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadarOutput {
    /// `[speed bin][range bin]`, dB: all the virtual channels added in power.
    pub map: Vec<Vec<f32>>,
    /// Axis values, m and m/s.
    pub range_axis: Vec<f64>,
    pub speed_axis: Vec<f64>,
    pub detections: Vec<Detection>,
    pub limits: Limits,
    /// How many cells passed CFAR before clustering.
    pub cfar_hits: usize,
    /// How many cells were tested.
    pub cfar_cells: usize,
}

/// The chance that noise alone passes a cell-averaging threshold, for a cell and `n` training
/// cells that each add `looks` channels in power, with `a` = threshold factor over `n`:
/// `(1 + a)^(−nL) · Σ_{k<L} C(nL + k − 1, k)·(a/(1 + a))^k`.
fn cfar_tail(a: f64, n: usize, looks: usize) -> f64 {
    let nl = (n * looks) as f64;
    let x = a / (1.0 + a);
    let (mut term, mut sum) = (1.0, 1.0);
    for k in 0..looks.saturating_sub(1) {
        term *= (nl + k as f64) / (k as f64 + 1.0) * x;
        sum += term;
    }
    (-nl * (1.0 + a).ln()).exp() * sum
}

/// Cell-averaging CFAR threshold factor: a cell passes above this times the mean of its `n`
/// training cells, when each adds `looks` channels in power. One look is the textbook
/// `n·(Pfa^(−1/n) − 1)`; more looks need far less, since the sum's noise is steadier.
pub fn cfar_factor(n: usize, looks: usize, pfa: f64) -> f64 {
    let (n, looks) = (n.max(1), looks.max(1));
    let pfa = pfa.clamp(1e-30, 0.5);
    let (mut lo, mut hi) = (1e-9f64, 1e6f64);
    for _ in 0..200 {
        let mid = (lo * hi).sqrt();
        if cfar_tail(mid, n, looks) > pfa {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    n as f64 * (lo * hi).sqrt()
}

/// A guard and a training width that fit an axis: reaching further would count a cell twice
/// on a wrapped axis, or leave nothing to test on a short one.
fn fit_window(len: usize, guard: usize, train: usize) -> (usize, usize) {
    let reach = len.saturating_sub(1) / 2;
    if guard + train <= reach {
        (guard, train)
    } else {
        let g = guard.min(reach / 3);
        (g, reach - g)
    }
}

/// Range FFT, Doppler FFT, CFAR, peak fit, then angle. Range bins beyond half of Nyquist of the
/// real beat don't apply: only positive frequencies hold a target, so half the bins are kept.
pub fn process(c: &Fmcw, cube: &Cube) -> Result<RadarOutput> {
    let (n_virtual, loops) = (cube.len(), cube.first().map_or(0, Vec::len));
    let samples = cube.first().and_then(|v| v.first()).map_or(0, Vec::len);
    if n_virtual == 0 || loops < 2 || samples < 4 {
        bail!("no data to process");
    }
    let limits = c.limits();
    let (w_fast, w_slow) = (hann(samples), hann(loops));
    let range_bins = samples / 2;
    // `rd[virtual][speed][range]`.
    let rd: Vec<Vec<Vec<C64>>> = cube
        .par_iter()
        .map(|chirps| {
            let ranged: Vec<Vec<C64>> = chirps
                .iter()
                .map(|chirp| {
                    let mut b: Vec<C64> = chirp.iter().zip(&w_fast).map(|(v, w)| *v * *w).collect();
                    fft(&mut b);
                    b.truncate(range_bins);
                    b
                })
                .collect();
            let mut by_speed = vec![vec![ZERO; range_bins]; loops];
            for r in 0..range_bins {
                let mut col: Vec<C64> = (0..loops).map(|l| ranged[l][r] * w_slow[l]).collect();
                fft(&mut col);
                let centered = fft_shift(&col);
                for (s, v) in centered.into_iter().enumerate() {
                    by_speed[s][r] = v;
                }
            }
            by_speed
        })
        .collect();
    // Power across all virtual channels.
    let power: Vec<Vec<f64>> = (0..loops).into_par_iter().map(|s| (0..range_bins).map(|r| rd.iter().map(|v| v[s][r].norm_sqr()).sum::<f64>()).collect()).collect();
    let bin_range = limits.max_range_m * 2.0 / samples as f64 * 1.0;
    let bin_speed = 2.0 * limits.max_speed_mps / loops as f64;
    let range_axis: Vec<f64> = (0..range_bins).map(|r| r as f64 * bin_range).collect();
    let speed_axis: Vec<f64> = (0..loops).map(|s| (s as f64 - (loops / 2) as f64) * bin_speed).collect();
    // 2-D CA-CFAR on a cross of training cells, with guards around the cell under test. Doppler
    // wraps; a cell near an end of the range axis trains on the side it has.
    let (guard_r, train_r) = fit_window(range_bins, 2, 8);
    let (guard_s, train_s) = fit_window(loops, 2, 8);
    let factors: Vec<f64> = (0..=2 * train_r + 2 * train_s).map(|n| if n == 0 { f64::INFINITY } else { cfar_factor(n, n_virtual, c.pfa) }).collect();
    let hits: Vec<(usize, usize, f64)> = (0..loops)
        .into_par_iter()
        .flat_map_iter(|s| {
            let (power, factors) = (&power, &factors);
            (0..range_bins).filter_map(move |r| {
                let (mut sum, mut count) = (0.0, 0usize);
                for t in 1..=train_r {
                    if r >= guard_r + t {
                        sum += power[s][r - guard_r - t];
                        count += 1;
                    }
                    if r + guard_r + t < range_bins {
                        sum += power[s][r + guard_r + t];
                        count += 1;
                    }
                }
                for t in 1..=train_s {
                    sum += power[(s + guard_s + t) % loops][r] + power[(s + 2 * loops - guard_s - t) % loops][r];
                    count += 2;
                }
                if count == 0 {
                    return None;
                }
                let floor = sum / count as f64;
                (power[s][r] > floor * factors[count]).then_some((s, r, power[s][r] / floor.max(1e-300)))
            })
        })
        .collect();
    let mut peaks: Vec<(usize, usize, f64)> = Vec::new();
    // Only local maxima: a strong target's skirt passes the threshold too.
    for (s, r, snr) in &hits {
        let v = power[*s][*r];
        let mut top = true;
        for ds in -2i64..=2 {
            for dr in -2i64..=2 {
                if ds == 0 && dr == 0 {
                    continue;
                }
                let (a, b) = ((*s as i64 + ds).rem_euclid(loops as i64) as usize, *r as i64 + dr);
                if b >= 0 && (b as usize) < range_bins && power[a][b as usize] > v {
                    top = false;
                }
            }
        }
        if top {
            peaks.push((*s, *r, *snr));
        }
    }
    peaks.sort_by(|a, b| power[b.0][b.1].total_cmp(&power[a.0][a.1]));
    // The window's own side lobes fall about 31 dB and more, so what's near a much stronger
    // target along one axis is its skirt, not another target.
    let mut kept: Vec<(usize, usize, f64)> = Vec::new();
    for peak in peaks {
        let skirt = kept.iter().any(|k| {
            let ds = (k.0 as i64 - peak.0 as i64).rem_euclid(loops as i64).min((peak.0 as i64 - k.0 as i64).rem_euclid(loops as i64));
            let dr = (k.1 as i64 - peak.1 as i64).abs();
            (ds <= 1 && dr <= 12 || dr <= 1 && ds <= 12) && power[k.0][k.1] > 200.0 * power[peak.0][peak.1]
        });
        if !skirt {
            kept.push(peak);
        }
        if kept.len() >= 32 {
            break;
        }
    }
    let peaks = kept;
    let virtual_pos = c.virtual_array();
    let tx_count = c.tx.len();
    let rx_count = c.rx.len();
    let f_line = virtual_line(&virtual_pos, c.lambda());
    let scan_step = 0.5;
    let responder = doa::Ideal::new(virtual_pos.clone(), c.f0);
    let table = Table::new(&responder, Scan::Plane { phi: 0.0 }, scan_step);
    let df_array = DfArray { pos: virtual_pos.clone(), f: c.f0, line: f_line, ring: false };
    let detections: Vec<Detection> = peaks
        .par_iter()
        .map(|(s, r, snr)| {
            let at = |ds: i64, dr: i64| -> f64 {
                let a = (*s as i64 + ds).rem_euclid(loops as i64) as usize;
                let b = (*r as i64 + dr).clamp(0, range_bins as i64 - 1) as usize;
                power[a][b].max(1e-300).ln()
            };
            let fine_r = parabolic_peak(at(0, -1), at(0, 0), at(0, 1));
            let fine_s = parabolic_peak(at(-1, 0), at(0, 0), at(1, 0));
            let range = (*r as f64 + fine_r) * bin_range;
            let speed = (*s as f64 - (loops / 2) as f64 + fine_s) * bin_speed;
            // Each transmitter fires later than the first, while the target moves: the phase of
            // that's in the channels and would read as angle unless taken out. The conjugate puts
            // what's left in the array's form, since the beat's phase has the path shortened by d·r.
            let corrected = |v_hyp: f64| -> Vec<C64> {
                (0..tx_count * rx_count)
                    .map(|v| {
                        let delay = (v / rx_count) as f64 * c.repeat_s;
                        (rd[v][*s][*r] * C64::from_polar(1.0, -4.0 * PI * v_hyp * delay / c.lambda())).conj()
                    })
                    .collect()
            };
            // Past the limit a speed folds, by a step only the transmitters' phases can see: a
            // true hypothesis lines all the channels up, and a false one leaves a staircase. It has
            // to win clearly, or noise could unfold a target that hasn't folded.
            let coherence = |x: &[C64]| table.a.iter().flatten().map(|a| a.iter().zip(x).map(|(p, q)| p.conj() * *q).sum::<C64>().norm_sqr()).fold(0.0, f64::max);
            let base = coherence(&corrected(speed));
            let fold_step = 2.0 * limits.max_speed_mps;
            let mut folds = 0i32;
            let mut best = base * 1.26;
            if tx_count > 1 && rx_count > 1 {
                // A fold is a phase step of 2π·m/N between transmitters, so m repeats every N:
                // each distinct step is tried once, as the smaller of the speeds that give it.
                let n = tx_count as i32;
                for step in 1..n {
                    let m = if (speed + step as f64 * fold_step).abs() <= (speed + (step - n) as f64 * fold_step).abs() { step } else { step - n };
                    let v = coherence(&corrected(speed + m as f64 * fold_step));
                    if v > best {
                        best = v;
                        folds = m;
                    }
                }
            }
            let speed = speed + folds as f64 * fold_step;
            let x = corrected(speed);
            let estimate = doa::estimate(Method::Beamscan, std::slice::from_ref(&x), &df_array, &table, &responder, &DfOptions { count: Some(1), ..Default::default() }).ok();
            let dir = estimate.as_ref().and_then(|e| e.dirs.first().copied());
            Detection { range_m: range, speed_mps: speed, theta: dir.map_or(0.0, |d| d.0), phi: dir.map_or(0.0, |d| d.1), snr_db: db10(*snr), has_angle: dir.is_some(), folds }
        })
        .collect();
    let map: Vec<Vec<f32>> = power.iter().map(|row| row.iter().map(|v| db10(*v) as f32).collect()).collect();
    Ok(RadarOutput { map, range_axis, speed_axis, detections, limits, cfar_hits: hits.len(), cfar_cells: loops * range_bins })
}

/// When the virtual array is a line along x at a common pitch, the fast methods can run on it.
fn virtual_line(pos: &[P3], lambda: f64) -> Option<super::aperture::LineInfo> {
    if pos.iter().any(|p| p[1].abs() > 1e-9 || p[2].abs() > 1e-9) {
        return None;
    }
    let spacing = lambda / 2.0;
    let first = pos.iter().map(|p| p[0]).fold(f64::MAX, f64::min);
    let index: Vec<i64> = pos.iter().map(|p| ((p[0] - first) / spacing).round() as i64).collect();
    pos.iter().zip(&index).all(|(p, i)| ((p[0] - first) - *i as f64 * spacing).abs() < 1e-6 * lambda).then_some(super::aperture::LineInfo { spacing, index })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(c: &Fmcw, targets: &[Target]) -> RadarOutput {
        process(c, &simulate(c, targets).unwrap()).unwrap()
    }

    #[test]
    fn the_waveform_limits_are_the_textbook_ones() {
        let c = Fmcw::default();
        let l = c.limits();
        assert!((l.range_resolution_m - 0.5).abs() < 0.01, "{l:?}");
        // v_max = λ/(4·N_tx·T).
        let want = c.lambda() / (4.0 * 3.0 * 50e-6);
        assert!((l.max_speed_mps - want).abs() < 1e-9, "{l:?}");
        assert_eq!(l.virtual_elements, 12);
        // R_max = fs·c/(4·S) for real sampling of the beat's Nyquist.
        assert!(l.max_range_m > 60.0 && l.max_range_m < 70.0, "{l:?}");
    }

    #[test]
    fn a_tx_spaced_at_rx_aperture_fills_a_uniform_virtual_line() {
        let c = Fmcw::default();
        let line = virtual_line(&c.virtual_array(), c.lambda()).unwrap();
        let mut index = line.index.clone();
        index.sort_unstable();
        index.dedup();
        assert_eq!(index.len(), 12, "{index:?}");
        assert!(index.windows(2).all(|w| w[1] - w[0] == 1), "{index:?}");
    }

    #[test]
    fn a_target_comes_out_where_it_is() {
        let c = Fmcw::default();
        let t = Target { range_m: 30.0, speed_mps: 5.0, theta: 15.0, phi: 0.0, rcs_dbsm: 10.0 };
        let out = run(&c, &[t]);
        let d = out.detections.first().expect("found");
        let l = out.limits;
        assert!((d.range_m - t.range_m).abs() < l.range_resolution_m / 2.0, "{d:?}");
        assert!((d.speed_mps - t.speed_mps).abs() < l.speed_resolution_mps / 2.0, "{d:?}");
        assert!((d.theta - t.theta).abs() < l.angle_resolution_deg / 2.0, "{d:?} res {}", l.angle_resolution_deg);
    }

    #[test]
    fn angles_on_both_sides_and_speeds_of_both_signs_come_out_right() {
        let c = Fmcw::default();
        for t in [Target { range_m: 20.0, speed_mps: -5.0, theta: 30.0, phi: 180.0, rcs_dbsm: 10.0 }, Target { range_m: 45.0, speed_mps: 6.0, theta: 40.0, phi: 0.0, rcs_dbsm: 20.0 }, Target { range_m: 12.0, speed_mps: 0.0, theta: 0.0, phi: 0.0, rcs_dbsm: 0.0 }] {
            let out = run(&c, &[t]);
            let d = out.detections.first().expect("found");
            assert!((d.range_m - t.range_m).abs() < 0.5, "{t:?} -> {d:?}");
            assert!((d.speed_mps - t.speed_mps).abs() < 0.5, "{t:?} -> {d:?}");
            let got = super::super::aperture::angle_between((d.theta, d.phi), (t.theta, t.phi));
            assert!(got < 2.5, "{t:?} -> {d:?}: {got}");
        }
    }

    #[test]
    fn the_tdm_correction_is_what_keeps_a_fast_targets_angle() {
        // Without it, a target at speed reads off in angle: this runs the real path and
        // checks against the direct formula for the phase it would leave.
        let c = Fmcw::default();
        let t = Target { range_m: 30.0, speed_mps: 6.0, theta: 20.0, phi: 0.0, rcs_dbsm: 20.0 };
        let out = run(&c, &[t]);
        let d = out.detections.first().unwrap();
        assert!((d.theta - t.theta).abs() < 1.5, "{d:?}");
        let phase = 4.0 * PI * t.speed_mps * c.repeat_s / c.lambda();
        assert!(phase.abs() > 0.5, "this speed does leave a large phase per transmitter: {phase}");
    }

    #[test]
    fn several_targets_separate_in_range_speed_or_angle() {
        let c = Fmcw::default();
        let targets = [Target { range_m: 15.0, speed_mps: 3.0, theta: 10.0, phi: 0.0, rcs_dbsm: 10.0 }, Target { range_m: 35.0, speed_mps: -6.0, theta: 25.0, phi: 180.0, rcs_dbsm: 15.0 }, Target { range_m: 50.0, speed_mps: 5.5, theta: 5.0, phi: 180.0, rcs_dbsm: 20.0 }];
        let out = run(&c, &targets);
        for t in &targets {
            let best = out.detections.iter().min_by(|a, b| (a.range_m - t.range_m).abs().total_cmp(&(b.range_m - t.range_m).abs())).unwrap();
            assert!((best.range_m - t.range_m).abs() < 0.6, "{t:?}: {:?}", out.detections);
            assert!((best.speed_mps - t.speed_mps).abs() < 0.6, "{t:?}: {:?}", out.detections);
        }
    }

    #[test]
    fn measured_snr_follows_the_radar_equation() {
        let c = Fmcw { samples: 256, loops: 64, ..Default::default() };
        let t = Target { range_m: 40.0, speed_mps: 3.0, theta: 0.0, phi: 0.0, rcs_dbsm: 0.0 };
        let out = run(&c, &[t]);
        let d = out.detections.first().expect("found");
        // Adding power over the channels lifts target and floor alike, so the ratio to the local
        // floor stays one channel's integrated SNR. Its noise is a one-sample draw: ±3 dB.
        let single = c.integrated_snr_db(&t);
        assert!((d.snr_db - single).abs() < 3.0, "{} vs {single}", d.snr_db);
    }

    #[test]
    fn noise_alone_alarms_at_about_the_design_rate() {
        // A rate high enough to count: neighbours are correlated by the window, so within a factor.
        for pfa in [1e-2, 1e-3] {
            let c = Fmcw { pfa, samples: 512, loops: 128, ..Default::default() };
            let out = run(&c, &[]);
            let rate = out.cfar_hits as f64 / out.cfar_cells as f64;
            assert!(rate > pfa / 4.0 && rate < pfa * 4.0, "Pfa {pfa}: read {rate} from {} of {}", out.cfar_hits, out.cfar_cells);
        }
    }

    #[test]
    fn a_target_with_a_modest_snr_per_channel_is_found() {
        // Twelve channels added in power need only a few dB each: a factor sized for one
        // channel would have asked for about 10 dB.
        let c = Fmcw::default();
        let base = Target { range_m: 30.0, speed_mps: 2.0, theta: 0.0, phi: 0.0, rcs_dbsm: 0.0 };
        let want = 6.0;
        let t = Target { rcs_dbsm: base.rcs_dbsm + want - c.integrated_snr_db(&base), ..base };
        assert!((c.integrated_snr_db(&t) - want).abs() < 1e-9);
        let out = run(&c, &[t]);
        assert!(out.detections.iter().any(|d| (d.range_m - t.range_m).abs() < 1.0 && (d.speed_mps - t.speed_mps).abs() < 1.0), "{:?}", out.detections);
    }

    #[test]
    fn a_short_waveform_still_tests_its_cells() {
        // The fewest chirps and samples the page allows: the cell under test must not train on itself.
        let c = Fmcw { samples: 16, loops: 4, ..Default::default() };
        let limit = c.limits().max_range_m;
        let t = Target { range_m: 0.6 * limit, speed_mps: 0.0, theta: 0.0, phi: 0.0, rcs_dbsm: 40.0 };
        let out = run(&c, &[t]);
        assert!(out.cfar_cells > 0);
        assert!(out.detections.iter().any(|d| (d.range_m - t.range_m).abs() < 1.0), "{:?} at {}", out.detections, t.range_m);
    }

    #[test]
    fn a_strong_target_doesnt_bring_its_skirt_along_as_targets() {
        let c = Fmcw::default();
        let t = Target { range_m: 30.0, speed_mps: 3.0, theta: 0.0, phi: 0.0, rcs_dbsm: 25.0 };
        let out = run(&c, &[t]);
        assert_eq!(out.detections.len(), 1, "{:?}", out.detections);
    }

    #[test]
    fn a_nearby_target_is_found_though_its_range_cell_is_near_the_end() {
        let c = Fmcw::default();
        let t = Target { range_m: 1.5, speed_mps: 1.0, theta: 0.0, phi: 0.0, rcs_dbsm: -10.0 };
        let out = run(&c, &[t]);
        assert!(out.detections.iter().any(|d| (d.range_m - 1.5).abs() < 0.6), "{:?}", out.detections);
    }

    #[test]
    fn one_virtual_element_says_it_has_no_angle() {
        let c = Fmcw { tx: vec![[0.0; 3]], rx: vec![[0.0; 3]], ..Default::default() };
        let out = run(&c, &[Target { rcs_dbsm: 20.0, ..Default::default() }]);
        let d = out.detections.first().unwrap();
        assert!(!d.has_angle, "{d:?}");
    }

    #[test]
    fn cfar_factor_matches_its_closed_form_and_falls_with_looks() {
        // One look: 16·(10^(4/16) − 1) = 12.45.
        let single = cfar_factor(16, 1, 1e-4);
        assert!((single - 16.0 * (10f64.powf(0.25) - 1.0)).abs() < 1e-6, "{single}");
        assert!(cfar_factor(32, 1, 1e-6) > cfar_factor(32, 1, 1e-3));
        // Twelve looks, 32 training cells, 1e-4: far less than the one-look figure.
        let many = cfar_factor(32, 12, 1e-4);
        assert!(many > 2.0 && many < 3.0, "{many}");
        // By sampling: the sum of 12 unit exponentials against the mean of 32 such sums.
        let mut r = proof::Rng::new(7);
        let mut sum_of = |n: usize| -> f64 { (0..n).map(|_| -r.uniform().max(1e-300).ln()).sum() };
        let trials = 200_000;
        let target = 1e-2;
        let factor = cfar_factor(32, 12, target);
        let mut alarms = 0;
        for _ in 0..trials {
            let cell = sum_of(12);
            let floor = sum_of(12 * 32) / 32.0;
            if cell > factor * floor {
                alarms += 1;
            }
        }
        let rate = alarms as f64 / trials as f64;
        assert!((rate / target - 1.0).abs() < 0.1, "{rate} vs {target}");
    }

    #[test]
    fn a_speed_past_the_limit_is_unfolded_by_the_transmitters_phases() {
        // Three transmitters see a fold as a phase step between them, which keeps the angle too.
        let c = Fmcw::default();
        let v_max = c.limits().max_speed_mps;
        for (speed, theta, phi) in [(v_max + 3.0, 20.0, 0.0), (-(v_max + 2.0), 15.0, 180.0)] {
            let t = Target { range_m: 30.0, speed_mps: speed, theta, phi, rcs_dbsm: 20.0 };
            let out = run(&c, &[t]);
            let d = out.detections.first().unwrap();
            assert!((d.speed_mps - speed).abs() < 1.0, "{speed} reads {}", d.speed_mps);
            assert!(d.folds != 0, "{d:?}");
            let got = super::super::aperture::angle_between((d.theta, d.phi), (theta, phi));
            assert!(got < 2.5, "{t:?} -> {d:?}: {got}");
        }
        // One transmitter has nothing to tell it by: it folds.
        let one = Fmcw { tx: vec![[0.0; 3]], ..Default::default() };
        let v1 = one.limits().max_speed_mps;
        let t = Target { range_m: 30.0, speed_mps: v1 + 3.0, theta: 0.0, phi: 0.0, rcs_dbsm: 20.0 };
        let d = run(&one, &[t]).detections[0];
        assert!((d.speed_mps - (-v1 + 3.0)).abs() < 1.0, "folds to {}, reads {}", -v1 + 3.0, d.speed_mps);
    }

    #[test]
    fn a_target_inside_the_limit_isnt_unfolded_by_noise() {
        let c = Fmcw::default();
        for (speed, theta) in [(0.0, 0.0), (3.0, 10.0), (-5.0, 30.0), (6.0, 0.0)] {
            let t = Target { range_m: 25.0, speed_mps: speed, theta, phi: 0.0, rcs_dbsm: 5.0 };
            let d = run(&c, &[t]).detections[0];
            assert_eq!(d.folds, 0, "{t:?} -> {d:?}");
        }
    }

    #[test]
    fn range_for_snr_inverts_the_equation() {
        let c = Fmcw::default();
        let t = Target::default();
        let r = c.range_for_snr(&t, 13.0);
        let at = Target { range_m: r, ..t };
        assert!((c.integrated_snr_db(&at) - 13.0).abs() < 1e-9);
    }
}
