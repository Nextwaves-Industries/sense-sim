//! The workspace tile at the left end of the ribbon, and the "Change workspace" list
//! that hangs from it. The workspaces of the project in front come first; the other
//! kind of project follows, set apart, because going there changes what is being
//! worked on and not just the tools.

use bevy_egui::egui::{self, Margin, RichText, vec2};

use super::workbench::{self, ProjectKind, Workbench};
use super::{LeftPanelSlot, UiState};
use crate::rtls::ui::RtlsUiState;
use crate::theme::{self, WorkspaceRow};

const WIDTH: f32 = 580.0;

/// The tile: which workspace is in front. Clicking opens the list.
pub fn tile(ui: &mut egui::Ui, state: &mut UiState, slot: &LeftPanelSlot, height: f32) {
    let bench = slot.bench.current;
    // Something going on behind another workspace; a problem wins over good news.
    let behind: Vec<_> = Workbench::ALL.into_iter().filter(|b| *b != bench).filter_map(|b| slot.activity(b)).map(|(_, c)| c).collect();
    let dot = behind.iter().copied().find(|c| *c == theme::DANGER).or(behind.first().copied());
    let r = theme::workspace_tile(ui, bench.glyph(), bench.name(), bench.color(), state.workspace_open, dot, height);
    state.workspace_anchor = Some(r.rect);
    if r.on_hover_text("Change workspace").clicked() {
        state.workspace_open = !state.workspace_open;
    }
}

fn tabs_line(bench: Workbench) -> String {
    let mut names: Vec<&str> = bench.tabs().to_vec();
    names.push("View");
    names.join(" · ")
}

fn section(ui: &mut egui::Ui, title: &str) {
    egui::Frame::new().inner_margin(Margin { left: 14, right: 14, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(title).size(11.5).color(theme::WEAK)).truncate());
    });
}

/// A workspace's row. Returns true when it was clicked.
fn row(ui: &mut egui::Ui, slot: &LeftPanelSlot, bench: Workbench) -> bool {
    let current = slot.bench.current == bench;
    let here = slot.bench.current.project() == bench.project();
    let (chip, chip_color) = if current {
        ("Current", theme::ACCENT)
    } else if here {
        ("Available", bench.color())
    } else {
        ("Other project", theme::WEAK)
    };
    let tabs = tabs_line(bench);
    let activity = slot.activity(bench);
    let r = theme::workspace_row(ui, WIDTH, &WorkspaceRow {
        glyph: bench.glyph(),
        name: bench.name(),
        color: bench.color(),
        about: bench.tip(),
        tabs: &tabs,
        chip,
        chip_color,
        note: activity.as_ref().map(|(text, color)| (text.as_str(), *color)),
        current,
    });
    let r = if here {
        r
    } else if bench == Workbench::Antenna {
        r.on_hover_text("Opens the antenna project. Your scene stays open behind it.")
    } else {
        r.on_hover_text("Leaves the antenna project for your scene. The antenna stays open.")
    };
    r.clicked()
}

/// Shows the list under the tile. Run this last in the frame, like the Downloads list:
/// whatever opened it has had its say by then, and the click that opened it doesn't
/// count as a click elsewhere.
pub fn dropdown(ctx: &egui::Context, state: &mut UiState, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    if state.start_open {
        state.workspace_open = false;
    }
    let shown_id = egui::Id::new("workspaces_shown");
    let was_shown = ctx.data(|d| d.get_temp::<bool>(shown_id)).unwrap_or(false);
    ctx.data_mut(|d| d.insert_temp(shown_id, state.workspace_open));
    let (true, Some(anchor)) = (state.workspace_open, state.workspace_anchor) else { return };

    let screen = ctx.content_rect();
    let room = (screen.bottom() - anchor.bottom() - 16.0).max(160.0);
    let in_antenna = slot.bench.current.project() == ProjectKind::Antenna;
    let scene_name = state.scene_title().trim();
    let scene_title = if scene_name.is_empty() { "Scene project".to_string() } else { format!("Scene project · {scene_name}") };
    let antenna_title = if slot.studio_doc.name.trim().is_empty() { "Antenna project".to_string() } else { format!("Antenna project · {}", slot.studio_doc.name.trim()) };

    let mut close = false;
    let mut chosen = None;
    let area = egui::Area::new(egui::Id::new("workspaces_list")).order(egui::Order::Foreground).fixed_pos(anchor.left_bottom() + vec2(0.0, 4.0)).constrain_to(screen).show(ctx, |ui| {
        theme::fixed_card(ui, WIDTH, room, |ui| {
            theme::card_title(ui, "Change workspace", |ui| close = theme::close_button(ui, "Close (Esc)").clicked());
            egui::ScrollArea::vertical().id_salt("workspaces").max_height(room - 44.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let scene = |ui: &mut egui::Ui, chosen: &mut Option<Workbench>| {
                    section(ui, &scene_title);
                    for bench in Workbench::SCENE {
                        if row(ui, slot, bench) {
                            *chosen = Some(bench);
                        }
                    }
                };
                let antenna = |ui: &mut egui::Ui, chosen: &mut Option<Workbench>| {
                    section(ui, &antenna_title);
                    if row(ui, slot, Workbench::Antenna) {
                        *chosen = Some(Workbench::Antenna);
                    }
                };
                let offers_antenna = slot.shows(Workbench::Antenna);
                if in_antenna {
                    antenna(ui, &mut chosen);
                    ui.add_space(4.0);
                    theme::hairline(ui);
                    scene(ui, &mut chosen);
                } else {
                    scene(ui, &mut chosen);
                    if offers_antenna {
                        ui.add_space(4.0);
                        theme::hairline(ui);
                        antenna(ui, &mut chosen);
                    }
                }
                ui.add_space(4.0);
                theme::hairline(ui);
                let note = if in_antenna {
                    "An antenna project needs no scene. It stays open while you visit a scene's workspaces, and the scene stays open behind it."
                } else {
                    "Every workspace works on the same scene, history and selection; only the ribbon tabs and the tree change. A tool in progress stops when you change workspace."
                };
                egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(note).size(12.0).color(theme::WEAK)).wrap());
                });
            });
        });
    });

    if let Some(bench) = chosen {
        state.ribbon_tab = super::ribbon::RibbonTab::Tools;
        workbench::switch(slot, rtls, bench);
        close = true;
    }
    let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if close || escape || (was_shown && area.response.clicked_elsewhere()) {
        state.workspace_open = false;
    }
}
