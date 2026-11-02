//! egui UI: top bar, left rail (library / scene / downloads), properties,
//! status bar, settings dialog, viewport overlays.

mod downloads;
mod inspector;
mod library;
pub mod loading;
mod outliner;
mod settings;
mod start;
mod statusbar;

use std::path::PathBuf;

use bevy::camera::Viewport;
use bevy::diagnostic::DiagnosticsStore;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::egui::{Align, Align2, Layout, Margin, RichText, vec2};
use bevy_egui::{EguiContexts, egui};

use crate::library::Library;
use crate::prototype::{ProtoState, PrototypeCache};
use crate::scene::{
    Action, Actions, Handle, HandleUi, OrbitCamera, PlaceMode, PlacedObject, Selection, Status, UiBlock, ViewSettings,
};
use crate::theme::{self, ACCENT, icon};

pub use settings::SettingsTab;

pub type ObjectQuery<'w, 's> = Query<'w, 's, (Entity, &'static mut PlacedObject, &'static mut Transform)>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LeftTab {
    Library,
    Scene,
    Downloads,
    Rtls,
}

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

/// The left panel area handed to systems that draw their own tab content
/// after `ui_system` (the RTLS panel). `rtls` is the content rect while the
/// RTLS tab is showing; set `open_rtls` to switch to that tab.
#[derive(Resource, Default)]
pub struct LeftPanelSlot {
    pub rtls: Option<egui::Rect>,
    pub open_rtls: bool,
    /// Active RTLS alarms, set every frame by the RTLS system; shown as a red dot on the rail.
    pub rtls_alerts: u32,
    /// Asks for the Design workspace; the design system takes it from here.
    pub open_design: bool,
    /// Asks for the Simulations panel.
    pub open_simulations: bool,
    /// A simulation result is on show: the empty-scene hint would cover it.
    pub results_shown: bool,
}

#[derive(Resource)]
pub struct UiState {
    layout_path: Option<PathBuf>,
    left_tab: LeftTab,
    left_open: bool,
    filter: Filter,
    /// Library shows only assets with moving parts.
    movable_only: bool,
    list_view: bool,
    scene_search: String,
    settings_open: bool,
    settings_tab: SettingsTab,
    /// 0: nothing applied, 1: fonts loaded, 2: style applied.
    theme_stage: u8,
    logo: Option<egui::TextureHandle>,
    /// Start screen (create / open / sample scene) is showing.
    start_open: bool,
    /// The start screen can be closed without choosing (a scene exists).
    start_cancelable: bool,
    scene_name: String,
    /// Sample to load as soon as the UI runs (`--sample N`).
    start_sample: Option<usize>,
    /// Frames since a sample was requested; frames the camera once it has loaded.
    frame_after_load: Option<u32>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            layout_path: None,
            left_tab: LeftTab::Library,
            left_open: true,
            filter: Filter::All,
            movable_only: false,
            list_view: false,
            scene_search: String::new(),
            settings_open: false,
            settings_tab: SettingsTab::General,
            theme_stage: 0,
            logo: None,
            start_open: true,
            start_cancelable: false,
            scene_name: String::new(),
            start_sample: None,
            frame_after_load: None,
        }
    }
}

impl UiState {
    fn open_tab(&mut self, tab: LeftTab) {
        self.left_tab = tab;
        self.left_open = true;
    }

    fn open_settings(&mut self, tab: SettingsTab) {
        self.settings_open = true;
        self.settings_tab = tab;
    }

    /// Goes straight to the editor (scripted runs), optionally loading a sample.
    pub fn skip_start(&mut self, sample: Option<usize>) {
        self.start_open = false;
        self.start_sample = sample.filter(|&i| i < crate::samples::SAMPLES.len());
    }

    /// Loads sample `i` into the scene (replacing it).
    fn load_sample(&mut self, i: usize, library: &Library, actions: &mut Actions, status: &mut Status) {
        match crate::samples::write_layout(i, library) {
            Ok((path, missing)) => {
                self.scene_name = crate::samples::SAMPLES[i].title.to_string();
                // Saving asks for a location instead of overwriting the sample.
                self.layout_path = None;
                self.frame_after_load = Some(0);
                actions.0.push(Action::LoadLayout(path));
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

/// Menu entry with icon and shortcut; returns true (and closes the menu) when clicked.
fn menu_entry(ui: &mut egui::Ui, icon: &str, text: &str, shortcut: &str, enabled: bool) -> bool {
    let mut b = egui::Button::new(theme::label(icon, text));
    if !shortcut.is_empty() {
        b = b.shortcut_text(RichText::new(shortcut).color(theme::WEAK));
    }
    let clicked = ui.add_enabled(enabled, b).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn pick_layout_save(scene_name: &str) -> Option<PathBuf> {
    let file = format!("{}.json", crate::samples::slug(scene_name));
    rfd::FileDialog::new().add_filter("Layout", &["json"]).set_file_name(file).save_file()
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
    (mut status, mut block, mut state, pending): (ResMut<Status>, ResMut<UiBlock>, ResMut<UiState>, Res<crate::scene::PendingSpawns>),
    (handles, mut undo_req, undo_stack, mut hide, mut slot): (
        Res<HandleUi>,
        ResMut<crate::undo::UndoRequest>,
        Res<crate::undo::UndoStack>,
        ResMut<HideOverlays>,
        ResMut<LeftPanelSlot>,
    ),
    mut objects: ObjectQuery,
    mut camera: Single<&mut Camera, With<OrbitCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
    diagnostics: Res<DiagnosticsStore>,
    mut walls: ResMut<crate::wall_tool::WallTool>,
    (mut agent, mut catalog, bridge, mut gpu): (
        ResMut<crate::agent::Agent>,
        ResMut<crate::agent::catalog::Catalog>,
        Res<crate::agent::tools::ToolBridge>,
        ResMut<crate::gpu::Gpu>,
    ),
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let overlays = hide.0 == 0;
    hide.0 = hide.0.saturating_sub(1);
    slot.rtls = None;
    if std::mem::take(&mut slot.open_rtls) {
        state.open_tab(LeftTab::Rtls);
    }
    match state.theme_stage {
        0 => {
            theme::apply_fonts(&ctx);
            state.logo = load_logo(&ctx);
            state.theme_stage = 1;
        }
        1 => {
            theme::apply_style(&ctx);
            state.theme_stage = 2;
        }
        _ => {}
    }
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
        state.load_sample(i, &library, &mut actions, &mut status);
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
    let (save, save_as, open, new) = ctx.input_mut(|i| {
        let save_as = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::S));
        let save = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::S));
        let open = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::O));
        let new = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::N));
        (save, save_as, open, new)
    });
    let (mut do_save, mut do_save_as, mut do_open, mut do_new) = (save, save_as, open, new);

    let mut root = egui::Ui::new(
        ctx.clone(),
        "root".into(),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()),
    );

    // ---------------- Top bar
    let (active_dl, queued_dl) = library.download_count();
    egui::Panel::top("menu")
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin::symmetric(10, 5)))
        .show(&mut root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                if let Some(logo) = &state.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(vec2(20.0, 20.0)));
                }
                ui.label(RichText::new(crate::APP_NAME).text_style(egui::TextStyle::Heading));
                if !state.scene_name.is_empty() {
                    ui.label(RichText::new(format!("/  {}", state.scene_name)).color(theme::WEAK));
                }
                ui.add_space(10.0);
                ui.menu_button("File", |ui| {
                    ui.set_min_width(230.0);
                    do_new |= menu_entry(ui, icon::PLUS, "New scene…", "Ctrl+N", true);
                    do_open |= menu_entry(ui, icon::FOLDER_OPEN, "Open layout…", "Ctrl+O", true);
                    do_save |= menu_entry(ui, icon::FLOPPY_DISK, "Save layout", "Ctrl+S", true);
                    do_save_as |= menu_entry(ui, icon::FLOPPY_DISK, "Save layout as…", "Ctrl+Shift+S", true);
                    ui.separator();
                    if menu_entry(ui, icon::EXPORT, "Export scene as USDA…", "", true)
                        && let Some(p) = rfd::FileDialog::new().add_filter("USD", &["usda"]).set_file_name("scene.usda").save_file()
                    {
                        actions.0.push(Action::ExportUsda(p));
                    }
                    ui.separator();
                    if menu_entry(ui, icon::TRASH, "Clear scene", "", true) {
                        actions.0.push(Action::ClearScene);
                    }
                    if menu_entry(ui, icon::SIGN_OUT, "Quit", "", true) {
                        crate::quit();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    ui.set_min_width(200.0);
                    if menu_entry(ui, icon::ARROW_U_UP_LEFT, "Undo", "Ctrl+Z", undo_stack.can_undo()) {
                        undo_req.undo = true;
                    }
                    if menu_entry(ui, icon::ARROW_U_UP_RIGHT, "Redo", "Ctrl+Y", undo_stack.can_redo()) {
                        undo_req.redo = true;
                    }
                    ui.separator();
                    let sel = selection.selected;
                    if menu_entry(ui, icon::COPY, "Duplicate", "Ctrl+D", sel.is_some()) {
                        actions.0.extend(sel.map(Action::Duplicate));
                    }
                    if menu_entry(ui, icon::CROSSHAIR, "Focus", "F", sel.is_some()) {
                        actions.0.extend(sel.map(Action::Focus));
                    }
                    if menu_entry(ui, icon::SELECTION_SLASH, "Deselect", "Esc", sel.is_some()) {
                        actions.0.push(Action::Select(None));
                    }
                    if menu_entry(ui, icon::TRASH, "Delete", "Del", sel.is_some()) {
                        actions.0.extend(sel.map(Action::Delete));
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.set_min_width(220.0);
                    if menu_entry(ui, icon::FRAME_CORNERS, "Frame all", "F", true) {
                        actions.0.push(Action::FrameAll);
                    }
                    ui.separator();
                    ui.checkbox(&mut settings.grid, "Grid");
                    ui.checkbox(&mut settings.bounds, "Hover bounds");
                    ui.checkbox(&mut settings.no_overlap, "Prevent overlap");
                    ui.checkbox(&mut settings.gravity, "Gravity");
                    ui.checkbox(&mut settings.shadows, "Shadows");
                    ui.separator();
                    ui.menu_button(theme::label(icon::IMAGE, "Texture quality"), |ui| {
                        for s in [256u32, 512, 1024, 2048, 4096] {
                            ui.radio_value(&mut cache.max_texture, s, format!("{s} px"));
                        }
                    });
                    if menu_entry(ui, icon::SLIDERS_HORIZONTAL, "Lighting & viewport…", "", true) {
                        state.open_settings(SettingsTab::Viewport);
                    }
                    ui.separator();
                    ui.checkbox(&mut agent.open, "Assistant panel");
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let ai = egui::Button::new(theme::label(icon::SPARKLE, "Assistant")).selected(agent.open).min_size(vec2(0.0, 28.0));
                    if ui.add(ai).on_hover_text("Build scenes from instructions or a reference photo").clicked() {
                        agent.open = !agent.open;
                    }
                    if theme::icon_button(ui, icon::GEAR_SIX, "Settings").clicked() {
                        state.settings_open = true;
                    }
                    let n = active_dl + queued_dl;
                    let tip = if n > 0 { format!("Downloads: {active_dl} active, {queued_dl} queued") } else { "Downloads".into() };
                    let r = theme::icon_button_colored(ui, icon::CLOUD_ARROW_DOWN, &tip, (n > 0).then_some(theme::ONLINE), false);
                    if n > 0 {
                        let c = r.rect.right_top() + vec2(-5.0, 6.0);
                        ui.painter().circle_filled(c, 7.0, theme::ONLINE);
                        ui.painter().text(c, Align2::CENTER_CENTER, n.min(99).to_string(), egui::FontId::proportional(9.5), theme::BG);
                    }
                    if r.clicked() {
                        state.open_tab(LeftTab::Downloads);
                    }
                });
            });
        });

    // ---------------- Start screen: every session begins by creating (or opening) a scene
    if do_new && !state.start_open {
        state.start_open = true;
        state.start_cancelable = true;
        state.scene_name.clear();
    }
    if state.start_open {
        let mut name = std::mem::take(&mut state.scene_name);
        let cancelable = state.start_cancelable;
        let choice = start::show(&ctx, &mut library, &mut name, cancelable);
        state.scene_name = name;
        match choice {
            Some(start::Choice::Empty) => {
                if state.scene_name.trim().is_empty() {
                    state.scene_name = "Untitled scene".into();
                }
                state.scene_name = state.scene_name.trim().to_string();
                state.layout_path = None;
                actions.0.push(Action::ClearScene);
                state.start_open = false;
            }
            Some(start::Choice::Sample(i)) => {
                state.load_sample(i, &library, &mut actions, &mut status);
                state.start_open = false;
            }
            Some(start::Choice::Open) => do_open = true,
            Some(start::Choice::Cancel) => state.start_open = false,
            None => {}
        }
        // The scene's own shortcuts wait until a scene exists.
        do_save = false;
        do_save_as = false;
    }

    if do_open && let Some(p) = rfd::FileDialog::new().add_filter("Layout", &["json"]).pick_file() {
        state.scene_name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        state.layout_path = Some(p.clone());
        state.start_open = false;
        actions.0.push(Action::LoadLayout(p));
    }
    if do_save || do_save_as {
        let name = state.scene_name.clone();
        let p = if do_save_as { pick_layout_save(&name) } else { state.layout_path.clone().or_else(|| pick_layout_save(&name)) };
        if let Some(p) = p {
            state.layout_path = Some(p.clone());
            actions.0.push(Action::SaveLayout(p));
        }
    }

    // ---------------- Status bar
    let object_count = objects.iter().count();
    statusbar::show(&mut root, &diagnostics, &mut library, &cache, &mut status, &mut state, object_count);

    // ---------------- Left rail
    egui::Panel::left("rail")
        .exact_size(48.0)
        .resizable(false)
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin::symmetric(6, 8)))
        .show(&mut root, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let tabs = [
                (LeftTab::Library, icon::SQUARES_FOUR, "Asset library"),
                (LeftTab::Scene, icon::TREE_STRUCTURE, "Scene objects"),
                (LeftTab::Downloads, icon::CLOUD_ARROW_DOWN, "Downloads"),
                (LeftTab::Rtls, icon::BROADCAST, "Positioning · RTLS"),
            ];
            for (tab, ic, tip) in tabs {
                let sel = state.left_open && state.left_tab == tab;
                let r = ui.add(
                    egui::Button::new(RichText::new(ic).size(18.0).color(if sel { ACCENT } else { theme::WEAK }))
                        .selected(sel)
                        .frame_when_inactive(sel)
                        .min_size(vec2(36.0, 36.0)),
                );
                if tab == LeftTab::Rtls && slot.rtls_alerts > 0 {
                    ui.painter().circle_filled(r.rect.right_top() + vec2(-6.0, 6.0), 4.5, theme::DANGER);
                }
                let tip = if tab == LeftTab::Rtls && slot.rtls_alerts > 0 { format!("{tip} · {} alarms", slot.rtls_alerts) } else { tip.to_string() };
                if r.on_hover_text(tip).clicked() {
                    if sel {
                        state.left_open = false;
                    } else {
                        state.open_tab(tab);
                    }
                }
            }
            let design = ui.add(egui::Button::new(RichText::new(icon::CUBE).size(18.0).color(theme::WEAK)).frame_when_inactive(false).min_size(vec2(36.0, 36.0)));
            if design.on_hover_text("3D design").clicked() {
                slot.open_design = true;
            }
            let simulate = ui.add(egui::Button::new(RichText::new(icon::STACK).size(18.0).color(theme::WEAK)).frame_when_inactive(false).min_size(vec2(36.0, 36.0)));
            if simulate.on_hover_text("Simulations").clicked() {
                slot.open_simulations = true;
            }
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                let r = ui.add(egui::Button::new(RichText::new(icon::GEAR_SIX).size(18.0).color(theme::WEAK)).frame_when_inactive(false).min_size(vec2(36.0, 36.0)));
                if r.on_hover_text("Settings").clicked() {
                    state.settings_open = true;
                }
            });
        });

    // ---------------- Left panel
    if state.left_open && state.left_tab == LeftTab::Rtls {
        // Own panel id so its (wider) size is kept separately from the library's.
        egui::Panel::left("left_panel_rtls")
            .default_size(440.0)
            .size_range(320.0..=760.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::same(10)))
            .show(&mut root, |ui| {
                // Reserve the space; the RTLS system draws into it after this system.
                let rect = ui.available_rect_before_wrap();
                ui.allocate_rect(rect, egui::Sense::hover());
                slot.rtls = Some(rect);
            });
    } else if state.left_open {
        egui::Panel::left("left_panel")
            .default_size(290.0)
            .size_range(230.0..=480.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::same(10)))
            .show(&mut root, |ui| match state.left_tab {
                LeftTab::Library => library::show(ui, &mut library, &mut place, &mut cache, &mut state),
                LeftTab::Scene => outliner::show(ui, &mut objects, &selection, &mut actions, &mut state),
                LeftTab::Downloads => downloads::show(ui, &mut library, &mut state),
                LeftTab::Rtls => {}
            });
    }

    // ---------------- AI assistant (outermost on the right)
    if crate::agent::panel::show(&mut root, &ctx, &mut agent, &mut library, &bridge) {
        state.open_settings(SettingsTab::Assistant);
    }

    // ---------------- Viewport = what's left
    let central = root.available_rect_before_wrap();
    let scale = window.scale_factor();
    let pos = (central.min.to_vec2() * scale).max(egui::Vec2::ZERO);
    let size = (central.size() * scale).max(egui::vec2(1.0, 1.0));
    let max = egui::vec2(window.physical_width() as f32, window.physical_height() as f32);
    let size = size.min(max - pos).max(egui::vec2(1.0, 1.0));
    camera.viewport = Some(Viewport {
        physical_position: UVec2::new(pos.x as u32, pos.y as u32),
        physical_size: UVec2::new(size.x as u32, size.y as u32),
        ..default()
    });

    // ---------------- Properties: floating card over the viewport while something is selected
    if overlays && selection.selected.is_some_and(|e| objects.contains(e)) {
        let w = 290.0f32.min(central.width() - 24.0).max(200.0);
        egui::Area::new(egui::Id::new("properties_card"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(central.right_top() + vec2(-12.0, 12.0))
            .constrain_to(central)
            .show(&ctx, |ui| {
                theme::floating_frame().inner_margin(Margin::same(10)).show(ui, |ui| {
                    ui.set_width(w);
                    ui.set_max_height(central.height() - 24.0 - 20.0);
                    inspector::show(ui, &mut objects, &selection, &handles, &mut actions);
                });
            });
    }

    // ---------------- Floating toolbar (top centre of the viewport)
    // Centred in the part of the viewport the properties card leaves free.
    let card_w = if selection.selected.is_some_and(|e| objects.contains(e)) { 314.0 } else { 0.0 };
    let toolbar_x = (central.left() + central.right() - card_w) / 2.0;
    if overlays && !state.start_open {
        egui::Area::new(egui::Id::new("viewport_toolbar"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(toolbar_x, central.top() + 10.0))
            .constrain_to(central)
            .show(&ctx, |ui| {
                theme::floating_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        theme::icon_toggle(ui, &mut place.sticky, icon::STACK, "Multi-place: keep placing copies after each click (or hold Shift)");
                        theme::icon_toggle(ui, &mut walls.active, icon::WALL, "Draw walls: click points on the floor (Ctrl snap, Shift 15°, Esc or right-click to stop)");
                        ui.separator();
                        if ui.add_enabled_ui(undo_stack.can_undo(), |ui| theme::icon_button(ui, icon::ARROW_U_UP_LEFT, "Undo (Ctrl+Z)")).inner.clicked() {
                            undo_req.undo = true;
                        }
                        if ui.add_enabled_ui(undo_stack.can_redo(), |ui| theme::icon_button(ui, icon::ARROW_U_UP_RIGHT, "Redo (Ctrl+Y)")).inner.clicked() {
                            undo_req.redo = true;
                        }
                        ui.separator();
                        if theme::icon_button(ui, icon::FRAME_CORNERS, "Frame all (F)").clicked() {
                            actions.0.push(Action::FrameAll);
                        }
                        theme::icon_toggle(ui, &mut settings.grid, icon::GRID_FOUR, "Grid");
                        theme::icon_toggle(ui, &mut settings.bounds, icon::BOUNDING_BOX, "Hover bounds");
                        theme::icon_toggle(ui, &mut settings.no_overlap, icon::STACK, "Prevent overlap");
                        theme::icon_toggle(ui, &mut settings.gravity, icon::ARROW_LINE_DOWN, "Gravity: objects fall onto what is under them (lift or Alt-move to keep them in the air)");
                        theme::icon_toggle(ui, &mut settings.shadows, icon::SUN, "Shadows");
                    });
                });
            });
    }

    // ---------------- Placing banner
    let placing = place.ghost.as_ref().map(|(_, a)| (a.clone(), true)).or_else(|| place.asset.clone().map(|a| (a, false)));
    if let Some((asset, ready)) = placing.filter(|_| overlays) {
        egui::Area::new(egui::Id::new("placing_banner"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(toolbar_x, central.top() + 58.0))
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
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(toolbar_x, central.top() + 58.0))
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
    if overlays && object_count == 0 && place.asset.is_none() && !walls.active && !slot.results_shown {
        egui::Area::new(egui::Id::new("empty_scene"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_CENTER)
            .fixed_pos(central.center())
            .interactable(false)
            .show(&ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(icon::CUBE).size(34.0).color(theme::WEAK));
                    ui.label(RichText::new("Your scene is empty").strong());
                    ui.label(RichText::new("Pick an asset from the library, or ask the assistant to build one.").small().color(theme::WEAK));
                });
            });
    }

    // ---------------- Joint overlay: rings on each moving part, label on the hot one
    let painter = ctx
        .layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("joint_overlay")))
        .with_clip_rect(central);
    for m in handles.joints.iter().filter(|_| overlays) {
        let p = egui::pos2(m.screen.x, m.screen.y);
        let anchor = egui::pos2(m.anchor.x, m.anchor.y);
        let h = Handle::Joint(m.index);
        let hot = handles.active == Some(h) || handles.hovered == Some(h);
        if p.distance(anchor) > 1.5 {
            // Dot was moved off an overlapping one: tie it to the real joint.
            painter.line_segment([anchor, p], egui::Stroke::new(1.0, egui::Color32::from_white_alpha(170)));
            painter.circle_filled(anchor, 1.8, egui::Color32::WHITE);
        }
        if hot {
            painter.circle_filled(p + egui::vec2(1.0, 1.5), 8.0, egui::Color32::from_black_alpha(120));
            painter.circle_filled(p, 8.0, ACCENT);
            painter.circle_stroke(p, 8.0, egui::Stroke::new(2.0, egui::Color32::WHITE));
            let galley = painter.layout_no_wrap(m.label.clone(), egui::FontId::monospace(12.5), egui::Color32::WHITE);
            let rect = egui::Rect::from_min_size(
                p + egui::vec2(16.0, -galley.size().y / 2.0 - 5.0),
                galley.size() + egui::vec2(16.0, 10.0),
            );
            painter.rect_filled(rect, 6.0, theme::PANEL.gamma_multiply(0.95));
            painter.rect_stroke(rect, 6.0, egui::Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
            painter.galley(rect.min + egui::vec2(8.0, 5.0), galley, egui::Color32::WHITE);
        } else {
            // Small, see-through dot so the part under it stays visible.
            painter.circle_filled(p, 5.0, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 90));
            painter.circle_stroke(p, 5.0, egui::Stroke::new(2.0, egui::Color32::WHITE));
        }
    }
    if overlays && let Some(c) = handles.compact {
        painter.text(
            egui::pos2(c.x, c.y + 14.0),
            Align2::CENTER_TOP,
            "Small object · press F to zoom",
            egui::FontId::proportional(11.5),
            egui::Color32::from_white_alpha(200),
        );
    }

    // ---------------- Settings dialog
    if state.settings_open {
        let mut tab = state.settings_tab;
        let open = settings::show(&ctx, &mut tab, &mut library, &mut settings, &mut cache, &mut agent, &mut catalog, &mut gpu);
        state.settings_tab = tab;
        state.settings_open = open;
    }

    let over_ui = ctx
        .pointer_latest_pos()
        .map(|p| {
            !central.contains(p)
                || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background && l.order != egui::Order::Middle)
        })
        .unwrap_or(true);
    block.pointer = over_ui || ctx.egui_is_using_pointer() || state.start_open;
    block.keyboard = ctx.egui_wants_keyboard_input() || state.start_open;
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
