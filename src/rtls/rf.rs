//! RF analysis: signal strength per link, link quality and LOS/NLOS, anchor
//! health, coverage and dead zones. The same summary goes into every snapshot
//! (`Snapshot::rf`), so OpenEMS / MQTT / the recorder get it too.

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;
use bevy_egui::egui;
use egui::{Color32, RichText, vec2};
use serde::{Deserialize, Serialize};

use super::planning::{Cell, Coverage, CoverageRequest, Tech};
use super::{LinkKind, RtlsSelection, RtlsState};
use crate::scene::OrbitCamera;
use crate::theme;

/// Below this a heard link is "weak" (dBm): UWB first-path power, BLE RSSI,
/// RFID backscatter at the reader.
pub fn weak_below(tech: Tech) -> f32 {
    match tech {
        Tech::Uwb => -100.0,
        Tech::Ble => -85.0,
        Tech::Rfid => -70.0,
    }
}

fn tech_of(kind: LinkKind) -> Tech {
    match kind {
        LinkKind::Range | LinkKind::Tdoa => Tech::Uwb,
        LinkKind::Rssi => Tech::Ble,
        LinkKind::Rfid => Tech::Rfid,
    }
}

/// Health of one anchor / beacon / reader over the links of the last update.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AnchorRow {
    pub id: String,
    pub tech: String,
    /// Tags within its range (links attempted).
    pub in_range: u32,
    pub heard: u32,
    pub used: u32,
    pub success_pct: f32,
    pub avg_dbm: f32,
    pub min_dbm: f32,
    pub nlos_pct: f32,
    /// UWB ranges heard but rejected by the solver.
    pub outliers: u32,
    /// Mean UWB range bias (measured - true), cm.
    pub bias_cm: f32,
}

impl AnchorRow {
    pub fn status(&self) -> (&'static str, Color32) {
        if self.in_range == 0 {
            ("idle", theme::WEAK)
        } else if self.heard == 0 {
            ("no signal", theme::DANGER)
        } else if self.success_pct < 50.0 || self.avg_dbm < weak_below(Tech::parse(&self.tech).unwrap_or_default()) {
            ("weak", theme::ACCENT)
        } else {
            ("ok", theme::SUCCESS)
        }
    }
}

/// RF summary over all current links (or one technology).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RfSnapshot {
    pub links: u32,
    pub heard: u32,
    pub success_pct: f32,
    pub avg_dbm: f32,
    pub min_dbm: f32,
    pub weak_links: u32,
    pub nlos_pct: f32,
    /// Links lost behind obstructions.
    pub blocked_links: u32,
    /// Mean UWB range bias on NLOS links, cm.
    pub nlos_bias_cm: f32,
    /// UWB / BLE tags heard by fewer than 3 anchors (no reliable 2D fix).
    pub tags_under3: u32,
    pub anchors_weak: u32,
    /// From the last coverage scan: floor share with a fix, and dead area (m²).
    pub covered_pct: Option<f32>,
    pub dead_area_m2: Option<f32>,
    pub anchors: Vec<AnchorRow>,
}

pub struct WeakLink {
    pub tag: String,
    pub other_id: String,
    pub dbm: f32,
    pub heard: bool,
    pub path: String,
    pub distance: f32,
}

pub struct Analysis {
    pub snap: RfSnapshot,
    /// Entity of each row of `snap.anchors`.
    pub anchor_entities: Vec<Entity>,
    pub weakest: Vec<WeakLink>,
    /// Signal (dBm) of every heard link, for the histogram.
    pub signals: Vec<f32>,
}

/// Analyses the links of the last update. `tech = None`: all technologies.
pub fn analyze(st: &RtlsState, ids: &HashMap<Entity, String>, tech: Option<Tech>) -> Analysis {
    #[derive(Default)]
    struct Acc {
        tech: Option<Tech>,
        in_range: u32,
        heard: u32,
        used: u32,
        dbm: Vec<f32>,
        nlos: u32,
        outliers: u32,
        bias: Vec<f32>,
    }
    let name = |e: &Entity| ids.get(e).cloned().unwrap_or_else(|| "?".into());
    let mut s = RfSnapshot::default();
    let mut per: HashMap<Entity, Acc> = HashMap::new();
    let mut signals = Vec::new();
    let mut bias = Vec::new();
    let mut nlos = 0u32;
    let mut weakest = Vec::new();
    for tr in st.tracks.values() {
        let mut heard_here = 0;
        let mut positioning = false;
        for l in &tr.links {
            let t = tech_of(l.kind);
            if tech.is_some_and(|x| x != t) {
                continue;
            }
            positioning |= t != Tech::Rfid;
            let heard = l.value.is_some();
            s.links += 1;
            let a = per.entry(l.other).or_default();
            a.tech = Some(t);
            a.in_range += 1;
            if l.walls > 0 {
                nlos += 1;
                a.nlos += 1;
            }
            if heard {
                s.heard += 1;
                heard_here += 1;
                a.heard += 1;
                a.dbm.push(l.rssi_dbm);
                signals.push(l.rssi_dbm);
                if l.used {
                    a.used += 1;
                }
                if l.rssi_dbm < weak_below(t) {
                    s.weak_links += 1;
                }
                if l.kind == LinkKind::Range
                    && let Some(r) = l.value
                {
                    let b = r - l.true_distance;
                    a.bias.push(b);
                    if l.walls > 0 {
                        bias.push(b);
                    }
                    if !l.used {
                        a.outliers += 1;
                    }
                }
            } else if l.walls > 0 || l.blocker.is_some() {
                s.blocked_links += 1;
            }
            if !heard || l.rssi_dbm < weak_below(t) {
                weakest.push(WeakLink {
                    tag: tr.id.clone(),
                    other_id: name(&l.other),
                    dbm: l.rssi_dbm,
                    heard,
                    path: l.path_label(),
                    distance: l.true_distance,
                });
            }
        }
        if positioning && heard_here < 3 {
            s.tags_under3 += 1;
        }
    }
    let mean = |v: &[f32]| if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
    let pct = |a: u32, b: u32| if b == 0 { 0.0 } else { a as f32 / b as f32 * 100.0 };
    s.success_pct = pct(s.heard, s.links);
    s.avg_dbm = mean(&signals);
    s.min_dbm = signals.iter().copied().fold(f32::INFINITY, f32::min);
    if !s.min_dbm.is_finite() {
        s.min_dbm = 0.0;
    }
    s.nlos_pct = pct(nlos, s.links);
    s.nlos_bias_cm = mean(&bias) * 100.0;
    let mut rows: Vec<(Entity, AnchorRow)> = per
        .into_iter()
        .map(|(e, a)| {
            let min = a.dbm.iter().copied().fold(f32::INFINITY, f32::min);
            (
                e,
                AnchorRow {
                    id: name(&e),
                    tech: a.tech.unwrap_or_default().label().to_lowercase(),
                    in_range: a.in_range,
                    heard: a.heard,
                    used: a.used,
                    success_pct: pct(a.heard, a.in_range),
                    avg_dbm: mean(&a.dbm),
                    min_dbm: if min.is_finite() { min } else { 0.0 },
                    nlos_pct: pct(a.nlos, a.in_range),
                    outliers: a.outliers,
                    bias_cm: mean(&a.bias) * 100.0,
                },
            )
        })
        .collect();
    rows.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    s.anchors_weak = rows.iter().filter(|(_, r)| matches!(r.status().0, "weak" | "no signal")).count() as u32;
    if let Some((covered, dead)) = st.rf_coverage {
        s.covered_pct = Some(covered * 100.0);
        s.dead_area_m2 = Some(dead);
    }
    weakest.sort_by(|a, b| (a.heard, a.dbm).partial_cmp(&(b.heard, b.dbm)).unwrap_or(std::cmp::Ordering::Equal));
    weakest.truncate(12);
    let anchor_entities = rows.iter().map(|r| r.0).collect();
    s.anchors = rows.into_iter().map(|r| r.1).collect();
    Analysis { snap: s, anchor_entities, weakest, signals }
}

// ---------------------------------------------------------------------------
// Dead zones

pub struct DeadZone {
    pub center: Vec3,
    pub area_m2: f32,
}

fn dead(tech: Tech, c: &Cell) -> bool {
    match tech {
        Tech::Rfid => c.heard == 0,
        _ => !c.err.is_finite(),
    }
}

/// Connected areas of the last coverage scan without a fix (UWB / BLE) or
/// without any reader (RFID), largest first; and the covered share.
pub fn dead_zones(cov: &Coverage) -> (f32, Vec<DeadZone>) {
    if cov.cells.is_empty() || cov.nx == 0 {
        return (0.0, Vec::new());
    }
    let (nx, nz) = (cov.nx, cov.nz);
    let is_dead: Vec<bool> = cov.cells.iter().map(|c| dead(cov.tech, c)).collect();
    let covered = is_dead.iter().filter(|d| !**d).count() as f32 / is_dead.len() as f32;
    let mut seen = vec![false; is_dead.len()];
    let mut zones = Vec::new();
    for start in 0..is_dead.len() {
        if !is_dead[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let (mut n, mut sx, mut sz) = (0usize, 0.0f32, 0.0f32);
        while let Some(i) = stack.pop() {
            let (x, z) = (i % nx, i / nx);
            n += 1;
            sx += x as f32;
            sz += z as f32;
            let mut push = |j: usize| {
                if is_dead[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < nx {
                push(i + 1);
            }
            if z > 0 {
                push(i - nx);
            }
            if z + 1 < nz {
                push(i + nx);
            }
        }
        let c = cov.origin + (Vec2::new(sx, sz) / n as f32 + 0.5) * cov.cell;
        zones.push(DeadZone { center: Vec3::new(c.x, 0.0, c.y), area_m2: n as f32 * cov.cell * cov.cell });
    }
    zones.sort_by(|a, b| b.area_m2.total_cmp(&a.area_m2));
    (covered, zones)
}

/// Keeps `RtlsState::rf_coverage` in step with the last coverage scan.
pub fn sync_coverage(cov: Res<Coverage>, mut st: ResMut<RtlsState>) {
    if !cov.is_changed() {
        return;
    }
    st.rf_coverage = (!cov.cells.is_empty()).then(|| {
        let (covered, zones) = dead_zones(&cov);
        (covered, zones.iter().map(|z| z.area_m2).sum())
    });
}

// ---------------------------------------------------------------------------
// History

#[derive(Clone, Copy)]
pub struct RfPoint {
    pub t: f64,
    pub success: f32,
    pub avg_dbm: f32,
    pub nlos: f32,
    pub weak: f32,
}

/// Last minutes of the RF summary per technology (UWB, BLE, RFID).
#[derive(Resource, Default)]
pub struct RfHistory {
    pub tech: [VecDeque<RfPoint>; 3],
    last_t: f64,
}

fn tech_index(t: Tech) -> usize {
    match t {
        Tech::Uwb => 0,
        Tech::Ble => 1,
        Tech::Rfid => 2,
    }
}

pub fn sample(st: Res<RtlsState>, mut hist: ResMut<RfHistory>) {
    if st.time < hist.last_t {
        *hist = RfHistory::default();
    }
    if st.time - hist.last_t < 0.5 {
        return;
    }
    hist.last_t = st.time;
    let ids = HashMap::new();
    for t in Tech::ALL {
        let a = analyze(&st, &ids, Some(t));
        if a.snap.links == 0 {
            continue;
        }
        let q = &mut hist.tech[tech_index(t)];
        q.push_back(RfPoint { t: st.time, success: a.snap.success_pct, avg_dbm: a.snap.avg_dbm, nlos: a.snap.nlos_pct, weak: a.snap.weak_links as f32 });
        while q.front().is_some_and(|p| p.t < st.time - 300.0) {
            q.pop_front();
        }
    }
}

// ---------------------------------------------------------------------------
// UI

fn kpi(ui: &mut egui::Ui, value: String, key: &str, col: Color32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(value).size(17.0).strong().color(col));
        ui.label(RichText::new(key).small().color(theme::WEAK));
    });
    ui.add_space(10.0);
}

fn histogram(ui: &mut egui::Ui, title: &str, values: &[f32], lo: f32, hi: f32, unit: &str, mark: Option<f32>, color: Color32) {
    const BINS: usize = 24;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 74.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    p.text(rect.left_top() + vec2(6.0, 2.0), egui::Align2::LEFT_TOP, format!("{title} ({} samples)", values.len()), egui::FontId::proportional(11.0), theme::TEXT);
    if values.is_empty() {
        return;
    }
    let mut bins = [0u32; BINS];
    for v in values {
        let k = (((v - lo) / (hi - lo)) * BINS as f32).floor().clamp(0.0, BINS as f32 - 1.0) as usize;
        bins[k] += 1;
    }
    let max = *bins.iter().max().unwrap_or(&1) as f32;
    let plot = egui::Rect::from_min_max(rect.min + vec2(4.0, 16.0), rect.max - vec2(4.0, 12.0));
    let w = plot.width() / BINS as f32;
    for (k, n) in bins.iter().enumerate() {
        let h = *n as f32 / max * plot.height();
        let r = egui::Rect::from_min_max(egui::pos2(plot.left() + k as f32 * w + 1.0, plot.bottom() - h), egui::pos2(plot.left() + (k + 1) as f32 * w - 1.0, plot.bottom()));
        p.rect_filled(r, 1.0, color);
    }
    if let Some(m) = mark {
        let x = plot.left() + ((m - lo) / (hi - lo)).clamp(0.0, 1.0) * plot.width();
        p.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], egui::Stroke::new(1.0, theme::DANGER));
    }
    let f = egui::FontId::proportional(10.0);
    p.text(egui::pos2(plot.left(), rect.bottom() - 1.0), egui::Align2::LEFT_BOTTOM, format!("{lo:.0}{unit}"), f.clone(), theme::WEAK);
    p.text(egui::pos2(plot.right(), rect.bottom() - 1.0), egui::Align2::RIGHT_BOTTOM, format!("{hi:.0}{unit}"), f, theme::WEAK);
}

/// The "RF analysis" tab.
#[allow(clippy::too_many_arguments)]
pub fn rf_tab(
    ui: &mut egui::Ui,
    tech: &mut Tech,
    state: &RtlsState,
    ids: &HashMap<Entity, String>,
    hist: &RfHistory,
    cov: &mut Coverage,
    coverage_job: &mut Option<CoverageRequest>,
    rsel: &mut RtlsSelection,
    orbit: &mut OrbitCamera,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Technology").color(theme::WEAK));
        for t in Tech::ALL {
            if ui.selectable_label(*tech == t, t.label()).clicked() {
                *tech = t;
            }
        }
    });
    let a = analyze(state, ids, Some(*tech));
    let s = &a.snap;
    if s.links == 0 {
        ui.label(RichText::new(format!("No {} links yet: place {} and tags, then run the simulation.", tech.label(), tech.infra().label())).color(theme::ACCENT));
    }

    // ---- Signal strength & link quality
    ui.add_space(4.0);
    ui.label(RichText::new("Signal & link quality").strong());
    ui.horizontal_wrapped(|ui| {
        let ok = |v: f32, good: f32, bad: f32| if v >= good { theme::SUCCESS } else if v >= bad { theme::ACCENT } else { theme::DANGER };
        kpi(ui, format!("{:.0}%", s.success_pct), "links heard", ok(s.success_pct, 90.0, 60.0));
        kpi(ui, format!("{:.0} dBm", s.avg_dbm), "avg signal", ok(s.avg_dbm, weak_below(*tech) + 10.0, weak_below(*tech)));
        kpi(ui, format!("{:.0} dBm", s.min_dbm), "weakest", ok(s.min_dbm, weak_below(*tech), weak_below(*tech) - 10.0));
        kpi(ui, format!("{}", s.weak_links), "weak links", if s.weak_links == 0 { theme::SUCCESS } else { theme::ACCENT });
        kpi(ui, format!("{:.0}%", s.nlos_pct), "NLOS", ok(100.0 - s.nlos_pct, 70.0, 40.0));
        kpi(ui, format!("{}", s.blocked_links), "blocked", if s.blocked_links == 0 { theme::SUCCESS } else { theme::DANGER });
        if *tech == Tech::Uwb {
            kpi(ui, format!("{:.0} cm", s.nlos_bias_cm), "NLOS range bias", ok(-s.nlos_bias_cm, -15.0, -50.0));
        }
        if *tech != Tech::Rfid {
            kpi(ui, format!("{}", s.tags_under3), "tags < 3 anchors", if s.tags_under3 == 0 { theme::SUCCESS } else { theme::DANGER });
        }
    });
    ui.label(RichText::new(format!("weak = below {:.0} dBm · NLOS = path crosses objects or walls", weak_below(*tech))).small().color(theme::WEAK));

    let h = &hist.tech[tech_index(*tech)];
    let series = |f: fn(&RfPoint) -> f32| h.iter().map(|p| (p.t, f(p))).collect::<Vec<_>>();
    super::tag_card::lines_chart(ui, "Avg signal (dBm)", " dBm", &[("avg".into(), Color32::from_rgb(94, 172, 255), series(|p| p.avg_dbm))], 70.0);
    ui.add_space(3.0);
    super::tag_card::lines_chart(
        ui,
        "Links heard % (green) · NLOS % (red)",
        " %",
        &[("heard".into(), theme::SUCCESS, series(|p| p.success)), ("nlos".into(), theme::DANGER, series(|p| p.nlos))],
        70.0,
    );
    ui.add_space(3.0);
    super::tag_card::lines_chart(ui, "Weak links", "", &[("weak".into(), theme::ACCENT, series(|p| p.weak))], 50.0);
    ui.add_space(3.0);
    let (lo, hi) = super::planning::signal_scale(*tech);
    histogram(ui, "Signal strength distribution", &a.signals, lo - 10.0, hi + 5.0, " dBm", Some(weak_below(*tech)), Color32::from_rgb(94, 172, 255));
    if *tech != Tech::Rfid {
        ui.add_space(3.0);
        let errs: Vec<f32> = state.errors.iter().filter(|(_, _, t)| *t == tech.label().to_lowercase()).map(|(_, e, _)| e * 100.0).collect();
        let max = errs.iter().copied().fold(50.0f32, f32::max).min(500.0);
        histogram(ui, "Position error distribution", &errs, 0.0, max, " cm", None, theme::ACCENT);
    }

    // ---- Anchor health
    ui.add_space(6.0);
    ui.label(RichText::new(format!("{} health", tech.infra().label())).strong());
    if s.anchors.is_empty() {
        ui.label(RichText::new("No links yet.").color(theme::WEAK));
    } else {
        egui::Grid::new("rf_anchors").striped(true).num_columns(8).spacing(vec2(10.0, 2.0)).show(ui, |ui| {
            for hdr in ["", "heard", "ok %", "avg dBm", "min dBm", "NLOS %", "outliers", "status"] {
                ui.label(RichText::new(hdr).small().color(theme::WEAK));
            }
            ui.end_row();
            for (row, e) in s.anchors.iter().zip(&a.anchor_entities) {
                let (status, col) = row.status();
                if ui.link(RichText::new(&row.id).strong()).on_hover_text("Select it in the scene").clicked() {
                    rsel.device = Some(*e);
                }
                ui.label(format!("{}/{}", row.heard, row.in_range));
                ui.label(format!("{:.0}", row.success_pct));
                ui.label(RichText::new(format!("{:.0}", row.avg_dbm)).monospace());
                ui.label(RichText::new(format!("{:.0}", row.min_dbm)).monospace());
                ui.label(format!("{:.0}", row.nlos_pct));
                ui.label(if *tech == Tech::Uwb { format!("{} · {:+.0} cm", row.outliers, row.bias_cm) } else { "–".into() });
                ui.label(RichText::new(status).color(col));
                ui.end_row();
            }
        });
    }

    // ---- Weakest links
    if !a.weakest.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("Weakest / lost links").strong());
        egui::Grid::new("rf_weak").striped(true).num_columns(5).spacing(vec2(10.0, 2.0)).show(ui, |ui| {
            for w in &a.weakest {
                ui.label(RichText::new(format!("{} → {}", w.tag, w.other_id)).strong());
                ui.label(RichText::new(format!("{:.0} dBm", w.dbm)).monospace().color(if w.heard { theme::ACCENT } else { theme::DANGER }));
                ui.label(if w.heard { "weak" } else { "lost" });
                ui.label(RichText::new(&w.path).small().color(theme::WEAK));
                ui.label(RichText::new(format!("{:.1} m", w.distance)).small().color(theme::WEAK));
                ui.end_row();
            }
        });
    }

    // ---- Coverage & dead zones
    ui.add_space(6.0);
    ui.label(RichText::new("Coverage & dead zones").strong());
    ui.horizontal(|ui| {
        let busy = coverage_job.is_some();
        if ui.add_enabled(!busy, egui::Button::new(if busy { "Scanning…" } else { "Scan coverage" })).on_hover_text("Ray-casts every 0.5 m of floor at tag height (1.2 m)").clicked() {
            *coverage_job = Some(CoverageRequest { tech: *tech, height: 1.2, cell: 0.5 });
        }
        if !cov.cells.is_empty() && ui.selectable_label(cov.visible, "Show on floor").clicked() {
            cov.visible = !cov.visible;
            cov.dirty = true;
        }
    });
    if cov.cells.is_empty() {
        ui.label(RichText::new("Not scanned yet.").color(theme::WEAK));
    } else if cov.tech != *tech {
        ui.label(RichText::new(format!("Last scan was for {}: scan again for {}.", cov.tech.label(), tech.label())).color(theme::WEAK));
    } else {
        let (covered, zones) = dead_zones(cov);
        let dead_area: f32 = zones.iter().map(|z| z.area_m2).sum();
        let col = if covered >= 0.95 { theme::SUCCESS } else if covered >= 0.8 { theme::ACCENT } else { theme::DANGER };
        ui.label(RichText::new(format!("{:.0}% of the floor covered · {} dead zones · {:.0} m² without {}", covered * 100.0, zones.len(), dead_area, if *tech == Tech::Rfid { "a read" } else { "a fix" })).color(col));
        egui::Grid::new("rf_dead").striped(true).num_columns(3).show(ui, |ui| {
            for (k, z) in zones.iter().take(8).enumerate() {
                ui.label(format!("#{}", k + 1));
                ui.label(format!("{:.1} m² around ({:.1}, {:.1})", z.area_m2, z.center.x, z.center.z));
                if ui.small_button("Show").clicked() {
                    orbit.focus = z.center;
                    orbit.radius = orbit.radius.min((z.area_m2.sqrt() * 2.5).max(6.0));
                    cov.visible = true;
                    cov.dirty = true;
                }
                ui.end_row();
            }
        });
        if zones.len() > 8 {
            ui.label(RichText::new(format!("+ {} smaller", zones.len() - 8)).small().color(theme::WEAK));
        }
        ui.label(RichText::new("The scan includes the padding around the scene, so the area outside the outer walls shows up as dead too.").small().color(theme::WEAK));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_zones_are_connected_areas() {
        // 4x3 grid, dead cells marked x:
        //   x x . .
        //   . . . x
        //   . . . x
        let mut cov = Coverage::default();
        (cov.tech, cov.cell, cov.nx, cov.nz) = (Tech::Uwb, 1.0, 4, 3);
        let live = Cell { err: 0.1, heard: 4, ..Default::default() };
        let gone = Cell { err: f32::NAN, ..Default::default() };
        let dead_at = [0, 1, 7, 11];
        cov.cells = (0..12).map(|i| if dead_at.contains(&i) { gone } else { live }).collect();
        let (covered, zones) = dead_zones(&cov);
        assert!((covered - 8.0 / 12.0).abs() < 1e-6);
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].area_m2, 2.0);
        assert_eq!(zones[1].area_m2, 2.0);
    }
}
