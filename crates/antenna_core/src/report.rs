//! A self-contained HTML datasheet for a simulated antenna: what it is, how well it
//! matches, how it radiates, what the numbers rest on, and the data behind the charts.
//! Everything is inside the one file: plots are inline SVG, and it loads nothing.
//!
//! A report describes one run, so what it says comes from that run wherever the run knows
//! it. The project can have moved on since (a new sweep, another variable), and then the
//! page says so rather than mixing the two.

mod chart;
mod drawing;
mod page;

use std::collections::BTreeMap;
use std::fmt::Write;

use chart::{Line, Opts, SmithCurve, line_chart, minus, polar_chart, smith_chart};

use crate::farfield::{Cut, FarField, Metrics, Pol, Quantity};
use crate::geometry::{Geometry, Shape, norm, sub};
use crate::material::{self, MatKind};
use crate::project::{Project, SimResult};
use crate::recipes::gate::{GateReport, GateSpec, HeatMap};
use crate::sparam::{self, Network};
use crate::{C0, C64, checks, db20, templates};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// What the app knows beyond the project and its result. All of it is optional: without
/// any, the report still covers everything the run itself holds.
#[derive(Clone, Debug, Default)]
pub struct Extras {
    /// When and by what the file was written, as it should read.
    pub stamp: String,
    pub link: Option<LinkInputs>,
    /// The regions' UHF RFID rules. Only those the sweep touches are shown, and only for
    /// an antenna that looks like it's for RFID.
    pub regions: Vec<RegionLimit>,
    /// The mesh study this result came out of, if it did.
    pub mesh_study: Vec<MeshStep>,
    /// The gate test, if it still applies to this model.
    pub gate: Option<GateSection>,
}

/// The inputs on the Link budget page.
#[derive(Clone, Copy, Debug)]
pub struct LinkInputs {
    pub tx_dbm: f64,
    pub rx_gain_dbi: f64,
    pub sensitivity_dbm: f64,
    pub margin_db: f64,
}

/// One region's rule for UHF RFID.
#[derive(Clone, Debug, Default)]
pub struct RegionLimit {
    pub name: String,
    /// Hz.
    pub band: (f64, f64),
    /// As the rule states it.
    pub limit_text: String,
    pub eirp_dbm: Option<f64>,
    /// The limit on the reader's own power, where the rule has one as well.
    pub conducted_dbm: Option<f64>,
    /// How far the rule has been confirmed.
    pub status: String,
    pub source: String,
    pub active: bool,
}

/// One pass of a mesh study.
#[derive(Clone, Copy, Debug)]
pub struct MeshStep {
    pub density: f64,
    pub size: u64,
    pub seconds: f64,
    pub f_res: f64,
    pub peak_dbi: f64,
    pub delta_s: f64,
    pub gain_change_db: f64,
}

/// A gate test's results, judged for the project's excitation states.
#[derive(Clone, Debug)]
pub struct GateSection {
    pub report: GateReport,
    pub spec: GateSpec,
    pub read_range: (f64, f64),
    pub stray_range: (f64, f64),
    pub evidence: String,
    pub assumptions: Vec<String>,
}

/// The whole report as one HTML page, from the project and its result alone.
pub fn html(project: &Project, r: &SimResult) -> String {
    html_with(project, r, &Extras::default())
}

/// The whole report as one HTML page.
pub fn html_with(project: &Project, r: &SimResult, x: &Extras) -> String {
    let c = Ctx::new(project, r, x);
    let sections: Vec<(&str, &str, &str, String)> = vec![
        ("summary", "Summary", "Summary", summary(&c)),
        ("antenna", "Antenna", "The antenna", antenna(&c)),
        ("match", "Match", "Match and S-parameters", matching(&c)),
        ("ports", "Ports", "Between the ports", between_ports(&c)),
        ("pattern", "Radiation", "Radiation pattern", radiation(&c)),
        ("fields", "Fields", "Currents and fields", fields(&c)),
        ("link", "Link budget", "Link budget", link_budget(&c)),
        ("rfid", "RFID limits", "UHF RFID limits", rfid_limits(&c)),
        ("gate", "Gate test", "RFID gate test", gate_test(&c)),
        ("method", "Method", "How these numbers were made", method(&c)),
        ("variables", "Variables", "Design variables", variables(&c)),
        ("data", "Data", "Data", data(&c)),
    ];
    let mut body = head(&c);
    let links: Vec<String> = sections.iter().filter(|s| !s.3.is_empty()).map(|s| format!("<a href=\"#{}\">{}</a>", s.0, s.1)).collect();
    let _ = write!(body, "<nav>{}</nav>", links.join(""));
    if !c.edited.is_empty() {
        let _ = write!(body, "<p class=\"banner\"><b>This design has changed since the run.</b> The figures and the drawing here are for the run: {}.</p>", esc(&c.edited.join("; ")));
    }
    for (id, _, title, content) in &sections {
        if !content.is_empty() {
            let _ = write!(body, "<section id=\"{id}\"><h2>{title}</h2>{content}</section>");
        }
    }
    page::wrap(&project.name, &body)
}

/// What every section needs to know.
struct Ctx<'a> {
    p: &'a Project,
    r: &'a SimResult,
    x: &'a Extras,
    net: &'a Network,
    n: usize,
    /// The design frequency, or the middle of the sweep if that isn't in it.
    f_ref: f64,
    f0_in_sweep: bool,
    /// The sample nearest `f_ref`, as the app's own result pages use.
    k_ref: usize,
    /// The far field nearest `f_ref`.
    far_k: Option<usize>,
    mhz: bool,
    /// The model as it was solved.
    geometry: Result<Geometry, String>,
    vars: BTreeMap<String, f64>,
    edited: Vec<String>,
    /// All the ports are driven together, with weights someone set.
    driven: bool,
}

impl<'a> Ctx<'a> {
    fn new(p: &'a Project, r: &'a SimResult, x: &'a Extras) -> Self {
        let net = &r.network;
        let n = net.ports();
        let now = p.vars().ok();
        // Pin what changed to the run's value, so the drawing shows what was solved.
        let mut pinned = p.clone();
        let mut edited = Vec::new();
        for q in &mut pinned.params {
            let (Some(run), Some(cur)) = (r.vars.get(&q.name), now.as_ref().and_then(|v| v.get(&q.name))) else { continue };
            if (run - cur).abs() > 1e-9 * run.abs().max(cur.abs()).max(1e-30) {
                edited.push(format!("{} is {} in this run and {} now", q.name, param_text(&q.name, *run), param_text(&q.name, *cur)));
                q.expr = format!("{run}");
            }
        }
        let f0 = r.vars.get("f0").copied().unwrap_or_else(|| p.f0());
        let (lo, hi) = (net.f.first().copied().unwrap_or(p.setup.f_lo), net.f.last().copied().unwrap_or(p.setup.f_hi));
        let f0_in_sweep = f0 >= lo && f0 <= hi;
        let f_ref = if f0_in_sweep { f0 } else { (lo + hi) / 2.0 };
        let k_ref = net.f.iter().enumerate().min_by(|a, b| (a.1 - f_ref).abs().total_cmp(&(b.1 - f_ref).abs())).map_or(0, |x| x.0);
        let far_k = r.far_index(f_ref).filter(|k| r.far.first().is_some_and(|list| list.get(*k).is_some()));
        let vars = if r.vars.is_empty() { now.unwrap_or_default().into_iter().collect() } else { r.vars.clone() };
        let geometry = pinned.geometry().map_err(|e| format!("{e:#}"));
        let driven = n > 1 && !p.drive.is_empty() && r.far.len() == n;
        Self { p, r, x, net, n, f_ref, f0_in_sweep, k_ref, far_k, mhz: hi < 1.5e9, geometry, vars, edited, driven }
    }

    fn has_network(&self) -> bool {
        self.n > 0 && !self.net.f.is_empty()
    }

    /// A frequency, in the one unit this report uses for them.
    fn f(&self, hz: f64) -> String {
        if !hz.is_finite() {
            "–".into()
        } else if self.mhz {
            format!("{:.1} MHz", hz / 1e6)
        } else {
            format!("{:.3} GHz", hz / 1e9)
        }
    }

    fn x_value(&self, hz: f64) -> f64 {
        if self.mhz { hz / 1e6 } else { hz / 1e9 }
    }

    fn x_label(&self) -> &'static str {
        if self.mhz { "Frequency (MHz)" } else { "Frequency (GHz)" }
    }

    fn curve(&self, y: impl IntoIterator<Item = f64>) -> Vec<(f64, f64)> {
        self.net.f.iter().map(|f| self.x_value(*f)).zip(y).collect()
    }

    fn marker(&self) -> Option<(f64, &'static str)> {
        Some((self.x_value(self.f_ref), if self.f0_in_sweep { "f0" } else { "mid" }))
    }

    fn ref_name(&self) -> &'static str {
        if self.f0_in_sweep { "Design frequency" } else { "Middle of the sweep" }
    }

    /// The pattern that speaks for the antenna: all the ports at their weights if someone
    /// set them, otherwise port 1. With what to call it.
    fn headline(&self, k: usize) -> Option<(FarField, &'static str)> {
        if self.driven
            && let Some(ff) = self.r.combined(k, &self.p.weights(self.n))
        {
            return Some((ff, "all ports, at the weights under Between the ports"));
        }
        let ff = self.r.far.first()?.get(k)?.clone();
        Some((ff, if self.n > 1 { "port 1 driven" } else { "" }))
    }
}

/// A number with its unit, or a dash for what can't be a number.
fn num(v: f64, decimals: usize, unit: &str) -> String {
    if v.is_finite() { minus(format!("{v:.decimals$}{unit}")) } else { "–".into() }
}

fn z_text(z: C64) -> String {
    if !z.is_finite() {
        return "–".into();
    }
    format!("{} {} {:.1}j Ω", minus(format!("{:.1}", z.re)), if z.im < 0.0 { "−" } else { "+" }, z.im.abs())
}

fn vswr_text(v: f64) -> String {
    if !v.is_finite() || v >= 1e3 { "over 1000".into() } else { format!("{v:.2}") }
}

fn table(head: &[&str], rows: &[Vec<String>]) -> String {
    let mut s = String::from("<div class=\"scroll\"><table><thead><tr>");
    for h in head {
        let _ = write!(s, "<th>{}</th>", esc(h));
    }
    s.push_str("</tr></thead><tbody>");
    for row in rows {
        s.push_str("<tr>");
        for cell in row {
            let _ = write!(s, "<td>{cell}</td>");
        }
        s.push_str("</tr>");
    }
    s.push_str("</tbody></table></div>");
    s
}

/// Two columns of label and value. Labels are text; values are already HTML.
fn pairs(rows: &[(&str, String)]) -> String {
    let mut s = String::from("<div class=\"scroll\"><table><tbody>");
    for (k, v) in rows {
        let _ = write!(s, "<tr><th>{}</th><td>{v}</td></tr>", esc(k));
    }
    s.push_str("</tbody></table></div>");
    s
}

fn tile(value: &str, label: &str, under: &str) -> String {
    let mut s = format!("<div class=\"tile\"><b>{value}</b><span>{}</span>", esc(label));
    if !under.is_empty() {
        let _ = write!(s, "<span>{}</span>", esc(under));
    }
    s.push_str("</div>");
    s
}

fn verdict(good: bool, text: &str) -> String {
    format!("<p class=\"verdict {}\">{} {}</p>", if good { "good" } else { "bad" }, if good { "✓" } else { "✕" }, esc(text))
}

/// The bands below a level. One whose edge touches a broken sample has no width to report.
fn bands_of(net: &Network, port: usize, limit_db: f64) -> Vec<(f64, f64)> {
    net.bandwidths(port, limit_db).into_iter().filter(|b| b.0.is_finite() && b.1.is_finite()).collect()
}

/// A band that reaches the end of the sweep is only as wide as the sweep lets it show.
fn cut_off(net: &Network, band: (f64, f64)) -> bool {
    net.f.first().is_some_and(|f| band.0 <= *f) || net.f.last().is_some_and(|f| band.1 >= *f)
}

fn width_text(net: &Network, band: (f64, f64)) -> String {
    format!("{}{:.1} MHz", if cut_off(net, band) { "≥ " } else { "" }, (band.1 - band.0) / 1e6)
}

fn head(c: &Ctx) -> String {
    let template = templates::by_id(&c.p.template);
    let mut s = format!("<h1>{}</h1>", esc(&c.p.name));
    let mut line = Vec::new();
    if let Some(t) = template {
        line.push(esc(t.title));
    }
    if !c.r.solver.is_empty() {
        line.push(esc(&c.r.solver));
    }
    line.push(size_text(c.r));
    line.push(format!("solved in {:.0} s", c.r.seconds));
    let _ = write!(s, "<p class=\"m\">{}</p>", line.join(" · "));
    if let Some(t) = template.filter(|t| !t.blurb.is_empty()) {
        let _ = write!(s, "<p class=\"m\">{}</p>", esc(t.blurb));
    }
    s.push_str("<p class=\"m\"><span class=\"chip\">Simulated, not measured</span>");
    s.push_str(&esc(&c.x.stamp));
    s.push_str("</p>");
    s
}

fn summary(c: &Ctx) -> String {
    let mut s = String::new();
    let mut tiles = Vec::new();
    if c.has_network() {
        let at = |p: usize| c.net.sij_db(p, p)[c.k_ref];
        let worst = (0..c.n).max_by(|a, b| at(*a).total_cmp(&at(*b))).unwrap_or(0);
        let good = (0..c.n).all(|p| at(p) <= -10.0);
        let word = if good { "Matched" } else { "Not matched" };
        let port = if c.n > 1 { format!(" Worst port: S{0}{0}.", worst + 1) } else { String::new() };
        let hint = c.net.resonances(worst).iter().min_by(|a, b| (a.0 - c.f_ref).abs().total_cmp(&(b.0 - c.f_ref).abs())).map_or(String::new(), |(fr, _)| {
            let off = (fr - c.f_ref) / c.f_ref * 100.0;
            if off.abs() > 1.0 { format!(" The nearest resonance is {:.1} % {} that, at {}.", off.abs(), if off > 0.0 { "above" } else { "below" }, c.f(*fr)) } else { String::new() }
        });
        s.push_str(&verdict(good, &format!("{word} at {}: {} at −10 dB.{port}{hint}", c.f(c.f_ref), num(at(worst), 1, " dB"))));
        if !c.f0_in_sweep {
            let _ = write!(s, "<p class=\"note\">The design frequency isn't inside this sweep, so these figures are for the middle of the sweep.</p>");
        }
        let z = c.net.zin(0)[c.k_ref];
        tiles.push(tile(&c.f(c.f_ref), c.ref_name(), ""));
        tiles.push(tile(&num(c.net.sij_db(0, 0)[c.k_ref], 1, " dB"), "S11", ""));
        tiles.push(tile(&vswr_text(c.net.vswr(0)[c.k_ref]), "VSWR", ""));
        tiles.push(tile(&z_text(z), "Input impedance", &format!("against {:.0} Ω", c.net.z0.first().copied().unwrap_or(50.0))));
        let bands = bands_of(c.net, 0, -10.0);
        match bands.iter().find(|b| b.0 <= c.f_ref && c.f_ref <= b.1).or(bands.first()) {
            Some(b) => tiles.push(tile(&width_text(c.net, *b), "Bandwidth at −10 dB", &format!("{:.1} % of its centre", (b.1 - b.0) / ((b.0 + b.1) / 2.0) * 100.0))),
            None => tiles.push(tile("None", "Bandwidth at −10 dB", "in this sweep")),
        }
    }
    if let Some((ff, who)) = c.far_k.and_then(|k| c.headline(k)) {
        let m = ff.metrics();
        tiles.push(tile(&num(m.peak_realized_dbi, 2, " dBi"), "Peak realized gain", who));
        tiles.push(tile(&num(m.total_efficiency * 100.0, 1, " %"), if c.r.lossless { "Total efficiency (mismatch only)" } else { "Total efficiency" }, ""));
        if let (Some(a), Some(b)) = (m.hpbw_e.filter(|v| v.is_finite()), m.hpbw_h.filter(|v| v.is_finite())) {
            tiles.push(tile(&format!("{a:.0}° × {b:.0}°"), "Beam width", "main plane × cross plane"));
        }
        if let Some(v) = m.front_to_back_db {
            tiles.push(tile(&num(v, 1, " dB"), "Front to back", ""));
        }
    }
    if let Ok(g) = &c.geometry {
        let (lo, hi) = g.bounds();
        let d = sub(hi, lo);
        tiles.push(tile(&format!("{} mm", dims(&[d[0], d[1], d[2]])), "Overall size", "x × y × z"));
    }
    let _ = write!(s, "<div class=\"tiles\">{}</div>", tiles.join(""));
    s
}

/// Millimetres with no trailing zeros, joined: `120 × 80 × 1.6`.
fn dims(v: &[f64]) -> String {
    v.iter().map(|m| mm_num(*m)).collect::<Vec<_>>().join(" × ")
}

fn mm_num(metres: f64) -> String {
    let t = format!("{:.2}", metres * 1e3);
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// What a part is and how big, in words.
fn shape_text(s: &Shape) -> (String, String) {
    let (lo, hi) = s.bounds();
    let d = sub(hi, lo);
    match s {
        Shape::Box { min, max } => {
            let sides: Vec<f64> = (0..3).map(|k| max[k] - min[k]).filter(|v| *v > 0.0).collect();
            (if s.is_sheet() { "Sheet" } else { "Block" }.to_string(), format!("{} mm", dims(&sides)))
        }
        Shape::Cylinder { a, b, r } => ("Cylinder".to_string(), format!("⌀{} × {} mm", mm_num(2.0 * r), mm_num(norm(sub(*b, *a))))),
        Shape::Sphere { r, .. } => ("Sphere".to_string(), format!("⌀{} mm", mm_num(2.0 * r))),
        Shape::Polygon { pts, thick, .. } => {
            let kind = if *thick > 0.0 { format!("Outline of {} points, {} mm thick", pts.len(), mm_num(*thick)) } else { format!("Flat outline of {} points", pts.len()) };
            (kind, format!("{} mm", dims(&[d[0], d[1], d[2]])))
        }
        Shape::Wire { pts, r } => {
            let length: f64 = pts.windows(2).map(|w| norm(sub(w[1], w[0]))).sum();
            ("Wire".to_string(), format!("⌀{} mm, {} mm long", mm_num(2.0 * r), mm_num(length)))
        }
        Shape::Tris { tris, solid, .. } => (format!("{} of {} triangles", if *solid { "Solid" } else { "Surface" }, tris.len()), format!("{} mm", dims(&[d[0], d[1], d[2]]))),
    }
}

fn antenna(c: &Ctx) -> String {
    let g = match &c.geometry {
        Ok(g) => g,
        Err(e) => return format!("<p class=\"note\">The model can't be drawn: {}.</p>", esc(e)),
    };
    let mut s = drawing::model_figure(g);
    let (lo, mut hi) = g.bounds();
    let mut lo = lo;
    let d = sub(hi, lo);
    let f = c.f_ref;
    let lambda = C0 / f.max(1.0);
    let mut rows: Vec<(&str, String)> = vec![
        ("Overall size", format!("{} mm (x × y × z)", dims(&[d[0], d[1], d[2]]))),
        ("In wavelengths", format!("{:.3} λ × {:.3} λ × {:.3} λ at {}", d[0] / lambda, d[1] / lambda, d[2] / lambda, c.f(f))),
    ];
    // Below a ground plane is its image, which is part of the antenna as far as Q goes.
    if g.ground {
        let top = hi[2].abs().max(lo[2].abs());
        lo[2] = -top;
        hi[2] = top;
    }
    let a = norm(sub(hi, lo)) / 2.0;
    let ka = 2.0 * std::f64::consts::PI * f / C0 * a;
    if ka > 0.0 {
        rows.push(("Enclosing sphere", format!("{} mm radius{}, ka = {ka:.2}", mm_num(a), if g.ground { ", with its image in the ground plane" } else { "" })));
        rows.push(("Smallest Q for that size (Chu)", num(sparam::chu_q_limit(ka), 1, "")));
        if c.has_network() {
            let q = c.net.q_factor(0)[c.k_ref];
            rows.push(("Q of this design at that frequency", num(q, 1, "")));
        }
        let limit = sparam::wheeler_bandwidth_limit(ka, 2.0) * 100.0;
        rows.push(("Widest bandwidth for that size, at VSWR 2", format!("{limit:.1} %, for a single-tuned antenna with no loss")));
    }
    s.push_str("<h3>Size</h3>");
    s.push_str(&pairs(&rows));
    if !g.ports.is_empty() {
        let ports: Vec<Vec<String>> = g
            .ports
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let at = p.center();
                vec![(k + 1).to_string(), esc(&p.name), format!("{}, {}, {} mm", mm_num(at[0]), mm_num(at[1]), mm_num(at[2])), format!("{} mm along {}", mm_num(norm(sub(p.b, p.a))), ["x", "y", "z"][p.axis()]), if p.width > 0.0 { format!("{} mm", mm_num(p.width)) } else { "line".into() }, format!("{:.1} Ω", p.z0)]
            })
            .collect();
        s.push_str("<h3>Feeds</h3>");
        s.push_str(&table(&["Port", "Name", "Centre (x, y, z)", "Gap", "Width", "Impedance"], &ports));
    }
    let parts: Vec<Vec<String>> = g
        .solids
        .iter()
        .map(|p| {
            let (kind, size) = shape_text(&p.shape);
            vec![esc(&p.name), esc(&p.material), esc(&kind), esc(&size)]
        })
        .collect();
    let parts_table = table(&["Part", "Material", "Shape", "Size"], &parts);
    if parts.len() <= 12 {
        let _ = write!(s, "<h3>Parts</h3>{parts_table}");
    } else {
        let _ = write!(s, "<h3>Parts</h3><details><summary>{} parts</summary>{parts_table}</details>", parts.len());
    }
    let mut names: Vec<&str> = g.solids.iter().map(|p| p.material.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let materials: Vec<Vec<String>> = names
        .iter()
        .map(|name| match material::by_name(name).map(|m| (m.kind, m)) {
            Some((MatKind::Pec, _)) => vec![esc(name), "Perfect conductor".into(), "–".into(), "–".into(), "No loss".into()],
            Some((MatKind::Metal { sigma }, m)) => vec![esc(name), "Metal".into(), "–".into(), "–".into(), format!("{:.2} × 10⁷ S/m, skin depth {:.2} µm at {}", sigma / 1e7, m.skin_depth(f) * 1e6, c.f(f))],
            Some((MatKind::Dielectric { eps_r, tan_d, .. }, _)) => vec![esc(name), "Dielectric".into(), format!("{eps_r}"), format!("{tan_d}"), "–".into()],
            None => vec![esc(name), "Not in the library".into(), "–".into(), "–".into(), "–".into()],
        })
        .collect();
    let _ = write!(s, "<h3>Materials</h3>{}", table(&["Material", "Kind", "εr", "tan δ", "Conductor"], &materials));
    if c.p.reconfig.is_on() {
        let _ = write!(s, "<p class=\"note\">This model has {} switches or varactors. The Reconfigurable page exports a report of every state; this one covers the state on screen.</p>", c.p.reconfig.elements.len());
    }
    s
}

/// The key rows for one port: the design frequency, its best matches, its worst and the ends.
fn key_rows(c: &Ctx, port: usize) -> Vec<Vec<String>> {
    let net = c.net;
    let (db, vs, ml, z, q) = (net.sij_db(port, port), net.vswr(port), net.mismatch_loss_db(port), net.zin(port), net.q_factor(port));
    let nearest = |f: f64| net.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map_or(0, |x| x.0);
    let row = |label: &str, k: usize, f: f64, s: f64| -> Vec<String> {
        let mut out = if c.n > 1 { vec![(port + 1).to_string()] } else { Vec::new() };
        out.extend([label.to_string(), c.f(f), num(s, 1, " dB"), vswr_text(vs[k]), z_text(z[k]), num(ml[k], 2, " dB"), num(q[k], 1, "")]);
        out
    };
    let mut rows = vec![row(c.ref_name(), c.k_ref, net.f[c.k_ref], db[c.k_ref])];
    for (f, level) in net.resonances(port).iter().take(3) {
        rows.push(row("Best match", nearest(*f), *f, *level));
    }
    if let Some(k) = (0..db.len()).max_by(|a, b| db[*a].total_cmp(&db[*b])) {
        rows.push(row("Worst in the sweep", k, net.f[k], db[k]));
    }
    let last = db.len() - 1;
    rows.push(row("Start of the sweep", 0, net.f[0], db[0]));
    rows.push(row("End of the sweep", last, net.f[last], db[last]));
    rows
}

fn band_rows(c: &Ctx, port: usize) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for (limit, name) in [(-6.0, "−6 dB"), (-20.0 * ((2.0f64 - 1.0) / (2.0 + 1.0)).log10() * -1.0, "VSWR 2 (−9.5 dB)"), (-10.0, "−10 dB"), (-15.0, "−15 dB")] {
        let bands = bands_of(c.net, port, limit);
        let lead = |rows: &mut Vec<String>| {
            if c.n > 1 {
                rows.push((port + 1).to_string());
            }
            rows.push(name.to_string());
        };
        if bands.is_empty() {
            let mut row = Vec::new();
            lead(&mut row);
            row.extend(["None in this sweep".to_string(), "–".to_string(), "–".to_string()]);
            rows.push(row);
        }
        for b in bands.iter().take(3) {
            let mut row = Vec::new();
            lead(&mut row);
            let cut = cut_off(c.net, *b);
            row.extend([format!("{} to {}", c.f(b.0), c.f(b.1)), width_text(c.net, *b), format!("{}{:.1} %", if cut { "≥ " } else { "" }, (b.1 - b.0) / ((b.0 + b.1) / 2.0) * 100.0)]);
            rows.push(row);
        }
    }
    rows
}

fn matching(c: &Ctx) -> String {
    if !c.has_network() {
        return String::new();
    }
    let net = c.net;
    let mut s = String::new();
    let shown = c.n.min(8);
    let reflect: Vec<Line> = (0..c.n).map(|p| Line::new(format!("S{0}{0}", p + 1), p, c.curve(net.sij_db(p, p)))).collect();
    s.push_str(&line_chart(&Opts { title: "Reflection at the feed", x_label: c.x_label(), y_unit: " dB", limit: Some((-10.0, "−10 dB")), marker: c.marker(), ..Default::default() }, &reflect));
    if c.n > 1 {
        // The strongest coupling gets the colours; the rest stay visible in grey.
        let mut pairs: Vec<(usize, usize, f64)> = (0..c.n).flat_map(|i| (0..i).map(move |j| (i, j, 0.0))).collect();
        for pair in &mut pairs {
            pair.2 = net.sij_db(pair.0, pair.1).into_iter().filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
        }
        pairs.sort_by(|a, b| b.2.total_cmp(&a.2));
        let lines: Vec<Line> = pairs.iter().enumerate().map(|(k, (i, j, _))| Line::new(format!("S{}{}", i + 1, j + 1), k, c.curve(net.sij_db(*i, *j)))).collect();
        s.push_str(&line_chart(&Opts { title: "Coupling between ports", x_label: c.x_label(), y_unit: " dB", marker: c.marker(), ..Default::default() }, &lines));
    }
    s.push_str("<h3>Key frequencies</h3>");
    let mut key = Vec::new();
    for p in 0..shown {
        key.extend(key_rows(c, p));
    }
    let head: &[&str] = if c.n > 1 { &["Port", "", "Frequency", "|S|", "VSWR", "Impedance", "Mismatch loss", "Q"] } else { &["", "Frequency", "S11", "VSWR", "Impedance", "Mismatch loss", "Q"] };
    s.push_str(&table(head, &key));
    s.push_str("<h3>Bandwidth</h3>");
    let mut bands = Vec::new();
    for p in 0..shown {
        bands.extend(band_rows(c, p));
    }
    let head: &[&str] = if c.n > 1 { &["Port", "Limit", "Band", "Width", "Share of its centre"] } else { &["Limit", "Band", "Width", "Share of its centre"] };
    s.push_str(&table(head, &bands));
    if (0..shown).any(|p| [-6.0, -10.0, -15.0].iter().any(|l| bands_of(net, p, *l).iter().any(|b| cut_off(net, *b)))) {
        s.push_str("<p class=\"note\">≥ means the match continues past the end of the sweep, so the real bandwidth is wider. Widen the sweep to measure it.</p>");
    }
    // Small charts, two a row.
    let half = |title: &str, unit: &str, y_range: Option<(f64, f64)>, limit: Option<(f64, &str)>, lines: &[Line]| line_chart(&Opts { title, x_label: c.x_label(), y_unit: unit, y_range, limit, marker: c.marker(), half: true }, lines);
    let mut grid = String::new();
    let vswr: Vec<Line> = (0..c.n).map(|p| Line::new(format!("Port {}", p + 1), p, c.curve(net.vswr(p).into_iter().map(|v| v.min(10.0))))).collect();
    grid.push_str(&half("VSWR, shown up to 10", "", Some((1.0, 10.0)), Some((2.0, "2")), &vswr));
    for p in 0..shown.min(4) {
        let z = net.zin(p);
        // A sharp antiresonance would flatten everything else.
        let lim = z.iter().map(|v| v.re.abs()).filter(|v| v.is_finite()).fold(0.0, f64::max).clamp(1e-3, 4000.0);
        let re = Line::new("R", 0, c.curve(z.iter().map(|v| v.re.clamp(-lim, lim))));
        let im = Line::new("X", 1, c.curve(z.iter().map(|v| v.im.clamp(-lim, lim))));
        let title = format!("Input impedance, port {}", p + 1);
        grid.push_str(&half(&title, " Ω", None, None, &[re, im]));
    }
    let q = Line::new("Q", 0, c.curve(net.q_factor(0).into_iter().map(|v| v.min(1000.0))));
    grid.push_str(&half("Quality factor at port 1 (Yaghjian-Best)", "", None, None, &[q]));
    let phase = Line::new("Phase", 0, c.curve(net.sij_phase_deg(0, 0)));
    grid.push_str(&half("Phase of S11, unwrapped", "°", None, None, &[phase]));
    let (gi, gj, gname) = if c.n > 1 { (1, 0, "Group delay of S21") } else { (0, 0, "Group delay of S11") };
    let delay = Line::new("Delay", 0, c.curve(net.group_delay(gi, gj).into_iter().map(|v| v * 1e9)));
    grid.push_str(&half(gname, " ns", None, None, &[delay]));
    let _ = write!(s, "<div class=\"grid2\">{grid}</div>");

    s.push_str("<h3>Smith chart</h3>");
    let gammas: Vec<(String, Vec<C64>)> = (0..shown).map(|p| (format!("S{0}{0}", p + 1), net.s.iter().map(|m| m[p][p]).collect())).collect();
    let curves: Vec<SmithCurve> = gammas.iter().enumerate().map(|(p, (name, gamma))| SmithCurve { name, color: chart::color(p), gamma, z0: net.z0.get(p).copied().unwrap_or(50.0) }).collect();
    let labels: Vec<String> = net.f.iter().map(|f| c.f(*f)).collect();
    s.push_str(&smith_chart("Smith chart", &curves, &labels, Some(c.k_ref)));
    let _ = write!(s, "<p class=\"note\">Against {:.0} Ω. Filled dot: start of the sweep, {}. Ring: its end, {}. Diamond: {}.</p>", net.z0.first().copied().unwrap_or(50.0), c.f(net.f[0]), c.f(net.f[net.f.len() - 1]), c.f(net.f[c.k_ref]));

    for p in 0..shown {
        let (db, vs, ml, z, q) = (net.sij_db(p, p), net.vswr(p), net.mismatch_loss_db(p), net.zin(p), net.q_factor(p));
        let last = db.len() - 1;
        let mut picks: Vec<usize> = (0..=20).map(|k| k * last / 20).collect();
        picks.push(c.k_ref);
        picks.sort_unstable();
        picks.dedup();
        let rows: Vec<Vec<String>> = picks.iter().map(|k| vec![c.f(net.f[*k]), num(db[*k], 2, " dB"), vswr_text(vs[*k]), z_text(z[*k]), num(ml[*k], 2, " dB"), num(q[*k], 1, "")]).collect();
        let _ = write!(s, "<details><summary>Table: port {} across the sweep</summary>{}</details>", p + 1, table(&["Frequency", "|S|", "VSWR", "Impedance", "Mismatch loss", "Q"], &rows));
    }
    s
}

fn between_ports(c: &Ctx) -> String {
    if c.n < 2 || !c.has_network() {
        return String::new();
    }
    let net = c.net;
    let k = c.k_ref;
    let mut s = String::new();
    if c.n <= 8 {
        let mut head: Vec<String> = vec![format!("|S| at {}", c.f(net.f[k]))];
        head.extend((0..c.n).map(|j| format!("From port {}", j + 1)));
        let refs: Vec<&str> = head.iter().map(String::as_str).collect();
        let rows: Vec<Vec<String>> = (0..c.n)
            .map(|i| {
                let mut row = vec![format!("To port {}", i + 1)];
                row.extend((0..c.n).map(|j| num(db20(net.s[k][i][j].norm()), 1, " dB")));
                row
            })
            .collect();
        s.push_str(&table(&refs, &rows));
    }
    let mut pairs = Vec::new();
    for i in 0..c.n {
        for j in 0..i {
            let db = net.sij_db(i, j);
            let worst = (0..db.len()).max_by(|a, b| db[*a].total_cmp(&db[*b])).unwrap_or(0);
            let from_s = net.ecc_from_s(i, j)[k];
            let from_far = c.far_k.and_then(|fk| Some(sparam::ecc_from_far(c.r.far.get(i)?.get(fk)?, c.r.far.get(j)?.get(fk)?)));
            pairs.push((db[worst], vec![format!("{} and {}", j + 1, i + 1), num(db[k], 1, " dB"), format!("{} at {}", num(db[worst], 1, " dB"), c.f(net.f[worst])), num(from_s, 3, ""), from_far.map_or("–".into(), |v| num(v, 3, "")), from_far.map_or("–".into(), |v| num(sparam::diversity_gain_db(v), 2, " dB"))]));
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let more = pairs.len().saturating_sub(12);
    let rows: Vec<Vec<String>> = pairs.into_iter().take(12).map(|p| p.1).collect();
    s.push_str("<h3>Coupling and correlation</h3>");
    s.push_str(&table(&["Ports", &format!("Coupling at {}", c.f(net.f[k])), "Strongest in the sweep", "Envelope correlation from S", "From the patterns", "Diversity gain"], &rows));
    if more > 0 {
        let _ = write!(s, "<p class=\"note\">The strongest 12 pairs; {more} more couple less.</p>");
    }
    s.push_str("<p class=\"note\">Correlation from S holds only for a lossless pair. From the patterns it assumes an isotropic environment with equal power in both polarisations.</p>");
    if !c.p.drive.is_empty() {
        let weights = c.p.weights(c.n);
        let rows: Vec<Vec<String>> = c.p.drive.iter().take(c.n).enumerate().map(|(p, d)| vec![(p + 1).to_string(), format!("{:.2}", d.amp), num(d.phase, 1, "°")]).collect();
        s.push_str("<h3>Port weights</h3>");
        s.push_str(&table(&["Port", "Amplitude", "Phase"], &rows));
        let tarc = net.tarc(&weights);
        let lines: Vec<Line> = (0..c.n)
            .map(|p| Line::new(format!("Port {}", p + 1), p, c.curve(net.active_reflection(p, &weights).iter().map(|g| db20(g.norm())))))
            .chain(std::iter::once(Line { name: "Total (TARC)".into(), color: chart::OTHER, points: c.curve(tarc.iter().map(|v| db20(*v))) }))
            .collect();
        s.push_str(&line_chart(&Opts { title: "Reflection with those weights", x_label: c.x_label(), y_unit: " dB", limit: Some((-10.0, "−10 dB")), marker: c.marker(), ..Default::default() }, &lines));
        let _ = write!(s, "<p class=\"note\">Total active reflection at {}: {}.</p>", c.f(net.f[k]), num(db20(tarc[k]), 1, " dB"));
    }
    if !c.p.states.is_empty() {
        let mut head = vec!["State".to_string()];
        head.extend((0..c.n).map(|p| format!("Port {}", p + 1)));
        let refs: Vec<&str> = head.iter().map(String::as_str).collect();
        let rows: Vec<Vec<String>> = c
            .p
            .states
            .iter()
            .map(|st| {
                let mut row = vec![esc(&st.name)];
                row.extend((0..c.n).map(|p| st.drive.get(p).filter(|d| d.amp > 0.0).map_or("off".to_string(), |d| format!("{:.2} at {}", d.amp, num(d.phase, 0, "°")))));
                row
            })
            .collect();
        s.push_str("<h3>Excitation states</h3>");
        s.push_str(&table(&refs, &rows));
    }
    s
}

fn direction_text(m: &Metrics) -> String {
    // On the z axis φ means nothing.
    if m.peak_theta < 0.5 || m.peak_theta > 179.5 { format!("θ {:.0}°", m.peak_theta) } else { format!("θ {:.0}°, φ {:.0}°", m.peak_theta, m.peak_phi) }
}

/// The polarisations to split a pattern into: circular if it's anywhere near, else θ and φ.
fn pol_pair(m: &Metrics) -> (Pol, Pol) {
    if m.axial_ratio_db < 10.0 { (Pol::Rhcp, Pol::Lhcp) } else { (Pol::Theta, Pol::Phi) }
}

fn metric_tables(c: &Ctx, shown: &[(String, FarField)]) -> String {
    let lossless = c.r.lossless;
    let metrics: Vec<(String, Metrics, f64)> = shown.iter().map(|(name, ff)| (name.clone(), ff.metrics(), ff.f)).collect();
    let mut s = String::new();
    for chunk in metrics.chunks(6) {
        let mut head: Vec<&str> = vec![""];
        head.extend(chunk.iter().map(|m| m.0.as_str()));
        let row = |label: &str, f: &dyn Fn(&Metrics, f64) -> String| -> Vec<String> {
            let mut r = vec![esc(label)];
            r.extend(chunk.iter().map(|m| f(&m.1, m.2)));
            r
        };
        let opt = |v: Option<f64>, unit: &'static str| v.map_or("–".to_string(), |x| num(x, 1, unit));
        let rows = vec![
            row("Peak directivity", &|m, _| num(m.peak_directivity_dbi, 2, " dBi")),
            row("Peak gain", &|m, _| num(m.peak_gain_dbi, 2, " dBi")),
            row("Peak realized gain", &|m, _| num(m.peak_realized_dbi, 2, " dBi")),
            row("Effective aperture", &|m, f| {
                let lambda = C0 / f.max(1.0);
                num(10f64.powf(m.peak_realized_dbi / 10.0) * lambda * lambda / (4.0 * std::f64::consts::PI) * 1e4, 1, " cm²")
            }),
            row("Peak direction", &|m, _| direction_text(m)),
            row(if lossless { "Power balance, radiated over accepted" } else { "Radiation efficiency" }, &|m, _| num(m.rad_efficiency * 100.0, 1, " %")),
            row(if lossless { "Total efficiency (mismatch only)" } else { "Total efficiency" }, &|m, _| num(m.total_efficiency * 100.0, 1, " %")),
            row("Beam width, main plane", &|m, _| opt(m.hpbw_e, "°")),
            row("Beam width, cross plane", &|m, _| opt(m.hpbw_h, "°")),
            row("Front to back", &|m, _| opt(m.front_to_back_db, " dB")),
            row("Side lobes", &|m, _| opt(m.side_lobe_db, " dB")),
            row("Axial ratio at the peak", &|m, _| if m.axial_ratio_db > 40.0 { "linear".into() } else { num(m.axial_ratio_db, 1, " dB") }),
            row("Polarisation at the peak", &|m, _| if m.axial_ratio_db > 20.0 { "linear".to_string() } else if m.rhcp_at_peak { "RHCP".to_string() } else { "LHCP".to_string() }),
        ];
        s.push_str(&table(&head, &rows));
    }
    s
}

fn radiation(c: &Ctx) -> String {
    let r = c.r;
    if r.far_f.is_empty() || r.far.is_empty() {
        return "<p>This run has no far field. Add a frequency on the Solver setup page and run again.</p>".into();
    }
    let mut shown: Vec<(String, FarField)> = Vec::new();
    for (k, f) in r.far_f.iter().enumerate() {
        for (port, list) in r.far.iter().enumerate().take(8) {
            if let Some(ff) = list.get(k) {
                let who = if c.n > 1 { format!("Port {}, ", port + 1) } else { String::new() };
                shown.push((format!("{who}{}", c.f(*f)), ff.clone()));
            }
        }
        if c.driven
            && let Some(ff) = r.combined(k, &c.p.weights(c.n))
        {
            shown.push((format!("All ports, {}", c.f(*f)), ff));
        }
    }
    if let Some(k) = c.far_k {
        for st in &c.p.states {
            if let Some(ff) = st.weights(c.n).and_then(|w| r.combined(k, &w)) {
                shown.push((format!("{}, {}", st.name, c.f(r.far_f[k])), ff));
            }
        }
    }
    let mut s = metric_tables(c, &shown);
    if c.n > 1 {
        s.push_str("<p class=\"note\">With one port driven, power coupled into the other ports, which absorb it, counts against its efficiency.</p>");
    }
    if r.lossless {
        let metal = if r.metal.is_empty() { c.p.metal.as_str() } else { r.metal.as_str() };
        let _ = write!(s, "<p class=\"note\">Metal: {}. This model has no loss, so gain is just directivity, and the power balance only checks the far-field integral. It says nothing about the real antenna's efficiency.</p>", esc(metal));
    }
    if r.far_f.len() >= 3 {
        let points = |f: &dyn Fn(&Metrics) -> f64| -> Vec<(f64, f64)> { r.far_f.iter().enumerate().filter_map(|(k, hz)| c.headline(k).map(|h| (c.x_value(*hz), f(&h.0.metrics())))).collect() };
        let gain = Line::new("Gain", 0, points(&|m| m.peak_realized_dbi));
        let eff = Line::new("Efficiency", 0, points(&|m| m.total_efficiency * 100.0));
        let chart = |title, unit, line: Line| line_chart(&Opts { title, x_label: c.x_label(), y_unit: unit, half: true, marker: c.marker(), ..Default::default() }, &[line]);
        let _ = write!(s, "<div class=\"grid2\">{}{}</div>", chart("Peak realized gain against frequency", " dBi", gain), chart("Total efficiency against frequency", " %", eff));
    }
    for (k, f) in r.far_f.iter().enumerate() {
        let Some((ff, who)) = c.headline(k) else { continue };
        let m = ff.metrics();
        let peak = if m.peak_realized_dbi.is_finite() { m.peak_realized_dbi } else { continue };
        let (a, b) = pol_pair(&m);
        let q = Quantity::RealizedGain;
        let pair = |cut: Cut| vec![Line::new(a.label(), 0, ff.cut(cut, q, a)), Line::new(b.label(), 1, ff.cut(cut, q, b))];
        let _ = write!(s, "<h3>At {}{}</h3>", c.f(*f), if who.is_empty() { String::new() } else { format!(", {who}") });
        s.push_str("<div class=\"polars\">");
        for (name, cut, signed) in [("φ = 0° plane", Cut::Phi(0.0), true), ("φ = 90° plane", Cut::Phi(90.0), true), ("θ = 90° cone", Cut::Theta(90.0), false)] {
            s.push_str(&polar_chart(name, &pair(cut), peak, signed, " dBi"));
        }
        s.push_str("</div>");
        let _ = write!(s, "<p class=\"note\">Realized gain. Outer ring {}, then every 10 dB. θ = 0° is the +z axis; angles run clockwise from the top.</p>", num(peak, 1, " dBi"));
        let beam = Cut::Phi(m.peak_phi);
        let total = Line::new("Total", 2, ff.cut(beam, q, Pol::Total));
        let (la, lb) = (Line::new(a.label(), 0, ff.cut(beam, q, a)), Line::new(b.label(), 1, ff.cut(beam, q, b)));
        let title = format!("Through the beam, the φ = {:.0}° plane", m.peak_phi);
        s.push_str(&line_chart(&Opts { title: &title, x_label: "θ (°)", y_unit: " dBi", y_range: Some((peak - 40.0, peak + 1.0)), limit: Some((peak - 3.0, "−3 dB")), ..Default::default() }, &[la, lb, total]));
        if Some(k) == c.far_k {
            let surface = drawing::pattern_3d(&ff);
            if !surface.is_empty() {
                let _ = write!(s, "<div class=\"views\">{surface}<div><p class=\"note\">Realized gain over every direction, seen from the same angle as the drawing of the model. Distance from the centre is the level: the surface reaches out to the peak of {}, in the direction {}, and in to the centre at 30 dB under it.</p>{}</div></div>", num(peak, 1, " dBi"), direction_text(&m), chart::scale_key("30 dB under the peak", "peak"));
            }
        }
        // The same numbers for anyone who can't read them off a chart.
        let at = |cut: Cut, angle: f64| ff.cut(cut, q, Pol::Total).iter().min_by(|x, y| (x.0 - angle).abs().total_cmp(&(y.0 - angle).abs())).map_or(f64::NAN, |p| p.1);
        let planes: Vec<Vec<String>> = (-12..=12).map(|i| {
            let ang = i as f64 * 15.0;
            vec![num(ang, 0, "°"), num(at(Cut::Phi(0.0), ang), 2, " dBi"), num(at(Cut::Phi(90.0), ang), 2, " dBi")]
        }).collect();
        let cone: Vec<Vec<String>> = (0..24).map(|i| {
            let ang = i as f64 * 15.0;
            vec![num(ang, 0, "°"), num(at(Cut::Theta(90.0), ang), 2, " dBi")]
        }).collect();
        let _ = write!(s, "<details><summary>Table: realized gain every 15° at {}</summary><div class=\"grid2\">{}{}</div></details>", c.f(*f), table(&["θ", "φ = 0° plane", "φ = 90° plane"], &planes), table(&["φ", "θ = 90° cone"], &cone));
    }
    s
}

fn fields(c: &Ctx) -> String {
    let mut s = String::new();
    if let Some(cur) = &c.r.currents {
        match drawing::currents_figure(cur) {
            Some(f) => {
                let at = c.r.far_f.first().map_or(String::new(), |hz| format!(" at {}", c.f(*hz)));
                let _ = write!(s, "<h3>Current on the metal</h3>{f}<p class=\"note\">Current magnitude on every wire segment or triangle{at}, for port 1 driven.</p>");
            }
            None if !cur.current.is_empty() => s.push_str("<p class=\"note\">The current is on too many elements to draw here. The app's Fields page shows it.</p>"),
            None => {}
        }
    }
    let planes: Vec<String> = c.r.planes.iter().filter_map(drawing::plane_figure).collect();
    if !planes.is_empty() {
        let _ = write!(s, "<h3>Field planes</h3><div class=\"planes\">{}</div>{}<p class=\"note\">At the design frequency, from the first port's run.</p>", planes.join(""), chart::scale_key("−40 dB", "0 dB (99th percentile)"));
    }
    s
}

fn power_text(dbm: f64) -> String {
    if !dbm.is_finite() {
        return "–".into();
    }
    let watts = 10f64.powf(dbm / 10.0) / 1e3;
    let power = if watts >= 1.0 { format!("{watts:.2} W") } else if watts >= 1e-3 { format!("{:.0} mW", watts * 1e3) } else { format!("{:.1} µW", watts * 1e6) };
    format!("{} ({power})", num(dbm, 1, " dBm"))
}

fn link_budget(c: &Ctx) -> String {
    let (Some(l), Some((ff, who))) = (c.x.link, c.far_k.and_then(|k| c.headline(k))) else { return String::new() };
    let m = ff.metrics();
    if !m.peak_realized_dbi.is_finite() {
        return String::new();
    }
    let g = m.peak_realized_dbi;
    let range = sparam::friis_range_m(l.tx_dbm, g, l.rx_gain_dbi, m.f, l.sensitivity_dbm + l.margin_db);
    let mut s = String::from("<p><span class=\"chip\">Estimate: free space</span></p>");
    let mut rows: Vec<(&str, String)> = vec![
        ("Transmit power", power_text(l.tx_dbm)),
        ("This antenna's realized gain", format!("{} at {}{}", num(g, 2, " dBi"), c.f(m.f), if who.is_empty() { String::new() } else { format!(", {who}") })),
        ("EIRP", power_text(l.tx_dbm + g)),
        ("Receive antenna gain", num(l.rx_gain_dbi, 1, " dBi")),
        ("Receiver sensitivity", num(l.sensitivity_dbm, 1, " dBm")),
        ("Fade margin", num(l.margin_db, 1, " dB")),
        ("Range with that margin", if range >= 1000.0 { format!("{:.1} km", range / 1000.0) } else { format!("{range:.1} m") }),
    ];
    let received: Vec<String> = [1.0, 3.0, 10.0, 30.0, 100.0].iter().map(|d| format!("{} at {d:.0} m", num(sparam::friis_received_dbm(l.tx_dbm, g, l.rx_gain_dbi, m.f, *d), 1, " dBm"))).collect();
    rows.push(("Received power", received.join(", ")));
    s.push_str(&pairs(&rows));
    s.push_str("<p class=\"note\">Friis in free space, with both antennas lined up at their peaks. Walls, floors, fading and interference aren't in it. These inputs are the ones on the Link budget page.</p>");
    s
}

fn rfid_limits(c: &Ctx) -> String {
    if c.x.regions.is_empty() || !c.has_network() {
        return String::new();
    }
    let net = c.net;
    let (lo, hi) = (net.f[0], net.f[net.f.len() - 1]);
    // A 900 MHz antenna isn't necessarily for RFID, so say nothing unless it looks it.
    let name = c.p.name.to_lowercase();
    if !(c.p.template.contains("rfid") || name.contains("rfid") || (lo >= 859.999e6 && hi <= 960.001e6)) {
        return String::new();
    }
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    for region in c.x.regions.iter().filter(|g| g.band.0 <= hi && g.band.1 >= lo) {
        let centre = (region.band.0 + region.band.1) / 2.0;
        // The gain has to be from near this band for the sum to mean anything.
        let pattern = c.r.far_index(centre).filter(|k| (c.r.far_f[*k] - centre).abs() / centre < 0.03).and_then(|k| c.headline(k));
        let power = match (region.eirp_dbm, pattern) {
            (None, _) => "No confirmed limit".to_string(),
            (Some(_), None) => "No pattern near this band".to_string(),
            (Some(eirp), Some((ff, _))) => {
                let m = ff.metrics();
                let by_eirp = eirp - m.peak_realized_dbi;
                let binding = region.conducted_dbm.filter(|v| *v < by_eirp);
                let p = binding.unwrap_or(by_eirp);
                format!("{}{}", power_text(p), if binding.is_some() { ", its reader limit" } else { "" })
            }
        };
        let covered = lo <= region.band.0 && hi >= region.band.1;
        let inside: Vec<usize> = (0..net.f.len()).filter(|k| net.f[*k] >= region.band.0 && net.f[*k] <= region.band.1).collect();
        let fit = if !covered || inside.is_empty() {
            "Band runs past the sweep".to_string()
        } else {
            let worst = (0..c.n).flat_map(|p| {
                let db = net.sij_db(p, p);
                inside.iter().map(move |k| db[*k]).collect::<Vec<f64>>()
            }).fold(f64::NEG_INFINITY, f64::max);
            format!("{} {}", if worst <= -10.0 { "✓" } else { "✕" }, num(worst, 1, " dB"))
        };
        let label = if region.active { format!("<b>{}</b> (active region)", esc(&region.name)) } else { esc(&region.name) };
        rows.push(vec![label, format!("{}–{} MHz", trim_mhz(region.band.0), trim_mhz(region.band.1)), esc(&region.limit_text), region.eirp_dbm.map_or("–".into(), |v| num(v, 2, " dBm")), power, fit, esc(&region.status)]);
        if !region.source.is_empty() {
            sources.push(format!("<li><b>{}:</b> {}</li>", esc(&region.name), esc(&region.source)));
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    let mut s = table(&["Region", "Band", "Limit as stated", "EIRP limit", "Reader power that reaches it", "Worst match in the band", "Rule"], &rows);
    s.push_str("<p class=\"note\">Reader power is the EIRP limit less this antenna's simulated peak realized gain, measured at the antenna's connector: cable and connector loss would allow more at the reader. Worst match is over every port. Read a regulator's own text before relying on any of these, above all those from a secondary source.</p>");
    if !sources.is_empty() {
        let _ = write!(s, "<details><summary>Where these limits come from</summary><ul>{}</ul></details>", sources.join(""));
    }
    s
}

fn trim_mhz(hz: f64) -> String {
    let t = format!("{:.1}", hz / 1e6);
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Margins through the gate on the scale the app uses: a read's threshold is neutral grey.
fn margin_map(title: &str, map: &HeatMap) -> String {
    let (nu, nv) = (map.u.len(), map.v.len());
    if nu == 0 || nv == 0 || map.db.len() != nu * nv {
        return String::new();
    }
    let color = |db: f64| -> [u8; 3] {
        let t = (db / 15.0).clamp(-1.0, 1.0);
        let (end, k) = if t >= 0.0 { ([0x2a, 0x78, 0xd6], t) } else { ([0xe3, 0x49, 0x48], -t) };
        let mid = [0xf0, 0xef, 0xec];
        std::array::from_fn(|i| (mid[i] as f64 + (end[i] as f64 - mid[i] as f64) * k).round() as u8)
    };
    let span = |a: &[f64]| if a.len() > 1 { (a[a.len() - 1] - a[0]).max(1e-6) } else { 1.0 };
    let (su, sv) = (span(&map.u), span(&map.v));
    let scale = (360.0 / su).min(240.0 / sv);
    let (w, h) = (su * scale, sv * scale);
    let (left, top) = (40.0, 26.0);
    let (cw, ch) = (w / nu as f64, h / nv as f64);
    const AXIS: [&str; 3] = ["x", "y", "z"];
    let (un, vn) = (AXIS[map.axes.0.min(2)], AXIS[map.axes.1.min(2)]);
    let mut cells = String::new();
    let mut pts = Vec::new();
    for j in 0..nv {
        for i in 0..nu {
            let v = map.db[i + nu * j];
            if !v.is_finite() {
                continue;
            }
            let (x, y) = (left + i as f64 * cw, top + h - (j + 1) as f64 * ch);
            let _ = write!(cells, r#"<rect x="{x:.1}" y="{y:.1}" width="{:.1}" height="{:.1}" fill="{}"/>"#, cw.max(0.5) - 0.5, ch.max(0.5) - 0.5, chart::hex(color(v)));
            let what = format!("{un} {:.2} m, {vn} {:.2} m: {}", map.u[i], map.v[j], if v >= 0.0 { "reads" } else { "doesn't read" });
            pts.push(format!("[{:.1},{:.1},{},{},\"\"]", x + cw / 2.0, y + ch / 2.0, serde_json::to_string(&minus(format!("{v:+.1} dB"))).unwrap_or_default(), serde_json::to_string(&what).unwrap_or_default()));
        }
    }
    let data = format!("{{\"pts\":[{}]}}", pts.join(",")).replace('&', "&amp;").replace('\'', "&#39;");
    let mut s = format!(r#"<svg class="margin" viewBox="0 0 {:.0} {:.0}" role="img" tabindex="0" aria-label="{}" data-hover='{data}'>"#, left + w + 16.0, top + h + 36.0, esc(title));
    let _ = write!(s, r#"<text x="{left}" y="16" class="t">{}</text>{cells}"#, esc(title));
    for a in [map.u[0], map.u[nu - 1]] {
        let _ = write!(s, r#"<text x="{:.1}" y="{:.1}" class="a" text-anchor="middle">{}</text>"#, left + (a - map.u[0]) / su * w, top + h + 14.0, minus(format!("{a:.1}")));
    }
    let _ = write!(s, r#"<text x="{:.1}" y="{:.1}" class="a" text-anchor="middle">{un} (m)</text>"#, left + w / 2.0, top + h + 28.0);
    for a in [map.v[0], map.v[nv - 1]] {
        let _ = write!(s, r#"<text x="{:.1}" y="{:.1}" class="a" text-anchor="end">{}</text>"#, left - 5.0, top + h - (a - map.v[0]) / sv * h + 4.0, minus(format!("{a:.1}")));
    }
    let _ = write!(s, r#"<text x="10" y="{:.1}" class="a">{vn}</text></svg>"#, top + h / 2.0);
    s
}

fn gate_test(c: &Ctx) -> String {
    let Some(g) = &c.x.gate else { return String::new() };
    let (r, spec) = (&g.report, &g.spec);
    let share = |v: f64| format!("{:.1} %", v * 100.0);
    let mut s = format!("<p><span class=\"chip\">{}</span>No real gate has been measured.</p>", esc(&g.evidence));
    let loud = r.states.iter().filter_map(|st| st.eirp_dbm).fold(f64::NEG_INFINITY, f64::max);
    let mut tiles = vec![
        tile(&share(r.read), "Read walking through", &format!("{} to {}, goal {}", share(g.read_range.0), share(g.read_range.1), share(spec.min_read))),
        tile(&share(r.stray), "On display, read", &format!("{} to {}, goal under {}", share(g.stray_range.0), share(g.stray_range.1), share(spec.max_stray))),
    ];
    if let Some((axis, v)) = r.by_orientation.first() {
        tiles.push(tile(&share(*v), "Worst way to point", axis));
    }
    if loud.is_finite() {
        tiles.push(tile(&num(loud, 1, " dBm"), "EIRP, loudest state", &format!("limit used {}", num(spec.eirp_limit_dbm, 1, " dBm"))));
    }
    tiles.push(tile(&r.power_for_goal.map_or("Not reached".to_string(), |p| num(p.0, 1, " dBm")), "Reader power for the goal", ""));
    tiles.push(tile(if r.reverse_limited { "Reader" } else { "Tag chip" }, "What limits a read", ""));
    let _ = write!(s, "<div class=\"tiles\">{}</div>", tiles.join(""));
    if let Some((tx, read, stray)) = r.legal {
        s.push_str(&verdict(false, &format!("{} EIRP is over the {} limit. Keeping to it means {} at the reader, where {} read and {} on display read.", num(loud, 1, " dBm"), num(spec.eirp_limit_dbm, 0, " dBm"), num(tx, 1, " dBm"), share(read), share(stray))));
    }
    let _ = write!(s, "<div class=\"maps\">{}{}</div>{}", margin_map("Walks read, by lane and orientation", &r.lane_map), margin_map("The worst orientation at mid height", &r.plane_map), {
        let stops = "linear-gradient(90deg,#e34948,#f0efec,#2a78d6)";
        format!(r#"<div class="key"><span>−15 dB, doesn't read</span><i style="background:{stops}"></i><span>+15 dB, reads</span></div>"#)
    });
    let states: Vec<Vec<String>> = r.states.iter().map(|st| vec![esc(&st.name), share(st.read), share(st.stray), num(st.tarc_db, 1, " dB"), st.eirp_dbm.map_or("–".into(), |v| num(v, 1, " dBm")), num(st.peak_field_vm, 1, " V/m")]).collect();
    if !states.is_empty() {
        s.push_str("<h3>Each state on its own</h3>");
        s.push_str(&table(&["State", "Read alone", "On display, read", "Reflected at the feed", "EIRP", "Largest field in the zone"], &states));
    }
    let orient: Vec<Vec<String>> = r.by_orientation.iter().map(|(axis, v)| vec![esc(axis), share(*v)]).collect();
    if !orient.is_empty() {
        s.push_str("<h3>By how the tag points</h3>");
        s.push_str(&table(&["Tag along", "Walks read"], &orient));
    }
    s.push_str("<h3>What it was judged with</h3>");
    s.push_str(&pairs(&[
        ("Frequency", c.f(r.f)),
        ("Reader power", num(spec.tx_dbm, 1, " dBm")),
        ("Loss to the antenna", num(spec.feed_loss_db, 1, " dB")),
        ("Tag chip threshold", num(spec.tag_sensitivity_dbm, 1, " dBm")),
        ("Tag antenna gain", num(spec.tag_gain_dbi, 1, " dBi")),
        ("Loss from what the tag is on", format!("{}, spread {}", num(spec.product_loss_db.0, 1, " dB"), num(spec.product_loss_db.1, 1, " dB"))),
        ("Reader sensitivity", num(spec.reader_sensitivity_dbm, 1, " dBm")),
        ("Walking speed", num(spec.walk_speed, 1, " m/s")),
        ("Time in each state", format!("{:.0} ms", spec.dwell_s * 1e3)),
        ("EIRP limit", format!("{}, as set on the gate page", num(spec.eirp_limit_dbm, 1, " dBm"))),
    ]));
    let items: String = g.assumptions.iter().chain(&r.warnings).map(|a| format!("<li>{}</li>", esc(a))).collect();
    if !items.is_empty() {
        let _ = write!(s, "<h3>What this assumes</h3><ul>{items}</ul>");
    }
    s
}

fn method(c: &Ctx) -> String {
    let (r, net) = (c.r, c.net);
    let mut rows: Vec<(&str, String)> = vec![("Evidence", "Simulated. Nothing in this report has been checked against a real antenna.".to_string())];
    rows.push(("Solver", esc(&r.solver)));
    rows.push(("Problem size", size_text(r)));
    rows.push(("Run time", format!("{:.1} s", r.seconds)));
    if let (Some(a), Some(b)) = (net.f.first(), net.f.last()) {
        let step = if net.f.len() > 1 { format!(", every {:.3} MHz", (b - a) / (net.f.len() - 1) as f64 / 1e6) } else { String::new() };
        rows.push(("Sweep", format!("{} to {}, {} points{step}", c.f(*a), c.f(*b), net.f.len())));
    }
    if r.solver.contains("MoM") && c.p.setup.points == net.f.len() {
        rows.push(("Solved frequencies", format!("{}, interpolated to the sweep", c.p.setup.mom_samples)));
    }
    let metal = if r.metal.is_empty() { c.p.metal.as_str() } else { r.metal.as_str() };
    rows.push(("Metal", esc(&if r.lossless { format!("{metal} (no loss modelled)") } else { metal.to_string() })));
    rows.push(("Substrate", esc(&c.p.substrate)));
    let impedances: Vec<String> = net.z0.iter().map(|z| format!("{z:.1} Ω")).collect();
    rows.push(("Reference impedance", impedances.join(", ")));
    if r.density > 0.0 {
        rows.push(("Mesh density", esc(&format!("{:.0} {} at the top of the sweep", r.density, r.density_unit))));
    }
    if r.end_db > 0.0 {
        let left = 10f64.powf(-r.end_db / 20.0);
        let error_db = |level_db: f64| 20.0 * (1.0 + left / 10f64.powf(level_db / 20.0)).log10();
        rows.push(("Stopped at", format!("−{:.0} dB of energy, cutting off about {:.1} % of the amplitude: S11 near −20 dB is good to about {:.1} dB, near −30 dB to about {:.1} dB", r.end_db, left * 100.0, error_db(-20.0), error_db(-30.0))));
    }
    rows.push((
        "Mesh convergence",
        match r.convergence {
            None => "Not checked: no mesh convergence study was run for this result".to_string(),
            Some(m) if m.converged => format!("Converged over {} passes: last ΔS {:.4}, about {:.4} left", m.passes, m.delta_s, m.remaining),
            Some(m) => format!("Not converged after {} passes: last ΔS {:.4}, about {:.4} left", m.passes, m.delta_s, m.remaining),
        },
    ));
    rows.push(("Feed model", format!("{}: no feed parasitics are modelled", if r.solver.contains("FDTD") { "Lumped port" } else { "Ideal delta gap" })));
    if !r.far_f.is_empty() {
        let list: Vec<String> = r.far_f.iter().map(|f| c.f(*f)).collect();
        rows.push(("Far-field frequencies", list.join(", ")));
    }
    if let Some(ff) = r.far.first().and_then(|p| p.first()) {
        rows.push(("Far-field grid", format!("{:.1}° in θ, {:.1}° in φ", ff.theta_step(), ff.phi_step())));
    }
    if c.has_network() {
        rows.push(("How the ports are driven", if c.driven { "As set under Between the ports".to_string() } else if c.n > 1 { "Each port on its own, others terminated".to_string() } else { "One port".to_string() }));
    }
    let mut s = pairs(&rows);

    // Whether the result can be believed, by the same rules a run applies to itself.
    let slack = if r.end_db > 0.0 { 4.0 * 10f64.powf(-r.end_db / 20.0) } else { 2e-3 };
    let passivity = checks::passivity_warnings(net, slack);
    let balance = checks::balance_warnings(net, &r.far, r.lossless, "");
    let mut checks_list = Vec::new();
    if c.has_network() {
        checks_list.push(if passivity.is_empty() { verdict(true, &format!("Passive at all {} frequencies: no port returns more power than it receives.", net.f.len())) } else { verdict(false, "Not passive everywhere. See the notes.") });
    }
    if !r.far.is_empty() {
        let text = if r.lossless { "Power balance is within 2 % on every far field." } else { "No far field radiates more than its port accepts." };
        checks_list.push(if balance.is_empty() { verdict(true, text) } else { verdict(false, "A far field's power balance is off. See the notes.") });
    }
    if c.n > 1 && c.has_network() {
        let mut worst = 0.0f64;
        for m in &net.s {
            for i in 0..c.n {
                for j in 0..i {
                    worst = worst.max((m[i][j] - m[j][i]).norm());
                }
            }
        }
        checks_list.push(if worst.is_finite() && worst < 0.02 { verdict(true, &format!("Reciprocal: Sij and Sji differ by at most {worst:.4}.")) } else { verdict(false, &format!("Sij and Sji differ by up to {worst:.3}, more than a passive model should.")) });
    }
    if r.stopped_short() {
        checks_list.push(verdict(false, "The run stopped at its step limit, so its narrow-band numbers can't be relied on."));
    }
    if !checks_list.is_empty() {
        let _ = write!(s, "<h3>Checks</h3>{}", checks_list.concat());
    }
    if !c.x.mesh_study.is_empty() {
        let unit = if r.density_unit.is_empty() { "Density".to_string() } else { format!("Density ({})", r.density_unit) };
        let finite = |v: f64, d: usize, u: &str| if v.is_finite() { num(v, d, u) } else { "–".to_string() };
        let rows: Vec<Vec<String>> = c.x.mesh_study.iter().map(|m| vec![format!("{:.0}", m.density), m.size.to_string(), format!("{:.1} s", m.seconds), c.f(m.f_res), num(m.peak_dbi, 2, " dBi"), finite(m.delta_s, 4, ""), finite(m.gain_change_db, 3, " dB")]).collect();
        let _ = write!(s, "<h3>Mesh study</h3>{}", table(&[&unit, "Size", "Time", "Best match", "Peak gain", "ΔS", "Gain change"], &rows));
    }
    s.push_str("<h3>What isn't in the model</h3><ul>");
    s.push_str("<li>Anything around the antenna that isn't listed as a part: mount, housing, walls, floor, people.</li>");
    s.push_str("<li>Cable, connector, balun and matching network, unless they are parts.</li>");
    s.push_str("<li>Manufacturing tolerances: every dimension and material value is nominal.</li>");
    if c.geometry.as_ref().is_ok_and(|g| g.ground) {
        s.push_str("<li>A real ground plane: this one is infinite and perfect, so only the upper half radiates.</li>");
    }
    s.push_str("</ul>");

    // What the run said, and whatever these checks found that it didn't.
    let mut notes = r.warnings.clone();
    for w in passivity {
        if !notes.contains(&w) {
            notes.push(w);
        }
    }
    for w in balance {
        if !notes.iter().any(|n| n.starts_with(w.trim_end())) {
            notes.push(w.trim_end().to_string());
        }
    }
    if !notes.is_empty() {
        // A check on a broken sample has no percentage to give.
        let items: String = notes.iter().map(|w| format!("<li>{}</li>", esc(&w.replace("NaN %", "an undefined share")))).collect();
        let _ = write!(s, "<h3>Notes</h3><ul>{items}</ul>");
    }
    s
}

fn variables(c: &Ctx) -> String {
    let row = |p: &crate::project::Param| {
        let v = c.vars.get(&p.name).copied().unwrap_or(f64::NAN);
        let range = p.range.map_or("–".to_string(), |(a, b)| format!("{} to {}", param_text(&p.name, a), param_text(&p.name, b)));
        vec![esc(&p.name), format!("<code>{}</code>", esc(&p.expr)), param_text(&p.name, v), range, esc(&p.note)]
    };
    let head = ["Name", "Expression", "Value in this run", "Allowed range", "Note"];
    let inputs: Vec<Vec<String>> = c.p.params.iter().filter(|p| !p.derived).map(row).collect();
    let derived: Vec<Vec<String>> = c.p.params.iter().filter(|p| p.derived).map(row).collect();
    let mut s = table(&head, &inputs);
    if !derived.is_empty() {
        let _ = write!(s, "<details><summary>{} derived values</summary>{}</details>", derived.len(), table(&head, &derived));
    }
    s
}

fn data(c: &Ctx) -> String {
    if !c.has_network() {
        return String::new();
    }
    let safe: String = c.p.name.chars().map(|ch| if ch.is_alphanumeric() { ch } else { '_' }).collect();
    let name = format!("{safe}.s{}p", c.n);
    let file = drawing::base64(c.net.to_touchstone().as_bytes());
    format!("<p><a download=\"{}\" href=\"data:text/plain;base64,{file}\">Download the S-parameters ({})</a>, which this page carries inside it. Save and export in the app also writes the far field for other tools.</p>", esc(&name), esc(&name))
}

/// A time as a report prints it. `secs` is since 1970, in UTC.
pub fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Civil date from a day count, by eras of 400 years.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rest / 3600, rest % 3600 / 60)
}

/// A self-contained HTML datasheet for a reconfigurable antenna: every state, side by side.
pub fn reconfig_html(project: &Project, report: &crate::reconfig::Report, assumptions: &[String]) -> String {
    let rc = &project.reconfig;
    let mut body = String::new();
    let _ = write!(body, "<h1>{}</h1><p class=m>Reconfigurable antenna: {} parts, {} states. Simulated, with typical part values; not measured.</p>", esc(&project.name), rc.elements.len(), report.states.len());
    body.push_str("<h2>Parts</h2><table><tr><th>Part</th><th>Across</th><th>Type</th><th>Values</th></tr>");
    for e in &rc.elements {
        let _ = write!(body, "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(&e.name), esc(&e.port), esc(&e.device.part), esc(&e.device.summary()));
    }
    body.push_str("</table><h2>Bias</h2><table><tr><th>State</th>");
    for e in &rc.elements {
        let _ = write!(body, "<th>{}</th>", esc(&e.name));
    }
    body.push_str("<th>Bias</th></tr>");
    for s in &report.states {
        let _ = write!(body, "<tr><td>{}</td>", esc(&s.name));
        for (e, v) in rc.elements.iter().zip(&s.settings) {
            let _ = write!(body, "<td>{}</td>", esc(&e.device.describe(*v)));
        }
        let _ = write!(body, "<td>{:.1} mA, {:.1} mW</td></tr>", s.dc_ma, s.dc_mw);
    }
    body.push_str("</table><h2>States</h2><table><tr><th>State</th><th>At</th><th>Match</th><th>Gain</th><th>Beam θ, φ</th><th>Polarisation</th><th>Lost in parts</th></tr>");
    for s in &report.states {
        let (gain, beam) = s.metrics.as_ref().map_or(("–".to_string(), "–".to_string()), |m| (format!("{:.1} dBi", m.peak_realized_dbi), format!("{:.0}°, {:.0}°", m.peak_theta, m.peak_phi)));
        let _ = write!(body, "<tr><td>{}</td><td>{:.3} GHz</td><td>{:.1} dB</td><td>{gain}</td><td>{beam}</td><td>{}</td><td>{:.1} %</td></tr>", esc(&s.name), s.f_op / 1e9, s.s_op_db, esc(&s.polarisation), s.part_loss * 100.0);
    }
    body.push_str("</table>");
    let series: Vec<Line> = report.states.iter().enumerate().map(|(k, s)| Line::new(s.name.as_str(), k, s.curve.iter().map(|c| (c.0 / 1e9, c.1)).collect())).collect();
    let _ = write!(body, "<h2>Reflection</h2>{}", line_chart(&Opts { title: "Reflection at the feed", x_label: "Frequency (GHz)", y_unit: " dB", limit: Some((-10.0, "−10 dB")), ..Default::default() }, &series));
    if !report.warnings.is_empty() {
        let items: String = report.warnings.iter().map(|w| format!("<li>{}</li>", esc(w))).collect();
        let _ = write!(body, "<h2>Warnings</h2><ul>{items}</ul>");
    }
    let items: String = assumptions.iter().map(|a| format!("<li>{}</li>", esc(a))).collect();
    let _ = write!(body, "<h2>What this assumes</h2><ul>{items}</ul>");
    page::wrap(&project.name, &body)
}

fn size_text(r: &SimResult) -> String {
    if r.solver.contains("FDTD") {
        format!("{:.2} million cells", r.size as f64 / 1e6)
    } else {
        format!("{} unknowns", r.size)
    }
}

/// A value with an SI prefix that fits an antenna: 2.45 G, 12.2 m, 1.6 m.
pub fn fmt_value(v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.4} G", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.3} M", v / 1e6)
    } else if a != 0.0 && a < 1.0 && a >= 1e-3 {
        format!("{:.3} m", v * 1e3)
    } else if a != 0.0 && a < 1e-3 {
        format!("{:.2} µ", v * 1e6)
    } else {
        format!("{v:.3}")
    }
}

/// `db20` for a value that may be absent, as table text.
pub fn db_text(v: Option<f64>) -> String {
    v.map_or("–".into(), |x| format!("{:.1} dB", db20(x)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::farfield::FarField;
    use crate::sparam::Network;

    #[test]
    fn report_has_every_section_and_is_escaped() {
        let mut p = Project::from_template("dipole", Some("1GHz")).unwrap();
        p.name = "A <test> & co".into();
        let f: Vec<f64> = crate::linspace(0.7e9, 1.3e9, 31);
        let s: Vec<Vec<Vec<C64>>> = f.iter().map(|x| vec![vec![C64::new((x - 1e9) / 1e9, 0.1)]]).collect();
        let mut ff = FarField::grid(1e9, 5.0, 5.0);
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                ff.e_theta[ti][pi] = C64::new(ff.theta[ti].to_radians().sin(), 0.0);
            }
        }
        let r = SimResult { network: Network { f, z0: vec![73.0], s }, far: vec![vec![ff]], far_f: vec![1e9], solver: "MoM, wires".into(), ..Default::default() };
        let h = html(&p, &r);
        for needle in ["Summary", "S-parameters", "Smith chart", "Radiation pattern", "Design variables", "A &lt;test&gt; &amp; co"] {
            assert!(h.contains(needle), "missing {needle}");
        }
        assert!(!h.contains("<test>"));
    }

    fn small_result() -> (Project, SimResult) {
        let p = Project::from_template("dipole", Some("1GHz")).unwrap();
        let f: Vec<f64> = crate::linspace(0.7e9, 1.3e9, 31);
        let s: Vec<Vec<Vec<C64>>> = f.iter().map(|x| vec![vec![C64::new((x - 1e9) / 1e9, 0.1)]]).collect();
        let mut ff = FarField::grid(1e9, 5.0, 5.0);
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                ff.e_theta[ti][pi] = C64::new(ff.theta[ti].to_radians().sin(), 0.0);
            }
        }
        ff.p_accepted = ff.p_rad();
        ff.p_incident = 1.0;
        let r = SimResult { network: Network { f, z0: vec![73.0], s }, far: vec![vec![ff]], far_f: vec![1e9], solver: "MoM, wires".into(), ..Default::default() };
        (p, r)
    }

    /// Without these a report cannot be audited.
    #[test]
    fn a_report_says_how_its_numbers_were_made() {
        let (p, mut r) = small_result();
        let plain = html(&p, &r);
        for needle in ["Metal", "PEC", "Reference impedance", "73", "Mesh convergence", "Not checked", "Feed model", "no feed parasitics"] {
            assert!(plain.contains(needle), "missing {needle}");
        }
        r.density = 24.0;
        r.density_unit = "segments per λ".into();
        assert!(html(&p, &r).contains("24 segments per λ"));
        r.solver = "FDTD time domain (openEMS)".into();
        r.density = 20.0;
        r.density_unit = "cells per λ".into();
        r.end_db = 50.0;
        let fdtd = html(&p, &r);
        assert!(fdtd.contains("20 cells per λ") && fdtd.contains("−50 dB"), "stop level");
        r.convergence = Some(crate::project::MeshCheck { converged: false, passes: 3, delta_s: 0.05, remaining: 0.1 });
        assert!(html(&p, &r).contains("Not converged"));
        r.convergence = Some(crate::project::MeshCheck { converged: true, passes: 3, delta_s: 0.005, remaining: 0.01 });
        assert!(html(&p, &r).contains("Converged"));
    }

    /// On PEC this figure checks the far-field integral. It says nothing about loss.
    #[test]
    fn a_lossless_model_reports_a_power_balance_not_an_efficiency() {
        let (p, mut r) = small_result();
        r.lossless = true;
        let h = html(&p, &r);
        assert!(h.contains("Power balance"), "label");
        assert!(!h.contains("Radiation efficiency"), "label");
        r.lossless = false;
        assert!(html(&p, &r).contains("Radiation efficiency"));
    }

    #[test]
    fn variables_read_in_their_own_units() {
        assert_eq!(param_text("h", 0.008191), "8.191 mm");
        assert_eq!(param_text("f0", 915e6), "915.00 MHz");
        assert_eq!(param_text("f0", 2.45e9), "2.4500 GHz");
        assert_eq!(param_text("Z0", 50.0), "50.0 Ω");
        assert_eq!(param_text("N", 8.0), "8.0000");
        assert_eq!(param_text("L", 1.5), "1.500 m");
    }

    #[test]
    fn values_get_prefixes() {
        assert_eq!(fmt_value(2.45e9), "2.4500 G");
        assert_eq!(fmt_value(0.0123), "12.300 m");
        assert_eq!(fmt_value(73.0), "73.000");
    }

    fn dipole_at(f0: &str) -> (Project, SimResult) {
        let p = Project::from_template("dipole", Some(f0)).unwrap();
        let hz = p.f0();
        let f: Vec<f64> = crate::linspace(0.7 * hz, 1.3 * hz, 61);
        let s: Vec<Vec<Vec<C64>>> = f.iter().map(|x| vec![vec![C64::new((x - hz) / hz * 2.0, 0.05)]]).collect();
        let mut ff = FarField::grid(hz, 5.0, 5.0);
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                ff.e_theta[ti][pi] = C64::new(ff.theta[ti].to_radians().sin(), 0.0);
            }
        }
        ff.p_accepted = ff.p_rad();
        ff.p_incident = 1.0;
        let vars = p.vars().unwrap();
        let r = SimResult { network: Network { f, z0: vec![50.0], s }, far: vec![vec![ff]], far_f: vec![hz], solver: "MoM, wires".into(), vars: vars.into_iter().collect(), seconds: 1.0, ..Default::default() };
        (p, r)
    }

    /// Two coupled ports, with a far field for each.
    fn two_ports() -> (Project, SimResult) {
        let (p, mut r) = dipole_at("1GHz");
        let ff = r.far[0][0].clone();
        r.far = vec![vec![ff.clone()], vec![ff]];
        r.network.z0 = vec![50.0, 50.0];
        r.network.s = r.network.f.iter().map(|x| {
            let g = C64::new((x - 1e9) / 1e9 * 2.0, 0.05);
            let t = C64::new(0.1, 0.02);
            vec![vec![g, t], vec![t, g]]
        }).collect();
        (p, r)
    }

    #[test]
    fn the_report_shows_the_antenna_itself() {
        let (p, r) = dipole_at("1GHz");
        let h = html(&p, &r);
        for needle in ["The antenna", "At an angle", "Front: x across, z up", "Overall size", "In wavelengths", "ka =", "Smallest Q for that size", "Feeds", "Parts", "Materials", "Peak directivity", "Effective aperture", "Key frequencies", "Bandwidth", "What isn&#39;t".trim_end_matches("&#39;t"), "Checks", "Download the S-parameters", "data:text/plain;base64,"] {
            assert!(h.contains(needle), "missing {needle}");
        }
        assert!(h.contains("Simulated, not measured"), "evidence label");
    }

    /// `≥` is what stops a narrow sweep reading as a narrow antenna.
    #[test]
    fn a_band_the_sweep_cuts_off_reads_as_at_least() {
        let (p, mut r) = dipole_at("1GHz");
        for m in &mut r.network.s {
            m[0][0] = C64::new(0.05, 0.0);
        }
        let h = html(&p, &r);
        assert!(h.contains("≥ 600.0 MHz"), "width");
        assert!(h.contains("continues past the end of the sweep"), "explained");
        // An antenna with room on both sides has a measured bandwidth, with nothing to say.
        let (p, r) = dipole_at("1GHz");
        let h = html(&p, &r);
        assert!(!h.contains("continues past the end of the sweep") && !h.contains("≥ "), "not cut off");
    }

    /// A run keeps its own numbers. Picking another RFID band after it must not relabel them.
    #[test]
    fn a_design_edited_after_the_run_doesnt_change_what_the_report_says() {
        let (mut p, r) = dipole_at("1GHz");
        let before = html(&p, &r);
        assert!(!before.contains("changed since the run"));
        let size = |h: &str| h.split("<th>Overall size</th><td>").nth(1).and_then(|t| t.split('(').next()).unwrap_or("").to_string();
        let original = size(&before);
        assert!(original.contains("mm"), "{original}");
        p.fit_band(865e6, 868e6);
        let h = html(&p, &r);
        assert!(h.contains("changed since the run"), "says so");
        assert!(h.contains("1.0000 GHz in this run"), "with the values");
        assert!(h.contains("700.0 MHz to 1300.0 MHz"), "sweep from the run");
        assert!(!h.contains("865.0 MHz"), "not the sweep it now has");
        assert_eq!(size(&h), original, "the drawing is of what was solved");
    }

    #[test]
    fn the_design_frequency_outside_the_sweep_is_not_read_off_the_nearest_end() {
        let (mut p, mut r) = dipole_at("1GHz");
        r.vars.clear();
        p.params.iter_mut().find(|q| q.name == "f0").unwrap().expr = "2.45GHz".into();
        let h = html(&p, &r);
        assert!(h.contains("isn&#39;t inside this sweep") || h.contains("isn't inside this sweep"), "note");
        assert!(h.contains("Middle of the sweep"), "row");
    }

    #[test]
    fn several_ports_get_their_coupling_and_correlation() {
        let (p, r) = two_ports();
        let h = html(&p, &r);
        for needle in ["Between the ports", "Coupling between ports", "From port 2", "Envelope correlation", "Diversity gain", "Port 2, 1000.0 MHz", "Reciprocal"] {
            assert!(h.contains(needle), "missing {needle}");
        }
        assert!(!h.contains("Port weights"), "none were set");
        // One port's report has no business with any of that.
        let (p, r) = dipole_at("1GHz");
        assert!(!html(&p, &r).contains("Between the ports"));
    }

    #[test]
    fn set_weights_add_the_combined_pattern_and_states_their_own() {
        let (mut p, r) = two_ports();
        p.drive = vec![crate::project::Drive { amp: 1.0, phase: 0.0 }, crate::project::Drive { amp: 1.0, phase: 90.0 }];
        p.states = vec![crate::project::DriveState { name: "Left <A>".into(), drive: p.drive.clone() }];
        let h = html(&p, &r);
        for needle in ["Port weights", "All ports, 1000.0 MHz", "Reflection with those weights", "Total active reflection", "Excitation states", "Left &lt;A&gt;, 1000.0 MHz"] {
            assert!(h.contains(needle), "missing {needle}");
        }
        assert!(!h.contains("Left <A>"), "escaped");
    }

    #[test]
    fn asymmetric_ports_are_flagged_as_not_reciprocal() {
        let (p, mut r) = two_ports();
        for m in &mut r.network.s {
            m[0][1] = C64::new(0.3, 0.0);
        }
        assert!(html(&p, &r).contains("more than a passive model should"));
    }

    fn region(name: &str, band: (f64, f64), eirp: f64, conducted: Option<f64>) -> RegionLimit {
        RegionLimit { name: name.into(), band, limit_text: "as stated".into(), eirp_dbm: Some(eirp), conducted_dbm: conducted, status: "verified".into(), source: "Regulator".into(), active: false }
    }

    #[test]
    fn regional_limits_turn_this_antennas_gain_into_reader_power() {
        let (mut p, r) = dipole_at("915MHz");
        p.name = "RFID reader antenna".into();
        let gain = r.far[0][0].metrics().peak_realized_dbi;
        let x = Extras { regions: vec![region("Here", (902e6, 928e6), 36.0, None), region("Capped", (902e6, 928e6), 36.0, Some(10.0)), region("Far away", (5e9, 6e9), 30.0, None)], ..Default::default() };
        let h = html_with(&p, &r, &x);
        assert!(h.contains("UHF RFID limits"), "section");
        assert!(h.contains(&format!("{:.1} dBm", 36.0 - gain)), "limit less gain");
        assert!(h.contains("its reader limit"), "the conducted limit can bind first");
        assert!(!h.contains("Far away"), "not in this sweep");
        // The same band on something else isn't RFID's business.
        let (p, r) = dipole_at("915MHz");
        assert!(!html_with(&p, &r, &x).contains("UHF RFID limits"), "wide sweep, not named for RFID");
    }

    #[test]
    fn a_pattern_from_another_band_isnt_used_for_a_regions_power() {
        let (mut p, mut r) = dipole_at("915MHz");
        p.name = "RFID".into();
        r.far_f = vec![960e6];
        r.far[0][0].f = 960e6;
        let x = Extras { regions: vec![region("Low band", (865e6, 868e6), 35.15, None)], ..Default::default() };
        assert!(html_with(&p, &r, &x).contains("No pattern near this band"));
    }

    #[test]
    fn the_link_budget_is_labelled_an_estimate() {
        let (p, r) = dipole_at("1GHz");
        assert!(!html(&p, &r).contains("Link budget"), "not without its inputs");
        let x = Extras { link: Some(LinkInputs { tx_dbm: 30.0, rx_gain_dbi: 2.0, sensitivity_dbm: -70.0, margin_db: 10.0 }), ..Default::default() };
        let h = html_with(&p, &r, &x);
        assert!(h.contains("Estimate: free space"), "label");
        let g = r.far[0][0].metrics().peak_realized_dbi;
        let want = sparam::friis_range_m(30.0, g, 2.0, 1e9, -60.0);
        assert!(h.contains(&format!("{want:.1} m")), "range");
        assert!(h.contains("1.00 W"), "power in watts");
    }

    #[test]
    fn the_gate_test_keeps_its_evidence_label_and_its_limit() {
        let (p, r) = dipole_at("915MHz");
        let spec = GateSpec { f: 915e6, ..GateSpec::for_model(&p.geometry().unwrap(), 915e6) };
        let report = GateReport {
            f: 915e6,
            read: 0.9,
            stray: 0.1,
            by_orientation: vec![("z".into(), 0.8)],
            states: vec![crate::recipes::gate::StateScore { name: "State <1>".into(), read: 0.5, stray: 0.1, tarc_db: -12.0, eirp_dbm: Some(37.0), peak_field_vm: 4.0 }],
            legal: Some((29.0, 0.7, 0.05)),
            lane_map: HeatMap { axes: (0, 2), u: vec![0.0, 1.0], v: vec![0.0, 1.0], db: vec![3.0, -3.0, f64::NAN, 15.0] },
            ..Default::default()
        };
        let x = Extras { gate: Some(GateSection { report, spec, read_range: (0.8, 0.95), stray_range: (0.05, 0.2), evidence: "Estimate".into(), assumptions: vec!["Tags don't disturb the field".into()] }), ..Default::default() };
        let h = html_with(&p, &r, &x);
        for needle in ["RFID gate test", ">Estimate<", "No real gate has been measured", "over the 36 dBm limit", "State &lt;1&gt;", "Tags don&#39;t".trim_end_matches("&#39;t"), "Walks read, by lane"] {
            assert!(h.contains(needle), "missing {needle}");
        }
        assert!(!h.contains("NaN"), "a sample too close to the metal draws nothing");
    }

    #[test]
    fn broken_results_leave_a_readable_page() {
        // A dead port: full reflection, and no far field at all.
        let (p, mut r) = dipole_at("1GHz");
        for m in &mut r.network.s {
            m[0][0] = C64::new(1.0, 0.0);
        }
        r.far.clear();
        r.far_f.clear();
        let h = html(&p, &r);
        assert!(h.contains("over 1000"), "VSWR");
        assert!(h.contains("Not matched"));
        assert!(h.contains("no far field"), "says what's missing");
        // Numbers that aren't.
        let (p, mut r) = dipole_at("1GHz");
        for m in r.network.s.iter_mut().skip(10) {
            m[0][0] = C64::new(f64::NAN, f64::NAN);
        }
        r.far[0][0].p_incident = 0.0;
        r.far[0][0].p_accepted = 0.0;
        let h = html(&p, &r);
        for bad in ["NaN", ">inf", " inf ", "-inf"] {
            if let Some(at) = h.find(bad) {
                let around: String = h[at..].chars().take(60).collect();
                let before: String = h[..at].chars().rev().take(120).collect::<Vec<_>>().into_iter().rev().collect();
                panic!("{bad} in the page: {before}|{around}");
            }
        }
        // Nothing solved at all.
        let (p, _) = dipole_at("1GHz");
        let h = html(&p, &SimResult::default());
        assert!(h.contains("The antenna") && !h.contains("Key frequencies"));
    }

    /// It has to work on a laptop in a lab with no network, and it can't phone home.
    #[test]
    fn the_page_loads_nothing_from_outside_itself() {
        let (p, r) = dipole_at("1GHz");
        let h = html(&p, &r);
        for bad in ["src=\"http", "href=\"http", "url(http", "@import", "<link "] {
            assert!(!h.contains(bad), "{bad}");
        }
        assert!(h.starts_with("<!doctype html>") && h.ends_with("</html>"));
    }

    #[test]
    fn the_same_result_writes_the_same_page() {
        let (p, r) = two_ports();
        assert_eq!(html(&p, &r), html(&p, &r));
    }

    #[test]
    fn the_export_time_prints_as_a_date() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14 22:13 UTC");
        // A leap day.
        assert_eq!(utc_stamp(1_709_164_800), "2024-02-29 00:00 UTC");
    }

    #[test]
    fn the_reconfigurable_report_still_draws_with_the_new_charts() {
        let p = Project::from_template("dipole", Some("1GHz")).unwrap();
        let state = crate::reconfig::StateReport { name: "A".into(), settings: vec![], curve: vec![(1e9, -5.0), (2e9, -12.0)], f_op: 1e9, s_op_db: -5.0, resonances: vec![], bands: vec![], far_f: None, metrics: None, polarisation: String::new(), part_loss: 0.0, parts: vec![], dc_ma: 0.0, dc_mw: 0.0 };
        let report = crate::reconfig::Report { states: vec![state], ..Default::default() };
        let h = reconfig_html(&p, &report, &["Typical part values".to_string()]);
        assert!(h.contains("Reflection at the feed") && h.contains("<svg"), "chart");
        assert!(h.contains("Simulated"), "evidence");
    }
}

/// A design variable's value in the unit it means, judged from its name: variables
/// are plain numbers in SI, and `8.191 m` must not stand for 8.2 mm.
pub fn param_text(name: &str, v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    match name {
        "f0" | "f_lo" | "f_hi" => {
            if v >= 1e9 {
                format!("{:.4} GHz", v / 1e9)
            } else {
                format!("{:.2} MHz", v / 1e6)
            }
        }
        "Z0" | "Redge" => format!("{v:.1} Ω"),
        "cone" => format!("{v:.1}°"),
        "N" | "n_dir" | "tau" | "sigma" | "taper" | "hand" | "spokes" | "eeff" | "msA" | "msB" | "er" | "tand" => format!("{v:.4}"),
        // Rows the user adds have no known meaning.
        _ if name.starts_with("var") => format!("{v}"),
        _ => {
            let a = v.abs();
            if a >= 1.0 {
                format!("{v:.3} m")
            } else if a >= 1e-3 || a == 0.0 {
                format!("{:.3} mm", v * 1e3)
            } else {
                format!("{:.1} µm", v * 1e6)
            }
        }
    }
}
