//! The strip of open scenes under the ribbon, and the "save before closing?" dialog.

use bevy_egui::egui::{self, Align, Align2, FontId, Layout, Margin, RichText, Sense, Stroke, vec2};

use super::{UiState, topbar::Flags};
use crate::scene::{Action, Actions};
use crate::tabs::{Request, Tab, Tabs};
use crate::theme::{self, icon};

pub(super) const HEIGHT: f32 = 32.0;
pub(super) const MIN_TAB: f32 = 120.0;
pub(super) const MAX_TAB: f32 = 220.0;
const UNTITLED: &str = "New scene";

fn title(t: &Tab) -> &str {
    if t.title.is_empty() { UNTITLED } else { &t.title }
}

/// Asks before closing a tab with unsaved changes (showing it first, to save it).
fn ask_close(tabs: &mut Tabs, state: &mut UiState, id: u64) {
    if tabs.get(id).is_some_and(|t| t.unsaved) {
        state.closing = Some(id);
        tabs.request(Request::Activate(id));
    } else {
        tabs.request(Request::Close(id));
    }
}

/// Draws one tab; returns its response.
fn tab(ui: &mut egui::Ui, t: &Tab, active: bool, width: f32, tabs_out: &mut Vec<(u64, Click)>) -> egui::Response {
    let text_color = if t.not_loaded() {
        theme::WEAK
    } else {
        theme::TEXT
    };
    let glyph = if t.path.is_some() { icon::FILE_TEXT } else { icon::FILE_DASHED };
    let (r, over_close) = tab_face(ui, title(t), glyph, text_color, t.unsaved, active, width);
    if r.middle_clicked() || (r.clicked() && over_close) {
        tabs_out.push((t.id, Click::Close));
    } else if r.clicked() {
        tabs_out.push((t.id, Click::Show));
    }
    r
}

/// Draws a tab of the strip. Returns its response, and whether the pointer is on its close button.
pub(super) fn tab_face(ui: &mut egui::Ui, title: &str, glyph: &str, text_color: egui::Color32, unsaved: bool, active: bool, width: f32) -> (egui::Response, bool) {
    let (rect, r) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::click());
    let hovered = r.hovered();
    let p = ui.painter();
    let fill = if active {
        theme::PANEL
    } else if hovered {
        theme::HOVER
    } else {
        theme::BAND
    };
    p.rect_filled(rect, 0.0, fill);
    if active {
        p.hline(rect.x_range(), rect.top() + 1.0, Stroke::new(2.0, theme::ACCENT));
    }
    p.vline(rect.right(), rect.y_range(), Stroke::new(1.0, theme::STROKE));

    p.text(rect.left_center() + vec2(14.0, 0.0), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), text_color);

    // Room for the title: up to the close button, shown on the active or hovered tab.
    let left = 26.0;
    let right = 26.0;
    let mut job = egui::text::LayoutJob::simple_singleline(title.to_string(), FontId::proportional(13.0), text_color);
    job.wrap = egui::text::TextWrapping { max_width: (width - left - right).max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter().galley(egui::pos2(rect.left() + left, y), galley, text_color);

    let close_rect = egui::Rect::from_center_size(rect.right_center() - vec2(14.0, 0.0), vec2(18.0, 18.0));
    let show_close = active || hovered;
    let over_close = show_close && ui.rect_contains_pointer(close_rect);
    if over_close {
        ui.painter().rect_filled(close_rect, 3.0, theme::STROKE);
    }
    if show_close {
        ui.painter().text(close_rect.center(), Align2::CENTER_CENTER, icon::X, FontId::proportional(12.0), theme::WEAK);
    } else if unsaved {
        // Amber means attention: changes a close would lose.
        ui.painter().circle_filled(close_rect.center(), 3.5, theme::WARNING);
    }
    (r, over_close)
}

enum Click {
    Show,
    Close,
    CloseOthers,
}

pub fn show(root: &mut egui::Ui, tabs: &mut Tabs, state: &mut UiState, flags: &mut Flags) {
    let mut clicks: Vec<(u64, Click)> = Vec::new();
    let active = tabs.active_id();
    let reveal = std::mem::take(&mut tabs.reveal_active);
    egui::Panel::top("tabs").exact_size(HEIGHT).show_separator_line(true).frame(egui::Frame::new().fill(theme::BAND).inner_margin(Margin::ZERO)).show(root, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(4.0);
            // Every open scene, for when they no longer fit the strip.
            ui.menu_button(icon::CARET_DOWN, |ui| {
                ui.set_min_width(260.0);
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for t in tabs.list() {
                        let text = if t.unsaved { format!("{} ●", title(t)) } else { title(t).to_string() };
                        if ui.selectable_label(t.id == active, text).clicked() {
                            clicks.push((t.id, Click::Show));
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text(format!("All open scenes ({})", tabs.list().len()));
            if theme::icon_button(ui, icon::PLUS, "New scene (Ctrl+N)").clicked() {
                flags.new = true;
            }
            ui.add_space(4.0);

            let room = ui.available_width();
            let n = tabs.list().len().max(1) as f32;
            let width = (room / n).clamp(MIN_TAB, MAX_TAB);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.set_max_width(room);
                egui::ScrollArea::horizontal().auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    // A wheel over the strip scrolls it sideways.
                    if ui.rect_contains_pointer(ui.clip_rect()) {
                        let dy = ui.input(|i| i.smooth_scroll_delta.y);
                        if dy != 0.0 {
                            ui.scroll_with_delta(vec2(dy, 0.0));
                        }
                    }
                    ui.horizontal(|ui| {
                        for t in tabs.list() {
                            let r = tab(ui, t, t.id == active, width, &mut clicks);
                            if reveal && t.id == active {
                                r.scroll_to_me(Some(Align::Center));
                            }
                            let tip = match (&t.path, t.not_loaded()) {
                                (Some(p), true) => format!("{}\nOpens when you show this tab", p.display()),
                                (Some(p), false) => p.display().to_string(),
                                (None, _) => "Not saved to a file yet".into(),
                            };
                            let r = r.on_hover_text(tip);
                            r.context_menu(|ui| {
                                if ui.button("Close").clicked() {
                                    clicks.push((t.id, Click::Close));
                                    ui.close();
                                }
                                if ui.button("Close others").clicked() {
                                    clicks.push((t.id, Click::CloseOthers));
                                    ui.close();
                                }
                            });
                        }
                    });
                });
            });
        });
    });
    for (id, click) in clicks {
        match click {
            Click::Show => tabs.request(Request::Activate(id)),
            Click::Close => ask_close(tabs, state, id),
            Click::CloseOthers => tabs.request(Request::CloseOthers(id)),
        }
    }
}

/// File > Close, and its shortcut: whichever strip shows, it closes the scene's tab.
pub fn close_if_asked(tabs: &mut Tabs, state: &mut UiState, flags: &Flags) {
    if flags.close {
        let active = tabs.active_id();
        ask_close(tabs, state, active);
    }
}

/// Once the tab being closed is showing: save, discard, or keep it open.
pub fn confirm_close(ctx: &egui::Context, tabs: &mut Tabs, state: &mut UiState, actions: &mut Actions) {
    let Some(id) = state.closing else { return };
    let Some(t) = tabs.get(id) else {
        state.closing = None;
        return;
    };
    if tabs.active_id() != id {
        return;
    }
    let name = title(t).to_string();
    let width = 420.0;
    let modal = egui::Modal::new(egui::Id::new("close_tab_modal")).backdrop_color(theme::BACKDROP).frame(theme::modal_frame()).show(ctx, |ui| {
        ui.set_width(width);
        if theme::modal_title(ui, width, "Save changes?", true, "Keep the tab open (Esc)") {
            state.closing = None;
        }
        egui::Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.label(RichText::new(format!("\"{name}\" has changes that are not saved. They are lost if you close without saving.")).color(theme::TEXT));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "Save").clicked() {
                    let path = state.layout_path.as_deref().map(crate::project_file::save_path).or_else(|| super::pick_layout_save(&state.scene_name));
                    if let Some(p) = path {
                        state.layout_path = Some(p.clone());
                        actions.0.push(Action::SaveLayout(p));
                        tabs.request(Request::CloseIfSaved(id));
                        state.closing = None;
                    }
                }
                if ui.button("Don't save").clicked() {
                    tabs.request(Request::Close(id));
                    state.closing = None;
                }
                if ui.button("Cancel").clicked() {
                    state.closing = None;
                }
            });
        });
    });
    if modal.should_close() {
        state.closing = None;
    }
}
