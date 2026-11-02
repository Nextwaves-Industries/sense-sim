//! Navigation over the 3D canvas: the view cube (click a face to look at it,
//! drag to orbit), step arrows, home, an axis triad, and the scale bar.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

use bevy::prelude::*;
use bevy_egui::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, pos2, vec2};

use crate::scene::{Action, Actions, OrbitCamera};
use crate::theme::{self, icon};

/// Room the cube takes above the canvas's bottom edge.
pub const HEIGHT: f32 = 184.0;
const WIDTH: f32 = 176.0;
/// Half the cube's edge, in points.
const HALF: f32 = 27.0;

/// The opening view of a scene.
const HOME: (f32, f32) = (0.65, 0.45);

struct Face {
    label: &'static str,
    normal: Vec3,
    /// Text direction on the face.
    u: Vec3,
    v: Vec3,
    /// Camera (yaw, pitch) that looks straight at it. `None` keeps the yaw.
    yaw: Option<f32>,
    pitch: f32,
}

// Y up, like the scene. Yaw 0 looks at +Z from the front.
const FACES: [Face; 6] = [
    Face { label: "FRONT", normal: Vec3::Z, u: Vec3::X, v: Vec3::Y, yaw: Some(0.0), pitch: 0.0 },
    Face { label: "BACK", normal: Vec3::NEG_Z, u: Vec3::NEG_X, v: Vec3::Y, yaw: Some(PI), pitch: 0.0 },
    Face { label: "RIGHT", normal: Vec3::X, u: Vec3::NEG_Z, v: Vec3::Y, yaw: Some(FRAC_PI_2), pitch: 0.0 },
    Face { label: "LEFT", normal: Vec3::NEG_X, u: Vec3::Z, v: Vec3::Y, yaw: Some(-FRAC_PI_2), pitch: 0.0 },
    Face { label: "TOP", normal: Vec3::Y, u: Vec3::X, v: Vec3::NEG_Z, yaw: None, pitch: 1.5 },
    Face { label: "BOTTOM", normal: Vec3::NEG_Y, u: Vec3::X, v: Vec3::Z, yaw: None, pitch: -1.5 },
];

fn wrap(a: f32) -> f32 {
    let a = (a + PI).rem_euclid(TAU) - PI;
    if a < -PI { a + TAU } else { a }
}

/// Glides the camera towards the view the cube was asked for.
pub fn animate(ctx: &egui::Context, orbit: &mut OrbitCamera, target: &mut Option<(f32, f32)>) {
    let Some((yaw, pitch)) = *target else { return };
    let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);
    let k = 1.0 - (-dt * 13.0).exp();
    let (dy, dp) = (wrap(yaw - orbit.yaw), pitch - orbit.pitch);
    orbit.yaw += dy * k;
    orbit.pitch += dp * k;
    if dy.abs() < 0.002 && dp.abs() < 0.002 {
        orbit.yaw = yaw;
        orbit.pitch = pitch;
        *target = None;
    }
    ctx.request_repaint();
}

fn inside(p: Pos2, quad: &[Pos2; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let cross = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}

/// A small glyph button; returns true when clicked.
fn glyph(ui: &mut egui::Ui, parent: &egui::Response, name: &str, at: Pos2, text: &str, size: f32, tip: &str) -> bool {
    let rect = Rect::from_center_size(at, vec2(22.0, 22.0));
    let r = ui.interact(rect, parent.id.with(name), Sense::click());
    let col = if r.hovered() { theme::INK } else { theme::WEAK };
    ui.painter().text(at, Align2::CENTER_CENTER, text, FontId::proportional(size), col);
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
    r.clicked()
}

pub fn show(ctx: &egui::Context, central: Rect, orbit: &mut OrbitCamera, target: &mut Option<(f32, f32)>, actions: &mut Actions) {
    let area = Rect::from_min_size(central.right_bottom() - vec2(WIDTH + 8.0, HEIGHT + 2.0), vec2(WIDTH, HEIGHT));
    egui::Area::new(egui::Id::new("view_cube")).order(egui::Order::Middle).fixed_pos(area.min).show(ctx, |ui| {
        let (rect, base) = ui.allocate_exact_size(area.size(), Sense::hover());
        let center = rect.center() + vec2(-2.0, 2.0);

        let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
        let inv = rot.inverse();
        let project = |v: Vec3| {
            let q = inv * v;
            center + vec2(q.x, -q.y) * HALF
        };

        // Drag anywhere on the cube to orbit; click a face to look at it.
        let cube_rect = Rect::from_center_size(center, egui::Vec2::splat(HALF * 3.4));
        let cube = ui.interact(cube_rect, base.id.with("cube"), Sense::click_and_drag());
        let pointer = cube.hover_pos();

        let mut hovered: Option<usize> = None;
        let mut quads: Vec<(usize, [Pos2; 4], f32)> = Vec::new();
        for (i, f) in FACES.iter().enumerate() {
            let facing = (inv * f.normal).z;
            if facing <= 0.015 {
                continue;
            }
            let quad = [
                project(f.normal - f.u - f.v),
                project(f.normal + f.u - f.v),
                project(f.normal + f.u + f.v),
                project(f.normal - f.u + f.v),
            ];
            if pointer.is_some_and(|p| inside(p, &quad)) {
                hovered = Some(i);
            }
            quads.push((i, quad, facing));
        }

        let painter = ui.painter();
        // Axis triad behind the cube, from its lower left.
        let origin = center + vec2(-HALF * 1.9, HALF * 1.45);
        let mut axes = [(Vec3::X, "X", theme::AXIS[0]), (Vec3::Y, "Y", theme::AXIS[1]), (Vec3::Z, "Z", theme::AXIS[2])];
        axes.sort_by(|a, b| (inv * a.0).z.total_cmp(&(inv * b.0).z));
        for (axis, name, col) in axes {
            let q = inv * axis;
            let tip = origin + vec2(q.x, -q.y) * 26.0;
            painter.line_segment([origin, tip], Stroke::new(1.5, col));
            let d = (tip - origin).normalized();
            let tip_label = tip + if (tip - origin).length() > 1.0 { d * 8.0 } else { vec2(0.0, 0.0) };
            painter.text(tip_label, Align2::CENTER_CENTER, name, theme::bold(11.0), col);
        }

        for (i, quad, facing) in &quads {
            let f = &FACES[*i];
            let shade = 248.0 - (1.0 - facing) * 16.0;
            let fill = if hovered == Some(*i) { theme::ACCENT_SOFT } else { Color32::from_gray(shade as u8) };
            let stroke = Stroke::new(1.0, Color32::from_rgb(96, 102, 112));
            painter.add(egui::Shape::convex_polygon(quad.to_vec(), fill, stroke));

            // Label along the face's own direction, while it faces us enough to read.
            if *facing > 0.28 {
                let du = project(f.u) - project(Vec3::ZERO);
                let mut angle = du.y.atan2(du.x);
                if angle.abs() > FRAC_PI_2 {
                    angle += PI;
                }
                let c = project(f.normal);
                let size = 8.0 + 2.0 * facing;
                let col = if hovered == Some(*i) { theme::ACCENT } else { theme::lerp_color(Color32::from_gray(200), theme::WEAK, *facing) };
                let g = painter.layout_no_wrap(f.label.to_string(), FontId::proportional(size), col);
                let half = g.size() * 0.5;
                let (s, co) = angle.sin_cos();
                let pos = c - vec2(half.x * co - half.y * s, half.x * s + half.y * co);
                painter.add(egui::epaint::TextShape::new(pos, g, col).with_angle(angle));
            }
        }

        if cube.dragged() {
            let d = cube.drag_delta();
            orbit.yaw -= d.x * 0.012;
            orbit.pitch = (orbit.pitch + d.y * 0.012).clamp(-1.5, 1.5);
            *target = None;
        } else if cube.clicked()
            && let Some(i) = hovered
        {
            let f = &FACES[i];
            *target = Some((f.yaw.unwrap_or(orbit.yaw), f.pitch));
        }
        if hovered.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        // Step arrows around the cube.
        let reach = HALF * 2.45;
        let (yaw, pitch) = target.unwrap_or((orbit.yaw, orbit.pitch));
        if glyph(ui, &base, "up", center + vec2(0.0, -reach), icon::CARET_UP, 15.0, "Look from higher") {
            *target = Some((yaw, (pitch + FRAC_PI_4 / 2.0).min(1.5)));
        }
        if glyph(ui, &base, "down", center + vec2(0.0, reach), icon::CARET_DOWN, 15.0, "Look from lower") {
            *target = Some((yaw, (pitch - FRAC_PI_4 / 2.0).max(-1.5)));
        }
        if glyph(ui, &base, "left", center + vec2(-reach, 0.0), icon::CARET_LEFT, 15.0, "Rotate left") {
            *target = Some((yaw - FRAC_PI_4, pitch));
        }
        if glyph(ui, &base, "right", center + vec2(reach, 0.0), icon::CARET_RIGHT, 15.0, "Rotate right") {
            *target = Some((yaw + FRAC_PI_4, pitch));
        }
        if glyph(ui, &base, "ccw", center + vec2(-reach * 0.78, reach * 0.8), icon::ARROW_COUNTER_CLOCKWISE, 14.0, "Quarter turn left") {
            *target = Some((yaw - FRAC_PI_2, pitch));
        }
        if glyph(ui, &base, "cw", center + vec2(reach * 0.78, reach * 0.8), icon::ARROW_CLOCKWISE, 14.0, "Quarter turn right") {
            *target = Some((yaw + FRAC_PI_2, pitch));
        }
        if glyph(ui, &base, "home", center + vec2(reach * 0.78, -reach * 0.78), icon::HOUSE, 15.0, "Home: default view, everything in frame") {
            *target = Some(HOME);
            actions.0.push(Action::FrameAll);
        }
        let help = Rect::from_center_size(rect.right_top() + vec2(-14.0, 14.0), vec2(22.0, 22.0));
        ui.painter().text(help.center(), Align2::CENTER_CENTER, icon::QUESTION, FontId::proportional(16.0), theme::WEAK);
        ui.interact(help, base.id.with("help"), Sense::hover()).on_hover_ui(|ui| {
            ui.set_min_width(340.0);
            super::topbar::shortcuts_grid(ui);
        });
    });
}

/// `0.1 m`, with as many decimals as the value needs.
fn format_m(v: f32) -> String {
    let decimals = if v >= 1.0 { 0 } else { (-v.log10().floor()) as usize };
    format!("{v:.decimals$} m")
}

/// Bar under the canvas showing how long a round distance is at the focus.
pub fn scale_bar(ctx: &egui::Context, central: Rect, radius: f32, fov: f32) {
    let px_per_m = central.height() / (2.0 * radius.max(1e-3) * (fov * 0.5).tan());
    if !px_per_m.is_finite() || px_per_m <= 0.0 {
        return;
    }
    let raw = 130.0 / px_per_m;
    let base = 10f32.powf(raw.log10().floor());
    let nice = [1.0f32, 2.0, 5.0, 10.0].iter().map(|k| k * base).min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs())).unwrap_or(base);
    let w = nice * px_per_m;

    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("scale_bar"))).with_clip_rect(central);
    let c = central.center_bottom() - vec2(0.0, 27.0);
    let (l, r) = (c.x - w / 2.0, c.x + w / 2.0);
    let s = Stroke::new(1.5, theme::INK);
    p.hline(l..=r, c.y, s);
    p.vline(l, (c.y - 3.0)..=(c.y + 3.0), s);
    p.vline(r, (c.y - 3.0)..=(c.y + 3.0), s);
    p.text(pos2(c.x, c.y + 6.0), Align2::CENTER_TOP, format_m(nice), FontId::proportional(12.5), theme::TEXT);
}
