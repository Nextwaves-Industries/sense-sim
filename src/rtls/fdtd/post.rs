//! openEMS results: port probes → S-parameters and input impedance, VTK
//! rectilinear-grid field dumps (`.vtr`, zlib-compressed), and the near- to
//! far-field transform (radiation pattern, directivity) from E/H dumps on a
//! closed box.

use std::f64::consts::PI;
use std::io::Read;
use std::ops::{Add, Div, Mul, Sub};
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;

use super::csx::C0;

const ETA0: f64 = 376.730_313_668;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub const ZERO: C = C { re: 0.0, im: 0.0 };
    pub fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
    pub fn polar(r: f64, a: f64) -> Self {
        Self { re: r * a.cos(), im: r * a.sin() }
    }
    pub fn norm2(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    pub fn abs(self) -> f64 {
        self.norm2().sqrt()
    }
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
    pub fn scale(self, k: f64) -> Self {
        Self { re: self.re * k, im: self.im * k }
    }
}

impl Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
}
impl Sub for C {
    type Output = C;
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
}
impl Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}
impl Div for C {
    type Output = C;
    fn div(self, o: C) -> C {
        let d = o.norm2().max(1e-300);
        C::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}

// ---------------------------------------------------------------------------
// Probes and S-parameters

/// A time series written by an openEMS probe (`port_ut1`, `port_it1`).
pub fn read_probe(path: &Path) -> Result<(Vec<f64>, Vec<f64>)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (mut t, mut v) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        let mut it = line.split_whitespace().map(|x| x.parse::<f64>());
        if let (Some(Ok(a)), Some(Ok(b))) = (it.next(), it.next()) {
            t.push(a);
            v.push(b);
        }
    }
    if t.len() < 4 {
        bail!("{} has no samples", path.display());
    }
    Ok((t, v))
}

/// Discrete Fourier transform of a pulse response at the given frequencies
/// (as openEMS' `DFT_time2freq`, so ratios match its tools).
pub fn dft(t: &[f64], v: &[f64], f: f64) -> C {
    let dt = (t[t.len() - 1] - t[0]) / (t.len() - 1) as f64;
    let mut s = C::ZERO;
    for (ti, vi) in t.iter().zip(v) {
        s = s + C::polar(*vi, -2.0 * PI * f * ti);
    }
    s.scale(2.0 * dt)
}

/// One port in the frequency domain: total voltage / current and the incident
/// and reflected voltage waves for reference impedance `z0`.
#[derive(Clone, Debug)]
pub struct PortFd {
    pub u: Vec<C>,
    pub i: Vec<C>,
    pub inc: Vec<C>,
    pub refl: Vec<C>,
}

pub fn port_fd(dir: &Path, nr: u32, z0: f64, freqs: &[f64]) -> Result<PortFd> {
    let (tu, u) = read_probe(&dir.join(format!("port_ut{nr}")))?;
    let (ti, i) = read_probe(&dir.join(format!("port_it{nr}")))?;
    let u: Vec<C> = freqs.iter().map(|f| dft(&tu, &u, *f)).collect();
    let i: Vec<C> = freqs.iter().map(|f| dft(&ti, &i, *f)).collect();
    let inc: Vec<C> = u.iter().zip(&i).map(|(u, i)| (*u + i.scale(z0)).scale(0.5)).collect();
    let refl: Vec<C> = u.iter().zip(&inc).map(|(u, a)| *u - *a).collect();
    Ok(PortFd { u, i, inc, refl })
}

/// Power the port delivered into the structure at each frequency, ½·Re(U·I*),
/// in the same spectral units as the frequency-domain field dumps.
pub fn port_accepted_power(dir: &Path, nr: u32, z0: f64, freqs: &[f64]) -> Result<Vec<f64>> {
    let p = port_fd(dir, nr, z0, freqs)?;
    Ok(p.u.iter().zip(&p.i).map(|(u, i)| (0.5 * (u.re * i.re + u.im * i.im)).abs()).collect())
}

/// S-parameters and input impedance over a frequency sweep.
#[derive(Clone, Debug, Default)]
pub struct Sweep {
    pub f: Vec<f64>,
    pub s11: Vec<C>,
    pub zin: Vec<C>,
    /// Present when the run had a second (passive) port.
    pub s21: Option<Vec<C>>,
}

impl Sweep {
    pub fn read(dir: &Path, z0: [f64; 2], two_port: bool, freqs: Vec<f64>) -> Result<Self> {
        let p1 = port_fd(dir, 1, z0[0], &freqs)?;
        let s11 = p1.refl.iter().zip(&p1.inc).map(|(r, a)| *r / *a).collect();
        let zin = p1.u.iter().zip(&p1.i).map(|(u, i)| *u / *i).collect();
        let s21 = if two_port {
            let p2 = port_fd(dir, 2, z0[1], &freqs)?;
            // Port 2 is passive: what arrives there is its reflected wave.
            Some(p2.refl.iter().zip(&p1.inc).map(|(b, a)| *b / *a).collect())
        } else {
            None
        };
        Ok(Self { f: freqs, s11, zin, s21 })
    }

    pub fn db(v: C) -> f64 {
        20.0 * v.abs().max(1e-12).log10()
    }

    /// Frequency of the deepest S11 and its value (dB).
    pub fn resonance(&self) -> Option<(f64, f64)> {
        self.f.iter().zip(&self.s11).map(|(f, s)| (*f, Self::db(*s))).min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Contiguous band around the resonance with S11 below `limit_db`.
    pub fn bandwidth(&self, limit_db: f64) -> Option<(f64, f64)> {
        let (fr, _) = self.resonance()?;
        let k = self.f.iter().position(|f| *f == fr)?;
        if Self::db(self.s11[k]) > limit_db {
            return None;
        }
        let mut lo = k;
        while lo > 0 && Self::db(self.s11[lo - 1]) <= limit_db {
            lo -= 1;
        }
        let mut hi = k;
        while hi + 1 < self.f.len() && Self::db(self.s11[hi + 1]) <= limit_db {
            hi += 1;
        }
        Some((self.f[lo], self.f[hi]))
    }

    pub fn vswr(s11: C) -> f64 {
        let g = s11.abs().min(0.999_999);
        (1.0 + g) / (1.0 - g)
    }

    /// Group delay of S21 (s) by finite differences of its unwrapped phase.
    pub fn group_delay(&self) -> Option<Vec<f64>> {
        let s21 = self.s21.as_ref()?;
        let mut phase: Vec<f64> = s21.iter().map(|c| c.arg()).collect();
        for k in 1..phase.len() {
            while phase[k] - phase[k - 1] > PI {
                phase[k] -= 2.0 * PI;
            }
            while phase[k] - phase[k - 1] < -PI {
                phase[k] += 2.0 * PI;
            }
        }
        Some(
            (0..phase.len())
                .map(|k| {
                    let (a, b) = (k.saturating_sub(1), (k + 1).min(phase.len() - 1));
                    if a == b {
                        return 0.0;
                    }
                    -(phase[b] - phase[a]) / (2.0 * PI * (self.f[b] - self.f[a]))
                })
                .collect(),
        )
    }
}

pub fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    (0..n).map(|k| a + (b - a) * k as f64 / (n - 1).max(1) as f64).collect()
}

// ---------------------------------------------------------------------------
// VTK rectilinear grid (.vtr)

/// A field on a rectilinear grid: point `(i, j, k)` is `x[i], y[j], z[k]`,
/// `data[(i + nx·(j + ny·k))·ncomp + c]`.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub z: Vec<f64>,
    pub ncomp: usize,
    pub data: Vec<f32>,
}

impl Grid {
    pub fn len(&self) -> usize {
        self.x.len() * self.y.len() * self.z.len()
    }
    pub fn at(&self, p: usize) -> &[f32] {
        &self.data[p * self.ncomp..(p + 1) * self.ncomp]
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')? + i;
    Some(&tag[i..j])
}

/// Decodes a `format="binary"` data array: base64 header + (zlib) blocks.
fn decode_array(text: &str, compressed: bool) -> Result<Vec<u8>> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !compressed {
        let raw = b64.decode(&text)?;
        let n = u32::from_le_bytes(raw.get(0..4).context("short array")?.try_into()?) as usize;
        return Ok(raw.get(4..4 + n).context("short array")?.to_vec());
    }
    // Header: [blocks, block size, last block size, compressed size × blocks] as u32,
    // base64-encoded on its own (so it ends with its own padding).
    let first = b64.decode(text.get(0..16).context("short header")?)?;
    let blocks = u32::from_le_bytes(first[0..4].try_into()?) as usize;
    let header_bytes = 4 * (3 + blocks);
    let header_chars = header_bytes.div_ceil(3) * 4;
    let header = b64.decode(text.get(0..header_chars).context("short header")?)?;
    let sizes: Vec<usize> = (0..blocks).map(|b| u32::from_le_bytes(header[12 + 4 * b..16 + 4 * b].try_into().unwrap()) as usize).collect();
    let data = b64.decode(&text[header_chars..])?;
    let mut out = Vec::new();
    let mut at = 0;
    for s in sizes {
        let mut z = flate2::read::ZlibDecoder::new(data.get(at..at + s).context("short block")?);
        z.read_to_end(&mut out)?;
        at += s;
    }
    Ok(out)
}

fn to_f64(bytes: &[u8], ty: &str) -> Vec<f64> {
    match ty {
        "Float64" => bytes.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect(),
        _ => bytes.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap()) as f64).collect(),
    }
}

pub fn read_vtr(path: &Path) -> Result<Grid> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let compressed = text.contains("vtkZLibDataCompressor");
    let mut arrays: Vec<(String, String, usize, Vec<f64>)> = Vec::new(); // (name, type, ncomp, values)
    let mut rest = text.as_str();
    while let Some(i) = rest.find("<DataArray") {
        let tag_end = rest[i..].find('>').context("bad DataArray")? + i;
        let tag = &rest[i..tag_end];
        let close = rest[tag_end..].find("</DataArray>").context("unclosed DataArray")? + tag_end;
        // Only the base64 text, not the InformationKey children openEMS adds.
        let body = &rest[tag_end + 1..close];
        let body = body.split('<').next().unwrap_or("");
        let ty = attr(tag, "type").unwrap_or("Float32").to_string();
        let ncomp = attr(tag, "NumberOfComponents").and_then(|n| n.parse().ok()).unwrap_or(1);
        let bytes = decode_array(body, compressed)?;
        arrays.push((attr(tag, "Name").unwrap_or("").to_string(), ty.clone(), ncomp, to_f64(&bytes, &ty)));
        rest = &rest[close + 12..];
    }
    // PointData first (the field), then the three coordinate arrays.
    if arrays.len() < 4 {
        bail!("{}: expected a field and 3 coordinate arrays", path.display());
    }
    let n = arrays.len();
    let (x, y, z) = (arrays[n - 3].3.clone(), arrays[n - 2].3.clone(), arrays[n - 1].3.clone());
    let (_, _, ncomp, field) = &arrays[0];
    let g = Grid { x, y, z, ncomp: *ncomp, data: field.iter().map(|v| *v as f32).collect() };
    if g.data.len() != g.len() * g.ncomp {
        bail!("{}: {} values for a {}x{}x{} grid", path.display(), g.data.len(), g.x.len(), g.y.len(), g.z.len());
    }
    Ok(g)
}

/// The `f=` token openEMS used for the dump of `name` closest to `f`. Its
/// formatting varies (`6500000000.000000`, `915000000`), so look it up.
fn fd_token(dir: &Path, name: &str, f: f64) -> Option<String> {
    let prefix = format!("{name}_f=");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            let rest = n.strip_prefix(&prefix)?.strip_suffix("_abs.vtr")?.to_string();
            let hz: f64 = rest.parse().ok()?;
            Some((rest, (hz - f).abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|x| x.0)
}

/// openEMS names frequency-domain dumps `<name>_f=<Hz>_abs.vtr` / `_arg.vtr` / `_p=NNN.vtr`.
pub fn fd_file(dir: &Path, name: &str, f: f64, part: &str) -> std::path::PathBuf {
    let token = fd_token(dir, name, f).unwrap_or_else(|| format!("{f:.6}"));
    dir.join(format!("{name}_f={token}_{part}.vtr"))
}

/// Complex vector field of a frequency-domain dump (abs + arg files).
pub fn read_fd_field(dir: &Path, name: &str, f: f64) -> Result<(Grid, Vec<[C; 3]>)> {
    let abs = read_vtr(&fd_file(dir, name, f, "abs"))?;
    let arg = read_vtr(&fd_file(dir, name, f, "arg"))?;
    if abs.ncomp != 3 || arg.data.len() != abs.data.len() {
        bail!("{name}: unexpected dump layout");
    }
    let v = (0..abs.len())
        .map(|p| {
            let (a, g) = (abs.at(p), arg.at(p));
            [C::polar(a[0] as f64, g[0] as f64), C::polar(a[1] as f64, g[1] as f64), C::polar(a[2] as f64, g[2] as f64)]
        })
        .collect();
    Ok((abs, v))
}

/// The animation frames openEMS writes for a frequency dump (`_p=000.vtr`…), sorted.
pub fn phase_frames(dir: &Path, name: &str, f: f64) -> Vec<std::path::PathBuf> {
    let Some(token) = fd_token(dir, name, f) else { return Vec::new() };
    let prefix = format!("{name}_f={token}_p=");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// Near field → far field

/// Face names of the NF2FF box, in openEMS' order, with outward normals.
pub const FACES: [(&str, usize, f64); 6] = [("xn", 0, -1.0), ("xp", 0, 1.0), ("yn", 1, -1.0), ("yp", 1, 1.0), ("zn", 2, -1.0), ("zp", 2, 1.0)];

/// Equivalent surface currents of one face: positions, J = n×H, M = -n×E, area.
struct Surface {
    pts: Vec<([f64; 3], [C; 3], [C; 3], f64)>,
}

fn cross_n(axis: usize, sign: f64, v: [C; 3]) -> [C; 3] {
    // n × v with n = sign · e_axis.
    let mut n = [0.0; 3];
    n[axis] = sign;
    [
        v[2].scale(n[1]) - v[1].scale(n[2]),
        v[0].scale(n[2]) - v[2].scale(n[0]),
        v[1].scale(n[0]) - v[0].scale(n[1]),
    ]
}

fn trapezoid(c: &[f64], i: usize) -> f64 {
    if c.len() < 2 {
        return 1.0;
    }
    let l = if i > 0 { c[i] - c[i - 1] } else { 0.0 };
    let r = if i + 1 < c.len() { c[i + 1] - c[i] } else { 0.0 };
    (l + r) * 0.5
}

fn surface(e: &(Grid, Vec<[C; 3]>), h: &(Grid, Vec<[C; 3]>), axis: usize, sign: f64) -> Surface {
    let g = &e.0;
    let mut pts = Vec::with_capacity(g.len());
    for k in 0..g.z.len() {
        for j in 0..g.y.len() {
            for i in 0..g.x.len() {
                let p = i + g.x.len() * (j + g.y.len() * k);
                let w = [trapezoid(&g.x, i), trapezoid(&g.y, j), trapezoid(&g.z, k)];
                let area = (0..3).filter(|a| *a != axis).map(|a| w[a]).product::<f64>();
                let jv = cross_n(axis, sign, h.1[p]);
                let mv = cross_n(axis, sign, e.1[p]).map(|c| c.scale(-1.0));
                pts.push(([g.x[i], g.y[j], g.z[k]], jv, mv, area));
            }
        }
    }
    Surface { pts }
}

/// Radiation pattern on a θ/φ grid (degrees), as directivity in dBi.
#[derive(Clone, Debug, Default)]
pub struct Pattern {
    pub f: f64,
    pub theta: Vec<f64>,
    pub phi: Vec<f64>,
    /// `d[ti][pi]`, dBi.
    pub d: Vec<Vec<f64>>,
    pub max_dbi: f64,
    pub max_dir: (f64, f64),
}

/// Far field from E/H frequency dumps named `<name>_E_<face>` / `<name>_H_<face>`.
pub fn nf2ff(dir: &Path, name: &str, f: f64, step_deg: f64, center: [f64; 3]) -> Result<Pattern> {
    let mut faces = Vec::new();
    for (face, axis, sign) in FACES {
        let e = read_fd_field(dir, &format!("{name}_E_{face}"), f)?;
        let h = read_fd_field(dir, &format!("{name}_H_{face}"), f)?;
        faces.push(surface(&e, &h, axis, sign));
    }
    let k = 2.0 * PI * f / C0;
    let theta: Vec<f64> = (0..=(180.0 / step_deg).round() as usize).map(|i| i as f64 * step_deg).collect();
    let phi: Vec<f64> = (0..=(360.0 / step_deg).round() as usize).map(|i| i as f64 * step_deg).collect();
    use rayon::prelude::*;
    let u: Vec<Vec<f64>> = theta
        .par_iter()
        .map(|th| {
        let mut row = vec![0.0; phi.len()];
        let (st, ct) = th.to_radians().sin_cos();
        for (pi, ph) in phi.iter().enumerate() {
            let (sp, cp) = ph.to_radians().sin_cos();
            let r = [st * cp, st * sp, ct];
            let mut n = [C::ZERO; 3];
            let mut l = [C::ZERO; 3];
            for s in &faces {
                for (p, jv, mv, a) in &s.pts {
                    let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
                    let ph = C::polar(*a, k * (d[0] * r[0] + d[1] * r[1] + d[2] * r[2]));
                    for c in 0..3 {
                        n[c] = n[c] + jv[c] * ph;
                        l[c] = l[c] + mv[c] * ph;
                    }
                }
            }
            let tv = [ct * cp, ct * sp, -st];
            let pv = [-sp, cp, 0.0];
            let dot = |v: &[C; 3], u: [f64; 3]| v[0].scale(u[0]) + v[1].scale(u[1]) + v[2].scale(u[2]);
            let (nt, np, lt, lp) = (dot(&n, tv), dot(&n, pv), dot(&l, tv), dot(&l, pv));
            row[pi] = (lp + nt.scale(ETA0)).norm2() + (lt - np.scale(ETA0)).norm2();
        }
        row
        })
        .collect();
    // Directivity: 4π U / ∫U dΩ (common factors cancel).
    let dth = step_deg.to_radians();
    let mut prad = 0.0;
    for (ti, th) in theta.iter().enumerate() {
        let w = th.to_radians().sin() * dth * dth;
        // The φ = 360° column repeats φ = 0°.
        for v in &u[ti][..phi.len() - 1] {
            prad += v * w;
        }
    }
    let prad = prad.max(1e-300);
    let mut d = vec![vec![0.0; phi.len()]; theta.len()];
    let (mut best, mut at) = (f64::MIN, (0.0, 0.0));
    for ti in 0..theta.len() {
        for pi in 0..phi.len() {
            let v = 10.0 * (4.0 * PI * u[ti][pi] / prad).max(1e-12).log10();
            d[ti][pi] = v;
            if v > best {
                best = v;
                at = (theta[ti], phi[pi]);
            }
        }
    }
    Ok(Pattern { f, theta, phi, d, max_dbi: best, max_dir: at })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complex_ops() {
        let a = C::new(3.0, 4.0);
        assert_eq!(a.abs(), 5.0);
        let q = a / a;
        assert!((q.re - 1.0).abs() < 1e-12 && q.im.abs() < 1e-12);
        assert!((Sweep::vswr(C::new(0.0, 0.0)) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn dft_of_a_delayed_pulse_has_linear_phase() {
        let t: Vec<f64> = (0..2000).map(|k| k as f64 * 1e-12).collect();
        let v: Vec<f64> = t.iter().map(|t| (-((t - 0.5e-9) / 0.05e-9).powi(2)).exp()).collect();
        let a = dft(&t, &v, 1e9);
        let b = dft(&t, &v, 2e9);
        // Delay 0.5 ns: phase -2π f τ.
        let pa = -2.0 * PI * 1e9 * 0.5e-9;
        let pb = -2.0 * PI * 2e9 * 0.5e-9;
        let wrap = |x: f64| (x + PI).rem_euclid(2.0 * PI) - PI;
        assert!((wrap(a.arg() - pa)).abs() < 1e-3);
        assert!((wrap(b.arg() - pb)).abs() < 1e-3);
    }

    #[test]
    fn decodes_uncompressed_array() {
        let mut raw = (8u32).to_le_bytes().to_vec();
        raw.extend(1.5f32.to_le_bytes());
        raw.extend((-2.0f32).to_le_bytes());
        let text = base64::engine::general_purpose::STANDARD.encode(&raw);
        let bytes = decode_array(&text, false).unwrap();
        assert_eq!(to_f64(&bytes, "Float32"), vec![1.5, -2.0]);
    }
}
