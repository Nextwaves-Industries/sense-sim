//! Tool ribbon under the top bar. The tab row holds File and View around the
//! workbenches; each shows captioned groups of icon-over-label buttons.

use bevy_egui::egui::{self, Align, Layout, Margin, vec2};

use super::workbench::{self, Workbench};
use super::{LeftPanelSlot, SettingsTab, UiState, topbar::Flags};
use crate::rtls::ui::{Page, RtlsUiState, Tab};
use crate::scene::{Action, Actions, PlaceMode, Selection, ViewSettings};
use crate::theme::{self, icon};

const TABS_HEIGHT: f32 = 26.0;
const TOOLS_HEIGHT: f32 = 60.0;
const GROUP_GAP: f32 = 14.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum RibbonTab {
    File,
    /// The tools of the workbench in front.
    #[default]
    Tools,
    View,
}

fn tab_divider(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::STROKE);
    ui.add_space(4.0);
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    root: &mut egui::Ui,
    state: &mut UiState,
    place: &mut PlaceMode,
    walls: &mut crate::wall_tool::WallTool,
    undo_stack: &crate::undo::UndoStack,
    undo_req: &mut crate::undo::UndoRequest,
    selection: &Selection,
    actions: &mut Actions,
    settings: &mut ViewSettings,
    slot: &mut LeftPanelSlot,
    rtls: &mut RtlsUiState,
    flags: &mut Flags,
    assistant_open: &mut bool,
) {
    // 3D Design's tools are SolidCraft's own ribbon, in the workspace: a second row of them here
    // would only stand between the tabs and it. Its file actions are in the strip below this.
    let tools_height = if slot.bench.current == Workbench::Design && state.ribbon_tab == RibbonTab::Tools { 0.0 } else { TOOLS_HEIGHT };
    egui::Panel::top("ribbon").exact_size(TABS_HEIGHT + tools_height).frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(8, 0))).show(root, |ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        ui.allocate_ui_with_layout(vec2(ui.available_width(), TABS_HEIGHT), Layout::left_to_right(Align::Center), |ui| {
            if theme::ribbon_tab(ui, "File", state.ribbon_tab == RibbonTab::File).clicked() {
                state.ribbon_tab = RibbonTab::File;
            }
            tab_divider(ui);
            let shown: Vec<Workbench> = Workbench::ALL.into_iter().filter(|b| slot.shows(*b)).collect();
            for bench in shown {
                let current = slot.bench.current == bench;
                // What goes on while the workbench is in the background.
                let dot = match bench {
                    Workbench::Simulation if slot.rtls_alerts > 0 => Some(theme::DANGER),
                    Workbench::Plant if slot.plant == Some(true) => Some(theme::SUCCESS),
                    Workbench::Factory if slot.twin.down > 0 => Some(theme::DANGER),
                    _ => None,
                };
                let showing = current && state.ribbon_tab == RibbonTab::Tools;
                if theme::workbench_tab(ui, bench.glyph(), bench.name(), current, showing, dot).on_hover_text(bench.tip()).clicked() {
                    state.ribbon_tab = RibbonTab::Tools;
                    workbench::switch(slot, rtls, bench);
                }
            }
            tab_divider(ui);
            if theme::ribbon_tab(ui, "View", state.ribbon_tab == RibbonTab::View).clicked() {
                state.ribbon_tab = RibbonTab::View;
            }
        });
        ui.horizontal_centered(|ui| match state.ribbon_tab {
            RibbonTab::File => file_tools(ui, actions, flags, slot),
            RibbonTab::View => view_tools(ui, state, actions, settings, assistant_open, slot.bench.current.has_library(), !slot.canvas_taken()),
            RibbonTab::Tools => match slot.bench.current {
                Workbench::Layout => layout_tools(ui, state, place, walls, undo_stack, undo_req, selection, actions, settings),
                Workbench::Design => {}
                Workbench::Placement => placement_tools(ui, slot, rtls),
                Workbench::Antenna => antenna_tools(ui, slot),
                Workbench::Simulation => simulation_tools(ui, slot, rtls),
                Workbench::Plant => plant_tools(ui, state, slot),
                Workbench::Factory => factory_tools(ui, slot),
            },
        });
    });
}

fn file_tools(ui: &mut egui::Ui, actions: &mut Actions, flags: &mut Flags, slot: &LeftPanelSlot) {
    if slot.canvas_taken() {
        let thing = if slot.studio_open { "antenna project" } else { "design" };
        theme::ribbon_caption(ui, if slot.studio_open { "Antenna" } else { "Design" });
        flags.new |= theme::ribbon_button(ui, icon::PLUS, "New", true, false, &format!("New {thing} (Ctrl+N)")).clicked();
        flags.open |= theme::ribbon_button(ui, icon::FOLDER_OPEN, "Open", true, false, &format!("Open a {thing} (Ctrl+O)")).clicked();
        flags.save |= theme::ribbon_button(ui, icon::FLOPPY_DISK, "Save", true, false, &format!("Save the {thing} (Ctrl+S)")).clicked();
        flags.save_as |= theme::ribbon_button(ui, icon::COPY, "Save as", true, false, &format!("Save the {thing} to a new file (Ctrl+Shift+S)")).clicked();
        return;
    }
    theme::ribbon_caption(ui, "Scene");
    flags.new |= theme::ribbon_button(ui, icon::PLUS, "New", true, false, "New scene in a tab (Ctrl+N)").clicked();
    flags.open |= theme::ribbon_button(ui, icon::FOLDER_OPEN, "Open", true, false, "Open one or more layouts, each in its own tab (Ctrl+O)").clicked();
    flags.close |= theme::ribbon_button(ui, icon::X, "Close", true, false, "Close this tab (Ctrl+W)").clicked();
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Save");
    flags.save |= theme::ribbon_button(ui, icon::FLOPPY_DISK, "Save", true, false, "Save the layout (Ctrl+S)").clicked();
    flags.save_as |= theme::ribbon_button(ui, icon::COPY, "Save as", true, false, "Save the layout to a new file (Ctrl+Shift+S)").clicked();
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Share");
    if theme::ribbon_button(ui, icon::EXPORT, "Export USDA", true, false, "Export a Y-up USDA stage that references the original assets").clicked()
        && let Some(p) = rfd::FileDialog::new().add_filter("USD", &["usda"]).set_file_name("scene.usda").save_file()
    {
        actions.0.push(Action::ExportUsda(p));
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_tools(
    ui: &mut egui::Ui,
    state: &mut UiState,
    place: &mut PlaceMode,
    walls: &mut crate::wall_tool::WallTool,
    undo_stack: &crate::undo::UndoStack,
    undo_req: &mut crate::undo::UndoRequest,
    selection: &Selection,
    actions: &mut Actions,
    settings: &mut ViewSettings,
) {
    let sel = selection.selected;

    theme::ribbon_caption(ui, "Create");
    if theme::ribbon_button(ui, icon::CUBE, "Assets", true, false, "Show the asset library").clicked() {
        state.sections.library = true;
    }
    if theme::ribbon_button(ui, icon::WALL, "Walls", true, walls.active, "Draw walls: click points on the floor (Ctrl snap, Shift 15°, Esc or right-click to stop)").clicked() {
        walls.active = !walls.active;
    }
    if theme::ribbon_button(ui, icon::STACK, "Multi-place", true, place.sticky, "Keep placing copies after each click (or hold Shift)").clicked() {
        place.sticky = !place.sticky;
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Edit");
    if theme::ribbon_button(ui, icon::ARROW_U_UP_LEFT, "Undo", undo_stack.can_undo(), false, "Undo (Ctrl+Z)").clicked() {
        undo_req.undo = true;
    }
    if theme::ribbon_button(ui, icon::ARROW_U_UP_RIGHT, "Redo", undo_stack.can_redo(), false, "Redo (Ctrl+Y)").clicked() {
        undo_req.redo = true;
    }
    if theme::ribbon_button(ui, icon::COPY, "Duplicate", sel.is_some(), false, "Duplicate the selection (Ctrl+D)").clicked() {
        actions.0.extend(sel.map(Action::Duplicate));
    }
    if theme::ribbon_button(ui, icon::CROSSHAIR, "Focus", sel.is_some(), false, "Focus the camera on the selection (F)").clicked() {
        actions.0.extend(sel.map(Action::Focus));
    }
    if theme::ribbon_button(ui, icon::ARROW_LINE_DOWN, "Drop", sel.is_some(), false, "Let a lifted object land on what is under it").clicked() {
        actions.0.extend(sel.map(Action::Drop));
    }
    if theme::ribbon_button(ui, icon::TRASH, "Delete", sel.is_some(), false, "Delete the selection (Del)").clicked() {
        actions.0.extend(sel.map(Action::Delete));
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Select");
    if theme::ribbon_button(ui, icon::CURSOR_CLICK, "Select all", true, false, "Select every object (Ctrl+A)").clicked() {
        actions.0.push(Action::SelectAll);
    }
    if theme::ribbon_button(ui, icon::X, "Deselect", sel.is_some(), false, "Clear the selection (Esc)").clicked() {
        actions.0.push(Action::Select(None));
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Placement");
    if theme::ribbon_button(ui, icon::SELECTION_SLASH, "No overlap", true, settings.no_overlap, "Placing and dragging won't let objects cut into each other").clicked() {
        settings.no_overlap = !settings.no_overlap;
    }
    if theme::ribbon_button(ui, icon::ARROW_LINE_DOWN, "Gravity", true, settings.gravity, "Objects fall onto what is under them (lift or Alt-move to keep them in the air)").clicked() {
        settings.gravity = !settings.gravity;
    }
}

/// A button for a page of the positioning card: opens it there, or closes it if it shows.
fn page_button(ui: &mut egui::Ui, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState, glyph: &str, text: &str, tip: &str, target: Page) {
    let here = slot.rtls_open && rtls.local.page() == target;
    if theme::ribbon_button(ui, glyph, text, true, here, tip).clicked() {
        slot.rtls_open = !here;
        if !here {
            rtls.local.set_page(target);
        }
    }
}

fn placement_tools(ui: &mut egui::Ui, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    let mut go = |ui: &mut egui::Ui, glyph: &str, text: &str, tip: &str, target: Page| page_button(ui, slot, rtls, glyph, text, tip, target);

    theme::ribbon_caption(ui, "Place");
    go(ui, icon::ROCKET_LAUNCH, "Deploy", "Place the RTLS for this scene with one button", Page::Deploy);
    go(ui, icon::BROADCAST, "Devices", "Anchors, tags and zones", Page::Tool(Tab::Devices));
    go(ui, icon::INFO, "Inspector", "Details of the selected device or zone", Page::Tool(Tab::Inspector));
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Plan");
    go(ui, icon::MAP_TRIFOLD, "Planning", "Coverage maps and suggested device positions", Page::Tool(Tab::Planning));
    go(ui, icon::SLIDERS, "Settings", "Positioning settings", Page::Tool(Tab::Settings));
}

fn simulation_tools(ui: &mut egui::Ui, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    let mut go = |ui: &mut egui::Ui, glyph: &str, text: &str, tip: &str, target: Page| page_button(ui, slot, rtls, glyph, text, tip, target);

    theme::ribbon_caption(ui, "Run");
    go(ui, icon::PLAY_CIRCLE, "Test live", "Run the positioning system and see how well it does", Page::Test);
    go(ui, icon::PULSE, "Live data", "Tracks, events and alarms as they happen", Page::Tool(Tab::Live));
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Analyze");
    go(ui, icon::CHART_LINE, "RF analysis", "Signal levels and dead zones", Page::Tool(Tab::Rf));
    go(ui, icon::TARGET, "Predictions", "Predictions and how they held up against measurements", Page::Outcomes);
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Connect");
    go(ui, icon::SHARE_NETWORK, "Outputs", "Stream positions to MQTT, webhooks, UDP or a recording", Page::Tool(Tab::Outputs));
    go(ui, icon::LIGHTNING, "OpenEMS", "Link the positioning data to OpenEMS Edge", Page::OpenEms);
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Solvers");
    go(ui, icon::WAVE_SINE, "RF solver", "Simulate antennas and materials with the openEMS field solver", Page::Fdtd);
    if theme::ribbon_button(ui, icon::STACK, "Simulations", true, false, "Create and run simulations: structural, flow, thermal, electromagnetic").clicked() {
        slot.open_simulations = true;
    }
}

#[cfg(feature = "antenna-studio")]
fn antenna_tools(ui: &mut egui::Ui, slot: &mut LeftPanelSlot) {
    use crate::antenna::ui::Page as Studio;
    type Button = (Studio, &'static str, &'static str, &'static str);
    const GROUPS: [(&str, &[Button]); 5] = [
        ("Design", &[
            (Studio::Samples, "samples", icon::PACKAGE, "Samples"),
            (Studio::Gallery, "gallery", icon::SQUARES_FOUR, "Templates"),
            (Studio::Draw, "draw", icon::NOTE_PENCIL, "Draw"),
            (Studio::Variables, "variables", icon::SLIDERS_HORIZONTAL, "Variables"),
            (Studio::Parts, "parts", icon::CUBE, "Parts"),
            (Studio::Reconfig, "reconfig", icon::LIGHTNING, "Reconfig"),
            (Studio::Materials, "materials", icon::STACK, "Materials"),
        ]),
        ("Solve", &[(Studio::Setup, "setup", icon::PLAY_CIRCLE, "Run")]),
        ("Results", &[
            (Studio::SParams, "sparams", icon::CHART_LINE, "S-params"),
            (Studio::Smith, "smith", icon::TARGET, "Smith"),
            (Studio::FarField, "farfield", icon::BROADCAST, "Far field"),
            (Studio::Fields, "fields", icon::WAVE_SINE, "Fields"),
        ]),
        ("Analysis", &[
            (Studio::Gate, "gate", icon::DOOR_OPEN, "Gate test"),
            (Studio::Array, "array", icon::GRID_FOUR, "Array"),
            (Studio::Matching, "matching", icon::PLUGS, "Matching"),
            (Studio::Mimo, "mimo", icon::SHARE_NETWORK, "MIMO"),
            (Studio::Link, "link", icon::PULSE, "Link"),
            (Studio::Rcs, "rcs", icon::CROSSHAIR, "RCS"),
        ]),
        ("Automate", &[
            (Studio::Sweep, "sweep", icon::FADERS, "Sweep"),
            (Studio::Optimize, "optimize", icon::ROCKET_LAUNCH, "Optimize"),
            (Studio::Converge, "converge", icon::GAUGE, "Mesh"),
            (Studio::Yield, "yield", icon::LIST_CHECKS, "Yield"),
            (Studio::Export, "export", icon::EXPORT, "Export"),
        ]),
    ];
    for (i, (caption, buttons)) in GROUPS.into_iter().enumerate() {
        if i > 0 {
            ui.add_space(GROUP_GAP);
        }
        theme::ribbon_caption(ui, caption);
        for &(page, name, glyph, text) in buttons {
            if theme::ribbon_button(ui, glyph, text, true, slot.studio_page == page, "").clicked() {
                slot.studio_goto.push(name.to_string());
            }
        }
    }
}

#[cfg(not(feature = "antenna-studio"))]
fn antenna_tools(_ui: &mut egui::Ui, _slot: &mut LeftPanelSlot) {}

fn plant_tools(ui: &mut egui::Ui, state: &mut UiState, slot: &mut LeftPanelSlot) {
    theme::ribbon_caption(ui, "Build");
    if theme::ribbon_button(ui, icon::CUBE, "Assets", true, false, "Show the asset library: pumps, tanks, conveyors and more").clicked() {
        state.sections.library = true;
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Run");
    let running = slot.plant == Some(true);
    let (glyph, text, tip) = match slot.plant {
        Some(true) => (icon::PAUSE, "Pause", "Pause the plant"),
        Some(false) => (icon::PLAY, "Run", "Run the plant"),
        None => (icon::PLAY, "Run", "This scene has no connected plant yet"),
    };
    if theme::ribbon_button(ui, glyph, text, slot.plant.is_some(), running, tip).clicked() {
        slot.plant_toggle = true;
    }
}

fn factory_tools(ui: &mut egui::Ui, slot: &mut LeftPanelSlot) {
    use crate::twin::{DEMO_SIZES, TwinPage, TwinRequest};
    let twin = &mut slot.twin;

    theme::ribbon_caption(ui, "Plant");
    if theme::ribbon_button(ui, icon::FLOW_ARROW, "Lines", true, twin.open && twin.page == TwinPage::Lines, "Each production line as a pipeline of its machines: plan, made and cycle time").clicked() {
        twin.toggle_page(TwinPage::Lines);
    }
    if theme::ribbon_button(ui, icon::LIST_BULLETS, "Devices", true, twin.open && twin.page == TwinPage::Devices, "How many machines, PLCs and sensors are on the network, and how they are doing").clicked() {
        twin.toggle_page(TwinPage::Devices);
    }
    if theme::ribbon_button(ui, icon::PULSE, "Events", true, twin.open && twin.page == TwinPage::Events, "What went down, recovered or changed").clicked() {
        twin.toggle_page(TwinPage::Events);
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "View");
    if theme::ribbon_button(ui, icon::FRAME_CORNERS, "Frame", twin.total > 0, false, "Move the camera to the devices").clicked() {
        twin.requests.push(TwinRequest::FrameDevices);
    }
    if theme::ribbon_button(ui, icon::INFO, "Labels", true, twin.labels, "Name the devices nearest the camera. Failing ones are always named").clicked() {
        twin.labels = !twin.labels;
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Connect");
    if theme::ribbon_button(ui, icon::PLUG, "Connection", true, twin.open && twin.page == TwinPage::Connection, "The Sense Twin service, and what a scan can and can't tell").clicked() {
        twin.toggle_page(TwinPage::Connection);
    }
    let demo = twin.is_demo();
    if theme::ribbon_button(ui, icon::PLAY_CIRCLE, "Demo", true, demo, "Try this workbench with invented devices").clicked() {
        twin.requests.push(TwinRequest::Demo(if demo { None } else { Some(DEMO_SIZES[0]) }));
    }
    theme::ribbon_button(ui, icon::MAGNIFYING_GLASS, "Scan", false, false, "Network scanning arrives in a later version, through the operator console");
}

fn view_tools(ui: &mut egui::Ui, state: &mut UiState, actions: &mut Actions, settings: &mut ViewSettings, assistant_open: &mut bool, has_library: bool, scene: bool) {
    fn toggle(ui: &mut egui::Ui, glyph: &str, text: &str, value: &mut bool, tip: &str) {
        if theme::ribbon_button(ui, glyph, text, true, *value, tip).clicked() {
            *value = !*value;
        }
    }

    // These are about the scene; a workspace with the canvas has its own.
    if scene {
    theme::ribbon_caption(ui, "Camera");
    if theme::ribbon_button(ui, icon::FRAME_CORNERS, "Frame all", true, false, "Fit the whole scene in the view (F)").clicked() {
        actions.0.push(Action::FrameAll);
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Display");
    toggle(ui, icon::GRID_FOUR, "Grid", &mut settings.grid, "Show the floor grid");
    toggle(ui, icon::BOUNDING_BOX, "Bounds", &mut settings.bounds, "Outline the object under the cursor");
    toggle(ui, icon::SUN, "Shadows", &mut settings.shadows, "Cast shadows from the sun");
    if theme::ribbon_button(ui, icon::SLIDERS_HORIZONTAL, "Lighting", true, false, "Lighting and viewport settings").clicked() {
        state.open_settings(SettingsTab::Viewport);
    }
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "Performance");
    toggle(ui, icon::GAUGE, "Sampling", &mut settings.sampling, "Draw coarser meshes when zoomed out and skip parts too small to see");
    toggle(ui, icon::CUBE_TRANSPARENT, "Occlusion", &mut settings.occlusion, "Skip parts hidden behind others");
    ui.add_space(GROUP_GAP);

    }

    theme::ribbon_caption(ui, "Panels");
    if theme::ribbon_button(ui, icon::CUBE, "Library", has_library, state.sections.library && has_library, "The asset library").clicked() {
        state.sections.library = !state.sections.library;
    }
    toggle(ui, icon::TREE_STRUCTURE, "Scene", &mut state.sections.scene, "The list of objects in the scene");
    toggle(ui, icon::CLOUD_ARROW_DOWN, "Downloads", &mut state.downloads_open, "Asset downloads");
    toggle(ui, icon::SPARKLE, "Assistant", assistant_open, "The AI assistant");
    ui.add_space(GROUP_GAP);

    theme::ribbon_caption(ui, "App");
    if theme::ribbon_button(ui, icon::GEAR_SIX, "Settings", true, false, "Sense Sim settings").clicked() {
        state.settings_open = true;
    }
}
