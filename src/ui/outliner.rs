//! The scene tree on the right: every placed object under the scene's name,
//! filterable; click to select, double-click to focus, the eye hides an object,
//! the padlock locks it, hover for quick actions.

use bevy::prelude::*;
use bevy_egui::egui::{self, Align2, FontId, Margin, Sense, vec2};

use super::{ObjectQuery, UiState};
use crate::scene::{Action, Actions, Selection};
use crate::theme::{self, TreeRow, icon};

/// Above this many objects the tree gets a filter box.
const FILTER_FROM: usize = 8;

pub fn card(ctx: &egui::Context, rect: egui::Rect, objects: &mut ObjectQuery, selection: &Selection, actions: &mut Actions, state: &mut UiState) {
    let total = objects.iter().count();
    egui::Area::new(egui::Id::new("scene_tree")).order(egui::Order::Middle).fixed_pos(rect.min).show(ctx, |ui| {
        theme::fixed_card(ui, rect.width(), rect.height(), |ui| {
            let mut open = state.sections.scene;
            let title = if total > 0 { format!("Scene ({total})") } else { "Scene".to_string() };
            theme::section_header(ui, "scene", &mut open, "", theme::WEAK, &title, |_| {});
            state.sections.scene = open;
            if open {
                theme::hairline(ui);
                body(ui, rect, objects, selection, actions, state, total);
            }
        });
    });
}

fn body(ui: &mut egui::Ui, rect: egui::Rect, objects: &mut ObjectQuery, selection: &Selection, actions: &mut Actions, state: &mut UiState, total: usize) {
    if total == 0 {
        egui::Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
            theme::empty_state(ui, icon::CUBE, "Nothing placed yet", "Pick an asset in the library and click in the viewport to place it.");
        });
        return;
    }
    if total >= FILTER_FROM {
        egui::Frame::new().inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
            super::library::search_box(ui, &mut state.scene_search, "Filter objects…");
        });
    }

    let terms: Vec<String> = state.scene_search.to_lowercase().split_whitespace().map(str::to_string).collect();
    let mut list: Vec<(Entity, String, usize, bool, bool)> = objects
        .iter()
        .filter(|(_, o, _, _)| terms.iter().all(|t| o.name.to_lowercase().contains(t)))
        .map(|(e, o, _, v)| (e, o.name.clone(), o.proto.joints.len(), *v != Visibility::Hidden, o.locked))
        .collect();
    list.sort_by(|a, b| a.1.cmp(&b.1));

    let width = rect.width();

    // Root row: the scene, with an eye for all of it.
    let scene_name = if state.scene_name.is_empty() { "Scene" } else { state.scene_name.as_str() };
    let root_open = state.sections.scene_root;
    let (c, r) = theme::tree_row(ui, width, &TreeRow { depth: 0, text: scene_name, glyph: "", dot: theme::Dot::None, selected: false, expander: Some(root_open), guide: false, last: false, note: "", dim: false });
    if c.expander || c.row {
        state.sections.scene_root = !root_open;
    }
    let all_visible = objects.iter().all(|(_, _, _, v)| *v != Visibility::Hidden);
    let eye = egui::Rect::from_center_size(r.rect.right_center() - vec2(18.0, 0.0), vec2(24.0, 24.0));
    let eye_r = ui.interact(eye, r.id.with("eye_all"), Sense::click());
    ui.painter().text(eye.center(), Align2::CENTER_CENTER, if all_visible { icon::EYE } else { icon::EYE_SLASH }, FontId::proportional(16.0), if eye_r.hovered() { theme::INK } else { theme::WEAK });
    if eye_r.on_hover_text(if all_visible { "Hide all" } else { "Show all" }).clicked() {
        let to = if all_visible { Visibility::Hidden } else { Visibility::Visible };
        for (_, _, _, mut v) in objects.iter_mut() {
            *v = to;
        }
    }
    if !state.sections.scene_root {
        ui.add_space(4.0);
        return;
    }
    if list.is_empty() {
        theme::empty_state(ui, icon::MAGNIFYING_GLASS, "No matches", "No object name contains that text.");
        return;
    }

    let n = list.len();
    let mut toggle: Option<Entity> = None;
    let max_h = (rect.height() - 120.0).max(90.0);
    egui::ScrollArea::vertical().id_salt("scene_rows").max_height(max_h).auto_shrink([false, true]).show_rows(ui, theme::TREE_ROW_H, n, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = (width - ui.spacing().scroll.allocated_width() - 1.0).floor();
        for i in range {
            let (e, name, joints, visible, locked) = &list[i];
            let (e, visible, locked) = (*e, *visible, *locked);
            let selected = selection.selected == Some(e) || selection.contains(e);
            let note = if *joints > 0 { icon::DOOR_OPEN } else { "" };
            let (click, r) = theme::tree_row(ui, width, &TreeRow { depth: 1, text: name, glyph: icon::CUBE, dot: theme::Dot::None, selected, expander: None, guide: true, last: i + 1 == n, note, dim: !visible });
            let hovered = ui.rect_contains_pointer(r.rect);

            let mut handled = false;
            let mut x = r.rect.right() - 6.0;
            // The eye is there while hovered, and stays while the object is hidden.
            if hovered || !visible {
                let b = egui::Rect::from_min_size(egui::pos2(x - 22.0, r.rect.center().y - 11.0), vec2(22.0, 22.0));
                let br = ui.interact(b, r.id.with("eye"), Sense::click());
                if hovered {
                    ui.painter().rect_filled(b, 3.0, if br.hovered() { theme::HOVER } else { theme::RAISED });
                }
                ui.painter().text(b.center(), Align2::CENTER_CENTER, if visible { icon::EYE } else { icon::EYE_SLASH }, FontId::proportional(14.0), if br.hovered() { theme::INK } else { theme::WEAK });
                if br.on_hover_text(if visible { "Hide" } else { "Show" }).clicked() {
                    toggle = Some(e);
                    handled = true;
                }
            }
            x -= 22.0;
            // So is the padlock, in a slot of its own so neither jumps on hover.
            if hovered || locked {
                let b = egui::Rect::from_min_size(egui::pos2(x - 22.0, r.rect.center().y - 11.0), vec2(22.0, 22.0));
                let br = ui.interact(b, r.id.with("lock"), Sense::click());
                if hovered {
                    ui.painter().rect_filled(b, 3.0, if br.hovered() { theme::HOVER } else { theme::RAISED });
                }
                ui.painter().text(b.center(), Align2::CENTER_CENTER, if locked { icon::LOCK } else { icon::LOCK_OPEN }, FontId::proportional(14.0), if br.hovered() { theme::INK } else { theme::WEAK });
                if br.on_hover_text(if locked { "Unlock" } else { "Lock" }).clicked() {
                    actions.0.push(Action::SetLocked(vec![e], !locked));
                    handled = true;
                }
            }
            x -= 22.0;
            if hovered {
                let quick = [(icon::TRASH, "Delete", Action::Delete(e)), (icon::COPY, "Duplicate", Action::Duplicate(e))];
                for (glyph, tip, action) in quick.into_iter().filter(|(_, _, a)| !(locked && matches!(a, Action::Delete(_)))) {
                    let b = egui::Rect::from_min_size(egui::pos2(x - 22.0, r.rect.center().y - 11.0), vec2(22.0, 22.0));
                    x -= 22.0;
                    let br = ui.interact(b, r.id.with(tip), Sense::click());
                    ui.painter().rect_filled(b, 3.0, if br.hovered() { theme::HOVER } else { theme::RAISED });
                    ui.painter().text(b.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), if br.hovered() { theme::INK } else { theme::WEAK });
                    if br.on_hover_text(tip).clicked() {
                        actions.0.push(action);
                        handled = true;
                    }
                }
            }
            if !handled && click.row {
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
                if theme::menu_item(ui, if visible { icon::EYE_SLASH } else { icon::EYE }, if visible { "Hide" } else { "Show" }).clicked() {
                    toggle = Some(e);
                    ui.close();
                }
                if theme::menu_item(ui, if locked { icon::LOCK_OPEN } else { icon::LOCK }, if locked { "Unlock" } else { "Lock" }).clicked() {
                    actions.0.push(Action::SetLocked(vec![e], !locked));
                    ui.close();
                }
                ui.separator();
                theme::menu_enabled(ui, !locked, |ui| {
                    if theme::menu_item_danger(ui, icon::TRASH, "Delete").clicked() {
                        actions.0.push(Action::Delete(e));
                        ui.close();
                    }
                });
            });
        }
    });
    if let Some(e) = toggle
        && let Ok((_, _, _, mut v)) = objects.get_mut(e)
    {
        *v = if *v == Visibility::Hidden { Visibility::Visible } else { Visibility::Hidden };
    }
    ui.add_space(4.0);
}
