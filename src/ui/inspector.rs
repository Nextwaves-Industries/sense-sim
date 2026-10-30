//! Properties of the selected object: name, quick actions, transform,
//! moving parts and load info.

use bevy::prelude::*;
use bevy_egui::egui::{self, RichText, vec2};

use super::ObjectQuery;
use crate::scene::{Action, Actions, Handle, HandleUi, Selection};
use crate::theme::{self, ACCENT, AXIS, icon};
use crate::usd_load::JointKind;

/// `X [value]` with a coloured axis letter.
fn axis_value(ui: &mut egui::Ui, axis: usize, value: &mut f32, speed: f64, decimals: usize, suffix: &str, width: f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(RichText::new(["X", "Y", "Z"][axis]).small().strong().color(AXIS[axis]));
        changed = ui
            .add_sized(vec2(width, 22.0), egui::DragValue::new(value).speed(speed).fixed_decimals(decimals).suffix(suffix))
            .changed();
    });
    changed
}

pub fn show(ui: &mut egui::Ui, objects: &mut ObjectQuery, selection: &Selection, handles: &HandleUi, actions: &mut Actions) {
    let Some((e, mut obj, mut t)) = selection.selected.and_then(|e| objects.get_mut(e).ok()) else {
        return;
    };

    // Header: editable name, asset path.
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon::CUBE).size(18.0).color(ACCENT));
        ui.add(
            egui::TextEdit::singleline(&mut obj.name)
                .font(egui::TextStyle::Heading)
                .desired_width(ui.available_width())
                .frame(egui::Frame::NONE),
        )
        .on_hover_text("Rename");
    });
    ui.horizontal(|ui| {
        let path = obj.asset.display().to_string();
        if ui.add(egui::Button::new(RichText::new(icon::COPY).small().color(theme::WEAK)).frame_when_inactive(false).small()).on_hover_text("Copy USD path").clicked() {
            ui.ctx().copy_text(path.clone());
        }
        ui.add(egui::Label::new(RichText::new(&path).small().color(theme::WEAK)).truncate()).on_hover_text(&path);
    });
    ui.add_space(4.0);

    // Quick actions.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if theme::icon_button(ui, icon::CROSSHAIR, "Focus (F)").clicked() {
            actions.0.push(Action::Focus(e));
        }
        if theme::icon_button(ui, icon::COPY, "Duplicate (Ctrl+D)").clicked() {
            actions.0.push(Action::Duplicate(e));
        }
        if theme::icon_button(ui, icon::ARROW_LINE_DOWN, "Drop (lands on what is under it)").clicked() {
            actions.0.push(Action::Drop(e));
        }
        if theme::icon_button(ui, icon::ARROW_COUNTER_CLOCKWISE, "Reset rotation").clicked() {
            t.rotation = Quat::IDENTITY;
        }
        if theme::icon_button(ui, icon::ARROWS_CLOCKWISE, "Rotate 90°").clicked() {
            t.rotate_y(-std::f32::consts::FRAC_PI_2);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::icon_button_colored(ui, icon::TRASH, "Delete (Del)", Some(theme::DANGER), false).clicked() {
                actions.0.push(Action::Delete(e));
            }
            if theme::icon_button(ui, icon::SELECTION_SLASH, "Deselect (Esc)").clicked() {
                actions.0.push(Action::Select(None));
            }
        });
    });
    theme::divider(ui);

    let proto = obj.proto.clone();
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        theme::section(ui, "Transform", true, |ui| {
            let w = ((ui.available_width() - 80.0) / 3.0 - 16.0).max(40.0);
            egui::Grid::new("xf").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Position").color(theme::WEAK));
                ui.horizontal(|ui| {
                    let tr = &mut t.translation;
                    for (k, v) in [&mut tr.x, &mut tr.y, &mut tr.z].into_iter().enumerate() {
                        axis_value(ui, k, v, 0.01, 2, "", w);
                    }
                });
                ui.end_row();

                let (y, x, z) = t.rotation.to_euler(EulerRot::YXZ);
                let mut deg = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
                ui.label(RichText::new("Rotation").color(theme::WEAK));
                let mut changed = false;
                ui.horizontal(|ui| {
                    for (k, d) in deg.iter_mut().enumerate() {
                        changed |= axis_value(ui, k, d, 0.5, 0, "°", w);
                    }
                });
                if changed {
                    t.rotation = Quat::from_euler(EulerRot::YXZ, deg[1].to_radians(), deg[0].to_radians(), deg[2].to_radians());
                }
                ui.end_row();

                ui.label(RichText::new("Scale").color(theme::WEAK));
                ui.horizontal(|ui| {
                    let mut s = t.scale.x;
                    if ui.add_sized(vec2(w + 14.0, 22.0), egui::DragValue::new(&mut s).speed(0.01).range(0.001..=1000.0).fixed_decimals(2).prefix("× ")).changed() {
                        t.scale = Vec3::splat(s);
                    }
                    if t.scale != Vec3::ONE && theme::icon_button(ui, icon::ARROW_COUNTER_CLOCKWISE, "Reset scale").clicked() {
                        t.scale = Vec3::ONE;
                    }
                });
                ui.end_row();
            });
            let (mn, mx) = obj.aabb();
            let size = (mx - mn) * t.scale;
            ui.label(RichText::new(format!("Size  {:.2} × {:.2} × {:.2} m", size.x, size.y, size.z)).small().color(theme::WEAK));
            if proto.joints.is_empty() {
                ui.label(RichText::new("No moving parts: this asset has no joints in its USD.").small().color(theme::WEAK));
            }
        });

        if !proto.joints.is_empty() {
            theme::section(ui, &format!("Moving parts  {}", proto.joints.len()), true, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button(theme::label(icon::DOOR_OPEN, "Open all")).clicked() {
                        for (i, j) in proto.joints.iter().enumerate() {
                            obj.joints[i] = j.range(proto.unit).1;
                        }
                    }
                    if ui.small_button("Close all").clicked() {
                        for (i, j) in proto.joints.iter().enumerate() {
                            let (lo, hi) = j.range(proto.unit);
                            obj.joints[i] = 0.0f32.clamp(lo, hi);
                        }
                    }
                    if ui.small_button(theme::label(icon::ARROW_COUNTER_CLOCKWISE, "Reset")).clicked() {
                        obj.joints.iter_mut().for_each(|v| *v = 0.0);
                    }
                });
                ui.label(RichText::new("Drag the white dots in the viewport (or scroll over one; Shift = fine), or use the sliders.").small().color(theme::WEAK));
                ui.add_space(2.0);
                ui.spacing_mut().slider_width = (ui.available_width() - 80.0).max(80.0);
                for (i, j) in proto.joints.iter().enumerate() {
                    let (lo, hi) = j.range(proto.unit);
                    let hot = handles.hovered == Some(Handle::Joint(i)) || handles.active == Some(Handle::Joint(i));
                    let kind = if j.kind == JointKind::Revolute { icon::ARROWS_CLOCKWISE } else { icon::ARROW_SQUARE_OUT };
                    let name = RichText::new(format!("{kind}  {}", j.name)).small();
                    ui.label(if hot { name.color(ACCENT) } else { name.color(theme::TEXT) });
                    let mut v = obj.joints[i];
                    let changed = match j.kind {
                        JointKind::Revolute => ui.add(egui::Slider::new(&mut v, lo..=hi).suffix("°").fixed_decimals(0)).changed(),
                        _ => {
                            let unit = proto.unit;
                            ui.add(
                                egui::Slider::new(&mut v, lo..=hi)
                                    .custom_formatter(move |x, _| format!("{:.3} m", x as f32 * unit))
                                    .custom_parser(move |s| s.trim_end_matches('m').trim().parse::<f64>().ok().map(|m| m / unit as f64)),
                            )
                            .changed()
                        }
                    };
                    if changed {
                        obj.joints[i] = v;
                    }
                }
            });
        }

        theme::section(ui, "Info", false, |ui| {
            egui::Grid::new("info").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new("Triangles").color(theme::WEAK));
                ui.label(proto.triangles.to_string());
                ui.end_row();
                ui.label(RichText::new("Parts").color(theme::WEAK));
                ui.label(proto.parts.len().to_string());
                ui.end_row();
                ui.label(RichText::new("Load time").color(theme::WEAK));
                ui.label(format!("{:.0} ms", proto.load_ms));
                ui.end_row();
            });
            if !proto.warnings.is_empty() {
                ui.collapsing(RichText::new(format!("{}  {} load warnings", icon::WARNING_CIRCLE, proto.warnings.len())).color(ACCENT), |ui| {
                    for w in &proto.warnings {
                        ui.label(RichText::new(w).small());
                    }
                });
            }
            ui.collapsing(format!("Parts ({})", proto.parts.len()), |ui| {
                egui::ScrollArea::vertical().id_salt("parts").max_height(180.0).show(ui, |ui| {
                    for part in &proto.parts {
                        ui.label(RichText::new(&part.name).small()).on_hover_text(&part.prim_path);
                    }
                });
            });
        });
    });
}
