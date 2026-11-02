//! The strip under the ribbon while 3D Design is in front: the open designs, where the open scenes
//! otherwise are, and what acts on them. SolidCraft's own application bar is off, so this is the way
//! to its File menu, undo and open designs. The menu itself is SolidCraft's, drawn in the workspace
//! just below this strip.

use bevy_egui::egui::{self, Align, Layout, Margin, vec2};

use super::workbench::DesignCommand;
use super::{LeftPanelSlot, tabbar};
use crate::theme::{self, icon};

fn divider(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::STROKE_STRONG);
    ui.add_space(6.0);
}

/// An icon button that greys out when there's nothing for it to do.
fn action(ui: &mut egui::Ui, enabled: bool, glyph: &str, tip: &str) -> bool {
    ui.add_enabled_ui(enabled, |ui| theme::icon_button(ui, glyph, tip)).inner.clicked()
}

pub fn show(root: &mut egui::Ui, slot: &mut LeftPanelSlot) {
    let info = slot.design_info.clone();
    let mut asked = Vec::new();
    let mut send = false;
    let mut licences = false;
    let mut file_button = None;
    egui::Panel::top("design_tabs").exact_size(tabbar::HEIGHT).show_separator_line(true).frame(egui::Frame::new().fill(theme::BAND).inner_margin(Margin::ZERO)).show(root, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(4.0);
            licences = theme::icon_button(ui, icon::INFO, "Licences of the 3D design tools").clicked();
            if theme::icon_button(ui, icon::GEAR_SIX, "3D design settings (Ctrl+,)").clicked() {
                asked.push(DesignCommand::Settings);
            }
            divider(ui);
            send = ui.add(egui::Button::new(theme::label(icon::EXPORT, "Send to scene")).frame_when_inactive(false)).on_hover_text("Place this design's bodies in the scene").clicked();
            divider(ui);
            if action(ui, info.can_redo, icon::ARROW_U_UP_RIGHT, "Redo in this design (Ctrl+Y)") {
                asked.push(DesignCommand::Redo);
            }
            if action(ui, info.can_undo, icon::ARROW_U_UP_LEFT, "Undo in this design (Ctrl+Z)") {
                asked.push(DesignCommand::Undo);
            }
            if action(ui, info.can_save, icon::FLOPPY_DISK, "Save this design (Ctrl+S)") {
                asked.push(DesignCommand::Save);
            }
            divider(ui);

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add_space(4.0);
                let file = ui.add(egui::Button::new(format!("{}  Design file  {}", icon::FILE_TEXT, icon::CARET_DOWN)).frame_when_inactive(false).selected(info.file_menu_open));
                file_button = Some(file.rect);
                if file.on_hover_text("New, open, insert, save and export designs").clicked() {
                    asked.push(DesignCommand::FileMenu(file_button.map_or(0.0, |r| r.left())));
                }
                divider(ui);

                let room = ui.available_width() - 32.0;
                let width = (room / info.designs.len().max(1) as f32).clamp(tabbar::MIN_TAB, tabbar::MAX_TAB);
                for (i, (name, unsaved)) in info.designs.iter().enumerate() {
                    let (r, over_close) = tabbar::tab_face(ui, name, icon::CUBE_TRANSPARENT, theme::TEXT, *unsaved, i == info.active, width);
                    if r.middle_clicked() || (r.clicked() && over_close) {
                        asked.push(DesignCommand::Close(i));
                    } else if r.clicked() {
                        asked.push(DesignCommand::Show(i));
                    }
                }
                if theme::icon_button(ui, icon::PLUS, "New design (Ctrl+N)").clicked() {
                    asked.push(DesignCommand::New);
                }
            });
        });
    });
    slot.design_file_button = file_button;
    slot.design_commands.extend(asked);
    slot.design_send |= send;
    slot.design_licences |= licences;
}
