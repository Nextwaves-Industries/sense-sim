//! Plant visuals: port markers, animated flow along pipes (coloured by the
//! liquid), belts between pulleys, conveyor travel arrows, leak markers, and
//! a liquid column inside every tank that rises and falls with its level.

use std::collections::{HashMap, HashSet};

use bevy::picking::Pickable;
use bevy::prelude::*;

use super::connect::{LinkKind, Links, PortRef};
use super::ports::{PortKind, Role};
use super::sim::{self, Model, Plant};
use crate::scene::{PlacedObject, Selection};

/// Flow, belts and ports: drawn over the geometry so they show through pipe walls.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct FlowGizmos;

#[derive(Resource, Default)]
pub struct Liquids {
    mesh: Handle<Mesh>,
    materials: HashMap<String, Handle<StandardMaterial>>,
    /// Tank object → liquid entity.
    spawned: HashMap<Entity, Entity>,
}

/// Marks the liquid column of a tank.
#[derive(Component)]
pub struct Liquid;

pub fn medium_color(m: Option<&str>) -> Color {
    match m.unwrap_or("water").to_ascii_lowercase().as_str() {
        "oil" | "hydraulic" => Color::srgb(0.95, 0.65, 0.1),
        "coolant" | "glycol" => Color::srgb(0.2, 0.95, 0.45),
        "fuel" | "diesel" | "gasoline" => Color::srgb(1.0, 0.45, 0.15),
        "chemical" | "acid" => Color::srgb(0.85, 0.3, 0.95),
        "air" | "gas" => Color::srgb(0.85, 0.9, 0.95),
        _ => Color::srgb(0.25, 0.65, 1.0),
    }
}

pub fn setup(mut store: ResMut<GizmoConfigStore>, mut liquids: ResMut<Liquids>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let (cfg, _) = store.config_mut::<FlowGizmos>();
    cfg.line.width = 3.0;
    cfg.depth_bias = -1.0;
    liquids.mesh = meshes.add(Cylinder::new(1.0, 1.0));
    for m in ["water", "oil", "coolant", "fuel", "chemical"] {
        let c = medium_color(Some(m)).to_linear();
        liquids.materials.insert(
            m.into(),
            materials.add(StandardMaterial {
                base_color: Color::linear_rgba(c.red, c.green, c.blue, 0.55),
                alpha_mode: AlphaMode::Blend,
                perceptual_roughness: 0.15,
                ..default()
            }),
        );
    }
}

/// Medium of the connected system an object belongs to: its own setting,
/// or the first tank's on any linked path.
pub fn medium_of(name: &str, plant: &Plant, links: &Links, model: &Model) -> Option<String> {
    if let Some(m) = plant.controls.get(name).and_then(|c| c.medium.clone()) {
        return Some(m);
    }
    let mut seen = HashSet::new();
    let mut stack = vec![name.to_string()];
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) || seen.len() > 400 {
            continue;
        }
        if let Some(m) = plant.controls.get(&n).and_then(|c| c.medium.clone()) {
            return Some(m);
        }
        let _ = model;
        for l in links.0.iter().filter(|l| l.kind == LinkKind::Pipe && l.touches(&n)) {
            stack.push(if l.a.object == n { l.b.object.clone() } else { l.a.object.clone() });
        }
    }
    None
}

/// Keeps one liquid column per tank, sized to its current level.
pub fn sync_liquid(
    mut commands: Commands,
    mut liquids: ResMut<Liquids>,
    plant: Res<Plant>,
    links: Res<Links>,
    objects: Query<(Entity, &PlacedObject, &Transform)>,
    mut cols: Query<(&mut Transform, &mut MeshMaterial3d<StandardMaterial>), (With<Liquid>, Without<PlacedObject>)>,
) {
    if liquids.mesh == Handle::default() {
        return;
    }
    let model = Model::build(objects.iter());
    let mut alive = HashSet::new();
    for o in model.objs.iter().filter(|o| o.role == Role::Tank) {
        alive.insert(o.entity);
        let g = sim::tank_geom(o);
        let vol = plant.live.volume.get(&o.name).copied().unwrap_or_else(|| sim::initial_volume(o, &plant.controls));
        let frac = (vol / g.capacity.max(1e-6)).clamp(0.0, 1.0);
        let s = o.t.scale.y.max(1e-3);
        let h_local = (g.height / s * frac).max(0.002);
        let t = Transform::from_translation(g.local_center + Vec3::Y * h_local * 0.5).with_scale(Vec3::new(g.radius, h_local, g.radius));
        let medium = medium_of(&o.name, &plant, &links, &model).unwrap_or_else(|| "water".into());
        let mat = liquids.materials.get(medium.as_str()).or_else(|| liquids.materials.get("water")).cloned().unwrap();
        match liquids.spawned.get(&o.entity).copied() {
            Some(e) => {
                if let Ok((mut tr, mut m)) = cols.get_mut(e) {
                    if *tr != t {
                        *tr = t;
                    }
                    if m.0 != mat {
                        m.0 = mat;
                    }
                } else {
                    liquids.spawned.remove(&o.entity);
                }
            }
            None => {
                let e = commands.spawn((Mesh3d(liquids.mesh.clone()), MeshMaterial3d(mat), t, Liquid, Pickable::IGNORE, bevy::light::NotShadowCaster, ChildOf(o.entity))).id();
                liquids.spawned.insert(o.entity, e);
            }
        }
    }
    liquids.spawned.retain(|tank, _| alive.contains(tank));
}

fn dashes(g: &mut Gizmos<FlowGizmos>, pts: &[Vec3], phase: f32, spacing: f32, len: f32, color: Color) {
    let total: f32 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    if total < 1e-4 {
        return;
    }
    let mut s = phase.rem_euclid(spacing);
    while s < total {
        let a = point_at(pts, s);
        let b = point_at(pts, (s + len).min(total));
        g.line(a, b, color);
        s += spacing;
    }
}

fn point_at(pts: &[Vec3], mut s: f32) -> Vec3 {
    for w in pts.windows(2) {
        let l = w[0].distance(w[1]);
        if s <= l {
            return w[0].lerp(w[1], if l > 0.0 { s / l } else { 0.0 });
        }
        s -= l;
    }
    *pts.last().unwrap()
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    mut g: Gizmos<FlowGizmos>,
    plant: Res<Plant>,
    links: Res<Links>,
    selection: Res<Selection>,
    time: Res<Time>,
    hide: Res<crate::ui::HideOverlays>,
    objects: Query<(Entity, &PlacedObject, &Transform)>,
) {
    if hide.0 > 0 {
        return;
    }
    let model = Model::build(objects.iter());
    if model.objs.iter().all(|o| o.role == Role::Other) && links.0.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    let linked: HashSet<PortRef> = links.0.iter().flat_map(|l| [l.a.clone(), l.b.clone()]).collect();

    // Ports: all of them with "show ports", else the selected object's, and
    // open pipe ends of piping always (they leak).
    for o in &model.objs {
        let selected = selection.selected == Some(o.entity);
        for p in &o.info.ports {
            let r = PortRef::new(&o.name, &p.id);
            let is_linked = linked.contains(&r);
            let open_pipe = p.kind == PortKind::Pipe && !is_linked && matches!(o.role, Role::Fitting | Role::Valve | Role::Pump);
            if !(plant.show_ports || selected || open_pipe) {
                continue;
            }
            let (w, d) = p.world(&o.t);
            let rad = (p.size * 0.5 * o.t.scale.x).clamp(0.02, 0.4) + 0.01;
            let col = if is_linked {
                Color::srgb(0.3, 0.95, 0.4)
            } else if open_pipe {
                Color::srgb(1.0, 0.25, 0.2).with_alpha(0.6 + 0.4 * (now * 6.0).sin().abs())
            } else {
                p.kind.color()
            };
            g.circle(Isometry3d::new(w, Quat::from_rotation_arc(Vec3::Z, d)), rad, col).resolution(24);
            if !is_linked || selected || plant.show_ports {
                g.arrow(w, w + d * (rad + 0.12), col);
            }
        }
    }

    // Flow along generated piping.
    if plant.show_flow {
        let flow = &plant.live.flow;
        for o in &model.objs {
            let Some(spec) = o.info.spec else { continue };
            let bore = spec.bore();
            if bore <= 0.0 {
                continue;
            }
            let color = medium_color(medium_of(&o.name, &plant, &links, &model).as_deref());
            for (port, path) in spec.paths() {
                let q = flow.port_flow.get(&PortRef::new(&o.name, &port)).copied().unwrap_or(0.0);
                if q.abs() < 1e-6 {
                    continue;
                }
                let v = q / (std::f32::consts::PI * bore * bore * 0.25);
                let pts: Vec<Vec3> = path.iter().map(|p| o.t.transform_point(*p)).collect();
                // Out of this port: dashes travel center → port.
                let vis = (v.abs().sqrt() * 0.6).clamp(0.15, 2.5) * v.signum();
                let bright = (0.55 + v.abs() * 0.2).min(1.0);
                dashes(&mut g, &pts, now * vis, 0.12, 0.05, color.with_alpha(bright));
            }
        }
        // Leaks: spray from the open end.
        for (r, q) in &flow.leaks {
            if let Some((w, d, _)) = model.port_world(r) {
                let n = (q * 4000.0).clamp(2.0, 10.0) as i32;
                for i in 0..n {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU + now * 3.0;
                    let (u, v) = (d.any_orthonormal_vector(), d.cross(d.any_orthonormal_vector()));
                    let tip = w + d * 0.25 + (u * a.cos() + v * a.sin()) * 0.06 - Vec3::Y * 0.1;
                    g.line(w, tip, Color::srgb(0.4, 0.75, 1.0).with_alpha(0.8));
                }
            }
        }
    }

    // Belts between pulleys, moving with the drive.
    for l in links.0.iter().filter(|l| l.kind == LinkKind::Belt) {
        let (Some((c1, ax, p1)), Some((c2, _, p2))) = (model.port_world(&l.a), model.port_world(&l.b)) else { continue };
        let (r1, r2) = (p1.size * 0.5, p2.size * 0.5);
        let u = (c2 - c1).reject_from_normalized(ax).normalize_or(Vec3::Y);
        let n = ax.cross(u);
        let col = Color::srgb(0.08, 0.08, 0.09);
        for (c, r) in [(c1, r1), (c2, r2)] {
            g.circle(Isometry3d::new(c, Quat::from_rotation_arc(Vec3::Z, ax)), r, col).resolution(32);
        }
        let top = [c1 + n * r1, c2 + n * r2];
        let bot = [c2 - n * r2, c1 - n * r1];
        g.line(top[0], top[1], col);
        g.line(bot[0], bot[1], col);
        let w = plant.live.omega.get(&l.a.object).copied().unwrap_or(0.0);
        if w.abs() > 0.01 {
            let phase = plant.live.angle.get(&l.a.object).copied().unwrap_or(0.0) * r1;
            let mark = Color::srgb(1.0, 0.85, 0.2);
            dashes(&mut g, &top, phase, 0.08, 0.02, mark);
            dashes(&mut g, &bot, phase, 0.08, 0.02, mark);
        }
    }

    // Conveyors: travel chevrons while running.
    let omega = &plant.live.omega;
    for b in sim::belts(&model, &links, omega) {
        if b.speed.abs() < 1e-4 {
            continue;
        }
        let side = b.dir.cross(Vec3::Y);
        let phase = plant.live.angle.get(&b.name).copied().unwrap_or(0.0);
        let mut s = phase.rem_euclid(0.4);
        while s < b.len {
            let p = b.start + b.dir * s + Vec3::Y * 0.012;
            let c = Color::srgb(0.55, 0.95, 0.35).with_alpha(0.7);
            g.line(p, p - b.dir * 0.08 + side * 0.08, c);
            g.line(p, p - b.dir * 0.08 - side * 0.08, c);
            s += 0.4;
        }
    }
}
