//! egui UI: top bar, tool ribbon, status bar, and over the full-bleed 3D canvas
//! the floating cards: project tree (library, simulations) on the
//! left, scene tree and nav cube on the right, detail cards beside them.

mod downloads;
mod inspector;
mod library;
pub mod loading;
mod outliner;
mod ribbon;
mod settings;
mod start;
mod statusbar;
mod tabbar;
mod topbar;
mod tree;
mod viewcube;
mod workbench;

use std::path::PathBuf;

use bevy::camera::Viewport;
use bevy::diagnostic::DiagnosticsStore;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::egui::{Align2, Margin, RichText, pos2, vec2};
use bevy_egui::{EguiContexts, egui};

use crate::library::Library;
use crate::prototype::{ProtoState, PrototypeCache};
use crate::scene::{
    Action, Actions, Handle, HandleUi, OrbitCamera, PlaceMode, PlacedObject, Selection, Status, UiBlock, ViewSettings,
};
use crate::tabs::{NewTab, Request, Tabs};
use crate::theme::{self, ACCENT, icon};

pub use settings::SettingsTab;
pub use workbench::{StudioDoc, StudioFile, Workbench};

pub type ObjectQuery<'w, 's> = Query<'w, 's, (Entity, &'static mut PlacedObject, &'static mut Transform, &'static mut Visibility)>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Local,
    Online,
}

/// Set to N to hide the viewport overlays (properties card, toolbar, banners,
/// joint dots) for the next N frames, e.g. before taking a clean screenshot.
#[derive(Resource, Default)]
pub struct HideOverlays(pub u32);

/// Progress of the positioning system, shown as the tree's status markers.
/// Published by the RTLS system each frame.
#[derive(Clone, Copy, Default)]
pub struct RtlsStatus {
    pub deploy: theme::Dot,
    pub test: theme::Dot,
    pub ems: theme::Dot,
    pub fdtd: theme::Dot,
    pub devices: usize,
    pub zones: usize,
}

/// A long-running task for the "Job status" card.
pub struct Job {
    pub text: String,
    /// 0..1, or negative when unknown.
    pub progress: f32,
}

/// What the UI and the systems that draw their own cards (positioning, plant)
/// tell each other: where a card goes, and what the trees should show.
#[derive(Resource, Default)]
pub struct LeftPanelSlot {
    /// The workbench in front.
    pub bench: workbench::Benches,
    /// Room for the positioning card this frame (its top-left and largest size).
    pub rtls: Option<egui::Rect>,
    /// The positioning card is open.
    pub rtls_open: bool,
    /// Active RTLS alarms, set every frame by the RTLS system.
    pub rtls_alerts: u32,
    /// Asks for the Simulations panel.
    pub open_simulations: bool,
    /// A simulation result is on show: the empty-scene hint would cover it.
    pub results_shown: bool,
    pub rtls_status: RtlsStatus,
    /// Part of the canvas no tree or detail card covers; other cards anchor to it.
    pub free: Option<egui::Rect>,
    /// The same, ignoring the properties card (which only covers the top).
    pub between: Option<egui::Rect>,
    /// Jobs started by other systems.
    pub jobs: Vec<Job>,
    /// A plant is in the scene, and whether it runs.
    pub plant: Option<bool>,
    pub plant_toggle: bool,
    /// Antenna Studio covers the canvas; the scene's cards step aside.
    pub studio_open: bool,
    /// The canvas Antenna Studio may fill this frame.
    pub studio_rect: Option<egui::Rect>,
    /// Scripted Studio steps (`--open antenna-<page>`), in order.
    pub studio_goto: Vec<String>,
    /// Asked of the open antenna project from the File menu.
    pub studio_file: Vec<workbench::StudioFile>,
    /// A new name for the antenna project, typed in the top bar.
    pub studio_rename: Option<String>,
    /// The open antenna project, for the top bar.
    pub studio_doc: workbench::StudioDoc,
    /// The Factory workbench's card.
    pub twin: crate::twin::TwinSlot,
    /// The page Antenna Studio shows, for the ribbon.
    #[cfg(feature = "antenna-studio")]
    pub studio_page: crate::antenna::ui::Page,
}

/// Which parts of the project trees are expanded.
pub struct Sections {
    library: bool,
    /// The workbench's own part of the tree.
    bench: bool,
    jobs: bool,
    scene: bool,
    scene_root: bool,
}

impl Default for Sections {
    fn default() -> Self {
        Self { library: true, bench: true, jobs: false, scene: true, scene_root: true }
    }
}

#[derive(Resource)]
pub struct UiState {
    layout_path: Option<PathBuf>,
    sections: Sections,
    /// The list under the top bar's Downloads button is showing.
    downloads_open: bool,
    /// Where that button is, for the list to hang from.
    downloads_anchor: Option<egui::Rect>,
    left_w: f32,
    right_w: f32,
    detail_w: f32,
    filter: Filter,
    /// Library shows only assets with moving parts.
    movable_only: bool,
    list_view: bool,
    scene_search: String,
    settings_open: bool,
    settings_tab: SettingsTab,
    ribbon_tab: ribbon::RibbonTab,
    /// The workbench the last frame drew, to notice a change.
    shown_bench: Workbench,
    /// 0: nothing applied, 1: fonts loaded, 2: style applied.
    theme_stage: u8,
    logo: Option<egui::TextureHandle>,
    /// Start screen (create / open / sample scene) is showing.
    start_open: bool,
    start: start::StartState,
    scene_name: String,
    /// What the start screen's name field holds.
    start_name: String,
    /// The tab waiting for its "save changes?" answer.
    closing: Option<u64>,
    /// Sample to load as soon as the UI runs (`--sample N`).
    start_sample: Option<usize>,
    /// Frames since a sample was requested; frames the camera once it has loaded.
    frame_after_load: Option<u32>,
    /// `--open`: cards and dialogs to open once the UI runs (scripted screenshots).
    script: Vec<String>,
    /// Nav cube: the (yaw, pitch) the camera is gliding to.
    view_target: Option<(f32, f32)>,
    /// The selected object as it was when selected, for the card's discard button.
    edit_base: Option<inspector::EditBase>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            layout_path: None,
            sections: Sections::default(),
            downloads_open: false,
            downloads_anchor: None,
            left_w: 284.0,
            right_w: 262.0,
            detail_w: 420.0,
            filter: Filter::All,
            movable_only: false,
            list_view: false,
            scene_search: String::new(),
            settings_open: false,
            settings_tab: SettingsTab::General,
            ribbon_tab: ribbon::RibbonTab::default(),
            shown_bench: Workbench::default(),
            theme_stage: 0,
            logo: None,
            start_open: true,
            start: start::StartState::default(),
            scene_name: String::new(),
            start_name: String::new(),
            closing: None,
            start_sample: None,
            frame_after_load: None,
            script: Vec::new(),
            view_target: None,
            edit_base: None,
        }
    }
}

impl UiState {
    fn open_settings(&mut self, tab: SettingsTab) {
        self.settings_open = true;
        self.settings_tab = tab;
    }

    /// The current scene's name, file and pending framing; tabs exchange these.
    pub(crate) fn scene_fields(&mut self) -> (&mut String, &mut Option<PathBuf>, &mut Option<u32>) {
        self.view_target = None;
        self.edit_base = None;
        (&mut self.scene_name, &mut self.layout_path, &mut self.frame_after_load)
    }

    pub(crate) fn scene_title(&self) -> &str {
        &self.scene_name
    }

    pub(crate) fn name_scene(&mut self, name: String) {
        self.scene_name = name;
    }

    /// Frame the camera on the scene once it has loaded.
    pub(crate) fn frame_on_load(&mut self) {
        self.frame_after_load = Some(0);
    }

    /// The last tab closed: there is nothing to go back to.
    pub(crate) fn ask_for_scene(&mut self) {
        self.start_open = true;
        self.start_name.clear();
        self.start = start::StartState::default();
    }

    /// Goes straight to the editor (scripted runs), optionally loading a sample.
    pub fn skip_start(&mut self, sample: Option<usize>) {
        self.start_open = false;
        self.start_sample = sample.filter(|&i| i < crate::samples::SAMPLES.len());
    }

    /// Cards and dialogs to open once the UI runs (`--open`, for scripted screenshots).
    pub fn open_at_start(&mut self, names: Vec<String>) {
        self.script = names;
    }

    fn run_script(&mut self, slot: &mut LeftPanelSlot, rtls: &mut crate::rtls::ui::RtlsUiState, agent: &mut crate::agent::Agent) {
        use crate::rtls::ui::{Page, Tab};
        for name in std::mem::take(&mut self.script) {
            let page = match name.as_str() {
                "deploy" => Some(Page::Deploy),
                "test" => Some(Page::Test),
                "devices" => Some(Page::Tool(Tab::Devices)),
                "inspector" => Some(Page::Tool(Tab::Inspector)),
                "live" => Some(Page::Tool(Tab::Live)),
                "rf" => Some(Page::Tool(Tab::Rf)),
                "planning" => Some(Page::Tool(Tab::Planning)),
                "outputs" => Some(Page::Tool(Tab::Outputs)),
                "rtls-settings" => Some(Page::Tool(Tab::Settings)),
                "openems" => Some(Page::OpenEms),
                "fdtd" => Some(Page::Fdtd),
                "predictions" => Some(Page::Outcomes),
                _ => None,
            };
            if let Some(p) = page {
                rtls.local.set_page(p);
                slot.rtls_open = true;
                continue;
            }
            if let Some(bench) = name.strip_prefix("bench-").and_then(Workbench::from_name) {
                workbench::switch(slot, rtls, bench);
                continue;
            }
            let tab = match name.as_str() {
                "settings" => Some(SettingsTab::General),
                "settings-viewport" => Some(SettingsTab::Viewport),
                "settings-graphics" => Some(SettingsTab::Graphics),
                "settings-performance" => Some(SettingsTab::Performance),
                "settings-assistant" => Some(SettingsTab::Assistant),
                _ => None,
            };
            if let Some(t) = tab {
                self.open_settings(t);
                continue;
            }
            if name == "antenna" {
                slot.studio_open = true;
                continue;
            }
            // Scripted screenshots of the Factory workbench.
            if let Some(what) = name.strip_prefix("twin-") {
                use crate::twin::{TwinPage, TwinRequest};
                match what {
                    "demo" => slot.twin.requests.push(TwinRequest::Demo(Some(crate::twin::DEMO_SIZES[0]))),
                    "demo-500" => slot.twin.requests.push(TwinRequest::Demo(Some(500))),
                    "demo-5000" => slot.twin.requests.push(TwinRequest::Demo(Some(5000))),
                    "lines" => slot.twin.page = TwinPage::Lines,
                    "events" => slot.twin.page = TwinPage::Events,
                    "connection" => slot.twin.page = TwinPage::Connection,
                    _ => {}
                }
                continue;
            }
            if let Some(page) = name.strip_prefix("antenna-") {
                slot.studio_goto.push(page.to_string());
                continue;
            }
            match name.as_str() {
                "assistant" => agent.open = true,
                "downloads" => self.downloads_open = true,
                "jobs" => self.sections.jobs = true,
                "start-empty" => self.start.select_empty(),
                "start-antenna" => self.start.select_antenna(),
                other => {
                    if let Some(i) = other.strip_prefix("start-antenna-").and_then(|n| n.parse::<usize>().ok()) {
                        self.start.select_antenna_sample(i.saturating_sub(1));
                        continue;
                    }
                    if let Some(i) = other.strip_prefix("start-").and_then(|n| n.parse::<usize>().ok()) {
                        self.start.select_sample(i.saturating_sub(1));
                    }
                }
            }
        }
    }

    /// Opens sample `i` in a tab.
    fn open_sample(&mut self, i: usize, name: Option<String>, library: &Library, tabs: &mut Tabs, status: &mut Status) {
        match crate::samples::write_layout(i, library) {
            Ok((path, missing)) => {
                let title = name.unwrap_or_else(|| crate::samples::SAMPLES[i].title.to_string());
                // Saving asks for a location instead of overwriting the sample.
                tabs.request(Request::Open(vec![NewTab { title, path: None, load: Some(path) }]));
                if !missing.is_empty() {
                    status.0 = format!("Downloading {} asset(s) for this sample: {}", missing.len(), missing.join(", "));
                }
            }
            Err(e) => status.0 = format!("Could not create the sample: {e:#}"),
        }
    }
}

fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(include_bytes!("../../assets/logo_small.png")).ok()?.thumbnail(64, 64).to_rgba8();
    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    Some(ctx.load_texture("logo", ci, egui::TextureOptions::LINEAR))
}

/// Opens the file the app was started with, such as a double-clicked project. Saving goes back to it.
pub(crate) fn open_at_launch(world: &mut World, path: PathBuf) {
    let mut state = world.resource_mut::<UiState>();
    state.name_scene(crate::tabs::title_of(&path));
    state.layout_path = Some(path.clone());
    state.frame_on_load();
    world.resource_mut::<crate::undo::UndoStack>().rebase();
    world.resource_mut::<Actions>().0.push(Action::LoadLayout(path));
}

fn pick_layout_save(scene_name: &str) -> Option<PathBuf> {
    let file = format!("{}.{}", crate::samples::slug(scene_name), crate::project_file::EXTENSION);
    rfd::FileDialog::new().add_filter("Sense Sim project", &[crate::project_file::EXTENSION]).set_file_name(file).save_file().map(|p| crate::project_file::save_path(&p))
}

/// How the app stands, for the assistant, which can't see these panels.
#[allow(clippy::too_many_arguments)]
fn what_is_on_screen(
    slot: &LeftPanelSlot,
    rtls: &crate::rtls::ui::RtlsUiState,
    state: &UiState,
    place: &PlaceMode,
    walls: &crate::wall_tool::WallTool,
    device_place: &crate::rtls::DevicePlace,
    selection: &Selection,
    objects: &ObjectQuery,
    object_count: usize,
    unsaved: bool,
    edits: u64,
) -> crate::agent::screen::Screen {
    let name_of = |e: Entity| objects.get(e).ok().map(|(_, o, _, _)| o.name.clone());
    let mut selected: Vec<String> = selection.selected.and_then(name_of).into_iter().collect();
    selected.extend(selection.group.iter().filter(|e| Some(**e) != selection.selected).filter_map(|e| name_of(*e)));

    #[cfg(feature = "antenna-studio")]
    let studio_view = || format!("Antenna Studio, the {:?} page", slot.studio_page);
    #[cfg(not(feature = "antenna-studio"))]
    let studio_view = || "Antenna Studio".to_string();
    let view = if slot.studio_open {
        Some(studio_view())
    } else if slot.rtls_open {
        Some(format!("the {} card", rtls.local.page().title()))
    } else {
        None
    };
    let project = if slot.studio_open {
        Some(slot.studio_doc.name.clone()).filter(|n| !n.is_empty())
    } else {
        None
    };
    let tool = if walls.active {
        Some("drawing walls".to_string())
    } else if let Some(asset) = &place.asset {
        Some(format!("placing {}", asset.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "an asset".into())))
    } else if let Some(profile) = &device_place.profile {
        Some(format!("placing {profile} devices"))
    } else if device_place.waypoints_for.is_some() {
        Some("adding path waypoints to a tag".to_string())
    } else {
        None
    };
    crate::agent::screen::Screen {
        workbench: slot.bench.current.name().to_string(),
        view,
        project,
        scene: state.scene_name.clone(),
        file: state.layout_path.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string()),
        unsaved,
        objects: object_count,
        selected,
        tool,
        jobs: slot.jobs.iter().map(|j| if j.progress >= 0.0 { format!("{} {:.0}%", j.text, j.progress * 100.0) } else { j.text.clone() }).collect(),
        edits,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ui_system(
    mut contexts: EguiContexts,
    mut library: ResMut<Library>,
    mut place: ResMut<PlaceMode>,
    selection: Res<Selection>,
    mut actions: ResMut<Actions>,
    mut settings: ResMut<ViewSettings>,
    mut cache: ResMut<PrototypeCache>,
    (mut status, mut block, mut state, pending, profiles, mut device_place): (
        ResMut<Status>,
        ResMut<UiBlock>,
        ResMut<UiState>,
        Res<crate::scene::PendingSpawns>,
        Res<crate::rtls::device::Profiles>,
        ResMut<crate::rtls::DevicePlace>,
    ),
    (handles, mut undo_req, undo_stack, mut hide, mut slot, mut rtls): (
        Res<HandleUi>,
        ResMut<crate::undo::UndoRequest>,
        Res<crate::undo::UndoStack>,
        ResMut<HideOverlays>,
        ResMut<LeftPanelSlot>,
        ResMut<crate::rtls::ui::RtlsUiState>,
    ),
    mut objects: ObjectQuery,
    mut camera: Single<(&mut Camera, &mut OrbitCamera, &Projection)>,
    window: Single<&Window, With<PrimaryWindow>>,
    diagnostics: Res<DiagnosticsStore>,
    mut walls: ResMut<crate::wall_tool::WallTool>,
    (mut agent, mut catalog, bridge, mut gpu, render_stats, mut tabs): (
        ResMut<crate::agent::Agent>,
        ResMut<crate::agent::catalog::Catalog>,
        Res<crate::agent::tools::ToolBridge>,
        ResMut<crate::gpu::Gpu>,
        Res<crate::lod::RenderStats>,
        ResMut<Tabs>,
    ),
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let overlays = hide.0 == 0 && !slot.canvas_taken();
    hide.0 = hide.0.saturating_sub(1);
    slot.rtls = None;
    slot.twin.rect = None;
    match state.theme_stage {
        0 => {
            theme::apply_fonts(&ctx);
            state.logo = load_logo(&ctx);
            state.theme_stage = 1;
            // The fonts exist from the next frame; nothing can name the bold family yet.
            return Ok(());
        }
        1 => {
            theme::apply_style(&ctx);
            state.theme_stage = 2;
        }
        _ => {}
    }
    state.run_script(&mut slot, &mut rtls, &mut agent);
    workbench::follow(&mut slot, &mut rtls);
    let (messages, to_place, rescanned) = library.update(&ctx);
    if let Some(m) = messages.into_iter().last() {
        status.0 = m;
    }
    if let Some(p) = to_place.into_iter().last() {
        place.asset = Some(p);
    }
    if rescanned {
        // Files may have arrived since a load failed; let it try again.
        cache.states.retain(|_, s| !matches!(s, ProtoState::Failed(_)));
    }

    if let Some(i) = state.start_sample.take() {
        state.open_sample(i, None, &library, &mut tabs, &mut status);
    }
    // Frame the camera on a freshly loaded sample once everything has arrived.
    if let Some(n) = state.frame_after_load {
        if n > 3 && pending.0.is_empty() && cache.loading_count() == 0 {
            actions.0.push(Action::FrameAll);
            state.frame_after_load = None;
        } else {
            state.frame_after_load = Some(n + 1);
        }
    }

    // Global shortcuts that egui owns (scene shortcuts live in scene.rs / undo.rs).
    let mut flags = ctx.input_mut(|i| {
        let save_as = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::S));
        let save = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::S));
        let open = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::O));
        let new = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::N))
            | i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::T));
        let close = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::W));
        // Longest modifier set first: Ctrl+Tab would also match Ctrl+Shift+Tab.
        if i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Tab)) {
            tabs.request(Request::Step(-1));
        } else if i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Tab)) {
            tabs.request(Request::Step(1));
        }
        topbar::Flags { new, open, save, save_as, close }
    });

    let mut root = egui::Ui::new(
        ctx.clone(),
        "root".into(),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()),
    );

    // ---------------- Bars
    topbar::show(&mut root, &mut state, &mut library, &mut agent, &mut actions, &mut settings, &mut cache, &mut undo_req, &undo_stack, &selection, &mut slot, &mut flags);
    ribbon::show(&mut root, &mut state, &mut place, &mut walls, &undo_stack, &mut undo_req, &selection, &mut actions, &mut settings, &mut slot, &mut rtls, &mut flags, &mut agent.open);
    // A tool left armed would keep acting on a workbench that doesn't show it.
    if state.shown_bench != slot.bench.current {
        state.shown_bench = slot.bench.current;
        state.ribbon_tab = ribbon::RibbonTab::Tools;
        walls.active = false;
        place.asset = None;
        device_place.profile = None;
        device_place.waypoints_for = None;
    }
    if agent.running {
        state.closing = None;
    }
    if !slot.studio_open {
        tabbar::show(&mut root, &mut tabs, &mut state, &mut flags);
    }
    workbench::route_file_commands(&mut flags, &mut slot);
    tabbar::close_if_asked(&mut tabs, &mut state, &flags);

    // ---------------- Start screen: what to work on. A blank scene is always there to fall back on.
    if flags.new && !state.start_open {
        state.start_open = true;
        state.start_name.clear();
        state.start = start::StartState::default();
    }
    if state.start_open {
        let mut name = std::mem::take(&mut state.start_name);
        let mut st = std::mem::take(&mut state.start);
        let choice = start::show(&ctx, &mut st, &mut library, &mut name, slot.shows(Workbench::Antenna));
        state.start = st;
        state.start_name = name;
        let typed = Some(state.start_name.trim().to_string()).filter(|t| !t.is_empty());
        match choice {
            Some(start::Choice::Empty) => {
                let title = typed.unwrap_or_else(|| "Untitled scene".into());
                tabs.request(Request::Open(vec![NewTab { title, path: None, load: None }]));
                state.start_open = false;
            }
            Some(start::Choice::Sample(i)) => {
                state.open_sample(i, typed, &library, &mut tabs, &mut status);
                state.start_open = false;
            }
            Some(start::Choice::Antenna) => {
                workbench::switch(&mut slot, &mut rtls, Workbench::Antenna);
                slot.studio_goto.push("gallery".to_string());
                state.start_open = false;
            }
            Some(start::Choice::AntennaSample(i)) => {
                workbench::switch(&mut slot, &mut rtls, Workbench::Antenna);
                if let Some(s) = antenna_core::samples::all().get(i) {
                    slot.studio_goto.push(format!("sample-{}", s.id));
                }
                state.start_open = false;
            }
            Some(start::Choice::OpenAntenna) => {
                workbench::switch(&mut slot, &mut rtls, Workbench::Antenna);
                slot.studio_file.push(workbench::StudioFile::Open);
                state.start_open = false;
            }
            Some(start::Choice::Open) => flags.open = true,
            Some(start::Choice::Cancel) => state.start_open = false,
            None => {}
        }
        // The scene's own shortcuts wait until a scene exists.
        flags.save = false;
        flags.save_as = false;
    }

    if flags.open && let Some(files) = rfd::FileDialog::new().add_filter("Sense Sim project", &[crate::project_file::EXTENSION]).add_filter("Older layout", &["json"]).pick_files() {
        let new = files.into_iter().map(|p| NewTab { title: crate::tabs::title_of(&p), path: Some(p.clone()), load: Some(p) }).collect();
        tabs.request(Request::Open(new));
        state.start_open = false;
    }
    if flags.save || flags.save_as {
        let name = state.scene_name.clone();
        // An older `.json` layout saves as a `.sesim` beside it, and stays as it was.
        let p = if flags.save_as { pick_layout_save(&name) } else { state.layout_path.as_deref().map(crate::project_file::save_path).or_else(|| pick_layout_save(&name)) };
        if let Some(p) = p {
            state.layout_path = Some(p.clone());
            actions.0.push(Action::SaveLayout(p));
        }
    }

    // ---------------- Status bar
    let object_count = objects.iter().count();
    // A sample or a new scene has no file yet, so anything in it would be lost.
    let unsaved = undo_stack.dirty() || (state.layout_path.is_none() && object_count > 0);
    tabs.mirror(&state.scene_name, state.layout_path.as_ref(), unsaved);
    agent.project_dir = state.layout_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let on_screen = what_is_on_screen(&slot, &rtls, &state, &place, &walls, &device_place, &selection, &objects, object_count, unsaved, undo_stack.revision());
    agent.observe_screen(on_screen);
    let selected_name = selection.selected.and_then(|e| objects.get(e).ok()).map(|(_, o, _, _)| o.name.clone());
    let selected_more = selection.group.iter().filter(|e| Some(**e) != selection.selected).count();
    // Nothing of the scene is drawn while a workspace has the canvas, so "0 of N triangles" would only mislead.
    let shown_stats = if slot.canvas_taken() { crate::lod::RenderStats::default() } else { *render_stats };
    statusbar::show(&mut root, &diagnostics, &mut library, &cache, &mut status, &mut state, object_count, shown_stats, selected_name.as_deref(), selected_more, !slot.canvas_taken());

    // ---------------- AI assistant (docked on the right)
    if crate::agent::panel::show(&mut root, &ctx, &mut agent, &mut library, &bridge) {
        state.open_settings(SettingsTab::Assistant);
    }

    // ---------------- Viewport = everything the bars leave; cards float over it
    let central = root.available_rect_before_wrap();
    slot.studio_rect = Some(central);
    let scale = window.scale_factor();
    let pos = (central.min.to_vec2() * scale).max(egui::Vec2::ZERO);
    let size = (central.size() * scale).max(egui::vec2(1.0, 1.0));
    let max = egui::vec2(window.physical_width() as f32, window.physical_height() as f32);
    let size = size.min(max - pos).max(egui::vec2(1.0, 1.0));
    let (ref mut cam, ref mut orbit, projection) = *camera;
    // These have their own canvas: no point rendering the scene behind them.
    cam.is_active = !slot.canvas_taken();
    cam.viewport = Some(Viewport {
        physical_position: UVec2::new(pos.x as u32, pos.y as u32),
        physical_size: UVec2::new(size.x as u32, size.y as u32),
        ..default()
    });

    // ---------------- Where the cards go
    const M: f32 = 10.0;
    let wide = central.width() > 720.0;
    let left_rect = egui::Rect::from_min_size(central.min + vec2(M, M), vec2(state.left_w, central.height() - 2.0 * M));
    let right_rect = egui::Rect::from_min_max(pos2(central.right() - M - state.right_w, central.top() + M), pos2(central.right() - M, central.bottom() - viewcube::HEIGHT - 6.0));
    let mut free = egui::Rect::from_min_max(pos2(left_rect.right() + M, central.top()), pos2(if wide { right_rect.left() - M } else { central.right() }, central.bottom()));

    // ---------------- Project tree (left), positioning card beside it
    if overlays {
        tree::left_card(&ctx, left_rect, &mut state, &mut library, &mut place, &mut cache, (&profiles, &mut device_place), &mut slot, &mut rtls, agent.running);
        let (mut w, range) = (state.left_w, 230.0..=480.0);
        theme::width_grip(&ctx, "left", pos2(left_rect.right() + 5.0, left_rect.center().y), &mut w, range, 1.0);
        state.left_w = w;
    }
    if overlays && slot.rtls_open {
        let w = state.detail_w.min(free.width() - M).max(300.0);
        slot.rtls = Some(egui::Rect::from_min_size(pos2(free.left(), central.top() + M), vec2(w, central.height() - 2.0 * M)));
        let (mut dw, range) = (state.detail_w, 320.0..=760.0);
        theme::width_grip(&ctx, "detail", pos2(free.left() + w + 5.0, central.top() + M + 120.0), &mut dw, range, 1.0);
        state.detail_w = dw;
        free.min.x += w + M;
    }

    if overlays && slot.twin.open && slot.bench.current == Workbench::Factory {
        let w = state.detail_w.min(free.width() - M).max(300.0);
        slot.twin.rect = Some(egui::Rect::from_min_size(pos2(free.left(), central.top() + M), vec2(w, central.height() - 2.0 * M)));
        let (mut dw, range) = (state.detail_w, 320.0..=760.0);
        theme::width_grip(&ctx, "twin_detail", pos2(free.left() + w + 5.0, central.top() + M + 120.0), &mut dw, range, 1.0);
        state.detail_w = dw;
        free.min.x += w + M;
    }

    // ---------------- Scene tree (right), properties card beside it
    if overlays && wide {
        outliner::card(&ctx, right_rect, &mut objects, &selection, &mut actions, &mut state);
        let (mut w, range) = (state.right_w, 200.0..=440.0);
        theme::width_grip(&ctx, "right", pos2(right_rect.left() - 5.0, right_rect.top() + 120.0), &mut w, range, -1.0);
        state.right_w = w;
    }
    slot.between = Some(free);
    let selected = selection.selected.is_some_and(|e| objects.contains(e));
    if overlays && selected {
        let w = 330.0f32.min(free.width() - 2.0 * M).max(220.0);
        egui::Area::new(egui::Id::new("properties_card"))
            .order(egui::Order::Middle)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(pos2(free.right(), central.top() + M))
            .constrain_to(central)
            .show(&ctx, |ui| {
                theme::fixed_card(ui, w, central.height() - 2.0 * M, |ui| {
                    inspector::show(ui, &mut objects, &selection, &handles, &mut actions, &mut state.edit_base);
                });
            });
        free.max.x -= w + M;
    } else {
        state.edit_base = None;
    }
    slot.free = Some(free);

    // ---------------- View toolbar (top centre of the free canvas)
    let toolbar_x = free.center().x;
    if overlays && !state.start_open {
        egui::Area::new(egui::Id::new("viewport_toolbar"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(pos2(toolbar_x, central.top() + M))
            .constrain_to(central)
            .show(&ctx, |ui| {
                theme::floating_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if theme::icon_button(ui, icon::FRAME_CORNERS, "Frame all (F)").clicked() {
                            actions.0.push(Action::FrameAll);
                        }
                        theme::icon_toggle(ui, &mut settings.grid, icon::GRID_FOUR, "Grid");
                        theme::icon_toggle(ui, &mut settings.bounds, icon::BOUNDING_BOX, "Hover bounds");
                        theme::icon_toggle(ui, &mut settings.shadows, icon::SUN, "Shadows");
                        ui.add_space(2.0);
                        let (sep, _) = ui.allocate_exact_size(vec2(1.0, 18.0), egui::Sense::hover());
                        ui.painter().rect_filled(sep, 0.0, theme::STROKE);
                        ui.add_space(2.0);
                        if theme::icon_button(ui, icon::SLIDERS_HORIZONTAL, "Lighting & viewport settings").clicked() {
                            state.open_settings(SettingsTab::Viewport);
                        }
                    });
                });
            });
    }

    // ---------------- Placing banner
    let placing = place.ghost.as_ref().map(|(_, a)| (a.clone(), true)).or_else(|| place.asset.clone().map(|a| (a, false)));
    if let Some((asset, ready)) = placing.filter(|_| overlays) {
        egui::Area::new(egui::Id::new("placing_banner"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(pos2(toolbar_x, central.top() + 58.0))
            .constrain_to(central)
            .show(&ctx, |ui| {
                theme::floating_frame().inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let name = asset_name(&asset);
                        if ready {
                            ui.label(RichText::new(icon::CURSOR_CLICK).color(ACCENT));
                            ui.label(RichText::new(format!("Placing {name}")).strong());
                            if place.blocked {
                                ui.label(RichText::new("overlaps another object · move to a free spot").small().color(theme::DANGER));
                            } else {
                                ui.label(RichText::new("click to drop · Q/E rotate · Shift keep placing").small().color(theme::WEAK));
                            }
                        } else {
                            ui.spinner();
                            ui.label(format!("Loading {name}…"));
                        }
                        if theme::icon_button(ui, icon::X, "Cancel (Esc)").clicked() {
                            place.asset = None;
                        }
                    });
                });
            });
    }

    // ---------------- Wall drawing banner
    if walls.active && overlays {
        egui::Area::new(egui::Id::new("wall_banner"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(pos2(toolbar_x, central.top() + 58.0))
            .constrain_to(central)
            .show(&ctx, |ui| {
                theme::floating_frame().inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::WALL).color(ACCENT));
                        ui.label(RichText::new("Drawing walls").strong());
                        ui.label(RichText::new("Height").small().color(theme::WEAK));
                        ui.add(egui::DragValue::new(&mut walls.height).speed(0.05).range(0.5..=20.0).fixed_decimals(2).suffix(" m"));
                        ui.label(RichText::new("Thickness").small().color(theme::WEAK));
                        ui.add(egui::DragValue::new(&mut walls.thickness).speed(0.01).range(0.05..=1.0).fixed_decimals(2).suffix(" m"));
                        let hint = match walls.segment_length() {
                            Some(l) => format!("{l:.2} m · click to place · Esc ends the run"),
                            None => "click a start point on the floor".into(),
                        };
                        ui.label(RichText::new(hint).small().color(theme::WEAK));
                        if ui.button("Done").clicked() {
                            walls.active = false;
                        }
                    });
                });
            });
    }

    // ---------------- Empty scene hint
    let page_needs_scene = !slot.rtls_open || !matches!(rtls.local.page(), crate::rtls::ui::Page::Fdtd | crate::rtls::ui::Page::Outcomes);
    if overlays && object_count == 0 && place.asset.is_none() && !walls.active && !slot.results_shown && page_needs_scene && slot.bench.current != Workbench::Factory {
        egui::Area::new(egui::Id::new("empty_scene"))
            .order(egui::Order::Background)
            .pivot(Align2::CENTER_CENTER)
            .fixed_pos(free.center())
            .interactable(false)
            .show(&ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(icon::CUBE).size(36.0).color(theme::FAINT));
                    ui.label(RichText::new("Your scene is empty").size(19.0).italics().color(theme::WEAK));
                    ui.label(RichText::new("Pick an asset from the library, or ask the assistant to build one.").small().color(theme::WEAK));
                });
            });
    }

    // ---------------- Joint overlay: rings on each moving part, label on the hot one
    let painter = ctx
        .layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("joint_overlay")))
        .with_clip_rect(central);
    for m in handles.joints.iter().filter(|_| overlays) {
        let p = pos2(m.screen.x, m.screen.y);
        let anchor = pos2(m.anchor.x, m.anchor.y);
        let h = Handle::Joint(m.index);
        let hot = handles.active == Some(h) || handles.hovered == Some(h);
        if p.distance(anchor) > 1.5 {
            // Dot was moved off an overlapping one: tie it to the real joint.
            painter.line_segment([anchor, p], egui::Stroke::new(1.0, theme::WEAK));
            painter.circle_filled(anchor, 1.8, theme::INK);
        }
        if hot {
            painter.circle_filled(p + vec2(1.0, 1.5), 8.0, egui::Color32::from_black_alpha(70));
            painter.circle_filled(p, 8.0, ACCENT);
            painter.circle_stroke(p, 8.0, egui::Stroke::new(2.0, egui::Color32::WHITE));
            let galley = painter.layout_no_wrap(m.label.clone(), egui::FontId::monospace(12.5), theme::TEXT);
            let rect = egui::Rect::from_min_size(p + vec2(16.0, -galley.size().y / 2.0 - 5.0), galley.size() + vec2(16.0, 10.0));
            painter.rect_filled(rect, 5.0, theme::PANEL);
            painter.rect_stroke(rect, 5.0, egui::Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
            painter.galley(rect.min + vec2(8.0, 5.0), galley, theme::TEXT);
        } else {
            // Small, see-through dot so the part under it stays visible; the
            // dark rim keeps it readable on light surfaces.
            painter.circle_filled(p, 5.0, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 90));
            painter.circle_stroke(p, 6.0, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(120)));
            painter.circle_stroke(p, 5.0, egui::Stroke::new(2.0, egui::Color32::WHITE));
        }
    }
    if overlays && let Some(c) = handles.compact {
        painter.text(pos2(c.x, c.y + 14.0), Align2::CENTER_TOP, "Small object · press F to zoom", egui::FontId::proportional(12.0), theme::TEXT);
    }

    // ---------------- Scale bar and nav cube
    viewcube::animate(&ctx, orbit, &mut state.view_target);
    if overlays {
        if let Projection::Perspective(p) = projection {
            viewcube::scale_bar(&ctx, central, orbit.radius, p.fov);
        }
        viewcube::show(&ctx, central, orbit, &mut state.view_target, &mut actions);
    }

    // ---------------- Settings dialog
    if state.settings_open {
        let mut tab = state.settings_tab;
        let open = settings::show(&ctx, &mut tab, &mut library, &mut settings, &mut cache, &mut agent, &mut catalog, &mut gpu);
        state.settings_tab = tab;
        state.settings_open = open;
    }

    tabbar::confirm_close(&ctx, &mut tabs, &mut state, &mut actions);

    downloads::dropdown(&ctx, &mut state, &mut library, slot.bench.current.has_library());

    let over_ui = ctx.pointer_latest_pos().map(|p| !central.contains(p) || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background)).unwrap_or(true);
    // The Studio's canvas is on the background order, but the scene isn't what's under it.
    block.pointer = over_ui || ctx.egui_is_using_pointer() || state.start_open || slot.studio_open;
    // A full-canvas workspace has its own keys: Delete there mustn't reach the scene.
    block.keyboard = ctx.egui_wants_keyboard_input() || state.start_open || slot.canvas_taken();
    Ok(())
}

/// Asset folder name for a USD path (`<root>/<folder>/<sub>/<file>.usd`), or the file stem.
pub fn asset_name(asset: &std::path::Path) -> String {
    asset
        .ancestors()
        .nth(2)
        .and_then(|p| p.file_name())
        .or_else(|| asset.file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod open_at_launch_tests {
    use super::*;

    #[test]
    fn a_project_opened_at_launch_saves_back_to_its_own_file() {
        let mut w = World::new();
        w.init_resource::<UiState>();
        w.init_resource::<crate::undo::UndoStack>();
        w.init_resource::<Actions>();
        open_at_launch(&mut w, PathBuf::from(r"C:\plans\dock.sesim"));
        let state = w.resource::<UiState>();
        assert_eq!(state.layout_path.as_deref(), Some(std::path::Path::new(r"C:\plans\dock.sesim")));
        assert_eq!(state.scene_title(), "dock");
        assert_eq!(state.frame_after_load, Some(0), "it opens framed, not at the default close-up");
        assert!(matches!(w.resource::<Actions>().0.as_slice(), [Action::LoadLayout(p)] if p.ends_with("dock.sesim")));
    }
}