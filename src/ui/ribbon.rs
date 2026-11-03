//! Tool ribbon under the top bar. At its left end the tile of the workspace in front
//! (see `switcher`); beside it File, that workspace's own tabs and View, each showing
//! groups of icon-over-label buttons with a caption under them.

use bevy_egui::egui::{self, Align, Layout, Margin, Stroke, vec2};

use super::workbench::Workbench;
use super::{LeftPanelSlot, SettingsTab, UiState, switcher, topbar::Flags};
use crate::rtls::ui::{Page, RtlsUiState, Tab};
use crate::scene::{Action, Actions, PlaceMode, Selection, ViewSettings};
use crate::theme::{self, icon};

const TABS_HEIGHT: f32 = 26.0;
const TOOLS_HEIGHT: f32 = 72.0;
const HEIGHT: f32 = TABS_HEIGHT + TOOLS_HEIGHT;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum RibbonTab {
    File,
    /// The tools of the workspace in front, on the tab `UiState::tool_tabs` says.
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
    #[cfg(feature = "antenna-studio")]
    follow_studio_page(state, slot);
    egui::Panel::top("ribbon").exact_size(HEIGHT).frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(8, 0))).show(root, |ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        ui.allocate_ui_with_layout(vec2(ui.available_width(), HEIGHT), Layout::left_to_right(Align::Center), |ui| {
            switcher::tile(ui, state, slot, HEIGHT - 8.0);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                let bench = slot.bench.current;
                let names = bench.tabs();
                let tool = state.tool_tabs[bench as usize].min(names.len() - 1);
                let row = ui.allocate_ui_with_layout(vec2(ui.available_width(), TABS_HEIGHT), Layout::left_to_right(Align::Center), |ui| {
                    if theme::ribbon_tab(ui, "File", state.ribbon_tab == RibbonTab::File).clicked() {
                        state.ribbon_tab = RibbonTab::File;
                    }
                    tab_divider(ui);
                    for (i, name) in names.iter().enumerate() {
                        if theme::ribbon_tab(ui, name, state.ribbon_tab == RibbonTab::Tools && tool == i).clicked() {
                            state.ribbon_tab = RibbonTab::Tools;
                            state.tool_tabs[bench as usize] = i;
                        }
                    }
                    if theme::ribbon_tab(ui, "View", state.ribbon_tab == RibbonTab::View).clicked() {
                        state.ribbon_tab = RibbonTab::View;
                    }
                });
                ui.painter().hline(ui.max_rect().x_range(), row.response.rect.bottom(), Stroke::new(1.0, theme::STROKE));
                ui.add_space(3.0);
                let tool = state.tool_tabs[bench as usize].min(names.len() - 1);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), TOOLS_HEIGHT - 3.0), Layout::left_to_right(Align::Min), |ui| match state.ribbon_tab {
                    RibbonTab::File => file_tools(ui, actions, flags, slot),
                    RibbonTab::View => view_tools(ui, state, actions, settings, assistant_open, bench.has_library(), slot),
                    RibbonTab::Tools => match bench {
                        Workbench::Layout => layout_tools(ui, tool, state, place, walls, undo_stack, undo_req, selection, actions, settings),
                        Workbench::Plan => plan_tools(ui, tool, slot, rtls),
                        Workbench::Antenna => antenna_tools(ui, tool, slot),
                        Workbench::Simulation => simulation_tools(ui, tool, slot, rtls),
                        Workbench::Plant => plant_tools(ui, state, slot),
                        Workbench::Factory => factory_tools(ui, tool, slot),
                    },
                });
            });
        });
    });
}

fn file_tools(ui: &mut egui::Ui, actions: &mut Actions, flags: &mut Flags, slot: &LeftPanelSlot) {
    if slot.canvas_taken() {
        let thing = if slot.studio_open { "antenna project" } else { "design" };
        theme::ribbon_group(ui, if slot.studio_open { "Antenna" } else { "Design" }, |ui| {
            flags.new |= theme::ribbon_button(ui, icon::PLUS, "New", true, false, &format!("New {thing} (Ctrl+N)")).clicked();
            flags.open |= theme::ribbon_button(ui, icon::FOLDER_OPEN, "Open", true, false, &format!("Open a {thing} (Ctrl+O)")).clicked();
            flags.save |= theme::ribbon_button(ui, icon::FLOPPY_DISK, "Save", true, false, &format!("Save the {thing} (Ctrl+S)")).clicked();
            flags.save_as |= theme::ribbon_button(ui, icon::COPY, "Save as", true, false, &format!("Save the {thing} to a new file (Ctrl+Shift+S)")).clicked();
        });
        return;
    }
    theme::ribbon_group(ui, "Project", |ui| {
        flags.new |= theme::ribbon_button(ui, icon::PLUS, "New", true, false, "New project in a tab (Ctrl+N)").clicked();
        flags.open |= theme::ribbon_button(ui, icon::FOLDER_OPEN, "Open", true, false, "Open scenes, each in its own tab, or an antenna project (Ctrl+O)").clicked();
        flags.close |= theme::ribbon_button(ui, icon::X, "Close", true, false, "Close this tab (Ctrl+W)").clicked();
    });
    theme::ribbon_divider(ui);

    theme::ribbon_group(ui, "Save", |ui| {
        flags.save |= theme::ribbon_button(ui, icon::FLOPPY_DISK, "Save", true, false, "Save the layout (Ctrl+S)").clicked();
        flags.save_as |= theme::ribbon_button(ui, icon::COPY, "Save as", true, false, "Save the layout to a new file (Ctrl+Shift+S)").clicked();
    });
    theme::ribbon_divider(ui);

    theme::ribbon_group(ui, "Share", |ui| {
        if theme::ribbon_button(ui, icon::EXPORT, "Export USDA", true, false, "Export a Y-up USDA stage that references the original assets").clicked()
            && let Some(p) = rfd::FileDialog::new().add_filter("USD", &["usda"]).set_file_name("scene.usda").save_file()
        {
            actions.0.push(Action::ExportUsda(p));
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn layout_tools(
    ui: &mut egui::Ui,
    tab: usize,
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
    if tab == 0 {
        theme::ribbon_group(ui, "Create", |ui| {
            if theme::ribbon_button(ui, icon::CUBE, "Assets", true, false, "Show the asset library").clicked() {
                state.sections.library = true;
            }
            if theme::ribbon_button(ui, icon::WALL, "Walls", true, walls.active, "Draw walls: click points on the floor (Ctrl snap, Shift 15°, Esc or right-click to stop)").clicked() {
                walls.active = !walls.active;
            }
            let placing_door = place.asset.as_deref().is_some_and(crate::agent::assetgen::is_glass_door);
            if theme::ribbon_button(ui, icon::DOOR_OPEN, "Glass door", true, placing_door, "Place a closed glass door, 1.8 by 2.1 m, for an entrance or exit. Stand it in a gap between walls: inside a solid wall, radio still pays for the wall. Q and E turn it.").clicked() {
                if placing_door {
                    place.asset = None;
                } else {
                    walls.glass_door = true;
                }
            }
            if theme::ribbon_button(ui, icon::STACK, "Multi-place", true, place.sticky, "Keep placing copies after each click (or hold Shift)").clicked() {
                place.sticky = !place.sticky;
            }
        });
        theme::ribbon_divider(ui);

        theme::ribbon_group(ui, "Rules", |ui| {
            if theme::ribbon_button(ui, icon::SELECTION_SLASH, "No overlap", true, settings.no_overlap, "Placing and dragging won't let objects cut into each other").clicked() {
                settings.no_overlap = !settings.no_overlap;
            }
            if theme::ribbon_button(ui, icon::ARROW_LINE_DOWN, "Gravity", true, settings.gravity, "Objects fall onto what is under them (lift or Alt-move to keep them in the air)").clicked() {
                settings.gravity = !settings.gravity;
            }
        });
        return;
    }

    theme::ribbon_group(ui, "History", |ui| {
        if theme::ribbon_button(ui, icon::ARROW_U_UP_LEFT, "Undo", undo_stack.can_undo(), false, "Undo (Ctrl+Z)").clicked() {
            undo_req.undo = true;
        }
        if theme::ribbon_button(ui, icon::ARROW_U_UP_RIGHT, "Redo", undo_stack.can_redo(), false, "Redo (Ctrl+Y)").clicked() {
            undo_req.redo = true;
        }
    });
    theme::ribbon_divider(ui);

    theme::ribbon_group(ui, "Object", |ui| {
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
    });
    theme::ribbon_divider(ui);

    theme::ribbon_group(ui, "Select", |ui| {
        if theme::ribbon_button(ui, icon::CURSOR_CLICK, "Select all", true, false, "Select every object (Ctrl+A)").clicked() {
            actions.0.push(Action::SelectAll);
        }
        if theme::ribbon_button(ui, icon::X, "Deselect", sel.is_some(), false, "Clear the selection (Esc)").clicked() {
            actions.0.push(Action::Select(None));
        }
    });
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

fn plan_tools(ui: &mut egui::Ui, tab: usize, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    let mut go = |ui: &mut egui::Ui, glyph: &str, text: &str, tip: &str, target: Page| page_button(ui, slot, rtls, glyph, text, tip, target);

    if tab == 0 {
        theme::ribbon_group(ui, "Deploy", |ui| {
            go(ui, icon::ROCKET_LAUNCH, "Deploy", "Place the RTLS for this scene with one button", Page::Deploy);
        });
        theme::ribbon_divider(ui);
        theme::ribbon_group(ui, "Devices", |ui| {
            go(ui, icon::BROADCAST, "Devices", "Anchors, tags and zones", Page::Tool(Tab::Devices));
            go(ui, icon::INFO, "Inspector", "Details of the selected device or zone", Page::Tool(Tab::Inspector));
        });
        return;
    }

    theme::ribbon_group(ui, "Coverage", |ui| {
        go(ui, icon::MAP_TRIFOLD, "Planning", "Coverage maps and suggested device positions", Page::Tool(Tab::Planning));
    });
    theme::ribbon_divider(ui);
    theme::ribbon_group(ui, "Setup", |ui| {
        go(ui, icon::SLIDERS, "Settings", "Positioning settings", Page::Tool(Tab::Settings));
    });
}

fn simulation_tools(ui: &mut egui::Ui, tab: usize, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    let mut go = |ui: &mut egui::Ui, glyph: &str, text: &str, tip: &str, target: Page| page_button(ui, slot, rtls, glyph, text, tip, target);

    match tab {
        0 => {
            theme::ribbon_group(ui, "Run", |ui| {
                go(ui, icon::PLAY_CIRCLE, "Test live", "Run the positioning system and see how well it does", Page::Test);
                go(ui, icon::PULSE, "Live data", "Tracks, events and alarms as they happen", Page::Tool(Tab::Live));
            });
            theme::ribbon_divider(ui);
            theme::ribbon_group(ui, "Analyze", |ui| {
                go(ui, icon::CHART_LINE, "RF analysis", "Signal levels and dead zones", Page::Tool(Tab::Rf));
                go(ui, icon::TARGET, "Predictions", "Predictions and how they held up against measurements", Page::Outcomes);
            });
        }
        1 => {
            theme::ribbon_group(ui, "Solvers", |ui| {
                page_button(ui, slot, rtls, icon::WAVE_SINE, "RF solver", "Simulate antennas and materials with the openEMS field solver", Page::Fdtd);
                if theme::ribbon_button(ui, icon::STACK, "Simulations", true, false, "Create and run simulations: structural, flow, thermal, electromagnetic").clicked() {
                    slot.open_simulations = true;
                }
            });
        }
        _ => {
            theme::ribbon_group(ui, "Connect", |ui| {
                go(ui, icon::SHARE_NETWORK, "Outputs", "Stream positions to MQTT, webhooks, UDP or a recording", Page::Tool(Tab::Outputs));
                go(ui, icon::LIGHTNING, "OpenEMS", "Link the positioning data to OpenEMS Edge", Page::OpenEms);
            });
        }
    }
}

/// The Antenna workspace's tools, one list per ribbon tab: everything the Studio can do is here.
#[cfg(feature = "antenna-studio")]
mod antenna_ribbon {
    use crate::antenna::ui::Page;
    use crate::theme::icon;

    /// A page, its name for `slot.studio_goto`, and how its button looks.
    pub type Button = (Page, &'static str, &'static str, &'static str);
    pub type Group = (&'static str, &'static [Button]);

    /// The Model tab has no list here: its buttons are tools, from `model_ribbon`.
    pub const TABS: [&[Group]; 6] = [
        &[
            ("Start", &[
                (Page::Samples, "samples", icon::PACKAGE, "Samples"),
                (Page::Gallery, "gallery", icon::SQUARES_FOUR, "Templates"),
                (Page::TagDesign, "tag", icon::SPARKLE, "RFID tag"),
            ]),
            ("Model", &[
                (Page::Draw, "draw", icon::CUBE_FOCUS, "3D model"),
                (Page::Variables, "variables", icon::SLIDERS_HORIZONTAL, "Variables"),
                (Page::Parts, "parts", icon::CUBE, "Parts"),
                (Page::Reconfig, "reconfig", icon::LIGHTNING, "Reconfig"),
                (Page::Materials, "materials", icon::STACK, "Materials"),
            ]),
        ],
        &[],
        &[
            ("Run", &[(Page::Setup, "setup", icon::PLAY_CIRCLE, "Simulation")]),
            ("Automate", &[
                (Page::Sweep, "sweep", icon::FADERS, "Sweep"),
                (Page::Optimize, "optimize", icon::ROCKET_LAUNCH, "Optimize"),
                (Page::Tuning, "tuning", icon::SLIDERS, "Tuning"),
                (Page::Converge, "converge", icon::GAUGE, "Mesh"),
                (Page::Yield, "yield", icon::LIST_CHECKS, "Yield"),
            ]),
        ],
        &[
            ("Plots", &[
                (Page::SParams, "sparams", icon::CHART_LINE, "S-params"),
                (Page::Smith, "smith", icon::TARGET, "Smith"),
                (Page::FarField, "farfield", icon::BROADCAST, "Far field"),
                (Page::Fields, "fields", icon::WAVE_SINE, "Fields"),
            ]),
            ("Output", &[(Page::Export, "export", icon::EXPORT, "Export")]),
        ],
        &[
            ("Circuit", &[
                (Page::Matching, "matching", icon::PLUGS, "Matching"),
                (Page::Mimo, "mimo", icon::SHARE_NETWORK, "MIMO"),
            ]),
            ("System", &[
                (Page::Link, "link", icon::PULSE, "Link"),
                (Page::Rcs, "rcs", icon::CROSSHAIR, "RCS"),
                (Page::Gate, "gate", icon::DOOR_OPEN, "Gate test"),
            ]),
        ],
        &[
            ("Arrays", &[
                (Page::Array, "array", icon::GRID_FOUR, "Array"),
                (Page::Phased, "phased", icon::GRID_FOUR, "Phased"),
            ]),
            ("Signals", &[
                (Page::DirectionFinding, "df", icon::CROSSHAIR, "DF"),
                (Page::SignalChain, "chain", icon::CPU, "Chain"),
                (Page::Radar, "radar", icon::BROADCAST, "Radar"),
            ]),
        ],
    ];

    /// What a page's button says when it's pointed at.
    pub fn tip(page: Page) -> &'static str {
        match page {
            Page::Samples => "Ready-made antennas for real uses, to start from",
            Page::Gallery => "A new antenna from a template",
            Page::TagDesign => "Design an RFID tag from what it must do",
            Page::Draw => "The 3D model, with nothing else open",
            Page::Variables => "The variables, along the bottom",
            Page::Parts => "Add a part or a lumped port by its numbers, or import a drawing",
            Page::Reconfig => "Switches and varactors across gaps, and their states",
            Page::Materials => "The antenna's metal and substrate, and the library",
            Page::Setup => "Band, solver and mesh, and run it",
            Page::SParams => "S-parameters against frequency",
            Page::Smith => "The port's impedance on a Smith chart",
            Page::FarField => "The radiation pattern, gain and efficiency",
            Page::Fields => "Fields and surface currents on the model",
            Page::Array => "An array of this element, from its pattern",
            Page::Phased => "Beams from a phased array",
            Page::DirectionFinding => "How well an array finds a direction",
            Page::SignalChain => "The receive chain behind an array",
            Page::Radar => "A radar's range and speed picture",
            Page::Matching => "A matching network, from the solved impedance",
            Page::Mimo => "Coupling and correlation between ports",
            Page::Link => "A link budget from the solved gain",
            Page::Rcs => "How strongly it reflects a radar",
            Page::Gate => "Tags read through an RFID gate",
            Page::Sweep => "Solve across the values of a variable",
            Page::Optimize => "Change the variables ticked until the goals are met",
            Page::Tuning => "Sliders for the variables ticked, solved again as they move",
            Page::Converge => "Refine the mesh until the answer stops changing",
            Page::Yield => "What manufacturing tolerances do to it",
            Page::Export => "STL, DXF, Gerber, Touchstone and reports",
        }
    }

    /// The ribbon tab that has this page's button.
    pub fn tab_of(page: Page) -> Option<usize> {
        TABS.iter().position(|groups| groups.iter().any(|(_, buttons)| buttons.iter().any(|b| b.0 == page)))
    }
}

/// When the Studio's page changes by any route (its tree, the assistant, a finished run),
/// the ribbon goes to the tab that page's button is on. The 3D model goes to the modeler's
/// tools, and the modeler can ask for them as the work moves on.
#[cfg(feature = "antenna-studio")]
fn follow_studio_page(state: &mut UiState, slot: &mut LeftPanelSlot) {
    use crate::antenna::ui::Page;
    if !slot.studio_open {
        return;
    }
    let antenna = Workbench::Antenna as usize;
    let model = super::workbench::ANTENNA_MODEL_TAB;
    if slot.ribbon_studio_page != Some(slot.studio_page) {
        slot.ribbon_studio_page = Some(slot.studio_page);
        if slot.studio_page == Page::Draw {
            state.tool_tabs[antenna] = model;
            state.ribbon_tab = RibbonTab::Tools;
        } else if let Some(i) = antenna_ribbon::tab_of(slot.studio_page) {
            state.tool_tabs[antenna] = i;
        }
    }
    if slot.ribbon_tab_asked != slot.model_bar.tab_asked {
        slot.ribbon_tab_asked = slot.model_bar.tab_asked;
        // The assistant can work on the model while another page shows: that leaves the ribbon be.
        if slot.model_bar.showing {
            state.tool_tabs[antenna] = model;
            state.ribbon_tab = RibbonTab::Tools;
        }
    }
}

#[cfg(feature = "antenna-studio")]
fn antenna_tools(ui: &mut egui::Ui, tab: usize, slot: &mut LeftPanelSlot) {
    if tab == super::workbench::ANTENNA_MODEL_TAB {
        let bar = slot.model_bar;
        if let Some(cmd) = crate::antenna::ui::model_ribbon::tools(ui, &bar) {
            slot.model_asks.push(cmd);
        }
        return;
    }
    let groups = antenna_ribbon::TABS[tab.min(antenna_ribbon::TABS.len() - 1)];
    for (i, (caption, buttons)) in groups.iter().enumerate() {
        if i > 0 {
            theme::ribbon_divider(ui);
        }
        theme::ribbon_group(ui, caption, |ui| {
            for &(page, name, glyph, text) in *buttons {
                if theme::ribbon_button(ui, glyph, text, true, slot.studio_page == page, antenna_ribbon::tip(page)).clicked() {
                    slot.studio_goto.push(name.to_string());
                }
            }
            // The older way of drawing, for parts laid out by hand on one plane.
            if *caption == "Model" && theme::ribbon_button(ui, icon::NOTE_PENCIL, "Flat parts", true, false, "Draw flat parts by hand on one plane: traces, plates and meanders").clicked() {
                slot.studio_goto.push("flat-parts".to_string());
            }
        });
    }
}

#[cfg(not(feature = "antenna-studio"))]
fn antenna_tools(_ui: &mut egui::Ui, _tab: usize, _slot: &mut LeftPanelSlot) {}

fn plant_tools(ui: &mut egui::Ui, state: &mut UiState, slot: &mut LeftPanelSlot) {
    theme::ribbon_group(ui, "Build", |ui| {
        if theme::ribbon_button(ui, icon::CUBE, "Assets", true, false, "Show the asset library: pumps, tanks, conveyors and more").clicked() {
            state.sections.library = true;
        }
    });
    theme::ribbon_divider(ui);

    theme::ribbon_group(ui, "Run", |ui| {
        let running = slot.plant == Some(true);
        let (glyph, text, tip) = match slot.plant {
            Some(true) => (icon::PAUSE, "Pause", "Pause the plant"),
            Some(false) => (icon::PLAY, "Run", "Run the plant"),
            None => (icon::PLAY, "Run", "This scene has no connected plant yet"),
        };
        if theme::ribbon_button(ui, glyph, text, slot.plant.is_some(), running, tip).clicked() {
            slot.plant_toggle = true;
        }
    });
}

fn factory_tools(ui: &mut egui::Ui, tab: usize, slot: &mut LeftPanelSlot) {
    use crate::twin::{DEMO_SIZES, TwinPage, TwinRequest};
    let twin = &mut slot.twin;

    if tab == 0 {
        theme::ribbon_group(ui, "Production", |ui| {
            if theme::ribbon_button(ui, icon::FLOW_ARROW, "Lines", true, twin.open && twin.page == TwinPage::Lines, "Each production line as a pipeline of its machines: plan, made and cycle time").clicked() {
                twin.toggle_page(TwinPage::Lines);
            }
            if theme::ribbon_button(ui, icon::LIST_BULLETS, "Devices", true, twin.open && twin.page == TwinPage::Devices, "How many machines, PLCs and sensors are on the network, and how they are doing").clicked() {
                twin.toggle_page(TwinPage::Devices);
            }
            if theme::ribbon_button(ui, icon::PULSE, "Events", true, twin.open && twin.page == TwinPage::Events, "What went down, recovered or changed").clicked() {
                twin.toggle_page(TwinPage::Events);
            }
        });
        theme::ribbon_divider(ui);

        theme::ribbon_group(ui, "Display", |ui| {
            if theme::ribbon_button(ui, icon::FRAME_CORNERS, "Frame", twin.total > 0, false, "Move the camera to the devices").clicked() {
                twin.requests.push(TwinRequest::FrameDevices);
            }
            if theme::ribbon_button(ui, icon::INFO, "Labels", true, twin.labels, "Name the devices nearest the camera. Failing ones are always named").clicked() {
                twin.labels = !twin.labels;
            }
        });
        return;
    }

    theme::ribbon_group(ui, "Connect", |ui| {
        if theme::ribbon_button(ui, icon::PLUG, "Connection", true, twin.open && twin.page == TwinPage::Connection, "The Sense Twin service, and what a scan can and can't tell").clicked() {
            twin.toggle_page(TwinPage::Connection);
        }
        let demo = twin.is_demo();
        if theme::ribbon_button(ui, icon::PLAY_CIRCLE, "Demo", true, demo, "Try this workspace with invented devices").clicked() {
            twin.requests.push(TwinRequest::Demo(if demo { None } else { Some(DEMO_SIZES[0]) }));
        }
        theme::ribbon_button(ui, icon::MAGNIFYING_GLASS, "Scan", false, false, "Network scanning arrives in a later version, through the operator console");
    });
}

fn view_tools(ui: &mut egui::Ui, state: &mut UiState, actions: &mut Actions, settings: &mut ViewSettings, assistant_open: &mut bool, has_library: bool, slot: &mut LeftPanelSlot) {
    let scene = !slot.canvas_taken();
    fn toggle(ui: &mut egui::Ui, glyph: &str, text: &str, value: &mut bool, tip: &str) {
        if theme::ribbon_button(ui, glyph, text, true, *value, tip).clicked() {
            *value = !*value;
        }
    }

    // These are about the scene; a workspace with the canvas has its own.
    if scene {
        theme::ribbon_group(ui, "Camera", |ui| {
            if theme::ribbon_button(ui, icon::FRAME_CORNERS, "Frame all", true, false, "Fit the whole scene in the view (F)").clicked() {
                actions.0.push(Action::FrameAll);
            }
        });
        theme::ribbon_divider(ui);

        theme::ribbon_group(ui, "Display", |ui| {
            toggle(ui, icon::GRID_FOUR, "Grid", &mut settings.grid, "Show the floor grid");
            toggle(ui, icon::BOUNDING_BOX, "Bounds", &mut settings.bounds, "Outline the object under the cursor");
            toggle(ui, icon::SUN, "Shadows", &mut settings.shadows, "Cast shadows from the sun");
            if theme::ribbon_button(ui, icon::SLIDERS_HORIZONTAL, "Lighting", true, false, "Lighting and viewport settings").clicked() {
                state.open_settings(SettingsTab::Viewport);
            }
        });
        theme::ribbon_divider(ui);

        theme::ribbon_group(ui, "Performance", |ui| {
            toggle(ui, icon::GAUGE, "Sampling", &mut settings.sampling, "Draw coarser meshes when zoomed out and skip parts too small to see");
            toggle(ui, icon::CUBE_TRANSPARENT, "Occlusion", &mut settings.occlusion, "Skip parts hidden behind others");
        });
        theme::ribbon_divider(ui);
    }

    theme::ribbon_group(ui, "Panels", |ui| {
        if slot.studio_open {
            if theme::ribbon_button(ui, icon::TREE_STRUCTURE, "Project", true, !slot.studio_hide_left, "The project tree and the parameters of what's picked in it").clicked() {
                slot.studio_hide_left = !slot.studio_hide_left;
            }
            if theme::ribbon_button(ui, icon::SLIDERS_HORIZONTAL, "Variables", true, !slot.studio_hide_bottom, "The variables, messages and jobs along the bottom").clicked() {
                slot.studio_hide_bottom = !slot.studio_hide_bottom;
            }
            toggle(ui, icon::SPARKLE, "Assistant", assistant_open, "The AI assistant");
            return;
        }
        if theme::ribbon_button(ui, icon::CUBE, "Library", has_library, state.sections.library && has_library, "The asset library").clicked() {
            state.sections.library = !state.sections.library;
        }
        toggle(ui, icon::TREE_STRUCTURE, "Scene", &mut state.sections.scene, "The list of objects in the scene");
        toggle(ui, icon::CLOUD_ARROW_DOWN, "Downloads", &mut state.downloads_open, "Asset downloads");
        toggle(ui, icon::SPARKLE, "Assistant", assistant_open, "The AI assistant");
    });
    theme::ribbon_divider(ui);

    theme::ribbon_group(ui, "App", |ui| {
        if theme::ribbon_button(ui, icon::GEAR_SIX, "Settings", true, false, "Sense Sim settings").clicked() {
            state.settings_open = true;
        }
    });
}

#[cfg(all(test, feature = "antenna-studio"))]
mod tests {
    use super::antenna_ribbon::{TABS, tab_of};
    use super::super::workbench::ANTENNA_MODEL_TAB;
    use super::*;
    use crate::antenna::ui::Page as Studio;

    fn tab_named(name: &str) -> usize {
        Workbench::Antenna.tabs().iter().position(|t| *t == name).unwrap()
    }

    #[test]
    fn the_antenna_workspace_has_a_tool_list_for_each_of_its_tabs() {
        assert_eq!(TABS.len(), Workbench::Antenna.tabs().len());
        assert_eq!(ANTENNA_MODEL_TAB, tab_named("Model"));
        assert!(TABS[ANTENNA_MODEL_TAB].is_empty(), "its tools are the modeler's own");
    }

    /// With the list of pages gone from the left of the Studio, this is the only way to reach them.
    #[test]
    fn every_antenna_page_has_exactly_one_button_that_really_opens_it() {
        let mut seen = Vec::new();
        for groups in TABS {
            for (_, buttons) in groups {
                for (page, name, _, _) in *buttons {
                    assert_eq!(Studio::from_name(name), Some(*page), "the button named {name} opens another page");
                    assert!(!seen.contains(page), "{page:?} has two buttons");
                    seen.push(*page);
                }
            }
        }
        assert_eq!(seen.len(), Studio::Export as usize + 1, "a page has no button");
    }

    #[test]
    fn the_ribbon_follows_the_page_to_its_tab() {
        assert_eq!(tab_of(Studio::Variables), Some(tab_named("Design")));
        assert_eq!(tab_of(Studio::Setup), Some(tab_named("Solve")));
        assert_eq!(tab_of(Studio::Tuning), Some(tab_named("Solve")));
        assert_eq!(tab_of(Studio::SParams), Some(tab_named("Results")), "a finished run lands on its results, so the ribbon must go there too");
        assert_eq!(tab_of(Studio::Radar), Some(tab_named("Arrays")));
    }

    fn on_page(state: &mut UiState, slot: &mut LeftPanelSlot, page: Studio) {
        slot.studio_page = page;
        follow_studio_page(state, slot);
    }

    #[test]
    fn opening_the_3d_model_shows_its_tools_on_the_ribbon() {
        let (mut state, mut slot) = (UiState::default(), LeftPanelSlot::default());
        slot.studio_open = true;
        on_page(&mut state, &mut slot, Studio::Gallery);
        assert_eq!(state.tool_tabs[Workbench::Antenna as usize], tab_named("Design"));
        state.ribbon_tab = RibbonTab::View;
        on_page(&mut state, &mut slot, Studio::Draw);
        assert_eq!(state.tool_tabs[Workbench::Antenna as usize], tab_named("Model"));
        assert!(state.ribbon_tab == RibbonTab::Tools, "from View too, or the tools still don't show");
    }

    #[test]
    fn closing_a_page_leaves_the_ribbon_where_it_is() {
        let (mut state, mut slot) = (UiState::default(), LeftPanelSlot::default());
        slot.studio_open = true;
        on_page(&mut state, &mut slot, Studio::SParams);
        // What the close button does.
        slot.ribbon_studio_page = Some(Studio::Draw);
        on_page(&mut state, &mut slot, Studio::Draw);
        assert_eq!(state.tool_tabs[Workbench::Antenna as usize], tab_named("Results"), "the next plot is one click away");
    }

    #[test]
    fn the_ribbon_goes_where_the_modeler_asks_and_only_when_it_shows() {
        let (mut state, mut slot) = (UiState::default(), LeftPanelSlot::default());
        slot.studio_open = true;
        on_page(&mut state, &mut slot, Studio::SParams);
        slot.model_bar.tab_asked += 1;
        follow_studio_page(&mut state, &mut slot);
        assert_eq!(state.tool_tabs[Workbench::Antenna as usize], tab_named("Results"), "not while the model isn't showing");
        slot.model_bar.showing = true;
        slot.model_bar.tab_asked += 1;
        follow_studio_page(&mut state, &mut slot);
        assert_eq!(state.tool_tabs[Workbench::Antenna as usize], tab_named("Model"));
    }
}
