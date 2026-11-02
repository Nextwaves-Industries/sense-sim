//! Charts for the report, as inline SVG so one file carries everything and prints
//! sharply. Each chart also carries its data in `data-hover` for the page's readout;
//! the page's tables hold the same numbers for anyone who can't hover.

use std::fmt::Write;

use super::esc;
use crate::C64;

/// Series colours, always taken in this order. Checked with the dataviz validator on white:
/// adjacent pairs clear the colour-blind and normal-vision floors. Three fall under 3:1
/// against the page, so every chart has a table with the same numbers.
pub const PALETTE: [&str; 8] = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#6250d6", "#e34948"];
/// Whatever doesn't get a colour of its own past the eighth series.
pub const OTHER: &str = "#c3c2b7";
const LIMIT: &str = "#d03b3b";

/// The colour of the `k`th series.
pub fn color(k: usize) -> &'static str {
    PALETTE.get(k).copied().unwrap_or(OTHER)
}

pub struct Line {
    pub name: String,
    pub color: &'static str,
    pub points: Vec<(f64, f64)>,
}

impl Line {
    pub fn new(name: impl Into<String>, k: usize, points: Vec<(f64, f64)>) -> Self {
        Self { name: name.into(), color: color(k), points }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Opts<'a> {
    pub title: &'a str,
    pub x_label: &'a str,
    /// Follows a value, with its space: " dB".
    pub y_unit: &'a str,
    /// A level that matters, and what to call it.
    pub limit: Option<(f64, &'a str)>,
    /// A place along x that matters, and what to call it.
    pub marker: Option<(f64, &'a str)>,
    /// Fixed y range. Values beyond it run along the edge.
    pub y_range: Option<(f64, f64)>,
    /// Half a page wide, for a pair side by side.
    pub half: bool,
}

/// Round tick values across a range, and the decimals they need.
pub fn nice_ticks(lo: f64, hi: f64, target: usize) -> (Vec<f64>, usize) {
    let span = hi - lo;
    if !(span > 0.0) || !span.is_finite() {
        return (vec![lo], 2);
    }
    let raw = span / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag);
    let decimals = (-step.log10().floor()).max(0.0) as usize;
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    ((first..=last).map(|k| k as f64 * step).collect(), decimals)
}

/// Text for an attribute holding JSON. The JSON's own quotes can stay as they are, since
/// the attribute is delimited by `'`.
fn attr_json(json: &str) -> String {
    json.replace('&', "&amp;").replace('\'', "&#39;").replace('<', "&lt;").replace('>', "&gt;")
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The minus sign a reader expects.
pub fn minus(s: String) -> String {
    s.replace('-', "−")
}

pub fn line_chart(o: &Opts, series: &[Line]) -> String {
    let (w, h, l) = if o.half { (440.0, 256.0, 50.0) } else { (900.0, 300.0, 56.0) };
    let b = 44.0;
    let clean: Vec<Vec<(f64, f64)>> = series.iter().map(|s| s.points.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()).collect()).collect();
    let all: Vec<(f64, f64)> = clean.iter().flatten().copied().collect();
    if all.len() < 2 {
        return String::new();
    }
    let (x0, x1) = all.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (y0, y1) = match o.y_range {
        Some(r) => r,
        None => {
            let (mut lo, mut hi) = all.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
            if let Some((v, _)) = o.limit {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let pad = if hi > lo { (hi - lo) * 0.06 } else { 0.5 };
            (lo - pad, hi + pad)
        }
    };

    // The legend, wrapped into rows. One series needs none: the title names it.
    let folded = series.iter().filter(|s| s.color == OTHER).count();
    let mut entries: Vec<(String, &str)> = series.iter().filter(|s| s.color != OTHER).map(|s| (s.name.clone(), s.color)).collect();
    if folded > 0 {
        entries.push((format!("Other ({folded})"), OTHER));
    }
    if series.len() < 2 {
        entries.clear();
    }
    let room = w - l - 16.0;
    let mut rows: Vec<Vec<(f64, usize)>> = Vec::new();
    let mut at = 0.0;
    for (k, (name, _)) in entries.iter().enumerate() {
        let need = 24.0 + name.chars().count() as f64 * 6.2;
        if rows.is_empty() || at + need > room {
            rows.push(Vec::new());
            at = 0.0;
        }
        if let Some(row) = rows.last_mut() {
            row.push((at, k));
        }
        at += need;
    }
    let t = 28.0 + rows.len() as f64 * 16.0 + if rows.is_empty() { 0.0 } else { 6.0 };
    let sy = |y: f64| h - b - ((y - y0) / (y1 - y0).max(1e-30)).clamp(0.0, 1.0) * (h - t - b);

    // Up to four lines are also named at their ends, unless the names would collide.
    let mut ends: Vec<(f64, String)> = Vec::new();
    if (2..=4).contains(&series.len()) {
        for (s, c) in series.iter().zip(&clean) {
            if let Some(p) = c.last() {
                ends.push((sy(p.1), s.name.clone()));
            }
        }
        let mut ys: Vec<f64> = ends.iter().map(|e| e.0).collect();
        ys.sort_by(f64::total_cmp);
        if ys.windows(2).any(|w| w[1] - w[0] < 12.0) || ends.iter().any(|e| e.1.chars().count() > 8) {
            ends.clear();
        }
    }
    let r = if ends.is_empty() { 16.0 } else { 16.0 + ends.iter().map(|e| e.1.chars().count() as f64 * 6.2).fold(0.0, f64::max) };
    let sx = |x: f64| l + (x - x0) / (x1 - x0).max(1e-30) * (w - l - r);

    // What the readout needs.
    let mut data = format!("{{\"m\":[{l},{},{t},{}],\"xr\":[{x0},{x1}],\"xl\":{},\"u\":{},\"s\":[", w - r, h - b, js_str(o.x_label), js_str(o.y_unit));
    for (k, (s, c)) in series.iter().zip(&clean).enumerate() {
        if k > 0 {
            data.push(',');
        }
        let ys: Vec<String> = c.iter().map(|p| format!("{:.3}", p.1)).collect();
        let _ = write!(data, "{{\"n\":{},\"c\":\"{}\",", js_str(&s.name), s.color);
        let same = k > 0 && c.len() == clean[0].len() && c.iter().zip(&clean[0]).all(|(a, b)| a.0 == b.0);
        if !same {
            let xs: Vec<String> = c.iter().map(|p| format!("{:.6}", p.0)).collect();
            let _ = write!(data, "\"x\":[{}],", xs.join(","));
        }
        let _ = write!(data, "\"y\":[{}]}}", ys.join(","));
    }
    data.push_str("]}");

    let mut s = format!(r#"<svg class="chart{}" viewBox="0 0 {w} {h}" role="img" tabindex="0" aria-label="{}" data-hover='{}'>"#, if o.half { " half" } else { "" }, esc(o.title), attr_json(&data));
    let _ = write!(s, r#"<text x="{l}" y="18" class="t">{}</text>"#, esc(o.title));
    for (i, row) in rows.iter().enumerate() {
        let y = 34.0 + i as f64 * 16.0;
        for (px, k) in row {
            let (name, color) = &entries[*k];
            let x = l + px;
            let _ = write!(s, r#"<line x1="{x:.1}" x2="{:.1}" y1="{:.1}" y2="{:.1}" stroke="{color}" stroke-width="2" stroke-linecap="round"/><text x="{:.1}" y="{:.1}" class="a">{}</text>"#, x + 14.0, y - 4.0, y - 4.0, x + 19.0, y, esc(name));
        }
    }
    let (yticks, yd) = nice_ticks(y0, y1, 5);
    for y in yticks {
        let (x2, yy, xt, yt) = (w - r, sy(y), l - 8.0, sy(y) + 4.0);
        let _ = write!(s, r#"<line x1="{l}" x2="{x2}" y1="{yy:.1}" y2="{yy:.1}" class="g"/><text x="{xt}" y="{yt:.1}" class="a" text-anchor="end">{}</text>"#, minus(format!("{y:.yd$}")));
    }
    let _ = write!(s, r#"<line x1="{l}" x2="{}" y1="{1}" y2="{1}" class="base"/>"#, w - r, h - b);
    let (xticks, xd) = nice_ticks(x0, x1, if o.half { 4 } else { 8 });
    for x in xticks {
        let _ = write!(s, r#"<text x="{:.1}" y="{}" class="a" text-anchor="middle">{}</text>"#, sx(x), h - b + 16.0, minus(format!("{x:.xd$}")));
    }
    let _ = write!(s, r#"<text x="{}" y="{}" class="a" text-anchor="middle">{}</text>"#, (l + w - r) / 2.0, h - 8.0, esc(o.x_label));
    if let Some((x, name)) = o.marker
        && x >= x0
        && x <= x1
    {
        let _ = write!(s, r#"<line x1="{0:.1}" x2="{0:.1}" y1="{t}" y2="{1}" class="mark"/><text x="{2:.1}" y="{3}" class="a">{4}</text>"#, sx(x), h - b, sx(x) + 4.0, t + 11.0, esc(name));
    }
    if let Some((v, name)) = o.limit {
        let _ = write!(s, r#"<line x1="{l}" x2="{}" y1="{1:.1}" y2="{1:.1}" stroke="{LIMIT}" stroke-dasharray="5 4"/><text x="{0}" y="{2:.1}" class="a" text-anchor="end">{3}</text>"#, w - r, sy(v), sy(v) - 4.0, esc(name));
    }
    // Folded series go under the ones that have a colour.
    let order = series.iter().zip(&clean).filter(|(l, _)| l.color == OTHER).chain(series.iter().zip(&clean).filter(|(l, _)| l.color != OTHER));
    for (line, c) in order {
        let d: Vec<String> = c.iter().map(|p| format!("{:.1},{:.1}", sx(p.0), sy(p.1))).collect();
        let width = if line.color == OTHER { 1.2 } else { 2.0 };
        let _ = write!(s, r#"<polyline fill="none" stroke="{}" stroke-width="{width}" stroke-linejoin="round" stroke-linecap="round" points="{}"><title>{}</title></polyline>"#, line.color, d.join(" "), esc(&line.name));
    }
    for (y, name) in &ends {
        let _ = write!(s, r#"<text x="{}" y="{:.1}" class="a">{}</text>"#, w - r + 5.0, y + 4.0, esc(name));
    }
    s.push_str("</svg>");
    s
}

/// A polar cut: 0° at the top, angles running clockwise, and the outer ring at `peak`.
/// `signed` labels the angles −150…180° for a plane through the z axis.
pub fn polar_chart(title: &str, lines: &[Line], peak: f64, signed: bool, unit: &str) -> String {
    let (w, cx, cy, r) = (340.0, 170.0, 186.0, 122.0);
    let floor = peak - 30.0;
    let h = if lines.len() > 1 { 352.0 } else { 334.0 };
    let place = |ang: f64, db: f64| {
        let rr = ((db - floor) / (peak - floor)).clamp(0.0, 1.0) * r;
        let t = ang.to_radians();
        (cx + rr * t.sin(), cy - rr * t.cos())
    };
    let mut pts = Vec::new();
    for line in lines {
        for (ang, db) in line.points.iter().filter(|p| p.0.is_finite() && p.1.is_finite()) {
            let (x, y) = place(*ang, *db);
            pts.push(format!("[{x:.1},{y:.1},{},{},\"{}\"]", js_str(&minus(format!("{db:.1}{unit}"))), js_str(&minus(format!("{} · {ang:.0}°", line.name))), line.color));
        }
    }
    let data = format!("{{\"pts\":[{}]}}", pts.join(","));
    let mut s = format!(r#"<svg class="polar" viewBox="0 0 {w} {h}" role="img" tabindex="0" aria-label="{}" data-hover='{}'>"#, esc(title), attr_json(&data));
    let _ = write!(s, r#"<text x="{cx}" y="18" class="t" text-anchor="middle">{}</text>"#, esc(title));
    for k in 1..=3 {
        let rr = r * k as f64 / 3.0;
        let _ = write!(s, r#"<circle cx="{cx}" cy="{cy}" r="{rr:.1}" class="g" fill="none"/>"#);
    }
    for a in (0..360).step_by(30) {
        let t = (a as f64).to_radians();
        let _ = write!(s, r#"<line x1="{cx}" y1="{cy}" x2="{:.1}" y2="{:.1}" class="g"/>"#, cx + r * t.sin(), cy - r * t.cos());
        let shown = if signed && a > 180 { a - 360 } else { a };
        let anchor = if a == 0 || a == 180 { "middle" } else if a < 180 { "start" } else { "end" };
        let _ = write!(s, r#"<text x="{:.1}" y="{:.1}" class="a" text-anchor="{anchor}">{}°</text>"#, cx + (r + 7.0) * t.sin(), cy - (r + 7.0) * t.cos() + 4.0, minus(shown.to_string()));
    }
    for k in 0..3 {
        let db = peak - 10.0 * k as f64;
        let rr = r * (3 - k) as f64 / 3.0;
        let _ = write!(s, r#"<text x="{:.1}" y="{:.1}" class="ring">{}</text>"#, cx + 3.0, cy - rr + 11.0, minus(format!("{db:.1}")));
    }
    for line in lines {
        let d: Vec<String> = line
            .points
            .iter()
            .filter(|p| p.0.is_finite() && !p.1.is_nan())
            .map(|p| {
                let (x, y) = place(p.0, p.1);
                format!("{x:.1},{y:.1}")
            })
            .collect();
        let _ = write!(s, r#"<polyline fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round" points="{}"><title>{}</title></polyline>"#, line.color, d.join(" "), esc(&line.name));
    }
    if lines.len() > 1 {
        let mut x = cx - lines.iter().map(|l| 22.0 + l.name.chars().count() as f64 * 6.2).sum::<f64>() / 2.0;
        for line in lines {
            let _ = write!(s, r#"<line x1="{x:.1}" x2="{:.1}" y1="{1}" y2="{1}" stroke="{2}" stroke-width="2" stroke-linecap="round"/><text x="{3:.1}" y="{4}" class="a">{5}</text>"#, x + 14.0, h - 8.0, line.color, x + 18.0, h - 4.0, esc(&line.name));
            x += 22.0 + line.name.chars().count() as f64 * 6.2;
        }
    }
    s.push_str("</svg>");
    s
}

/// One port's curve on a Smith chart.
pub struct SmithCurve<'a> {
    pub name: &'a str,
    pub color: &'static str,
    pub gamma: &'a [C64],
    /// What this port's reflection is against.
    pub z0: f64,
}

/// The curves of `gamma`, with `labels[k]` naming each sample's frequency and `mark` the
/// sample of the design frequency. Its resistance circles are labelled for the first port.
pub fn smith_chart(title: &str, curves: &[SmithCurve], labels: &[String], mark: Option<usize>) -> String {
    let z0 = curves.first().map_or(50.0, |c| c.z0);
    let (size, c, r) = (440.0, 220.0, 196.0);
    let height = if curves.len() > 1 { size + 18.0 } else { size };
    let place = |g: &C64| (c + g.re * r, c - g.im * r);
    let mut pts = Vec::new();
    for curve in curves {
        let every = (curve.gamma.len() / 220).max(1);
        for (k, g) in curve.gamma.iter().enumerate().step_by(every) {
            if !g.is_finite() {
                continue;
            }
            let (x, y) = place(g);
            let z = curve.z0 * (C64::new(1.0, 0.0) + g) / (C64::new(1.0, 0.0) - g);
            let label = labels.get(k).map_or(String::new(), Clone::clone);
            let value = if z.is_finite() { format!("{:.0} {} {:.0}j Ω", z.re, if z.im < 0.0 { "−" } else { "+" }, z.im.abs()) } else { "open".into() };
            pts.push(format!("[{x:.1},{y:.1},{},{},\"{}\"]", js_str(&value), js_str(&format!("{} · {label}", curve.name)), curve.color));
        }
    }
    let data = format!("{{\"pts\":[{}]}}", pts.join(","));
    let mut s = format!(r#"<svg class="smith" viewBox="0 0 {size} {height}" role="img" tabindex="0" aria-label="{}" data-hover='{}'>"#, esc(title), attr_json(&data));
    let _ = write!(s, r#"<circle cx="{c}" cy="{c}" r="{r}" class="base" fill="none"/><line x1="{}" x2="{}" y1="{c}" y2="{c}" class="g"/>"#, c - r, c + r);
    for rn in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        let (cc, rr) = (rn / (1.0 + rn), 1.0 / (1.0 + rn));
        let _ = write!(s, r#"<circle cx="{:.1}" cy="{c}" r="{:.1}" class="g" fill="none"/>"#, c + cc * r, rr * r);
        let left = c + (cc - rr) * r;
        let _ = write!(s, r#"<text x="{:.1}" y="{:.1}" class="ring">{:.0}</text>"#, left + 3.0, c + 11.0, rn * z0);
    }
    for xn in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        for sign in [1.0, -1.0] {
            // The reactance circle, clipped to the unit circle by sampling.
            let pts: Vec<String> = (0..=80)
                .map(|k| {
                    let ra = 50f64.powf(k as f64 / 80.0) - 1.0;
                    let z = C64::new(ra, sign * xn);
                    let g = (z - 1.0) / (z + 1.0);
                    format!("{:.1},{:.1}", c + g.re * r, c - g.im * r)
                })
                .collect();
            let _ = write!(s, r#"<polyline class="g" fill="none" points="{}"/>"#, pts.join(" "));
        }
    }
    for curve in curves {
        let d: Vec<String> = curve.gamma.iter().filter(|g| g.is_finite()).map(|g| {
            let (x, y) = place(g);
            format!("{x:.1},{y:.1}")
        }).collect();
        let _ = write!(s, r#"<polyline fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round" points="{}"><title>{}</title></polyline>"#, curve.color, d.join(" "), esc(curve.name));
        // Filled dot: start of the sweep. Ring: its end. Same as the app's chart.
        if let (Some(a), Some(b)) = (curve.gamma.first().filter(|g| g.is_finite()), curve.gamma.last().filter(|g| g.is_finite())) {
            let (xa, ya) = place(a);
            let (xb, yb) = place(b);
            let _ = write!(s, r##"<circle cx="{xa:.1}" cy="{ya:.1}" r="4" fill="{0}" stroke="#fff" stroke-width="2"/><circle cx="{xb:.1}" cy="{yb:.1}" r="4" fill="#fff" stroke="{0}" stroke-width="2"/>"##, curve.color);
        }
        if let Some(g) = mark.and_then(|k| curve.gamma.get(k)).filter(|g| g.is_finite()) {
            let (x, y) = place(g);
            let _ = write!(s, r##"<rect x="{:.1}" y="{:.1}" width="9" height="9" transform="rotate(45 {x:.1} {y:.1})" fill="{}" stroke="#fff" stroke-width="2"/>"##, x - 4.5, y - 4.5, curve.color);
        }
    }
    if curves.len() > 1 {
        let mut x = 12.0;
        for curve in curves {
            let _ = write!(s, r#"<line x1="{x:.1}" x2="{:.1}" y1="{1}" y2="{1}" stroke="{2}" stroke-width="2" stroke-linecap="round"/><text x="{3:.1}" y="{4}" class="a">{5}</text>"#, x + 14.0, height - 8.0, curve.color, x + 18.0, height - 4.0, esc(curve.name));
            x += 26.0 + curve.name.chars().count() as f64 * 6.2;
        }
    }
    s.push_str("</svg>");
    s
}

/// Light to dark in one hue, for a magnitude. `t` runs 0..=1.
pub fn ramp(t: f64) -> [u8; 3] {
    const STOPS: [[u8; 3]; 13] = [
        [0xcd, 0xe2, 0xfb],
        [0xb7, 0xd3, 0xf6],
        [0x9e, 0xc5, 0xf4],
        [0x86, 0xb6, 0xef],
        [0x6d, 0xa7, 0xec],
        [0x55, 0x98, 0xe7],
        [0x39, 0x87, 0xe5],
        [0x2a, 0x78, 0xd6],
        [0x25, 0x6a, 0xbf],
        [0x1c, 0x5c, 0xab],
        [0x18, 0x4f, 0x95],
        [0x10, 0x42, 0x81],
        [0x0d, 0x36, 0x6b],
    ];
    let v = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) } * (STOPS.len() - 1) as f64;
    let i = (v.floor() as usize).min(STOPS.len() - 2);
    let f = v - i as f64;
    std::array::from_fn(|k| (STOPS[i][k] as f64 + (STOPS[i + 1][k] as f64 - STOPS[i][k] as f64) * f).round() as u8)
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The key for a `ramp`: what its two ends mean.
pub fn scale_key(low: &str, high: &str) -> String {
    let stops: Vec<String> = (0..=6).map(|k| hex(ramp(k as f64 / 6.0))).collect();
    format!(r#"<div class="key"><span>{}</span><i style="background:linear-gradient(90deg,{})"></i><span>{}</span></div>"#, esc(low), stops.join(","), esc(high))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_land_on_round_values_for_a_narrow_rfid_band() {
        let (t, d) = nice_ticks(865.0, 868.0, 8);
        assert_eq!(t.first().copied(), Some(865.0));
        assert_eq!(t.last().copied(), Some(868.0));
        assert_eq!(d, 1, "half a MHz apart: {t:?}");
        let (t, d) = nice_ticks(-0.5, 0.5, 4);
        assert!(t.contains(&0.0) && d >= 1, "{t:?} {d}");
    }

    #[test]
    fn broken_values_never_reach_the_svg() {
        let line = Line::new("a", 0, vec![(0.0, 1.0), (1.0, f64::NAN), (2.0, f64::INFINITY), (3.0, 2.0)]);
        let s = line_chart(&Opts { title: "t", ..Default::default() }, &[line]);
        assert!(!s.contains("NaN") && !s.contains("inf"), "{s}");
        let polar = polar_chart("p", &[Line::new("a", 0, vec![(0.0, f64::NEG_INFINITY), (30.0, f64::NAN), (60.0, 1.0)])], 1.0, true, " dBi");
        assert!(!polar.contains("NaN") && !polar.contains("inf"), "{polar}");
    }

    #[test]
    fn a_name_cannot_break_out_of_the_data_attribute() {
        let line = Line::new("it's <b>\"x\"</b>", 0, vec![(0.0, 1.0), (1.0, 2.0)]);
        let s = line_chart(&Opts { title: "t", ..Default::default() }, &[line]);
        assert!(!s.contains("<b>"), "{s}");
        assert!(s.contains("&#39;"), "{s}");
    }

    #[test]
    fn past_eight_series_nothing_gets_a_new_colour() {
        assert_eq!(color(7), "#e34948");
        assert_eq!(color(8), OTHER);
        assert_eq!(color(50), OTHER);
        let lines: Vec<Line> = (0..10).map(|k| Line::new(format!("S{k}"), k, vec![(0.0, k as f64), (1.0, k as f64)])).collect();
        assert!(line_chart(&Opts { title: "t", ..Default::default() }, &lines).contains("Other (2)"));
    }
}
