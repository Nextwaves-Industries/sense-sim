//! The "FDTD" page of the Positioning panel: install, antenna S11 / impedance /
//! pattern (optionally applied to the floor map), S21 through a scene object,
//! and the field views.

use bevy::prelude::*;
use bevy_egui::egui;
use egui::{Color32, RichText, Stroke, vec2};

use super::antenna::{Band, Design, Kind, Quality};
use super::post::{C, Pattern, Sweep};
use super::{Fdtd, Slice, Status};
use crate::rtls::planning::{AppliedAntenna, Coverage, FdtdAntenna, MapSource, Tech};
use crate::rtls::antenna::AntennaLibrary;
use crate::rtls::radio::{CalBand, CalSource, Material, MaterialCal};
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Antenna,
    Region,
    /// Whole-scene coverage.
    Scene,
}

pub struct UiState {
    pub mode: Mode,
    pub design: Design,
    pub region_band: Band,
    pub region_quality: Quality,
    pub show_field: bool,
    pub animate: bool,
    /// Whole-scene run: technology, mesh, tag height; which transmitter the
    /// field view shows (`None`: the strongest of all) and at which heights.
    pub scene_tech: Tech,
    pub scene_quality: Quality,
    pub scene_height: f32,
    pub scene_tx: Option<usize>,
    pub scene_all_levels: bool,
    /// Scene object selected in the viewport (for S21).
    pub selected: Option<(Entity, String)>,
    /// Name to save the current antenna under, and the last save's outcome.
    pub lib_name: String,
    pub lib_status: Option<String>,
    tex: std::collections::HashMap<String, egui::TextureHandle>,
    frame: f32,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            mode: Mode::Antenna,
            design: Design::tuned(Kind::Dipole, Band::Uwb5, Quality::Normal),
            region_band: Band::Ble,
            region_quality: Quality::Fast,
            show_field: true,
            animate: true,
            scene_tech: Tech::Rfid,
            scene_quality: Quality::Fast,
            scene_height: 1.2,
            scene_tx: None,
            scene_all_levels: true,
            selected: None,
            lib_name: String::new(),
            lib_status: None,
            tex: Default::default(),
            frame: 0.0,
        }
    }
}

/// Keeps `UiState::selected` in step with the scene selection.
pub fn track_selection(sel: Res<crate::scene::Selection>, objects: Query<&crate::scene::PlacedObject>, mut fdtd: ResMut<Fdtd>) {
    let s = sel.selected.and_then(|e| objects.get(e).ok().map(|o| (e, o.name.clone())));
    if fdtd.ui.selected != s {
        fdtd.ui.selected = s;
    }
}

const BLUE: Color32 = Color32::from_rgb(38, 110, 228);
const ORANGE: Color32 = Color32::from_rgb(226, 118, 22);
const GREEN: Color32 = Color32::from_rgb(40, 158, 84);

/// Line chart over frequency: series of (GHz, value), optional horizontal
/// reference line and a shaded x band.
fn xy_chart(ui: &mut egui::Ui, title: &str, unit: &str, series: &[(&str, Color32, Vec<(f64, f64)>)], hline: Option<f64>, band: Option<(f64, f64)>, height: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let pts: Vec<(f64, f64)> = series.iter().flat_map(|s| s.2.iter().copied()).filter(|v| v.1.is_finite()).collect();
    if pts.len() < 2 {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, format!("{title}: no data"), egui::FontId::proportional(11.0), theme::WEAK);
        return;
    }
    let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v.0), b.max(v.0)));
    let (mut y0, mut y1) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v.1), b.max(v.1)));
    if let Some(h) = hline {
        y0 = y0.min(h);
        y1 = y1.max(h);
    }
    let pad = ((y1 - y0) * 0.08).max(1e-6);
    y0 -= pad;
    y1 += pad;
    let plot = egui::Rect::from_min_max(rect.min + vec2(34.0, 16.0), rect.max - vec2(6.0, 14.0));
    let to = |x: f64, y: f64| egui::pos2(plot.left() + ((x - x0) / (x1 - x0).max(1e-12)) as f32 * plot.width(), plot.bottom() - ((y - y0) / (y1 - y0)) as f32 * plot.height());
    if let Some((a, b)) = band {
        let r = egui::Rect::from_min_max(to(a.max(x0), y1), to(b.min(x1), y0));
        p.rect_filled(r, 0.0, Color32::from_black_alpha(14));
    }
    if let Some(h) = hline {
        p.line_segment([to(x0, h), to(x1, h)], Stroke::new(1.0, theme::DANGER.gamma_multiply(0.7)));
    }
    for (_, col, s) in series {
        let line: Vec<egui::Pos2> = s.iter().filter(|v| v.1.is_finite()).map(|v| to(v.0, v.1)).collect();
        if line.len() >= 2 {
            p.add(egui::Shape::line(line, Stroke::new(1.6, *col)));
        }
    }
    let f = egui::FontId::proportional(10.0);
    p.text(rect.left_top() + vec2(6.0, 2.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(11.0), theme::TEXT);
    let mut lx = rect.right() - 6.0;
    for (name, col, _) in series.iter().rev() {
        let g = p.layout_no_wrap(name.to_string(), f.clone(), *col);
        lx -= g.size().x;
        p.galley(egui::pos2(lx, rect.top() + 2.0), g, *col);
        lx -= 10.0;
    }
    p.text(egui::pos2(plot.left() - 3.0, plot.top()), egui::Align2::RIGHT_TOP, format!("{y1:.0}"), f.clone(), theme::WEAK);
    p.text(egui::pos2(plot.left() - 3.0, plot.bottom()), egui::Align2::RIGHT_BOTTOM, format!("{y0:.0}"), f.clone(), theme::WEAK);
    p.text(egui::pos2(plot.left(), rect.bottom() - 1.0), egui::Align2::LEFT_BOTTOM, format!("{x0:.2} GHz"), f.clone(), theme::WEAK);
    p.text(egui::pos2(plot.right(), rect.bottom() - 1.0), egui::Align2::RIGHT_BOTTOM, format!("{x1:.2} GHz"), f.clone(), theme::WEAK);
    // Hover: read the curves at the cursor.
    if let Some(pos) = resp.hover_pos()
        && plot.contains(pos)
    {
        let x = x0 + ((pos.x - plot.left()) / plot.width()) as f64 * (x1 - x0);
        p.line_segment([egui::pos2(pos.x, plot.top()), egui::pos2(pos.x, plot.bottom())], Stroke::new(1.0, Color32::from_black_alpha(70)));
        let mut txt = format!("{x:.3} GHz");
        for (name, _, s) in series {
            if let Some(v) = s.iter().min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs())) {
                txt.push_str(&format!("\n{name}: {:.2}{unit}", v.1));
            }
        }
        p.text(pos + vec2(8.0, -4.0), egui::Align2::LEFT_BOTTOM, txt, f, theme::TEXT);
    }
}

fn polar(ui: &mut egui::Ui, title: &str, cut: &[(f64, f64)], max_dbi: f64) {
    let size = (ui.available_width() / 3.0 - 6.0).clamp(90.0, 170.0);
    ui.vertical(|ui| {
        ui.label(RichText::new(title).small().color(theme::WEAK));
        let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
        let p = ui.painter_at(rect);
        let c = rect.center();
        let r = size * 0.45;
        let floor = max_dbi - 30.0;
        for k in 0..=3 {
            let rr = r * k as f32 / 3.0;
            p.circle_stroke(c, rr, Stroke::new(1.0, Color32::from_black_alpha(if k == 3 { 60 } else { 24 })));
        }
        for a in (0..360).step_by(30) {
            let v = vec2((a as f32).to_radians().sin(), -(a as f32).to_radians().cos());
            p.line_segment([c, c + v * r], Stroke::new(1.0, Color32::from_black_alpha(14)));
        }
        let pts: Vec<egui::Pos2> = cut
            .iter()
            .map(|(ang, db)| {
                let rr = (((db - floor) / (max_dbi - floor)).clamp(0.0, 1.0)) as f32 * r;
                let a = (*ang as f32).to_radians();
                c + vec2(a.sin(), -a.cos()) * rr
            })
            .collect();
        if pts.len() > 2 {
            p.add(egui::Shape::closed_line(pts, Stroke::new(1.6, ORANGE)));
        }
        p.text(rect.right_bottom() - vec2(2.0, 2.0), egui::Align2::RIGHT_BOTTOM, format!("{floor:.0}…{max_dbi:.1} dBi"), egui::FontId::proportional(9.5), theme::WEAK);
    });
}

/// What "through an object" simulates, drawn: transmitter, the object, receiver,
/// and the wave losing strength in it. With a result, the wave after the object
/// is scaled by the simulated loss and the numbers are written on it.
fn through_diagram(ui: &mut egui::Ui, object: &str, loss_db: Option<f64>, bias_cm: Option<f32>) {
    const RED: Color32 = Color32::from_rgb(232, 86, 86);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let font = |s: f32| egui::FontId::proportional(s);
    let mid = rect.center().y - 6.0;
    let (tx, rx) = (rect.left() + 26.0, rect.right() - 26.0);
    let (wall_l, wall_r) = (rect.center().x - 16.0, rect.center().x + 16.0);

    // The object.
    let wall = egui::Rect::from_min_max(egui::pos2(wall_l, mid - 30.0), egui::pos2(wall_r, mid + 30.0));
    p.rect_filled(wall, 3.0, Color32::from_rgb(148, 154, 164));
    p.text(egui::pos2(wall.center().x, wall.bottom() + 3.0), egui::Align2::CENTER_TOP, object, font(11.0), theme::TEXT);

    // Antennas: transmitter red, receiver green (as in the field image).
    for (x, col, label) in [(tx, RED, "TX"), (rx, theme::SUCCESS, "RX")] {
        p.line_segment([egui::pos2(x, mid - 14.0), egui::pos2(x, mid + 14.0)], Stroke::new(3.0, col));
        p.circle_filled(egui::pos2(x, mid), 3.5, col);
        p.text(egui::pos2(x, mid + 18.0), egui::Align2::CENTER_TOP, label, font(11.0), col);
    }

    // The wave: full strength before the object, weaker inside, and after it
    // scaled by the simulated loss (amplitude ∝ 10^(−dB/20)).
    let after = loss_db.map_or(0.45, |l| 10f32.powf(-(l as f32) / 20.0).clamp(0.06, 1.0));
    let wave = |x0: f32, x1: f32, amp: f32, col: Color32| {
        let n = ((x1 - x0) / 2.0).max(2.0) as usize;
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|k| {
                let x = x0 + (x1 - x0) * k as f32 / n as f32;
                egui::pos2(x, mid + (x * 0.33).sin() * 12.0 * amp)
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.8, col)));
    };
    wave(tx + 6.0, wall_l, 1.0, ORANGE);
    wave(wall_l, wall_r, (1.0 + after) * 0.5, ORANGE.gamma_multiply(0.7));
    wave(wall_r, rx - 6.0, after, ORANGE.gamma_multiply(0.45 + 0.55 * after));

    // What comes out of it.
    let top = rect.top() + 2.0;
    match loss_db {
        Some(l) => {
            let col = if l > 20.0 { theme::DANGER } else if l > 6.0 { theme::WARNING } else { theme::SUCCESS };
            p.text(egui::pos2(wall.center().x, top), egui::Align2::CENTER_TOP, format!("−{l:.1} dB"), font(13.0), col);
            if let Some(b) = bias_cm {
                p.text(egui::pos2((wall_r + rx) * 0.5, top), egui::Align2::CENTER_TOP, format!("arrives late: {b:+.0} cm"), font(10.5), theme::WEAK);
            }
        }
        None => {
            p.text(egui::pos2((tx + wall_l) * 0.5, top), egui::Align2::CENTER_TOP, "strong", font(10.5), theme::WEAK);
            p.text(egui::pos2((wall_r + rx) * 0.5, top), egui::Align2::CENTER_TOP, "weaker?", font(10.5), theme::WEAK);
        }
    }
}

/// Pattern cuts: φ = 0° and φ = 90° planes (θ around, 0° = antenna axis) and the
/// θ = 90° plane (φ around).
fn cuts(p: &Pattern) -> [Vec<(f64, f64)>; 3] {
    let step = p.theta.get(1).copied().unwrap_or(5.0);
    let col = |deg: f64| ((deg / step).round() as usize).min(p.phi.len() - 1);
    let vertical = |phi: f64| {
        let (a, b) = (col(phi), col(phi + 180.0));
        let mut v: Vec<(f64, f64)> = p.theta.iter().enumerate().map(|(i, t)| (*t, p.d[i][a])).collect();
        v.extend(p.theta.iter().enumerate().rev().map(|(i, t)| (360.0 - t, p.d[i][b])));
        v
    };
    let mid = ((90.0 / step).round() as usize).min(p.theta.len() - 1);
    let horiz = p.phi.iter().enumerate().map(|(j, ph)| (*ph, p.d[mid][j])).collect();
    [vertical(0.0), vertical(90.0), horiz]
}

fn field_image(ui: &mut egui::Ui, st: &mut UiState, s: &Slice, key: &str, caption: &str) {
    let data: &[f32] = if st.animate && !s.frames.is_empty() {
        // From the clock, so several images on screen stay in step.
        st.frame = (ui.input(|i| i.time) as f32 * 12.0) % s.frames.len() as f32;
        ui.ctx().request_repaint();
        &s.frames[(st.frame as usize).min(s.frames.len() - 1)]
    } else {
        &s.abs
    };
    // Opaque in the panel (the scene overlay fades weak field out instead).
    let mut rgba = super::slice_rgba(s, data);
    rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    let img = egui::ColorImage::from_rgba_unmultiplied([s.nu, s.nv], &rgba);
    let tex = match st.tex.get_mut(key) {
        Some(t) => {
            t.set(img, egui::TextureOptions::LINEAR);
            t.clone()
        }
        None => {
            let t = ui.ctx().load_texture(key, img, egui::TextureOptions::LINEAR);
            st.tex.insert(key.to_string(), t.clone());
            t
        }
    };
    let w = ui.available_width();
    let aspect = ((s.v.1 - s.v.0) / (s.u.1 - s.u.0).max(1e-9)) as f32;
    ui.add(egui::Image::new(&tex).fit_to_exact_size(vec2(w, (w * aspect).clamp(120.0, 560.0))).corner_radius(4.0));
    ui.label(RichText::new(caption).small().color(theme::WEAK));
}

/// Colour scale of the field images: −40 dB … 0 dB.
fn field_legend(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), egui::Sense::hover());
    let n = 64;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let c = super::colormap(10f32.powf((t * 40.0 - 40.0) / 20.0));
        let x0 = r.left() + r.width() * i as f32 / n as f32;
        let x1 = r.left() + r.width() * (i + 1) as f32 / n as f32 + 0.5;
        ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(x0, r.top()), egui::pos2(x1, r.bottom())), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("−40 dB  weak").small().color(theme::WEAK));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new("strong  0 dB").small().color(theme::WEAK)));
    });
}

/// Rounded card around a section.
fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    theme::card_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Metric tiles, three per row: value over its label. Fixed widths, so a long
/// value truncates instead of pushing the panel wider.
pub fn tiles(ui: &mut egui::Ui, items: &[(String, &str, Color32)]) {
    let gap = 6.0;
    let w = ((ui.available_width() - 2.0 * gap) / 3.0).floor();
    for row in items.chunks(3) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (v, k, col) in row {
                egui::Frame::new().fill(theme::RAISED).corner_radius(5).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                    ui.set_width(w - 16.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.add(egui::Label::new(RichText::new(v).size(15.0).strong().color(*col)).truncate());
                        ui.add(egui::Label::new(RichText::new(*k).small().color(theme::WEAK)).truncate());
                    });
                });
            }
        });
    }
}

/// One-line verdict with a coloured marker.
fn verdict(ui: &mut egui::Ui, good: bool, text: &str) {
    let col = if good { theme::SUCCESS } else { theme::WARNING };
    egui::Frame::new().fill(col.gamma_multiply(0.12)).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.label(RichText::new(if good { theme::icon::CHECK_CIRCLE } else { theme::icon::WARNING_CIRCLE }).color(col));
            ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
        });
    });
}

/// Collapsible section with a strong title.
fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(open).show(ui, add);
}

/// Full-width accent call-to-action.
fn run_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let b = egui::Button::new(RichText::new(text).strong().color(theme::ON_ACCENT)).fill(theme::ACCENT).stroke(egui::Stroke::NONE).min_size(vec2(ui.available_width(), 32.0));
    ui.add_enabled(enabled, b).clicked()
}

fn status_ui(ui: &mut egui::Ui, fdtd: &mut Fdtd) {
    match fdtd.status() {
        Status::Busy(msg, p) => {
            card(ui, |ui| {
                // Title row: spinner, percentage, Cancel. The solver's own
                // message wraps below it; the bar stays thin (no text inside).
                ui.horizontal(|ui| {
                    ui.spinner();
                    let title = if p >= 0.0 { format!("Simulating… {:.0}%", p * 100.0) } else { "Working…".to_string() };
                    ui.label(RichText::new(title).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            fdtd.cancel();
                        }
                    });
                });
                if p >= 0.0 {
                    ui.add(egui::ProgressBar::new(p).desired_height(6.0));
                }
                ui.add(egui::Label::new(RichText::new(&msg).small().color(theme::WEAK)).wrap());
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        Status::Failed(e) => {
            verdict(ui, false, &format!("Simulation failed: {}", e.chars().take(300).collect::<String>()));
        }
        Status::Idle => {}
    }
}

/// What a technology's fixed devices are called.
fn infra_word(t: Tech) -> &'static str {
    match t {
        Tech::Uwb => "anchors",
        Tech::Ble => "beacons",
        Tech::Rfid => "readers",
        Tech::Wifi => "access points",
    }
}

/// The technology whose devices use an antenna of this band.
pub(super) fn band_tech(b: Band) -> Tech {
    match b {
        Band::Uwb5 | Band::Uwb9 => Tech::Uwb,
        Band::Ble => Tech::Ble,
        Band::Rfid => Tech::Rfid,
        Band::Wifi5 => Tech::Wifi,
    }
}

/// The band's calibrations.
fn cal_band(b: Band) -> CalBand {
    CalBand::of_mhz((b.center() / 1e6) as f32)
}

/// A warning for each line, in the panel's small type.
fn warning_lines(ui: &mut egui::Ui, lines: &[String]) {
    for l in lines {
        ui.label(RichText::new(l).small().color(theme::WARNING));
    }
}

/// What a region run's result can't be relied on for, in words.
fn region_warnings(res: &super::RegionResult) -> Vec<String> {
    let mut out = Vec::new();
    if res.truncated {
        out.push("A run hit its step limit before the field had decayed: the loss and delay below are cut short.".to_string());
    }
    if res.layout.shrunk {
        out.push(format!(
            "The cell budget cut the region to {:.2} × {:.2} m around the antennas, inside {}. What's cut carries on through the absorbing boundary, so this reads as a slab going on for ever: nothing past the region's edge is seen.",
            res.layout.region_max.x - res.layout.region_min.x,
            res.layout.region_max.y - res.layout.region_min.y,
            res.target
        ));
    }
    if let Some((m, cells)) = res.layout.worst.filter(|w| w.1 < super::scene::MIN_CELLS_PER_LAMBDA) {
        out.push(format!("{} gets {cells:.1} cells per wavelength inside it, under the {:.0} a field solver wants: waves in it run slow and its loss is less reliable.", m.name(), super::scene::MIN_CELLS_PER_LAMBDA));
    }
    out.extend(res.layout.thickness.iter().filter_map(|t| t.problem()));
    out
}

/// The "FDTD" page of the Positioning panel. Returns the technology whose
/// floor map should be recomputed (the applied antenna changed).
pub fn tab(ui: &mut egui::Ui, fdtd: &mut Fdtd, applied: &mut AppliedAntenna, library: &mut AntennaLibrary, cov: &mut Coverage) -> Option<Tech> {
    // Solver: built in, or openEMS if it's here and wanted.
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Field solver (FDM)").strong())
                .on_hover_text("Solves the radio waves themselves on a 3D grid. The built-in solver needs nothing installed. openEMS (openems.de) is the free alternative it can be checked against; it is unrelated to OpenEMS Edge, the energy-management system on the OpenEMS Edge page.");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match super::exe() {
                Some(_) => {
                    let mut wanted = super::prefers_openems();
                    if theme::segmented(ui, &mut wanted, &[(false, "Built in"), (true, "openEMS")]) {
                        super::set_prefers_openems(wanted);
                    }
                }
                None => {
                    let b = ui.add_enabled(!fdtd.busy(), egui::Button::new("Add openEMS (~65 MB)"));
                    if b.on_hover_text("Optional: a second solver to check results against. Downloads the official Windows build into %LOCALAPPDATA%\\SenseSim\\openems-fdtd").clicked() {
                        fdtd.install();
                    }
                    theme::badge(ui, "Built in", theme::SUCCESS);
                }
            });
        });
        ui.label(
            RichText::new("Full-wave 3D simulation of an antenna, of a signal passing through one object, or of the whole room. Slow, and only as good as its mesh and materials, so the live floor map stays ray-traced unless you switch it to a whole-scene run.")
                .small()
                .color(theme::WEAK),
        );
    });
    status_ui(ui, fdtd);
    ui.add_space(2.0);
    theme::segmented_fill(ui, &mut fdtd.ui.mode, &[(Mode::Antenna, "Antenna"), (Mode::Region, "Through object"), (Mode::Scene, "Whole scene")]);
    ui.add_space(2.0);
    let rescan = match fdtd.ui.mode {
        Mode::Antenna => antenna_ui(ui, fdtd, applied, library),
        Mode::Region => region_ui(ui, fdtd),
        Mode::Scene => scene_ui(ui, fdtd, cov),
    };
    let log = fdtd.log_tail(6);
    if !log.is_empty() {
        egui::CollapsingHeader::new(RichText::new("Solver log").small().color(theme::WEAK)).show(ui, |ui| {
            for l in log {
                ui.label(RichText::new(l).small().monospace().color(theme::WEAK));
            }
        });
    }
    rescan
}

fn antenna_ui(ui: &mut egui::Ui, fdtd: &mut Fdtd, applied: &mut AppliedAntenna, library: &mut AntennaLibrary) -> Option<Tech> {
    let busy = fdtd.busy();
    let d = &mut fdtd.ui.design;
    card(ui, |ui| {
        theme::caption(ui, "Setup");
        let (mut kind, mut band, mut quality) = (d.kind, d.band, d.quality);
        let w = (ui.available_width() - 90.0).max(120.0);
        egui::Grid::new("fdtd_ant").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Antenna");
            egui::ComboBox::from_id_salt("fdtd_kind").selected_text(kind.label()).width(w).show_ui(ui, |ui| {
                for k in Kind::ALL {
                    ui.selectable_value(&mut kind, k, k.label());
                }
            });
            ui.end_row();
            ui.label("Band");
            egui::ComboBox::from_id_salt("fdtd_band").selected_text(band.label()).width(w).show_ui(ui, |ui| {
                for b in Band::ALL {
                    ui.selectable_value(&mut band, b, b.label());
                }
            });
            ui.end_row();
            ui.label("Mesh");
            egui::ComboBox::from_id_salt("fdtd_q").selected_text(quality.label()).width(w).show_ui(ui, |ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut quality, q, q.label());
                }
            });
            ui.end_row();
        });
        if (kind, band) != (d.kind, d.band) {
            *d = Design::tuned(kind, band, quality);
        }
        d.quality = quality;
        egui::CollapsingHeader::new(RichText::new("Dimensions").color(theme::WEAK)).id_salt("fdtd_dims").show(ui, |ui| {
            let (a, b, c) = match d.kind {
                Kind::Dipole => ("Length", "Strip width", "Feed gap"),
                Kind::Monopole => ("Height", "Strip width", "Feed gap"),
                Kind::Patch => ("Patch length", "Patch width", "Substrate"),
            };
            let mm = |ui: &mut egui::Ui, label: &str, v: &mut f64| {
                let mut x = *v * 1000.0;
                ui.label(label);
                if ui.add(egui::DragValue::new(&mut x).speed(0.1).range(0.05..=2000.0).suffix(" mm").fixed_decimals(2)).changed() {
                    *v = x / 1000.0;
                }
                ui.end_row();
            };
            egui::Grid::new("fdtd_dim_grid").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                mm(ui, a, &mut d.length);
                mm(ui, b, &mut d.width);
                mm(ui, c, &mut d.gap);
                if d.kind == Kind::Patch {
                    ui.label("Substrate εr");
                    ui.add(egui::DragValue::new(&mut d.eps_r).speed(0.01).range(1.0..=12.0));
                    ui.end_row();
                    ui.label("Substrate tan δ");
                    ui.add(egui::DragValue::new(&mut d.tan_d).speed(0.001).range(0.0..=0.2).max_decimals(4));
                    ui.end_row();
                    mm(ui, "Feed offset", &mut d.feed);
                }
            });
            if ui.small_button("Reset to tuned values").clicked() {
                *d = Design::tuned(d.kind, d.band, d.quality);
            }
        });
    });
    let sim = d.sim();
    let design = d.clone();
    if run_button(ui, &theme::label(theme::icon::PLAY, "Run antenna simulation"), !busy) {
        fdtd.run_antenna(design);
    }
    ui.label(RichText::new(format!("{:.2} M cells · about {:.0} s on this PC", sim.cells() as f64 / 1e6, sim.estimate_s().max(5.0))).small().color(theme::WEAK));

    let res = fdtd.antenna()?;
    ui.add_space(6.0);
    theme::caption(ui, "Result");
    ui.label(RichText::new(format!("{} · {} · {:.2} M cells · {:.0} s", res.design.kind.label(), res.design.band.label(), res.cells as f64 / 1e6, res.seconds)).small().color(theme::WEAK));
    let s = &res.sweep;
    let ghz = |f: f64| f / 1e9;
    let (ca, cb) = res.design.band.channel();
    let fc = res.design.band.center();
    let at = |v: &[C]| s.f.iter().zip(v).min_by(|a, b| (a.0 - fc).abs().total_cmp(&(b.0 - fc).abs())).map(|x| *x.1).unwrap_or_default();
    let s_fc = at(&s.s11);
    let z_fc = at(&s.zin);
    let s11_db = Sweep::db(s_fc);
    let matched = s11_db <= -10.0;
    // Verdict first: is this antenna usable in its band, and how to tune it.
    let hint = s
        .resonance()
        .and_then(|(fr, _)| {
            let off = (fr - fc) / fc * 100.0;
            (off.abs() > 2.0).then(|| {
                format!(
                    " Resonance is {:.1}% {} the band: {} the {} by about {:.1}%.",
                    off.abs(),
                    if off > 0.0 { "above" } else { "below" },
                    if off > 0.0 { "lengthen" } else { "shorten" },
                    if res.design.kind == Kind::Patch { "patch" } else { "element" },
                    off.abs()
                )
            })
        })
        .unwrap_or_default();
    let ohm = res.design.port_ohm();
    let text = if matched {
        format!("Well matched to {ohm:.0} Ω: S11 {s11_db:.1} dB at the band centre.{hint}")
    } else {
        format!("Poorly matched to {ohm:.0} Ω: S11 {s11_db:.1} dB at the band centre, power is reflected.{hint}")
    };
    verdict(ui, matched, &text);
    if res.run.truncated {
        verdict(ui, false, "The run hit its step limit before the field had decayed, so these numbers are cut short.");
    }
    let ok_col = if matched { theme::SUCCESS } else { theme::WARNING };
    let s11_label = format!("S11 at centre ({ohm:.0} Ω)");
    let mut items = vec![
        (format!("{s11_db:.1} dB"), s11_label.as_str(), ok_col),
        (format!("{:.2}", Sweep::vswr(s_fc)), "VSWR", ok_col),
        (format!("{:.0}{:+.0}j Ω", z_fc.re, z_fc.im), "Impedance", theme::TEXT),
    ];
    match s.resonance() {
        Some((fr, _)) => items.push((format!("{:.3} GHz", ghz(fr)), "Resonance", theme::TEXT)),
        None => items.push(("–".into(), "Resonance", theme::WEAK)),
    }
    match s.bandwidth(-10.0) {
        Some((a, b)) => items.push((format!("{:.0} MHz", (b - a) / 1e6), "−10 dB bandwidth", theme::TEXT)),
        None => items.push(("–".into(), "−10 dB bandwidth", theme::WARNING)),
    }
    if let Some(p) = &res.pattern {
        items.push((format!("{:.1} dBi", p.max_dbi), "Directivity", theme::TEXT));
    }
    if let Some(e) = res.efficiency {
        let col = if e > super::MAX_EFFICIENCY { theme::WARNING } else { theme::TEXT };
        items.push((format!("{:.0} %", e * 100.0), "Radiated / accepted", col));
    }
    tiles(ui, &items);

    // Use it in the environment: the floor map then takes this pattern and
    // match for every device of the band's technology.
    let mut rescan = None;
    if let Some(p) = &res.pattern {
        let tech = band_tech(res.design.band);
        let on = applied.0.as_ref().is_some_and(|a| a.source == res.dir);
        let mismatch = 10.0 * (1.0 - (s_fc.re * s_fc.re + s_fc.im * s_fc.im)).max(1e-6).log10();
        let gain_fix = res.efficiency_db();
        let usable = gain_fix.is_ok() && !res.run.truncated;
        let loss_db = gain_fix.clone().unwrap_or(0.0);
        let make = |label: String| FdtdAntenna {
            tech,
            label,
            source: res.dir.clone(),
            theta_step: p.theta.get(1).map_or(5.0, |t| (*t - p.theta[0]) as f32),
            phi_step: p.phi.get(1).map_or(5.0, |f| (*f - p.phi[0]) as f32),
            d: res.planner_gain().unwrap_or_default(),
            mismatch_db: mismatch as f32,
            axis_forward: res.design.kind == Kind::Patch,
            bias_m: None,
        };
        let default_name = format!("{} · {}", res.design.kind.label(), res.design.band.label());
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Use on all {} {}", tech.label(), infra_word(tech))).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut v = on;
                    ui.add_enabled_ui(usable || on, |ui| {
                        if theme::toggle_switch(ui, &mut v).changed() {
                            applied.0 = v.then(|| make(default_name.clone()));
                            rescan = Some(tech);
                        }
                    });
                });
            });
            let how = if res.design.kind == Kind::Patch { "facing where the device faces" } else { "standing upright" };
            ui.label(
                RichText::new(format!(
                    "Every {} {} without its own antenna gets this one ({how}): its {:.1} dBi peak gain ({:.1} dBi directivity, {:.1} dB lost in the substrate; metal counted as lossless) and {:.1} dB mismatch loss replace the ideal antenna in the floor map, the anchor optimiser and the live test.",
                    tech.label(),
                    infra_word(tech),
                    p.max_dbi + loss_db,
                    p.max_dbi,
                    -loss_db,
                    mismatch
                ))
                .small()
                .color(theme::WEAK),
            );
            if let Err(why) = &gain_fix {
                ui.label(RichText::new(format!("Can't be used: {why}.")).small().color(theme::WARNING));
            } else if res.run.truncated {
                ui.label(RichText::new("Can't be used: the run hit its step limit before the field had decayed.").small().color(theme::WARNING));
            }
            if let Some(a) = applied.0.as_ref().filter(|_| !on) {
                ui.label(RichText::new(format!("Applied now: {} (an earlier run)", a.label)).small().color(theme::WARNING));
            }
            // Save it under a name, to pick it for single anchors or tags (Inspector → Antenna).
            ui.separator();
            let st = &mut fdtd.ui;
            if st.lib_name.is_empty() {
                st.lib_name.clone_from(&default_name);
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut st.lib_name).desired_width(170.0).hint_text("name"));
                let name = st.lib_name.trim().to_string();
                let exists = library.get(&name).is_some();
                let b = ui.add_enabled(usable && !name.is_empty(), egui::Button::new(if exists { "Update in antenna library" } else { "Save to antenna library" }));
                if b.on_hover_text("Saved for good; pick it per anchor or tag in Inspector → Antenna.").clicked() {
                    st.lib_status = Some(match library.save(make(name.clone())) {
                        Ok(()) => format!("Saved \"{name}\". Pick it in Inspector → Antenna."),
                        Err(e) => format!("Could not save: {e}"),
                    });
                }
            });
            if let Some(s) = st.lib_status.as_ref() {
                ui.label(RichText::new(s).small().color(theme::WEAK));
            }
        });
    }

    section(ui, "Match and impedance", true, |ui| {
        let s11: Vec<(f64, f64)> = s.f.iter().zip(&s.s11).map(|(f, v)| (ghz(*f), Sweep::db(*v))).collect();
        xy_chart(ui, "S11 (dB)", " dB", &[("S11", BLUE, s11)], Some(-10.0), Some((ghz(ca), ghz(cb))), 120.0);
        ui.add_space(3.0);
        let r: Vec<(f64, f64)> = s.f.iter().zip(&s.zin).map(|(f, z)| (ghz(*f), z.re.clamp(-1000.0, 1000.0))).collect();
        let x: Vec<(f64, f64)> = s.f.iter().zip(&s.zin).map(|(f, z)| (ghz(*f), z.im.clamp(-1000.0, 1000.0))).collect();
        xy_chart(ui, "Input impedance (Ω)", " Ω", &[("R", GREEN, r), ("X", ORANGE, x)], Some(res.design.port_ohm()), Some((ghz(ca), ghz(cb))), 100.0);
        ui.label(RichText::new("Shaded: the channel. Red line: the −10 dB limit / the port impedance.").small().color(theme::WEAK));
    });
    if let Some(p) = &res.pattern {
        section(ui, "Radiation pattern", true, |ui| {
            ui.label(RichText::new(format!("At {:.3} GHz · max {:.1} dBi at θ {:.0}° φ {:.0}°", p.f / 1e9, p.max_dbi, p.max_dir.0, p.max_dir.1)).small().color(theme::WEAK));
            let [xz, yz, xy] = cuts(p);
            ui.horizontal_wrapped(|ui| {
                polar(ui, "Vertical (xz)", &xz, p.max_dbi);
                polar(ui, "Vertical (yz)", &yz, p.max_dbi);
                polar(ui, "Horizontal", &xy, p.max_dbi);
            });
            ui.label(RichText::new("Antenna axis points up (θ = 0°). Rings every 10 dB.").small().color(theme::WEAK));
        });
    }
    if let Some(sl) = &res.slice {
        section(ui, "Electric field", false, |ui| {
            ui.checkbox(&mut fdtd.ui.animate, "Animate");
            let key = format!("fdtd_ant_{}", res.dir.display());
            field_image(ui, &mut fdtd.ui, sl, &key, "Plane through the antenna axis at the band centre, |E| in dB (−40 … 0).");
        });
    }
    rescan
}

/// "Through an object". Returns the technology whose floor map should be
/// recomputed (a material calibration was applied or removed).
fn region_ui(ui: &mut egui::Ui, fdtd: &mut Fdtd) -> Option<Tech> {
    let busy = fdtd.busy();
    let st = &mut fdtd.ui;
    card(ui, |ui| {
        ui.label(RichText::new("How much does this object weaken the signal?").strong());
        ui.label(
            RichText::new(
                "A transmitter goes on one side of the object you pick and a receiver on the other. The radio wave is simulated \
                 passing through it, then again with the object removed. The difference is how much signal the object absorbs \
                 (dB; 3 dB = half the power, 20 dB+ ≈ blocked) and, for UWB, how late the signal arrives through it, which \
                 makes a measured distance too long (cm).",
            )
            .small()
            .color(theme::WEAK),
        );
        let name = st.selected.as_ref().map_or("object", |(_, n)| n.as_str());
        through_diagram(ui, name, None, None);
    });
    card(ui, |ui| {
        theme::caption(ui, "Setup");
        let w = (ui.available_width() - 90.0).max(120.0);
        egui::Grid::new("fdtd_reg").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Object");
            match &st.selected {
                Some((_, n)) => ui.label(RichText::new(n).strong()),
                None => ui.label(RichText::new("click a wall, rack or door in the scene").color(theme::WARNING)),
            };
            ui.end_row();
            ui.label("Band");
            egui::ComboBox::from_id_salt("fdtd_rband").selected_text(st.region_band.label()).width(w).show_ui(ui, |ui| {
                for b in Band::ALL {
                    ui.selectable_value(&mut st.region_band, b, b.label());
                }
            });
            ui.end_row();
            ui.label("Mesh");
            egui::ComboBox::from_id_salt("fdtd_rq").selected_text(region_quality_label(st.region_quality)).width(w).show_ui(ui, |ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut st.region_quality, q, region_quality_label(q));
                }
            });
            ui.end_row();
        });
        ui.label(
            RichText::new("Two dipoles go on either side of the object, the objects around are voxelised from their meshes (ITU-R P.2040 for concrete, wood and glass, typical values for the rest), and the run is repeated in free space to compare.")
                .small()
                .color(theme::WEAK),
        );
    });
    let can = !busy && st.selected.is_some();
    if run_button(ui, &theme::label(theme::icon::PLAY, "Run through the object"), can)
        && let Some((e, _)) = st.selected.clone()
    {
        fdtd.region_requested = Some((st.region_band, st.region_quality, e));
    }
    ui.label(RichText::new("UWB through concrete is the slowest case (minutes or more); BLE and RFID are quick.").small().color(theme::WEAK));

    let Some(res) = fdtd.region() else { return None };
    ui.add_space(6.0);
    theme::caption(ui, "Result");
    ui.label(RichText::new(format!("Through {} · {} · {:.2} M cells × 2 runs · {:.0} s", res.target, res.band.label(), res.cells as f64 / 1e6, res.seconds)).small().color(theme::WEAK));
    let f0 = res.band.center();
    let ghz = |f: f64| f / 1e9;
    let (Some(s_with), Some(s_free)) = (&res.with.s21, &res.free.s21) else { return None };
    let w: Vec<(f64, f64)> = res.with.f.iter().zip(s_with).map(|(f, v)| (ghz(*f), Sweep::db(*v))).collect();
    let fr: Vec<(f64, f64)> = res.free.f.iter().zip(s_free).map(|(f, v)| (ghz(*f), Sweep::db(*v))).collect();
    let loss: Vec<(f64, f64)> = w.iter().zip(&fr).map(|(a, b)| (a.0, b.1 - a.1)).collect();
    let k0 = res.with.f.iter().enumerate().min_by(|a, b| (a.1 - f0).abs().total_cmp(&(b.1 - f0).abs())).map(|x| x.0).unwrap_or(0);
    // Excess delay: group delay with the object minus free space (band average).
    let excess = match (res.with.group_delay(), res.free.group_delay()) {
        (Some(a), Some(b)) => {
            let n = a.len();
            let mid = n / 4..3 * n / 4;
            let d: f64 = mid.clone().map(|k| a[k] - b[k]).sum::<f64>() / mid.len().max(1) as f64;
            Some(d)
        }
        _ => None,
    };
    let l = loss.get(k0).map(|v| v.1).unwrap_or(0.0);
    let text = if l > 20.0 {
        format!("{} blocks the signal: {l:.1} dB lost passing through it.", res.target)
    } else if l > 6.0 {
        format!("{} weakens the signal: {l:.1} dB lost passing through it.", res.target)
    } else {
        format!("{} barely matters: {l:.1} dB lost passing through it.", res.target)
    };
    verdict(ui, l <= 6.0, &text);
    let warnings = region_warnings(&res);
    warning_lines(ui, &warnings);
    // FDTD next to what the floor map's ray model assumes (live: follows the calibration).
    let mat = res.layout.target_material;
    let band = cal_band(res.band);
    let (k_loss, k_delay) = MaterialCal::scales_in(mat, band);
    let bias_m = crate::rtls::device::Profile::default().nlos_bias_m;
    let ray_loss = res.layout.wall_db * res.layout.target_weight * k_loss;
    let ray_bias_cm = res.layout.target_weight * k_delay * bias_m * 100.0;
    let fdtd_bias_cm = excess.map(|d| (d * super::csx::C0 * 100.0) as f32);
    card(ui, |ui| through_diagram(ui, &res.target, Some(l), fdtd_bias_cm.filter(|_| band_tech(res.band) == Tech::Uwb)));
    let mut rescan = None;
    card(ui, |ui| {
        egui::Grid::new("fdtd_vs_ray").num_columns(3).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
            ui.label("");
            ui.label(RichText::new("FDTD").small().strong()).on_hover_text("What this simulation gives: not a measurement");
            ui.label(RichText::new("Floor map assumes").small().color(theme::WEAK))
                .on_hover_text("The quick ray model the floor map, the anchor optimiser and the live simulation use");
            ui.end_row();
            ui.label("Loss through it");
            let col = if l > 20.0 {
                theme::DANGER
            } else if l > 6.0 {
                theme::WARNING
            } else {
                theme::SUCCESS
            };
            ui.label(RichText::new(format!("{l:.1} dB")).strong().color(col));
            ui.label(RichText::new(format!("{ray_loss:.1} dB")).color(theme::WEAK));
            ui.end_row();
            if let Some(b) = fdtd_bias_cm {
                ui.label("UWB range bias").on_hover_text("How much too long UWB measures the distance when the signal goes through this object");
                ui.label(RichText::new(format!("{b:+.0} cm")).strong());
                ui.label(RichText::new(format!("{ray_bias_cm:+.0} cm")).color(theme::WEAK));
                ui.end_row();
            }
        });
        // In words: which way the floor map is off for this material.
        let diff = l as f32 - ray_loss;
        let words = if diff.abs() < 2.0 {
            format!("The floor map's estimate for {} is about right here.", mat.name())
        } else if diff > 0.0 {
            format!("The floor map is too optimistic for {}: it assumes {diff:.0} dB less loss than this simulation, so it shows coverage behind it that isn't there.", mat.name())
        } else {
            format!("The floor map is too pessimistic for {}: it assumes {:.0} dB more loss than this simulation, so it may place more anchors than needed.", mat.name(), -diff)
        };
        ui.label(RichText::new(words).small());

        // Use the result in the floor map, optimiser and simulation.
        let cal = res.layout.calibration(res.band, &res.target, &res.dir, l, fdtd_bias_cm, bias_m).filter(|_| !res.truncated);
        if matches!(mat, Material::Metal) {
            ui.label(RichText::new("Metal blocks UWB in the floor map anyway; nothing to calibrate.").small().color(theme::WEAK));
        } else if res.layout.target_weight > 0.0 {
            let applied = MaterialCal::applied_in(mat, band);
            let on = applied.as_ref().is_some_and(|a| a.source.path == res.dir);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Use for all {} at {} in the floor map", mat.name(), band.label())).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut v = on;
                    ui.add_enabled_ui(on || cal.is_some(), |ui| {
                        if theme::toggle_switch(ui, &mut v).changed() {
                            match cal.clone().filter(|_| v) {
                                Some(c) => MaterialCal::set(mat, Some(c)),
                                None => MaterialCal::clear_in(mat, band),
                            }
                            rescan = Some(band_tech(res.band));
                        }
                    });
                });
            });
            ui.label(
                RichText::new(format!("Applies to every object of this material, at {} only, in this tab, until you restart the app. The floor map, the anchor optimiser and the live simulation all use it. It's a simulation, so numbers using it stay Simulated.", band.label()))
                    .small()
                    .color(theme::WEAK),
            );
            if cal.is_none() {
                ui.label(RichText::new("Can't be used: the object isn't in the simulation, or the run was cut short.").small().color(theme::WARNING));
            }
            if let Some(a) = applied.filter(|_| !on) {
                ui.label(RichText::new(format!("Applied now: {} (an earlier run)", a.label)).small().color(theme::WARNING));
            }
        }
    });
    tiles(
        ui,
        &[
            (format!("{:.1} dB", w.get(k0).map(|v| v.1).unwrap_or(0.0)), "S21 with object", theme::TEXT),
            (format!("{:.1} dB", fr.get(k0).map(|v| v.1).unwrap_or(0.0)), "S21 free space", theme::TEXT),
            (format!("{:.1} mm", res.layout.cell * 1000.0), "Voxel size", theme::TEXT),
        ],
    );
    ui.label(
        RichText::new("S21 = signal reaching the receiver (dB, relative to what was sent). Voxel = one cell of the simulation grid.")
            .small()
            .color(theme::WEAK),
    );
    let (ca, cb) = res.band.channel();
    section(ui, "Transmission", true, |ui| {
        xy_chart(ui, "S21 (dB)", " dB", &[("with object", ORANGE, w), ("free space", BLUE, fr)], None, Some((ghz(ca), ghz(cb))), 120.0);
        ui.add_space(3.0);
        xy_chart(ui, "Loss through the object (dB)", " dB", &[("loss", theme::DANGER, loss)], None, Some((ghz(ca), ghz(cb))), 80.0);
    });
    section(ui, "Materials", false, |ui| {
        egui::Grid::new("fdtd_objs").striped(true).num_columns(4).show(ui, |ui| {
            for (k, (name, m, eps, kappa)) in res.layout.objects.iter().enumerate() {
                ui.label(name);
                ui.label(RichText::new(m.name()).color(theme::WEAK));
                ui.label(if kappa.is_infinite() { "metal (PEC)".to_string() } else { format!("εr {eps:.2} · σ {kappa:.3} S/m") });
                match res.layout.thickness.get(k).filter(|t| !t.not_measured) {
                    Some(t) => {
                        let col = if t.problem().is_some() { theme::WARNING } else { theme::WEAK };
                        ui.label(RichText::new(format!("{:.0} mm → {:.0} mm", t.true_m * 1e3, t.voxel_m * 1e3)).color(col)).on_hover_text("Thickness of its mesh through its thinnest side, and of its voxels");
                    }
                    None => {
                        ui.label("");
                    }
                }
                ui.end_row();
            }
        });
        ui.label(RichText::new(format!("{} voxels", res.layout.voxels)).small().color(theme::WEAK));
    });
    if let Some(sl) = &res.slice {
        section(ui, "Electric field", true, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut fdtd.ui.show_field, "Show in the scene (3D)");
                ui.checkbox(&mut fdtd.ui.animate, "Animate");
            });
            field_legend(ui);
            // The vertical cut shows the whole object; older runs only have the slice.
            if let Some(v) = res.planes.get(1) {
                let key = format!("fdtd_reg_v_{}", res.dir.display());
                field_image(ui, &mut fdtd.ui, v, &key, "Vertical cut along the path, through the whole object: transmitter on one side, receiver on the other.");
                ui.add_space(4.0);
            }
            let key = format!("fdtd_reg_{}", res.dir.display());
            field_image(ui, &mut fdtd.ui, sl, &key, "Seen from above at antenna height. Red marker: transmitter, green: receiver.");
            if res.planes.len() > 2 {
                ui.label(RichText::new(format!("In the scene: these two planes plus {} levels over the object's height.", res.planes.len() - 2)).small().color(theme::WEAK));
            }
        });
    }
    rescan
}

/// "45 s", "12 min", "2.5 h".
fn duration(s: f64) -> String {
    if s < 90.0 {
        format!("{s:.0} s")
    } else if s < 5400.0 {
        format!("{:.0} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

fn scene_quality_label(q: Quality) -> &'static str {
    match q {
        Quality::Fast => "Fast (λ/10 in air)",
        Quality::Normal => "Normal (λ/12 in air)",
        Quality::Fine => "Fine (λ/15 in air)",
    }
}

/// `region::build` meshes to these, against the top of the pulse in the densest material.
fn region_quality_label(q: Quality) -> &'static str {
    match q {
        Quality::Fast => "Fast (λ/10)",
        Quality::Normal => "Normal (λ/15)",
        Quality::Fine => "Fine (λ/20)",
    }
}

fn infra_plural(t: Tech) -> &'static str {
    match t {
        Tech::Uwb => "anchors",
        Tech::Ble => "BLE nodes",
        Tech::Rfid => "readers",
        Tech::Wifi => "access points",
    }
}

fn scene_ui(ui: &mut egui::Ui, fdtd: &mut Fdtd, cov: &mut Coverage) -> Option<Tech> {
    let busy = fdtd.busy();
    card(ui, |ui| {
        ui.label(RichText::new("How does the signal really spread through the room?").strong());
        ui.label(
            RichText::new(
                "Each anchor, node or reader is simulated in turn with the whole room around it: walls, racks and goods voxelised \
                 from their meshes (ITU-R P.2040 for concrete, wood and glass, typical values for the rest) on a concrete floor. The result is the received power everywhere, \
                 reflections, diffraction and multipath included, which the floor map can use instead of its straight-line ray model, \
                 and which can correct that model's material losses.",
            )
            .small()
            .color(theme::WEAK),
        );
    });
    card(ui, |ui| {
        theme::caption(ui, "Setup");
        let st = &mut fdtd.ui;
        let w = (ui.available_width() - 90.0).max(120.0);
        egui::Grid::new("fdtd_scene").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Technology");
            egui::ComboBox::from_id_salt("fdtd_stech").selected_text(st.scene_tech.label()).width(w).show_ui(ui, |ui| {
                for t in Tech::ALL {
                    ui.selectable_value(&mut st.scene_tech, t, t.label());
                }
            });
            ui.end_row();
            ui.label("Mesh");
            egui::ComboBox::from_id_salt("fdtd_sq").selected_text(scene_quality_label(st.scene_quality)).width(w).show_ui(ui, |ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut st.scene_quality, q, scene_quality_label(q));
                }
            });
            ui.end_row();
            ui.label("Tag height");
            ui.add(egui::DragValue::new(&mut st.scene_height).range(0.1..=3.0).speed(0.05).suffix(" m"));
            ui.end_row();
        });
    });
    let tech = fdtd.ui.scene_tech;
    let preview = fdtd.scene_preview.clone().filter(|p| p.tech == tech);
    if let Some(p) = &preview {
        card(ui, |ui| {
            if p.txs == 0 {
                ui.label(RichText::new(format!("No {} placed yet: add some on the Positioning page first.", infra_plural(tech))).color(theme::WARNING));
                return;
            }
            ui.horizontal(|ui| {
                theme::caption(ui, "Estimate");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if p.plan.scaled() {
                        theme::badge(ui, &format!("Scaled ×{:.1}", p.plan.scale), theme::WARNING);
                    } else {
                        theme::badge(ui, "Real frequency", theme::SUCCESS);
                    }
                });
            });
            let todo = p.txs - p.reused.min(p.txs);
            tiles(
                ui,
                &[
                    (format!("{}", p.txs), infra_plural(tech), theme::TEXT),
                    (format!("{:.0} M", p.plan.cells as f64 / 1e6), "cells per run", theme::TEXT),
                    (format!("{:.1} GB", p.plan.memory as f64 / 1e9), "memory", theme::TEXT),
                    (format!("{:.0} mm", p.plan.cell * 1000.0), "voxel size", theme::TEXT),
                    (format!("{todo}"), "runs needed", theme::TEXT),
                    (format!("~{}", duration(p.plan.seconds_per_tx * todo as f64)), "run time", theme::TEXT),
                ],
            );
            let text = if p.plan.scaled() {
                format!(
                    "{} at its real {:.2} GHz would need far more cells than the {:.0} M budget for this mesh, so the room is simulated at {:.2} GHz. \
                     Materials keep their real-band properties and received power uses the real wavelength, so loss through a wall roughly holds. \
                     Diffraction doesn't: a shadow behind an edge can read up to {:.0} dB too strong, per edge.",
                    p.band.label(),
                    p.plan.f_real / 1e9,
                    p.plan.budget / 1e6,
                    p.plan.f_sim / 1e9,
                    p.plan.shadow_error_db()
                )
            } else {
                format!("{} is simulated at its real frequency.", p.band.label())
            };
            ui.label(RichText::new(text).small().color(theme::WEAK));
            if let Some(why) = p.plan.scale_block() {
                ui.label(RichText::new(format!("You can look at this run, but it can't feed the floor map or a calibration: {why}.")).small().color(theme::WARNING));
            }
            warning_lines(ui, &p.plan.warnings());
            if let Some(why) = &p.problem {
                ui.label(RichText::new(format!("Can't run: {why}.")).small().color(theme::DANGER));
            }
            if p.reused > 0 {
                ui.label(RichText::new(format!("{} unchanged since the last run: reused, not simulated again.", p.reused)).small().color(theme::ONLINE));
            }
        });
    }
    let can = !busy && preview.as_ref().is_some_and(|p| p.txs > 0 && p.problem.is_none());
    if run_button(ui, &theme::label(theme::icon::PLAY, "Run the whole scene"), can) {
        fdtd.scene_requested = Some((tech, fdtd.ui.scene_quality, fdtd.ui.scene_height));
    }

    let Some(res) = fdtd.scene(tech) else { return None };
    let mut rescan = None;
    ui.add_space(6.0);
    theme::caption(ui, "Result");
    let scaled = if res.plan.scaled() { format!(" · scaled ×{:.1}", res.plan.scale) } else { String::new() };
    ui.label(
        RichText::new(format!(
            "{} · {} · {} {} ({} simulated) · {:.0} M cells each · {}{scaled}",
            res.band.label(),
            scene_quality_label(res.quality),
            res.per_tx.len(),
            infra_plural(tech),
            res.simulated,
            res.plan.cells as f64 / 1e6,
            duration(res.seconds)
        ))
        .small()
        .color(theme::WEAK),
    );

    warning_lines(ui, &res.warnings());
    let use_block = res.use_block();

    // Floor map source.
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Use in the floor map").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut on = cov.source == MapSource::Fdtd;
                // Always possible to turn off, never on when the numbers can't carry it.
                ui.add_enabled_ui(on || use_block.is_none(), |ui| {
                    if theme::toggle_switch(ui, &mut on).changed() {
                        cov.source = if on { MapSource::Fdtd } else { MapSource::Ray };
                        rescan = Some(tech);
                    }
                });
            });
        });
        if let Some(why) = &use_block {
            ui.label(RichText::new(format!("Not available: {why}. The floor map stays on the ray model.")).small().color(theme::WARNING));
        }
        ui.label(
            RichText::new(format!(
                "The floor map's Quality, Accuracy and Signal views use this run's received power instead of the ray model, for {}. \
                 {} moved or added after the run stay on the ray model until you run again.",
                tech.label(),
                infra_plural(tech)
            ))
            .small()
            .color(theme::WEAK),
        );
        if cov.tech == tech && !cov.cells.is_empty() && !cov.source_note.is_empty() {
            ui.label(RichText::new(&cov.source_note).small().color(theme::ONLINE));
        }
    });

    section(ui, "Field in the room", true, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut fdtd.ui.show_field, "Show in the scene (3D)");
            ui.checkbox(&mut fdtd.ui.scene_all_levels, "All heights").on_hover_text("Off: only the plane at tag height (plus the vertical cuts)");
        });
        let names: Vec<String> = res.per_tx.iter().map(|t| t.tx.name.clone()).collect();
        let sel = fdtd.ui.scene_tx.and_then(|i| names.get(i)).cloned().unwrap_or_else(|| "Strongest of all".into());
        egui::ComboBox::from_id_salt("fdtd_stx").selected_text(sel).width(ui.available_width()).show_ui(ui, |ui| {
            ui.selectable_value(&mut fdtd.ui.scene_tx, None, "Strongest of all");
            for (i, n) in names.iter().enumerate() {
                ui.selectable_value(&mut fdtd.ui.scene_tx, Some(i), n);
            }
        });
        field_legend(ui);
        let (_, hi) = crate::rtls::planning::signal_scale(tech);
        ui.label(
            RichText::new(format!(
                "Received power at the tag ({}): red ≈ {hi:.0} dBm, yellow ≈ {:.0}, cyan ≈ {:.0}, dark blue ≤ {:.0} dBm. Averaged over a wavelength.",
                if tech == Tech::Ble { "RSSI at the node" } else { "the tag's received power" },
                hi - 14.0,
                hi - 26.0,
                hi - 40.0
            ))
            .small()
            .color(theme::WEAK),
        );
        let directional = res.per_tx.iter().filter(|t| t.tx.directional).count();
        if directional > 0 {
            ui.label(RichText::new(format!("{directional} aimed antenna(s) simulated as a dipole with a reflector behind it. The device's own pattern is applied along the straight line to each point, an estimate where the signal arrives another way. Circular polarisation isn't modelled.")).small().color(theme::WEAK));
        }
    });

    let band = cal_band(res.band);
    section(ui, "Calibrate the ray model", true, |ui| match fdtd.scene_fit(&res.dir) {
        None => {
            ui.label(RichText::new("Comparing with the ray model…").small().color(theme::WEAK));
        }
        Some(None) => {
            ui.label(
                RichText::new(format!(
                    "Too few floor points behind objects to fit their materials (at least {} per material, from omnidirectional transmitters).",
                    super::scene::MIN_SAMPLES
                ))
                .small()
                .color(theme::WEAK),
            );
        }
        Some(Some(fit)) => {
            ui.label(
                RichText::new(format!(
                    "{} floor points at {:.1} m: how much each material weakens the signal, fitted so the ray model matches this run.",
                    fit.samples, res.tag_height
                ))
                .small()
                .color(theme::WEAK),
            );
            if fit.mats.is_empty() {
                ui.label(RichText::new("No material is crossed often enough to fit.").small().color(theme::WEAK));
                return;
            }
            egui::Grid::new("fdtd_fit").striped(true).num_columns(4).spacing(vec2(14.0, 3.0)).show(ui, |ui| {
                ui.label(RichText::new("Material").small().strong());
                ui.label(RichText::new("Points").small().strong()).on_hover_text("Floor points whose straight path crosses it");
                ui.label(RichText::new("Floor map").small().strong()).on_hover_text("Loss scale the ray model uses now (1 = built-in)");
                ui.label(RichText::new("FDTD fit").small().strong());
                ui.end_row();
                for m in &fit.mats {
                    ui.label(m.material.name());
                    ui.label(RichText::new(format!("{}", m.samples)).color(theme::WEAK));
                    ui.label(RichText::new(format!("×{:.2}", MaterialCal::scales_in(m.material, band).0)).color(theme::WEAK));
                    let col = if (m.fitted - m.current).abs() > 0.25 { theme::WARNING } else { theme::TEXT };
                    ui.label(RichText::new(format!("×{:.2}", m.fitted)).strong().color(col));
                    ui.end_row();
                }
            });
            let side = if fit.offset_db < 0.0 { "stronger" } else { "weaker" };
            ui.label(
                RichText::new(format!(
                    "Spread of ray model vs FDTD: {:.1} dB → {:.1} dB with the fit. The room is {:.1} dB {side} on average than free space plus walls \
                     (reflections); that offset is shown, not applied.",
                    fit.rms_before,
                    fit.rms_after,
                    fit.offset_db.abs()
                ))
                .small(),
            );
            let on = fit.mats.iter().all(|m| MaterialCal::applied_in(m.material, band).is_some_and(|a| a.source.path == res.dir));
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Use these losses in the floor map at {}", band.label())).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut v = on;
                    ui.add_enabled_ui(on || use_block.is_none(), |ui| {
                        if theme::toggle_switch(ui, &mut v).changed() {
                            for m in &fit.mats {
                                let prev = MaterialCal::applied_in(m.material, band);
                                if v {
                                    MaterialCal::set(
                                        m.material,
                                        Some(MaterialCal {
                                            loss_scale: m.fitted.max(0.0),
                                            bias_scale: prev.map_or(1.0, |p| p.bias_scale),
                                            label: format!("FDTD scene · {}", res.band.label()),
                                            source: CalSource { path: res.dir.clone(), band, ..Default::default() },
                                        }),
                                    );
                                } else if prev.is_some_and(|p| p.source.path == res.dir) {
                                    MaterialCal::clear_in(m.material, band);
                                }
                            }
                            rescan = Some(tech);
                        }
                    });
                });
            });
            if let Some(why) = &use_block {
                ui.label(RichText::new(format!("Not available: {why}.")).small().color(theme::WARNING));
            }
            ui.label(
                RichText::new(format!("Applies to every object of these materials, at {} only, in this tab, until you restart the app (as \"Through an object\" does). It's a simulation, so numbers using it stay Simulated.", band.label()))
                    .small()
                    .color(theme::WEAK),
            );
        }
    });
    rescan
}
