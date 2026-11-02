//! Result plots: line charts with hover read-out, polar pattern cuts, the Smith
//! chart and field images. All drawn with the egui painter on the light theme.

use antenna_core::C64;
use bevy_egui::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, pos2, vec2};

use crate::theme;

pub const BLUE: Color32 = Color32::from_rgb(38, 110, 228);
pub const ORANGE: Color32 = Color32::from_rgb(226, 118, 22);
pub const GREEN: Color32 = Color32::from_rgb(40, 158, 84);
pub const PURPLE: Color32 = Color32::from_rgb(131, 56, 196);
pub const RED: Color32 = Color32::from_rgb(214, 62, 58);
pub const TEAL: Color32 = Color32::from_rgb(20, 150, 160);
pub const GREY: Color32 = Color32::from_rgb(120, 126, 136);

/// Colours for many overlaid curves.
pub const PALETTE: [Color32; 7] = [BLUE, ORANGE, GREEN, PURPLE, RED, TEAL, GREY];

pub struct Series<'a> {
    pub name: &'a str,
    pub color: Color32,
    pub points: &'a [(f64, f64)],
}

#[derive(Default, Clone, Copy)]
pub struct ChartOptions<'a> {
    pub title: &'a str,
    pub unit: &'a str,
    pub x_unit: &'a str,
    pub height: f32,
    /// A dashed horizontal limit line.
    pub limit: Option<f64>,
    /// A shaded x range (the band of interest).
    pub band: Option<(f64, f64)>,
    /// Fixed y range, otherwise from the data.
    pub y_range: Option<(f64, f64)>,
    /// Vertical marker at this x.
    pub marker: Option<f64>,
}

/// Round tick spacing for a span.
fn nice_step(span: f64, target: f64) -> f64 {
    let raw = span / target.max(1.0);
    let mag = 10f64.powf(raw.abs().max(1e-30).log10().floor());
    let norm = raw / mag;
    let step = if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    };
    step * mag
}

fn number(v: f64, step: f64) -> String {
    let d = if step >= 1.0 { 0 } else { (-step.log10()).ceil().min(4.0) as usize };
    format!("{v:.d$}")
}

/// A line chart. Hovering reads every curve at the cursor.
pub fn line_chart(ui: &mut egui::Ui, o: ChartOptions, series: &[Series]) {
    let h = if o.height > 0.0 { o.height } else { 170.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let f = FontId::proportional(10.5);
    p.text(rect.left_top() + vec2(8.0, 5.0), Align2::LEFT_TOP, o.title, FontId::proportional(12.0), theme::TEXT);
    let finite: Vec<(f64, f64)> = series.iter().flat_map(|s| s.points.iter().copied()).filter(|v| v.0.is_finite() && v.1.is_finite()).collect();
    if finite.len() < 2 {
        p.text(rect.center(), Align2::CENTER_CENTER, "No data", f, theme::WEAK);
        return;
    }
    let (x0, x1) = finite.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(v.0), a.1.max(v.0)));
    let (mut y0, mut y1) = o.y_range.unwrap_or_else(|| finite.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(v.1), a.1.max(v.1))));
    if let Some(l) = o.limit {
        y0 = y0.min(l);
        y1 = y1.max(l);
    }
    if o.y_range.is_none() {
        let pad = ((y1 - y0) * 0.08).max(1e-6);
        y0 -= pad;
        y1 += pad;
    }
    let plot = Rect::from_min_max(rect.min + vec2(40.0, 24.0), rect.max - vec2(10.0, 20.0));
    let span_x = (x1 - x0).max(1e-12);
    let span_y = (y1 - y0).max(1e-12);
    let to = |x: f64, y: f64| pos2(plot.left() + ((x - x0) / span_x) as f32 * plot.width(), plot.bottom() - ((y - y0) / span_y) as f32 * plot.height());
    if let Some((a, b)) = o.band {
        let r = Rect::from_min_max(to(a.clamp(x0, x1), y1), to(b.clamp(x0, x1), y0));
        p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(45, 110, 240, 22));
    }
    let grid = Stroke::new(1.0, theme::STROKE);
    let ys = nice_step(span_y, (plot.height() / 34.0) as f64);
    let mut v = (y0 / ys).ceil() * ys;
    while v <= y1 {
        let a = to(x0, v);
        p.hline(plot.left()..=plot.right(), a.y, grid);
        p.text(pos2(plot.left() - 4.0, a.y), Align2::RIGHT_CENTER, number(v, ys), f.clone(), theme::WEAK);
        v += ys;
    }
    let xs = nice_step(span_x, (plot.width() / 70.0) as f64);
    let mut v = (x0 / xs).ceil() * xs;
    while v <= x1 {
        let a = to(v, y0);
        p.vline(a.x, plot.top()..=plot.bottom(), grid);
        if a.x < plot.right() - 30.0 {
            p.text(pos2(a.x, plot.bottom() + 3.0), Align2::CENTER_TOP, number(v, xs), f.clone(), theme::WEAK);
        }
        v += xs;
    }
    p.text(pos2(plot.right(), rect.bottom() - 2.0), Align2::RIGHT_BOTTOM, o.x_unit, f.clone(), theme::WEAK);
    if let Some(l) = o.limit {
        let y = to(x0, l).y;
        let dashes = egui::Shape::dashed_line(&[pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, theme::DANGER), 5.0, 3.0);
        p.extend(dashes);
    }
    if let Some(m) = o.marker {
        let x = to(m, y0).x;
        p.vline(x, plot.top()..=plot.bottom(), Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)));
    }
    let clip = p.with_clip_rect(plot.intersect(ui.clip_rect()));
    for s in series {
        let line: Vec<Pos2> = s.points.iter().filter(|v| v.1.is_finite()).map(|v| to(v.0, v.1)).collect();
        if line.len() >= 2 {
            clip.add(egui::Shape::line(line, Stroke::new(1.6, s.color)));
        }
    }
    // Legend, right to left.
    let mut lx = rect.right() - 8.0;
    if series.len() <= 8 {
        for s in series.iter().rev() {
            let g = p.layout_no_wrap(s.name.to_string(), f.clone(), s.color);
            lx -= g.size().x;
            p.galley(pos2(lx, rect.top() + 6.0), g, s.color);
            lx -= 10.0;
        }
    }
    if let Some(pos) = resp.hover_pos()
        && plot.contains(pos)
    {
        let x = x0 + ((pos.x - plot.left()) / plot.width()) as f64 * span_x;
        p.vline(pos.x, plot.top()..=plot.bottom(), Stroke::new(1.0, Color32::from_black_alpha(80)));
        let mut lines = vec![format!("{} {}", number(x, xs / 100.0), o.x_unit)];
        for s in series.iter().take(8) {
            if let Some(v) = s.points.iter().min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs())) {
                p.circle_filled(to(v.0, v.1), 3.0, s.color);
                lines.push(format!("{}: {:.3}{}", s.name, v.1, o.unit));
            }
        }
        let text = lines.join("\n");
        let g = p.layout_no_wrap(text, f.clone(), theme::TEXT);
        let mut at = pos + vec2(10.0, -g.size().y - 6.0);
        if at.x + g.size().x > rect.right() - 4.0 {
            at.x = pos.x - g.size().x - 10.0;
        }
        at.y = at.y.max(rect.top() + 2.0);
        let bg = Rect::from_min_size(at, g.size()).expand(4.0);
        p.rect(bg, 3.0, theme::PANEL, Stroke::new(1.0, theme::STROKE), egui::StrokeKind::Inside);
        p.galley(at, g, theme::TEXT);
    }
}

/// Polar pattern cut: `(angle°, dB)`. 0° is up; rings every 10 dB below `peak`.
pub fn polar_chart(ui: &mut egui::Ui, title: &str, curves: &[(&str, Color32, &[(f64, f64)])], peak: f64, size: f32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(egui::RichText::new(title).size(12.0).color(theme::TEXT));
        let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        let p = ui.painter_at(rect);
        let c = rect.center();
        let r = size * 0.44;
        let floor = peak - 30.0;
        p.circle_filled(c, r, theme::RAISED);
        let f = FontId::proportional(9.5);
        for k in 1..=3 {
            let rr = r * k as f32 / 3.0;
            p.circle_stroke(c, rr, Stroke::new(1.0, if k == 3 { theme::STROKE_STRONG } else { theme::STROKE }));
        }
        for a in (0..360).step_by(30) {
            let t = (a as f32).to_radians();
            let v = vec2(t.sin(), -t.cos());
            p.line_segment([c, c + v * r], Stroke::new(1.0, theme::STROKE));
            if a % 90 == 0 {
                p.text(c + v * (r + 8.0), Align2::CENTER_CENTER, format!("{a}"), f.clone(), theme::WEAK);
            }
        }
        for (_, col, cut) in curves {
            let pts: Vec<Pos2> = cut
                .iter()
                .map(|(ang, db)| {
                    let rr = (((db - floor) / (peak - floor).max(1e-6)).clamp(0.0, 1.0)) as f32 * r;
                    let a = (*ang as f32).to_radians();
                    c + vec2(a.sin(), -a.cos()) * rr
                })
                .collect();
            if pts.len() > 2 {
                p.add(egui::Shape::line(pts, Stroke::new(1.5, *col)));
            }
        }
        p.text(rect.right_bottom() - vec2(1.0, 1.0), Align2::RIGHT_BOTTOM, format!("{peak:.1} dBi"), f.clone(), theme::WEAK);
        if let Some(pos) = resp.hover_pos() {
            let d = pos - c;
            let ang = d.x.atan2(-d.y).to_degrees() as f64;
            let target = |cut: &[(f64, f64)]| {
                let span = cut.first().zip(cut.last()).map_or(0.0, |(a, b)| b.0 - a.0);
                let a = if span > 359.0 && cut.first().is_some_and(|v| v.0 >= 0.0) { ang.rem_euclid(360.0) } else { ang };
                cut.iter().min_by(|x, y| (x.0 - a).abs().total_cmp(&(y.0 - a).abs())).copied()
            };
            let mut lines = Vec::new();
            for (name, _, cut) in curves {
                if let Some(v) = target(cut) {
                    lines.push(format!("{name} {:.0}°: {:.2} dB", v.0, v.1));
                }
            }
            let g = p.layout_no_wrap(lines.join("\n"), f.clone(), theme::TEXT);
            let at = rect.left_top() + vec2(0.0, 0.0);
            let bg = Rect::from_min_size(at, g.size()).expand(3.0);
            p.rect_filled(bg, 3.0, Color32::from_white_alpha(230));
            p.galley(at, g, theme::TEXT);
        }
    });
}

/// Smith chart of reflection coefficients; hover reads frequency and impedance.
pub fn smith_chart(ui: &mut egui::Ui, curves: &[(&str, Color32, &[C64])], f_ghz: &[f64], z0: f64, size: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size), Sense::hover());
    let p = ui.painter_at(rect);
    let c = rect.center();
    let r = (size * 0.46).min(rect.width() * 0.46);
    p.circle_filled(c, r, theme::RAISED);
    let grid = Stroke::new(1.0, theme::STROKE);
    let map = |g: C64| c + vec2(g.re as f32, -g.im as f32) * r;
    let f = FontId::proportional(9.5);
    // Constant-resistance and constant-reactance circles, drawn from Γ(z) directly.
    let trace = |zs: &mut dyn Iterator<Item = C64>| -> Vec<Pos2> { zs.map(|z| map((z - 1.0) / (z + 1.0))).collect() };
    for rn in [0.0f64, 0.2, 0.5, 1.0, 2.0, 5.0] {
        let pts = trace(&mut (-200..=200).map(|k| {
            let x = 40f64 * (k as f64 / 200.0).powi(5);
            C64::new(rn, x)
        }));
        p.add(egui::Shape::line(pts, grid));
    }
    for xn in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        for s in [1.0, -1.0] {
            let pts = trace(&mut (0..=100).map(|k| C64::new(100f64.powf(k as f64 / 100.0 * 1.5) - 1.0, s * xn)));
            p.add(egui::Shape::line(pts, grid));
        }
    }
    p.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], grid);
    p.circle_stroke(c, r, Stroke::new(1.2, theme::STROKE_STRONG));
    for rn in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        let z = C64::new(rn, 0.0);
        let g = (z - 1.0) / (z + 1.0);
        p.text(map(g) + vec2(2.0, 2.0), Align2::LEFT_TOP, format!("{}", rn * z0), f.clone(), theme::WEAK);
    }
    for (_, col, gamma) in curves {
        let pts: Vec<Pos2> = gamma.iter().map(|g| map(*g)).collect();
        if pts.len() > 1 {
            p.add(egui::Shape::line(pts.clone(), Stroke::new(1.6, *col)));
            p.circle_filled(pts[0], 3.0, *col);
            p.circle_stroke(pts[pts.len() - 1], 3.0, Stroke::new(1.5, *col));
        }
    }
    if let Some(pos) = resp.hover_pos()
        && let Some((_, col, gamma)) = curves.first()
        && let Some((k, g)) = gamma.iter().enumerate().min_by(|a, b| (map(*a.1) - pos).length().total_cmp(&(map(*b.1) - pos).length()))
        && (map(*g) - pos).length() < 40.0
    {
        p.circle_filled(map(*g), 4.0, *col);
        let z = (C64::new(1.0, 0.0) + *g) / (C64::new(1.0, 0.0) - *g) * z0;
        let t = format!("{:.3} GHz\nZ = {:.1} {:+.1}j Ω\n|Γ| = {:.1} dB", f_ghz.get(k).copied().unwrap_or(0.0), z.re, z.im, 20.0 * g.norm().max(1e-9).log10());
        let gal = p.layout_no_wrap(t, FontId::proportional(11.0), theme::TEXT);
        let at = map(*g) + vec2(8.0, 8.0);
        let bg = Rect::from_min_size(at, gal.size()).expand(4.0);
        p.rect(bg, 3.0, theme::PANEL, Stroke::new(1.0, theme::STROKE), egui::StrokeKind::Inside);
        p.galley(at, gal, theme::TEXT);
    }
}

/// Dark blue → cyan → yellow → red over 0..1.
pub fn heat(t: f32) -> Color32 {
    let stops = [(0.0, [24.0, 30.0, 110.0]), (0.33, [30.0, 150.0, 220.0]), (0.66, [240.0, 220.0, 60.0]), (1.0, [226.0, 40.0, 30.0])];
    let t = t.clamp(0.0, 1.0);
    let mut c = stops[stops.len() - 1].1;
    for w in stops.windows(2) {
        if t <= w[1].0 {
            let k = (t - w[0].0) / (w[1].0 - w[0].0);
            c = std::array::from_fn(|i| w[0].1[i] + (w[1].1[i] - w[0].1[i]) * k);
            break;
        }
    }
    Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8)
}

/// A horizontal colour bar with its end labels.
pub fn heat_legend(ui: &mut egui::Ui, low: &str, high: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    let n = 64;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let x0 = r.left() + r.width() * i as f32 / n as f32;
        let x1 = r.left() + r.width() * (i + 1) as f32 / n as f32 + 0.5;
        ui.painter().rect_filled(Rect::from_min_max(pos2(x0, r.top()), pos2(x1, r.bottom())), 0.0, heat(t));
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(low).size(11.0).color(theme::WEAK));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(egui::RichText::new(high).size(11.0).color(theme::WEAK)));
    });
}
