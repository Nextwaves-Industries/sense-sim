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
    fn label(self) -> &'static str {
        match self {
            Tab::Devices => "Devices & zones",
            Tab::Inspector => "Inspector",
            Tab::Live => "Live data",
            Tab::Rf => "RF analysis",
            Tab::Planning => "Planning",
            Tab::Outputs => "Outputs / EMS",
            Tab::Settings => "Positioning settings",
        }
    }
}

/// What the detail card shows; the project tree picks it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Deploy,
    Test,
    /// One of the positioning tools.
    Tool(Tab),
    OpenEms,
    Fdtd,
    /// Predictions and how they held up (`crate::outcomes`).
    Outcomes,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Deploy => "Deploy",
            Page::Test => "Test live",
            Page::Tool(t) => t.label(),
            Page::OpenEms => "OpenEMS Edge",
            Page::Fdtd => "RF solver (FDTD)",
            Page::Outcomes => "Predictions",
        }
    }

    fn key(self) -> u8 {
        match self {
            Page::Deploy => 0,
            Page::Test => 1,
            Page::OpenEms => 2,
            Page::Fdtd => 3,
            Page::Outcomes => 4,
            Page::Tool(t) => 10 + t as u8,
        }
    }
}

pub struct UiLocal {
    page: Page,
    tech: Tech,
    height: f32,
    cell: f32,
    add: usize,
    /// Profile to plan with, when a technology has more than one kind.
    plan_profile: Option<String>,
    /// Extra tag heights to plan for, as typed ("0.3 1.8").
    plan_heights: String,
    plan_3d: bool,
    plan_redundancy: bool,
    plan_spacing: f32,
    plan_cost: f32,
    plan_refine: bool,
    draft: Option<OutputsConfig>,
    new_zone: String,
}

impl UiLocal {
    pub fn page(&self) -> Page {
        self.page
    }

    pub fn set_page(&mut self, page: Page) {
        self.page = page;
    }
}

impl Default for UiLocal {
    fn default() -> Self {
        Self { page: Page::Deploy, tech: Tech::Uwb, height: 1.2, cell: 0.5, add: 4, plan_profile: None, plan_heights: String::new(), plan_3d: false, plan_redundancy: false, plan_spacing: 1.5, plan_cost: 0.0005, plan_refine: true, draft: None, new_zone: "Zone 1".into() }
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
        LinkState::Connecting => theme::WARNING,
        LinkState::Error => theme::DANGER,
    }
}

fn c32(c: Color) -> Color32 {
    let s = c.to_srgba();
    theme::ink(Color32::from_rgb((s.red * 255.0) as u8, (s.green * 255.0) as u8, (s.blue * 255.0) as u8))
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
    pub ble_aoa: bool,
    pub wifi: bool,
    /// Keep the target with any one device failed.
    pub redundancy: bool,
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
        Self { uwb: true, ble: false, rfid: false, ble_aoa: false, wifi: false, redundancy: false, target_cm: 30.0, max_anchors: 0, anchor_tx_dbm_mhz: None, pending: None, last: None }
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

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    theme::card_frame().show(ui, |ui| {
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
    outcomes: ResMut<'w, crate::outcomes::ui::OutcomesUi>,
    predictions: ResMut<'w, crate::outcomes::Predictions>,
    recipe_jobs: Res<'w, crate::outcomes::hardware::Jobs>,
}

impl RtlsPanel<'_, '_> {
    /// The card's body: the page the tree picked. Deploy and Test live, the
    /// positioning tools, OpenEMS (the energy-management link) and FDTD (the
    /// openEMS field solver).
    pub fn show(&mut self, ui: &mut egui::Ui, max_height: f32) {
        let Self { commands, cfg, state, hub, place, rsel, cov, profiles, jobs, ems, deploy, bindings, ui_state, devices, zones, orbit, names, rf_hist, fdtd, applied, library, outcomes, predictions, recipe_jobs } = self;
        let local = &mut ui_state.local;
        let Ok(mut orbit) = orbit.single_mut() else { return };
        let page = local.page;
            egui::ScrollArea::vertical().id_salt(("rtls_page", page.key())).max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                match page {
                    Page::OpenEms => {
                        openems_page(ui, ems, hub, bindings, state);
                        return;
                    }
                    Page::Outcomes => {
                        crate::outcomes::ui::show(ui, outcomes, predictions, recipe_jobs);
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
                    Page::Tool(tab) => {
                        match tab {
                            Tab::Devices => devices_tab(ui, local, place, rsel, profiles, devices, zones, commands, state, &mut orbit),
                            Tab::Inspector => inspector_tab(ui, place, rsel, profiles, library, devices, zones, &names.0, state, commands),
                            Tab::Live => live_tab(ui, state, hub, devices),
                            Tab::Rf => {
                                let ids: std::collections::HashMap<Entity, String> = devices.iter().map(|(e, d, _)| (e, d.id.clone())).collect();
                                super::rf::rf_tab(ui, &mut local.tech, state, &ids, rf_hist, cov, &mut jobs.coverage, rsel, &mut orbit)
                            }
                            Tab::Planning => planning_tab(ui, local, cov, jobs, profiles, commands, devices),
                            Tab::Outputs => outputs_tab(ui, local, cfg, hub),
                            Tab::Settings => settings_tab(ui, cfg, profiles),
                        }
                        return;
                    }
                    Page::Deploy | Page::Test => {}
                }

                if page == Page::Deploy {
                    ui.horizontal(|ui| {
                        // Any mix of technologies: each is planned with its own
                        // link model, on spots the others leave free.
                        ui.checkbox(&mut deploy.uwb, "UWB");
                        if ui.checkbox(&mut deploy.ble, "BLE").on_hover_text("RSSI beacons").changed() && deploy.ble {
                            deploy.ble_aoa = false;
                        }
                        if ui.checkbox(&mut deploy.ble_aoa, "BLE AoA").on_hover_text("Direction-finding locators: far fewer devices, better accuracy").changed() && deploy.ble_aoa {
                            deploy.ble = false;
                        }
                        ui.checkbox(&mut deploy.wifi, "WiFi").on_hover_text("Access points with RTT (802.11mc)");
                        ui.checkbox(&mut deploy.rfid, "RFID").on_hover_text("Readers aimed at shelves and zones, portal pairs on doorways");
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut deploy.redundancy, "survive 1 failure").on_hover_text("Keep the target with any one device down. Takes more devices.");
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
                                        "Too weak: an anchor reaches only {:.1} m, so tags can't be positioned. Raise the power (−41.3 is the legal maximum) or use many anchors close together.",
                                        range_of(tx_now, &p)
                                    ))
                                    .small()
                                    .color(theme::DANGER),
                                );
                            }
                        }
                    }
                    let busy = deploy.pending.is_some();
                    let label = if busy { "Placing devices and testing…".to_string() } else { theme::label(theme::icon::LIGHTNING, "Auto deploy") };
                    let b = egui::Button::new(RichText::new(label).strong().size(15.0).color(theme::ON_ACCENT)).fill(if busy { theme::FAINT } else { theme::ACCENT }).stroke(egui::Stroke::NONE).min_size(vec2(ui.available_width(), 36.0));
                    if ui.add_enabled(!busy && (deploy.uwb || deploy.ble || deploy.ble_aoa || deploy.wifi || deploy.rfid), b)
                        .on_hover_text("Places anchors, beacons, readers and tags for this scene, then runs a 30 s test. Running it again replaces only what it placed.")
                        .clicked()
                    {
                        deploy.pending = Some(DeployRequest { uwb: deploy.uwb, ble: deploy.ble, rfid: deploy.rfid, ble_aoa: deploy.ble_aoa, wifi: deploy.wifi, redundancy: deploy.redundancy, target_m: deploy.target_cm / 100.0, max_anchors: (deploy.max_anchors > 0).then_some(deploy.max_anchors), anchor_tx_dbm_mhz: deploy.anchor_tx_dbm_mhz, ..Default::default() });
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
                        let scan_tech = if cov.cells.is_empty() { infra_tech.unwrap_or(if deploy.ble || deploy.ble_aoa { Tech::Ble } else if deploy.wifi { Tech::Wifi } else { Tech::Uwb }) } else { cov.tech };
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
                                let col = if share >= 0.95 { theme::SUCCESS } else { theme::WARNING };
                                ui.label(RichText::new(format!("{:.0}% of the floor passes ({}, tag at {:.1} m)", share * 100.0, cov.tech.label(), cov.height)).color(col));
                            }
                        }
                        let legend = match (cov.view, cov.tech) {
                            (MapView::Quality, Tech::Rfid) | (MapView::Accuracy, Tech::Rfid) => "blue = read chance per round over 90% · grey = less".to_string(),
                            (MapView::Quality, _) => format!(
                                "green = ≥ {:.0}% of readings within {:.0} cm · orange/red = less · grey = no position",
                                cov.quality.min_share * 100.0,
                                deploy.target_cm
                            ),
                            (MapView::Accuracy, _) => "RMS error, 95% of readings within 1.7× that: green ≤ 10 cm · yellow ≈ 30 cm · red ≥ 1 m · grey = no position".to_string(),
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
                }

                if page == Page::Test {
                    let snap = state.snapshot.clone();
                    ui.horizontal(|ui| {
                        let s = &mut cfg.settings;
                        let label = if s.running { theme::label(theme::icon::PAUSE, "Pause") } else { theme::label(theme::icon::PLAY, "Run") };
                        if ui.add(egui::Button::new(RichText::new(label).strong()).min_size(vec2(80.0, 28.0))).clicked() {
                            s.running = !s.running;
                        }
                        egui::ComboBox::from_id_salt("rtls_speed").selected_text(format!("{}×", s.speed)).width(55.0).show_ui(ui, |ui| {
                            for v in [0.5, 1.0, 2.0, 5.0, 10.0] {
                                ui.selectable_value(&mut s.speed, v, format!("{v}×"));
                            }
                        });
                        if ui.button(theme::icon::ARROW_COUNTER_CLOCKWISE).on_hover_text("Restart time and statistics").clicked() {
                            let seed = s.seed;
                            state.restart(seed);
                        }
                        ui.label(RichText::new(format!("{:.0} s", state.time)).color(theme::WEAK));
                    });
                    super::tag_card::add_tags_row(ui, place, profiles, devices.iter().map(|(_, d, t)| (d, t.translation)), commands, &mut state.rng);
                    if ui.button("RF analysis: signal, NLOS, anchors, dead zones").clicked() {
                        local.page = Page::Tool(Tab::Rf);
                    }
                    let live = state.error_stats_checked(None);
                    ui.horizontal(|ui| {
                        let kpi = |ui: &mut egui::Ui, v: String, k: &str, col: Color32| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(v).size(18.0).strong().color(col));
                                ui.label(RichText::new(k).small().color(theme::WEAK));
                            });
                            ui.add_space(14.0);
                        };
                        let located_col = if snap.tags_lost > 0 { theme::WARNING } else { theme::SUCCESS };
                        kpi(ui, format!("{}/{}", snap.tags_located, snap.tags_total), "tags located", located_col);
                        let cm = |v: Option<f32>| v.map_or("–".to_string(), |v| format!("{:.0} cm", v * 100.0));
                        kpi(ui, cm(live.map(|e| e.rmse)), "RMS error (30 s)", theme::TEXT);
                        kpi(ui, cm(live.map(|e| e.cep95)), "95% within (30 s)", theme::TEXT);
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
        Phase::Working(step, p) if *p >= 0.0 => (theme::WARNING, "Starting…", format!("{step} · {:.0}%", p * 100.0)),
        Phase::Working(step, _) => (theme::WARNING, "Starting…", step.clone()),
        Phase::Running { .. } if connected => (theme::SUCCESS, "Connected", "Positioning data is streaming into OpenEMS.".to_string()),
        Phase::Running { .. } => match &link_error {
            Some(e) => (theme::DANGER, "Can't reach OpenEMS", e.clone()),
            None => (theme::WARNING, "Connecting…", String::new()),
        },
    };

    // ---- Status and the action that goes with it
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").size(14.0).color(dot));
            ui.label(RichText::new(title).strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let accent = |text: &str| egui::Button::new(RichText::new(text).strong().color(theme::ON_ACCENT)).fill(theme::ACCENT).stroke(egui::Stroke::NONE).min_size(vec2(0.0, 28.0));
                match &phase {
                    Phase::Idle | Phase::Failed(_) => {
                        if ui.add(accent(&if installed { theme::label(theme::icon::PLAY, "Start") } else { theme::label(theme::icon::DOWNLOAD_SIMPLE, "Install & start") })).clicked() {
                            ems.start();
                        }
                    }
                    Phase::Working(..) => {
                        ui.spinner();
                    }
                    Phase::Running { .. } => {
                        if !connected || !ems.owns_process() {
                            if ui.add(accent(&if installed { theme::label(theme::icon::ARROW_COUNTER_CLOCKWISE, "Restart") } else { theme::label(theme::icon::DOWNLOAD_SIMPLE, "Install & start") })).clicked() {
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
            "Address, password and the other outputs (MQTT, Modbus…): Simulation → Outputs.",
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
            if ui.button(theme::label(theme::icon::DOWNLOAD_SIMPLE, "Reinstall OpenEMS")).clicked() {
                let dir = super::openems_local::base_dir();
                let _ = std::fs::remove_file(dir.join("openems-edge.jar"));
                let _ = std::fs::remove_dir_all(dir.join("jre"));
                ems.stop();
                ems.start();
            }
        }
    });
}

/// Viewport overlay: device labels, placement hints, the card next to the
/// selected device, the tag details, and the detail card the tree opened.
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
    publish_status(&mut slot, &mut panel);
    // The assistant hides overlays while it captures the viewport; Antenna Studio covers it.
    if hide.0 > 0 || slot.canvas_taken() {
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
    // The part of the canvas no tree or detail card covers.
    let free = slot.free.unwrap_or(vp);

    // ---- Labels over the viewport
    if panel.cfg.settings.show_labels {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("rtls_labels"))).with_clip_rect(vp);
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
            theme::viewport_chip(&painter, r);
            painter.galley(r.min + vec2(4.0, 1.0), galley, col);
        }
        for (_, z) in panel.zones.iter() {
            let c = (Vec3::from(z.min) + Vec3::from(z.max)) * 0.5;
            if let Ok(s) = cam.world_to_viewport(cam_gt, Vec3::new(c.x, z.max[1], c.z)) {
                let n = panel.state.zone_occupancy.get(&z.name).copied().unwrap_or(0);
                let col = if z.restricted { theme::DANGER } else { theme::ONLINE };
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
        egui::Area::new(egui::Id::new("rtls_hint")).order(egui::Order::Middle).pivot(egui::Align2::CENTER_TOP).fixed_pos(egui::pos2(free.center().x, vp.top() + 58.0)).show(&ctx, |ui| {
            theme::floating_frame().inner_margin(8).show(ui, |ui| {
                ui.label(RichText::new(msg));
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
        egui::Area::new(egui::Id::new("rtls_device_card")).order(egui::Order::Middle).fixed_pos(egui::pos2(s.x + 18.0, s.y - 10.0)).constrain(true).show(&ctx, |ui| {
            theme::floating_frame().inner_margin(8).show(ui, |ui| {
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
                ui.label(RichText::new("drag onto racks, walls or ceiling · Shift+drag flat · Rings rotate · Green arrow lifts").small().color(theme::WEAK));
            });
        });
        if delete {
            panel.commands.entity(e).despawn();
            panel.rsel.device = None;
        }
        if details {
            let l = &mut panel.ui_state.local;
            l.page = Page::Tool(Tab::Inspector);
            slot.rtls_open = true;
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
        let width = 380.0f32.min(free.width() * 0.7).max(260.0);
        let rect = egui::Rect::from_min_max(egui::pos2(free.right() - width, vp.top() + 10.0), egui::pos2(free.right(), vp.bottom() - 10.0));
        let mut close = false;
        let mut delete = false;
        let mut enabled = d.enabled;
        egui::Area::new(egui::Id::new("rtls_tag_dock")).order(egui::Order::Middle).fixed_pos(rect.min).show(&ctx, |ui| {
            ui.set_max_height(rect.height());
            theme::floating_frame().inner_margin(10).show(ui, |ui| {
                ui.set_width(rect.width() - 20.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(color));
                    ui.label(RichText::new(&title).strong().size(15.0));
                    ui.label(RichText::new(kind).color(theme::WEAK));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::close_button(ui, "Close (click the tag again or \"Show data & graphs\" to reopen)").clicked() {
                            close = true;
                        }
                        if theme::icon_button(ui, theme::icon::TRASH, "Delete (Del)").clicked() {
                            delete = true;
                        }
                        ui.checkbox(&mut enabled, "on");
                    });
                });
                ui.label(RichText::new("drag the tag onto racks, walls or ceiling · Shift+drag flat · Rings rotate · Green arrow lifts").small().color(theme::WEAK));
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

    // ---- The detail card the tree opened, beside the tree.
    if let Some(rect) = slot.rtls {
        let title = panel.ui_state.local.page.title();
        let mut close = false;
        egui::Area::new(egui::Id::new("rtls_card")).order(egui::Order::Middle).fixed_pos(rect.min).show(&ctx, |ui| {
            theme::fixed_card(ui, rect.width(), rect.height(), |ui| {
                theme::card_title(ui, title, |ui| {
                    close = theme::close_button(ui, "Close this card").clicked();
                });
                egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 10, top: 10, bottom: 12 }).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    panel.show(ui, rect.height() - 76.0);
                });
            });
        });
        if close {
            slot.rtls_open = false;
        }
    }
    Ok(())
}

/// What the project tree shows about the positioning system.
fn publish_status(slot: &mut crate::ui::LeftPanelSlot, panel: &mut RtlsPanel) {
    use theme::Dot;
    slot.rtls_alerts = panel.state.alarms.len() as u32;
    let devices = panel.devices.iter().count();
    let infra = panel.devices.iter().any(|(_, d, _)| !d.kind.is_mobile());
    let deploying = panel.deploy.pending.is_some();
    let ems_phase = panel.ems.phase();
    let connected = panel.hub.sinks.iter().find(|s| s.name == "OpenEMS REST").is_some_and(|s| s.shared.status().state == LinkState::Connected);
    let fdtd = panel.fdtd.status();
    slot.rtls_status = crate::ui::RtlsStatus {
        deploy: if deploying {
            Dot::Busy
        } else if infra || panel.deploy.last.is_some() {
            Dot::Done
        } else {
            Dot::Todo
        },
        test: if panel.cfg.settings.running && panel.state.snapshot.tags_total > 0 {
            Dot::Done
        } else {
            Dot::Optional
        },
        ems: match &ems_phase {
            Phase::Running { .. } if connected => Dot::Done,
            Phase::Running { .. } | Phase::Working(..) => Dot::Busy,
            Phase::Failed(_) => Dot::Todo,
            Phase::Idle => Dot::Optional,
        },
        fdtd: match &fdtd {
            super::fdtd::Status::Busy(..) => Dot::Busy,
            super::fdtd::Status::Failed(_) => Dot::Todo,
            super::fdtd::Status::Idle => Dot::Optional,
        },
        devices,
        zones: panel.zones.iter().count(),
    };
    slot.jobs.clear();
    if deploying {
        slot.jobs.push(crate::ui::Job { text: "Placing devices and testing".into(), progress: -1.0 });
    }
    if let Phase::Working(step, p) = &ems_phase {
        slot.jobs.push(crate::ui::Job { text: format!("OpenEMS: {step}"), progress: *p });
    }
    if let super::fdtd::Status::Busy(msg, p) = &fdtd {
        slot.jobs.push(crate::ui::Job { text: format!("RF solver: {}", msg.chars().take(60).collect::<String>()), progress: *p });
    }
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
    theme::caption(ui, "Place a device (click in the scene)");
    let w = ui.available_width();
    for kind in DeviceKind::ALL {
        for p in profiles.0.iter().filter(|p| p.kind == kind) {
            let active = place.profile.as_deref() == Some(p.id.as_str());
            if theme::color_row(ui, w, c32(kind.color()), &p.name, active).on_hover_text(format!("{}\n{} · {} · id {}", p.name, kind.label(), p.vendor, p.id)).clicked() {
                place.profile = if active { None } else { Some(p.id.clone()) };
                place.waypoints_for = None;
            }
        }
    }
    ui.checkbox(&mut place.sticky, "Keep placing after each click");
    ui.label(
        RichText::new(format!("Extra vendor profiles (e.g. Nextwaves models) are read from {}", Profiles::user_dirs().last().map(|p| p.display().to_string()).unwrap_or_default()))
            .small()
            .color(theme::WEAK),
    );
    ui.separator();

    theme::caption(ui, "Zones");
    ui.horizontal(|ui| {
        ui.add_sized([(ui.available_width() - 78.0).max(60.0), 26.0], egui::TextEdit::singleline(&mut local.new_zone));
        if ui.button(theme::label(theme::icon::PLUS, "Add")).on_hover_text("Add a zone at the centre of the view").clicked() {
            let c = orbit.focus;
            let z = Zone { name: local.new_zone.clone(), min: [c.x - 2.0, 0.0, c.z - 2.0], max: [c.x + 2.0, 2.5, c.z + 2.0], restricted: false, capacity: 0, auto: false };
            let e = commands.spawn((Name::new(format!("Zone {}", z.name)), z)).id();
            rsel.zone = Some(e);
            rsel.device = None;
            let n = zones.iter().count() + 2;
            local.new_zone = format!("Zone {n}");
            local.page = Page::Tool(Tab::Inspector);
        }
    });
    let mut zl: Vec<(Entity, String, bool)> = zones.iter().map(|(e, z)| (e, z.name.clone(), z.restricted)).collect();
    zl.sort_by(|a, b| a.1.cmp(&b.1));
    for (e, name, restricted) in zl {
        ui.horizontal(|ui| {
            let n = state.zone_occupancy.get(&name).copied().unwrap_or(0);
            let txt = format!("{}{name} · {n} inside", if restricted { "\u{e4e2} " } else { "" });
            if ui.selectable_label(rsel.zone == Some(e), txt).clicked() {
                rsel.zone = Some(e);
                rsel.device = None;
                local.page = Page::Tool(Tab::Inspector);
            }
        });
    }
    ui.separator();

    ui.horizontal(|ui| {
        theme::caption(ui, &format!("Devices ({})", devices.iter().count()));
        // Remove every device of one kind (e.g. all anchors) or all of them.
        let present: Vec<(DeviceKind, usize)> =
            DeviceKind::ALL.into_iter().map(|k| (k, devices.iter().filter(|(_, d, _)| d.kind == k).count())).filter(|&(_, n)| n > 0).collect();
        if present.is_empty() {
            return;
        }
        let mut remove: Option<Option<DeviceKind>> = None;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button(format!("{} Remove all", theme::icon::TRASH), |ui| {
                for &(k, n) in &present {
                    if ui.button(format!("All {} ({n})", plural(k.label()))).clicked() {
                        remove = Some(Some(k));
                    }
                }
                ui.separator();
                if ui.button(RichText::new("Every device").color(theme::DANGER)).clicked() {
                    remove = Some(None);
                }
            });
        });
        if let Some(kind) = remove {
            for (e, d, _) in devices.iter() {
                if kind.is_none_or(|k| d.kind == k) {
                    commands.entity(e).despawn();
                    if rsel.device == Some(e) {
                        rsel.device = None;
                    }
                }
            }
        }
    });
    let mut list: Vec<(Entity, String, DeviceKind, bool, Vec3)> = devices.iter().map(|(e, d, t)| (e, d.id.clone(), d.kind, d.enabled, t.translation)).collect();
    list.sort_by(|a, b| (a.2 as u8, &a.1).cmp(&(b.2 as u8, &b.1)));
    egui::Grid::new("rtls_dev_list").striped(true).num_columns(4).show(ui, |ui| {
        for (e, id, kind, enabled, pos) in list {
            if ui.selectable_label(rsel.device == Some(e), RichText::new(&id).color(c32(kind.color()))).clicked() {
                rsel.device = Some(e);
                rsel.zone = None;
                orbit.focus = pos;
                local.page = Page::Tool(Tab::Inspector);
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
            ui.label(RichText::new(format!("\"{name}\" is not in the antenna library: treated as 0 dBi.")).small().color(theme::WARNING));
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
    if matches!(kind, DeviceKind::UwbAnchor | DeviceKind::UwbTag | DeviceKind::BleNode | DeviceKind::BleTag | DeviceKind::WifiAp | DeviceKind::WifiClient) {
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
        // Each field turns by the change alone, so editing one keeps the other two
        // (and a ceiling mount, where yaw and roll share an axis, stays put).
        let (yaw, tilt, roll) = t.rotation.to_euler(EulerRot::YXZ);
        let angle = |ui: &mut egui::Ui, name: &str, tip: &str, rad: f32| {
            let mut deg = rad.to_degrees();
            ui.label(name).on_hover_text(tip);
            ui.add(egui::DragValue::new(&mut deg).speed(1.0).suffix("°")).changed().then(|| deg.to_radians() - rad)
        };
        if let Some(d) = angle(ui, "Yaw", "Turn around the vertical.", yaw) {
            t.rotate_y(d);
        }
        if let Some(d) = angle(ui, "Tilt", "Aim up (+) or down (-). -90° points straight down.", tilt) {
            t.rotate_local_x(d);
        }
        if let Some(d) = angle(ui, "Roll", "Turn around the direction it faces.", roll) {
            t.rotate_local_z(d);
        }
    });
    if !kind.is_mobile() && ui.button("Height = profile").on_hover_text(format!("{:.1} m", prof.mount_height_m)).clicked() {
        t.translation.y = prof.mount_height_m;
    }
    ui.label(
        RichText::new("Drag in the viewport to move · Rings rotate (Ctrl: 15° steps) · Green arrow or Alt+drag changes height · Q/E rotate · G look at · Del deletes · Ctrl+D duplicates").small().color(theme::WEAK),
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
                    if ui.add(egui::Button::new(if adding { theme::label(theme::icon::CHECK, "Done adding") } else { theme::label(theme::icon::PLUS, "Add waypoints by clicking") }).selected(adding)).clicked() {
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
                ui.label(if l.used { theme::icon::CHECK } else { "" });
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
    let live = state.error_stats_checked(None);
    let whole = state.run_stats(None, None);
    let snap = state.snapshot.clone();
    let cm = |v: Option<f32>, digits: usize| v.map_or("–".to_string(), |v| format!("{:.digits$} cm", v * 100.0));
    egui::Grid::new("rtls_metrics").num_columns(4).spacing(vec2(14.0, 4.0)).show(ui, |ui| {
        let kv = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(RichText::new(k).color(theme::WEAK));
            ui.label(RichText::new(v).strong());
        };
        kv(ui, "Tags located", format!("{} / {}", snap.tags_located, snap.tags_total));
        kv(ui, "Lost", format!("{}", snap.tags_lost));
        ui.end_row();
        kv(ui, "RMSE (30 s)", cm(live.map(|e| e.rmse), 1));
        kv(ui, "CEP50 / 95 (30 s)", format!("{} / {}", cm(live.map(|e| e.cep50), 0), cm(live.map(|e| e.cep95), 0)));
        ui.end_row();
        kv(ui, "Max error (30 s)", cm(live.map(|e| e.max), 0));
        kv(ui, "RFID reads/s", format!("{:.0}", state.rfid_rate()));
        ui.end_row();
        kv(ui, "RMSE (whole run)", cm(whole.stats().map(|e| e.rmse), 1));
        kv(ui, "CEP95 (whole run)", cm(whole.stats().map(|e| e.cep95), 0));
        ui.end_row();
        kv(ui, "Fixes (whole run)", format!("{} of {} updates", whole.fixes, whole.attempts));
        kv(ui, "Rejected as wild", format!("{}", whole.rejected));
        ui.end_row();
        kv(ui, "Infra online", format!("{} / {}", snap.infra_online, snap.infra_total));
        kv(ui, "Events out", format!("{} ({} published)", state.events_total, hub.published));
        ui.end_row();
    });
    for tech in ["uwb", "ble", "wifi"] {
        if let Some(e) = state.error_stats_checked(Some(tech)) {
            ui.label(RichText::new(format!("{} (30 s): RMSE {:.1} cm · CEP95 {:.1} cm · {} fixes", tech.to_uppercase(), e.rmse * 100.0, e.cep95 * 100.0, e.n)).color(theme::WEAK));
        }
    }
    if ui.button("Reset statistics").clicked() {
        state.reset_metrics();
    }
    let rm: Vec<(f64, f32)> = state.series.iter().filter(|p| p.rmse.is_finite()).map(|p| (p.t, p.rmse * 100.0)).collect();
    chart(ui, &rm, Color32::from_rgb(20, 130, 200), "RMSE", " cm");
    let lo: Vec<(f64, f32)> = state.series.iter().map(|p| (p.t, p.located as f32)).collect();
    chart(ui, &lo, theme::SUCCESS, "Tags located", "");
    let rd: Vec<(f64, f32)> = state.series.iter().map(|p| (p.t, p.reads)).collect();
    if rd.iter().any(|p| p.1 > 0.0) {
        chart(ui, &rd, Color32::from_rgb(40, 158, 84), "RFID reads/s", "");
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
            let at = |v: f32| if v.is_finite() { format!("{v:.2}") } else { "–".to_string() };
            ui.label(at(t.x));
            ui.label(at(t.z));
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
            (Tech::Rfid, _) => "blue = read chance per round over 90% (brighter = more link margin) · grey = less".to_string(),
            (_, MapView::Quality) => format!(
                "green = ≥ {:.0}% of readings within {:.0} cm ({:.0}% of the floor) · red = less · grey = no fix",
                cov.quality.min_share * 100.0,
                cov.quality.target_m * 100.0,
                cov.quality_share() * 100.0
            ),
            (_, MapView::Accuracy) => "predicted RMS error, 95% of readings within 1.7× that: green ≤ 10 cm · yellow ≈ 30 cm · red ≥ 1 m · grey = no fix".to_string(),
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
            let mut heights = vec![local.height];
            heights.extend(local.plan_heights.split([' ', ',']).filter_map(|h| h.parse::<f32>().ok()));
            jobs.optimize = Some(OptimizeRequest {
                tech: local.tech,
                add: local.add,
                height: local.height,
                heights,
                profile: local.plan_profile.clone(),
                dim3: local.plan_3d,
                redundancy: local.plan_redundancy,
                min_spacing_m: Some(local.plan_spacing),
                cost_weight: Some(local.plan_cost),
                refine: local.plan_refine,
                expand_heights: true,
                ..Default::default()
            });
        }
    });
    // A technology with several kinds of device (BLE beacon or AoA locator, WiFi RTT or RSSI).
    let kinds: Vec<(String, String)> = profiles.0.iter().filter(|p| p.kind == local.tech.infra()).map(|p| (p.id.clone(), p.name.clone())).collect();
    if local.plan_profile.as_ref().is_some_and(|id| !kinds.iter().any(|k| k.0 == *id)) {
        local.plan_profile = None;
    }
    if kinds.len() > 1 {
        let shown = local.plan_profile.as_ref().and_then(|id| kinds.iter().find(|k| k.0 == *id)).or(kinds.first()).map_or("", |k| k.1.as_str()).to_string();
        egui::ComboBox::from_id_salt("rtls_plan_profile").selected_text(shown).show_ui(ui, |ui| {
            for (id, name) in &kinds {
                ui.selectable_value(&mut local.plan_profile, Some(id.clone()), name);
            }
        });
    }
    egui::CollapsingHeader::new(RichText::new("Options").small().color(theme::WEAK)).id_salt("rtls_plan_options").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("more tag heights");
            ui.add(egui::TextEdit::singleline(&mut local.plan_heights).desired_width(70.0).hint_text("0.3 1.8"));
        });
        ui.checkbox(&mut local.plan_3d, "full 3D fixes").on_hover_text("Asks for height too: needs anchors at different heights");
        ui.checkbox(&mut local.plan_redundancy, "survive 1 device failure");
        ui.checkbox(&mut local.plan_refine, "fine-tune along the mounting surface");
        ui.add(egui::DragValue::new(&mut local.plan_spacing).range(0.3..=20.0).speed(0.1).prefix("min spacing ").suffix(" m"));
        ui.add(egui::DragValue::new(&mut local.plan_cost).range(0.0..=0.05).speed(0.0002).prefix("cost weight "));
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
                && let Some(prof) = local.plan_profile.as_deref().and_then(|id| profiles.get(id)).filter(|p| p.kind == local.tech.infra()).or_else(|| profiles.default_for(local.tech.infra()))
            {
                let mut ids: Vec<String> = devices.iter().map(|(_, d, _)| d.id.clone()).collect();
                let aims = std::mem::take(&mut cov.suggestion_facing);
                for (k, p) in std::mem::take(&mut cov.suggestions).into_iter().enumerate() {
                    let id = super::device::next_id(prof.kind, ids.iter().map(String::as_str));
                    ids.push(id.clone());
                    // Aimed as planned: a reader's beam or an array's view depends on it.
                    let dir = aims.get(k).copied().unwrap_or(Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
                    let up = if dir.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
                    super::view::spawn_device(
                        commands,
                        Device { id, kind: prof.kind, profile: prof.id.clone(), enabled: true, motion: Motion::Static, address: String::new(), auto: false, radio: Default::default() },
                        Transform::from_translation(p).looking_to(dir, up),
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

/// Picks the regulatory region and shows what it means for this deployment.
fn region_section(ui: &mut egui::Ui, region: &mut String, profiles: &mut Profiles) {
    ui.label(RichText::new("Regulatory region").strong());
    let shown = profiles.1.active.as_ref().map_or_else(|| "None (profile values)".to_string(), |r| r.name.clone());
    let mut pick = None;
    egui::ComboBox::from_id_salt("rtls_region").selected_text(shown).width(240.0).show_ui(ui, |ui| {
        if ui.selectable_label(profiles.1.active.is_none(), "None (profile values)").clicked() {
            pick = Some(String::new());
        }
        for r in &profiles.1.regions {
            let on = profiles.1.active.as_ref().is_some_and(|a| a.id == r.id);
            if ui.selectable_label(on, &r.name).clicked() {
                pick = Some(r.id.clone());
            }
        }
    });
    if let Some(id) = pick
        && profiles.set_region(&id).is_ok()
    {
        *region = id;
    }
    let small = |ui: &mut egui::Ui, text: String, color: Color32| {
        ui.add(egui::Label::new(RichText::new(text).small().color(color)).wrap());
    };
    let Some(r) = &profiles.1.active else {
        small(ui, "Devices use their profile values, which may exceed local limits.".into(), theme::WARNING);
        ui.separator();
        return;
    };
    let channels: Vec<String> = r.uwb.channels.iter().map(u32::to_string).collect();
    let uwb = if channels.is_empty() {
        format!("UWB: {}. {}", r.uwb.status.label(), r.uwb.note)
    } else {
        let power = r.uwb.max_dbm_mhz.map_or("power limit not confirmed".to_string(), |m| format!("{m} dBm/MHz"));
        format!("UWB: channel {}, {power} ({}). {}", channels.join(", "), r.uwb.status.label(), r.uwb.note)
    };
    small(ui, uwb, theme::WEAK);
    let rfid = match r.rfid.band_mhz {
        Some([lo, hi]) => format!("RFID: {lo} to {hi} MHz, {} ({}). {}", r.rfid.limit_text, r.rfid.status.label(), r.rfid.note),
        None => format!("RFID: {}. {}", r.rfid.status.label(), r.rfid.note),
    };
    small(ui, rfid, theme::WEAK);
    for c in r.changes(&profiles.1.base) {
        small(ui, format!("Changed: {c}"), theme::WEAK);
    }
    for w in r.warnings() {
        small(ui, w, theme::WARNING);
    }
    small(ui, format!("Checked {}. {} Design inputs, not legal advice: confirm with the regulator. Add countries as JSON in {}.", r.checked, super::region::PATTERN_NOTE, super::region::user_dirs().last().map(|d| d.display().to_string()).unwrap_or_default()), theme::WEAK);
    ui.separator();
}

fn settings_tab(ui: &mut egui::Ui, cfg: &mut RtlsConfig, profiles: &mut Profiles) {
    let before = cfg.settings.clone();
    region_section(ui, &mut cfg.settings.region, profiles);
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

use super::deploy::{ClassResult, Verdict, dist};

/// One class of tags in plain words: how far 95 % land, out of how many updates and how many
/// of those had a fix. A position that isn't there counts against the 95 %.
fn class_line(c: &ClassResult) -> String {
    let share = c.availability().unwrap_or(0.0) * 100.0;
    let count = format!("{} updates, {:.0}% with a fix, {} tag{}", c.attempts, share, c.tags, if c.tags == 1 { "" } else { "s" });
    match c.p95_m {
        Some(p) if p.is_finite() => format!("95% within {} · typically {} · {count}", dist(p), dist(c.rmse_m.unwrap_or(f32::NAN))),
        _ => match c.fix_p95_m {
            Some(p) => format!("95% not reached: {count}. Of the fixes, 95% within {}", dist(p)),
            None => format!("no position: {count}"),
        },
    }
}

/// Auto deploy result in plain words: a verdict, the numbers that matter,
/// what to do next, and the planner's notes folded away.
fn deploy_result(ui: &mut egui::Ui, r: &super::deploy::DeployReport) {
    let uwb = r.anchor_reach_m > 0.0;
    let too_weak = uwb && r.anchor_reach_m < MIN_USEFUL_RANGE_M;
    let (level, verdict) = r.verdict();
    let (icon, color) = match level {
        Verdict::Good => (theme::icon::CHECK_CIRCLE, theme::SUCCESS),
        Verdict::Almost => (theme::icon::WARNING, theme::WARNING),
        Verdict::Bad => (theme::icon::WARNING_CIRCLE, theme::DANGER),
    };
    ui.label(RichText::new(format!("{icon} {verdict}")).strong().size(15.0).color(color));
    ui.label(RichText::new(r.headline()).color(theme::WEAK));

    // The numbers that matter.
    let row = |ui: &mut egui::Ui, k: &str, v: String, good: bool| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(k).color(theme::WEAK));
            ui.label(RichText::new(v).strong().color(if good { theme::TEXT } else { theme::WARNING }));
        });
    };
    if r.total_tags > 0 {
        row(ui, &format!("Tags found in the {:.0} s simulated test:", r.test_s), format!("{} of {}", r.found, r.total_tags), r.found == r.total_tags);
    }
    for a in &r.accuracy {
        let t = a.tech.to_uppercase();
        for (name, c) in [("moving", &a.moving), ("parked", &a.parked)] {
            if c.attempts > 0 {
                row(ui, &format!("{t} {name} tags:"), class_line(c), c.p95_m.is_some_and(|p| p <= a.target_m));
            }
        }
    }
    if r.rfid_tags > 0 {
        let read = r.rfid_tags - r.rfid_unread.len();
        row(ui, &format!("RFID read within {:.0} s:", r.test_s), format!("{read} of {} tags · {:.0}% read chance per round", r.rfid_tags, r.rfid_read_chance * 100.0), r.rfid_unread.is_empty());
    }
    if r.anchors > 0 {
        row(ui, &format!("Floor with a usable position (95% within {}):", dist(r.target_m * 3.0)), format!("{:.0}%", r.coverage * 100.0), r.coverage >= 0.95);
        if r.reachable_coverage < 0.95 {
            row(ui, "Most these mounting points can reach:", format!("{:.0}%", r.reachable_coverage * 100.0), false);
        }
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

/// "UWB anchor" -> "UWB anchors".
fn plural(label: &str) -> String {
    format!("{label}s")
}
