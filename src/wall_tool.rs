//! Draw walls by hand: click points on the floor and a wall goes up between
//! each pair. Walls are the same generated USD boxes the assistant builds, so
//! selection, undo, layouts, export and RTLS treat them like any asset.

use std::f32::consts::PI;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::agent::structures::{self, Kind};
use crate::prototype::PrototypeCache;
use crate::scene::{OrbitCamera, PendingSpawn, PendingSpawns, PlaceMode, PlacedObject, Selection, Status, UiBlock, ViewSettings, ray_plane_y, snap};

/// Clicks this close to an existing wall end join it.
const JOIN_RADIUS: f32 = 0.3;
/// Shorter segments are ignored (double clicks, mis-clicks).
const MIN_LENGTH: f32 = 0.1;

#[derive(Resource)]
pub struct WallTool {
    pub active: bool,
    pub height: f32,
    pub thickness: f32,
    /// Start of the segment being drawn.
    start: Option<Vec3>,
    /// Snapped point under the cursor.
    cursor: Option<Vec3>,
    was_active: bool,
}

impl Default for WallTool {
    fn default() -> Self {
        Self { active: false, height: 3.0, thickness: 0.2, start: None, cursor: None, was_active: false }
    }
}

impl WallTool {
    /// Length of the segment being drawn, if any.
    pub fn segment_length(&self) -> Option<f32> {
        Some(self.start?.distance(self.cursor?))
    }
}

/// Wall from `a` to `b` on the floor: box size (length, height, thickness)
/// and its transform. The length grows by the thickness so chained walls
/// close their corners.
fn segment(a: Vec3, b: Vec3, height: f32, thickness: f32) -> (Vec3, Transform) {
    let d = b - a;
    let mid = (a + b) * 0.5;
    let size = Vec3::new(d.xz().length() + thickness, height, thickness);
    // Local +X along the wall: axis_x(yaw) = (cos, -sin).
    let t = Transform::from_xyz(mid.x, 0.0, mid.z).with_rotation(Quat::from_rotation_y((-d.z).atan2(d.x)));
    (size, t)
}

/// Both floor-level ends of a placed wall.
fn wall_ends(o: &PlacedObject, t: &Transform) -> [Vec3; 2] {
    // Inset by half the thickness: the length includes the corner overlap.
    let half = ((o.proto.aabb_max.x - o.proto.aabb_min.x) * t.scale.x - (o.proto.aabb_max.z - o.proto.aabb_min.z) * t.scale.z) * 0.5;
    let along = t.rotation * Vec3::X * half.max(0.0);
    let c = t.translation.with_y(0.0);
    [c - along, c + along]
}

#[allow(clippy::too_many_arguments)]
pub fn draw_walls(
    mut tool: ResMut<WallTool>,
    (mouse, keys): (Res<ButtonInput<MouseButton>>, Res<ButtonInput<KeyCode>>),
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    objects: Query<(&PlacedObject, &Transform)>,
    mut place: ResMut<PlaceMode>,
    mut selection: ResMut<Selection>,
    mut pending: ResMut<PendingSpawns>,
    mut cache: ResMut<PrototypeCache>,
    (block, settings, mut status): (Res<UiBlock>, Res<ViewSettings>, ResMut<Status>),
    mut gizmos: Gizmos,
) {
    // Turning the tool on stops placing; picking an asset turns it off.
    if tool.active && !tool.was_active {
        place.asset = None;
        selection.selected = None;
    } else if tool.active && place.asset.is_some() {
        tool.active = false;
    }
    tool.was_active = tool.active;
    if !tool.active {
        tool.start = None;
        tool.cursor = None;
        return;
    }
    selection.hovered = None;

    let (cam, cam_gt) = *camera;
    let ray = window.cursor_position().and_then(|c| cam.viewport_to_world(cam_gt, c).ok());
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    tool.cursor = ray.filter(|_| !block.pointer).and_then(|r| ray_plane_y(r, 0.0)).map(|mut p| {
        if ctrl {
            p = Vec3::new(snap(p.x, settings.snap), 0.0, snap(p.z, settings.snap));
        }
        if shift && let Some(s) = tool.start {
            let d = p - s;
            let step = PI / 12.0;
            let ang = (d.z.atan2(d.x) / step).round() * step;
            p = s + Vec3::new(ang.cos(), 0.0, ang.sin()) * d.length();
        }
        // Join the nearest wall end, or the chain start to close a loop.
        let ends = objects.iter().filter(|(o, _)| Kind::of_asset(&o.asset) == Some(Kind::Wall)).flat_map(|(o, t)| wall_ends(o, t));
        if let Some(e) = ends.filter(|e| e.distance(p) < JOIN_RADIUS && Some(*e) != tool.start).min_by(|a, b| a.distance(p).total_cmp(&b.distance(p))) {
            p = e;
        }
        p
    });

    if !block.pointer && mouse.just_pressed(MouseButton::Left)
        && let Some(p) = tool.cursor
    {
        match tool.start {
            Some(s) if s.distance(p) >= MIN_LENGTH => {
                let (size, transform) = segment(s, p, tool.height, tool.thickness);
                match structures::usd_for(Kind::Wall, size) {
                    Ok(asset) => {
                        cache.request(&asset);
                        pending.0.push(PendingSpawn { asset, transform, name: None, joints: None, select: false });
                        tool.start = Some(p);
                    }
                    Err(e) => status.0 = format!("Could not create wall: {e}"),
                }
            }
            Some(_) => {}
            None => tool.start = Some(p),
        }
    }
    let cancel = (!block.pointer && mouse.just_pressed(MouseButton::Right)) || (!block.keyboard && keys.just_pressed(KeyCode::Escape));
    if cancel {
        if tool.start.is_some() {
            tool.start = None;
        } else {
            tool.active = false;
        }
    }

    // Preview: footprint and outline of the wall being drawn, cursor cross.
    let amber = Color::srgb(1.0, 0.69, 0.13);
    if let Some(p) = tool.cursor {
        gizmos.line(p - Vec3::X * 0.25, p + Vec3::X * 0.25, amber);
        gizmos.line(p - Vec3::Z * 0.25, p + Vec3::Z * 0.25, amber);
        if let Some(s) = tool.start.filter(|s| s.distance(p) >= MIN_LENGTH) {
            let (size, t) = segment(s, p, tool.height, tool.thickness);
            gizmos.cube(t.with_translation(t.translation + Vec3::Y * size.y * 0.5).with_scale(size), amber);
        }
    }
    if let Some(s) = tool.start {
        gizmos.sphere(Isometry3d::from_translation(s), 0.08, amber);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_along_x() {
        let (size, t) = segment(Vec3::ZERO, Vec3::new(4.0, 0.0, 0.0), 3.0, 0.2);
        assert!((size - Vec3::new(4.2, 3.0, 0.2)).length() < 1e-5);
        assert!((t.translation - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-5);
        assert!((t.rotation * Vec3::X - Vec3::X).length() < 1e-5);
    }

    #[test]
    fn segment_along_z() {
        let (size, t) = segment(Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 5.0), 2.5, 0.3);
        assert!((size.x - 5.3).abs() < 1e-5);
        assert!((t.translation - Vec3::new(1.0, 0.0, 2.5)).length() < 1e-5);
        // Local X points from the start to the end.
        assert!((t.rotation * Vec3::X - Vec3::Z).length() < 1e-5);
    }
}
