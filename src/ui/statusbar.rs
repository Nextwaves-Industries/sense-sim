//! Bottom status bar: fps, object count, loading / download activity, the
//! latest status message and a keyboard shortcut reference.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy_egui::egui::{self, Align, Layout, Margin, RichText};

use super::{LeftTab, UiState};
use crate::library::Library;
use crate::prototype::PrototypeCache;
use crate::scene::Status;
use crate::theme::{self, icon};

const SHORTCUTS: &[(&str, &str)] = &[
    ("Left click", "Select / place / drag"),
    ("Orange ring", "Rotate"),
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
    ("Ctrl+S / Ctrl+O", "Save / open layout"),
    ("Esc", "Cancel / deselect"),
];

fn dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

pub fn show(
    root: &mut egui::Ui,
    diagnostics: &DiagnosticsStore,
    library: &mut Library,
    cache: &PrototypeCache,
    status: &mut Status,
    state: &mut UiState,
    objects: usize,
) {
    egui::Panel::bottom("status").exact_size(26.0).frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin::symmetric(10, 3))).show(root, |ui| {
        // Shortcuts first (pinned right), then the rest left to right so a long
        // status message truncates instead of pushing the menu off screen.
        ui.style_mut().override_text_style = Some(egui::TextStyle::Small);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.menu_button(RichText::new(format!("{}  Shortcuts", icon::KEYBOARD)).color(theme::WEAK), |ui| {
                ui.style_mut().override_text_style = Some(egui::TextStyle::Body);
                ui.label(RichText::new("Keyboard & mouse").strong());
                ui.add_space(4.0);
                egui::Grid::new("shortcuts").num_columns(2).spacing([16.0, 5.0]).show(ui, |ui| {
                    for (k, v) in SHORTCUTS {
                        ui.label(RichText::new(*k).monospace().color(theme::ACCENT));
                        ui.label(*v);
                        ui.end_row();
                    }
                });
            });
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                // Refresh the shown value twice a second so the number doesn't flicker.
                let now = ui.input(|i| i.time);
                let id = egui::Id::new("status_fps");
                let (shown, at) = ui.data(|d| d.get_temp::<(f64, f64)>(id)).unwrap_or((0.0, f64::NEG_INFINITY));
                let fps = if now - at >= 0.5 {
                    let v = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
                    ui.data_mut(|d| d.insert_temp(id, (v, now)));
                    v
                } else {
                    shown
                };
                let c = if fps >= 50.0 {
                    theme::SUCCESS
                } else if fps >= 25.0 {
                    theme::ACCENT
                } else {
                    theme::DANGER
                };
                dot(ui, c);
                // Fixed width, so the rest of the bar doesn't shift as the digits change.
                ui.add_sized([52.0, 16.0], egui::Label::new(RichText::new(format!("{fps:.0} fps")).color(theme::WEAK)));
                ui.label(RichText::new("·").color(theme::WEAK));
                ui.label(RichText::new(format!("{} {objects} objects", icon::CUBE)).color(theme::WEAK));

                let loading = cache.loading_count();
                if loading > 0 {
                    ui.label(RichText::new("·").color(theme::WEAK));
                    ui.add(egui::Spinner::new().size(11.0));
                    ui.label(format!("loading {loading}"));
                }
                let (active, queued) = library.download_count();
                if active + queued > 0 {
                    ui.label(RichText::new("·").color(theme::WEAK));
                    let text = format!("{} downloading {active}, queued {queued}", icon::CLOUD_ARROW_DOWN);
                    let r = ui.add(egui::Label::new(RichText::new(text).color(theme::ONLINE)).sense(egui::Sense::click()));
                    if r.on_hover_text("Show downloads").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        state.open_tab(LeftTab::Downloads);
                    }
                }
                if !status.0.is_empty() {
                    ui.label(RichText::new("·").color(theme::WEAK));
                    ui.label(RichText::new(icon::INFO).color(theme::ACCENT));
                    // Dismiss button pinned right of the message; the message truncates.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new(icon::X).color(theme::WEAK)).frame_when_inactive(false).small())
                            .on_hover_text("Dismiss")
                            .clicked()
                        {
                            status.0.clear();
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(status.0.as_str()).truncate()).on_hover_text(&status.0);
                        });
                    });
                }
            });
        });
    });
}
