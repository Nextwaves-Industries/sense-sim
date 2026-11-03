//! The Studio's shell: a full-canvas workspace with the design tree on the left,
//! the 3D canvas in the middle and the selected page on the right.

pub(crate) mod analysis;
pub mod charts;
mod design;
pub mod draw;
pub mod expr_field;
pub mod gate;
pub mod import;
pub mod phased;
pub mod reconfig;
mod results;
pub mod samples;
pub(crate) mod solve;
pub mod tagdesign;
pub mod tagtune;
mod view3d;

use std::collections::HashMap;

use antenna_core::array::ArrayConfig;
use antenna_core::farfield::{Pol, Quantity};
use antenna_core::geometry::Geometry;
use antenna_core::matching;
use antenna_core::optimize::Method;
use antenna_core::project::{FieldPlane, Goal, SimResult};
use bevy::prelude::*;
use bevy_egui::egui::{self, Align2, Margin, RichText, pos2, vec2};
use bevy_egui::EguiContexts;

use super::{Status, Studio};
use crate::rtls::antenna::AntennaLibrary;
use crate::rtls::planning::AppliedAntenna;
use crate::theme::{self, Dot, TreeRow, icon};
use view3d::{Camera, ViewMode};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Page {
    Samples,
    Gallery,
    TagDesign,
    Draw,
    #[default]
    Variables,
    Parts,
    Reconfig,
    Materials,
    Setup,
    SParams,
    Smith,
    FarField,
    Fields,
    Array,
    Phased,
    DirectionFinding,
    SignalChain,
    Radar,
    Matching,
    Mimo,
    Link,
    Rcs,
    Gate,
    Sweep,
    Optimize,
    Converge,
    Yield,
    Export,
}

impl Page {
    /// `--open antenna-<name>`, and the assistant's tool.
    pub fn from_name(s: &str) -> Option<Page> {
        Some(match s {
            "samples" => Page::Samples,
            "gallery" => Page::Gallery,
            "tag" => Page::TagDesign,
            "draw" => Page::Draw,
            "variables" => Page::Variables,
            "parts" => Page::Parts,
            "reconfig" => Page::Reconfig,
            "materials" => Page::Materials,
            "setup" => Page::Setup,
            "sparams" => Page::SParams,
            "smith" => Page::Smith,
            "farfield" => Page::FarField,
            "fields" => Page::Fields,
            "array" => Page::Array,
            "phased" => Page::Phased,
            "direction_finding" | "df" => Page::DirectionFinding,
            "signal_chain" | "chain" => Page::SignalChain,
            "radar" => Page::Radar,
            "matching" => Page::Matching,
            "mimo" => Page::Mimo,
            "link" => Page::Link,
            "rcs" => Page::Rcs,
            "gate" => Page::Gate,
            "sweep" => Page::Sweep,
            "optimize" => Page::Optimize,
            "converge" => Page::Converge,
            "yield" => Page::Yield,
            "export" => Page::Export,
            _ => return None,
        })
    }
}

/// Editing state of the Studio's pages.
pub struct StudioUi {
    /// The file the project came from or went to.
    pub file: Option<ProjectFile>,
    pub page: Page,
    pub cam: Camera,
    pub view: ViewMode,
    pub transparent: bool,
    /// A view asked for by a scripted run, applied after the page sets its own.
    pub script_view: Option<ViewMode>,
    pub show_ports: bool,
    // Model cache: the geometry of the current project, or why it can't be built.
    cached_for: Option<antenna_core::project::Project>,
    pub geometry: Result<Geometry, String>,
    pub plan: Option<antenna_core::csx::Plan>,
    /// The parts as they stand, with or without a feed: what the canvas draws.
    pub model: Option<Geometry>,
    /// Each solid as triangles and lines, ready to draw.
    tess: Vec<view3d::Tessellated>,
    /// The solver a run would use, and its mesh when that is MoM.
    pub chosen: antenna_core::project::SolverKind,
    mom_mesh: Option<(antenna_core::mom::MeshView, usize)>,
    /// Why a run with this setup can't go ahead, in words for the setup page.
    pub setup_problem: Option<String>,
    /// The setup page asked for openEMS, and how that install is going.
    pub want_install: bool,
    pub install_status: Option<String>,
    shown_page: Option<Page>,
    seen_result: usize,
    plane_shown: HashMap<String, Option<usize>>,
    array_cache: Option<((usize, usize, ArrayConfig), ArrayView)>,
    // Far field view.
    pub quantity: Quantity,
    pub pol: Pol,
    pub range_db: f64,
    pub far_port: usize,
    pub far_k: usize,
    pub use_drive: bool,
    // S-parameter view.
    pub sp_view: results::SpView,
    pub sp_pair: (usize, usize),
    pub sp_limit: f64,
    // Field planes.
    pub plane_k: usize,
    pub animate: bool,
    plane_tex: HashMap<String, egui::TextureHandle>,
    // Gallery.
    pub gallery_cat: Option<antenna_core::templates::Category>,
    pub gallery_search: String,
    pub sample_search: String,
    pub sample_group: Option<antenna_core::samples::Group>,
    thumbs: HashMap<&'static str, Geometry>,
    // Array.
    pub array: ArrayConfig,
    pub array_spacing_lambda: (f64, f64),
    pub phased: phased::PhasedUi,
    // Matching.
    pub match_f: f64,
    pub match_series: matching::Series,
    pub match_q: (f64, f64),
    pub match_pick: usize,
    pub match_band: bool,
    pub match_z0: f64,
    // Solver setup.
    pub rfid_bands: Vec<solve::RfidBand>,
    pub rfid_limits: Vec<antenna_core::report::RegionLimit>,
    // Link budget.
    pub link: results::LinkBudget,
    // Radar cross section.
    pub rcs_f: f64,
    pub rcs_phi: f64,
    pub rcs_pol_theta: bool,
    pub rcs_mono: bool,
    pub rcs_inc: f64,
    pub rcs_step: f64,
    // Sweep.
    pub sweep_var: String,
    pub sweep_range: (f64, f64),
    pub sweep_n: usize,
    pub sweep_var2: String,
    pub sweep_range2: (f64, f64),
    pub sweep_n2: usize,
    pub sweep_two: bool,
    pub parallel: usize,
    // Optimiser.
    pub opt_vars: Vec<(String, f64, f64, bool)>,
    pub goals: Vec<(Goal, f64)>,
    pub method: Method,
    pub max_evals: usize,
    // Convergence.
    pub conv_limit: f64,
    pub conv_steps: usize,
    // Yield.
    /// Name, relative tolerance, whether it varies, absolute tolerance.
    pub tol: Vec<(String, f64, bool, f64)>,
    pub yield_n: usize,
    pub yield_limit: f64,
    // Save and export.
    pub message: Option<(String, bool)>,
    pub lib_name: String,
    pub new_part: design::NewPart,
    pub new_port: design::NewPort,
    pub detail_w: f32,
    pub gate: gate::GateUi,
    pub reconfig: reconfig::ReconfigUi,
    pub import: import::ImportUi,
    pub draw: draw::DrawUi,
    pub tag: tagdesign::TagUi,
}

impl Default for StudioUi {
    fn default() -> Self {
        Self {
            file: None,
            page: Page::Variables,
            cam: Camera::default(),
            view: ViewMode::Model,
            transparent: false,
            script_view: None,
            show_ports: true,
            cached_for: None,
            geometry: Err(String::new()),
            plan: None,
            model: None,
            tess: Vec::new(),
            chosen: antenna_core::project::SolverKind::Auto,
            mom_mesh: None,
            setup_problem: None,
            want_install: false,
            install_status: None,
            shown_page: None,
            seen_result: 0,
            plane_shown: HashMap::new(),
            array_cache: None,
            quantity: Quantity::RealizedGain,
            pol: Pol::Total,
            range_db: 30.0,
            far_port: 0,
            far_k: 0,
            use_drive: false,
            sp_view: results::SpView::Db,
            sp_pair: (0, 0),
            sp_limit: -10.0,
            plane_k: 0,
            animate: true,
            plane_tex: HashMap::new(),
            gallery_cat: None,
            gallery_search: String::new(),
            sample_search: String::new(),
            sample_group: None,
            thumbs: HashMap::new(),
            array: ArrayConfig::default(),
            array_spacing_lambda: (0.5, 0.5),
            phased: Default::default(),
            match_f: 0.0,
            match_series: matching::Series::E24,
            match_q: (0.0, 0.0),
            match_pick: 0,
            match_band: false,
            match_z0: 50.0,
            rfid_bands: Vec::new(),
            rfid_limits: Vec::new(),
            link: results::LinkBudget::default(),
            rcs_f: 0.0,
            rcs_phi: 0.0,
            rcs_pol_theta: true,
            rcs_mono: true,
            rcs_inc: 0.0,
            rcs_step: 5.0,
            sweep_var: String::new(),
            sweep_range: (0.0, 0.0),
            sweep_n: 5,
            sweep_var2: String::new(),
            sweep_range2: (0.0, 0.0),
            sweep_n2: 3,
            sweep_two: false,
            parallel: 4,
            opt_vars: Vec::new(),
            goals: Vec::new(),
            method: Method::NelderMead,
            max_evals: 40,
            conv_limit: 0.02,
            conv_steps: 5,
            tol: Vec::new(),
            yield_n: 30,
            yield_limit: -10.0,
            message: None,
            lib_name: String::new(),
            new_part: design::NewPart::default(),
            new_port: design::NewPort::default(),
            detail_w: 470.0,
            gate: gate::GateUi::default(),
            reconfig: reconfig::ReconfigUi::default(),
            import: import::ImportUi::default(),
            draw: draw::DrawUi::default(),
            tag: tagdesign::TagUi::default(),
        }
    }
}

impl StudioUi {
    /// Only what the canvas draws, for each step of a drag: the mesh waits for the drop.
    fn refresh_shapes(&mut self, project: &antenna_core::project::Project) {
        self.model = project.scatterer().ok();
        self.tess = self.model.as_ref().map_or(Vec::new(), |g| g.solids.iter().map(|s| antenna_core::geometry::tessellate(&s.shape)).collect());
    }

    /// Rebuilds the geometry and mesh when the project changed.
    fn refresh(&mut self, project: &antenna_core::project::Project) {
        if self.cached_for.as_ref() == Some(project) {
            return;
        }
        self.cached_for = Some(project.clone());
        self.geometry = project.geometry().map_err(|e| format!("{e:#}"));
        let plan = self.geometry.as_ref().ok().map(|g| antenna_core::csx::plan(g, &project.setup, project.f0()));
        self.model = project.scatterer().ok();
        self.tess = self.model.as_ref().map_or(Vec::new(), |g| g.solids.iter().map(|s| antenna_core::geometry::tessellate(&s.shape)).collect());
        // Choosing the solver can mean meshing, so it happens here, not per frame.
        self.chosen = self.geometry.as_ref().map_or(antenna_core::project::SolverKind::Fdtd, |g| project.solver_for(g));
        self.mom_mesh = None;
        self.setup_problem = project.setup.check_band().err().map(|e| e.to_string());
        if self.chosen != antenna_core::project::SolverKind::Fdtd
            && let Ok(g) = &self.geometry
        {
            let s = &project.setup;
            let opts = antenna_core::mom::MomOptions { segments_per_lambda: s.segments_per_lambda, edges_per_lambda: s.edges_per_lambda, ..Default::default() };
            match antenna_core::mom::MomProblem::new(g, s.f_hi, opts) {
                Ok(p) => self.mom_mesh = Some((p.view(), p.unknowns())),
                Err(e) => {
                    self.setup_problem.get_or_insert(format!("{e:#}"));
                }
            }
        } else if let Some(Err(e)) = &plan {
            self.setup_problem.get_or_insert(format!("{e:#}"));
        }
        self.plan = plan.and_then(Result::ok);
    }
}

/// A short label for a number of seconds.
pub fn duration(s: f64) -> String {
    if s < 90.0 {
        format!("{s:.0} s")
    } else if s < 5400.0 {
        format!("{:.0} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

/// Coloured verdict line.
pub fn verdict(ui: &mut egui::Ui, good: bool, text: &str) {
    let col = if good { theme::SUCCESS } else { theme::WARNING };
    egui::Frame::new().fill(col.gamma_multiply(0.12)).corner_radius(5).inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.label(RichText::new(if good { icon::CHECK_CIRCLE } else { icon::WARNING_CIRCLE }).color(col));
            ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
        });
    });
}

/// A quiet group inside a page.
pub fn group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    theme::card_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui)
        })
        .inner
}

pub fn weak(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.add(egui::Label::new(RichText::new(text.into()).size(12.5).color(theme::WEAK)).wrap());
}

/// Full-width accent button.
pub fn run_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let b = egui::Button::new(RichText::new(text).strong().color(theme::ON_ACCENT)).fill(theme::ACCENT).stroke(egui::Stroke::NONE).min_size(vec2(ui.available_width(), 32.0));
    ui.add_enabled(enabled, b).clicked()
}

/// Metric tiles, three per row.
pub fn tiles(ui: &mut egui::Ui, items: &[(String, &str, egui::Color32)]) {
    crate::rtls::fdtd::ui::tiles(ui, items);
}

/// GHz number field. A drag snaps to steps about twice its speed, so below 3 GHz it moves
/// 1 MHz a pixel to reach band edges like 902 MHz. Typing takes units, and applies only
/// when finished: on the way to `915`, `91` GHz would mesh the whole model at 91 GHz.
pub fn ghz_field(ui: &mut egui::Ui, v: &mut f64) -> egui::Response {
    let mut x = *v / 1e9;
    let speed = if x < 3.0 { 0.001 } else { 0.01 };
    let r = ui
        .add(egui::DragValue::new(&mut x).speed(speed).range(1e-5..=1000.0).suffix(" GHz").max_decimals(6).update_while_editing(false).custom_parser(antenna_core::expr::parse_ghz))
        .on_hover_text("Type a value with its unit, such as 915 MHz or 2.45 GHz. Hold Shift while dragging for fine steps.");
    if r.changed() {
        *v = x * 1e9;
    }
    r
}

struct NavRow {
    page: Page,
    text: &'static str,
    glyph: &'static str,
    /// Available without a result.
    always: bool,
}

const NAV: [(&str, &[NavRow]); 6] = [
    (
        "Design",
        &[
            NavRow { page: Page::Samples, text: "Sample projects", glyph: icon::PACKAGE, always: true },
            NavRow { page: Page::Gallery, text: "Antenna templates", glyph: icon::SQUARES_FOUR, always: true },
            NavRow { page: Page::TagDesign, text: "Design an RFID tag", glyph: icon::SPARKLE, always: true },
            NavRow { page: Page::Draw, text: "Draw", glyph: icon::NOTE_PENCIL, always: true },
            NavRow { page: Page::Variables, text: "Variables", glyph: icon::SLIDERS_HORIZONTAL, always: true },
            NavRow { page: Page::Parts, text: "Parts and ports", glyph: icon::CUBE, always: true },
            NavRow { page: Page::Reconfig, text: "Reconfigurable", glyph: icon::LIGHTNING, always: true },
            NavRow { page: Page::Materials, text: "Materials", glyph: icon::STACK, always: true },
        ],
    ),
    ("Solve", &[NavRow { page: Page::Setup, text: "Solver setup and run", glyph: icon::PLAY_CIRCLE, always: true }]),
    (
        "Results",
        &[
            NavRow { page: Page::SParams, text: "S-parameters", glyph: icon::CHART_LINE, always: false },
            NavRow { page: Page::Smith, text: "Smith chart", glyph: icon::TARGET, always: false },
            NavRow { page: Page::FarField, text: "Far field", glyph: icon::BROADCAST, always: false },
            NavRow { page: Page::Fields, text: "Fields and currents", glyph: icon::WAVE_SINE, always: false },
        ],
    ),
    (
        "Analysis",
        &[
            NavRow { page: Page::Array, text: "Array synthesis", glyph: icon::GRID_FOUR, always: false },
            NavRow { page: Page::Matching, text: "Matching network", glyph: icon::PLUGS, always: false },
            NavRow { page: Page::Mimo, text: "MIMO and coupling", glyph: icon::SHARE_NETWORK, always: false },
            NavRow { page: Page::Link, text: "Link budget", glyph: icon::PULSE, always: false },
            NavRow { page: Page::Rcs, text: "Radar cross section", glyph: icon::CROSSHAIR, always: true },
            NavRow { page: Page::Gate, text: "RFID gate test", glyph: icon::DOOR_OPEN, always: true },
        ],
    ),
    (
        "Arrays and signals",
        &[
            NavRow { page: Page::Phased, text: "Phased array", glyph: icon::GRID_FOUR, always: true },
            NavRow { page: Page::DirectionFinding, text: "Direction finding", glyph: icon::CROSSHAIR, always: true },
            NavRow { page: Page::SignalChain, text: "Signal chain", glyph: icon::CPU, always: true },
            NavRow { page: Page::Radar, text: "Radar", glyph: icon::BROADCAST, always: true },
        ],
    ),
    (
        "Automate",
        &[
            NavRow { page: Page::Sweep, text: "Parameter sweep", glyph: icon::FADERS, always: true },
            NavRow { page: Page::Optimize, text: "Optimizer", glyph: icon::ROCKET_LAUNCH, always: true },
            NavRow { page: Page::Converge, text: "Mesh convergence", glyph: icon::GAUGE, always: true },
            NavRow { page: Page::Yield, text: "Tolerance and yield", glyph: icon::LIST_CHECKS, always: true },
            NavRow { page: Page::Export, text: "Save and export", glyph: icon::EXPORT, always: true },
        ],
    ),
];

fn nav_card(ui: &mut egui::Ui, st: &mut Studio, width: f32, has_result: bool) {
    let busy = st.busy();
    egui::ScrollArea::vertical().id_salt("studio_nav").show(ui, |ui| {
        for (title, rows) in NAV {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(title.to_uppercase()).size(11.5).color(theme::WEAK));
            });
            ui.add_space(2.0);
            for row in rows {
                let dim = !row.always && !has_result;
                let dot = match row.page {
                    Page::Setup if busy => Dot::Busy,
                    Page::Setup if has_result => Dot::Done,
                    _ => Dot::None,
                };
                let (c, r) = theme::tree_row(ui, width, &TreeRow { depth: 0, text: row.text, glyph: row.glyph, dot, selected: st.ui.page == row.page, expander: None, guide: false, last: false, note: "", dim });
                if dim {
                    r.on_hover_text("Run a simulation first");
                }
                if c.row {
                    st.ui.page = row.page;
                }
            }
        }
        ui.add_space(8.0);
    });
}

/// On arriving at a page, puts the canvas on what that page is about. After
/// that the view toolbar is the user's.
fn canvas_for_page(st: &mut Studio) {
    if st.ui.shown_page == Some(st.ui.page) {
        return;
    }
    st.ui.shown_page = Some(st.ui.page);
    st.ui.message = None;
    match st.ui.page {
        Page::Draw => draw::entered(st),
        Page::FarField | Page::Array | Page::Phased | Page::Link => st.ui.view = ViewMode::Pattern,
        Page::Fields => {
            let has_field = st.latest().is_some_and(|r| !r.planes.is_empty());
            st.ui.view = if has_field { ViewMode::Field } else { ViewMode::Currents };
        }
        Page::Variables | Page::Parts | Page::Reconfig | Page::Materials | Page::Samples | Page::Gallery | Page::Gate | Page::TagDesign if !matches!(st.ui.view, ViewMode::Model | ViewMode::Mesh) => st.ui.view = ViewMode::Model,
        _ => {}
    }
}

/// A finished run takes the user from the setup page to its results.
fn follow_new_result(st: &mut Studio) {
    let Some(r) = st.latest() else { return };
    let id = std::sync::Arc::as_ptr(&r) as usize;
    if st.ui.seen_result != id {
        st.ui.seen_result = id;
        st.ui.plane_shown.clear();
        st.ui.array_cache = None;
        if let Some(f) = st.ui.reconfig.view_f.take()
            && let Some(k) = r.far_index(f)
        {
            st.ui.far_k = k;
        }
        if st.ui.page == Page::Setup {
            st.ui.page = Page::SParams;
        }
    }
}

/// The array pattern for the current settings, recomputed only when they change.
pub fn array_far(ui: &mut StudioUi, r: &SimResult, element: &antenna_core::farfield::FarField) -> ArrayView {
    let key = (std::ptr::from_ref(r) as usize, ui.far_k, ui.array.clone());
    if let Some((k, view)) = &ui.array_cache
        && *k == key
    {
        return view.clone();
    }
    let (far, gain) = antenna_core::array::apply_with_gain(element, &ui.array);
    let summary = antenna_core::array::summarize(&ui.array, element.f);
    let cuts = [0.0, 90.0].map(|phi| antenna_core::array::cut_db(element, &ui.array, &gain, phi));
    let view = ArrayView { far, summary, gain, cuts };
    ui.array_cache = Some((key, view.clone()));
    view
}

/// An array as its page shows it. `far` is on the element's grid, for the 3D view; `gain`
/// and `cuts` resolve the beam whatever that grid is.
#[derive(Clone)]
pub struct ArrayView {
    pub far: antenna_core::farfield::FarField,
    pub summary: antenna_core::array::ArraySummary,
    pub gain: antenna_core::array::ArrayGain,
    /// Directivity in the planes φ = 0° and 90°.
    pub cuts: [Vec<(f64, f64)>; 2],
}

/// The dialog-free page router. Returns a far field to show instead of the run's.
fn page_ui(ui: &mut egui::Ui, st: &mut Studio, lib: (&mut AntennaLibrary, &mut AppliedAntenna)) {
    match st.ui.page {
        Page::Samples => samples::page(ui, st),
        Page::Gallery => design::gallery(ui, st),
        Page::TagDesign => tagdesign::page(ui, st),
        Page::Draw => draw::page(ui, st),
        Page::Variables => design::variables(ui, st),
        Page::Parts => design::parts(ui, st),
        Page::Reconfig => reconfig::page(ui, st),
        Page::Materials => design::materials(ui, st),
        Page::Setup => solve::setup(ui, st),
        Page::SParams => results::sparams(ui, st),
        Page::Smith => results::smith(ui, st),
        Page::FarField => results::far_field(ui, st),
        Page::Fields => results::fields(ui, st),
        Page::Array => analysis::array(ui, st),
        Page::Phased => phased::phased_page(ui, st),
        Page::DirectionFinding => phased::df_page(ui, st),
        Page::SignalChain => phased::chain_page(ui, st),
        Page::Radar => phased::radar_page(ui, st),
        Page::Matching => analysis::matching_page(ui, st),
        Page::Mimo => analysis::mimo(ui, st),
        Page::Link => results::link(ui, st),
        Page::Rcs => analysis::rcs(ui, st),
        Page::Gate => gate::page(ui, st),
        Page::Sweep => solve::sweep(ui, st),
        Page::Optimize => solve::optimize(ui, st),
        Page::Converge => solve::converge(ui, st),
        Page::Yield => solve::yield_page(ui, st),
        Page::Export => analysis::export(ui, st, lib.0, lib.1),
    }
}

fn status_strip(ui: &mut egui::Ui, st: &mut Studio) {
    match st.status() {
        Status::Busy(msg, p) => {
            group(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let title = if p >= 0.0 { format!("Solving… {:.0} %", p * 100.0) } else { "Working…".to_string() };
                    ui.label(RichText::new(title).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            st.cancel();
                        }
                    });
                });
                if p >= 0.0 {
                    ui.add(egui::ProgressBar::new(p).desired_height(5.0).fill(theme::ACCENT));
                }
                weak(ui, msg);
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        Status::Failed(e) if e != "Cancelled" => verdict(ui, false, &format!("The run failed: {}", e.chars().take(400).collect::<String>())),
        _ => {}
    }
}

/// Texture of a field plane on a dB colour scale, redrawn only when the frame changes.
fn plane_texture(st: &mut StudioUi, ctx: &egui::Context, p: &FieldPlane, key: &str, frame: Option<usize>) -> egui::TextureId {
    if st.plane_shown.get(key) == Some(&frame)
        && let Some(t) = st.plane_tex.get(key)
    {
        return t.id();
    }
    st.plane_shown.insert(key.to_string(), frame);
    let data: &[f32] = match frame.and_then(|k| p.frames.get(k)) {
        Some(f) => f,
        None => &p.mag,
    };
    let mut rgba = Vec::with_capacity(p.nu * p.nv * 4);
    for j in (0..p.nv).rev() {
        for i in 0..p.nu {
            let v = data.get(i + p.nu * j).copied().unwrap_or(0.0);
            let db = 20.0 * v.max(1e-6).log10();
            let t = (db + 40.0) / 40.0;
            let c = charts::heat(t);
            let a = (70.0 + 170.0 * t.clamp(0.0, 1.0)) as u8;
            rgba.extend([c.r(), c.g(), c.b(), a]);
        }
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([p.nu, p.nv], &rgba);
    match st.plane_tex.get_mut(key) {
        Some(t) => {
            t.set(img, egui::TextureOptions::LINEAR);
            t.id()
        }
        None => {
            let t = ctx.load_texture(key, img, egui::TextureOptions::LINEAR);
            let id = t.id();
            st.plane_tex.insert(key.to_string(), t);
            id
        }
    }
}

/// Far field shown on the canvas for the current page and controls.
fn canvas_far(st: &mut Studio, r: &SimResult) -> Option<antenna_core::farfield::FarField> {
    let k = st.ui.far_k.min(r.far_f.len().saturating_sub(1));
    let base = if st.ui.use_drive && r.far.len() > 1 {
        r.combined(k, &st.project.weights(r.far.len()))
    } else {
        r.far.get(st.ui.far_port.min(r.far.len().saturating_sub(1))).and_then(|p| p.get(k)).cloned()
    }?;
    if st.ui.page == Page::Array {
        return Some(array_far(&mut st.ui, r, &base).far);
    }
    Some(base)
}

/// What the top bar shows for the open project.
fn publish_doc(st: &Studio, slot: &mut crate::ui::LeftPanelSlot) {
    let file = st.ui.file.as_ref().filter(|f| f.is_for(&st.project));
    slot.studio_doc = crate::ui::StudioDoc {
        name: st.project.name.clone(),
        file: file.and_then(|f| f.path.file_name()).map(|n| n.to_string_lossy().to_string()),
        path: file.map(|f| f.path.display().to_string()),
        unsaved: file.is_some_and(|f| f.saved != st.project),
    };
}

/// Where the open project was last read or written, and how it stood then.
pub struct ProjectFile {
    pub path: std::path::PathBuf,
    pub saved: antenna_core::project::Project,
}

impl ProjectFile {
    /// False once a different antenna is showing, which must not overwrite this file.
    pub fn is_for(&self, project: &antenna_core::project::Project) -> bool {
        self.saved.template == project.template && self.saved.name == project.name
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use antenna_core::project::Project;

    #[test]
    fn a_different_antenna_does_not_count_as_the_one_in_the_file() {
        let saved = Project::from_template("dipole", None).unwrap();
        let file = ProjectFile { path: "a.antenna.json".into(), saved: saved.clone() };
        let mut edited = saved.clone();
        edited.params[0].expr = "1GHz".into();
        assert!(file.is_for(&edited), "the same antenna, edited: Save goes to its file");
        assert!(file.saved != edited, "and it shows as unsaved");
        assert!(!file.is_for(&Project::from_template("patch_probe", None).unwrap()), "another antenna must not overwrite it");
    }
}

/// Draws the Studio over the whole canvas when it is open.
#[allow(clippy::too_many_arguments)]
pub fn studio_ui(
    mut contexts: EguiContexts,
    mut studio: ResMut<Studio>,
    mut slot: ResMut<crate::ui::LeftPanelSlot>,
    mut library: ResMut<AntennaLibrary>,
    mut applied: ResMut<AppliedAntenna>,
    hide: Res<crate::ui::HideOverlays>,
    mut fdtd: ResMut<crate::rtls::fdtd::Fdtd>,
    profiles: Res<crate::rtls::device::Profiles>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    if profiles.is_changed() {
        studio.ui.rfid_bands = solve::rfid_bands(&profiles.1);
        studio.ui.rfid_limits = solve::rfid_limits(&profiles.1);
    }
    if let Some(name) = slot.studio_rename.take() {
        studio.project.name = name;
    }
    for ask in std::mem::take(&mut slot.studio_file) {
        use crate::ui::StudioFile;
        match ask {
            StudioFile::Save => analysis::save_project(&mut studio, false),
            StudioFile::SaveAs => analysis::save_project(&mut studio, true),
            StudioFile::Open => analysis::open_project(&mut studio),
        }
    }
    // Scripted runs: `--open antenna-<page>`, `--open antenna-run-<template>`.
    for name in std::mem::take(&mut slot.studio_goto) {
        slot.studio_open = true;
        if let Some(id) = name.strip_prefix("run-") {
            if let Ok(p) = antenna_core::project::Project::from_template(id, None) {
                studio.project = p;
                studio.run();
            }
        } else if let Some(v) = name.strip_prefix("view-") {
            studio.ui.script_view = ViewMode::ALL.iter().find(|m| m.1.to_lowercase().starts_with(v)).map(|m| m.0);
        } else if let Some(id) = name.strip_prefix("new-") {
            if let Ok(p) = antenna_core::project::Project::from_template(id, None) {
                studio.project = p;
            }
        } else if let Some(path) = name.strip_prefix("import-") {
            import::load(&mut studio, &[std::path::PathBuf::from(path)]);
            studio.ui.page = Page::Parts;
        } else if let Some(id) = name.strip_prefix("sample-") {
            match antenna_core::samples::by_id(id) {
                Some(s) => {
                    if let Err(e) = samples::open(&mut studio, s) {
                        studio.ui.message = Some((e, false));
                    }
                }
                None => warn!("no antenna sample '{id}'"),
            }
        } else if name == "reconfig-run" {
            studio.ui.page = Page::Reconfig;
            studio.run_reconfig();
        } else if name == "chain-run" {
            studio.ui.page = Page::SignalChain;
            studio.run_chain();
        } else if name == "radar-run" {
            studio.ui.page = Page::Radar;
            studio.run_radar();
        } else if name == "df-study" {
            studio.ui.page = Page::DirectionFinding;
            studio.run_df_study();
        } else if name == "gate-run" {
            if let Ok(g) = studio.project.geometry() {
                let spec = antenna_core::recipes::gate::GateSpec::for_project(&studio.project, &g);
                let project = studio.project.clone();
                studio.ui.gate.keep_spec(spec, &project);
                studio.ui.page = Page::Gate;
                studio.run_gate(spec);
            }
        } else if let Some(p) = Page::from_name(&name) {
            studio.ui.page = p;
        }
    }
    let Some(canvas) = slot.studio_rect.filter(|_| slot.studio_open && hide.0 == 0) else {
        return Ok(());
    };
    let st: &mut Studio = &mut studio;
    if std::mem::take(&mut st.ui.want_install) {
        fdtd.install();
    }
    st.ui.install_status = match fdtd.status() {
        crate::rtls::fdtd::Status::Busy(m, _) => Some(m),
        crate::rtls::fdtd::Status::Failed(e) => Some(e),
        crate::rtls::fdtd::Status::Idle => None,
    };
    let mut project = st.project.clone();
    // Array settings don't change the model, so they mustn't re-mesh it at each step of a drag.
    project.phased = Default::default();
    if st.ui.draw.dragging() {
        st.ui.refresh_shapes(&project);
    } else {
        st.ui.refresh(&project);
    }
    follow_new_result(st);
    canvas_for_page(st);
    if let Some(v) = st.ui.script_view.take() {
        st.ui.view = v;
    }
    let latest = st.latest();
    let has_result = latest.is_some();

    const M: f32 = 10.0;
    let left_w = 232.0;
    let detail_w = st.ui.detail_w.min(canvas.width() - left_w - 120.0).max(320.0);
    let left = egui::Rect::from_min_size(canvas.min + vec2(M, M), vec2(left_w, canvas.height() - 2.0 * M));
    let detail = egui::Rect::from_min_size(pos2(canvas.right() - M - detail_w, canvas.top() + M), vec2(detail_w, canvas.height() - 2.0 * M));
    let view = egui::Rect::from_min_max(pos2(left.right() + M, canvas.top()), pos2(detail.left() - M, canvas.bottom()));

    // Canvas, under the cards. On a lower order than they are, because pressing an
    // area raises it over the others of its order, and this one would hide them all.
    egui::Area::new(egui::Id::new("studio_canvas")).order(egui::Order::Background).constrain(false).fixed_pos(canvas.min).show(&ctx, |ui| {
        ui.set_min_size(canvas.size());
        ui.set_max_size(canvas.size());
        let markers = phased::canvas_model(st);
        let geom = markers.as_ref().map_or_else(|| st.ui.model.clone(), |m| Some(m.0.clone()));
        let tess = std::mem::take(&mut st.ui.tess);
        // The far field and plane the canvas needs.
        let far = phased::canvas_far(st).or_else(|| latest.as_ref().and_then(|r| canvas_far(st, r)));
        let plane = latest.as_ref().and_then(|r| r.planes.get(st.ui.plane_k.min(r.planes.len().saturating_sub(1)))).cloned();
        let frame = if st.ui.animate {
            let n = plane.as_ref().map_or(0, |p| p.frames.len());
            (n > 0).then(|| ((ui.input(|i| i.time) * 12.0) as usize) % n.max(1))
        } else {
            None
        };
        let plane_tex = plane.as_ref().filter(|_| st.ui.view == ViewMode::Field).map(|p| {
            let key = format!("studio_plane_{}", p.name);
            if frame.is_some() {
                ui.ctx().request_repaint();
            }
            plane_texture(&mut st.ui, ui.ctx(), p, &key, frame)
        });
        // On the gate page the read zone shows where tags are being judged.
        let boxes: Vec<(antenna_core::geometry::P3, antenna_core::geometry::P3, egui::Color32)> = match (st.ui.page, st.ui.gate.spec.as_ref()) {
            (Page::Gate, Some(s)) => {
                let (lo, hi) = s.aisle.zone();
                let mut list = vec![(lo, hi, theme::ACCENT)];
                list.extend(s.aisle.stray_boxes().into_iter().map(|(a, b)| (a, b, charts::GREY)));
                list
            }
            _ => Vec::new(),
        };
        let part_marks = reconfig::canvas_marks(st);
        let drawing = st.ui.page == Page::Draw && matches!(st.ui.view, ViewMode::Model | ViewMode::Mesh);
        let scene = view3d::Scene {
            part_marks: &part_marks,
            boxes: &boxes,
            drawing,
            frame: if drawing { st.ui.draw.frame } else { None },
            grid: if drawing { draw::canvas_grid(st) } else { None },
            geometry: geom.as_ref(),
            tess: markers.as_ref().map_or(&tess[..], |m| &m.1[..]),
            mom_mesh: st.ui.mom_mesh.as_ref(),
            mode: st.ui.view,
            plan: st.ui.plan.as_ref(),
            far: far.as_ref(),
            quantity: st.ui.quantity,
            pol: st.ui.pol,
            range_db: st.ui.range_db,
            currents: latest.as_ref().and_then(|r| r.currents.as_ref()),
            plane: plane.as_ref(),
            plane_tex,
            transparent: st.ui.transparent,
            show_ports: st.ui.show_ports,
        };
        let (resp, pj) = view3d::show(ui, canvas, view, &mut st.ui.cam, &scene);
        drop(scene);
        st.ui.tess = tess;
        draw::canvas(ui, st, canvas, view, &resp, &pj);
    });

    // View toolbar, over the canvas.
    egui::Area::new(egui::Id::new("studio_view_bar")).order(egui::Order::Middle).pivot(Align2::CENTER_TOP).fixed_pos(pos2(view.center().x, view.top() + M)).show(&ctx, |ui| {
        theme::floating_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mut v = st.ui.view;
                theme::segmented(ui, &mut v, &ViewMode::ALL);
                st.ui.view = v;
                ui.add_space(4.0);
                theme::icon_toggle(ui, &mut st.ui.transparent, icon::CUBE_TRANSPARENT, "See-through model");
                theme::icon_toggle(ui, &mut st.ui.show_ports, icon::PLUG, "Show ports");
                if theme::icon_button(ui, icon::FRAME_CORNERS, "Reset the view (double-click the canvas)").clicked() {
                    st.ui.cam = Camera::default();
                }
                ui.add_space(4.0);
                for (name, tip, yaw, pitch) in Camera::VIEWS {
                    if ui.add(egui::Button::new(name).frame_when_inactive(false)).on_hover_text(tip).clicked() {
                        (st.ui.cam.yaw, st.ui.cam.pitch) = (yaw, pitch);
                    }
                }
            });
        });
    });

    // Project name and how to leave, top left of the canvas.
    egui::Area::new(egui::Id::new("studio_left")).order(egui::Order::Middle).constrain(false).fixed_pos(left.min).show(&ctx, |ui| {
        theme::fixed_card(ui, left_w, left.height(), |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            let (r, _) = ui.allocate_exact_size(vec2(left_w, 44.0), egui::Sense::hover());
            ui.painter().text(r.left_top() + vec2(12.0, 7.0), Align2::LEFT_TOP, "Antenna Studio", theme::bold(15.0), theme::TEXT);
            let name = st.project.name.clone();
            let g = ui.painter().layout(name, egui::FontId::proportional(12.0), theme::WEAK, left_w - 50.0);
            ui.painter().galley(r.left_top() + vec2(12.0, 25.0), g, theme::WEAK);
            let close = egui::Rect::from_center_size(r.right_center() - vec2(20.0, 0.0), vec2(24.0, 24.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(close));
            if theme::close_button(&mut child, "Leave Antenna Design").clicked() {
                slot.studio_open = false;
            }
            theme::hairline(ui);
            nav_card(ui, st, left_w, has_result);
        });
    });

    // The selected page.
    egui::Area::new(egui::Id::new("studio_detail")).order(egui::Order::Middle).constrain(false).fixed_pos(detail.min).show(&ctx, |ui| {
        theme::fixed_card(ui, detail_w, detail.height(), |ui| {
            egui::ScrollArea::vertical().id_salt(("studio_page", st.ui.page as u8)).max_height(detail.height()).show(ui, |ui| {
                egui::Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(detail_w - 24.0);
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    status_strip(ui, st);
                    if let Some((m, good)) = st.ui.message.clone() {
                        verdict(ui, good, &m);
                    }
                    page_ui(ui, st, (&mut library, &mut applied));
                });
            });
        });
    });
    let (mut w, range) = (st.ui.detail_w, 340.0..=820.0);
    theme::width_grip(&ctx, "studio_detail", pos2(detail.left() - 5.0, detail.top() + 120.0), &mut w, range, -1.0);
    st.ui.detail_w = w;
    slot.studio_page = st.ui.page;
    publish_doc(st, &mut slot);
    Ok(())
}
