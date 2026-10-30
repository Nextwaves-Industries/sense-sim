//! Plant controls in the viewport: a strip at the bottom (play/pause, port
//! and flow overlays, live summary and alarms) and a card for the selected
//! object (switch on, speed, valve, tank level, liquid, its ports and links).

use bevy::prelude::*;
use bevy_egui::egui::{self, Align2, RichText, vec2};
use bevy_egui::{EguiContexts, egui::Color32};

use super::connect::{Links, PortRef};
use super::ports::Role;
use super::sim::{self, Model, Plant};
use crate::scene::{OrbitCamera, PlacedObject, Selection};
use crate::theme::{self, icon};

#[allow(clippy::too_many_arguments)]
pub fn plant_ui(
    mut contexts: EguiContexts,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    window: Single<&Window, With<bevy::window::PrimaryWindow>>,
    hide: Res<crate::ui::HideOverlays>,
    mut plant: ResMut<Plant>,
    mut links: ResMut<Links>,
    selection: Res<Selection>,
    objects: Query<(Entity, &PlacedObject, &Transform)>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    if hide.0 > 0 {
        return Ok(());
    }
    let model = Model::build(objects.iter());
    let has_plant = !links.0.is_empty() || model.objs.iter().any(|o| o.role != Role::Other);
    if !has_plant {
        return Ok(());
    }
    let (cam, cam_gt) = *camera;
    let scale = window.scale_factor();
    let vp = cam
        .viewport
        .as_ref()
        .map(|v| {
            egui::Rect::from_min_size(
                egui::pos2(v.physical_position.x as f32 / scale, v.physical_position.y as f32 / scale),
                vec2(v.physical_size.x as f32 / scale, v.physical_size.y as f32 / scale),
            )
        })
        .unwrap_or(ctx.content_rect());

    // Port labels.
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("plant_labels"))).with_clip_rect(vp);
    for o in &model.objs {
        let selected = selection.selected == Some(o.entity);
        if !(plant.show_ports || selected) {
            continue;
        }
        for p in &o.info.ports {
            let (w, d) = p.world(&o.t);
            let Ok(s) = cam.world_to_viewport(cam_gt, w + d * 0.18) else { continue };
            let linked = links.partner(&PortRef::new(&o.name, &p.id)).is_some();
            let col = if linked { theme::SUCCESS } else { theme::TEXT };
            let galley = painter.layout_no_wrap(p.id.clone(), egui::FontId::proportional(11.0), col);
            let r = egui::Rect::from_center_size(egui::pos2(s.x, s.y), galley.size() + vec2(8.0, 2.0));
            painter.rect_filled(r, 3.0, Color32::from_black_alpha(150));
            painter.galley(r.min + vec2(4.0, 1.0), galley, col);
        }
    }

    // ---- Bottom strip
    let running_n = plant.live.omega.values().filter(|w| w.abs() > 1e-3).count();
    let pumped: f32 = plant.live.flow.pumps.values().map(|p| p.0.max(0.0)).sum();
    let alarms = plant.live.alarms.clone();
    egui::Area::new(egui::Id::new("plant_strip"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(vp.center().x, vp.bottom() - 12.0))
        .constrain_to(vp)
        .show(&ctx, |ui| {
            theme::floating_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (ic, tip) = if plant.running { (icon::PAUSE, "Pause the plant") } else { (icon::PLAY, "Run the plant") };
                    if theme::icon_button_colored(ui, ic, tip, Some(theme::ACCENT), plant.running).clicked() {
                        plant.running = !plant.running;
                    }
                    theme::icon_toggle(ui, &mut plant.show_ports, icon::PLUGS, "Show all ports");
                    theme::icon_toggle(ui, &mut plant.show_flow, icon::DROP, "Show flow");
                    ui.add(egui::DragValue::new(&mut plant.speed).range(0.1..=10.0).speed(0.05).suffix("×").fixed_decimals(1)).on_hover_text("Simulation speed");
                    ui.separator();
                    ui.label(RichText::new(format!("{running_n} moving · {:.2} l/s pumped · {} links", pumped.max(0.0) * 1000.0, links.0.len())).small().color(theme::WEAK));
                    if !alarms.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(format!("{}  {} alarm{}", icon::WARNING_CIRCLE, alarms.len(), if alarms.len() == 1 { "" } else { "s" })).small().color(theme::DANGER))
                            .on_hover_ui(|ui| {
                                for a in alarms.iter().take(12) {
                                    ui.label(RichText::new(a).small());
                                }
                            });
                    }
                });
            });
        });

    // ---- Selected object card
    let Some(o) = selection.selected.and_then(|e| model.objs.iter().find(|o| o.entity == e)) else { return Ok(()) };
    if o.role == Role::Other && o.info.ports.is_empty() {
        return Ok(());
    }
    let name = o.name.clone();
    egui::Area::new(egui::Id::new("plant_card"))
        .order(egui::Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(egui::pos2(vp.left() + 12.0, vp.bottom() - 64.0))
        .constrain_to(vp)
        .show(&ctx, |ui| {
            theme::floating_frame().inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(250.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::GEAR).size(16.0).color(theme::ACCENT));
                    ui.label(RichText::new(o.role.name()).strong());
                    ui.label(RichText::new(&name).small().color(theme::WEAK));
                });
                let w = plant.live.omega.get(&name).copied().unwrap_or(0.0);
                let mut c = plant.control(&name);
                let before = c.clone();
                if sim::self_driven(o) {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut c.on, "On");
                        if o.role == Role::Conveyor {
                            let mut v = c.speed_mps.unwrap_or(0.4);
                            if ui.add(egui::Slider::new(&mut v, -1.5..=1.5).suffix(" m/s").fixed_decimals(2)).changed() {
                                c.speed_mps = Some(v);
                            }
                        } else {
                            let mut r = c.rpm.unwrap_or_else(|| sim::rated_rpm(o));
                            if ui.add(egui::DragValue::new(&mut r).range(0.0..=6000.0).speed(10.0).suffix(" rpm")).changed() {
                                c.rpm = Some(r);
                            }
                        }
                    });
                }
                if w.abs() > 1e-3 {
                    let txt = if o.role == Role::Conveyor { format!("Belt {w:.2} m/s") } else { format!("Turning {:.0} rpm", w * 60.0 / std::f32::consts::TAU) };
                    ui.label(RichText::new(txt).small().color(theme::SUCCESS));
                } else if !sim::self_driven(o) && matches!(o.role, Role::Rotor | Role::Pump) {
                    ui.label(RichText::new("Driven through its shaft or belt").small().color(theme::WEAK));
                }
                if o.role == Role::Valve {
                    let mut open = sim::valve_open(o, &plant.controls) * 100.0;
                    let target = plant.lever_targets.get(&name).map(|d| (1.0 - d / 90.0) * 100.0);
                    let mut shown = target.unwrap_or(open);
                    if ui.add(egui::Slider::new(&mut shown, 0.0..=100.0).suffix(" % open").fixed_decimals(0)).changed() {
                        open = shown;
                        c.valve_open = Some(open / 100.0);
                        if o.proto.joints.iter().any(|j| j.name.to_ascii_lowercase().contains("lever")) {
                            plant.lever_targets.insert(name.clone(), (1.0 - open / 100.0) * 90.0);
                        }
                    }
                    let q = plant.live.flow.element_flow.get(&name).copied().unwrap_or(0.0);
                    ui.label(RichText::new(format!("Flow {:.2} l/s", q * 1000.0)).small().color(theme::WEAK));
                }
                if o.role == Role::Pump
                    && let Some((q, h)) = plant.live.flow.pumps.get(&name)
                {
                    ui.label(RichText::new(format!("{:.2} l/s at {:.1} m head", q * 1000.0, h)).small().color(theme::WEAK));
                }
                if o.role == Role::Tank {
                    let g = sim::tank_geom(o);
                    let v = plant.live.volume.get(&name).copied().unwrap_or_else(|| sim::initial_volume(o, &plant.controls));
                    let mut pct = v / g.capacity.max(1e-6) * 100.0;
                    if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" % full").fixed_decimals(0)).changed() {
                        c.fill = Some(pct / 100.0);
                        plant.live.volume.insert(name.clone(), pct / 100.0 * g.capacity);
                    }
                    let inflow = plant.live.flow.tank_inflow.get(&name).copied().unwrap_or(0.0);
                    ui.label(RichText::new(format!("{:.0} of {:.0} l · net {:+.2} l/s", v * 1000.0, g.capacity * 1000.0, inflow * 1000.0)).small().color(theme::WEAK));
                }
                if o.role.is_fluid() {
                    let cur = c.medium.clone().unwrap_or_else(|| "water".into());
                    egui::ComboBox::from_id_salt("plant_medium").selected_text(&cur).width(110.0).show_ui(ui, |ui| {
                        for m in ["water", "coolant", "oil", "fuel", "chemical"] {
                            if ui.selectable_label(cur == m, m).clicked() {
                                c.medium = Some(m.into());
                            }
                        }
                    });
                }
                if c != before {
                    plant.controls.insert(name.clone(), c);
                    plant.running = true;
                }
                if !o.info.ports.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Ports").small().strong());
                    let mut unlink = None;
                    for p in &o.info.ports {
                        let r = PortRef::new(&name, &p.id);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} · {}", p.id, p.kind.name())).small());
                            match links.partner(&r) {
                                Some((_, other)) => {
                                    ui.label(RichText::new(format!("→ {}", other.label())).small().color(theme::SUCCESS));
                                    if ui.small_button(icon::X).on_hover_text("Disconnect").clicked() {
                                        unlink = Some(r.clone());
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("open").small().color(theme::WEAK));
                                }
                            }
                        });
                    }
                    if let Some(r) = unlink {
                        links.remove_port(&r);
                    }
                }
            });
        });
    Ok(())
}
