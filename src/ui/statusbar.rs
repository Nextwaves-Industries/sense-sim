//! Dark bar along the bottom: object count, loading / download activity and the
//! latest status message on the left; the selection and frame rate on the right.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy_egui::egui::{self, Align, Layout, Margin, RichText, Stroke};

use super::UiState;
use crate::library::Library;
use crate::lod::{RenderStats, short_count};
use crate::prototype::PrototypeCache;
use crate::scene::Status;
use crate::theme::{self, STATUS_TEXT, STATUS_WEAK, icon};

#[allow(clippy::too_many_arguments)]
pub fn show(
    root: &mut egui::Ui,
    diagnostics: &DiagnosticsStore,
    library: &mut Library,
    cache: &PrototypeCache,
    status: &mut Status,
    state: &mut UiState,
    objects: usize,
    stats: RenderStats,
    selected: Option<&str>,
    selected_more: usize,
    scene_showing: bool,
) {
    egui::Panel::bottom("status")
        .exact_size(26.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(theme::STATUS_BAR).inner_margin(Margin::symmetric(10, 3)))
        .show(root, |ui| {
            // Light-on-dark for everything in this bar.
            {
                let v = ui.visuals_mut();
                v.override_text_color = Some(STATUS_TEXT);
                v.widgets.hovered.weak_bg_fill = egui::Color32::from_gray(70);
                v.widgets.active.weak_bg_fill = egui::Color32::from_gray(84);
            }
            ui.style_mut().override_text_style = Some(egui::TextStyle::Small);

            // The right side first (it is pinned), then the rest left to right, so a
            // long status message truncates instead of pushing it off screen.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // Frame rate, in an outlined pill. The shown value refreshes twice a
                // second so the number doesn't flicker.
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
                    egui::Color32::from_rgb(92, 212, 136)
                } else if fps >= 25.0 {
                    egui::Color32::from_rgb(240, 180, 80)
                } else {
                    egui::Color32::from_rgb(244, 118, 110)
                };
                egui::Frame::new().stroke(Stroke::new(1.0, c)).corner_radius(9).inner_margin(Margin::symmetric(9, 0)).show(ui, |ui| {
                    // Fixed width, so the bar doesn't shift as the digits change.
                    ui.add_sized([44.0, 16.0], egui::Label::new(RichText::new(format!("{fps:.0} fps")).color(c)));
                });
                let (sep, _) = ui.allocate_exact_size(egui::vec2(1.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(sep, 0.0, egui::Color32::from_gray(90));

                // The selection.
                if scene_showing {
                    let text = match (selected, selected_more) {
                        (Some(n), 0) => n.to_string(),
                        (Some(n), more) => format!("{n} + {more} more"),
                        (None, _) => "Nothing selected".to_string(),
                    };
                    ui.add(egui::Label::new(RichText::new(text).color(if selected.is_some() { STATUS_TEXT } else { STATUS_WEAK })).truncate());
                    ui.label(RichText::new(icon::CURSOR).color(STATUS_WEAK));
                }

                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if scene_showing {
                        ui.label(RichText::new(format!("{} {objects} objects", icon::CUBE)).color(STATUS_WEAK));
                    }
                    if stats.total > 0 {
                        ui.label(RichText::new("·").color(STATUS_WEAK));
                        ui.label(RichText::new(format!("{} of {} triangles", short_count(stats.drawn), short_count(stats.total))).color(STATUS_WEAK))
                            .on_hover_text("Triangles drawn now, out of all in the scene. What is off screen, too small to see or far away costs less.");
                    }

                    let loading = cache.loading_count();
                    if loading > 0 {
                        ui.label(RichText::new("·").color(STATUS_WEAK));
                        ui.add(egui::Spinner::new().size(11.0).color(STATUS_TEXT));
                        ui.label(format!("loading {loading}"));
                    }
                    let (active, queued) = library.download_count();
                    if active + queued > 0 {
                        ui.label(RichText::new("·").color(STATUS_WEAK));
                        let text = format!("{} downloading {active}, queued {queued}", icon::CLOUD_ARROW_DOWN);
                        let r = ui.add(egui::Label::new(RichText::new(text).color(egui::Color32::from_rgb(140, 190, 255))).sense(egui::Sense::click()));
                        if r.on_hover_text("Show downloads").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            state.sections.downloads = true;
                        }
                    }
                    if !status.0.is_empty() {
                        ui.label(RichText::new("·").color(STATUS_WEAK));
                        ui.label(RichText::new(icon::INFO).color(egui::Color32::from_rgb(140, 190, 255)));
                        // Dismiss button pinned right of the message; the message truncates.
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(egui::Button::new(RichText::new(icon::X).color(STATUS_WEAK)).frame_when_inactive(false).small())
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
