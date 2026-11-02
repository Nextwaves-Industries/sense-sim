//! Devices in the 3D scene: one plain marker per device, sharing a mesh per class and a
//! material per status, so thousands of them stay cheap. They aren't `PlacedObject`s, since
//! undo despawns all of those, and they aren't RTLS `Device`s, which would feed them into the
//! radio simulation.

use std::collections::{HashMap, HashSet};

use bevy::light::NotShadowCaster;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::window::{CursorLeft, CursorMoved, PrimaryWindow};
use twin_proto::model::{DeviceClass, DeviceId, Liveness};

use super::yard::Yard;
use super::{Placement, TwinInventory, TwinScene, TwinSelection};
use crate::scene::{OrbitCamera, PlacedObject, Selection, UiBlock};
use crate::ui::{HideOverlays, LeftPanelSlot, Workbench};

/// How close to a marker on screen counts as pointing at it.
const PICK_PIXELS: f32 = 14.0;
const DRAG_PIXELS: f32 = 4.0;
/// Gap between the scene and the yard.
const YARD_MARGIN: f32 = 8.0;

#[derive(Component)]
pub struct TwinMarker {
    pub id: DeviceId,
}

/// Width, height and depth of a class's marker, in metres.
fn marker_size(class: DeviceClass) -> Vec3 {
    match class {
        DeviceClass::Plc => Vec3::new(0.6, 0.8, 0.35),
        DeviceClass::Hmi => Vec3::new(0.7, 0.5, 0.12),
        DeviceClass::Pc => Vec3::new(0.45, 0.6, 0.45),
        DeviceClass::Switch => Vec3::new(0.7, 0.2, 0.3),
        DeviceClass::Drive => Vec3::new(0.35, 1.0, 0.35),
        DeviceClass::Sensor => Vec3::new(0.14, 0.14, 0.14),
        DeviceClass::IoLinkMaster => Vec3::new(0.35, 0.3, 0.2),
        DeviceClass::Gateway => Vec3::new(0.3, 0.4, 0.2),
        DeviceClass::Robot => Vec3::new(0.5, 1.3, 0.5),
        DeviceClass::Cnc => Vec3::new(1.2, 1.1, 0.9),
        DeviceClass::Meter => Vec3::new(0.3, 0.3, 0.12),
        DeviceClass::Unknown => Vec3::new(0.3, 0.3, 0.3),
    }
}

#[derive(Resource)]
pub struct TwinAssets {
    meshes: HashMap<DeviceClass, Handle<Mesh>>,
    up: Handle<StandardMaterial>,
    suspect: Handle<StandardMaterial>,
    down: Handle<StandardMaterial>,
    stale: Handle<StandardMaterial>,
}

impl TwinAssets {
    fn material(&self, liveness: Liveness) -> &Handle<StandardMaterial> {
        match liveness {
            Liveness::Up => &self.up,
            Liveness::Suspect => &self.suspect,
            Liveness::Down => &self.down,
            Liveness::Unknown | Liveness::Stale | Liveness::Retired => &self.stale,
        }
    }
}

pub fn setup_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mut paint = |r: f32, g: f32, b: f32| materials.add(StandardMaterial { base_color: Color::srgb(r, g, b), perceptual_roughness: 0.7, ..default() });
    let (up, suspect, down, stale) = (paint(0.16, 0.62, 0.34), paint(0.88, 0.55, 0.08), paint(0.88, 0.2, 0.18), paint(0.6, 0.62, 0.66));
    let meshes = DeviceClass::ALL.into_iter().map(|class| {
        let size = marker_size(class);
        (class, meshes.add(Cuboid::new(size.x, size.y, size.z)))
    });
    let meshes = meshes.collect();
    commands.insert_resource(TwinAssets { meshes, up, suspect, down, stale });
}

fn in_front(slot: &LeftPanelSlot) -> bool {
    slot.bench.current == Workbench::Factory && !slot.canvas_taken()
}

/// Where the yard goes: past the scene's far side, or at the origin in an empty scene.
fn yard_origin(objects: &Query<&Transform, (With<PlacedObject>, Without<TwinMarker>)>) -> Vec2 {
    let mut any = false;
    let (mut max_x, mut min_z) = (f32::MIN, f32::MAX);
    for transform in objects {
        any = true;
        max_x = max_x.max(transform.translation.x);
        min_z = min_z.min(transform.translation.z);
    }
    if any { Vec2::new(max_x + YARD_MARGIN, min_z) } else { Vec2::ZERO }
}

/// Keeps one marker per device up to date. All of this only runs after something changed.
#[allow(clippy::too_many_arguments)]
pub fn sync_markers(
    mut commands: Commands,
    inventory: Res<TwinInventory>,
    scene: Res<TwinScene>,
    mut yard: ResMut<Yard>,
    slot: Res<LeftPanelSlot>,
    hide: Res<HideOverlays>,
    assets: Option<Res<TwinAssets>>,
    mut markers: Query<(Entity, &TwinMarker, &mut Transform, &mut MeshMaterial3d<StandardMaterial>, &mut Visibility)>,
    objects: Query<&Transform, (With<PlacedObject>, Without<TwinMarker>)>,
    mut seen: Local<(u64, usize, bool, HashSet<DeviceId>)>,
) {
    let Some(assets) = assets else { return };
    let visible = in_front(&slot) && hide.0 == 0;
    let object_count = objects.iter().count();
    let list_changed = seen.0 != inventory.generation;
    if !list_changed && seen.1 == object_count && seen.2 == visible && !scene.is_changed() {
        return;
    }
    let rebuild_yard = list_changed && (seen.3.len() != inventory.mirror.devices.len() || seen.3.is_empty()) || seen.1 != object_count;
    let first_devices = seen.3.is_empty() && !inventory.mirror.devices.is_empty();
    if rebuild_yard {
        let frame_pending = yard.frame_pending || first_devices;
        *yard = Yard::build(inventory.mirror.devices.values(), yard_origin(&objects));
        yard.frame_pending = frame_pending;
    }
    seen.0 = inventory.generation;
    seen.1 = object_count;
    seen.2 = visible;

    let shown_visibility = if visible { Visibility::Inherited } else { Visibility::Hidden };
    let target_of = |id: &DeviceId| -> Option<(Vec3, Liveness)> {
        let device = inventory.mirror.devices.get(id).filter(|d| d.liveness != Liveness::Retired)?;
        let ground = yard.ground(device, &scene.placed)?;
        Some((ground + Vec3::Y * marker_size(device.class).y / 2.0, inventory.shown_liveness(device)))
    };

    let mut have = HashSet::with_capacity(inventory.mirror.devices.len());
    for (entity, marker, mut transform, mut material, mut visibility) in &mut markers {
        let Some((position, liveness)) = target_of(&marker.id) else {
            commands.entity(entity).despawn();
            continue;
        };
        have.insert(marker.id.clone());
        // Touch a component only when it differs, so Bevy doesn't redo work for 5,000 markers.
        if transform.translation != position {
            transform.translation = position;
        }
        let wanted = assets.material(liveness);
        if material.0 != *wanted {
            material.0 = wanted.clone();
        }
        if *visibility != shown_visibility {
            *visibility = shown_visibility;
        }
    }
    for device in inventory.mirror.devices.values() {
        if have.contains(&device.id) {
            continue;
        }
        let Some((position, liveness)) = target_of(&device.id) else { continue };
        have.insert(device.id.clone());
        commands.spawn((
            TwinMarker { id: device.id.clone() },
            Mesh3d(assets.meshes[&device.class].clone()),
            MeshMaterial3d(assets.material(liveness).clone()),
            Transform::from_translation(position),
            shown_visibility,
            Pickable::IGNORE,
            NotShadowCaster,
        ));
    }
    seen.3 = inventory.mirror.devices.keys().cloned().collect();
}

#[derive(Default)]
pub struct MarkerDrag {
    device: Option<DeviceId>,
    press: Vec2,
    offset: Vec3,
    moved: bool,
}

fn floor_point(ray: Ray3d) -> Option<Vec3> {
    crate::scene::ray_plane_y(ray, 0.0)
}

/// Picks and drags markers. Screen space, because they're small. Runs before the scene's own
/// pointer handling and takes the click when it hits one.
#[allow(clippy::too_many_arguments)]
pub fn marker_pointer(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    markers: Query<(&TwinMarker, &Transform)>,
    block: Res<UiBlock>,
    slot: Res<LeftPanelSlot>,
    mut scene: ResMut<TwinScene>,
    mut twin_selection: ResMut<TwinSelection>,
    mut selection: ResMut<Selection>,
    mut rtls_selection: ResMut<crate::rtls::RtlsSelection>,
    mut drag: Local<MarkerDrag>,
    (mut moved, mut left, mut last_cursor): (MessageReader<CursorMoved>, MessageReader<CursorLeft>, Local<Option<Vec2>>),
) {
    // From the input messages rather than the window, so scripted runs (`--drive`) can
    // pick and drag markers too. Those can't move the window's cursor without warping the
    // real pointer.
    if let Some(latest) = moved.read().last() {
        *last_cursor = Some(latest.position);
    }
    if left.read().next().is_some() {
        *last_cursor = None;
    }
    if !in_front(&slot) {
        twin_selection.hovered = None;
        drag.device = None;
        return;
    }
    let (cam, cam_gt) = *camera;
    let cursor = last_cursor.or_else(|| window.cursor_position());
    let ray = cursor.and_then(|c| cam.viewport_to_world(cam_gt, c).ok());

    if let Some(id) = drag.device.clone() {
        if !mouse.pressed(MouseButton::Left) {
            drag.device = None;
            return;
        }
        if !drag.moved && cursor.is_none_or(|c| c.distance(drag.press) < DRAG_PIXELS) {
            return;
        }
        drag.moved = true;
        if let Some(point) = ray.and_then(floor_point) {
            let position = (point + drag.offset).with_y(0.0);
            scene.placed.insert(id, Placement { position: position.to_array() });
        }
        return;
    }

    twin_selection.hovered = None;
    if block.pointer {
        return;
    }
    let mut best: Option<(f32, &DeviceId, Vec3)> = None;
    if let Some(cursor) = cursor {
        for (marker, transform) in &markers {
            if let Ok(on_screen) = cam.world_to_viewport(cam_gt, transform.translation) {
                let distance = on_screen.distance(cursor);
                if distance < PICK_PIXELS && best.is_none_or(|(b, ..)| distance < b) {
                    best = Some((distance, &marker.id, transform.translation));
                }
            }
        }
    }
    twin_selection.hovered = best.map(|(_, id, _)| id.clone());

    if mouse.just_pressed(MouseButton::Left) {
        if let Some((_, id, at)) = best {
            mouse.clear_just_pressed(MouseButton::Left);
            twin_selection.device = Some(id.clone());
            selection.set(None);
            rtls_selection.device = None;
            rtls_selection.zone = None;
            let offset = ray.and_then(floor_point).map_or(Vec3::ZERO, |p| (at - p).with_y(0.0));
            *drag = MarkerDrag { device: Some(id.clone()), press: cursor.unwrap_or_default(), offset, moved: false };
        } else {
            twin_selection.device = None;
        }
    }
    if !block.keyboard && keys.just_pressed(KeyCode::Escape) {
        twin_selection.device = None;
    }
}

/// "Frame all" here means the devices: the scene may be empty, and framing an empty scene
/// would leave the camera staring at nothing.
pub fn frame_request(actions: Res<crate::scene::Actions>, slot: Res<LeftPanelSlot>, mut yard: ResMut<Yard>) {
    if in_front(&slot) && actions.0.iter().any(|a| matches!(a, crate::scene::Action::FrameAll)) {
        yard.frame_pending = true;
    }
}

/// Moves the camera to take in every device, once there are markers to look at. Runs after
/// the scene's actions so it has the last word over `FrameAll`.
pub fn frame_devices(mut yard: ResMut<Yard>, markers: Query<&Transform, With<TwinMarker>>, camera: Single<(&mut OrbitCamera, &GlobalTransform)>) {
    if !yard.frame_pending {
        return;
    }
    let mut extent: Option<(Vec3, Vec3)> = None;
    for transform in &markers {
        let at = transform.translation;
        extent = Some(extent.map_or((at, at), |(min, max)| (min.min(at), max.max(at))));
    }
    let Some((min, max)) = extent else { return };
    yard.frame_pending = false;
    let (mut orbit, placed_at) = camera.into_inner();
    orbit.radius = (min.distance(max) * 1.25).clamp(8.0, 800.0);
    orbit.pitch = orbit.pitch.max(0.7);
    // The cards cover the left of the view, so look a little left of the middle: that puts
    // the devices in the part of the canvas that's actually free.
    orbit.focus = (min + max) / 2.0 - placed_at.right() * orbit.radius * 0.2;
}

/// An object picked elsewhere takes over from a selected device.
pub fn sync_selection(selection: Res<Selection>, mut twin_selection: ResMut<TwinSelection>) {
    if selection.is_changed() && selection.selected.is_some() && twin_selection.device.is_some() {
        twin_selection.device = None;
    }
}

/// Rings for the selected and hovered device, a line to a sensor's master, and yard outlines.
pub fn draw(mut gizmos: Gizmos, inventory: Res<TwinInventory>, scene: Res<TwinScene>, yard: Res<Yard>, selected: Res<TwinSelection>, slot: Res<LeftPanelSlot>, hide: Res<HideOverlays>) {
    if !in_front(&slot) || hide.0 > 0 {
        return;
    }
    let outline = Color::srgba(0.35, 0.4, 0.5, 0.55);
    for group in &yard.groups {
        let (a, b) = (group.min, group.max);
        let corners = [Vec3::new(a.x, 0.01, a.y), Vec3::new(b.x, 0.01, a.y), Vec3::new(b.x, 0.01, b.y), Vec3::new(a.x, 0.01, b.y)];
        for i in 0..4 {
            gizmos.line(corners[i], corners[(i + 1) % 4], outline);
        }
    }
    let ground_of = |id: &DeviceId| inventory.mirror.devices.get(id).and_then(|d| yard.ground(d, &scene.placed));
    let ring = |gizmos: &mut Gizmos, at: Vec3, radius: f32, color: Color| {
        gizmos.circle(Isometry3d::new(at + Vec3::Y * 0.02, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), radius, color);
    };
    if let Some(at) = selected.hovered.as_ref().and_then(ground_of) {
        ring(&mut gizmos, at, 0.55, Color::srgb(0.45, 0.6, 0.95));
    }
    if let Some(id) = &selected.device
        && let Some(at) = ground_of(id)
    {
        ring(&mut gizmos, at, 0.7, Color::srgb(0.18, 0.43, 0.94));
        let parent = inventory.mirror.devices.get(id).and_then(|d| d.parent.as_ref());
        if let Some(from) = parent.and_then(|p| ground_of(&p.device)) {
            gizmos.line(at + Vec3::Y * 0.1, from + Vec3::Y * 0.1, Color::srgb(0.18, 0.43, 0.94));
        }
    }
}
