//! A self-contained HTML datasheet for a simulated antenna: summary, variable
//! table, S-parameters, Smith chart, pattern cuts. Plots are inline SVG.

use std::fmt::Write;

use crate::farfield::{Cut, Pol, Quantity};
use crate::project::{Project, SimResult};
use crate::{C64, db20};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A line chart over `x`; several series share the axes.
fn line_chart(title: &str, x_label: &str, y_label: &str, series: &[(&str, &str, Vec<(f64, f64)>)], limit: Option<f64>) -> String {
    let (w, h, l, r, t, b) = (640.0, 280.0, 56.0, 16.0, 32.0, 40.0);
    let pts: Vec<(f64, f64)> = series.iter().flat_map(|s| s.2.iter().copied()).filter(|p| p.1.is_finite()).collect();
    if pts.len() < 2 {
        return String::new();
    }
    let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (mut y0, mut y1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    if let Some(v) = limit {
        y0 = y0.min(v);
        y1 = y1.max(v);
    }
    let pad = ((y1 - y0) * 0.06).max(0.5);
    y0 -= pad;
    y1 += pad;
    let sx = |x: f64| l + (x - x0) / (x1 - x0).max(1e-30) * (w - l - r);
    let sy = |y: f64| h - b - (y - y0) / (y1 - y0).max(1e-30) * (h - t - b);
    let mut s = format!(r#"<svg viewBox="0 0 {w} {h}" role="img" aria-label="{}">"#, esc(title));
    let _ = write!(s, r#"<text x="{l}" y="20" class="t">{}</text>"#, esc(title));
    for k in 0..=5 {
        let y = y0 + (y1 - y0) * k as f64 / 5.0;
        let (x2, yy, xt, yt) = (w - r, sy(y), l - 6.0, sy(y) + 4.0);
        let _ = write!(s, r#"<line x1="{l}" x2="{x2}" y1="{yy:.1}" y2="{yy:.1}" class="g"/><text x="{xt}" y="{yt:.1}" class="a" text-anchor="end">{y:.1}</text>"#);
    }
    for k in 0..=6 {
        let x = x0 + (x1 - x0) * k as f64 / 6.0;
        let _ = write!(s, r#"<text x="{:.1}" y="{}" class="a" text-anchor="middle">{x:.3}</text>"#, sx(x), h - b + 16.0);
    }
    let _ = write!(s, r#"<text x="{}" y="{}" class="a" text-anchor="middle">{}</text>"#, (l + w - r) / 2.0, h - 6.0, esc(x_label));
    let _ = write!(s, r#"<text x="14" y="{}" class="a" text-anchor="middle" transform="rotate(-90 14 {0})">{}</text>"#, (t + h - b) / 2.0, esc(y_label));
    if let Some(v) = limit {
        let _ = write!(s, r##"<line x1="{l}" x2="{}" y1="{1:.1}" y2="{1:.1}" stroke="#d33" stroke-dasharray="4 3"/>"##, w - r, sy(v));
    }
    for (name, color, data) in series {
        let d: Vec<String> = data.iter().filter(|p| p.1.is_finite()).map(|p| format!("{:.1},{:.1}", sx(p.0), sy(p.1))).collect();
        let _ = write!(s, r#"<polyline fill="none" stroke="{color}" stroke-width="1.8" points="{}"><title>{}</title></polyline>"#, d.join(" "), esc(name));
    }
    let mut lx = w - r;
    for (name, color, _) in series.iter().rev() {
        let _ = write!(s, r#"<text x="{lx}" y="20" class="a" fill="{color}" text-anchor="end">{}</text>"#, esc(name));
        lx -= 14.0 + name.chars().count() as f64 * 6.5;
    }
    s.push_str("</svg>");
    s
}

fn smith_chart(title: &str, gamma: &[C64]) -> String {
    let (size, c, r) = (360.0, 180.0, 160.0);
    let mut s = format!(r#"<svg viewBox="0 0 {size} {size}" class="sq" role="img" aria-label="{}">"#, esc(title));
    let _ = write!(s, r#"<circle cx="{c}" cy="{c}" r="{r}" class="g" fill="none"/><line x1="{}" x2="{}" y1="{c}" y2="{c}" class="g"/>"#, c - r, c + r);
    for rn in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        let (cc, rr) = (rn / (1.0 + rn), 1.0 / (1.0 + rn));
        let _ = write!(s, r#"<circle cx="{:.1}" cy="{c}" r="{:.1}" class="g" fill="none"/>"#, c + cc * r, rr * r);
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
    let d: Vec<String> = gamma.iter().map(|g| format!("{:.1},{:.1}", c + g.re * r, c - g.im * r)).collect();
    let _ = write!(s, r##"<polyline fill="none" stroke="#2d6ef0" stroke-width="1.8" points="{}"/>"##, d.join(" "));
    s.push_str("</svg>");
    s
}

fn polar_chart(title: &str, cut: &[(f64, f64)], peak: f64) -> String {
    let (size, c, r) = (300.0, 150.0, 120.0);
    let floor = peak - 30.0;
    let mut s = format!(r#"<svg viewBox="0 0 {size} {}" class="sq" role="img" aria-label="{}">"#, size + 22.0, esc(title));
    let _ = write!(s, r#"<text x="{c}" y="16" class="a" text-anchor="middle">{}</text>"#, esc(title));
    for k in 1..=3 {
        let _ = write!(s, r#"<circle cx="{c}" cy="{}" r="{:.1}" class="g" fill="none"/>"#, c + 14.0, r * k as f64 / 3.0);
    }
    for a in (0..360).step_by(30) {
        let t = (a as f64).to_radians();
        let _ = write!(s, r#"<line x1="{c}" y1="{}" x2="{:.1}" y2="{:.1}" class="g"/>"#, c + 14.0, c + r * t.sin(), c + 14.0 - r * t.cos());
    }
    let d: Vec<String> = cut
        .iter()
        .map(|(ang, db)| {
            let rr = ((db - floor) / (peak - floor)).clamp(0.0, 1.0) * r;
            let t = ang.to_radians();
            format!("{:.1},{:.1}", c + rr * t.sin(), c + 14.0 - rr * t.cos())
        })
        .collect();
    let _ = write!(s, r##"<polygon fill="rgba(226,118,22,.12)" stroke="#e27616" stroke-width="1.6" points="{}"/>"##, d.join(" "));
    let _ = write!(s, r#"<text x="{c}" y="{}" class="a" text-anchor="middle">{floor:.0} … {peak:.1} dBi, 10 dB rings</text>"#, size + 16.0);
    s.push_str("</svg>");
    s
}

/// The whole report as one HTML page.
pub fn html(project: &Project, r: &SimResult) -> String {
    let net = &r.network;
    let ghz: Vec<f64> = net.f.iter().map(|f| f / 1e9).collect();
    let mut body = String::new();
    let _ = write!(body, "<h1>{}</h1><p class=m>{} · {} · {:.0} s · {}</p>", esc(&project.name), esc(&r.solver), crate::templates::by_id(&project.template).map_or("", |t| t.title), r.seconds, size_text(r));
    // Summary.
    let mut rows = String::new();
    let mut row = |k: &str, v: String| {
        let _ = write!(rows, "<tr><th>{}</th><td>{}</td></tr>", esc(k), esc(&v));
    };
    row("Sweep", format!("{:.3} to {:.3} GHz", project.setup.f_lo / 1e9, project.setup.f_hi / 1e9));
    let metal = if r.metal.is_empty() { project.metal.as_str() } else { r.metal.as_str() };
    row("Metal", if r.lossless { format!("{metal} (no loss modelled)") } else { metal.to_string() });
    let impedances: Vec<String> = net.z0.iter().map(|z| format!("{z:.1} Ω")).collect();
    row("Reference impedance", impedances.join(", "));
    if r.density > 0.0 {
        row("Mesh density", format!("{:.0} {} at the top of the sweep", r.density, r.density_unit));
    }
    if r.end_db > 0.0 {
        row("Stopped at", format!("−{:.0} dB of energy", r.end_db));
    }
    row(
        "Mesh convergence",
        match r.convergence {
            None => "Not checked: no mesh convergence study was run for this result".to_string(),
            Some(c) if c.converged => format!("Converged over {} passes: last ΔS {:.4}, about {:.4} left", c.passes, c.delta_s, c.remaining),
            Some(c) => format!("Not converged after {} passes: last ΔS {:.4}, about {:.4} left", c.passes, c.delta_s, c.remaining),
        },
    );
    row("Feed model", format!("{}: no feed parasitics are modelled", if r.solver.contains("FDTD") { "Lumped port" } else { "Ideal delta gap" }));
    for port in 0..net.ports() {
        let name = port + 1;
        for (f, db) in net.resonances(port).iter().take(3) {
            row(&format!("Port {name} resonance"), format!("{:.3} GHz at {db:.1} dB", f / 1e9));
        }
        for (a, b) in net.bandwidths(port, -10.0).iter().take(2) {
            let fm = (a + b) / 2.0;
            row(&format!("Port {name} −10 dB band"), format!("{:.3}–{:.3} GHz ({:.0} MHz, {:.1} %)", a / 1e9, b / 1e9, (b - a) / 1e6, (b - a) / fm * 100.0));
        }
    }
    if let Some(m) = r.metrics(0, 0) {
        row("Frequency of the pattern", format!("{:.3} GHz", m.f / 1e9));
        row("Peak directivity", format!("{:.2} dBi", m.peak_directivity_dbi));
        row("Peak gain", format!("{:.2} dBi", m.peak_gain_dbi));
        row("Peak realized gain", format!("{:.2} dBi", m.peak_realized_dbi));
        row("Peak direction", format!("θ {:.0}°, φ {:.0}°", m.peak_theta, m.peak_phi));
        // With no loss in the model this only checks the far-field integral.
        row(if r.lossless { "Power balance, radiated over accepted" } else { "Radiation efficiency" }, format!("{:.1} %", m.rad_efficiency * 100.0));
        row(if r.lossless { "Total efficiency (mismatch only)" } else { "Total efficiency" }, format!("{:.1} %", m.total_efficiency * 100.0));
        if let Some(v) = m.hpbw_e {
            row("Beam width, main plane", format!("{v:.1}°"));
        }
        if let Some(v) = m.hpbw_h {
            row("Beam width, cross plane", format!("{v:.1}°"));
        }
        if let Some(v) = m.front_to_back_db {
            row("Front to back", format!("{v:.1} dB"));
        }
        if let Some(v) = m.side_lobe_db {
            row("Side lobes", format!("{v:.1} dB"));
        }
        row("Axial ratio at the peak", if m.axial_ratio_db > 40.0 { "linear".into() } else { format!("{:.1} dB ({})", m.axial_ratio_db, if m.rhcp_at_peak { "RHCP" } else { "LHCP" }) });
    }
    let _ = write!(body, "<h2>Summary</h2><table>{rows}</table>");
    // S-parameters and Smith chart.
    if net.ports() > 0 {
        let palette = ["#2d6ef0", "#e27616", "#289e54", "#8338c4"];
        let mut series = Vec::new();
        let names: Vec<String> = (0..net.ports()).flat_map(|i| (0..net.ports()).map(move |j| format!("S{}{}", i + 1, j + 1))).collect();
        let mut k = 0;
        for i in 0..net.ports() {
            for j in 0..net.ports() {
                if i < j {
                    continue;
                }
                let data: Vec<(f64, f64)> = ghz.iter().copied().zip(net.sij_db(i, j)).collect();
                series.push((names[i * net.ports() + j].as_str(), palette[k % palette.len()], data));
                k += 1;
            }
        }
        let _ = write!(body, "<h2>S-parameters</h2>{}", line_chart("|S| (dB)", "Frequency (GHz)", "dB", &series, Some(-10.0)));
        let v: Vec<(f64, f64)> = ghz.iter().copied().zip(net.vswr(0).into_iter().map(|x| x.min(20.0))).collect();
        let _ = write!(body, "{}", line_chart("VSWR, port 1", "Frequency (GHz)", "VSWR", &[("VSWR", "#2d6ef0", v)], Some(2.0)));
        let z = net.zin(0);
        let re: Vec<(f64, f64)> = ghz.iter().copied().zip(z.iter().map(|c| c.re.clamp(-2000.0, 2000.0))).collect();
        let im: Vec<(f64, f64)> = ghz.iter().copied().zip(z.iter().map(|c| c.im.clamp(-2000.0, 2000.0))).collect();
        let _ = write!(body, "{}", line_chart("Input impedance, port 1", "Frequency (GHz)", "Ω", &[("R", "#289e54", re), ("X", "#e27616", im)], None));
        let gamma: Vec<C64> = net.s.iter().map(|m| m[0][0]).collect();
        let _ = write!(body, "<h2>Smith chart, port 1</h2>{}", smith_chart("Smith chart", &gamma));
    }
    // Patterns.
    for (k, f) in r.far_f.iter().enumerate() {
        let Some(ff) = r.far.first().and_then(|p| p.get(k)) else { continue };
        let m = ff.metrics();
        let _ = write!(body, "<h2>Radiation pattern at {:.3} GHz</h2><div class=row>", f / 1e9);
        let q = Quantity::RealizedGain;
        for (name, cut) in [("φ = 0°", Cut::Phi(0.0)), ("φ = 90°", Cut::Phi(90.0)), ("θ = 90°", Cut::Theta(90.0))] {
            let _ = write!(body, "{}", polar_chart(name, &ff.cut(cut, q, Pol::Total), m.peak_realized_dbi.max(-30.0)));
        }
        body.push_str("</div>");
    }
    // Variables.
    let mut vars = String::new();
    for p in &project.params {
        let v = r.vars.get(&p.name).copied().unwrap_or(f64::NAN);
        let _ = write!(vars, "<tr><th>{}</th><td><code>{}</code></td><td>{}</td><td>{}</td></tr>", esc(&p.name), esc(&p.expr), param_text(&p.name, v), esc(&p.note));
    }
    let _ = write!(body, "<h2>Design variables</h2><table>{vars}</table>");
    if !r.warnings.is_empty() {
        let items: String = r.warnings.iter().map(|w| format!("<li>{}</li>", esc(w))).collect();
        let _ = write!(body, "<h2>Notes</h2><ul>{items}</ul>");
    }
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{}</title><style>
body{{font:15px/1.5 system-ui,sans-serif;color:#26292e;background:#fff;margin:0 auto;max-width:880px;padding:24px 16px}}
h1{{font-size:26px;margin:0}}h2{{font-size:18px;margin:28px 0 8px}}.m{{color:#6c727c;margin:4px 0 0}}
table{{border-collapse:collapse;width:100%}}th,td{{text-align:left;padding:5px 10px 5px 0;border-bottom:1px solid #e0e3e7;font-weight:400;vertical-align:top}}th{{color:#6c727c;white-space:nowrap}}
svg{{display:block;width:100%;height:auto;max-width:640px}}svg.sq{{max-width:300px}}.row{{display:flex;flex-wrap:wrap;gap:12px}}.row svg{{flex:1 1 240px}}
.g{{stroke:#e0e3e7;stroke-width:1}}.t{{font-size:14px;fill:#26292e;font-weight:600}}.a{{font-size:11px;fill:#6c727c}}code{{font-size:13px}}
</style></head><body>{body}</body></html>"##,
        esc(&project.name)
    )
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
    const COLORS: [&str; 7] = ["#266ee4", "#e27616", "#289e54", "#8338c4", "#d63e3a", "#1496a0", "#787e88"];
    let series: Vec<(&str, &str, Vec<(f64, f64)>)> = report.states.iter().enumerate().map(|(k, s)| (s.name.as_str(), COLORS[k % COLORS.len()], s.curve.iter().map(|c| (c.0 / 1e9, c.1)).collect())).collect();
    let _ = write!(body, "<h2>Reflection</h2>{}", line_chart("Reflection at the feed", "GHz", "dB", &series, Some(-10.0)));
    if !report.warnings.is_empty() {
        let items: String = report.warnings.iter().map(|w| format!("<li>{}</li>", esc(w))).collect();
        let _ = write!(body, "<h2>Warnings</h2><ul>{items}</ul>");
    }
    let items: String = assumptions.iter().map(|a| format!("<li>{}</li>", esc(a))).collect();
    let _ = write!(body, "<h2>What this assumes</h2><ul>{items}</ul>");
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>{}</title><style>body{{font:15px/1.5 system-ui,sans-serif;color:#26292e;background:#fff;margin:0 auto;max-width:880px;padding:24px 16px}}h1{{font-size:26px;margin:0}}h2{{font-size:18px;margin:28px 0 8px}}.m{{color:#6c727c;margin:4px 0 0}}table{{border-collapse:collapse;width:100%}}th,td{{text-align:left;padding:5px 10px 5px 0;border-bottom:1px solid #e0e3e7;font-weight:400}}th{{color:#6c727c}}svg{{display:block;width:100%;height:auto;max-width:640px}}.g{{stroke:#e0e3e7;stroke-width:1}}.t{{font-size:14px;fill:#26292e;font-weight:600}}.a{{font-size:11px;fill:#6c727c}}</style></head><body>{body}</body></html>"##,
        esc(&project.name)
    )
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
