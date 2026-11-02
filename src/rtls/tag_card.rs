//! Tag details for the card next to a selected tag: live metrics, every radio
//! link (anchor, measured vs true range, LOS/NLOS) and graphs of the error and
//! of the ranges over time. Also the "add tag" row of the Test step.

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;
use bevy_egui::egui;
use egui::{Color32, RichText, vec2};

use super::device::{Device, DeviceKind, Motion, Profiles, epc_for};
use super::rng::Rng;
use super::{DevicePlace, LinkKind, RtlsState, Track};
use crate::theme;

/// Seconds of history kept per tag.
const WINDOW_S: f64 = 60.0;
/// Minimum simulated time between samples.
const STEP_S: f64 = 0.1;

#[derive(Clone)]
pub struct Sample {
    pub t: f64,
    /// Position error (m); `None` while lost.
    pub err: Option<f32>,
    pub used: u32,
    /// Measured value per link partner (range m or RSSI dBm).
    pub links: Vec<(Entity, f32)>,
}

/// Recent samples of every track, so a tag's graphs are full as soon as it is clicked.
#[derive(Resource, Default)]
pub struct TagHistory {
    pub tags: HashMap<Entity, VecDeque<Sample>>,
    last_t: f64,
}

pub fn sample(state: Res<RtlsState>, mut hist: ResMut<TagHistory>) {
    if state.time < hist.last_t {
        // Restarted.
        hist.tags.clear();
        hist.last_t = state.time;
    }
    if state.time - hist.last_t < STEP_S {
        return;
    }
    hist.last_t = state.time;
    hist.tags.retain(|e, _| state.tracks.contains_key(e));
    for (e, tr) in &state.tracks {
        let q = hist.tags.entry(*e).or_default();
        q.push_back(Sample {
            t: state.time,
            err: if tr.lost { None } else { tr.err },
            used: tr.used,
            links: tr.links.iter().filter_map(|l| l.value.map(|v| (l.other, v))).collect(),
        });
        while q.front().is_some_and(|s| s.t < state.time - WINDOW_S) {
            q.pop_front();
        }
    }
}

const PALETTE: [Color32; 8] = [
    Color32::from_rgb(38, 110, 228),
    Color32::from_rgb(226, 118, 22),
    Color32::from_rgb(40, 158, 84),
    Color32::from_rgb(214, 62, 124),
    Color32::from_rgb(128, 84, 214),
    Color32::from_rgb(20, 150, 158),
    Color32::from_rgb(178, 150, 20),
    Color32::from_rgb(120, 124, 132),
];

/// A small multi-line time chart with a title, the latest values and a y range.
pub(super) fn lines_chart(ui: &mut egui::Ui, title: &str, unit: &str, series: &[(String, Color32, Vec<(f64, f32)>)], height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let pts: Vec<&(f64, f32)> = series.iter().flat_map(|s| s.2.iter()).collect();
    if pts.len() < 2 {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, format!("{title}: waiting for data"), egui::FontId::proportional(11.0), theme::WEAK);
        return;
    }
    let t0 = pts.iter().map(|x| x.0).fold(f64::MAX, f64::min);
    let t1 = pts.iter().map(|x| x.0).fold(f64::MIN, f64::max);
    let mut lo = pts.iter().map(|x| x.1).fold(f32::MAX, f32::min);
    let mut hi = pts.iter().map(|x| x.1).fold(f32::MIN, f32::max);
    if unit != " dBm" {
        lo = lo.min(0.0);
    }
    let pad = ((hi - lo) * 0.1).max(1e-3);
    hi += pad;
    if unit == " dBm" {
        lo -= pad;
    }
    let plot = egui::Rect::from_min_max(rect.min + vec2(4.0, 16.0), rect.max - vec2(4.0, 4.0));
    let to = |(t, v): (f64, f32)| {
        egui::pos2(
            plot.left() + ((t - t0) / (t1 - t0).max(1e-6)) as f32 * plot.width(),
            plot.bottom() - (v - lo) / (hi - lo) * plot.height(),
        )
    };
    for frac in [0.5f32] {
        let y = plot.bottom() - frac * plot.height();
        p.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], egui::Stroke::new(1.0, Color32::from_black_alpha(14)));
    }
    for (_, col, s) in series {
        if s.len() >= 2 {
            p.add(egui::Shape::line(s.iter().map(|x| to(*x)).collect(), egui::Stroke::new(1.5, *col)));
        }
    }
    p.text(rect.left_top() + vec2(6.0, 2.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(11.0), theme::TEXT);
    p.text(
        rect.right_top() + vec2(-6.0, 2.0),
        egui::Align2::RIGHT_TOP,
        format!("{:.1}…{:.1}{unit} · {:.0} s", lo, hi, t1 - t0),
        egui::FontId::proportional(10.5),
        theme::WEAK,
    );
}

fn link_kind(k: LinkKind) -> (&'static str, &'static str) {
    match k {
        LinkKind::Range => ("TWR", " m"),
        LinkKind::Tdoa => ("TDoA", " m"),
        LinkKind::Rssi => ("RSSI", " dBm"),
        LinkKind::Aoa => ("AoA", " °"),
        LinkKind::WifiRtt => ("RTT", " m"),
        LinkKind::WifiRssi => ("WiFi", " dBm"),
        LinkKind::Rfid => ("RFID", " dBm"),
    }
}

fn metric(ui: &mut egui::Ui, key: &str, value: String) {
    ui.label(RichText::new(key).color(theme::WEAK));
    ui.label(RichText::new(value).monospace());
    ui.end_row();
}

/// Live data of a tag: metrics, links and graphs.
pub fn details(ui: &mut egui::Ui, e: Entity, track: Option<&Track>, running: bool, ids: &HashMap<Entity, String>, hist: &TagHistory) {
    ui.set_min_width(300.0);
    let Some(tr) = track else {
        let msg = if running { "No data yet: waiting for the first update." } else { "Simulation is paused: press Run on the Test live page." };
        ui.label(RichText::new(msg).color(theme::WARNING));
        return;
    };
    let name = |o: &Entity| ids.get(o).cloned().unwrap_or_else(|| "?".into());

    // ---- Metrics
    egui::Grid::new(("tag_metrics", e)).num_columns(2).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
        metric(ui, "Technology", tr.tech.to_uppercase());
        let status = if tr.lost { "lost (no position)".to_string() } else { "located".to_string() };
        metric(ui, "Status", status);
        if tr.tech == "rfid" {
            metric(ui, "Reads", format!("{}", tr.reads));
            metric(ui, "Best reader", tr.best_reader.map(|r| name(&r)).unwrap_or_else(|| "–".into()));
        } else {
            let v = |p: Option<Vec3>| p.map_or("–".into(), |p| format!("{:.2}, {:.2}, {:.2}", p.x, p.y, p.z));
            metric(ui, "Position (m)", v(tr.est));
            metric(ui, "True position", v(Some(tr.truth)));
            metric(ui, "Raw fix", v(tr.raw));
            metric(ui, "Error", tr.err.map_or("–".into(), |e| format!("{:.1} cm", e * 100.0)));
            metric(ui, "DOP", format!("{:.2}", tr.dop));
            metric(ui, "Quality", format!("{:.0} %", tr.quality * 100.0));
            metric(ui, "Residual", format!("{:.1} cm", tr.residual * 100.0));
            let rate = if tr.attempts > 0 { tr.fixes as f32 / tr.attempts as f32 * 100.0 } else { 0.0 };
            metric(ui, "Fix rate", format!("{rate:.0} % ({}/{})", tr.fixes, tr.attempts));
        }
        let heard = tr.links.iter().filter(|l| l.value.is_some()).count();
        metric(ui, "Links", format!("{} used · {heard} heard · {} total", tr.used, tr.links.len()));
    });

    // ---- Links
    if !tr.links.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("Radio links").strong());
        egui::Grid::new(("tag_links", e)).striped(true).num_columns(6).spacing(vec2(10.0, 2.0)).show(ui, |ui| {
            for h in ["", "to", "type", "measured", "true", "path"] {
                ui.label(RichText::new(h).small().color(theme::WEAK));
            }
            ui.end_row();
            let mut links: Vec<_> = tr.links.iter().collect();
            links.sort_by_key(|l| name(&l.other));
            for (i, l) in links.iter().enumerate() {
                let (kind, unit) = link_kind(l.kind);
                ui.label(RichText::new("●").color(PALETTE[i % PALETTE.len()]));
                ui.label(RichText::new(name(&l.other)).strong());
                ui.label(RichText::new(kind).small());
                match l.value {
                    Some(v) => ui.label(RichText::new(format!("{v:.2}{unit}")).monospace().color(if l.used { theme::TEXT } else { theme::WEAK })),
                    None => ui.label(RichText::new("lost").color(theme::DANGER)),
                };
                ui.label(RichText::new(format!("{:.2} m", l.true_distance)).monospace().color(theme::WEAK));
                let path = l.path_label();
                ui.label(RichText::new(path).small().color(if l.walls == 0 { theme::SUCCESS } else { theme::WARNING }));
                ui.end_row();
            }
        });
        ui.label(RichText::new("grey value = not used in the last fix (outlier or not needed)").small().color(theme::WEAK));
    }

    // ---- Graphs
    let Some(h) = hist.tags.get(&e) else { return };
    ui.add_space(4.0);
    if tr.tech != "rfid" {
        let err: Vec<(f64, f32)> = h.iter().filter_map(|s| s.err.map(|v| (s.t, v * 100.0))).collect();
        lines_chart(ui, "Error (cm)", " cm", &[("error".into(), theme::WARNING, err)], 70.0);
        ui.add_space(3.0);
    }
    let used: Vec<(f64, f32)> = h.iter().map(|s| (s.t, s.used as f32)).collect();
    // One line per link partner, coloured like the table.
    let mut partners: Vec<Entity> = tr.links.iter().map(|l| l.other).collect();
    partners.sort_by_key(|o| name(o));
    let unit = tr.links.first().map_or(" m", |l| link_kind(l.kind).1);
    let series: Vec<(String, Color32, Vec<(f64, f32)>)> = partners
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let pts = h.iter().filter_map(|s| s.links.iter().find(|(p, _)| p == o).map(|(_, v)| (s.t, *v))).collect();
            (name(o), PALETTE[i % PALETTE.len()], pts)
        })
        .collect();
    let title = if unit == " dBm" { "Signal per link (dBm)" } else { "Range per anchor (m)" };
    lines_chart(ui, title, unit, &series, 90.0);
    ui.add_space(3.0);
    lines_chart(ui, "Links used", "", &[("used".into(), theme::SUCCESS, used)], 50.0);
}

/// Row of the Test step: place tags by clicking, or drop a few walking ones at once.
pub fn add_tags_row<'a>(
    ui: &mut egui::Ui,
    place: &mut DevicePlace,
    profiles: &Profiles,
    devices: impl Iterator<Item = (&'a Device, Vec3)>,
    commands: &mut Commands,
    rng: &mut Rng,
) {
    let devices: Vec<(&Device, Vec3)> = devices.collect();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Add tags").color(theme::WEAK));
        for kind in [DeviceKind::UwbTag, DeviceKind::BleTag, DeviceKind::RfidTag, DeviceKind::WifiClient] {
            let Some(p) = profiles.default_for(kind) else { continue };
            let active = place.profile.as_deref() == Some(p.id.as_str());
            if ui
                .add(egui::Button::new(theme::label(theme::icon::PLUS, kind.label())).selected(active))
                .on_hover_text(format!("{}: click in the scene to place, keeps placing until Esc / right-click", p.name))
                .clicked()
            {
                place.profile = if active { None } else { Some(p.id.clone()) };
                place.sticky = true;
                place.waypoints_for = None;
            }
        }
        let uwb = profiles.default_for(DeviceKind::UwbTag);
        if let Some(p) = uwb
            && ui.button(theme::label(theme::icon::PLUS, "5 walking")).on_hover_text("Five UWB tags that wander around the area covered by the anchors").clicked()
        {
            // Wander inside the anchors' footprint (or around the origin without any).
            let infra: Vec<Vec3> = devices.iter().filter(|(d, _)| !d.kind.is_mobile()).map(|(_, t)| *t).collect();
            let (center, radius) = if infra.is_empty() {
                (Vec3::ZERO, 5.0)
            } else {
                let (lo, hi) = infra.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
                (((lo + hi) * 0.5).with_y(0.0), ((hi - lo).xz().min_element() * 0.4).max(1.0))
            };
            let mut ids: Vec<String> = devices.iter().map(|(d, _)| d.id.clone()).collect();
            for _ in 0..5 {
                let id = super::device::next_id(DeviceKind::UwbTag, ids.iter().map(String::as_str));
                ids.push(id.clone());
                let a = rng.range(0.0, std::f32::consts::TAU);
                let r = radius * rng.uniform().sqrt();
                let pos = center + Vec3::new(a.cos() * r, p.mount_height_m, a.sin() * r);
                let motion = Motion::Wander { center: center.with_y(p.mount_height_m).to_array(), radius, speed: rng.range(0.6, 1.4) };
                let address = if p.kind == DeviceKind::RfidTag { epc_for(&id) } else { String::new() };
                super::view::spawn_device(
                    commands,
                    Device { id, kind: p.kind, profile: p.id.clone(), enabled: true, motion, address, auto: false, radio: Default::default() },
                    Transform::from_translation(pos),
                );
            }
        }
    });
}
