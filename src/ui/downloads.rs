//! Downloads tab: active and queued Hugging Face downloads.

use bevy_egui::egui::{self, RichText, vec2};

use super::{Filter, UiState};
use crate::library::{DownloadState, Library};
use crate::theme::{self, ONLINE, icon};

pub fn show(ui: &mut egui::Ui, library: &mut Library, state: &mut UiState) {
    let list = library.downloads();
    let queued = list.iter().filter(|(_, s)| matches!(s, DownloadState::Queued)).count();
    if queued > 0 && ui.add(egui::Button::new(theme::label(icon::X, "Cancel queued")).small()).clicked() {
        library.cancel_queued();
    }
    ui.label(RichText::new("Two downloads run at a time. Assets you clicked are placed when they finish.").small().color(theme::WEAK));
    ui.add_space(2.0);

    if list.is_empty() {
        ui.label(RichText::new("Online assets (blue, with a cloud badge) download when you click them in the library.").small().color(theme::WEAK));
        ui.vertical_centered(|ui| {
            if ui.button(theme::label(icon::GLOBE, "Browse online assets")).clicked() {
                state.filter = Filter::Online;
                state.sections.library = true;
            }
        });
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
