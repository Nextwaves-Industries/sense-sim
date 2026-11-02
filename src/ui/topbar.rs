//! Top bar: logo, scene name and state, save and settings on the left, the
//! File / Edit / View menus, then downloads, assistant and help on the right.

use bevy_egui::egui::{self, Align, Align2, FontId, Layout, Margin, RichText, Sense, Stroke, StrokeKind, vec2};

use super::{LeftPanelSlot, SettingsTab, UiState};
use crate::agent::Agent;
use crate::library::Library;
use crate::prototype::PrototypeCache;
use crate::scene::{Action, Actions, Selection, ViewSettings};
use crate::theme::{self, ACCENT, icon};

/// Commands asked for this frame, by shortcut or from here.
pub struct Flags {
    pub new: bool,
    pub open: bool,
    pub save: bool,
    pub save_as: bool,
    pub close: bool,
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("Left click", "Select / place / drag"),
    ("Orange ring", "Rotate"),
    ("Purple / teal ring", "Tilt / roll a device"),
    ("Green arrow", "Lift"),
    ("White dots", "Move joints"),
    ("Right drag", "Orbit"),
    ("Middle drag / Shift+Right", "Pan"),
    ("Wheel", "Zoom"),
    ("W A S D  Space C", "Fly camera"),
    ("F", "Focus selection / frame all"),
    ("Q / E", "Rotate preview (Shift: 90°)"),
    ("Alt drag / Ctrl drag", "Keep height / snap"),
    ("Ctrl+D", "Duplicate"),
    ("Del", "Delete"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("Ctrl+S / Ctrl+O", "Save / open layouts"),
    ("Ctrl+N / Ctrl+T", "New scene in a tab"),
    ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"),
    ("Ctrl+W", "Close tab"),
    ("Esc", "Cancel / deselect"),
];

/// The keyboard and mouse reference, for the Help menu and the nav cube's "?".
pub fn shortcuts_grid(ui: &mut egui::Ui) {
    ui.label(RichText::new("Keyboard & mouse").font(theme::bold(14.0)));
    ui.add_space(4.0);
    egui::Grid::new("shortcuts").num_columns(2).spacing([16.0, 5.0]).show(ui, |ui| {
        for (k, v) in SHORTCUTS {
            ui.label(RichText::new(*k).monospace().color(ACCENT));
            ui.label(*v);
            ui.end_row();
        }
    });
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

/// Outlined button with an icon, text and an optional count.
fn outlined(ui: &mut egui::Ui, glyph: &str, text: &str, count: usize) -> egui::Response {
    let font = FontId::proportional(14.0);
    let tw = ui.painter().layout_no_wrap(text.to_string(), font.clone(), theme::TEXT).size().x;
    let extra = if count > 0 { 24.0 } else { 0.0 };
    let (rect, r) = ui.allocate_exact_size(vec2(tw + 46.0 + extra, 32.0), Sense::click());
    let p = ui.painter();
    p.rect(rect, 5.0, if r.hovered() { theme::HOVER } else { theme::PANEL }, Stroke::new(1.0, egui::Color32::from_rgb(214, 218, 224)), StrokeKind::Inside);
    p.text(rect.left_center() + vec2(18.0, 0.0), Align2::CENTER_CENTER, glyph, FontId::proportional(16.0), theme::TEXT);
    p.text(rect.left_center() + vec2(34.0, 0.0), Align2::LEFT_CENTER, text, font, theme::TEXT);
    if count > 0 {
        let c = rect.right_center() - vec2(16.0, 0.0);
        p.circle_filled(c, 9.0, ACCENT);
        p.text(c, Align2::CENTER_CENTER, count.min(99).to_string(), theme::bold(10.5), egui::Color32::WHITE);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    root: &mut egui::Ui,
    state: &mut UiState,
    library: &mut Library,
    agent: &mut Agent,
    actions: &mut Actions,
    settings: &mut ViewSettings,
    cache: &mut PrototypeCache,
    undo_req: &mut crate::undo::UndoRequest,
    undo_stack: &crate::undo::UndoStack,
    selection: &Selection,
    slot: &mut LeftPanelSlot,
    flags: &mut Flags,
) {
    let scene = !slot.canvas_taken();
    let thing = if slot.studio_open { "antenna project" } else if slot.design_open { "design" } else { "layout" };
    let (active_dl, queued_dl) = library.download_count();
    egui::Panel::top("menu").exact_size(46.0).frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(10, 6))).show(root, |ui| {
        ui.horizontal_centered(|ui| {
            if let Some(logo) = &state.logo {
                ui.add(egui::Image::new(logo).fit_to_exact_size(vec2(26.0, 26.0)));
            }
            ui.add_space(2.0);

            // What is in front: its name (click to rename) and where it lives.
            let name_font = theme::bold(16.0);
            let design = slot.design_info.designs.get(slot.design_info.active).map_or_else(|| "3D Design".to_string(), |d| d.0.clone());
            let shown = if slot.studio_open {
                slot.studio_doc.name.clone()
            } else if slot.design_open {
                design.clone()
            } else if state.scene_name.is_empty() {
                crate::APP_NAME.to_string()
            } else {
                state.scene_name.clone()
            };
            let w = ui.painter().layout_no_wrap(shown.clone(), name_font.clone(), theme::TEXT).size().x + 6.0;
            if slot.studio_open {
                let mut name = shown;
                let r = ui.add(egui::TextEdit::singleline(&mut name).font(name_font).frame(egui::Frame::NONE).desired_width(w.clamp(40.0, 280.0)).id_salt("studio_name")).on_hover_text("Antenna project name: click to rename");
                if r.changed() {
                    slot.studio_rename = Some(name);
                }
            } else if slot.design_open {
                ui.label(RichText::new(shown).font(name_font));
            } else {
                ui.add(egui::TextEdit::singleline(&mut state.scene_name).font(name_font).frame(egui::Frame::NONE).hint_text(crate::APP_NAME).desired_width(w.clamp(40.0, 280.0)).id_salt("scene_name"))
                    .on_hover_text("Scene name: click to rename");
            }

            let (pill, tip) = if slot.studio_open {
                let doc = &slot.studio_doc;
                match &doc.file {
                    Some(f) if doc.unsaved => (format!("{f} ●"), format!("{} has unsaved changes", doc.path.clone().unwrap_or_default())),
                    Some(f) => (f.clone(), doc.path.clone().unwrap_or_default()),
                    None => ("Draft".to_string(), "This antenna project isn't saved to a file yet".to_string()),
                }
            } else if slot.design_open {
                let unsaved = slot.design_info.designs.get(slot.design_info.active).is_some_and(|d| d.1);
                ((if unsaved { "Unsaved" } else { "Design" }).to_string(), "Save and open designs from the Design file menu".to_string())
            } else {
                match &state.layout_path {
                    Some(p) => (p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(), p.display().to_string()),
                    None => ("Draft".to_string(), "Not saved to a file yet".to_string()),
                }
            };
            let text = format!("{} {pill}", icon::FILE_TEXT);
            let g = ui.painter().layout_no_wrap(text, FontId::proportional(12.0), egui::Color32::WHITE);
            let (rect, r) = ui.allocate_exact_size(g.size() + vec2(18.0, 6.0), Sense::hover());
            ui.painter().rect_filled(rect, rect.height() / 2.0, theme::PURPLE);
            ui.painter().galley(rect.min + vec2(9.0, 3.0), g, egui::Color32::WHITE);
            r.on_hover_text(tip);
            ui.add_space(4.0);

            if ui.add(egui::Button::new("Save").min_size(vec2(0.0, 32.0))).on_hover_text(format!("Save the {thing} (Ctrl+S)")).clicked() {
                flags.save = true;
            }
            if theme::icon_button(ui, icon::COPY, "Save as… (Ctrl+Shift+S)").clicked() {
                flags.save_as = true;
            }
            if theme::icon_button(ui, icon::GEAR_SIX, "Settings").clicked() {
                state.settings_open = true;
            }
            ui.add_space(6.0);

            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.set_min_width(230.0);
                    if slot.studio_open {
                        flags.new |= menu_entry(ui, icon::PLUS, "New antenna…", "Ctrl+N", true);
                        flags.open |= menu_entry(ui, icon::FOLDER_OPEN, "Open antenna project…", "Ctrl+O", true);
                        flags.save |= menu_entry(ui, icon::FLOPPY_DISK, "Save antenna project", "Ctrl+S", true);
                        flags.save_as |= menu_entry(ui, icon::FLOPPY_DISK, "Save antenna project as…", "Ctrl+Shift+S", true);
                        ui.separator();
                    } else if slot.design_open {
                        flags.new |= menu_entry(ui, icon::PLUS, "New design", "Ctrl+N", true);
                        flags.save |= menu_entry(ui, icon::FLOPPY_DISK, "Save design", "Ctrl+S", true);
                        ui.separator();
                    } else {
                        flags.new |= menu_entry(ui, icon::PLUS, "New…", "Ctrl+N", true);
                        flags.open |= menu_entry(ui, icon::FOLDER_OPEN, "Open layouts…", "Ctrl+O", true);
                        flags.close |= menu_entry(ui, icon::X, "Close tab", "Ctrl+W", true);
                        flags.save |= menu_entry(ui, icon::FLOPPY_DISK, "Save layout", "Ctrl+S", true);
                        flags.save_as |= menu_entry(ui, icon::FLOPPY_DISK, "Save layout as…", "Ctrl+Shift+S", true);
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
                    }
                    if menu_entry(ui, icon::SIGN_OUT, "Quit", "", true) {
                        crate::quit();
                    }
                });
                // Editing and viewing the scene: the workspaces have their own.
                if scene {
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
                }
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_space(2.0);
                ui.menu_button(format!("Help  {}", icon::CARET_DOWN), |ui| {
                    ui.set_min_width(340.0);
                    shortcuts_grid(ui);
                    ui.separator();
                    ui.label(RichText::new(format!("{} {}", crate::APP_NAME, env!("CARGO_PKG_VERSION"))).small().color(theme::WEAK));
                });

                // Assistant: outlined with a blue-to-coral stroke.
                let (rect, r) = ui.allocate_exact_size(vec2(118.0, 32.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(rect, 5.0, if agent.open { theme::ACCENT_SOFT } else if r.hovered() { theme::HOVER } else { theme::PANEL });
                theme::gradient_outline(p, rect, 5.0, ACCENT, egui::Color32::from_rgb(232, 96, 86));
                p.text(rect.left_center() + vec2(18.0, 0.0), Align2::CENTER_CENTER, icon::SPARKLE, FontId::proportional(16.0), ACCENT);
                p.text(rect.left_center() + vec2(34.0, 0.0), Align2::LEFT_CENTER, "AI Assistant", FontId::proportional(14.0), theme::TEXT);
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Build scenes from instructions or a reference photo").clicked() {
                    agent.open = !agent.open;
                }

                let n = active_dl + queued_dl;
                let tip = if n > 0 { format!("Downloads: {active_dl} active, {queued_dl} queued") } else { "Downloads".into() };
                if outlined(ui, icon::CLOUD_ARROW_DOWN, "Downloads", n).on_hover_text(tip).clicked() {
                    state.sections.downloads = !state.sections.downloads;
                }
            });
        });
    });
}
