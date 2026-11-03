//! The receive chain from the antenna to the digital beam, and the transmit amplifier: a cascade
//! budget by formula, and a sample-by-sample simulation with noise, compression, LO phase noise,
//! I/Q imbalance, the converter, a decimating filter and a digital beamformer.
//!
//! Samples are complex baseband, scaled so `|x|²` is power in watts. A 50 Ω load is taken
//! as read throughout, so dBm is dBm everywhere.

use std::f64::consts::{PI, TAU};

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::beam::covariance_of;
use super::math::{CMat, ZERO, bessel_i0, fft, fft_shift, hann, inner, mat_vec, norm_sqr};
use crate::linalg::invert;
use crate::{C64, db10};

const BOLTZMANN: f64 = 1.380_649e-23;
/// The reference temperature of a noise figure.
const T0: f64 = 290.0;

fn watts(dbm: f64) -> f64 {
    10f64.powf((dbm - 30.0) / 10.0)
}

fn dbm(w: f64) -> f64 {
    db10(w) + 30.0
}

/// An amplifier, filter or mixer: its gain, noise figure and input third-order intercept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub name: String,
    pub gain_db: f64,
    pub nf_db: f64,
    pub iip3_dbm: Option<f64>,
}

impl Stage {
    pub fn new(name: &str, gain_db: f64, nf_db: f64, iip3_dbm: Option<f64>) -> Self {
        Self { name: name.into(), gain_db, nf_db, iip3_dbm }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Adc {
    pub bits: u8,
    /// Complex sample rate, Hz.
    pub fs_hz: f64,
    /// A full-scale tone's power, dBm.
    pub full_scale_dbm: f64,
    /// Clock jitter, rms seconds.
    pub jitter_s: f64,
    /// Frequency at the converter's input, which jitter scales with.
    pub f_in_hz: f64,
}

/// The chain up to and including the converter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RxChain {
    pub stages: Vec<Stage>,
    pub adc: Adc,
    /// Bandwidth the signal is judged in, Hz.
    pub bw_hz: f64,
    /// Antenna noise temperature, K.
    pub t_ant_k: f64,
}

impl Default for RxChain {
    /// A plain 2.4 GHz direct-conversion receiver.
    fn default() -> Self {
        Self {
            stages: vec![Stage::new("Filter", -1.5, 1.5, None), Stage::new("LNA", 18.0, 1.2, Some(-3.0)), Stage::new("Mixer", -6.0, 8.0, Some(10.0)), Stage::new("Baseband amp", 30.0, 6.0, Some(15.0))],
            adc: Adc { bits: 12, fs_hz: 80e6, full_scale_dbm: 4.0, jitter_s: 1e-12, f_in_hz: 10e6 },
            bw_hz: 20e6,
            t_ant_k: T0,
        }
    }
}

/// One line of a cascade: everything up to and including that stage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StageRow {
    pub name: String,
    pub gain_db: f64,
    pub nf_db: f64,
    pub iip3_dbm: Option<f64>,
    /// Noise at this stage's output in the signal bandwidth, dBm.
    pub noise_dbm: f64,
}

/// A cascade worked out by formula.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub rows: Vec<StageRow>,
    /// The analog chain's noise figure, dB (Friis).
    pub nf_analog_db: f64,
    /// With the converter's noise, dB.
    pub nf_system_db: f64,
    pub gain_db: f64,
    pub iip3_dbm: Option<f64>,
    /// `−10·log(1/10^(IIP3/10))`: 1 dB compression at the input, about 9.6 dB below IIP3.
    pub p1db_input_dbm: Option<f64>,
    /// Input-referred noise in the signal bandwidth, dBm.
    pub noise_floor_dbm: f64,
    /// The tone that fills the converter, at the antenna, dBm.
    pub clip_input_dbm: f64,
    /// Clip over noise floor, dB.
    pub dynamic_range_db: f64,
    /// Two-tone: ⅔·(IIP3 − floor), dB.
    pub sfdr_db: Option<f64>,
    /// `6.02·N + 1.76`, dB, and what filtering to the signal bandwidth adds.
    pub sqnr_db: f64,
    pub processing_gain_db: f64,
    /// Jitter's ceiling on any signal's SNR: `−20·log(2π·f·σ)`, dB.
    pub jitter_snr_db: f64,
    /// Thermal over quantisation noise at the converter, dB. Below about 6 the converter is
    /// setting the noise, and more gain would help.
    pub thermal_over_quant_db: f64,
}

/// The cascade by Friis' noise formula and the cascaded intercept formula.
pub fn budget(c: &RxChain) -> Budget {
    let mut rows = Vec::with_capacity(c.stages.len());
    let (mut gain, mut factor, mut inv_ip3) = (1.0f64, 1.0f64, 0.0f64);
    let mut any_ip3 = false;
    let ant = BOLTZMANN * c.t_ant_k * c.bw_hz;
    for s in &c.stages {
        let (g, f) = (10f64.powf(s.gain_db / 10.0), 10f64.powf(s.nf_db / 10.0));
        factor += (f - 1.0) / gain;
        if let Some(ip) = s.iip3_dbm {
            inv_ip3 += gain / watts(ip);
            any_ip3 = true;
        }
        gain *= g;
        rows.push(StageRow { name: s.name.clone(), gain_db: db10(gain), nf_db: db10(factor), iip3_dbm: any_ip3.then(|| dbm(1.0 / inv_ip3)), noise_dbm: dbm((ant + BOLTZMANN * T0 * (factor - 1.0) * c.bw_hz) * gain) });
    }
    let p_fs = watts(c.adc.full_scale_dbm);
    let levels = 2f64.powi(c.adc.bits as i32);
    let quant = p_fs / (1.5 * levels * levels);
    let extra = quant / c.adc.fs_hz / (BOLTZMANN * T0 * gain.max(1e-30));
    let system = factor + extra;
    let floor = BOLTZMANN * (c.t_ant_k + T0 * (system - 1.0)) * c.bw_hz;
    let clip = c.adc.full_scale_dbm - db10(gain);
    let iip3 = any_ip3.then(|| dbm(1.0 / inv_ip3));
    let jitter = 2.0 * PI * c.adc.f_in_hz * c.adc.jitter_s;
    Budget {
        rows,
        nf_analog_db: db10(factor),
        nf_system_db: db10(system),
        gain_db: db10(gain),
        iip3_dbm: iip3,
        p1db_input_dbm: iip3.map(|v| v - 9.64),
        noise_floor_dbm: dbm(floor),
        clip_input_dbm: clip,
        dynamic_range_db: clip - dbm(floor),
        sfdr_db: iip3.map(|v| 2.0 / 3.0 * (v - dbm(floor))),
        sqnr_db: db10(1.5 * levels * levels),
        processing_gain_db: db10(c.adc.fs_hz / c.bw_hz),
        jitter_snr_db: if jitter > 0.0 { -2.0 * db10(jitter) } else { f64::INFINITY },
        thermal_over_quant_db: db10(BOLTZMANN * (c.t_ant_k + T0 * (factor - 1.0)) * c.adc.fs_hz * gain / quant),
    }
}

/// Amplitude compression and third-order distortion as one complex-envelope cubic, from the
/// input third-order intercept: `y = x·(1 − |x|²/P_IP3)`, which puts a two-tone product at
/// the textbook level and 1 dB compression 9.64 dB below the intercept. It holds its
/// peak beyond that rather than folding back.
pub fn compress(x: C64, p_ip3: f64) -> C64 {
    let p = x.norm_sqr();
    let knee = p_ip3 / 3.0;
    if p <= knee {
        x * (1.0 - p / p_ip3)
    } else {
        let top = knee.sqrt() * (2.0 / 3.0);
        x / p.sqrt() * top
    }
}

/// Image rejection ratio of a gain and phase imbalance, dB.
pub fn image_rejection_db(gain_db: f64, phase_deg: f64) -> f64 {
    let g = 10f64.powf(gain_db / 20.0);
    let c = phase_deg.to_radians().cos();
    db10((1.0 + 2.0 * g * c + g * g) / (1.0 - 2.0 * g * c + g * g).max(1e-30))
}

/// Applies gain and phase imbalance between I and Q: `y = α·x + β·x*`.
pub fn iq_imbalance(x: &mut [C64], gain_db: f64, phase_deg: f64) {
    let g = 10f64.powf(gain_db / 20.0);
    let ph = phase_deg.to_radians();
    let alpha = (C64::new(1.0, 0.0) + C64::from_polar(g, -ph)) * 0.5;
    let beta = (C64::new(1.0, 0.0) - C64::from_polar(g, ph)) * 0.5;
    for v in x {
        *v = alpha * *v + beta * v.conj();
    }
}

/// A windowed-sinc low-pass by Kaiser's method: `cut` and `transition` as fractions of the
/// sample rate. The length is the one the window formula asks for, made odd, with its delay a
/// multiple of `delay_multiple` so a decimated stream stays on the sample grid.
pub fn kaiser_lowpass(cut: f64, transition: f64, atten_db: f64, delay_multiple: usize) -> Vec<f64> {
    let a = atten_db.max(21.0);
    let beta = if a > 50.0 { 0.1102 * (a - 8.7) } else { 0.5842 * (a - 21.0).powf(0.4) + 0.07886 * (a - 21.0) };
    let want = ((a - 8.0) / (2.285 * TAU * transition.max(1e-4))).ceil() as usize;
    let multiple = delay_multiple.max(1);
    let half = (want / 2 + 1).div_ceil(multiple) * multiple;
    let n = 2 * half + 1;
    let top = bessel_i0(beta);
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let m = i as f64 - half as f64;
            let sinc = if m == 0.0 { 2.0 * cut } else { (TAU * cut * m).sin() / (PI * m) };
            let w = bessel_i0(beta * (1.0 - (m / half as f64).powi(2)).max(0.0).sqrt()) / top;
            sinc * w
        })
        .collect();
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= sum);
    h
}

/// Filters and keeps every `decim`-th output. The first output is at the filter's middle, so
/// filtered and unfiltered streams line up.
pub fn fir_decimate(x: &[C64], h: &[f64], decim: usize) -> Vec<C64> {
    let half = h.len() / 2;
    let decim = decim.max(1);
    (0..x.len() / decim)
        .map(|n| {
            let center = n * decim;
            let mut sum = ZERO;
            for (i, c) in h.iter().enumerate() {
                let at = center as i64 + half as i64 - i as i64;
                if at >= 0 && (at as usize) < x.len() {
                    sum += x[at as usize] * *c;
                }
            }
            sum
        })
        .collect()
}

/// What's being received.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Waveform {
    Tone { offset_hz: f64 },
    Qpsk { symbol_rate: f64, rolloff: f64 },
    Qam16 { symbol_rate: f64, rolloff: f64 },
    /// Subcarriers of 16-QAM across `bw_hz`.
    Ofdm { subcarriers: usize, bw_hz: f64 },
    Chirp { bw_hz: f64, period_s: f64 },
    /// Band-limited noise, as a wideband jammer.
    Noise { bw_hz: f64 },
}

impl Waveform {
    pub fn label(&self) -> &'static str {
        match self {
            Waveform::Tone { .. } => "Tone",
            Waveform::Qpsk { .. } => "QPSK",
            Waveform::Qam16 { .. } => "16-QAM",
            Waveform::Ofdm { .. } => "OFDM",
            Waveform::Chirp { .. } => "Chirp",
            Waveform::Noise { .. } => "Noise",
        }
    }
}

/// A waveform and, if it has them, the symbols and timing a demodulator would use.
#[derive(Clone, Debug, Default)]
pub struct Generated {
    pub x: Vec<C64>,
    pub symbols: Vec<C64>,
    /// Samples per symbol at the rate it was made.
    pub sps: usize,
    /// The root-raised-cosine pulse, for a matched filter.
    pub pulse: Vec<f64>,
}

/// Root-raised-cosine impulse response, unit energy, `span` symbols long.
pub fn rrc(sps: usize, span: usize, rolloff: f64) -> Vec<f64> {
    let half = span * sps / 2;
    let b = rolloff.clamp(0.0, 1.0);
    let mut h: Vec<f64> = (0..=2 * half)
        .map(|i| {
            let t = (i as f64 - half as f64) / sps as f64;
            if t.abs() < 1e-9 {
                1.0 - b + 4.0 * b / PI
            } else if b > 0.0 && (t.abs() - 1.0 / (4.0 * b)).abs() < 1e-9 {
                b / 2f64.sqrt() * ((1.0 + 2.0 / PI) * (PI / (4.0 * b)).sin() + (1.0 - 2.0 / PI) * (PI / (4.0 * b)).cos())
            } else {
                ((PI * t * (1.0 - b)).sin() + 4.0 * b * t * (PI * t * (1.0 + b)).cos()) / (PI * t * (1.0 - (4.0 * b * t).powi(2)))
            }
        })
        .collect();
    let energy: f64 = h.iter().map(|v| v * v).sum::<f64>().sqrt();
    h.iter_mut().for_each(|v| *v /= energy);
    h
}

fn unit_power(x: &mut [C64]) {
    let p = norm_sqr(x) / x.len().max(1) as f64;
    if p > 0.0 {
        x.iter_mut().for_each(|v| *v /= p.sqrt());
    }
}

/// `n` samples at `fs`, unit power.
pub fn generate(w: &Waveform, fs: f64, n: usize, rng: &mut proof::Rng) -> Generated {
    let mut out = Generated::default();
    match *w {
        Waveform::Tone { offset_hz } => {
            out.x = (0..n).map(|i| C64::from_polar(1.0, TAU * offset_hz * i as f64 / fs)).collect();
        }
        Waveform::Qpsk { symbol_rate, rolloff } | Waveform::Qam16 { symbol_rate, rolloff } => {
            let sps = (fs / symbol_rate).round().max(2.0) as usize;
            let span = 12;
            let pulse = rrc(sps, span, rolloff);
            let count = n / sps + span;
            let qam = matches!(w, Waveform::Qam16 { .. });
            let levels = [-3.0, -1.0, 1.0, 3.0];
            out.symbols = (0..count)
                .map(|_| {
                    let pick = |r: &mut proof::Rng| levels[(r.uniform() * 4.0) as usize % 4];
                    if qam { C64::new(pick(rng), pick(rng)) / 10f64.sqrt() } else { C64::new(if rng.uniform() < 0.5 { 1.0 } else { -1.0 }, if rng.uniform() < 0.5 { 1.0 } else { -1.0 }) / 2f64.sqrt() }
                })
                .collect();
            let mut x = vec![ZERO; n + pulse.len()];
            for (k, s) in out.symbols.iter().enumerate() {
                let start = k * sps;
                for (i, c) in pulse.iter().enumerate() {
                    if start + i < x.len() {
                        x[start + i] += *s * *c;
                    }
                }
            }
            x.truncate(n);
            out.x = x;
            out.sps = sps;
            out.pulse = pulse;
        }
        Waveform::Ofdm { subcarriers, bw_hz } => {
            let spacing = bw_hz / subcarriers.max(1) as f64;
            let size = (fs / spacing).round().max(2.0) as usize;
            let levels = [-3.0, -1.0, 1.0, 3.0];
            let mut x = Vec::with_capacity(n + size);
            while x.len() < n {
                let mut bins = vec![ZERO; size];
                for k in 0..subcarriers.min(size) {
                    let offset = k as i64 - subcarriers as i64 / 2;
                    if offset == 0 {
                        continue;
                    }
                    let pick = |r: &mut proof::Rng| levels[(r.uniform() * 4.0) as usize % 4];
                    bins[offset.rem_euclid(size as i64) as usize] = C64::new(pick(rng), pick(rng));
                }
                super::math::ifft(&mut bins);
                // A guard interval of an eighth, as a cyclic prefix.
                let guard = size / 8;
                x.extend_from_slice(&bins[size - guard..]);
                x.extend_from_slice(&bins);
            }
            x.truncate(n);
            out.x = x;
        }
        Waveform::Chirp { bw_hz, period_s } => {
            let period = (period_s * fs).round().max(2.0) as usize;
            let slope = bw_hz / period_s;
            out.x = (0..n)
                .map(|i| {
                    let t = (i % period) as f64 / fs;
                    C64::from_polar(1.0, PI * slope * t * t - PI * bw_hz * t)
                })
                .collect();
        }
        Waveform::Noise { bw_hz } => {
            let white: Vec<C64> = (0..n + 64).map(|_| C64::new(rng.gauss(), rng.gauss())).collect();
            let h = kaiser_lowpass((bw_hz / fs / 2.0).clamp(0.002, 0.49), (bw_hz / fs / 10.0).max(0.01), 50.0, 1);
            out.x = fir_decimate(&white, &h, 1);
            out.x.truncate(n);
        }
    }
    unit_power(&mut out.x);
    out
}

/// A local oscillator's imperfections.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lo {
    /// Phase noise at `pn_offset_hz`, dBc/Hz, on a 1/f² slope. `None` for a clean LO.
    pub pn_dbc_hz: Option<f64>,
    pub pn_offset_hz: f64,
    /// Bandwidth of the loop that locks it to the reference, Hz: below it the noise is flat.
    pub loop_hz: f64,
    /// All channels off one synthesiser: its phase noise is common, so the beam holds, but
    /// nothing averages it away. Separate ones average out by the channel count.
    pub shared: bool,
}

impl Default for Lo {
    fn default() -> Self {
        Self { pn_dbc_hz: None, pn_offset_hz: 100e3, loop_hz: 100e3, shared: true }
    }
}

/// A phase error whose single-sideband spectrum is `pn_dbc_hz` at `offset`: a walk of linewidth
/// `Δν` pulled back towards zero by a loop `f_L` wide, so `L(f) = Δν / 2π(f² + f_L²)`, flat inside
/// the loop and falling as 1/f² outside it. That fixes `Δν = 2π·L·(offset² + f_L²)`, and its rms is
/// `√(Δν / 2·f_L)`, radians. With no loop it wanders without limit, as a free-running oscillator does.
pub fn phase_noise(n: usize, fs: f64, pn_dbc_hz: f64, offset: f64, loop_hz: f64, rng: &mut proof::Rng) -> Vec<f64> {
    let linewidth = TAU * (offset * offset + loop_hz * loop_hz) * 10f64.powf(pn_dbc_hz / 10.0);
    let step = (TAU * linewidth / fs).sqrt();
    let pull = (TAU * loop_hz.max(0.0) / fs).min(1.0);
    let mut phase = if pull > 0.0 { step / (2.0 * pull).sqrt() * rng.gauss() } else { 0.0 };
    (0..n)
        .map(|_| {
            phase += step * rng.gauss() - pull * phase;
            phase
        })
        .collect()
}

/// The digital weights.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Bf {
    /// One channel, no array gain.
    Single,
    /// Phases to the signal's direction.
    #[default]
    Steered,
    /// Capon, from the samples.
    Mvdr,
}

impl Bf {
    pub const ALL: [Bf; 3] = [Bf::Steered, Bf::Mvdr, Bf::Single];
    pub fn label(self) -> &'static str {
        match self {
            Bf::Single => "One channel",
            Bf::Steered => "Steered",
            Bf::Mvdr => "Adaptive (Capon)",
        }
    }
}

/// An interferer at the elements.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interferer {
    pub waveform: Waveform,
    /// Power at each element, dBm.
    pub dbm: f64,
    /// Relative phases across the channels.
    pub a: Vec<C64>,
}

/// A whole receive simulation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainSim {
    pub chain: RxChain,
    pub waveform: Waveform,
    /// Power at each element, dBm.
    pub signal_dbm: f64,
    /// Relative phases of the signal across the channels. Its length is the channel count.
    pub a: Vec<C64>,
    pub interferers: Vec<Interferer>,
    pub lo: Lo,
    pub iq_gain_db: f64,
    pub iq_phase_deg: f64,
    /// Fixed errors between channels: gain rms dB and phase rms degrees.
    pub channel_gain_db: f64,
    pub channel_phase_deg: f64,
    /// Measure channel errors with a pilot of this SNR, dB, and take them out.
    pub calibrate_snr_db: Option<f64>,
    /// Digital gain control: the converter's rms this far below full scale, dB.
    pub agc_backoff_db: Option<f64>,
    pub decim: usize,
    /// Samples at the converter's rate.
    pub samples: usize,
    pub bf: Bf,
    pub seed: u64,
}

impl Default for ChainSim {
    fn default() -> Self {
        let a = (0..8).map(|i| C64::from_polar(1.0, i as f64 * 0.6)).collect();
        Self {
            chain: RxChain::default(),
            waveform: Waveform::Qpsk { symbol_rate: 10e6, rolloff: 0.35 },
            signal_dbm: -80.0,
            a,
            interferers: Vec::new(),
            lo: Lo::default(),
            iq_gain_db: 0.2,
            iq_phase_deg: 1.0,
            channel_gain_db: 0.0,
            channel_phase_deg: 0.0,
            calibrate_snr_db: None,
            agc_backoff_db: Some(12.0),
            decim: 2,
            samples: 1 << 16,
            bf: Bf::Steered,
            seed: 1,
        }
    }
}

/// SNR in the signal's bandwidth at one point in the chain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tap {
    pub name: String,
    pub sinr_db: f64,
    pub power_dbm: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChainReport {
    pub taps: Vec<Tap>,
    pub channels: usize,
    /// After the digital filter on one channel, dB.
    pub element_sinr_db: f64,
    /// After the beamformer, dB.
    pub output_sinr_db: f64,
    /// Their difference, dB; `10·log N` for a perfect array in white noise.
    pub array_gain_db: f64,
    pub evm_percent: Option<f64>,
    /// `(I, Q)` of decided symbols, scaled to unit power.
    pub constellation: Vec<(f64, f64)>,
    /// `(MHz, dB against full scale)` at the converter.
    pub spectrum_adc: Vec<(f64, f64)>,
    /// `(MHz, dB against its peak)` after the beamformer.
    pub spectrum_out: Vec<(f64, f64)>,
    pub adc_rms_dbfs: f64,
    pub clipped_percent: f64,
    pub agc_db: f64,
    /// The budget this was run on.
    pub budget: Budget,
}

/// `(f in MHz, dB)` by averaged periodograms, normalised so a full-scale tone reads 0 dB.
pub fn welch(x: &[C64], fs: f64, reference: f64, size: usize) -> Vec<(f64, f64)> {
    let size = size.min(x.len().max(2)).next_power_of_two().min(x.len().max(2));
    let w = hann(size);
    let gain: f64 = w.iter().sum::<f64>().powi(2);
    let mut sum = vec![0.0; size];
    let mut count = 0.0f64;
    let mut at = 0;
    while at + size <= x.len() {
        let mut b: Vec<C64> = x[at..at + size].iter().zip(&w).map(|(v, c)| *v * *c).collect();
        fft(&mut b);
        for (s, v) in sum.iter_mut().zip(&b) {
            *s += v.norm_sqr();
        }
        count += 1.0;
        at += size / 2;
    }
    let shifted = fft_shift(&sum);
    shifted.iter().enumerate().map(|(i, v)| ((i as f64 - (size / 2) as f64) / size as f64 * fs / 1e6, db10(v / count.max(1.0) / gain / reference.max(1e-300)))).collect()
}

/// `y ≈ g·r` by least squares; the signal over what's left, dB, and `g`.
pub fn sinr_against(y: &[C64], r: &[C64]) -> (f64, C64) {
    let n = y.len().min(r.len());
    let g = inner(&r[..n], &y[..n]) / norm_sqr(&r[..n]).max(1e-300);
    let error: f64 = y[..n].iter().zip(&r[..n]).map(|(a, b)| (*a - *b * g).norm_sqr()).sum();
    (db10(norm_sqr(&r[..n]) * g.norm_sqr() / error.max(1e-300)), g)
}

fn noise_into(x: &mut [C64], power: f64, rng: &mut proof::Rng) {
    if power <= 0.0 {
        return;
    }
    let s = (power / 2.0).sqrt();
    for v in x.iter_mut() {
        *v += C64::new(rng.gauss(), rng.gauss()) * s;
    }
}

/// A mid-rise quantiser with each rail clipped at the full-scale tone's peak. Returns how many
/// rails hit the stops.
pub fn adc_quantise(x: &mut [C64], bits: u8, p_fs: f64) -> usize {
    let full = p_fs.sqrt();
    let delta = 2.0 * full / 2f64.powi(bits as i32);
    let top = 2f64.powi(bits as i32 - 1) - 0.5;
    let mut clipped = 0;
    let q = |v: f64, clipped: &mut usize| -> f64 {
        let k = (v / delta).floor() + 0.5;
        if k.abs() > top {
            *clipped += 1;
        }
        k.clamp(-top, top) * delta
    };
    for v in x.iter_mut() {
        *v = C64::new(q(v.re, &mut clipped), q(v.im, &mut clipped));
    }
    clipped
}

/// Runs a chain sample by sample and reads it at each point.
pub fn simulate(sim: &ChainSim) -> Result<ChainReport> {
    let n_ch = sim.a.len();
    if n_ch == 0 {
        bail!("no channels");
    }
    let c = &sim.chain;
    let fs = c.adc.fs_hz;
    let decim = sim.decim.max(1);
    if fs / decim as f64 <= c.bw_hz {
        bail!("{:.1} MHz after decimating by {decim} can't hold a {:.1} MHz signal", fs / decim as f64 / 1e6, c.bw_hz / 1e6);
    }
    let n = sim.samples.clamp(1 << 10, 1 << 20);
    let mut rng = proof::Rng::new(sim.seed);
    let wave = generate(&sim.waveform, fs, n, &mut rng);
    let intf: Vec<(Vec<C64>, f64, &Vec<C64>)> = sim.interferers.iter().map(|i| (generate(&i.waveform, fs, n, &mut rng).x, watts(i.dbm).sqrt(), &i.a)).collect();
    // The digital filter every stage is judged through, and the output stream comes from.
    let cut = (c.bw_hz / 2.0 / fs).min(0.49);
    let transition = ((fs / decim as f64 - c.bw_hz) / 2.0 / fs).clamp(0.005, 0.2);
    let h = kaiser_lowpass(cut + transition / 2.0, transition, 70.0, decim);
    let amp = watts(sim.signal_dbm).sqrt();
    let reference: Vec<C64> = fir_decimate(&wave.x, &h, decim);
    let p_fs = watts(c.adc.full_scale_dbm);
    let shared_pn = if sim.lo.shared { sim.lo.pn_dbc_hz.map(|l| phase_noise(n, fs, l, sim.lo.pn_offset_hz, sim.lo.loop_hz, &mut rng)) } else { None };
    let errors: Vec<C64> = (0..n_ch).map(|_| C64::from_polar(10f64.powf(sim.channel_gain_db * rng.gauss() / 20.0), (sim.channel_phase_deg * rng.gauss()).to_radians())).collect();
    let seeds: Vec<u64> = (0..n_ch).map(|_| rng.next_u64()).collect();
    // The antenna and the analog stages, per channel: taps only from channel 0, to save the sums.
    struct Analog {
        x: Vec<C64>,
        taps: Vec<(String, Vec<C64>)>,
    }
    let mut analog: Vec<Analog> = (0..n_ch)
        .into_par_iter()
        .map(|ch| {
            let mut r = proof::Rng::new(seeds[ch]);
            let mut x: Vec<C64> = wave.x.iter().map(|v| *v * sim.a[ch] * amp).collect();
            for (w, a, phases) in &intf {
                for (v, y) in x.iter_mut().zip(w) {
                    *v += *y * phases[ch] * *a;
                }
            }
            noise_into(&mut x, BOLTZMANN * c.t_ant_k * fs, &mut r);
            let mut taps = Vec::new();
            if ch == 0 {
                taps.push(("At the antenna".to_string(), x.clone()));
            }
            for s in &c.stages {
                let f = 10f64.powf(s.nf_db / 10.0);
                // Input-referred noise, then the gain and the distortion.
                noise_into(&mut x, BOLTZMANN * T0 * (f - 1.0) * fs, &mut r);
                let g = 10f64.powf(s.gain_db / 20.0);
                match s.iip3_dbm {
                    Some(ip) => {
                        let p = watts(ip);
                        x.iter_mut().for_each(|v| *v = compress(*v, p) * g);
                    }
                    None => x.iter_mut().for_each(|v| *v *= g),
                }
                if ch == 0 {
                    taps.push((format!("After {}", s.name), x.clone()));
                }
            }
            let own_pn = if sim.lo.shared { None } else { sim.lo.pn_dbc_hz.map(|l| phase_noise(n, fs, l, sim.lo.pn_offset_hz, sim.lo.loop_hz, &mut r)) };
            if let Some(pn) = shared_pn.as_ref().or(own_pn.as_ref()) {
                for (v, p) in x.iter_mut().zip(pn) {
                    *v *= C64::from_polar(1.0, *p);
                }
            }
            iq_imbalance(&mut x, sim.iq_gain_db, sim.iq_phase_deg);
            x.iter_mut().for_each(|v| *v *= errors[ch]);
            if ch == 0 {
                taps.push(("After LO and I/Q".to_string(), x.clone()));
            }
            Analog { x, taps }
        })
        .collect();
    let m = reference.len();
    let skip = (h.len() / decim).min(m / 4);
    let judged = |y: &[C64]| -> (f64, C64) { sinr_against(&y[skip..m - skip], &reference[skip..m - skip]) };
    // Every analog point read through the same filter, so all are in one bandwidth.
    let mut taps: Vec<Tap> = std::mem::take(&mut analog[0].taps).into_par_iter().map(|(name, x)| Tap { name, sinr_db: judged(&fir_decimate(&x, &h, decim)).0, power_dbm: dbm(norm_sqr(&x) / n as f64) }).collect();
    let rms: f64 = analog.iter().map(|a| norm_sqr(&a.x) / n as f64).sum::<f64>() / n_ch as f64;
    let agc = sim.agc_backoff_db.map_or(1.0, |b| (p_fs * 10f64.powf(-b / 10.0) / rms.max(1e-300)).sqrt());
    let jitter = TAU * c.adc.f_in_hz * c.adc.jitter_s;
    let converted: Vec<(Vec<C64>, usize, Vec<C64>)> = analog
        .into_par_iter()
        .enumerate()
        .map(|(ch, a)| {
            let mut r = proof::Rng::new(seeds[ch].wrapping_add(1));
            let mut x: Vec<C64> = a.x.into_iter().map(|v| v * agc).collect();
            if jitter > 0.0 {
                for v in x.iter_mut() {
                    *v *= C64::from_polar(1.0, jitter * r.gauss());
                }
            }
            let clipped = adc_quantise(&mut x, c.adc.bits, p_fs);
            let out = fir_decimate(&x, &h, decim);
            (out, clipped, x)
        })
        .collect();
    let first = &converted[0];
    let clipped: usize = converted.iter().map(|x| x.1).sum();
    let adc_power = norm_sqr(&first.2) / n as f64;
    let (element_sinr, _) = judged(&first.0);
    taps.push(Tap { name: format!("After the {}-bit converter", c.adc.bits), sinr_db: element_sinr, power_dbm: dbm(adc_power) });
    // Channel gains as the beamformer knows them.
    let known: Vec<C64> = (0..n_ch)
        .map(|ch| {
            let mut r = proof::Rng::new(seeds[ch].wrapping_add(2));
            let truth = sim.a[ch];
            match sim.calibrate_snr_db {
                Some(snr) => {
                    let e = C64::new(r.gauss(), r.gauss()) * (0.5f64.sqrt() * 10f64.powf(-snr / 20.0));
                    truth * errors[ch] * (C64::new(1.0, 0.0) + e)
                }
                None => truth,
            }
        })
        .collect();
    let out_len = converted[0].0.len();
    let weights: Vec<C64> = match sim.bf {
        Bf::Single => {
            let mut w = vec![ZERO; n_ch];
            w[0] = known[0].conj() / known[0].norm().max(1e-300);
            w
        }
        Bf::Steered => known.iter().map(|a| a.conj() / n_ch as f64 / a.norm().max(1e-300)).collect(),
        Bf::Mvdr => {
            let mut r = vec![vec![ZERO; n_ch]; n_ch];
            for t in skip..out_len - skip {
                for i in 0..n_ch {
                    let xi = converted[i].0[t];
                    for (j, slot) in r[i].iter_mut().enumerate() {
                        *slot += xi * converted[j].0[t].conj();
                    }
                }
            }
            let count = (out_len - 2 * skip).max(1) as f64;
            r.iter_mut().flatten().for_each(|v| *v /= count);
            let trace: f64 = (0..n_ch).map(|i| r[i][i].re).sum::<f64>() / n_ch as f64;
            for (i, row) in r.iter_mut().enumerate() {
                row[i] += trace * 1e-5;
            }
            let inv = invert(&r)?;
            let ria = mat_vec(&inv, &known);
            let d = inner(&known, &ria);
            ria.iter().map(|v| (*v / d).conj()).collect()
        }
    };
    let beam: Vec<C64> = (0..out_len).map(|t| (0..n_ch).map(|ch| weights[ch] * converted[ch].0[t]).sum()).collect();
    let (output_sinr, gain) = judged(&beam);
    taps.push(Tap { name: format!("After {} beamforming, {n_ch} channels", sim.bf.label().to_lowercase()), sinr_db: output_sinr, power_dbm: dbm(norm_sqr(&beam) / out_len.max(1) as f64) });
    // Decisions, for modulated signals.
    let (mut evm, mut constellation) = (None, Vec::new());
    if wave.sps > 0 && wave.sps % decim == 0 {
        let sps = wave.sps / decim;
        let pulse: Vec<f64> = wave.pulse.iter().step_by(decim).copied().collect();
        let energy: f64 = pulse.iter().map(|v| v * v).sum();
        let delay = pulse.len() / 2;
        let (mut points, mut want) = (Vec::new(), Vec::new());
        for (k, s) in wave.symbols.iter().enumerate() {
            let center = k * sps + delay;
            if center < skip + delay || center + delay >= out_len.saturating_sub(skip) {
                continue;
            }
            let mut v = ZERO;
            for (i, c) in pulse.iter().enumerate() {
                v += beam[center + i - delay] * *c;
            }
            points.push(v / energy / gain);
            want.push(*s);
        }
        // The matched filter sets its own scale, so the error is against the best fit.
        if !want.is_empty() {
            let (_, fit) = sinr_against(&points, &want);
            let err: f64 = points.iter().zip(&want).map(|(p, s)| (*p - *s * fit).norm_sqr()).sum();
            let sig: f64 = want.iter().map(|s| (*s * fit).norm_sqr()).sum();
            constellation = points.iter().take(1500).map(|p| { let v = *p / fit; (v.re, v.im) }).collect();
            if sig > 0.0 {
                evm = Some(100.0 * (err / sig).sqrt());
            }
        }
    }
    Ok(ChainReport {
        taps,
        channels: n_ch,
        element_sinr_db: element_sinr,
        output_sinr_db: output_sinr,
        array_gain_db: output_sinr - element_sinr,
        evm_percent: evm,
        constellation,
        spectrum_adc: welch(&first.2, fs, p_fs, 1024),
        spectrum_out: {
            let s = welch(&beam, fs / decim as f64, 1.0, 512);
            let top = s.iter().map(|v| v.1).fold(f64::MIN, f64::max);
            s.into_iter().map(|(f, v)| (f, v - top)).collect()
        },
        adc_rms_dbfs: db10(adc_power / p_fs),
        clipped_percent: 100.0 * clipped as f64 / (2.0 * n as f64 * n_ch as f64),
        agc_db: 2.0 * db10(agc),
        budget: budget(c),
    })
}

/// An ideal covariance for this simulation, for reading the adaptive beam against theory.
pub fn theory_output_sinr(sim: &ChainSim, element_snr_lin: f64) -> f64 {
    let noise = 1.0 / element_snr_lin.max(1e-30);
    let sources: Vec<(Vec<C64>, f64)> = sim.interferers.iter().map(|i| (i.a.clone(), watts(i.dbm) / watts(sim.signal_dbm))).collect();
    let r: CMat = if sources.is_empty() { covariance_of(noise, &[(sim.a.clone(), 0.0)]) } else { covariance_of(noise, &sources) };
    invert(&r).map_or(f64::NAN, |inv| inner(&sim.a, &mat_vec(&inv, &sim.a)).re)
}

/// A power amplifier's AM/AM by Rapp's model: `|y| = |x| / (1 + (|x|/A_sat)^(2p))^(1/2p)`.
pub fn rapp(x: C64, a_sat: f64, smooth: f64) -> C64 {
    let m = x.norm();
    if m == 0.0 {
        return x;
    }
    x / (1.0 + (m / a_sat).powf(2.0 * smooth)).powf(1.0 / (2.0 * smooth))
}

/// How a modulated signal fares at one output back-off from saturation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TxPoint {
    pub backoff_db: f64,
    /// Average output power, dBm.
    pub out_dbm: f64,
    pub evm_percent: f64,
    /// Next channel over the wanted one, dB.
    pub acpr_db: f64,
    /// With a memoryless polynomial predistorter.
    pub dpd_evm_percent: f64,
    pub dpd_acpr_db: f64,
    /// Peak over average at the amplifier's input, dB.
    pub papr_db: f64,
}

/// Power in a band of a signal, by periodogram, relative to full band power.
fn band_power(x: &[C64], fs: f64, lo: f64, hi: f64) -> f64 {
    let s = welch(x, fs, 1.0, 4096);
    s.iter().filter(|(f, _)| *f * 1e6 >= lo && *f * 1e6 <= hi).map(|(_, v)| 10f64.powf(v / 10.0)).sum()
}

/// Inverts a compressive amplifier with an odd polynomial, fitted by indirect learning: the
/// polynomial that maps the output back to the input is then run ahead of the amplifier.
pub fn fit_predistorter(input: &[C64], output: &[C64], order: usize) -> Result<Vec<C64>> {
    let terms = order.max(1);
    let n = input.len().min(output.len());
    let basis = |y: C64, i: usize| y * y.norm_sqr().powi(i as i32);
    let mut gram = vec![vec![ZERO; terms]; terms];
    let mut rhs = vec![ZERO; terms];
    for t in (0..n).step_by(3) {
        let y = output[t];
        let b: Vec<C64> = (0..terms).map(|i| basis(y, i)).collect();
        for i in 0..terms {
            for j in 0..terms {
                gram[i][j] += b[i].conj() * b[j];
            }
            rhs[i] += b[i].conj() * input[t];
        }
    }
    let scale: f64 = (0..terms).map(|i| gram[i][i].re).sum::<f64>() / terms as f64;
    for (i, row) in gram.iter_mut().enumerate() {
        row[i] += scale * 1e-12;
    }
    Ok(mat_vec(&invert(&gram)?, &rhs))
}

/// Applies a fitted polynomial.
pub fn predistort(x: &[C64], c: &[C64]) -> Vec<C64> {
    x.iter().map(|v| c.iter().enumerate().map(|(i, k)| *k * *v * v.norm_sqr().powi(i as i32)).sum()).collect()
}

/// Sweeps back-off on one modulated signal through one amplifier. `bw_hz` is the channel, and
/// the adjacent one sits one channel over.
pub fn pa_sweep(w: &Waveform, fs: f64, bw_hz: f64, p_sat_dbm: f64, smooth: f64, backoffs: &[f64], seed: u64) -> Vec<TxPoint> {
    let n = 1 << 15;
    let mut rng = proof::Rng::new(seed);
    let raw = generate(w, fs, n, &mut rng).x;
    // A real transmitter filters its channel first, or the waveform's own skirts hide the amplifier's.
    let shaping = kaiser_lowpass(0.55 * bw_hz / fs, 0.1 * bw_hz / fs, 80.0, 1);
    let mut wave = fir_decimate(&raw, &shaping, 1);
    let edge = shaping.len().min(n / 8);
    wave.truncate(n - edge);
    wave.drain(..edge);
    unit_power(&mut wave);
    let n = wave.len();
    let a_sat = 1.0;
    let band = |x: &[C64]| -> f64 {
        let main = band_power(x, fs, -bw_hz / 2.0, bw_hz / 2.0);
        // One channel wide, starting clear of the channel filter's own skirt.
        let adjacent = band_power(x, fs, 0.62 * bw_hz, 1.62 * bw_hz);
        db10(adjacent / main.max(1e-300))
    };
    let peak = wave.iter().map(C64::norm_sqr).fold(0.0, f64::max);
    let papr = db10(peak / (norm_sqr(&wave) / n as f64));
    backoffs
        .par_iter()
        .map(|b| {
            let level = 10f64.powf(-b / 20.0) * a_sat;
            let drive: Vec<C64> = wave.iter().map(|v| *v * level).collect();
            let out: Vec<C64> = drive.iter().map(|v| rapp(*v, a_sat, smooth)).collect();
            let (snr, g) = sinr_against(&out, &wave);
            let evm = 100.0 * 10f64.powf(-snr / 20.0);
            let out_power = norm_sqr(&out) / n as f64;
            let acpr = band(&out);
            let linear: Vec<C64> = out.iter().map(|v| *v / (g / level)).collect();
            let (dpd_evm, dpd_acpr) = match fit_predistorter(&drive, &linear, 5) {
                Ok(c) => {
                    let pre = predistort(&drive, &c);
                    let shaped: Vec<C64> = pre.iter().map(|v| rapp(*v, a_sat, smooth)).collect();
                    let (s, _) = sinr_against(&shaped, &wave);
                    (100.0 * 10f64.powf(-s / 20.0), band(&shaped))
                }
                Err(_) => (evm, acpr),
            };
            TxPoint { backoff_db: *b, out_dbm: p_sat_dbm + db10(out_power), evm_percent: evm, acpr_db: acpr, dpd_evm_percent: dpd_evm, dpd_acpr_db: dpd_acpr, papr_db: papr }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_at(power: f64, f: f64, fs: f64, n: usize) -> Vec<C64> {
        (0..n).map(|i| C64::from_polar(power.sqrt(), TAU * f * i as f64 / fs)).collect()
    }

    #[test]
    fn a_cascade_adds_up_by_friis() {
        let c = RxChain { stages: vec![Stage::new("LNA", 20.0, 1.0, Some(-5.0)), Stage::new("Mixer", -7.0, 7.0, Some(10.0)), Stage::new("Amp", 25.0, 5.0, Some(20.0))], ..Default::default() };
        let b = budget(&c);
        let f1 = 10f64.powf(0.1);
        let f2 = 10f64.powf(0.7);
        let f3 = 10f64.powf(0.5);
        let g1 = 100.0;
        let g2 = g1 * 10f64.powf(-0.7);
        let want = f1 + (f2 - 1.0) / g1 + (f3 - 1.0) / g2;
        assert!((b.nf_analog_db - db10(want)).abs() < 1e-9, "{} vs {}", b.nf_analog_db, db10(want));
        assert!((b.gain_db - 38.0).abs() < 1e-9);
        let ip = 1.0 / (1.0 / watts(-5.0) + g1 / watts(10.0) + g2 / watts(20.0));
        assert!((b.iip3_dbm.unwrap() - dbm(ip)).abs() < 1e-9, "{:?}", b.iip3_dbm);
        // kT at 290 K is −174 dBm/Hz.
        let floor = dbm(BOLTZMANN * T0 * 1.0);
        assert!((floor + 174.0).abs() < 0.03, "{floor}");
    }

    #[test]
    fn an_adc_reaches_six_db_per_bit() {
        for bits in [6u8, 8, 10, 12] {
            let p_fs = 1.0;
            // A little below full scale, at a frequency that exercises every code.
            let level = 0.5;
            let mut x = tone_at(p_fs * level, 1234.567, 80e6, 1 << 15);
            let clean = x.clone();
            adc_quantise(&mut x, bits, p_fs);
            let (snr, _) = sinr_against(&x, &clean);
            let want = 6.02 * bits as f64 + 1.76 - 3.01;
            assert!((snr - want).abs() < 1.0, "{bits} bits: {snr} vs {want}");
        }
    }

    #[test]
    fn clipping_is_counted() {
        let mut x = tone_at(4.0, 1e6, 80e6, 4096);
        let hits = adc_quantise(&mut x, 8, 1.0);
        assert!(hits > 1000, "{hits}");
        let top = x.iter().map(|v| v.re.abs().max(v.im.abs())).fold(0.0, f64::max);
        assert!(top <= 1.0, "{top}");
    }

    #[test]
    fn a_cubic_compresses_at_the_textbook_points() {
        let ip3 = 1.0;
        // 1 dB compression 9.64 dB below IP3.
        let p = 10f64.powf(-0.964);
        let got = compress(C64::new(p.sqrt(), 0.0), ip3).norm() / p.sqrt();
        assert!((20.0 * got.log10() + 1.0).abs() < 0.02, "{}", 20.0 * got.log10());
        // Two tones: the third-order product is P_IM3 = 3·P − 2·IP3, in dBm.
        let fs = 1024.0;
        let n = 4096;
        let level = 1e-3f64; // −30 dB against the intercept per tone.
        let tone = |f: f64, i: usize| C64::from_polar(level.sqrt(), TAU * f * i as f64 / fs);
        let x: Vec<C64> = (0..n).map(|i| tone(50.0, i) + tone(60.0, i)).collect();
        let mut y: Vec<C64> = x.iter().map(|v| compress(*v, ip3)).collect();
        fft(&mut y);
        let bin = |f: f64| ((f / fs * n as f64).round() as usize) % n;
        let fund = y[bin(50.0)].norm_sqr() / (n * n) as f64;
        let im3 = y[bin(40.0)].norm_sqr() / (n * n) as f64;
        let want = 3.0 * db10(level) - 2.0 * db10(ip3);
        assert!((db10(im3) - want).abs() < 0.2, "IM3 {} vs {want}", db10(im3));
        assert!((db10(fund) - db10(level)).abs() < 0.1);
    }

    #[test]
    fn image_rejection_matches_its_formula() {
        for (g, ph) in [(0.5, 2.0), (1.0, 5.0), (0.1, 0.5)] {
            let mut x = tone_at(1.0, 1000.0, 8192.0, 8192);
            iq_imbalance(&mut x, g, ph);
            fft(&mut x);
            let want = image_rejection_db(g, ph);
            let got = db10(x[1000].norm_sqr() / x[8192 - 1000].norm_sqr());
            assert!((got - want).abs() < 0.05, "({g}, {ph}): {got} vs {want}");
        }
        // 1 dB and 5° is close to the rule-of-thumb 25 dB.
        assert!((image_rejection_db(1.0, 5.0) - 22.7).abs() < 1.0, "{}", image_rejection_db(1.0, 5.0));
    }

    #[test]
    fn jitter_sets_the_ceiling_its_formula_says() {
        let f = 100e6;
        let jitter = 2e-12;
        let n = 1 << 14;
        let mut r = proof::Rng::new(3);
        let x = tone_at(1.0, 1e5, 1e8, n);
        let mut y = x.clone();
        for v in y.iter_mut() {
            *v *= C64::from_polar(1.0, TAU * f * jitter * r.gauss());
        }
        let (snr, _) = sinr_against(&y, &x);
        let want = -2.0 * db10(TAU * f * jitter);
        assert!((snr - want).abs() < 0.2, "{snr} vs {want}");
    }

    #[test]
    fn phase_noise_has_the_level_asked_for() {
        // −90 dBc/Hz at 100 kHz, read at 100 kHz off the carrier, well above a 1 kHz loop.
        let (fs, n) = (20e6, 1 << 19);
        let level = -90.0;
        let mut r = proof::Rng::new(4);
        let p = phase_noise(n, fs, level, 100e3, 1e3, &mut r);
        // Its rms is what the loop's formula says: Δν = 2π·f²·L, σ² = Δν/(2·f_loop).
        let var = p.iter().map(|v| v * v).sum::<f64>() / n as f64;
        let want_var = TAU * (1e10 + 1e6) * 1e-9 / (2.0 * 1e3);
        assert!((var / want_var - 1.0).abs() < 0.35, "{var} vs {want_var}");
        let x: Vec<C64> = p.iter().map(|v| C64::from_polar(1.0, *v)).collect();
        let size = 1 << 12;
        let s = welch(&x, fs, 1.0, size);
        // Each bin's power over the bin's noise bandwidth, Hann's 1.5 bins.
        let rbw = 1.5 * fs / size as f64;
        let near: Vec<f64> = s.iter().filter(|(f, _)| (*f * 1e6 - 100e3).abs() < 15e3).map(|(_, v)| *v).collect();
        let mean = db10(near.iter().map(|v| 10f64.powf(v / 10.0)).sum::<f64>() / near.len() as f64);
        let got = mean - db10(rbw);
        assert!((got - level).abs() < 2.0, "{got} vs {level}");
    }

    #[test]
    fn phase_noise_has_the_level_asked_for_inside_the_loop_too() {
        // −90 dBc/Hz at 10 kHz, inside a 100 kHz loop: flat there, at that level.
        let (fs, n) = (20e6, 1 << 19);
        let mut r = proof::Rng::new(5);
        let p = phase_noise(n, fs, -90.0, 10e3, 100e3, &mut r);
        // σ² = Δν/2f_L = 2π·L·(f² + f_L²)/2f_L.
        let want = TAU * 1e-9 * (1e8 + 1e10) / 2e5;
        let got = p.iter().map(|v| v * v).sum::<f64>() / n as f64;
        assert!((got / want - 1.0).abs() < 0.35, "{got} vs {want}");
        let x: Vec<C64> = p.iter().map(|v| C64::from_polar(1.0, *v)).collect();
        let size = 1 << 13;
        let s = welch(&x, fs, 1.0, size);
        let rbw = 1.5 * fs / size as f64;
        let near: Vec<f64> = s.iter().filter(|(f, _)| (*f * 1e6 - 10e3).abs() < 6e3).map(|(_, v)| *v).collect();
        let mean = db10(near.iter().map(|v| 10f64.powf(v / 10.0)).sum::<f64>() / near.len() as f64) - db10(rbw);
        assert!((mean + 90.0).abs() < 2.5, "{mean}");
    }

    #[test]
    fn a_kaiser_filter_passes_its_band_and_stops_the_rest() {
        let h = kaiser_lowpass(0.125, 0.05, 70.0, 2);
        assert!(h.len() % 2 == 1);
        assert_eq!((h.len() / 2) % 2, 0, "delay is on the decimated grid");
        let response = |f: f64| h.iter().enumerate().map(|(i, c)| C64::from_polar(*c, -TAU * f * i as f64)).sum::<C64>().norm();
        assert!((20.0 * response(0.05).log10()).abs() < 0.01, "{}", response(0.05));
        for f in [0.16, 0.2, 0.3, 0.45] {
            assert!(20.0 * response(f).log10() < -68.0, "{f}: {}", 20.0 * response(f).log10());
        }
    }

    #[test]
    fn waveforms_have_unit_power_and_their_spectrum() {
        let mut r = proof::Rng::new(1);
        let fs = 80e6;
        for w in [Waveform::Tone { offset_hz: 1e6 }, Waveform::Qpsk { symbol_rate: 10e6, rolloff: 0.35 }, Waveform::Qam16 { symbol_rate: 10e6, rolloff: 0.35 }, Waveform::Ofdm { subcarriers: 52, bw_hz: 20e6 }, Waveform::Chirp { bw_hz: 20e6, period_s: 100e-6 }, Waveform::Noise { bw_hz: 20e6 }] {
            let g = generate(&w, fs, 1 << 15, &mut r);
            assert_eq!(g.x.len(), 1 << 15);
            let p = norm_sqr(&g.x) / g.x.len() as f64;
            assert!((p - 1.0).abs() < 1e-9, "{}: {p}", w.label());
            if !matches!(w, Waveform::Tone { .. }) {
                let s = welch(&g.x, fs, 1.0, 1024);
                let inside: f64 = s.iter().filter(|v| v.0.abs() < 13.5).map(|v| 10f64.powf(v.1 / 10.0)).sum();
                let outside: f64 = s.iter().filter(|v| v.0.abs() > 15.0).map(|v| 10f64.powf(v.1 / 10.0)).sum();
                assert!(inside > 50.0 * outside, "{}: {inside} vs {outside}", w.label());
            }
        }
    }

    #[test]
    fn an_ofdm_signal_has_the_peak_it_should() {
        let mut r = proof::Rng::new(2);
        let g = generate(&Waveform::Ofdm { subcarriers: 256, bw_hz: 20e6 }, 80e6, 1 << 16, &mut r);
        let peak = g.x.iter().map(C64::norm_sqr).fold(0.0, f64::max);
        assert!((8.0..14.0).contains(&db10(peak)), "PAPR {}", db10(peak));
    }

    fn clean_sim(channels: usize) -> ChainSim {
        ChainSim {
            chain: RxChain { adc: Adc { bits: 14, fs_hz: 80e6, full_scale_dbm: 4.0, jitter_s: 0.0, f_in_hz: 10e6 }, bw_hz: 20e6, ..Default::default() },
            a: (0..channels).map(|i| C64::from_polar(1.0, i as f64 * 0.7)).collect(),
            iq_gain_db: 0.0,
            iq_phase_deg: 0.0,
            decim: 2,
            samples: 1 << 16,
            signal_dbm: -85.0,
            ..Default::default()
        }
    }

    #[test]
    fn the_chain_measures_the_noise_figure_the_budget_predicts() {
        let sim = ChainSim { chain: RxChain { adc: Adc { bits: 14, fs_hz: 80e6, full_scale_dbm: 4.0, jitter_s: 0.0, f_in_hz: 10e6 }, bw_hz: 20e6, stages: vec![Stage::new("LNA", 18.0, 1.5, None), Stage::new("Mixer", -6.0, 8.0, None), Stage::new("Amp", 40.0, 6.0, None)], ..Default::default() }, ..clean_sim(1) };
        let report = simulate(&sim).unwrap();
        let b = budget(&sim.chain);
        // The bandwidth SNR is read in is the filter's, close to 30 MHz at this decimation.
        let want = sim.signal_dbm - b.noise_floor_dbm;
        let effective = 10.0 * ((1.5 * sim.chain.bw_hz) / sim.chain.bw_hz).log10();
        assert!((report.element_sinr_db - (want - effective)).abs() < 1.2, "{} vs {}", report.element_sinr_db, want - effective);
    }

    #[test]
    fn eight_channels_give_nine_db_of_array_gain() {
        let report = simulate(&clean_sim(8)).unwrap();
        assert!((report.array_gain_db - 9.03).abs() < 0.5, "{} from {} and {}", report.array_gain_db, report.element_sinr_db, report.output_sinr_db);
    }

    #[test]
    fn a_shared_lo_keeps_the_array_gain_and_separate_ones_lose_it() {
        // Strong signal, so phase noise is the limit: −70 dBc/Hz at 100 kHz in a 100 kHz loop
        // is about 10° rms, a ceiling near 15 dB on any one channel.
        let base = |shared: bool| ChainSim { lo: Lo { pn_dbc_hz: Some(-70.0), pn_offset_hz: 100e3, loop_hz: 100e3, shared }, signal_dbm: -55.0, ..clean_sim(8) };
        let together = simulate(&base(true)).unwrap();
        let apart = simulate(&base(false)).unwrap();
        assert!((10.0..20.0).contains(&together.element_sinr_db), "{}", together.element_sinr_db);
        // One LO: the same phase error on every channel survives the beam whole.
        assert!(together.array_gain_db.abs() < 1.0, "{}", together.array_gain_db);
        // Separate LOs: it adds up incoherently, and the beam gains the full 9 dB on it.
        assert!((apart.array_gain_db - 9.03).abs() < 1.0, "{}", apart.array_gain_db);
    }

    #[test]
    fn evm_follows_snr_for_modulated_signals() {
        for dbm_in in [-85.0, -75.0, -65.0] {
            let report = simulate(&ChainSim { signal_dbm: dbm_in, ..clean_sim(8) }).unwrap();
            let evm = report.evm_percent.unwrap();
            assert!(evm > 0.0 && !report.constellation.is_empty(), "{evm}");
            // The filter is wider than the symbols, so read against SNR in the symbol's band.
            let snr = report.output_sinr_db + 10.0 * (30.0f64 / 10.0).log10();
            let want = 100.0 * 10f64.powf(-snr / 20.0);
            assert!(evm / want > 0.5 && evm / want < 2.5, "at {dbm_in} dBm: EVM {evm} vs {want}");
        }
    }

    #[test]
    fn a_strong_jammer_is_cancelled_and_more_bits_do_it_better() {
        let phases: Vec<C64> = (0..8).map(|i| C64::from_polar(1.0, i as f64 * 2.1)).collect();
        let make = |bits: u8, bf: Bf| ChainSim {
            chain: RxChain { adc: Adc { bits, fs_hz: 80e6, full_scale_dbm: 4.0, jitter_s: 0.0, f_in_hz: 10e6 }, bw_hz: 20e6, stages: vec![Stage::new("LNA", 20.0, 1.5, None), Stage::new("Amp", 40.0, 6.0, None)], ..Default::default() },
            signal_dbm: -80.0,
            bf,
            interferers: vec![Interferer { waveform: Waveform::Tone { offset_hz: 2e6 }, dbm: -40.0, a: phases.clone() }],
            iq_gain_db: 0.0,
            iq_phase_deg: 0.0,
            ..clean_sim(8)
        };
        let steered = simulate(&make(14, Bf::Steered)).unwrap();
        let nulled = simulate(&make(14, Bf::Mvdr)).unwrap();
        assert!(steered.output_sinr_db < 0.0, "a side lobe is no match for 40 dB: {}", steered.output_sinr_db);
        assert!(nulled.output_sinr_db > steered.output_sinr_db + 25.0, "{} vs {}", nulled.output_sinr_db, steered.output_sinr_db);
        let coarse = simulate(&make(6, Bf::Mvdr)).unwrap();
        assert!(nulled.output_sinr_db > coarse.output_sinr_db + 3.0, "more bits: {} vs {}", nulled.output_sinr_db, coarse.output_sinr_db);
    }

    #[test]
    fn channel_errors_cost_array_gain_until_calibrated() {
        let rough = ChainSim { channel_gain_db: 1.5, channel_phase_deg: 45.0, seed: 3, ..clean_sim(8) };
        let cal = ChainSim { calibrate_snr_db: Some(35.0), ..rough.clone() };
        let bad = simulate(&rough).unwrap();
        let good = simulate(&cal).unwrap();
        assert!(good.array_gain_db > bad.array_gain_db + 0.8, "{} vs {}", good.array_gain_db, bad.array_gain_db);
        assert!(good.array_gain_db > 8.3, "{}", good.array_gain_db);
    }

    #[test]
    fn a_decimation_too_far_is_refused() {
        let mut sim = clean_sim(2);
        sim.decim = 8;
        assert!(simulate(&sim).is_err());
    }

    #[test]
    fn rapp_saturates_and_is_linear_small() {
        let small = rapp(C64::new(0.01, 0.0), 1.0, 2.0).norm();
        assert!((small - 0.01).abs() < 1e-6);
        let big = rapp(C64::new(100.0, 0.0), 1.0, 2.0).norm();
        assert!((big - 1.0).abs() < 1e-3);
        // Smooth = 2 at saturation input: 2^(−1/4).
        let knee = rapp(C64::new(1.0, 0.0), 1.0, 2.0).norm();
        assert!((knee - 2f64.powf(-0.25)).abs() < 1e-9, "{knee}");
    }

    #[test]
    fn back_off_and_predistortion_each_clean_up_an_amplifier() {
        let w = Waveform::Ofdm { subcarriers: 128, bw_hz: 10e6 };
        let points = pa_sweep(&w, 80e6, 10e6, 30.0, 2.0, &[3.0, 8.0, 14.0], 1);
        assert!(points[1].evm_percent < points[0].evm_percent, "{points:?}");
        assert!(points[2].evm_percent < points[1].evm_percent, "{points:?}");
        assert!(points[2].acpr_db < points[0].acpr_db - 8.0, "{points:?}");
        // Predistortion: at a moderate back-off, a lot closer to linear.
        assert!(points[1].dpd_acpr_db < points[1].acpr_db - 6.0, "{points:?}");
        assert!(points[1].dpd_evm_percent < 0.6 * points[1].evm_percent, "{points:?}");
        // Output power follows the back-off, less the compression.
        assert!(points[2].out_dbm < points[0].out_dbm - 5.0, "{points:?}");
    }
}
