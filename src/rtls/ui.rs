//! The RTLS window: device palette and list, inspector, live dashboard,
//! planning, outputs (OpenEMS / MQTT / Modbus / webhook / UDP / recorder)
//! and settings. Plus device labels drawn over the viewport.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};
use egui::{Color32, RichText, vec2};

use super::deploy::{DeployReport, DeployRequest};
use super::device::{Device, DeviceKind, Motion, Profiles, Zone, epc_for};
use super::openems_local::{LocalEms, Phase};
use super::events::RtlsEvent;
use super::io::{EventFilter, Hub, LinkState, OutputsConfig, UdpFormat};
use super::planning::{Coverage, CoverageRequest, MapSource, MapView, OptimizeRequest, Tech};
use super::{DevicePlace, LinkKind, RtlsConfig, RtlsSelection, RtlsState};
use crate::scene::{OrbitCamera, PlacedObject};
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Devices,
    Inspector,
    Live,
    Rf,
    Planning,
    Outputs,
    Settings,
}

impl Tab {
    const ALL: [Tab; 7] = [Tab::Devices, Tab::Inspector, Tab::Live, Tab::Rf, Tab::Planning, Tab::Outputs, Tab::Settings];
    fn label(self) -> &'static str {
        match self {
            Tab::Devices => "Devices",
            Tab::Inspector => "Inspector",
            Tab::Live => "Live",
            Tab::Rf => "RF analysis",
            Tab::Planning => "Planning",
            Tab::Outputs => "Outputs / EMS",
            Tab::Settings => "Settings",
        }
    }
}

/// The panel's top-level pages.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Positioning,
    OpenEms,
    Fdtd,
}

pub struct UiLocal {
    page: Page,
    tab: Tab,
    tech: Tech,
    height: f32,
    cell: f32,
    add: usize,
    draft: Option<OutputsConfig>,
    new_zone: String,
    advanced: bool,
}

impl Default for UiLocal {
    fn default() -> Self {
        Self { page: Page::Positioning, tab: Tab::Devices, tech: Tech::Uwb, height: 1.2, cell: 0.5, add: 4, draft: None, new_zone: "Zone 1".into(), advanced: false }
    }
}

/// Planning jobs need exclusive world access; the UI queues them here.
#[derive(Resource, Default)]
pub struct PlanJobs {
    pub coverage: Option<CoverageRequest>,
    pub optimize: Option<OptimizeRequest>,
}

pub fn run_plan_jobs(world: &mut World) {
    let (cov, opt) = {
        let mut j = world.resource_mut::<PlanJobs>();
        (j.coverage.take(), j.optimize.take())
    };
    if let Some(r) = opt {
        let _ = world.run_system_cached_with(super::planning::optimize, r);
    }
    if let Some(r) = cov {
        let _ = world.run_system_cached_with(super::planning::compute, r);
    }
}

fn state_color(s: LinkState) -> Color32 {
    match s {
        LinkState::Connected => theme::SUCCESS,
        LinkState::Connecting => theme::ACCENT,
        LinkState::Error => theme::DANGER,
    }
}

fn c32(c: Color) -> Color32 {
    let s = c.to_srgba();
    Color32::from_rgb((s.red * 255.0) as u8, (s.green * 255.0) as u8, (s.blue * 255.0) as u8)
}

fn filter_ui(ui: &mut egui::Ui, f: &mut EventFilter) {
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut f.positions, "positions");
        ui.checkbox(&mut f.measurements, "raw ranges/RSSI");
        ui.checkbox(&mut f.rfid, "RFID reads");
        ui.checkbox(&mut f.zones, "zones");
        ui.checkbox(&mut f.alarms, "alarms");
        ui.checkbox(&mut f.status, "device status");
        ui.checkbox(&mut f.snapshots, "snapshots");
    });
}

fn vec3_edit(ui: &mut egui::Ui, v: &mut Vec3, speed: f64) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        for (i, l) in ["x", "y", "z"].iter().enumerate() {
            ui.label(RichText::new(*l).color(theme::AXIS[i]));
            ch |= ui.add(egui::DragValue::new(&mut v[i]).speed(speed).max_decimals(2).suffix(" m")).changed();
        }
    });
    ch
}

fn arr_edit(ui: &mut egui::Ui, a: &mut [f32; 3]) -> bool {
    let mut v = Vec3::from(*a);
    let ch = vec3_edit(ui, &mut v, 0.05);
    *a = v.to_array();
    ch
}

/// Line chart drawn with the painter (no extra plotting crate).
fn chart(ui: &mut egui::Ui, series: &[(f64, f32)], color: Color32, label: &str, unit: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 70.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    if series.len() < 2 {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, format!("{label}: waiting for data"), egui::FontId::proportional(11.0), theme::WEAK);
        return;
    }
    let (t0, t1) = (series[0].0, series[series.len() - 1].0);
    let max = series.iter().map(|s| s.1).fold(0.0f32, f32::max).max(1e-3) * 1.15;
    let pts: Vec<egui::Pos2> = series
        .iter()
        .map(|(t, v)| {
            let x = rect.left() + ((t - t0) / (t1 - t0).max(1e-6)) as f32 * rect.width();
            let y = rect.bottom() - (v / max) * (rect.height() - 14.0) - 2.0;
            egui::pos2(x, y)
        })
        .collect();
    p.add(egui::Shape::line(pts, egui::Stroke::new(1.5, color)));
    let last = series.last().unwrap().1;
    p.text(rect.left_top() + vec2(6.0, 3.0), egui::Align2::LEFT_TOP, format!("{label}: {last:.2}{unit}  (max {:.2})", max / 1.15), egui::FontId::proportional(11.0), theme::TEXT);
}

/// Choices for the one-button deployment and its last result.
#[derive(Resource)]
pub struct DeployState {
    pub uwb: bool,
    pub ble: bool,
    pub rfid: bool,
    pub target_cm: f32,
    /// Anchor cap; 0 = as few as meet the target.
    pub max_anchors: usize,
    /// UWB anchor transmit power, dBm/MHz; `None` = the profile's.
    pub anchor_tx_dbm_mhz: Option<f32>,
    pub pending: Option<DeployRequest>,
    pub last: Option<DeployReport>,
}

impl Default for DeployState {
    fn default() -> Self {
        Self { uwb: true, ble: false, rfid: false, target_cm: 30.0, max_anchors: 0, anchor_tx_dbm_mhz: None, pending: None, last: None }
    }
}

pub fn run_deploy_jobs(world: &mut World) {
    let Some(req) = world.resource_mut::<DeployState>().pending.take() else { return };
    let report = super::deploy::auto_deploy(world, &req);
    world.resource_mut::<DeployState>().last = Some(report);
}

/// Plain-language version of an output error.
fn humanize(detail: &str) -> String {
    let d = detail.to_ascii_lowercase();
    if d.contains("10061") || d.contains("refused") || d.contains("connection") && d.contains("failed") {
        "Can't reach OpenEMS. Is it running? Use Install & start below.".into()
    } else if d.contains("401") || d.contains("denied") {
        "OpenEMS rejected the password.".into()
    } else if d.contains("unable to find openems component") || d.contains("404") {
        "OpenEMS has no rtls0 component. The built-in OpenEMS adds it automatically.".into()
    } else if d.contains("timed out") || d.contains("timeout") {
        "OpenEMS doesn't answer (timeout).".into()
    } else {
        detail.chars().take(140).collect()
    }
}

fn step_header(ui: &mut egui::Ui, n: &str, title: &str, done: bool) {
    ui.horizontal(|ui| {
        let col = if done { theme::SUCCESS } else { theme::ACCENT };
        ui.label(RichText::new(n).strong().size(16.0).color(col));
        ui.label(RichText::new(title).strong().size(15.0));
        if done {
            ui.label(RichText::new("✔").color(theme::SUCCESS));
        }
    });
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new().fill(theme::RAISED).corner_radius(8).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    })
    .inner
}

/// UI state shared by the panel (wherever it is shown) and the overlay.
#[derive(Resource, Default)]
pub struct RtlsUiState {
    pub local: UiLocal,
    /// The tag whose details dock was closed (selecting another tag reopens it).
    pub tag_dock_hidden: Option<Entity>,
}

/// Names of placed objects (for "follow object"), refreshed every frame so the
/// panel does not need to query `PlacedObject` itself.
#[derive(Resource, Default)]
pub struct ObjectNames(pub Vec<String>);

pub fn refresh_object_names(objects: Query<&PlacedObject>, mut names: ResMut<ObjectNames>) {
    let mut v: Vec<String> = objects.iter().map(|o| o.name.clone()).collect();
    v.sort();
    if names.0 != v {
        names.0 = v;
    }
}

pub type DeviceQuery<'w, 's> = Query<'w, 's, (Entity, &'static mut Device, &'static mut Transform), (Without<OrbitCamera>, Without<PlacedObject>)>;

/// Everything the RTLS panel needs (drawn into the left-panel "Positioning"
/// tab by `rtls_ui`). It touches no `Camera`, `PlacedObject` or object
/// `Transform`, so it can sit next to the scene UI.
#[derive(SystemParam)]
pub struct RtlsPanel<'w, 's> {
    commands: Commands<'w, 's>,
    cfg: ResMut<'w, RtlsConfig>,
    state: ResMut<'w, RtlsState>,
    hub: ResMut<'w, Hub>,
    place: ResMut<'w, DevicePlace>,
    rsel: ResMut<'w, RtlsSelection>,
    cov: ResMut<'w, Coverage>,
    profiles: ResMut<'w, Profiles>,
    jobs: ResMut<'w, PlanJobs>,
    ems: ResMut<'w, LocalEms>,
    deploy: ResMut<'w, DeployState>,
    bindings: Res<'w, super::actuate::EmsBindings>,
    ui_state: ResMut<'w, RtlsUiState>,
    devices: DeviceQuery<'w, 's>,
    zones: Query<'w, 's, (Entity, &'static mut Zone)>,
    orbit: Query<'w, 's, &'static mut OrbitCamera>,
    names: Res<'w, ObjectNames>,
    rf_hist: Res<'w, super::rf::RfHistory>,
    fdtd: ResMut<'w, super::fdtd::Fdtd>,
    applied: ResMut<'w, super::planning::AppliedAntenna>,
    library: ResMut<'w, super::antenna::AntennaLibrary>,
}

impl RtlsPanel<'_, '_> {
    /// The panel body: three pages. Positioning (① Deploy, ② Test live,
    /// Advanced tools), OpenEMS (the energy-management link) and FDTD (the
    /// openEMS field solver).
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let Self { commands, cfg, state, hub, place, rsel, cov, profiles, jobs, ems, deploy, bindings, ui_state, devices, zones, orbit, names, rf_hist, fdtd, applied, library } = self;
        let local = &mut ui_state.local;
        let Ok(mut orbit) = orbit.single_mut() else { return };
        theme::segmented_fill(ui, &mut local.page, &[(Page::Positioning, "Positioning"), (Page::OpenEms, "OpenEMS Edge"), (Page::Fdtd, "RF solver (FDTD)")]);
        ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt(("rtls_page", local.page as u8)).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                match local.page {
                    Page::OpenEms => {
                        openems_page(ui, ems, hub, bindings, state);
                        return;
                    }
                    Page::Fdtd => {
                        // Applying an FDTD antenna redraws that technology's floor map.
                        if let Some(t) = super::fdtd::ui::tab(ui, fdtd, applied, library, cov) {
                            cov.visible = true;
                            jobs.coverage = Some(CoverageRequest { tech: t, height: 1.2, cell: 0.5 });
                        }
                        return;
                    }
                    Page::Positioning => {}
                }

                // ① Deploy
                let deployed = deploy.last.is_some() || devices.iter().any(|(_, d, _)| !d.kind.is_mobile());
                step_header(ui, "①", "Deploy", deployed);
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        // One positioning technology: UWB or BLE.
                        if ui.radio(deploy.uwb, "UWB").clicked() {
                            (deploy.uwb, deploy.ble) = (true, false);
                        }
                        if ui.radio(deploy.ble, "BLE").clicked() {
                            (deploy.uwb, deploy.ble) = (false, true);
                        }
                        ui.label(RichText::new("accuracy").color(theme::WEAK));
                        egui::ComboBox::from_id_salt("rtls_target").selected_text(format!("{:.0} cm", deploy.target_cm)).width(70.0).show_ui(ui, |ui| {
                            for t in [10.0, 20.0, 30.0, 50.0, 100.0] {
                                ui.selectable_value(&mut deploy.target_cm, t, format!("{t:.0} cm"));
                            }
                        });
                    });
                    if deploy.uwb {
                        ui.horizontal(|ui| {
                            let mut limit = deploy.max_anchors > 0;
                            if ui
                                .checkbox(&mut limit, "limit anchors to")
                                .on_hover_text("Off = the fewest anchors that meet the accuracy target on the floor and in every zone. On = at most this many (yours included), placed for the best coverage.")
                                .changed()
                            {
                                deploy.max_anchors = if limit { deploy.last.as_ref().map_or(8, |r| r.anchors.max(3)) } else { 0 };
                            }
                            if limit {
                                ui.add(egui::DragValue::new(&mut deploy.max_anchors).range(3..=64).suffix(" anchors"));
                            } else {
                                ui.label(RichText::new("fewest that meet the target").color(theme::WEAK));
                            }
                        });
                        if let Some(p) = profiles.default_for(super::device::DeviceKind::UwbAnchor).cloned() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("anchor TX power").color(theme::WEAK));
                                let mut tx = deploy.anchor_tx_dbm_mhz.unwrap_or(p.uwb_tx_dbm_mhz);
                                if ui
                                    .add(egui::DragValue::new(&mut tx).range(-80.0..=super::radio::UWB_MAX_DBM_MHZ).speed(0.5).suffix(" dBm/MHz"))
                                    .on_hover_text("UWB transmit power (−41.3 = regulatory maximum). Lower power = shorter range: the optimiser plans with it, so it may need more anchors.")
                                    .changed()
                                {
                                    deploy.anchor_tx_dbm_mhz = Some(tx);
                                    // Applies now: the floor map (rescans by itself) and the live test use it.
                                    let tx = tx.min(super::radio::UWB_MAX_DBM_MHZ);
                                    for q in profiles.0.iter_mut().filter(|q| q.kind == super::device::DeviceKind::UwbAnchor) {
                                        q.uwb_tx_dbm_mhz = tx;
                                    }
                                }
                                let range = super::radio::uwb_link_range_m(&super::device::Profile { uwb_tx_dbm_mhz: tx, ..p.clone() });
                                let short = range < MIN_USEFUL_RANGE_M;
                                ui.label(RichText::new(format!("≈ {range:.1} m reach")).color(if short { theme::DANGER } else { theme::WEAK }));
                            });
                            let tx_now = deploy.anchor_tx_dbm_mhz.unwrap_or(p.uwb_tx_dbm_mhz);
                            if range_of(tx_now, &p) < MIN_USEFUL_RANGE_M {
                                ui.label(
                                    RichText::new(format!(
                                        "⚠ Too weak: an anchor reaches only {:.1} m, so tags can't be positioned. Raise the power (−41.3 is the legal maximum) or use many anchors close together.",
                                        range_of(tx_now, &p)
                                    ))
                                    .small()
                                    .color(theme::DANGER),
                                );
                            }
                        }
                    }
                    let busy = deploy.pending.is_some();
                    let label = if busy { "Placing devices and testing…" } else { "⚡ Auto deploy" };
                    let b = egui::Button::new(RichText::new(label).strong().size(15.0)).fill(theme::ACCENT.gamma_multiply(if busy { 0.4 } else { 0.85 })).min_size(vec2(ui.available_width(), 34.0));
                    if ui.add_enabled(!busy && (deploy.uwb || deploy.ble || deploy.rfid), b)
                        .on_hover_text("Places anchors, beacons, readers and tags for this scene, then runs a 30 s test. Running it again replaces only what it placed.")
                        .clicked()
                    {
                        deploy.pending = Some(DeployRequest { uwb: deploy.uwb, ble: deploy.ble, rfid: false, target_m: deploy.target_cm / 100.0, max_anchors: (deploy.max_anchors > 0).then_some(deploy.max_anchors), anchor_tx_dbm_mhz: deploy.anchor_tx_dbm_mhz, ..Default::default() });
                    }
                    if let Some(r) = &deploy.last {
                        deploy_result(ui, r);
                    }
                    // Floor map: where readings are good enough, predicted accuracy or
                    // scanned signal strength. Available as soon as any anchor,
                    // beacon or reader is placed (by hand or by Auto deploy).
                    let placed = |k: super::device::DeviceKind| devices.iter().any(|(_, d, _)| d.kind == k);
                    let infra_tech = Tech::ALL.into_iter().find(|t| placed(t.infra()));
                    if !cov.cells.is_empty() || deploy.last.is_some() || infra_tech.is_some() {
                        // The quality criterion follows the accuracy target above.
                        let target_m = deploy.target_cm / 100.0;
                        if cov.quality.target_m != target_m {
                            cov.quality.target_m = target_m;
                            cov.dirty = true;
                        }
                        let scan_tech = if cov.cells.is_empty() { infra_tech.unwrap_or(if deploy.ble { Tech::Ble } else { Tech::Uwb }) } else { cov.tech };
                        // Two full-width rows (what to show, for which technology), so
                        // the controls never push the panel wider than it is.
                        ui.add_space(2.0);
                        theme::caption(ui, "Floor map");
                        let mut view = cov.visible.then_some(cov.view);
                        if theme::segmented_fill(ui, &mut view, &[(None, "Off"), (Some(MapView::Quality), "Quality"), (Some(MapView::Accuracy), "Accuracy"), (Some(MapView::Signal), "Signal")]) {
                            cov.visible = view.is_some();
                            if let Some(v) = view {
                                cov.view = v;
                                if cov.cells.is_empty() {
                                    jobs.coverage = Some(CoverageRequest { tech: scan_tech, height: 1.2, cell: 0.5 });
                                }
                            }
                            cov.dirty = true;
                        }
                        if cov.visible {
                            let mut tech = if cov.cells.is_empty() { scan_tech } else { cov.tech };
                            let opts: Vec<(Tech, &str)> = Tech::ALL.iter().map(|t| (*t, t.label())).collect();
                            if theme::segmented_fill(ui, &mut tech, &opts) {
                                jobs.coverage = Some(CoverageRequest { tech, height: 1.2, cell: 0.5 });
                            }
                        }
                        // Propagation model: the ray model, or a whole-scene FDTD run of this technology.
                        if cov.visible && !cov.cells.is_empty() && fdtd.scene(cov.tech).is_some() {
                            let mut src = cov.source;
                            if theme::segmented_fill(ui, &mut src, &[(MapSource::Ray, "Ray model"), (MapSource::Fdtd, "FDTD whole scene")]) {
                                cov.source = src;
                                jobs.coverage = Some(CoverageRequest { tech: cov.tech, height: if cov.height > 0.0 { cov.height } else { 1.2 }, cell: 0.5 });
                            }
                        }
                        if cov.visible && !cov.source_note.is_empty() {
                            ui.label(RichText::new(&cov.source_note).small().color(theme::ONLINE));
                        }
                        if cov.view == MapView::Quality && cov.tech != Tech::Rfid {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("pass where ≥").color(theme::WEAK));
                                let mut pct = cov.quality.min_share * 100.0;
                                if ui
                                    .add(egui::DragValue::new(&mut pct).range(50.0..=99.0).speed(0.5).suffix(" %"))
                                    .on_hover_text("Share of position readings that must be correct")
                                    .changed()
                                {
                                    cov.quality.min_share = pct / 100.0;
                                    cov.dirty = true;
                                }
                                ui.label(RichText::new(format!("of readings are within {:.0} cm", deploy.target_cm)).color(theme::WEAK));
                            });
                            if !cov.cells.is_empty() {
                                let share = cov.quality_share();
                                let col = if share >= 0.95 { theme::SUCCESS } else { theme::ACCENT };
                                ui.label(RichText::new(format!("{:.0}% of the floor passes ({}, tag at {:.1} m)", share * 100.0, cov.tech.label(), cov.height)).color(col));
                            }
                        }
                        let legend = match (cov.view, cov.tech) {
                            (MapView::Quality, Tech::Rfid) | (MapView::Accuracy, Tech::Rfid) => "blue = tags readable here · grey = no read".to_string(),
                            (MapView::Quality, _) => format!(
                                "green = ≥ {:.0}% of readings within {:.0} cm · orange/red = less · grey = no position",
                                cov.quality.min_share * 100.0,
                                deploy.target_cm
                            ),
                            (MapView::Accuracy, _) => "green ≤ 10 cm · yellow ≈ 30 cm · red ≥ 1 m · grey = no position".to_string(),
                            (MapView::Signal, t) => {
                                let (lo, hi) = super::planning::signal_scale(t);
                                format!("red {lo:.0} dBm (weak) → green {hi:.0} dBm (strong) · grey = nothing heard")
                            }
                        };
                        ui.label(RichText::new(legend).small().color(theme::WEAK));
                        if let Some(a) = applied.0.as_ref().filter(|a| a.tech == cov.tech && !cov.cells.is_empty()) {
                            ui.label(RichText::new(format!("{} Antennas: {} from FDTD (pattern + {:.1} dB match)", theme::icon::BROADCAST, a.label, a.mismatch_db)).small().color(theme::ONLINE));
                        }
                    }
                });

                // ② Test
                ui.add_space(4.0);
                let snap = state.snapshot.clone();
                step_header(ui, "②", "Test live", cfg.settings.running && snap.tags_total > 0);
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let s = &mut cfg.settings;
                        let label = if s.running { "⏸ Pause" } else { "▶ Run" };
                        if ui.add(egui::Button::new(RichText::new(label).strong()).min_size(vec2(80.0, 28.0))).clicked() {
                            s.running = !s.running;
                        }
                        egui::ComboBox::from_id_salt("rtls_speed").selected_text(format!("{}×", s.speed)).width(55.0).show_ui(ui, |ui| {
                            for v in [0.5, 1.0, 2.0, 5.0, 10.0] {
                                ui.selectable_value(&mut s.speed, v, format!("{v}×"));
                            }
                        });
                        if ui.button("⟲").on_hover_text("Restart time and statistics").clicked() {
                            let seed = s.seed;
                            state.restart(seed);
                        }
                        ui.label(RichText::new(format!("{:.0} s", state.time)).color(theme::WEAK));
                    });
                    super::tag_card::add_tags_row(ui, place, profiles, devices.iter().map(|(_, d, t)| (d, t.translation)), commands, &mut state.rng);
                    if ui.button("RF analysis: signal, NLOS, anchors, dead zones").clicked() {
                        local.advanced = true;
                        local.tab = Tab::Rf;
                    }
                    let (rmse, _, c95, _) = state.error_stats(None);
                    ui.horizontal(|ui| {
                        let kpi = |ui: &mut egui::Ui, v: String, k: &str, col: Color32| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(v).size(18.0).strong().color(col));
                                ui.label(RichText::new(k).small().color(theme::WEAK));
                            });
                            ui.add_space(14.0);
                        };
                        let located_col = if snap.tags_lost > 0 { theme::ACCENT } else { theme::SUCCESS };
                        kpi(ui, format!("{}/{}", snap.tags_located, snap.tags_total), "tags located", located_col);
                        kpi(ui, format!("{:.0} cm", rmse * 100.0), "avg error", theme::TEXT);
                        kpi(ui, format!("{:.0} cm", c95 * 100.0), "95% within", theme::TEXT);
                        kpi(ui, format!("{}", state.alarms.len()), "alarms", if state.alarms.is_empty() { theme::TEXT } else { theme::DANGER });
                    });
                    for a in state.alarms.values().take(3) {
                        ui.label(RichText::new(format!("{} {}", theme::icon::WARNING_CIRCLE, a.message)).color(theme::DANGER).small());
                    }
                    if !snap.tags.is_empty() {
                        egui::Grid::new("rtls_simple_tags").striped(true).num_columns(3).show(ui, |ui| {
                            for t in snap.tags.iter().take(8) {
                                ui.label(RichText::new(&t.id).strong());
                                let tr = state.tracks.values().find(|x| x.id == t.id);
                                let status = match tr {
                                    _ if t.lost => RichText::new("no position").color(theme::DANGER),
                                    Some(x) if x.tech == "rfid" => RichText::new(format!("read {}×", x.reads)).color(theme::SUCCESS),
                                    Some(x) => RichText::new(format!("{:.0} cm", x.err.unwrap_or(0.0) * 100.0)),
                                    None => RichText::new(""),
                                };
                                ui.label(status);
                                let zone = tr.and_then(|x| x.zones.iter().next().cloned()).unwrap_or_default();
                                ui.label(RichText::new(zone).color(theme::WEAK));
                                ui.end_row();
                            }
                        });
                        if snap.tags.len() > 8 {
                            ui.label(RichText::new(format!("+ {} more", snap.tags.len() - 8)).small().color(theme::WEAK));
                        }
                    }
                });

                // Advanced
                ui.add_space(6.0);
                let header = egui::CollapsingHeader::new(RichText::new("Advanced").color(theme::WEAK)).open(Some(local.advanced)).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for t in Tab::ALL {
                            if ui.selectable_label(local.tab == t, t.label()).clicked() {
                                local.tab = t;
                            }
                        }
                    });
                    ui.separator();
                    match local.tab {
                        Tab::Devices => devices_tab(ui, local, place, rsel, profiles, devices, zones, commands, state, &mut orbit),
                        Tab::Inspector => inspector_tab(ui, place, rsel, profiles, library, devices, zones, &names.0, state, commands),
                        Tab::Live => live_tab(ui, state, hub, devices),
                        Tab::Rf => {
                            let ids: std::collections::HashMap<Entity, String> = devices.iter().map(|(e, d, _)| (e, d.id.clone())).collect();
                            super::rf::rf_tab(ui, &mut local.tech, state, &ids, rf_hist, cov, &mut jobs.coverage, rsel, &mut orbit)
                        }
                        Tab::Planning => planning_tab(ui, local, cov, jobs, profiles, commands, devices),
                        Tab::Outputs => outputs_tab(ui, local, cfg, hub),
                        Tab::Settings => settings_tab(ui, cfg),
                    }
                });
                if header.header_response.clicked() {
                    local.advanced = !local.advanced;
                }
            });
    }
}

/// The OpenEMS page: one status card with the one action that fits it, what
/// OpenEMS reads and drives, and the technical details folded away.
fn openems_page(ui: &mut egui::Ui, ems: &mut LocalEms, hub: &Hub, bindings: &super::actuate::EmsBindings, state: &RtlsState) {
    let phase = ems.phase();
    let sink = hub.sinks.iter().find(|s| s.name == "OpenEMS REST").map(|s| s.shared.status());
    let installed = LocalEms::installed();
    let connected = sink.as_ref().is_some_and(|s| s.state == LinkState::Connected);
    let link_error = sink.as_ref().filter(|s| s.state == LinkState::Error).map(|s| humanize(&s.detail));
    let (dot, title, detail) = match &phase {
        Phase::Idle if !installed => (theme::WEAK, "Not installed", "The first start downloads Java 21 and OpenEMS Edge (~120 MB). No admin rights needed.".to_string()),
        Phase::Idle => (theme::WEAK, "Stopped", "Start it to stream the positioning data into a local OpenEMS Edge.".to_string()),
        Phase::Failed(e) => (theme::DANGER, "Couldn't start", e.chars().take(200).collect()),
        Phase::Working(step, p) if *p >= 0.0 => (theme::ACCENT, "Starting…", format!("{step} · {:.0}%", p * 100.0)),
        Phase::Working(step, _) => (theme::ACCENT, "Starting…", step.clone()),
        Phase::Running { .. } if connected => (theme::SUCCESS, "Connected", "Positioning data is streaming into OpenEMS.".to_string()),
        Phase::Running { .. } => match &link_error {
            Some(e) => (theme::DANGER, "Can't reach OpenEMS", e.clone()),
            None => (theme::ACCENT, "Connecting…", String::new()),
        },
    };

    // ---- Status and the action that goes with it
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").size(14.0).color(dot));
            ui.label(RichText::new(title).strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let accent = |text: &str| egui::Button::new(RichText::new(text).strong().color(Color32::from_rgb(28, 22, 8))).fill(theme::ACCENT).min_size(vec2(0.0, 28.0));
                match &phase {
                    Phase::Idle | Phase::Failed(_) => {
                        if ui.add(accent(if installed { "▶ Start" } else { "⬇ Install & start" })).clicked() {
                            ems.start();
                        }
                    }
                    Phase::Working(..) => {
                        ui.spinner();
                    }
                    Phase::Running { .. } => {
                        if !connected || !ems.owns_process() {
                            if ui.add(accent(if installed { "⟲ Restart" } else { "⬇ Install & start" })).clicked() {
                                ems.stop();
                                ems.start();
                            }
                        } else if ui.button("Stop").clicked() {
                            ems.stop();
                        }
                    }
                }
            });
        });
        if !detail.is_empty() {
            ui.add(egui::Label::new(RichText::new(&detail).small().color(if dot == theme::DANGER { theme::TEXT } else { theme::WEAK })).wrap());
        }
        if let Phase::Working(_, p) = &phase
            && *p >= 0.0
        {
            ui.add(egui::ProgressBar::new(*p).desired_height(6.0));
        }
        if let Phase::Running { attached } = &phase {
            ui.label(
                RichText::new(if *attached { "Using an OpenEMS that was already running on this PC." } else { "Runs in the background and stops when Sense Sim closes." })
                    .small()
                    .color(theme::WEAK),
            );
        }
    });

    if let Phase::Running { .. } = &phase {
        // ---- What OpenEMS reads
        ui.add_space(4.0);
        theme::caption(ui, "What OpenEMS sees");
        let fb = hub.feedback();
        let get = |k: &str| fb.iter().find(|(s, _, _)| s == &format!("openems:{k}")).map(|(_, v, _)| v.clone()).unwrap_or_else(|| "–".into());
        super::fdtd::ui::tiles(
            ui,
            &[
                (get("rtls0/TagsLocated"), "Tags located", theme::TEXT),
                (get("_sum/EssSoc"), "Battery (%)", theme::TEXT),
                (get("_sum/GridActivePower"), "Grid power (W)", theme::TEXT),
            ],
        );
        if ui.add(egui::Button::new(theme::label(theme::icon::ARROW_SQUARE_OUT, "Open the OpenEMS console")).min_size(vec2(ui.available_width(), 28.0))).on_hover_text("Log in with admin / admin").clicked() {
            let _ = open_url(super::openems_local::CONSOLE_URL);
        }
        ui.label(RichText::new("Console login: admin / admin").small().color(theme::WEAK));

        // ---- What OpenEMS drives in the scene
        ui.add_space(4.0);
        theme::caption(ui, "Scene controls");
        card(ui, |ui| {
            if bindings.0.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new("Nothing to drive yet. Objects with moving parts (doors, gates, shutters) that stand inside a zone are opened and closed by OpenEMS.").color(theme::WEAK),
                    )
                    .wrap(),
                );
                return;
            }
            egui::Grid::new("ems_controls").num_columns(3).striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                ui.label(RichText::new("Object").small().color(theme::WEAK));
                ui.label(RichText::new("Zone").small().color(theme::WEAK));
                ui.label(RichText::new("State").small().color(theme::WEAK));
                ui.end_row();
                for (k, zone, name) in bindings.0.iter().take(12) {
                    let pct = state.actuators.get(*k as usize - 1).copied().flatten();
                    let (txt, col) = match pct {
                        Some(p) if p > 50.0 => (format!("open {p:.0}%"), theme::SUCCESS),
                        Some(p) => (format!("closed {p:.0}%"), theme::TEXT),
                        None => ("waiting".to_string(), theme::WEAK),
                    };
                    ui.label(name.to_string()).on_hover_text(format!("rtls0/Actuator{k}"));
                    ui.label(RichText::new(zone.to_string()).color(theme::WEAK));
                    ui.label(RichText::new(txt).color(col));
                    ui.end_row();
                }
            });
            ui.label(RichText::new("Each opens while its zone is occupied.").small().color(theme::WEAK));
        });
    }

    // ---- Details for whoever wires OpenEMS up
    ui.add_space(4.0);
    egui::CollapsingHeader::new(RichText::new("How the link works").color(theme::WEAK)).id_salt("ems_how").show(ui, |ui| {
        for line in [
            "Sense Sim writes the positioning values into the OpenEMS component rtls0 over REST (tags located, zone occupancy, alarms).",
            "OpenEMS drives objects through rtls0/Actuator1…n. Set rtls0/Automation = 0 to drive them yourself instead of the zone rule.",
            "Write rtls0/SimRunning = 0 to pause the simulation, 1 to resume.",
            "Address, password and the other outputs (MQTT, Modbus…): Positioning → Advanced → Outputs / EMS.",
        ] {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("•").color(theme::WEAK));
                ui.add(egui::Label::new(RichText::new(line).small()).wrap());
            });
        }
    });
    egui::CollapsingHeader::new(RichText::new("Troubleshooting").color(theme::WEAK)).id_salt("ems_trouble").show(ui, |ui| {
        if let Some(s) = &sink {
            ui.label(RichText::new(format!("Link: {:?} · {}", s.state, s.detail.chars().take(160).collect::<String>())).small().color(theme::WEAK));
        }
        if installed {
            ui.add(egui::Label::new(RichText::new("Starting keeps failing? Reinstall downloads Java and OpenEMS Edge again (~120 MB), then starts it.").small().color(theme::WEAK)).wrap());
            if ui.button("⬇ Reinstall OpenEMS").clicked() {
                let dir = super::openems_local::base_dir();
                let _ = std::fs::remove_file(dir.join("openems-edge.jar"));
                let _ = std::fs::remove_dir_all(dir.join("jre"));
                ems.stop();
                ems.start();
            }
        }
    });
}

/// Viewport overlay: device labels, the RTLS badge, placement hints, the card
/// next to the selected device, and the panel as a floating window while no
/// left-panel tab hosts it.
#[allow(clippy::too_many_arguments)]
pub fn rtls_ui(
    mut contexts: EguiContexts,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
    hide: Res<crate::ui::HideOverlays>,
    mut slot: ResMut<crate::ui::LeftPanelSlot>,
    mut panel: RtlsPanel,
    hist: Res<super::tag_card::TagHistory>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    // The assistant hides overlays while it captures the viewport.
    if hide.0 > 0 {
        return Ok(());
    }
    let (cam, cam_gt) = *camera;
    let scale = window.scale_factor();
    let vp = cam
        .viewport
        .as_ref()
        .map(|v| {
            egui::Rect::from_min_size(
                egui::pos2(v.physical_position.x as f32 / scale, v.physical_position.y as f32 / scale),
                vec2(v.physical_size.x as f32 / scale, v.physical_size.y as f32 / scale),
            )
        })
        .unwrap_or(ctx.content_rect());

    // ---- Labels over the viewport
    if panel.cfg.settings.show_labels {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("rtls_labels"))).with_clip_rect(vp);
        for (e, d, t) in panel.devices.iter() {
            let Ok(s) = cam.world_to_viewport(cam_gt, t.translation + Vec3::Y * 0.12) else { continue };
            let mut text = d.id.clone();
            if let Some(tr) = panel.state.tracks.get(&e) {
                if tr.lost {
                    text.push_str(" lost");
                } else if let Some(err) = tr.err.filter(|_| tr.tech != "rfid") {
                    text.push_str(&format!(" {:.0} cm", err * 100.0));
                }
            }
            let col = if d.enabled { c32(d.kind.color()) } else { theme::WEAK };
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(11.0), col);
            let r = egui::Rect::from_center_size(egui::pos2(s.x, s.y) - vec2(0.0, 8.0), galley.size() + vec2(8.0, 2.0));
            painter.rect_filled(r, 3.0, Color32::from_black_alpha(160));
            painter.galley(r.min + vec2(4.0, 1.0), galley, col);
        }
        for (_, z) in panel.zones.iter() {
            let c = (Vec3::from(z.min) + Vec3::from(z.max)) * 0.5;
            if let Ok(s) = cam.world_to_viewport(cam_gt, Vec3::new(c.x, z.max[1], c.z)) {
                let n = panel.state.zone_occupancy.get(&z.name).copied().unwrap_or(0);
                let col = if z.restricted { theme::DANGER } else { Color32::from_rgb(110, 190, 255) };
                painter.text(egui::pos2(s.x, s.y), egui::Align2::CENTER_BOTTOM, format!("{} · {n}", z.name), egui::FontId::proportional(12.0), col);
            }
        }
    }

    if panel.place.profile.is_some() || panel.place.waypoints_for.is_some() {
        let msg = if panel.place.waypoints_for.is_some() {
            "Click the floor to add waypoints · Enter / right-click / Esc to finish"
        } else {
            "Click to place · Shift keeps placing · right-click / Esc to stop"
        };
        egui::Area::new(egui::Id::new("rtls_hint")).order(egui::Order::Foreground).fixed_pos(vp.center_top() + vec2(-170.0, 56.0)).show(&ctx, |ui| {
            egui::Frame::new().fill(Color32::from_black_alpha(200)).corner_radius(6).inner_margin(8).show(ui, |ui| {
                ui.label(RichText::new(msg).color(theme::ACCENT));
            });
        });
    }

    // ---- Small card next to the selected device
    let ids: std::collections::HashMap<Entity, String> = panel.devices.iter().map(|(e, d, _)| (e, d.id.clone())).collect();
    let running = panel.cfg.settings.running;
    let dock_hidden = panel.ui_state.tag_dock_hidden;
    let mut show_dock = false;
    // A tag with its data panel open on the right shows only that panel, not this card too.
    if let Some(e) = panel.rsel.device
        && let Ok((_, mut d, t)) = panel.devices.get_mut(e)
        && !(d.kind.is_mobile() && dock_hidden != Some(e))
        && let Ok(s) = cam.world_to_viewport(cam_gt, t.translation)
        && vp.contains(egui::pos2(s.x, s.y))
    {
        let mut delete = false;
        let mut details = false;
        let track = panel.state.tracks.get(&e).cloned();
        let replies = panel.state.infra_links.get(&e).copied();
        egui::Area::new(egui::Id::new("rtls_device_card")).order(egui::Order::Foreground).fixed_pos(egui::pos2(s.x + 18.0, s.y - 10.0)).constrain(true).show(&ctx, |ui| {
            egui::Frame::new().fill(theme::PANEL).stroke(egui::Stroke::new(1.0, theme::STROKE)).corner_radius(8).inner_margin(8).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(c32(d.kind.color())));
                    ui.label(RichText::new(&d.id).strong());
                    ui.label(RichText::new(d.kind.label()).color(theme::WEAK));
                });
                if let Some(tr) = &track {
                    let txt = if tr.lost {
                        RichText::new("no position").color(theme::DANGER)
                    } else if tr.tech == "rfid" {
                        RichText::new(format!("{} reads", tr.reads)).color(theme::SUCCESS)
                    } else {
                        RichText::new(format!("error {:.0} cm · {} links", tr.err.unwrap_or(0.0) * 100.0, tr.used)).color(theme::SUCCESS)
                    };
                    ui.label(txt);
                } else if let Some(n) = replies {
                    ui.label(RichText::new(format!("{n} replies")).color(theme::WEAK));
                }
                if d.kind.is_mobile() && dock_hidden == Some(e) && ui.small_button("Show data & graphs ▸").clicked() {
                    show_dock = true;
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.enabled, "on");
                    if !d.kind.is_mobile() && ui.small_button("Details").clicked() {
                        details = true;
                    }
                    if ui.small_button(theme::icon::TRASH).on_hover_text("Delete (Del)").clicked() {
                        delete = true;
                    }
                });
                ui.label(RichText::new("drag onto racks, walls or ceiling · Shift+drag flat · Alt+drag height · Q/E rotate").small().color(theme::WEAK));
            });
        });
        if delete {
            panel.commands.entity(e).despawn();
            panel.rsel.device = None;
        }
        if details {
            let l = &mut panel.ui_state.local;
            l.advanced = true;
            l.tab = Tab::Inspector;
            slot.open_rtls = true;
        }
    }

    if show_dock {
        panel.ui_state.tag_dock_hidden = None;
    }

    // ---- Tag details docked on the right of the viewport (metrics, links, graphs).
    if let Some(e) = panel.rsel.device
        && panel.ui_state.tag_dock_hidden != Some(e)
        && let Ok((_, mut d, _)) = panel.devices.get_mut(e)
        && d.kind.is_mobile()
    {
        let title = d.id.clone();
        let kind = d.kind.label();
        let color = c32(d.kind.color());
        let track = panel.state.tracks.get(&e).cloned();
        let width = 380.0f32.min(vp.width() * 0.45);
        let rect = egui::Rect::from_min_max(egui::pos2(vp.right() - width - 8.0, vp.top() + 8.0), egui::pos2(vp.right() - 8.0, vp.bottom() - 8.0));
        let mut close = false;
        let mut delete = false;
        let mut enabled = d.enabled;
        egui::Area::new(egui::Id::new("rtls_tag_dock")).order(egui::Order::Foreground).fixed_pos(rect.min).show(&ctx, |ui| {
            egui::Frame::new().fill(theme::PANEL).stroke(egui::Stroke::new(1.0, theme::STROKE)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                ui.set_width(rect.width() - 20.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(color));
                    ui.label(RichText::new(&title).strong().size(15.0));
                    ui.label(RichText::new(kind).color(theme::WEAK));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(theme::icon::X).on_hover_text("Close (click the tag again or \"Show data & graphs\" to reopen)").clicked() {
                            close = true;
                        }
                        if ui.button(theme::icon::TRASH).on_hover_text("Delete (Del)").clicked() {
                            delete = true;
                        }
                        ui.checkbox(&mut enabled, "on");
                    });
                });
                ui.label(RichText::new("drag the tag onto racks, walls or ceiling · Shift+drag flat · Alt+drag height · Q/E rotate").small().color(theme::WEAK));
                ui.separator();
                egui::ScrollArea::vertical().id_salt("rtls_tag_dock_scroll").max_height(rect.height() - 64.0).auto_shrink([false, false]).show(ui, |ui| {
                    super::tag_card::details(ui, e, track.as_ref(), running, &ids, &hist);
                });
            });
        });
        if d.enabled != enabled {
            d.enabled = enabled;
        }
        if close {
            panel.ui_state.tag_dock_hidden = Some(e);
        }
        if delete {
            panel.commands.entity(e).despawn();
            panel.rsel.device = None;
        }
    }

    slot.rtls_alerts = panel.state.alarms.len() as u32;

    // ---- The panel itself, in the left-panel "Positioning" tab.
    if let Some(rect) = slot.rtls {
        // Draw on the panels' own (background) layer: a free-standing layer isn't
        // registered as an area, so egui never sees it as hovered and the scroll
        // area inside would ignore the mouse wheel.
        let layer = egui::LayerId::background();
        let mut ui = egui::Ui::new(ctx.clone(), egui::Id::new("rtls_panel"), egui::UiBuilder::new().layer_id(layer).max_rect(rect));
        ui.set_clip_rect(rect);
        theme::panel_header(&mut ui, "Positioning", |_| {});
        panel.show(&mut ui);
    }
    Ok(())
}

fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").args(["/C", "start", "", url]).creation_flags(0x0800_0000).spawn().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ())
    }
}

#[allow(clippy::too_many_arguments)]
fn devices_tab(
    ui: &mut egui::Ui,
    local: &mut UiLocal,
    place: &mut DevicePlace,
    rsel: &mut RtlsSelection,
    profiles: &Profiles,
    devices: &mut DeviceQuery<'_, '_>,
    zones: &mut Query<(Entity, &mut Zone)>,
    commands: &mut Commands,
    state: &RtlsState,
    orbit: &mut OrbitCamera,
) {
    ui.label(RichText::new("Place a device (click in the scene)").strong());
    for kind in DeviceKind::ALL {
        let list: Vec<_> = profiles.0.iter().filter(|p| p.kind == kind).collect();
        if list.is_empty() {
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("●").color(c32(kind.color())));
            for p in list {
                let active = place.profile.as_deref() == Some(p.id.as_str());
                let b = egui::Button::new(&p.name).selected(active);
                if ui.add(b).on_hover_text(format!("{} · {} · id {}", kind.label(), p.vendor, p.id)).clicked() {
                    place.profile = if active { None } else { Some(p.id.clone()) };
                    place.waypoints_for = None;
                }
            }
        });
    }
    ui.checkbox(&mut place.sticky, "Keep placing after each click");
    ui.label(
        RichText::new(format!("Extra vendor profiles (e.g. Nextwaves models) are read from {}", Profiles::user_dirs().last().map(|p| p.display().to_string()).unwrap_or_default()))
            .small()
            .color(theme::WEAK),
    );
    ui.separator();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Zones").strong());
        ui.text_edit_singleline(&mut local.new_zone);
        if ui.button(theme::label(theme::icon::PLUS, "Add at view centre")).clicked() {
            let c = orbit.focus;
            let z = Zone { name: local.new_zone.clone(), min: [c.x - 2.0, 0.0, c.z - 2.0], max: [c.x + 2.0, 2.5, c.z + 2.0], restricted: false, capacity: 0, auto: false };
            let e = commands.spawn((Name::new(format!("Zone {}", z.name)), z)).id();
            rsel.zone = Some(e);
            rsel.device = None;
            let n = zones.iter().count() + 2;
            local.new_zone = format!("Zone {n}");
            local.tab = Tab::Inspector;
        }
    });
    let mut zl: Vec<(Entity, String, bool)> = zones.iter().map(|(e, z)| (e, z.name.clone(), z.restricted)).collect();
    zl.sort_by(|a, b| a.1.cmp(&b.1));
    for (e, name, restricted) in zl {
        ui.horizontal(|ui| {
            let n = state.zone_occupancy.get(&name).copied().unwrap_or(0);
            let txt = format!("{}{name} · {n} inside", if restricted { "⛔ " } else { "" });
            if ui.selectable_label(rsel.zone == Some(e), txt).clicked() {
                rsel.zone = Some(e);
                rsel.device = None;
                local.tab = Tab::Inspector;
            }
        });
    }
    ui.separator();

    ui.label(RichText::new(format!("Devices ({})", devices.iter().count())).strong());
    let mut list: Vec<(Entity, String, DeviceKind, bool, Vec3)> = devices.iter().map(|(e, d, t)| (e, d.id.clone(), d.kind, d.enabled, t.translation)).collect();
    list.sort_by(|a, b| (a.2 as u8, &a.1).cmp(&(b.2 as u8, &b.1)));
    egui::Grid::new("rtls_dev_list").striped(true).num_columns(4).show(ui, |ui| {
        for (e, id, kind, enabled, pos) in list {
            if ui.selectable_label(rsel.device == Some(e), RichText::new(&id).color(c32(kind.color()))).clicked() {
                rsel.device = Some(e);
                rsel.zone = None;
                orbit.focus = pos;
                local.tab = Tab::Inspector;
            }
            ui.label(RichText::new(kind.label()).color(theme::WEAK));
            let status = match state.tracks.get(&e) {
                Some(t) if t.lost => RichText::new("lost").color(theme::DANGER),
                Some(t) => match t.err {
                    Some(err) if t.tech != "rfid" => RichText::new(format!("{:.0} cm", err * 100.0)).color(theme::SUCCESS),
                    _ => RichText::new(format!("{} reads", t.reads)).color(theme::SUCCESS),
                },
                None if !enabled => RichText::new("off").color(theme::WEAK),
                None => RichText::new(format!("{:.1}, {:.1}, {:.1}", pos.x, pos.y, pos.z)).color(theme::WEAK),
            };
            ui.label(status);
            let mut en = enabled;
            if ui.checkbox(&mut en, "").on_hover_text("Enabled (off = simulate a failed / offline device)").changed()
                && let Ok((_, mut d, _)) = devices.get_mut(e)
            {
                d.enabled = en;
            }
            ui.end_row();
        }
    });
}

#[allow(clippy::too_many_arguments)]
/// Antenna of one anchor / beacon / tag: default, omni, directional (aimed
/// where the device faces) or a pattern from the antenna library.
fn antenna_edit(
    ui: &mut egui::Ui,
    choice: &mut Option<super::antenna::AntennaChoice>,
    prof: &super::device::Profile,
    library: &super::antenna::AntennaLibrary,
    kind: DeviceKind,
    commands: &mut Commands,
) {
    use super::antenna::AntennaChoice as A;
    ui.horizontal(|ui| {
        ui.label("Antenna");
        let current = choice.as_ref().map_or_else(|| "Default".to_string(), A::label);
        egui::ComboBox::from_id_salt("rtls_antenna").selected_text(current).width(200.0).show_ui(ui, |ui| {
            if ui.selectable_label(choice.is_none(), "Default").on_hover_text("The profile's antenna, or the RF-solver antenna applied to all of them").clicked() {
                *choice = None;
            }
            let omni = matches!(choice, Some(A::Omni { .. }));
            if ui.selectable_label(omni, "Omni").clicked() && !omni {
                *choice = Some(A::Omni { gain_dbi: prof.antenna_gain_dbi.max(0.0) });
            }
            let directional = matches!(choice, Some(A::Directional { .. }));
            if ui.selectable_label(directional, "Directional").clicked() && !directional {
                *choice = Some(A::Directional { gain_dbi: 6.0, beam_deg: 90.0 });
            }
            if !library.0.is_empty() {
                ui.separator();
            }
            for a in &library.0 {
                let sel = matches!(choice, Some(A::Library { name }) if *name == a.label);
                if ui.selectable_label(sel, &a.label).on_hover_text(format!("Simulated in the RF solver ({})", a.tech.label())).clicked() {
                    *choice = Some(A::Library { name: a.label.clone() });
                }
            }
        });
    });
    match choice {
        Some(A::Omni { gain_dbi }) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("gain").color(theme::WEAK));
                ui.add(egui::DragValue::new(gain_dbi).range(-10.0..=15.0).speed(0.1).suffix(" dBi"));
            });
        }
        Some(A::Directional { gain_dbi, beam_deg }) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("gain").color(theme::WEAK));
                ui.add(egui::DragValue::new(gain_dbi).range(0.0..=20.0).speed(0.1).suffix(" dBi"));
                ui.label(RichText::new("beam").color(theme::WEAK));
                ui.add(egui::DragValue::new(beam_deg).range(10.0..=180.0).speed(1.0).suffix("°"))
                    .on_hover_text("Half-power beam width");
            });
            ui.label(RichText::new("Aimed where the device faces (rotate it in the viewport).").small().color(theme::WEAK));
        }
        Some(A::Library { name }) if library.get(name).is_none() => {
            ui.label(RichText::new(format!("\"{name}\" is not in the antenna library: treated as 0 dBi.")).small().color(theme::ACCENT));
        }
        _ => {}
    }
    let all = format!("{}s", kind.label().to_lowercase());
    if ui.small_button(format!("Use on all {all}")).on_hover_text("Give every device of this kind this antenna").clicked() {
        let c = choice.clone();
        commands.queue(move |w: &mut World| {
            let mut q = w.query::<&mut Device>();
            for mut d in q.iter_mut(w) {
                if d.kind == kind {
                    d.radio.antenna = c.clone();
                }
            }
        });
    }
}

fn inspector_tab(
    ui: &mut egui::Ui,
    place: &mut DevicePlace,
    rsel: &mut RtlsSelection,
    profiles: &Profiles,
    library: &super::antenna::AntennaLibrary,
    devices: &mut DeviceQuery<'_, '_>,
    zones: &mut Query<(Entity, &mut Zone)>,
    objects: &[String],
    state: &RtlsState,
    commands: &mut Commands,
) {
    if let Some(ze) = rsel.zone
        && let Ok((_, mut z)) = zones.get_mut(ze)
    {
        ui.label(RichText::new("Zone").strong());
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut z.name);
        });
        ui.label("Min");
        arr_edit(ui, &mut z.min);
        ui.label("Max");
        arr_edit(ui, &mut z.max);
        ui.checkbox(&mut z.restricted, "Restricted (alarm on entry)");
        ui.horizontal(|ui| {
            ui.label("Capacity (0 = none)");
            ui.add(egui::DragValue::new(&mut z.capacity).range(0..=10_000));
        });
        let inside = state.zone_occupancy.get(&z.name).copied().unwrap_or(0);
        ui.label(format!("Tags inside now: {inside}"));
        if ui.button("Delete zone").clicked() {
            commands.entity(ze).despawn();
            rsel.zone = None;
        }
        return;
    }
    let Some(e) = rsel.device else {
        ui.label(RichText::new("Select a device in the viewport or the Devices tab.").color(theme::WEAK));
        return;
    };
    let ids: Vec<String> = devices.iter().filter(|(x, _, _)| *x != e).map(|(_, d, _)| d.id.clone()).collect();
    let Ok((_, mut d, mut t)) = devices.get_mut(e) else { return };
    let kind = d.kind;
    let prof = profiles.resolve(&d.profile, kind);
    ui.horizontal(|ui| {
        ui.label(RichText::new("●").color(c32(kind.color())));
        ui.label(RichText::new(kind.label()).strong());
        ui.checkbox(&mut d.enabled, "enabled");
    });
    ui.horizontal(|ui| {
        ui.label("Id");
        let mut id = d.id.clone();
        if ui.text_edit_singleline(&mut id).changed() && !id.trim().is_empty() && !ids.contains(&id) {
            d.id = id;
        }
    });
    ui.horizontal(|ui| {
        ui.label("Profile");
        egui::ComboBox::from_id_salt("rtls_prof").selected_text(prof.name.clone()).show_ui(ui, |ui| {
            for p in profiles.0.iter().filter(|p| p.kind == kind) {
                if ui.selectable_label(p.id == d.profile, &p.name).clicked() {
                    d.profile = p.id.clone();
                }
            }
        });
    });
    if kind == DeviceKind::UwbAnchor {
        // This anchor's own transmit power (the floor map, optimiser and simulation follow it).
        ui.horizontal(|ui| {
            let mut own = d.radio.uwb_tx_dbm_mhz.is_some();
            if ui.checkbox(&mut own, "TX power").on_hover_text("Off = the profile's power. On = this anchor only.").changed() {
                d.radio.uwb_tx_dbm_mhz = own.then_some(prof.uwb_tx_dbm_mhz);
            }
            let mut tx = d.radio.uwb_tx_dbm_mhz.unwrap_or(prof.uwb_tx_dbm_mhz);
            let r = ui.add_enabled(own, egui::DragValue::new(&mut tx).range(-80.0..=super::radio::UWB_MAX_DBM_MHZ).speed(0.5).suffix(" dBm/MHz"));
            if r.changed() {
                d.radio.uwb_tx_dbm_mhz = Some(tx);
            }
            let range = super::radio::uwb_link_range_m(&super::device::Profile { uwb_tx_dbm_mhz: tx, ..prof.clone() });
            ui.label(RichText::new(format!("≈ {range:.0} m LOS")).color(theme::WEAK));
        });
    }
    if matches!(kind, DeviceKind::UwbAnchor | DeviceKind::UwbTag | DeviceKind::BleNode | DeviceKind::BleTag) {
        antenna_edit(ui, &mut d.radio.antenna, &prof, library, kind, commands);
    }
    ui.horizontal(|ui| {
        ui.label(if kind == DeviceKind::RfidTag { "EPC" } else { "Address" });
        ui.text_edit_singleline(&mut d.address);
        if kind == DeviceKind::RfidTag && ui.small_button("gen").clicked() {
            d.address = epc_for(&d.id);
        }
    });
    ui.label("Position");
    let mut p = t.translation;
    if vec3_edit(ui, &mut p, 0.02) {
        t.translation = p;
    }
    ui.horizontal(|ui| {
        let (mut yaw, _, _) = t.rotation.to_euler(EulerRot::YXZ);
        let mut deg = yaw.to_degrees();
        ui.label("Yaw");
        if ui.add(egui::DragValue::new(&mut deg).speed(1.0).suffix("°")).changed() {
            yaw = deg.to_radians();
            t.rotation = Quat::from_rotation_y(yaw);
        }
        if !kind.is_mobile() && ui.button("Height = profile").on_hover_text(format!("{:.1} m", prof.mount_height_m)).clicked() {
            t.translation.y = prof.mount_height_m;
        }
    });
    ui.label(
        RichText::new("Drag in the viewport to move · Alt+drag changes height · Q/E rotate · G look at · Del deletes · Ctrl+D duplicates").small().color(theme::WEAK),
    );

    if kind.is_mobile() {
        ui.separator();
        ui.label(RichText::new("Motion").strong());
        let current = d.motion.label();
        let here = t.translation;
        egui::ComboBox::from_id_salt("rtls_motion").selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(matches!(d.motion, Motion::Static), "Static").clicked() {
                d.motion = Motion::Static;
            }
            if ui.selectable_label(matches!(d.motion, Motion::Path { .. }), "Path").clicked() && !matches!(d.motion, Motion::Path { .. }) {
                d.motion = Motion::Path { points: vec![here.to_array()], speed: 1.2, ping_pong: false };
            }
            if ui.selectable_label(matches!(d.motion, Motion::Wander { .. }), "Wander").clicked() && !matches!(d.motion, Motion::Wander { .. }) {
                d.motion = Motion::Wander { center: here.to_array(), radius: 4.0, speed: 1.0 };
            }
            if ui.selectable_label(matches!(d.motion, Motion::Follow { .. }), "Follow object").clicked() && !matches!(d.motion, Motion::Follow { .. }) {
                d.motion = Motion::Follow { object: objects.first().cloned().unwrap_or_default(), offset: [0.0, 1.5, 0.0] };
            }
        });
        let tag_y = t.translation.y;
        match &mut d.motion {
            Motion::Static => {}
            Motion::Path { points, speed, ping_pong } => {
                ui.horizontal(|ui| {
                    ui.label("Speed");
                    ui.add(egui::DragValue::new(speed).speed(0.05).range(0.0..=30.0).suffix(" m/s"));
                    ui.checkbox(ping_pong, "ping-pong");
                });
                ui.horizontal(|ui| {
                    let adding = place.waypoints_for == Some(e);
                    if ui.add(egui::Button::new(if adding { "✔ Done adding".to_string() } else { theme::label(theme::icon::PLUS, "Add waypoints by clicking") }).selected(adding)).clicked() {
                        place.waypoints_for = if adding { None } else { Some(e) };
                        place.profile = None;
                    }
                    if ui.button("Clear").clicked() {
                        points.clear();
                    }
                });
                let mut remove = None;
                for (k, pt) in points.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{k}"));
                        ui.add(egui::DragValue::new(&mut pt[0]).speed(0.05).prefix("x ").max_decimals(2));
                        ui.add(egui::DragValue::new(&mut pt[2]).speed(0.05).prefix("z ").max_decimals(2));
                        pt[1] = tag_y;
                        if ui.small_button("✕").clicked() {
                            remove = Some(k);
                        }
                    });
                }
                if let Some(k) = remove {
                    points.remove(k);
                }
            }
            Motion::Wander { center, radius, speed } => {
                ui.label("Centre");
                arr_edit(ui, center);
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(radius).speed(0.1).range(0.0..=200.0).prefix("radius ").suffix(" m"));
                    ui.add(egui::DragValue::new(speed).speed(0.05).range(0.0..=30.0).prefix("speed ").suffix(" m/s"));
                });
            }
            Motion::Follow { object, offset } => {
                egui::ComboBox::from_id_salt("rtls_follow").selected_text(object.clone()).show_ui(ui, |ui| {
                    let names: Vec<String> = objects.to_vec();
                    let _ = &names;
                    for n in names {
                        if ui.selectable_label(*object == n, &n).clicked() {
                            *object = n;
                        }
                    }
                });
                ui.label("Offset on the object");
                arr_edit(ui, offset);
            }
        }
    }

    // Live link table.
    if let Some(tr) = state.tracks.get(&e) {
        ui.separator();
        ui.label(RichText::new("Live").strong());
        egui::Grid::new("rtls_live_tag").num_columns(2).show(ui, |ui| {
            ui.label("Truth");
            ui.label(format!("{:.2}, {:.2}, {:.2}", tr.truth.x, tr.truth.y, tr.truth.z));
            ui.end_row();
            ui.label("Estimate");
            ui.label(tr.est.map(|p| format!("{:.2}, {:.2}, {:.2}", p.x, p.y, p.z)).unwrap_or("—".into()));
            ui.end_row();
            ui.label("Error");
            ui.label(match (tr.err, tr.tech) {
                (_, "rfid") => "zone-level (RFID)".into(),
                (Some(e), _) => format!("{:.1} cm", e * 100.0),
                _ => "—".into(),
            });
            ui.end_row();
            ui.label("DOP / residual");
            ui.label(format!("{:.2} / {:.2} m", tr.dop, tr.residual));
            ui.end_row();
            ui.label("Fixes");
            ui.label(format!("{} of {} updates ({} RFID reads)", tr.fixes, tr.attempts, tr.reads));
            ui.end_row();
            ui.label("State");
            ui.label(if tr.lost { RichText::new("LOST").color(theme::DANGER) } else { RichText::new("tracking").color(theme::SUCCESS) });
            ui.end_row();
        });
        let names: std::collections::HashMap<Entity, String> = devices.iter().map(|(e, d, _)| (e, d.id.clone())).collect();
        egui::Grid::new("rtls_links").striped(true).num_columns(5).show(ui, |ui| {
            for h in ["link", "true d", "measured", "path", "used"] {
                ui.label(RichText::new(h).color(theme::WEAK));
            }
            ui.end_row();
            for l in &tr.links {
                ui.label(names.get(&l.other).cloned().unwrap_or_default());
                ui.label(format!("{:.2} m", l.true_distance));
                ui.label(match (l.kind, l.value) {
                    (_, None) => "—".into(),
                    (LinkKind::Range, Some(v)) => format!("{v:.2} m"),
                    (LinkKind::Tdoa, Some(v)) => format!("Δ {v:+.2} m"),
                    (_, Some(v)) => format!("{v:.0} dBm"),
                });
                ui.label(RichText::new(l.path_label()).color(if l.walls == 0 { theme::SUCCESS } else { theme::DANGER }));
                ui.label(if l.used { "✔" } else { "" });
                ui.end_row();
            }
        });
    }
    ui.separator();
    if ui.button("Delete device").clicked() {
        commands.entity(e).despawn();
        rsel.device = None;
    }
}

fn live_tab(ui: &mut egui::Ui, state: &mut RtlsState, hub: &Hub, devices: &DeviceQuery<'_, '_>) {
    let (rmse, c50, c95, max) = state.error_stats(None);
    let snap = state.snapshot.clone();
    egui::Grid::new("rtls_metrics").num_columns(4).spacing(vec2(14.0, 4.0)).show(ui, |ui| {
        let kv = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(RichText::new(k).color(theme::WEAK));
            ui.label(RichText::new(v).strong());
        };
        kv(ui, "Tags located", format!("{} / {}", snap.tags_located, snap.tags_total));
        kv(ui, "Lost", format!("{}", snap.tags_lost));
        ui.end_row();
        kv(ui, "RMSE (30 s)", format!("{:.1} cm", rmse * 100.0));
        kv(ui, "CEP50 / 95", format!("{:.0} / {:.0} cm", c50 * 100.0, c95 * 100.0));
        ui.end_row();
        kv(ui, "Max error", format!("{:.0} cm", max * 100.0));
        kv(ui, "RFID reads/s", format!("{:.0}", state.rfid_rate()));
        ui.end_row();
        kv(ui, "Infra online", format!("{} / {}", snap.infra_online, snap.infra_total));
        kv(ui, "Events out", format!("{} ({} published)", state.events_total, hub.published));
        ui.end_row();
    });
    for tech in ["uwb", "ble"] {
        let (r, _, c95, _) = state.error_stats(Some(tech));
        if r > 0.0 {
            ui.label(RichText::new(format!("{}: RMSE {:.1} cm · CEP95 {:.1} cm", tech.to_uppercase(), r * 100.0, c95 * 100.0)).color(theme::WEAK));
        }
    }
    if ui.button("Reset statistics").clicked() {
        state.reset_metrics();
    }
    let rm: Vec<(f64, f32)> = state.series.iter().map(|p| (p.t, p.rmse * 100.0)).collect();
    chart(ui, &rm, Color32::from_rgb(90, 210, 255), "RMSE", " cm");
    let lo: Vec<(f64, f32)> = state.series.iter().map(|p| (p.t, p.located as f32)).collect();
    chart(ui, &lo, theme::SUCCESS, "Tags located", "");
    let rd: Vec<(f64, f32)> = state.series.iter().map(|p| (p.t, p.reads)).collect();
    if rd.iter().any(|p| p.1 > 0.0) {
        chart(ui, &rd, Color32::from_rgb(140, 230, 120), "RFID reads/s", "");
    }

    if !state.alarms.is_empty() {
        ui.separator();
        ui.label(RichText::new(format!("Active alarms ({})", state.alarms.len())).strong().color(theme::DANGER));
        for a in state.alarms.values() {
            ui.label(format!("{} [{:.1}s] {}", theme::icon::WARNING_CIRCLE, a.since, a.message));
        }
    }
    ui.separator();
    ui.label(RichText::new("Tags").strong());
    let kinds: std::collections::HashMap<String, DeviceKind> = devices.iter().map(|(_, d, _)| (d.id.clone(), d.kind)).collect();
    egui::Grid::new("rtls_tag_table").striped(true).num_columns(6).show(ui, |ui| {
        for h in ["tag", "tech", "x", "z", "err", "q"] {
            ui.label(RichText::new(h).color(theme::WEAK));
        }
        ui.end_row();
        for t in &snap.tags {
            let col = kinds.get(&t.id).map(|k| c32(k.color())).unwrap_or(theme::TEXT);
            ui.label(RichText::new(&t.id).color(col));
            ui.label(&t.tech);
            ui.label(format!("{:.2}", t.x));
            ui.label(format!("{:.2}", t.z));
            let err = state.tracks.values().find(|x| x.id == t.id).and_then(|x| x.err);
            ui.label(if t.lost { RichText::new("lost").color(theme::DANGER) } else { RichText::new(err.map(|e| format!("{:.0} cm", e * 100.0)).unwrap_or_default()) });
            ui.label(format!("{:.0}%", t.quality * 100.0));
            ui.end_row();
        }
    });
    if !snap.zones.is_empty() {
        ui.separator();
        ui.label(RichText::new("Zone occupancy").strong());
        for (z, n) in &snap.zones {
            ui.label(format!("{z}: {n}"));
        }
    }
    ui.separator();
    ui.label(RichText::new("Event log").strong());
    for (t, msg) in state.log.iter().rev().take(40) {
        ui.label(RichText::new(format!("[{t:7.1}s] {msg}")).monospace().small());
    }
}

fn planning_tab(
    ui: &mut egui::Ui,
    local: &mut UiLocal,
    cov: &mut Coverage,
    jobs: &mut PlanJobs,
    profiles: &Profiles,
    commands: &mut Commands,
    devices: &DeviceQuery<'_, '_>,
) {
    ui.horizontal(|ui| {
        ui.label("Technology");
        for t in Tech::ALL {
            ui.selectable_value(&mut local.tech, t, t.label());
        }
    });
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut local.height).speed(0.05).range(0.0..=10.0).prefix("tag height ").suffix(" m"));
        ui.add(egui::DragValue::new(&mut local.cell).speed(0.05).range(0.1..=5.0).prefix("cell ").suffix(" m"));
    });
    ui.horizontal(|ui| {
        if ui.button("Compute coverage heatmap").clicked() {
            jobs.coverage = Some(CoverageRequest { tech: local.tech, height: local.height, cell: local.cell });
        }
        ui.checkbox(&mut cov.visible, "show");
    });
    if !cov.summary.is_empty() {
        ui.label(&cov.summary);
        let legend = match (cov.tech, cov.view) {
            (Tech::Rfid, _) => "blue = readable (brighter = more link margin) · grey = no read".to_string(),
            (_, MapView::Quality) => format!(
                "green = ≥ {:.0}% of readings within {:.0} cm ({:.0}% of the floor) · red = less · grey = no fix",
                cov.quality.min_share * 100.0,
                cov.quality.target_m * 100.0,
                cov.quality_share() * 100.0
            ),
            (_, MapView::Accuracy) => "green ≤ 10 cm · yellow ≈ 30 cm · red ≥ 1 m predicted error · grey = no fix".to_string(),
            (t, MapView::Signal) => {
                let (lo, hi) = super::planning::signal_scale(t);
                format!("red {lo:.0} dBm (weak) → green {hi:.0} dBm (strong) · grey = nothing heard")
            }
        };
        ui.label(RichText::new(legend).small().color(theme::WEAK));
    }
    ui.separator();
    ui.label(RichText::new("Placement optimiser").strong());
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut local.add).range(1..=32).prefix("add "));
        ui.label(local.tech.infra().label());
        if ui.button("Suggest positions").clicked() {
            jobs.optimize = Some(OptimizeRequest { tech: local.tech, add: local.add, height: local.height, ..Default::default() });
        }
    });
    if !cov.suggestion_note.is_empty() {
        ui.label(&cov.suggestion_note);
    }
    if !cov.suggestions.is_empty() {
        for p in &cov.suggestions {
            ui.label(RichText::new(format!("  {:.1}, {:.1}, {:.1}", p.x, p.y, p.z)).monospace());
        }
        ui.horizontal(|ui| {
            if ui.button("Place all suggested").clicked()
                && let Some(prof) = profiles.default_for(local.tech.infra())
            {
                let mut ids: Vec<String> = devices.iter().map(|(_, d, _)| d.id.clone()).collect();
                for p in std::mem::take(&mut cov.suggestions) {
                    let id = super::device::next_id(prof.kind, ids.iter().map(String::as_str));
                    ids.push(id.clone());
                    super::view::spawn_device(
                        commands,
                        Device { id, kind: prof.kind, profile: prof.id.clone(), enabled: true, motion: Motion::Static, address: String::new(), auto: false, radio: Default::default() },
                        Transform::from_translation(p),
                    );
                }
                jobs.coverage = Some(CoverageRequest { tech: local.tech, height: local.height, cell: local.cell });
            }
            if ui.button("Discard").clicked() {
                cov.suggestions.clear();
                cov.suggestion_note.clear();
            }
        });
    }
}

fn outputs_tab(ui: &mut egui::Ui, local: &mut UiLocal, cfg: &mut RtlsConfig, hub: &mut Hub) {
    let draft = local.draft.get_or_insert_with(|| cfg.outputs.clone());
    ui.label(RichText::new("Stream the simulation in real time. Edit, then Apply & connect.").color(theme::WEAK));

    // Status of running sinks.
    for s in &hub.sinks {
        let st = s.shared.status();
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").color(state_color(st.state)));
            ui.label(RichText::new(s.name).strong());
            ui.label(RichText::new(&st.detail).small());
        });
        ui.label(
            RichText::new(format!(
                "   sent {} · dropped {}{}",
                st.sent,
                st.dropped,
                st.rtt_ms.map(|r| format!(" · {r:.0} ms")).unwrap_or_default()
            ))
            .small()
            .color(theme::WEAK),
        );
    }
    if hub.sinks.is_empty() {
        ui.label(RichText::new("No outputs running.").color(theme::WEAK));
    }
    let fb = hub.feedback();
    if !fb.is_empty() {
        ui.label(RichText::new("Read back from the EMS / broker").strong());
        egui::Grid::new("rtls_feedback").striped(true).num_columns(3).show(ui, |ui| {
            for (k, v, age) in fb.iter().take(40) {
                ui.label(RichText::new(k).small());
                ui.label(RichText::new(v.chars().take(80).collect::<String>()).monospace().small());
                ui.label(RichText::new(format!("{age:.0}s")).small().color(theme::WEAK));
                ui.end_row();
            }
        });
    }
    ui.separator();

    egui::CollapsingHeader::new(RichText::new("OpenEMS Edge (REST read/write)").strong()).default_open(true).show(ui, |ui| {
        let c = &mut draft.openems;
        ui.checkbox(&mut c.enabled, "enabled");
        ui.horizontal(|ui| {
            ui.label("URL");
            ui.text_edit_singleline(&mut c.url);
        });
        ui.horizontal(|ui| {
            ui.label("User");
            ui.add(egui::TextEdit::singleline(&mut c.username).desired_width(60.0));
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut c.password).password(true).desired_width(100.0));
            ui.add(egui::DragValue::new(&mut c.period_s).speed(0.1).range(0.1..=60.0).prefix("every ").suffix(" s"));
        });
        ui.label(RichText::new("Write: metric → Component/Channel (needs Controller.Api.Rest.ReadWrite and writable channels)").small().color(theme::WEAK));
        let mut remove = None;
        for (k, m) in c.write.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut m.metric).desired_width(120.0));
                ui.label("→");
                ui.add(egui::TextEdit::singleline(&mut m.address).desired_width(170.0));
                if ui.small_button("✕").clicked() {
                    remove = Some(k);
                }
            });
        }
        if let Some(k) = remove {
            c.write.remove(k);
        }
        if ui.small_button(theme::label(theme::icon::PLUS, "mapping")).clicked() {
            c.write.push(super::io::ChannelMap { metric: "zone:Zone 1".into(), address: "rtls0/Zone1Occupancy".into() });
        }
        ui.label(RichText::new("Metrics: tags_total, tags_located, tags_lost, infra_online, alarms_active, rmse_cm, cep95_cm, rfid_reads_per_s, running, zone:<name>, tag:<id>:x|y|z|lost").small().color(theme::WEAK));
        ui.label("Read back (one Component/Channel per line)");
        let mut text = c.read.join("\n");
        if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3)).changed() {
            c.read = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        }
    });
    egui::CollapsingHeader::new(RichText::new("MQTT").strong()).default_open(true).show(ui, |ui| {
        let c = &mut draft.mqtt;
        ui.checkbox(&mut c.enabled, "enabled");
        ui.horizontal(|ui| {
            ui.label("Broker");
            ui.add(egui::TextEdit::singleline(&mut c.host).desired_width(140.0));
            ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
            ui.label("QoS");
            ui.add(egui::DragValue::new(&mut c.qos).range(0..=1));
            ui.checkbox(&mut c.retain, "retain state");
        });
        ui.horizontal(|ui| {
            ui.label("User");
            ui.add(egui::TextEdit::singleline(&mut c.username).desired_width(90.0));
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut c.password).password(true).desired_width(90.0));
        });
        ui.horizontal(|ui| {
            ui.label("Topic prefix");
            ui.text_edit_singleline(&mut c.prefix);
        });
        ui.horizontal(|ui| {
            ui.label("Also listen to");
            ui.text_edit_singleline(&mut c.subscribe);
        });
        ui.label(RichText::new(format!("Control topic: {}/control  ({{\"cmd\":\"pause|run|reset\"}}, {{\"cmd\":\"speed\",\"value\":2}})", c.prefix)).small().color(theme::WEAK));
        filter_ui(ui, &mut c.filter);
    });
    egui::CollapsingHeader::new(RichText::new("Modbus/TCP server").strong()).show(ui, |ui| {
        let c = &mut draft.modbus;
        ui.checkbox(&mut c.enabled, "enabled");
        ui.horizontal(|ui| {
            ui.label("Listen on");
            ui.text_edit_singleline(&mut c.bind);
        });
        ui.label(
            RichText::new("Regs: 0 time · 1 tags · 2 located · 3 lost · 4 infra online · 6 alarms · 7 RMSE cm · 9 RFID reads/s · 20+ zones · 100+10k tag k (x,y,z mm i32, q%, age, flags) · write 1000 run, 1001 speed×10, 1002 reset")
                .small()
                .color(theme::WEAK),
        );
    });
    egui::CollapsingHeader::new(RichText::new("HTTP webhook").strong()).show(ui, |ui| {
        let c = &mut draft.webhook;
        ui.checkbox(&mut c.enabled, "enabled");
        ui.horizontal(|ui| {
            ui.label("URL");
            ui.text_edit_singleline(&mut c.url);
        });
        ui.horizontal(|ui| {
            ui.label("Batch");
            ui.add(egui::DragValue::new(&mut c.batch_ms).range(10..=60_000).suffix(" ms"));
        });
        ui.label("Headers (Name: value per line)");
        ui.add(egui::TextEdit::multiline(&mut c.headers).desired_rows(2));
        filter_ui(ui, &mut c.filter);
    });
    egui::CollapsingHeader::new(RichText::new("UDP stream").strong()).show(ui, |ui| {
        let c = &mut draft.udp;
        ui.checkbox(&mut c.enabled, "enabled");
        ui.horizontal(|ui| {
            ui.label("Target");
            ui.text_edit_singleline(&mut c.target);
            ui.selectable_value(&mut c.format, UdpFormat::Json, "JSON");
            ui.selectable_value(&mut c.format, UdpFormat::Csv, "CSV");
        });
        filter_ui(ui, &mut c.filter);
    });
    egui::CollapsingHeader::new(RichText::new("Recorder (JSONL)").strong()).show(ui, |ui| {
        let c = &mut draft.recorder;
        ui.checkbox(&mut c.enabled, "enabled");
        ui.horizontal(|ui| {
            ui.label("File");
            ui.text_edit_singleline(&mut c.path);
            if ui.small_button("…").clicked()
                && let Some(p) = rfd::FileDialog::new().add_filter("JSON lines", &["jsonl"]).set_file_name("rtls-recording.jsonl").save_file()
            {
                c.path = p.to_string_lossy().replace('\\', "/");
            }
        });
        filter_ui(ui, &mut c.filter);
    });
    ui.separator();
    let changed = *draft != cfg.outputs;
    ui.horizontal(|ui| {
        if ui.add_enabled(true, egui::Button::new(if changed { "Apply & connect" } else { "Reconnect" })).clicked() {
            cfg.outputs = draft.clone();
            cfg.save();
            hub.restart(&cfg.outputs);
        }
        if changed && ui.button("Revert").clicked() {
            *draft = cfg.outputs.clone();
        }
        if ui.button("Stop all").clicked() {
            draft.mqtt.enabled = false;
            draft.openems.enabled = false;
            draft.modbus.enabled = false;
            draft.webhook.enabled = false;
            draft.udp.enabled = false;
            draft.recorder.enabled = false;
            cfg.outputs = draft.clone();
            cfg.save();
            hub.apply(&cfg.outputs);
        }
    });
    let sample = RtlsEvent::Position {
        ts: 12.5,
        tag: "T1".into(),
        tech: "uwb".into(),
        x: 3.21,
        y: 1.2,
        z: 4.56,
        quality: 0.92,
        anchors: 4,
        dop: 1.1,
        error_m: Some(0.07),
        truth: Some([3.18, 1.2, 4.6]),
    };
    egui::CollapsingHeader::new("Payload example").show(ui, |ui| {
        ui.label(RichText::new(format!("{}/{}", cfg.outputs.mqtt.prefix, sample.topic())).monospace().small());
        ui.label(RichText::new(serde_json::to_string_pretty(&sample).unwrap_or_default()).monospace().small());
    });
}

fn settings_tab(ui: &mut egui::Ui, cfg: &mut RtlsConfig) {
    let before = cfg.settings.clone();
    let s = &mut cfg.settings;
    egui::Grid::new("rtls_settings").num_columns(2).show(ui, |ui| {
        ui.label("Tick rate");
        ui.add(egui::DragValue::new(&mut s.tick_hz).range(1.0..=200.0).suffix(" Hz"));
        ui.end_row();
        ui.label("Random seed");
        ui.add(egui::DragValue::new(&mut s.seed));
        ui.end_row();
        ui.label("Lost after");
        ui.add(egui::DragValue::new(&mut s.lost_after_s).speed(0.1).range(0.2..=600.0).suffix(" s"));
        ui.end_row();
        ui.label("Snapshot rate");
        ui.add(egui::DragValue::new(&mut s.snapshot_hz).speed(0.1).range(0.05..=20.0).suffix(" Hz"));
        ui.end_row();
        ui.label("Device status every");
        ui.add(egui::DragValue::new(&mut s.status_every_s).speed(0.5).range(0.5..=600.0).suffix(" s"));
        ui.end_row();
    });
    ui.separator();
    ui.label(RichText::new("Positioning engine").strong());
    ui.checkbox(&mut s.kalman, "Kalman tracking filter (constant velocity)");
    ui.checkbox(&mut s.robust, "NLOS / outlier rejection (needs redundant anchors)");
    ui.checkbox(&mut s.solve_3d, "Solve height (3D) when anchors are at different heights");
    ui.separator();
    ui.label(RichText::new("Overlay").strong());
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut s.show_labels, "labels");
        ui.checkbox(&mut s.show_links, "links");
        ui.checkbox(&mut s.show_ranges, "range circles");
        ui.checkbox(&mut s.show_trails, "trails & paths");
        ui.checkbox(&mut s.show_truth, "ground truth");
        ui.checkbox(&mut s.show_zones, "zones");
        ui.checkbox(&mut s.show_beams, "RFID beams");
    });
    ui.label(RichText::new("Links: green = LOS used · amber = LOS rejected · red = NLOS (through objects) · grey = no reply").small().color(theme::WEAK));
    if cfg.settings != before {
        cfg.save();
    }
}

use super::deploy::MIN_USEFUL_RANGE_M;

fn range_of(tx: f32, p: &super::device::Profile) -> f32 {
    super::radio::uwb_link_range_m(&super::device::Profile { uwb_tx_dbm_mhz: tx, ..p.clone() })
}

/// "35 cm" or "6.4 m".
fn dist(m: f32) -> String {
    if !m.is_finite() {
        "–".into()
    } else if m < 1.0 {
        format!("{:.0} cm", m * 100.0)
    } else {
        format!("{m:.1} m")
    }
}

/// Auto deploy result in plain words: a verdict, the numbers that matter,
/// what to do next, and the planner's notes folded away.
fn deploy_result(ui: &mut egui::Ui, r: &super::deploy::DeployReport) {
    let uwb = r.anchor_reach_m > 0.0;
    let too_weak = uwb && r.anchor_reach_m < MIN_USEFUL_RANGE_M;
    let (err, err95) = if r.anchors > 0 { (r.uwb_rmse_m, r.uwb_cep95_m) } else { (r.rmse_m, r.cep95_m) };
    let (icon, verdict, color) = if too_weak {
        ("✗", "Anchors are too weak to position anything".to_string(), theme::DANGER)
    } else if uwb && r.anchors == 0 {
        ("✗", "No anchors were placed".to_string(), theme::DANGER)
    } else if r.targets_met {
        ("✓", format!("Good: 95% of positions within {}", dist(err95.min(r.target_m))), theme::SUCCESS)
    } else if r.coverage >= 0.8 {
        ("!", format!("Almost: not every spot reaches {}", dist(r.target_m)), theme::ACCENT)
    } else {
        ("✗", format!("Not accurate enough for {}", dist(r.target_m)), theme::DANGER)
    };
    ui.label(RichText::new(format!("{icon} {verdict}")).strong().size(15.0).color(color));
    ui.label(RichText::new(r.headline()).color(theme::WEAK));

    // The numbers that matter.
    let row = |ui: &mut egui::Ui, k: &str, v: String, good: bool| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(k).color(theme::WEAK));
            ui.label(RichText::new(v).strong().color(if good { theme::TEXT } else { theme::ACCENT }));
        });
    };
    if r.total_tags > 0 {
        row(ui, "Tags found in the 30 s test:", format!("{} of {}", r.located, r.total_tags), r.located == r.total_tags);
        if r.located > 0 {
            row(ui, "Position error:", format!("typically {}, 95% within {}", dist(err), dist(err95)), err95 <= r.target_m);
        }
    }
    if r.anchors > 0 {
        row(ui, "Floor with a good position:", format!("{:.0}%", r.coverage * 100.0), r.coverage >= 0.95);
    }

    // What to do.
    let mut todo: Vec<String> = Vec::new();
    if too_weak {
        todo.push(format!(
            "At this TX power an anchor reaches only {}. Raise the TX power above (−41.3 dBm/MHz is the legal maximum).",
            dist(r.anchor_reach_m)
        ));
    } else if !r.targets_met {
        if let Some(m) = r.capped_at {
            todo.push(format!("{m} anchors can't cover this floor to {}. Allow more anchors, or relax the accuracy.", dist(r.target_m)));
        }
        if !r.weak_zones.is_empty() {
            let names = if r.weak_zones.len() > 3 { format!("{} and {} more", r.weak_zones[..3].join(", "), r.weak_zones.len() - 3) } else { r.weak_zones.join(", ") };
            todo.push(format!("Racks hide {names} from the anchors. More anchors, or anchors on the racks, fix this."));
        }
        if r.located < r.total_tags {
            todo.push("Some tags were never located: the Quality map below shows where positioning fails.".into());
        }
        if todo.is_empty() {
            todo.push("Relax the accuracy target or allow more anchors.".into());
        }
    }
    if !todo.is_empty() {
        ui.label(RichText::new("What to do").strong());
        for t in todo.iter().take(3) {
            ui.label(RichText::new(format!("→ {t}")).small());
        }
    }
    if !r.notes.is_empty() {
        egui::CollapsingHeader::new(RichText::new(format!("Details ({})", r.notes.len())).small().color(theme::WEAK)).id_salt("deploy_details").default_open(false).show(ui, |ui| {
            for n in &r.notes {
                ui.label(RichText::new(format!("• {n}")).small().color(theme::WEAK));
            }
        });
    }
}
