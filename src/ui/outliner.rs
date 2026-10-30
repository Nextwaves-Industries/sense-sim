//! Scene tab: every placed object, filterable; click to select,
//! double-click to focus, hover for quick actions.

use bevy::prelude::*;
use bevy_egui::egui::{self, Align2, FontId, RichText, Sense, vec2};

use super::{ObjectQuery, UiState};
use crate::scene::{Action, Actions, Selection};
use crate::theme::{self, ACCENT, icon};

const ROW_H: f32 = 30.0;

pub fn show(ui: &mut egui::Ui, objects: &mut ObjectQuery, selection: &Selection, actions: &mut Actions, state: &mut UiState) {
    let total = objects.iter().count();
    theme::panel_header(ui, "Scene", |ui| {
        if theme::icon_button(ui, icon::FRAME_CORNERS, "Frame all (F)").clicked() {
            actions.0.push(Action::FrameAll);
        }
        ui.label(RichText::new(total.to_string()).small().color(theme::WEAK));
    });
    super::library::search_box(ui, &mut state.scene_search, "Filter objects…");
    ui.add_space(4.0);

    let terms: Vec<String> = state.scene_search.to_lowercase().split_whitespace().map(str::to_string).collect();
    let mut list: Vec<(Entity, String, usize)> = objects
        .iter()
        .filter(|(_, o, _)| terms.iter().all(|t| o.name.to_lowercase().contains(t)))
        .map(|(e, o, _)| (e, o.name.clone(), o.proto.joints.len()))
        .collect();
    list.sort_by(|a, b| a.1.cmp(&b.1));

    if total == 0 {
        theme::empty_state(ui, icon::CUBE, "Nothing placed yet", "Pick an asset in the library and click in the viewport to place it.");
        return;
    }
    if list.is_empty() {
        theme::empty_state(ui, icon::MAGNIFYING_GLASS, "No matches", "No object name contains that text.");
        return;
    }

    let width = (ui.available_width() - ui.spacing().scroll.allocated_width() - 1.0).floor();
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, ROW_H, list.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (e, name, joints) in &list[range] {
            let e = *e;
            let selected = selection.selected == Some(e);
            let (rect, r) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
            let hovered = ui.rect_contains_pointer(rect);
            let p = ui.painter();
            if selected {
                p.rect_filled(rect, 6.0, ui.visuals().selection.bg_fill);
            } else if hovered {
                p.rect_filled(rect, 6.0, theme::HOVER.gamma_multiply(0.6));
            }
            let color = if selected { ACCENT } else { theme::TEXT };
            p.text(rect.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, icon::CUBE, FontId::proportional(14.0), if selected { ACCENT } else { theme::WEAK });
            let right_pad = if hovered { 66.0 } else { 30.0 };
            let galley = p.layout(name.clone(), FontId::proportional(13.0), color, rect.width() - 34.0 - right_pad);
            p.galley(egui::pos2(rect.left() + 32.0, rect.center().y - galley.size().y / 2.0), galley, color);
            if *joints > 0 && !hovered {
                p.text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, icon::DOOR_OPEN, FontId::proportional(13.0), theme::WEAK);
            }

            let mut handled = false;
            if hovered {
                // Quick actions on the hovered row.
                let mut x = rect.right() - 4.0;
                for (glyph, tip, action) in [(icon::TRASH, "Delete", Action::Delete(e)), (icon::COPY, "Duplicate", Action::Duplicate(e)), (icon::CROSSHAIR, "Focus", Action::Focus(e))] {
                    let b = egui::Rect::from_min_size(egui::pos2(x - 22.0, rect.center().y - 11.0), vec2(22.0, 22.0));
                    x -= 22.0;
                    let br = ui.interact(b, r.id.with(tip), Sense::click());
                    let c = if br.hovered() { egui::Color32::WHITE } else { theme::WEAK };
                    if br.hovered() {
                        ui.painter().rect_filled(b, 4.0, theme::HOVER);
                    }
                    ui.painter().text(b.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), c);
                    if br.on_hover_text(tip).clicked() {
                        actions.0.push(action);
                        handled = true;
                    }
                }
            }
            if !handled && r.clicked() {
                actions.0.push(Action::Select(Some(e)));
            }
            if r.double_clicked() {
                actions.0.push(Action::Focus(e));
            }
            r.on_hover_text(name.as_str()).context_menu(|ui| {
                ui.set_min_width(180.0);
                if theme::menu_item(ui, icon::CROSSHAIR, "Focus").clicked() {
                    actions.0.push(Action::Focus(e));
                    ui.close();
                }
                if theme::menu_item(ui, icon::COPY, "Duplicate").clicked() {
                    actions.0.push(Action::Duplicate(e));
                    ui.close();
                }
                ui.separator();
                if theme::menu_item_danger(ui, icon::TRASH, "Delete").clicked() {
                    actions.0.push(Action::Delete(e));
                    ui.close();
                }
            });
        }
    });
}
