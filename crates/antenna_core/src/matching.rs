//! Matching networks: closed-form L, pi, T, stub and quarter-wave synthesis, E-series
//! snapping, finite-Q losses, broadband refinement and the Bode-Fano limit.
//!
//! Components with a finite Q are modelled with a series resistance:
//! `ωL/Q` for an inductor and `1/(ωCQ)` for a capacitor, Q constant with frequency. A
//! line or stub with a finite Q has `αℓ = βℓ/(2Q)`.

use std::f64::consts::PI;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::optimize::{Method, Options, minimize};
use crate::{C64, db10, db20};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    SeriesL,
    SeriesC,
    ShuntL,
    ShuntC,
    /// Transmission line: `z0`, electrical length in degrees at design frequency `f0`.
    Line { z0: f64, deg: f64, f0: f64 },
    /// Shunt stub, shorted or open.
    Stub { z0: f64, deg: f64, f0: f64, shorted: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub kind: Kind,
    /// H or F for lumped elements, unused for lines and stubs.
    pub value: f64,
    /// Component Q (`None`: ideal).
    pub q: Option<f64>,
}

/// Elements ordered from the source towards the load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Network {
    pub name: String,
    pub elements: Vec<Element>,
}

const NEGLIGIBLE: f64 = 1e-9;
/// Every closed-form solution is checked against the chain model before it is returned.
const MAX_SYNTHESIS_ERROR: f64 = 1e-4;

fn angular(f: f64) -> f64 {
    (2.0 * PI * f).max(1e-9)
}

fn lumped(kind: Kind, value: f64) -> Element {
    Element { kind, value, q: None }
}

fn line(z0: f64, deg: f64, f0: f64) -> Element {
    Element { kind: Kind::Line { z0, deg, f0 }, value: 0.0, q: None }
}

/// The inductor or capacitor with reactance `x` at `w`; none for a negligible `x`.
fn series_reactance(x: f64, w: f64, z0: f64) -> Option<Element> {
    if x.abs() < NEGLIGIBLE * z0 {
        None
    } else if x > 0.0 {
        Some(lumped(Kind::SeriesL, x / w))
    } else {
        Some(lumped(Kind::SeriesC, -1.0 / (w * x)))
    }
}

/// The inductor or capacitor with susceptance `b` at `w`; none for a negligible `b`.
fn shunt_susceptance(b: f64, w: f64, z0: f64) -> Option<Element> {
    if b.abs() < NEGLIGIBLE / z0 {
        None
    } else if b > 0.0 {
        Some(lumped(Kind::ShuntC, b / w))
    } else {
        Some(lumped(Kind::ShuntL, -1.0 / (w * b)))
    }
}

fn describe(element: &Element) -> String {
    match element.kind {
        Kind::SeriesL => "series L".to_string(),
        Kind::SeriesC => "series C".to_string(),
        Kind::ShuntL => "shunt L".to_string(),
        Kind::ShuntC => "shunt C".to_string(),
        Kind::Line { .. } => "line".to_string(),
        Kind::Stub { shorted: true, .. } => "shorted stub".to_string(),
        Kind::Stub { shorted: false, .. } => "open stub".to_string(),
    }
}

fn named(family: &str, elements: Vec<Element>) -> Network {
    let parts: Vec<String> = elements.iter().map(describe).collect();
    let name = if parts.is_empty() { format!("{family}: direct connection") } else { format!("{family}: {}", parts.join(", ")) };
    Network { name, elements }
}

fn same_values(a: &Network, b: &Network, tolerance: f64) -> bool {
    a.elements.len() == b.elements.len()
        && a.elements.iter().zip(&b.elements).all(|(x, y)| {
            let close = |p: f64, q: f64| (p - q).abs() <= tolerance * p.abs().max(q.abs()).max(1e-30);
            match (x.kind, y.kind) {
                (Kind::Line { z0: za, deg: da, .. }, Kind::Line { z0: zb, deg: db, .. }) => close(za, zb) && close(da, db),
                (Kind::Stub { z0: za, deg: da, shorted: sa, .. }, Kind::Stub { z0: zb, deg: db, shorted: sb, .. }) => {
                    sa == sb && close(za, zb) && close(da, db)
                }
                (ka, kb) => ka == kb && close(x.value, y.value),
            }
        })
}

/// Keeps distinct networks that really do match at `f`.
fn keep_valid(candidates: Vec<Network>, zl: C64, z0: f64, f: f64) -> Vec<Network> {
    let mut out: Vec<Network> = Vec::new();
    for net in candidates {
        if reflection(&net, zl, z0, f).norm() < MAX_SYNTHESIS_ERROR && !out.iter().any(|n| same_values(n, &net, 1e-9)) {
            out.push(net);
        }
    }
    out
}

fn is_matchable(zl: C64, z0: f64, f: f64) -> bool {
    zl.re > 0.0 && zl.re.is_finite() && zl.im.is_finite() && z0 > 0.0 && f > 0.0
}

/// Every valid two-element L solution: up to four, fewer for loads inside the
/// constant-resistance or constant-conductance circle of `z0`. A load already at
/// `z0` gets one network with no elements.
pub fn synth_l(zl: C64, z0: f64, f: f64) -> Vec<Network> {
    if !is_matchable(zl, z0, f) {
        return Vec::new();
    }
    let w = angular(f);
    let y = zl.inv();
    let mut candidates = Vec::new();

    // Shunt across the load, series towards the source.
    if y.re * z0 <= 1.0 + 1e-12 {
        let root = (y.re / z0 - y.re * y.re).max(0.0).sqrt();
        for sign in [1.0, -1.0] {
            let total = sign * root;
            let series = total / (y.re * y.re + total * total);
            let elements = [series_reactance(series, w, z0), shunt_susceptance(total - y.im, w, z0)];
            candidates.push(named("L", elements.into_iter().flatten().collect()));
        }
    }
    // Series with the load, shunt towards the source.
    if zl.re <= z0 * (1.0 + 1e-12) {
        let root = (zl.re * (z0 - zl.re)).max(0.0).sqrt();
        for sign in [1.0, -1.0] {
            let total = sign * root;
            let shunt = total / (zl.re * zl.re + total * total);
            let elements = [shunt_susceptance(shunt, w, z0), series_reactance(total - zl.im, w, z0)];
            candidates.push(named("L", elements.into_iter().flatten().collect()));
        }
    }
    keep_valid(candidates, zl, z0, f)
}

/// Pi solutions through a virtual resistance `R/(1 + q²)`, `R` being the larger of `z0`
/// and the load's parallel resistance. Below the minimum
/// `q = √(R_high/R_low − 1)` there is no solution.
pub fn synth_pi(zl: C64, z0: f64, f: f64, q: f64) -> Vec<Network> {
    if !is_matchable(zl, z0, f) {
        return Vec::new();
    }
    let w = angular(f);
    let y = zl.inv();
    let parallel = 1.0 / y.re;
    let virtual_r = z0.max(parallel) / (1.0 + q * q);
    if virtual_r > z0.min(parallel) {
        return Vec::new();
    }
    let q_source = (z0 / virtual_r - 1.0).max(0.0).sqrt();
    let q_load = (parallel / virtual_r - 1.0).max(0.0).sqrt();
    let mut candidates = Vec::new();
    for s1 in [1.0, -1.0] {
        for s2 in [1.0, -1.0] {
            let elements = [
                shunt_susceptance(s1 * q_source / z0, w, z0),
                series_reactance((s1 * q_source + s2 * q_load) * virtual_r, w, z0),
                shunt_susceptance(s2 * q_load / parallel - y.im, w, z0),
            ];
            candidates.push(named("Pi", elements.into_iter().flatten().collect()));
        }
    }
    keep_valid(candidates, zl, z0, f)
}

/// T solutions through a virtual resistance `R·(1 + q²)`, `R` being the smaller of `z0`
/// and the load's series resistance. Below the minimum
/// `q = √(R_high/R_low − 1)` there is no solution.
pub fn synth_t(zl: C64, z0: f64, f: f64, q: f64) -> Vec<Network> {
    if !is_matchable(zl, z0, f) {
        return Vec::new();
    }
    let w = angular(f);
    let virtual_r = z0.min(zl.re) * (1.0 + q * q);
    if virtual_r < z0.max(zl.re) {
        return Vec::new();
    }
    let q_source = (virtual_r / z0 - 1.0).max(0.0).sqrt();
    let q_load = (virtual_r / zl.re - 1.0).max(0.0).sqrt();
    let mut candidates = Vec::new();
    for s1 in [1.0, -1.0] {
        for s2 in [1.0, -1.0] {
            let elements = [
                series_reactance(s1 * q_source * z0, w, z0),
                shunt_susceptance((s1 * q_source + s2 * q_load) / virtual_r, w, z0),
                series_reactance(s2 * q_load * zl.re - zl.im, w, z0),
            ];
            candidates.push(named("T", elements.into_iter().flatten().collect()));
        }
    }
    keep_valid(candidates, zl, z0, f)
}

/// Series line then shunt stub, all of impedance `z0`, shorted and open: up to four
/// solutions. Electrical lengths are at `f`, each within 0..180°.
pub fn synth_stub(zl: C64, z0: f64, f: f64) -> Vec<Network> {
    if !is_matchable(zl, z0, f) {
        return Vec::new();
    }
    let (r, x) = (zl.re, zl.im);
    let lengths: Vec<f64> = if (r - z0).abs() < 1e-9 * z0 {
        vec![(-x / (2.0 * z0)).atan().rem_euclid(PI), PI / 2.0]
    } else {
        let root = (r * ((z0 - r).powi(2) + x * x) / z0).sqrt();
        [1.0, -1.0].iter().map(|s| ((x + s * root) / (r - z0)).atan().rem_euclid(PI)).collect()
    };
    let mut candidates = Vec::new();
    for theta in lengths {
        let series = line(z0, theta.to_degrees(), f);
        let looking_in = named("", vec![series]);
        let needed = -input_impedance(&looking_in, zl, f).inv().im;
        let open = (needed * z0).atan().rem_euclid(PI);
        let shorted = (-1.0 / (needed * z0)).atan().rem_euclid(PI);
        for (deg, is_shorted) in [(shorted.to_degrees(), true), (open.to_degrees(), false)] {
            let stub = Element { kind: Kind::Stub { z0, deg, f0: f, shorted: is_shorted }, value: 0.0, q: None };
            let mut elements = vec![stub];
            if theta.to_degrees() > 1e-9 {
                elements.push(series);
            }
            candidates.push(named("Stub", elements));
        }
    }
    keep_valid(candidates, zl, z0, f)
}

/// A line of `z0` to the nearest point where the impedance is real, then a λ/4 section.
pub fn synth_quarter_wave(zl: C64, z0: f64, f: f64) -> Option<Network> {
    if !is_matchable(zl, z0, f) {
        return None;
    }
    let gamma = (zl - z0) / (zl + z0);
    let magnitude = gamma.norm();
    if magnitude >= 1.0 {
        return None;
    }
    // Moving towards the source by βd turns Γ by −2βd.
    let angle = if magnitude < 1e-12 { 0.0 } else { gamma.arg() };
    let to_maximum = (angle / 2.0).rem_euclid(PI);
    let to_minimum = ((angle + PI) / 2.0).rem_euclid(PI);
    let (theta, real_z) = if to_maximum <= to_minimum {
        (to_maximum, z0 * (1.0 + magnitude) / (1.0 - magnitude))
    } else {
        (to_minimum, z0 * (1.0 - magnitude) / (1.0 + magnitude))
    };
    let mut elements = vec![line((z0 * real_z).sqrt(), 90.0, f)];
    if theta.to_degrees() > 1e-9 {
        elements.push(line(z0, theta.to_degrees(), f));
    }
    let net = named("Quarter wave", elements);
    (reflection(&net, zl, z0, f).norm() < MAX_SYNTHESIS_ERROR).then_some(net)
}

type Abcd = [[C64; 2]; 2];

fn abcd_mul(a: &Abcd, b: &Abcd) -> Abcd {
    [
        [a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]],
        [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]],
    ]
}

fn series_abcd(z: C64) -> Abcd {
    let (one, zero) = (C64::new(1.0, 0.0), C64::new(0.0, 0.0));
    [[one, z], [zero, one]]
}

fn shunt_abcd(y: C64) -> Abcd {
    let (one, zero) = (C64::new(1.0, 0.0), C64::new(0.0, 0.0));
    [[one, zero], [y, one]]
}

fn inductor_z(l: f64, q: Option<f64>, w: f64) -> C64 {
    let x = w * l;
    C64::new(q.map_or(0.0, |q| x / q), x)
}

fn capacitor_z(c: f64, q: Option<f64>, w: f64) -> C64 {
    let x = 1.0 / (w * c).max(1e-30);
    C64::new(q.map_or(0.0, |q| x / q), -x)
}

/// `γℓ` of a line at `f`.
fn propagation(deg: f64, f0: f64, q: Option<f64>, f: f64) -> C64 {
    let theta = deg.to_radians() * f / f0.max(1e-30);
    C64::new(q.map_or(0.0, |q| theta / (2.0 * q)), theta)
}

/// Keeps a dead short or a dead open finite. `C64::tanh` is avoided altogether: it
/// divides by zero at an exact quarter wave.
fn nonzero(v: C64) -> C64 {
    if v.norm() < 1e-12 { C64::new(1e-12, 0.0) } else { v }
}

fn element_abcd(e: &Element, f: f64) -> Abcd {
    let w = angular(f);
    match e.kind {
        Kind::SeriesL => series_abcd(inductor_z(e.value, e.q, w)),
        Kind::SeriesC => series_abcd(capacitor_z(e.value, e.q, w)),
        Kind::ShuntL => shunt_abcd(nonzero(inductor_z(e.value, e.q, w)).inv()),
        Kind::ShuntC => shunt_abcd(capacitor_z(e.value, e.q, w).inv()),
        Kind::Line { z0, deg, f0 } => {
            let g = propagation(deg, f0, e.q, f);
            [[g.cosh(), g.sinh() * z0], [g.sinh() / z0, g.cosh()]]
        }
        Kind::Stub { z0, deg, f0, shorted } => {
            let g = propagation(deg, f0, e.q, f);
            let y = if shorted { g.cosh() / nonzero(g.sinh()) } else { g.sinh() / nonzero(g.cosh()) };
            shunt_abcd(y / z0)
        }
    }
}

fn chain(net: &Network, f: f64) -> Abcd {
    let (one, zero) = (C64::new(1.0, 0.0), C64::new(0.0, 0.0));
    net.elements.iter().fold([[one, zero], [zero, one]], |total, e| abcd_mul(&total, &element_abcd(e, f)))
}

/// Impedance seen at the source end with `zl` on the load end.
pub fn input_impedance(net: &Network, zl: C64, f: f64) -> C64 {
    let m = chain(net, f);
    (m[0][0] * zl + m[0][1]) / (m[1][0] * zl + m[1][1])
}

/// Reflection seen by a source of real impedance `z0`.
pub fn reflection(net: &Network, zl: C64, z0: f64, f: f64) -> C64 {
    let z = input_impedance(net, zl, f);
    (z - z0) / (z + z0)
}

/// Reflection at every frequency, with `zl[k]` the load at `f[k]`.
pub fn sweep(net: &Network, f: &[f64], zl: &[C64], z0: f64) -> Vec<C64> {
    f.iter().zip(zl).map(|(f, z)| reflection(net, *z, z0, *f)).collect()
}

/// Component series. The E6, E12, E24 and E96 preferred values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Series {
    E6,
    E12,
    E24,
    E96,
}

const E6: [f64; 6] = [1.0, 1.5, 2.2, 3.3, 4.7, 6.8];
const E12: [f64; 12] = [1.0, 1.2, 1.5, 1.8, 2.2, 2.7, 3.3, 3.9, 4.7, 5.6, 6.8, 8.2];
const E24: [f64; 24] =
    [1.0, 1.1, 1.2, 1.3, 1.5, 1.6, 1.8, 2.0, 2.2, 2.4, 2.7, 3.0, 3.3, 3.6, 3.9, 4.3, 4.7, 5.1, 5.6, 6.2, 6.8, 7.5, 8.2, 9.1];
const E96: [f64; 96] = [
    1.00, 1.02, 1.05, 1.07, 1.10, 1.13, 1.15, 1.18, 1.21, 1.24, 1.27, 1.30, 1.33, 1.37, 1.40, 1.43, 1.47, 1.50, 1.54, 1.58, 1.62, 1.65,
    1.69, 1.74, 1.78, 1.82, 1.87, 1.91, 1.96, 2.00, 2.05, 2.10, 2.15, 2.21, 2.26, 2.32, 2.37, 2.43, 2.49, 2.55, 2.61, 2.67, 2.74, 2.80,
    2.87, 2.94, 3.01, 3.09, 3.16, 3.24, 3.32, 3.40, 3.48, 3.57, 3.65, 3.74, 3.83, 3.92, 4.02, 4.12, 4.22, 4.32, 4.42, 4.53, 4.64, 4.75,
    4.87, 4.99, 5.11, 5.23, 5.36, 5.49, 5.62, 5.76, 5.90, 6.04, 6.19, 6.34, 6.49, 6.65, 6.81, 6.98, 7.15, 7.32, 7.50, 7.68, 7.87, 8.06,
    8.25, 8.45, 8.66, 8.87, 9.09, 9.31, 9.53, 9.76,
];

fn mantissas(s: Series) -> &'static [f64] {
    match s {
        Series::E6 => &E6,
        Series::E12 => &E12,
        Series::E24 => &E24,
        Series::E96 => &E96,
    }
}

/// The nearest preferred value on a logarithmic scale. A value that is not positive
/// and finite comes back unchanged.
pub fn snap(value: f64, s: Series) -> f64 {
    if value <= 0.0 || !value.is_finite() {
        return value;
    }
    let exponent = value.log10().floor() as i32;
    let mantissa = value / 10f64.powi(exponent);
    let mut best = 1.0;
    let mut least = f64::INFINITY;
    for candidate in mantissas(s).iter().copied().chain([10.0]) {
        let distance = (candidate / mantissa).ln().abs();
        if distance < least {
            least = distance;
            best = candidate;
        }
    }
    best * 10f64.powi(exponent)
}

/// The network with its lumped values on a series; lines and stubs are kept.
pub fn snapped(net: &Network, s: Series) -> Network {
    let elements = net
        .elements
        .iter()
        .map(|e| match e.kind {
            Kind::Line { .. } | Kind::Stub { .. } => *e,
            _ => Element { value: snap(e.value, s), ..*e },
        })
        .collect();
    Network { name: net.name.clone(), elements }
}

fn with_component_q(net: &Network, q_l: Option<f64>, q_c: Option<f64>) -> Network {
    let elements = net
        .elements
        .iter()
        .map(|e| match e.kind {
            Kind::SeriesL | Kind::ShuntL => Element { q: q_l, ..*e },
            Kind::SeriesC | Kind::ShuntC => Element { q: q_c, ..*e },
            _ => *e,
        })
        .collect();
    Network { name: net.name.clone(), elements }
}

fn worst_reflection(net: &Network, band: &[(f64, C64)], z0: f64) -> f64 {
    band.iter().map(|(f, z)| reflection(net, *z, z0, *f).norm()).fold(0.0, f64::max)
}

fn with_values(net: &Network, log_values: &[f64]) -> Network {
    let elements = net.elements.iter().zip(log_values).map(|(e, v)| Element { value: v.exp(), ..*e }).collect();
    Network { name: net.name.clone(), elements }
}

/// Broadband matching: starts from L, pi and T solutions at the band centre, then
/// refines the element values to minimise the worst `|Γ|` across the band, with
/// `crate::optimize` in log-value space within a factor of 30 of each seed. Returns
/// distinct candidates best-first, with their worst-case S11 in dB over the band samples
/// of `f`. Empty when no sample falls in the band or the load has no resistance at the
/// band centre. Candidates have at most three elements.
pub fn best_for_band(f: &[f64], zl: &[C64], z0: f64, f_lo: f64, f_hi: f64, q_l: Option<f64>, q_c: Option<f64>) -> Vec<(Network, f64)> {
    let band: Vec<(f64, C64)> = f.iter().copied().zip(zl.iter().copied()).filter(|(x, _)| (f_lo..=f_hi).contains(x)).collect();
    if band.is_empty() {
        return Vec::new();
    }
    let center = (f_lo * f_hi).sqrt();
    let nearest = band.iter().min_by(|a, b| (a.0 - center).abs().total_cmp(&(b.0 - center).abs())).map_or(band[0], |v| *v);
    let (fc, zc) = nearest;
    if !is_matchable(zc, z0, fc) {
        return Vec::new();
    }

    let sparse: Vec<(f64, C64)> = if band.len() > 41 {
        (0..41).map(|i| band[i * (band.len() - 1) / 40]).collect()
    } else {
        band.clone()
    };

    let parallel_r = 1.0 / zc.inv().re;
    let needed = |a: f64, b: f64| (a.max(b) / a.min(b) - 1.0).max(0.0).sqrt();
    let mut seeds = synth_l(zc, z0, fc);
    for factor in [1.05, 1.5, 2.5, 4.0] {
        seeds.extend(synth_pi(zc, z0, fc, (needed(z0, parallel_r) * factor).max(0.5 * factor)));
        seeds.extend(synth_t(zc, z0, fc, (needed(z0, zc.re) * factor).max(0.5 * factor)));
    }

    let refined: Vec<Network> = seeds
        .par_iter()
        .enumerate()
        .map(|(index, seed)| {
            let seed = with_component_q(seed, q_l, q_c);
            if seed.elements.is_empty() {
                return seed;
            }
            let start: Vec<f64> = seed.elements.iter().map(|e| e.value.ln()).collect();
            let reach = 30f64.ln();
            let bounds: Vec<(f64, f64)> = start.iter().map(|v| (v - reach, v + reach)).collect();
            let options = Options {
                method: Method::NelderMead,
                max_evals: 300 + 150 * start.len(),
                seed: index as u64,
                population: None,
                tol: 1e-7,
                parallel: 1,
            };
            let objective = |x: &[f64]| worst_reflection(&with_values(&seed, x), &sparse, z0);
            let out = minimize(objective, &start, &bounds, &options, |_| true);
            with_values(&seed, &out.x)
        })
        .collect();

    let mut scored: Vec<(Network, f64)> = refined
        .into_iter()
        .map(|net| {
            let worst = worst_reflection(&net, &band, z0);
            (net, worst)
        })
        .collect();
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut out: Vec<(Network, f64)> = Vec::new();
    for (net, worst) in scored {
        if !out.iter().any(|(kept, _)| same_values(kept, &net, 0.01)) {
            out.push((net, db20(worst)));
        }
    }
    out
}

/// Bode-Fano limit in Hz: the widest band over which any lossless network can hold
/// `|Γ| ≤ gamma_max` for a load of quality factor `q_load` at `f0`, modelled as series
/// or parallel RC, RL or RLC. All give `Δf = π·f0 / (Q·ln(1/Γ))` for a narrow band.
pub fn bode_fano_bandwidth(q_load: f64, f0: f64, gamma_max: f64) -> f64 {
    if gamma_max <= 0.0 {
        return 0.0;
    }
    if gamma_max >= 1.0 {
        return f64::INFINITY;
    }
    PI * f0 / (q_load * (1.0 / gamma_max).ln())
}

/// What a built network does at `f`: S11 in dB, and power reaching the antenna in dB
/// with its mismatch and finite-Q loss. Pass the snapped network to read standard parts.
pub fn as_built(net: &Network, zl: C64, z0: f64, f: f64) -> (f64, f64) {
    (crate::db20(reflection(net, zl, z0, f).norm()), efficiency_db(net, zl, z0, f))
}

/// Power delivered to the load over the source's available power, in dB (0 at best).
/// Includes mismatch and finite-Q loss.
pub fn efficiency_db(net: &Network, zl: C64, z0: f64, f: f64) -> f64 {
    let m = chain(net, f);
    // Unit load current: the source EMF is then V₁ + z0·I₁.
    let v1 = m[0][0] * zl + m[0][1];
    let i1 = m[1][0] * zl + m[1][1];
    let emf = v1 + i1 * z0;
    db10(4.0 * z0 * zl.re / emf.norm_sqr().max(1e-300))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linspace;

    const F: f64 = 1e9;
    const Z0: f64 = 50.0;

    fn c(re: f64, im: f64) -> C64 {
        C64::new(re, im)
    }

    fn loads() -> Vec<C64> {
        vec![
            c(10.0, -40.0),
            c(200.0, 100.0),
            c(50.0, 80.0),
            c(25.0, 0.0),
            c(100.0, 0.0),
            c(50.0, 0.0),
            c(5.0, 20.0),
            c(300.0, -400.0),
            c(1.0, 0.0),
            c(1000.0, 0.0),
            c(75.0, -30.0),
            c(30.0, 45.0),
        ]
    }

    /// Snapped parts are what get built, so their mismatch is in the power figure: an
    /// S11 of −10 dB costs 0.46 dB that the exact network wouldn't show.
    #[test]
    fn standard_parts_read_their_own_mismatch() {
        let zl = c(25.0, 30.0);
        let exact = synth_l(zl, Z0, F).swap_remove(0);
        let parts = snapped(&exact, Series::E6);
        let (s11, power) = as_built(&parts, zl, Z0, F);
        let lost = db10(1.0 - 10f64.powf(s11 / 10.0));
        assert!((power - lost).abs() < 1e-9, "{power} vs {lost}");
        assert!(power < efficiency_db(&exact, zl, Z0, F) - 1e-3, "{power}");
    }

    fn assert_all_match(nets: &[Network], zl: C64, what: &str) {
        for net in nets {
            let g = reflection(net, zl, Z0, F).norm();
            assert!(g < 1e-3, "{what} {} for {zl}: |Γ| = {g}", net.name);
        }
    }

    #[test]
    fn every_l_solution_matches() {
        // 50 + j80 has one network reached from both topologies: a lone series C.
        let want = [(c(10.0, -40.0), 4), (c(200.0, 100.0), 2), (c(50.0, 80.0), 2), (c(25.0, 0.0), 2), (c(100.0, 0.0), 2), (c(50.0, 0.0), 1)];
        for (zl, count) in want {
            let nets = synth_l(zl, Z0, F);
            assert_eq!(nets.len(), count, "{zl}: {nets:?}");
            assert_all_match(&nets, zl, "L");
        }
        let mut worst = 0.0f64;
        for zl in loads() {
            let nets = synth_l(zl, Z0, F);
            assert!(!nets.is_empty(), "{zl} has a solution");
            assert_all_match(&nets, zl, "L");
            for net in &nets {
                assert!(net.elements.len() <= 2);
                assert!(net.elements.iter().all(|e| e.value > 0.0));
                worst = worst.max(reflection(net, zl, Z0, F).norm());
            }
        }
        println!("L: worst |Γ| {worst:.2e}");
    }

    #[test]
    fn matched_load_is_a_direct_connection() {
        let nets = synth_l(c(Z0, 0.0), Z0, F);
        assert_eq!(nets.len(), 1);
        assert!(nets[0].elements.is_empty());
        assert_eq!(input_impedance(&nets[0], c(33.0, 7.0), F), c(33.0, 7.0));
    }

    #[test]
    fn unmatchable_loads_give_nothing() {
        for zl in [c(0.0, 50.0), c(-10.0, 5.0), c(f64::NAN, 0.0)] {
            assert!(synth_l(zl, Z0, F).is_empty());
            assert!(synth_pi(zl, Z0, F, 5.0).is_empty());
            assert!(synth_t(zl, Z0, F, 5.0).is_empty());
            assert!(synth_stub(zl, Z0, F).is_empty());
            assert!(synth_quarter_wave(zl, Z0, F).is_none());
        }
    }

    #[test]
    fn pi_and_t_solutions_match() {
        let mut worst = 0.0f64;
        for zl in loads() {
            for q in [8.0, 25.0] {
                let pi = synth_pi(zl, Z0, F, q);
                let t = synth_t(zl, Z0, F, q);
                assert!(!pi.is_empty() && !t.is_empty(), "{zl} q={q}");
                assert_all_match(&pi, zl, "Pi");
                assert_all_match(&t, zl, "T");
                for net in pi.iter().chain(&t) {
                    assert!(net.elements.iter().all(|e| e.value > 0.0), "{net:?}");
                    worst = worst.max(reflection(net, zl, Z0, F).norm());
                }
            }
        }
        println!("Pi/T: worst |Γ| {worst:.2e}");
    }

    #[test]
    fn pi_and_t_have_the_requested_q_and_a_minimum() {
        // 200 Ω to 50 Ω: q below √3 is impossible.
        assert!(synth_pi(c(200.0, 0.0), Z0, F, 1.5).is_empty());
        assert!(!synth_pi(c(200.0, 0.0), Z0, F, 1.8).is_empty());
        assert!(synth_t(c(200.0, 0.0), Z0, F, 1.5).is_empty());
        assert!(!synth_t(c(200.0, 0.0), Z0, F, 1.8).is_empty());
        // Four sign combinations, each a pi: shunt, series, shunt.
        let pi = synth_pi(c(200.0, 0.0), Z0, F, 5.0);
        assert_eq!(pi.len(), 4);
        for net in &pi {
            assert!(net.name.starts_with("Pi"));
            assert!(matches!(net.elements[0].kind, Kind::ShuntC | Kind::ShuntL));
            assert!(matches!(net.elements[1].kind, Kind::SeriesC | Kind::SeriesL));
            assert!(matches!(net.elements[2].kind, Kind::ShuntC | Kind::ShuntL));
        }
        let t = synth_t(c(200.0, 0.0), Z0, F, 5.0);
        assert_eq!(t.len(), 4);
        assert!(matches!(t[0].elements[0].kind, Kind::SeriesC | Kind::SeriesL));
        assert!(matches!(t[0].elements[1].kind, Kind::ShuntC | Kind::ShuntL));
    }

    #[test]
    fn stub_solutions_match() {
        let mut worst = 0.0f64;
        for zl in loads() {
            let nets = synth_stub(zl, Z0, F);
            assert!(!nets.is_empty(), "{zl}");
            assert_all_match(&nets, zl, "Stub");
            assert!(nets.iter().any(|n| matches!(n.elements[0].kind, Kind::Stub { shorted: true, .. })), "{zl}");
            assert!(nets.iter().any(|n| matches!(n.elements[0].kind, Kind::Stub { shorted: false, .. })), "{zl}");
            for net in &nets {
                worst = worst.max(reflection(net, zl, Z0, F).norm());
            }
            if (zl - c(Z0, 0.0)).norm() > 1e-6 {
                assert_eq!(nets.len(), 4, "two line lengths, two stub types: {zl}");
            }
        }
        println!("Stub: worst |Γ| {worst:.2e}");
    }

    #[test]
    fn stub_textbook_case() {
        // Pozar, Microwave Engineering, example 5.2: 15 + j10 on 50 Ω at 2 GHz.
        let nets = synth_stub(c(15.0, 10.0), Z0, 2e9);
        let series_deg: Vec<f64> = nets
            .iter()
            .filter_map(|n| match n.elements.last().unwrap().kind {
                Kind::Line { deg, .. } => Some(deg),
                _ => None,
            })
            .collect();
        // Line lengths d/λ = 0.044 and 0.387, i.e. 15.8° and 139.3°.
        assert!(series_deg.iter().any(|d| (d - 0.044 * 360.0).abs() < 1.0), "{series_deg:?}");
        assert!(series_deg.iter().any(|d| (d - 0.387 * 360.0).abs() < 1.0), "{series_deg:?}");
    }

    #[test]
    fn quarter_wave_matches() {
        let mut worst = 0.0f64;
        for zl in loads() {
            let net = synth_quarter_wave(zl, Z0, F).expect("a passive load");
            let g = reflection(&net, zl, Z0, F).norm();
            assert!(g < 1e-3, "{zl}: {g}");
            worst = worst.max(g);
            assert!(matches!(net.elements[0].kind, Kind::Line { deg, .. } if deg == 90.0));
        }
        println!("Quarter wave: worst |Γ| {worst:.2e}");
        let real = synth_quarter_wave(c(100.0, 0.0), Z0, F).unwrap();
        assert_eq!(real.elements.len(), 1);
        match real.elements[0].kind {
            Kind::Line { z0, .. } => assert!((z0 - 70.7107).abs() < 1e-3, "{z0}"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn known_l_values() {
        // 200 Ω to 50 Ω: Q = √3, series X = 86.6 Ω and shunt B = 8.66 mS at the load.
        let nets = synth_l(c(200.0, 0.0), Z0, F);
        let w = 2.0 * PI * F;
        let hit = nets.iter().find(|n| matches!(n.elements[0].kind, Kind::SeriesL)).unwrap();
        assert!((hit.elements[0].value * w - 86.6025).abs() < 1e-3);
        assert!(matches!(hit.elements[1].kind, Kind::ShuntC));
        assert!((hit.elements[1].value * w - 8.66025e-3).abs() < 1e-7);
    }

    fn mantissa(v: f64) -> f64 {
        v / 10f64.powi(v.log10().floor() as i32)
    }

    #[test]
    fn snapping_lands_on_the_series() {
        assert!((snap(4.9e-9, Series::E24) - 5.1e-9).abs() < 1e-20);
        assert!((snap(4.9e-9, Series::E6) - 4.7e-9).abs() < 1e-20);
        assert!((snap(4.9e-9, Series::E96) - 4.87e-9).abs() < 1e-20);
        assert!((snap(8.6e-12, Series::E12) - 8.2e-12).abs() < 1e-23);
        assert!((snap(9.9e-12, Series::E12) - 10e-12).abs() < 1e-23, "rolls into the next decade");
        assert_eq!(snap(0.0, Series::E6), 0.0);
        assert_eq!(snap(-1.0, Series::E6), -1.0);

        let net = synth_pi(c(10.0, -40.0), Z0, F, 6.0).remove(0);
        for (series, table) in [(Series::E6, &E6[..]), (Series::E12, &E12[..]), (Series::E24, &E24[..]), (Series::E96, &E96[..])] {
            let snapped_net = snapped(&net, series);
            assert_eq!(snapped_net.elements.len(), net.elements.len());
            for (a, b) in snapped_net.elements.iter().zip(&net.elements) {
                let m = mantissa(a.value * (1.0 + 1e-12));
                assert!(table.iter().chain([10.0].iter()).any(|t| (t - m).abs() < 1e-9), "{series:?}: {}", a.value);
                assert_eq!(a.kind, b.kind);
            }
        }
        let stub = synth_stub(c(10.0, -40.0), Z0, F).remove(0);
        assert_eq!(snapped(&stub, Series::E6), stub, "lines are left alone");
    }

    #[test]
    fn series_tables_are_the_standard_ones() {
        for (i, v) in E96.iter().enumerate() {
            let formula = (100.0 * 10f64.powf(i as f64 / 96.0)).round() / 100.0;
            assert!((v - formula).abs() < 1e-9, "E96[{i}] = {v}, formula {formula}");
        }
        assert!(E12.iter().all(|v| E24.contains(v)));
        assert!(E6.iter().all(|v| E12.contains(v)));
    }

    #[test]
    fn finite_q_matches_a_series_l_analytically() {
        let (l, q, rl) = (10e-9, 40.0, 30.0);
        let net = Network { name: "L".to_string(), elements: vec![Element { kind: Kind::SeriesL, value: l, q: Some(q) }] };
        let w = 2.0 * PI * F;
        let rs = w * l / q;
        let want = 10.0 * (4.0 * Z0 * rl / ((Z0 + rl + rs).powi(2) + (w * l).powi(2))).log10();
        let got = efficiency_db(&net, c(rl, 0.0), Z0, F);
        println!("series L, Q = 40: efficiency {got:.5} dB, analytic {want:.5} dB");
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        assert_eq!(input_impedance(&net, c(rl, 0.0), F), c(rl + rs, w * l));

        let ideal = Network { name: "L".to_string(), elements: vec![Element { q: None, ..net.elements[0] }] };
        assert!(efficiency_db(&ideal, c(rl, 0.0), Z0, F) > got);
    }

    #[test]
    fn finite_q_lowers_a_real_match() {
        let zl = c(10.0, -40.0);
        for net in synth_l(zl, Z0, F) {
            let ideal = efficiency_db(&net, zl, Z0, F);
            assert!(ideal.abs() < 1e-6, "a lossless match delivers everything: {ideal}");
            let lossy = with_component_q(&net, Some(50.0), Some(100.0));
            let loss = efficiency_db(&lossy, zl, Z0, F);
            println!("{}: finite-Q efficiency {loss:.3} dB", net.name);
            assert!(loss < -0.01, "{loss}");
        }
    }

    #[test]
    fn efficiency_follows_power_balance_in_a_shunt_case() {
        // Shunt C with ESR across a resistive load: compare against a hand division of current.
        let (cap, q, rl) = (4e-12, 25.0, 100.0);
        let net = Network { name: "C".to_string(), elements: vec![Element { kind: Kind::ShuntC, value: cap, q: Some(q) }] };
        let w = 2.0 * PI * F;
        let esr = 1.0 / (w * cap * q);
        let zc = c(esr, -1.0 / (w * cap));
        let total = (zc * rl) / (zc + rl);
        let got = efficiency_db(&net, c(rl, 0.0), Z0, F);
        // V_load / EMF = Z∥ / (Z∥ + Z0); the available power of an EMF E is E²/(8·Z0).
        let divider = (total / (total + Z0)).norm_sqr();
        let want = 10.0 * (divider / (2.0 * rl) * 8.0 * Z0).log10();
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
    }

    #[test]
    fn lines_and_stubs_behave_like_textbook_lines() {
        // λ/4 of 100 Ω into 200 Ω reads 50 Ω; a shorted λ/4 stub is invisible; an open λ/8 is capacitive.
        let quarter = Network { name: String::new(), elements: vec![line(100.0, 90.0, F)] };
        assert!((input_impedance(&quarter, c(200.0, 0.0), F) - c(50.0, 0.0)).norm() < 1e-9);
        // Half the design frequency: λ/8 electrically.
        let half = input_impedance(&quarter, c(100.0, 0.0), F / 2.0);
        assert!((half - c(100.0, 0.0)).norm() < 1e-9);

        let stub = |deg: f64, shorted: bool| Network {
            name: String::new(),
            elements: vec![Element { kind: Kind::Stub { z0: 50.0, deg, f0: F, shorted }, value: 0.0, q: None }],
        };
        assert!((input_impedance(&stub(90.0, true), c(75.0, 0.0), F) - c(75.0, 0.0)).norm() < 1e-9);
        let open = input_impedance(&stub(45.0, false), c(1e9, 0.0), F);
        assert!((open - c(0.0, -50.0)).norm() < 1e-3, "{open}");
        let shorted = input_impedance(&stub(45.0, true), c(1e9, 0.0), F);
        assert!((shorted - c(0.0, 50.0)).norm() < 1e-3, "{shorted}");

        let mut lossy = quarter.clone();
        lossy.elements[0].q = Some(20.0);
        assert!(efficiency_db(&lossy, c(100.0, 0.0), 100.0, F) < -0.1);
    }

    #[test]
    fn sweep_uses_each_frequency() {
        let f = linspace(0.9e9, 1.1e9, 5);
        let zl = vec![c(25.0, 0.0); 5];
        let net = synth_l(c(25.0, 0.0), Z0, F).remove(0);
        let g = sweep(&net, &f, &zl, Z0);
        assert_eq!(g.len(), 5);
        assert!(g[2].norm() < 1e-9);
        assert!(g[0].norm() > 1e-3);
        assert_eq!(g[4], reflection(&net, zl[4], Z0, f[4]));
    }

    fn resonant_load(r: f64, f0: f64, q: f64, f: &[f64]) -> Vec<C64> {
        let w0 = 2.0 * PI * f0;
        let l = q * r / w0;
        let cap = 1.0 / (w0 * w0 * l);
        f.iter()
            .map(|f| {
                let w = 2.0 * PI * f;
                c(r, w * l - 1.0 / (w * cap))
            })
            .collect()
    }

    #[test]
    fn best_for_band_beats_a_single_frequency_l() {
        let f = linspace(0.8e9, 1.2e9, 401);
        let (r, q) = (20.0, 12.0);
        let zl = resonant_load(r, F, q, &f);
        let (lo, hi) = (0.96e9, 1.04e9);

        let in_band: Vec<(f64, C64)> = f.iter().copied().zip(zl.iter().copied()).filter(|p| (lo..=hi).contains(&p.0)).collect();
        let centre = zl[200];
        let single = synth_l(centre, Z0, F)
            .iter()
            .map(|n| worst_reflection(n, &in_band, Z0))
            .fold(f64::INFINITY, f64::min);

        let start = std::time::Instant::now();
        let results = best_for_band(&f, &zl, Z0, lo, hi, None, None);
        let elapsed = start.elapsed();
        let (best, best_db) = &results[0];
        println!(
            "resonant load R={r} Q={q}, {:.0} MHz band: single-frequency L {:.2} dB, best {:.2} dB ({}, {} candidates, {elapsed:?})",
            (hi - lo) / 1e6,
            db20(single),
            best_db,
            best.name,
            results.len()
        );
        assert!(*best_db < db20(single) - 1.0, "{best_db} vs {}", db20(single));
        assert!(results.windows(2).all(|w| w[0].1 <= w[1].1), "best first");
        let again = worst_reflection(best, &in_band, Z0);
        assert!((db20(again) - best_db).abs() < 1e-9);

        // Physics cross-check: no lossless network beats Bode-Fano, at this band and level.
        let limit = bode_fano_bandwidth(q, F, 10f64.powf(best_db / 20.0));
        println!("Bode-Fano width at that level: {:.1} MHz vs band {:.1} MHz", limit / 1e6, (hi - lo) / 1e6);
        assert!(hi - lo <= limit, "{} > {limit}", hi - lo);
    }

    #[test]
    fn best_for_band_with_lossy_components_reports_lower_efficiency() {
        let f = linspace(0.9e9, 1.1e9, 81);
        let zl = resonant_load(20.0, F, 10.0, &f);
        let results = best_for_band(&f, &zl, Z0, 0.98e9, 1.02e9, Some(40.0), Some(100.0));
        let (best, _) = &results[0];
        assert!(best.elements.iter().all(|e| e.q.is_some()), "{best:?}");
        let eff = efficiency_db(best, zl[40], Z0, f[40]);
        assert!(eff < -0.01, "{eff}");
    }

    #[test]
    fn best_for_band_edge_cases() {
        let f = linspace(0.9e9, 1.1e9, 11);
        let zl = vec![c(25.0, 0.0); 11];
        assert!(best_for_band(&f, &zl, Z0, 2e9, 3e9, None, None).is_empty(), "no samples in the band");
        let purely_reactive = vec![c(0.0, 30.0); 11];
        assert!(best_for_band(&f, &purely_reactive, Z0, 0.9e9, 1.1e9, None, None).is_empty());
        let matched = vec![c(Z0, 0.0); 11];
        let out = best_for_band(&f, &matched, Z0, 0.9e9, 1.1e9, None, None);
        assert!(out[0].1 < -100.0, "{}", out[0].1);
    }

    #[test]
    fn bode_fano_against_the_textbook_formula() {
        // Pozar: ∫ln(1/|Γ|)dω = π·ω0/Q, so Δω·ln(1/Γm) = π·ω0/Q.
        let (q, f0, gm) = (10.0, 1e9, 0.1);
        let bw = bode_fano_bandwidth(q, f0, gm);
        let want = PI * f0 / (q * 10f64.ln());
        println!("Bode-Fano Q=10, -20 dB, 1 GHz: {:.2} MHz", bw / 1e6);
        assert!((bw - want).abs() < 1e-3);
        assert!((bw - 136.438e6).abs() < 1e4, "{bw}");
        assert_eq!(bode_fano_bandwidth(q, f0, 0.0), 0.0);
        assert_eq!(bode_fano_bandwidth(q, f0, 1.0), f64::INFINITY);
        assert!(bode_fano_bandwidth(2.0 * q, f0, gm) < bw);
    }
}
