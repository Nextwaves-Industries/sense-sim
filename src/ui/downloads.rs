//! The Downloads list under the top bar's button: active and queued Hugging Face downloads.

use bevy_egui::egui::{self, Align2, Margin, RichText, vec2};

use super::{Filter, UiState};
use crate::library::{DownloadState, Library};
use crate::theme::{self, ONLINE, icon};

const WIDTH: f32 = 340.0;

/// Shows the list under the button. Run this last in the frame: whatever opened the list has had
/// its say by then, and the click that opened it doesn't count as a click elsewhere.
pub fn dropdown(ctx: &egui::Context, state: &mut UiState, library: &mut Library, has_library: bool) {
    let shown_id = egui::Id::new("downloads_shown");
    let was_shown = ctx.data(|d| d.get_temp::<bool>(shown_id)).unwrap_or(false);
    ctx.data_mut(|d| d.insert_temp(shown_id, state.downloads_open));
    let (true, Some(anchor)) = (state.downloads_open, state.downloads_anchor) else { return };

    let screen = ctx.content_rect();
    let mut close = false;
    let area = egui::Area::new(egui::Id::new("downloads_dropdown")).order(egui::Order::Foreground).pivot(Align2::RIGHT_TOP).fixed_pos(anchor.right_bottom() + vec2(0.0, 4.0)).constrain_to(screen).show(ctx, |ui| {
        theme::fixed_card(ui, WIDTH, screen.bottom() - anchor.bottom() - 12.0, |ui| {
            let (active, queued) = library.download_count();
            theme::card_title(ui, &format!("Downloads ({})", active + queued), |ui| close = theme::close_button(ui, "Close").clicked());
            egui::Frame::new().inner_margin(Margin { left: 12, right: 12, top: 8, bottom: 10 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                show(ui, library, state, has_library);
            });
        });
    });
    if close || (was_shown && area.response.clicked_elsewhere()) {
        state.downloads_open = false;
    }
}

fn show(ui: &mut egui::Ui, library: &mut Library, state: &mut UiState, has_library: bool) {
    let list = library.downloads();
    let queued = list.iter().filter(|(_, s)| matches!(s, DownloadState::Queued)).count();
    if queued > 0 && ui.add(egui::Button::new(theme::label(icon::X, "Cancel queued")).small()).clicked() {
        library.cancel_queued();
    }
    ui.label(RichText::new("Two downloads run at a time. Assets you clicked are placed when they finish.").small().color(theme::WEAK));
    ui.add_space(2.0);

    if list.is_empty() {
        ui.label(RichText::new("Online assets (blue, with a cloud badge) download when you click them in the library.").small().color(theme::WEAK));
        // Not every workbench has a library to show them in.
        if has_library {
            ui.vertical_centered(|ui| {
                if ui.button(theme::label(icon::GLOBE, "Browse online assets")).clicked() {
                    state.filter = Filter::Online;
                    state.sections.library = true;
                    state.downloads_open = false;
                }
            });
        }
        return;
    }

    let mut cancel = None;
    egui::ScrollArea::vertical().id_salt("downloads").max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
        for (folder, st) in &list {
            let entry = library.entries.iter().position(|e| &e.folder == folder);
            if let Some(i) = entry {
                library.want_thumb(i);
            }
            let thumb = entry.and_then(|i| library.entries[i].thumb.clone());
            theme::card_frame().inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let size = vec2(44.0, 44.0);
                    match &thumb {
                        Some(t) => {
                            ui.add(egui::Image::new(t).fit_to_exact_size(size).corner_radius(6));
                        }
                        None => {
                            let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                            ui.painter().rect_filled(r, 5.0, theme::HOVER);
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, icon::CUBE, egui::FontId::proportional(18.0), theme::FAINT);
                        }
                    }
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(folder).strong()).truncate());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if matches!(st, DownloadState::Queued) && theme::icon_button(ui, icon::X, "Remove from queue").clicked() {
                                    cancel = Some(folder.clone());
                                }
                            });
                        });
                        match st {
                            DownloadState::Active { done, total } => {
                                let frac = if *total > 0 { (*done as f32 / *total as f32).min(1.0) } else { 0.0 };
                                ui.add(egui::ProgressBar::new(frac).desired_height(5.0).fill(ONLINE).corner_radius(3));
                                ui.label(
                                    RichText::new(format!("{:.0}% · {:.1} / {:.1} MB", frac * 100.0, *done as f64 / 1e6, *total as f64 / 1e6))
                                        .small()
                                        .color(theme::WEAK),
                                );
                            }
                            DownloadState::Queued => {
                                ui.label(RichText::new(format!("{}  Waiting", icon::CLOUD_ARROW_DOWN)).small().color(theme::WEAK));
                            }
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }
    });
    if let Some(f) = cancel {
        library.cancel_download(&f);
    }
}
