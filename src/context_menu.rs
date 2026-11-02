//! Right-click context menus in the viewport, plus hover feedback (cursor
//! shape and a name label under the pointer).

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};

use crate::library::Library;
use crate::scene::{
    Action, Actions, HandleUi, OrbitCamera, PendingSpawn, PendingSpawns, PlaceMode, PlacedObject, Selection, Status,
    UiBlock, ViewSettings,
};
use crate::theme::{self, icon};
use crate::usd_load::JointKind;

enum Target {
    Object(Entity),
    /// Empty space; the ground point under the cursor, if any.
    Empty(Option<Vec3>),
}

#[derive(Resource, Default)]
pub struct ContextMenu {
    open: Option<(Target, egui::Pos2)>,
    press: Option<Vec2>,
    /// Frame the menu opened on, so the opening click doesn't close it.
    opened_frame: u64,
    frame: u64,
    /// Most recently placed asset, offered as "Place … here".
    last_asset: Option<PathBuf>,
}

/// Opens the menu on a right click that wasn't an orbit drag.
pub fn detect_right_click(
    mut menu: ResMut<ContextMenu>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    block: Res<UiBlock>,
    place: Res<PlaceMode>,
    mut selection: ResMut<Selection>,
    added: Query<&PlacedObject, Added<PlacedObject>>,
) {
    menu.frame += 1;
    if let Some(o) = added.iter().last() {
        menu.last_asset = Some(o.asset.clone());
    }
    let cursor = window.cursor_position();
    if mouse.just_pressed(MouseButton::Right) {
        menu.press = if block.pointer || place.asset.is_some() { None } else { cursor };
    }
    if mouse.just_released(MouseButton::Right)
        && let (Some(start), Some(now)) = (menu.press.take(), cursor)
        && start.distance(now) < 5.0
    {
        let target = match selection.hovered {
            Some(e) => {
                selection.selected = Some(e);
                Target::Object(e)
            }
            None => {
                let (cam, gt) = *camera;
                let ground = cam.viewport_to_world(gt, now).ok().and_then(|ray| {
                    let t = -ray.origin.y / ray.direction.y;
                    (ray.direction.y.abs() > 1e-6 && t > 0.0).then(|| ray.origin + ray.direction * t)
                });
                Target::Empty(ground)
            }
        };
        menu.open = Some((target, egui::pos2(now.x, now.y)));
        menu.opened_frame = menu.frame;
    }
}

fn open_in_explorer(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

#[allow(clippy::too_many_arguments)]
pub fn draw_context_menu(
    mut contexts: EguiContexts,
    mut menu: ResMut<ContextMenu>,
    mut actions: ResMut<Actions>,
    mut pending: ResMut<PendingSpawns>,
    mut settings: ResMut<ViewSettings>,
    mut library: ResMut<Library>,
    selection: Res<Selection>,
    mut status: ResMut<Status>,
    mut objects: Query<(&mut PlacedObject, &mut Transform, &mut Visibility)>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let Some((target, pos)) = &menu.open else { return Ok(()) };
    let pos = *pos;
    let mut close = false;
    let mut hide: Vec<Entity> = Vec::new();
    let mut show_hidden = false;

    let area = egui::Area::new(egui::Id::new("viewport_context_menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .constrain(true)
        .show(&ctx, |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_width(230.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                match target {
                    Target::Object(e) => {
                        let e = *e;
                        let Ok((mut obj, mut t, _)) = objects.get_mut(e) else {
                            close = true;
                            return;
                        };
                        let locked = obj.locked;
                        theme::menu_section(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(icon::CUBE).color(theme::ACCENT));
                                ui.label(egui::RichText::new(obj.name.clone()).strong());
                                if locked {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(icon::LOCK).color(theme::WEAK)).on_hover_text("Locked");
                                    });
                                }
                            });
                        });
                        ui.separator();
                        if theme::menu_item(ui, icon::CROSSHAIR, "Focus").clicked() {
                            actions.0.push(Action::Focus(e));
                            close = true;
                        }
                        if theme::menu_item(ui, icon::COPY, "Duplicate").clicked() {
                            actions.0.push(Action::Duplicate(e));
                            close = true;
                        }
                        // These two act on the whole selection when this object is part of it.
                        let targets = if selection.contains(e) { selection.all() } else { vec![e] };
                        let count = |verb: &str| if targets.len() > 1 { format!("{verb} {} objects", targets.len()) } else { verb.to_string() };
                        if theme::menu_item(ui, icon::EYE_SLASH, &count("Hide")).clicked() {
                            hide = targets.clone();
                            close = true;
                        }
                        let (glyph, verb) = if locked { (icon::LOCK_OPEN, "Unlock") } else { (icon::LOCK, "Lock") };
                        if theme::menu_item(ui, glyph, &count(verb)).clicked() {
                            actions.0.push(Action::SetLocked(targets.clone(), !locked));
                            close = true;
                        }
                        ui.separator();
                        theme::menu_enabled(ui, !locked, |ui| {
                            theme::menu_row(ui, icon::ARROWS_CLOCKWISE, "Rotate 90°", |ui| {
                                if theme::menu_row_button(ui, icon::ARROW_CLOCKWISE, "Right").clicked() {
                                    t.rotate_y(-std::f32::consts::FRAC_PI_2);
                                }
                                if theme::menu_row_button(ui, icon::ARROW_COUNTER_CLOCKWISE, "Left").clicked() {
                                    t.rotate_y(std::f32::consts::FRAC_PI_2);
                                }
                            });
                            if theme::menu_item(ui, icon::ARROW_COUNTER_CLOCKWISE, "Reset rotation").clicked() {
                                t.rotation = Quat::IDENTITY;
                                close = true;
                            }
                            if theme::menu_item(ui, icon::ARROW_LINE_DOWN, "Drop").clicked() {
                                actions.0.push(Action::Drop(e));
                                close = true;
                            }
                        });
                        let proto = obj.proto.clone();
                        if !proto.joints.is_empty() {
                            ui.separator();
                            theme::menu_section(ui, |ui| {
                                theme::caption(ui, "Moving parts");
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
                                });
                                for (i, j) in proto.joints.iter().enumerate() {
                                    let (lo, hi) = j.range(proto.unit);
                                    let mut v = obj.joints[i];
                                    let unit = proto.unit;
                                    let slider = egui::Slider::new(&mut v, lo..=hi).text(&j.name);
                                    let slider = if j.kind == JointKind::Revolute {
                                        slider.suffix("°").fixed_decimals(0)
                                    } else {
                                        slider.custom_formatter(move |x, _| format!("{:.3} m", x as f32 * unit))
                                    };
                                    if ui.add(slider).changed() {
                                        obj.joints[i] = v;
                                    }
                                }
                            });
                        }
                        ui.separator();
                        if theme::menu_item(ui, icon::MAGNIFYING_GLASS, "Show in library").clicked() {
                            library.search = crate::ui::asset_name(&obj.asset);
                            close = true;
                        }
                        if theme::menu_item(ui, icon::FOLDER_OPEN, "Open file location").clicked() {
                            open_in_explorer(&obj.asset);
                            close = true;
                        }
                        if theme::menu_item(ui, icon::LINK, "Copy USD path").clicked() {
                            ui.ctx().copy_text(obj.asset.display().to_string());
                            close = true;
                        }
                        ui.separator();
                        theme::menu_enabled(ui, !locked, |ui| {
                            if theme::menu_item_danger(ui, icon::TRASH, "Delete").clicked() {
                                actions.0.push(Action::Delete(e));
                                close = true;
                            }
                        });
                    }
                    Target::Empty(ground) => {
                        if let (Some(p), Some(sel)) = (ground, selection.selected)
                            && let Ok((obj, mut t, _)) = objects.get_mut(sel)
                            && !obj.locked
                            && theme::menu_item(ui, icon::ARROW_LINE_DOWN, &format!("Move {} here", obj.name)).clicked()
                        {
                            t.translation = *p;
                            close = true;
                        }
                        if let (Some(p), Some(asset)) = (ground, menu.last_asset.clone()) {
                            let name = crate::ui::asset_name(&asset);
                            if theme::menu_item(ui, icon::PLUS, &format!("Place another {name} here")).clicked() {
                                pending.0.push(PendingSpawn {
                                    asset,
                                    transform: Transform::from_translation(*p),
                                    name: None,
                                    joints: None,
                                    select: true,
                                    locked: false,
                                });
                                close = true;
                            }
                        }
                        if theme::menu_item(ui, icon::FRAME_CORNERS, "Frame all").clicked() {
                            actions.0.push(Action::FrameAll);
                            close = true;
                        }
                        // A hidden object can't be right-clicked to bring it back.
                        let hidden = objects.iter().filter(|(_, _, v)| **v == Visibility::Hidden).count();
                        if hidden > 0 && theme::menu_item(ui, icon::EYE, &format!("Show {hidden} hidden")).clicked() {
                            show_hidden = true;
                            close = true;
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            theme::icon_toggle(ui, &mut settings.grid, icon::GRID_FOUR, "Grid");
                            theme::icon_toggle(ui, &mut settings.bounds, icon::BOUNDING_BOX, "Hover bounds");
                            theme::icon_toggle(ui, &mut settings.no_overlap, icon::STACK, "Prevent overlap");
                            theme::icon_toggle(ui, &mut settings.gravity, icon::ARROW_LINE_DOWN, "Gravity");
                            theme::icon_toggle(ui, &mut settings.shadows, icon::SUN, "Shadows");
                        });
                        ui.separator();
                        if theme::menu_item_danger(ui, icon::TRASH, "Clear scene").clicked() {
                            actions.0.push(Action::ClearScene);
                            close = true;
                        }
                    }
                }
            });
        });

    if !hide.is_empty() {
        for e in &hide {
            if let Ok((_, _, mut v)) = objects.get_mut(*e) {
                *v = Visibility::Hidden;
            }
        }
        // Its selection box would be left outlining nothing.
        if selection.selected.is_some_and(|s| hide.contains(&s)) {
            actions.0.push(Action::Select(None));
        }
        status.0 = format!("Hid {} object{}: right-click empty space or use the Scene list to show again", hide.len(), if hide.len() == 1 { "" } else { "s" });
    }
    if show_hidden {
        for (_, _, mut v) in &mut objects {
            if *v == Visibility::Hidden {
                *v = Visibility::Visible;
            }
        }
    }

    // Close on Escape or a click outside the menu.
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx.pointer_interact_pos().is_some_and(|p| !area.response.rect.contains(p))
        && menu.frame > menu.opened_frame;
    if close || clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        menu.open = None;
    }
    Ok(())
}

/// Cursor shape and a name label for whatever is under the pointer.
pub fn hover_feedback(
    hide_overlays: Res<crate::ui::HideOverlays>,
    mut contexts: EguiContexts,
    selection: Res<Selection>,
    handles: Res<HandleUi>,
    place: Res<PlaceMode>,
    block: Res<UiBlock>,
    menu: Res<ContextMenu>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    objects: Query<&PlacedObject>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    if block.pointer || menu.open.is_some() {
        return Ok(());
    }
    let dragging = mouse.pressed(MouseButton::Left);
    let hovered_locked = selection.hovered.and_then(|h| objects.get(h).ok()).is_some_and(|o| o.locked);
    let icon = if place.ghost.is_some() {
        egui::CursorIcon::Crosshair
    } else if handles.active.is_none() && dragging && hovered_locked {
        egui::CursorIcon::NotAllowed
    } else if handles.active.is_some() || (dragging && selection.hovered.is_some()) {
        egui::CursorIcon::Grabbing
    } else if handles.hovered.is_some() {
        egui::CursorIcon::Grab
    } else if selection.hovered.is_some() {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::Default
    };
    ctx.set_cursor_icon(icon);

    // Name tag next to the pointer for hovered, unselected objects.
    if !dragging
        && hide_overlays.0 == 0
        && handles.hovered.is_none()
        && let Some(h) = selection.hovered
        && Some(h) != selection.selected
        && let Ok(obj) = objects.get(h)
        && let Some(c) = window.cursor_position()
    {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("hover_name")));
        let tag = if obj.locked { theme::label(theme::icon::LOCK, &obj.name) } else { obj.name.clone() };
        let galley = painter.layout_no_wrap(tag, egui::FontId::proportional(13.0), theme::TEXT);
        let rect = egui::Rect::from_min_size(egui::pos2(c.x + 16.0, c.y + 14.0), galley.size() + egui::vec2(10.0, 6.0));
        painter.rect_filled(rect, 4.0, theme::PANEL);
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, theme::STROKE_STRONG), egui::StrokeKind::Inside);
        painter.galley(rect.min + egui::vec2(5.0, 3.0), galley, theme::TEXT);
    }
    Ok(())
}

/// Right-click menus, hover feedback and undo/redo.
pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        use crate::scene::{apply_actions, interact};
        app.init_resource::<ContextMenu>()
            .init_resource::<crate::undo::UndoStack>()
            .init_resource::<crate::undo::UndoRequest>()
            .add_systems(Update, (detect_right_click.after(interact), crate::undo::undo_system.after(apply_actions)))
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (draw_context_menu, hover_feedback).after(crate::ui::ui_system),
            );
    }
}
