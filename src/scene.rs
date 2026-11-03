//! Scene editing: orbit camera, placing assets, selection, move/rotate/lift
//! handles, interactive joints, gizmos, and saving/exporting the layout.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::camera::visibility::{NoAutoAabb, VisibilityRange};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::picking::mesh_picking::ray_cast::RayCastBackfaces;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::bvh::Bvh;
use crate::lod::LodRange;
use crate::prototype::{ProtoState, Prototype, PrototypeCache};
use crate::usd_load::JointKind;

// ---------------------------------------------------------------------------
// Components & resources

#[derive(Component)]
pub struct PlacedObject {
    pub asset: PathBuf,
    pub name: String,
    pub proto: Arc<Prototype>,
    /// Current value of each movable joint (degrees or stage units).
    pub joints: Vec<f32>,
    /// Can't be moved, rotated, scaled, dropped or deleted, by hand or by the
    /// assistant, until unlocked. Its moving parts still work.
    pub locked: bool,
}

impl PlacedObject {
    pub fn aabb(&self) -> (Vec3, Vec3) {
        (self.proto.aabb_min, self.proto.aabb_max)
    }
}

/// Root of the placement preview that follows the cursor.
#[derive(Component)]
pub struct Ghost;

/// Marks a mesh entity as belonging to an object root (placed or ghost).
#[derive(Component)]
pub struct PartOf(pub Entity);

/// Index of the prototype part this entity renders (for joint posing).
#[derive(Component)]
pub struct PartRest(pub usize);

#[derive(Component)]
pub struct Ground;

#[derive(Component)]
pub struct OrbitCamera {
    pub focus: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub radius: f32,
}

#[derive(Component)]
pub struct Sun;

/// Thick, always-on-top lines for the selection box and handles.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct HandleGizmos;

#[derive(Resource, Default)]
pub struct Selection {
    /// The primary selection (properties card, handles).
    pub selected: Option<Entity>,
    pub hovered: Option<Entity>,
    /// Further selected objects (Ctrl/Shift+click, Ctrl+A): they move,
    /// rotate, duplicate and delete together with the primary one.
    pub group: Vec<Entity>,
}

impl Selection {
    /// Every selected object, primary first.
    pub fn all(&self) -> Vec<Entity> {
        self.selected.into_iter().chain(self.group.iter().copied().filter(|e| Some(*e) != self.selected)).collect()
    }
    pub fn contains(&self, e: Entity) -> bool {
        self.selected == Some(e) || self.group.contains(&e)
    }
    /// Selects only `e` (or nothing).
    pub fn set(&mut self, e: Option<Entity>) {
        self.selected = e;
        self.group.clear();
    }
    /// Ctrl/Shift+click: add `e`, or take it out if it is already selected.
    pub fn toggle(&mut self, e: Entity) {
        if self.selected == Some(e) {
            self.selected = (!self.group.is_empty()).then(|| self.group.remove(0));
        } else if let Some(i) = self.group.iter().position(|x| *x == e) {
            self.group.remove(i);
        } else {
            if let Some(p) = self.selected {
                self.group.push(p);
            }
            self.selected = Some(e);
        }
    }
}

#[derive(Resource, Default)]
pub struct PlaceMode {
    pub asset: Option<PathBuf>,
    pub ghost: Option<(Entity, PathBuf)>,
    pub yaw: f32,
    /// Keep placing copies after each click.
    pub sticky: bool,
    /// The preview currently cuts into another object, so it can't be placed.
    pub blocked: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Handle {
    Joint(usize),
    Rotate,
    Lift,
}

#[derive(Resource, Default)]
pub struct DragState {
    active: Option<Drag>,
    /// Object just let go after a move, for gravity to drop.
    released: Option<Entity>,
}

impl DragState {
    /// The object being moved or lifted right now.
    pub fn holding(&self) -> Option<Entity> {
        match &self.active {
            Some(Drag::Move { entity, .. } | Drag::Handle { entity, .. }) => Some(*entity),
            None => None,
        }
    }
}

/// Held in the air on purpose (lifted, or moved with Alt): gravity leaves it alone.
#[derive(Component)]
pub struct Floating;

/// Falling under gravity, with its current speed (m/s).
#[derive(Component)]
pub struct Falling(f32);

/// "Drop": land on whatever is under the object (a conveyor, a shelf, the
/// floor), with or without gravity switched on.
#[derive(Component)]
pub struct DropNow;

enum Drag {
    /// Moving the object across surfaces by its body.
    Move { entity: Entity, offset: Vec3, moved: bool, press_pos: Vec2 },
    /// Dragging one of the selection handles.
    Handle { entity: Entity, handle: Handle, start: f32, ref_vec: Vec3, ref_param: f32, press_pos: Vec2 },
}

/// Screen-space joint markers for the selected object, drawn by the UI.
#[derive(Resource, Default)]
pub struct HandleUi {
    pub joints: Vec<JointMarker>,
    pub hovered: Option<Handle>,
    pub active: Option<Handle>,
    /// Set when the selection is tiny on screen: ring and lift arrow are hidden
    /// and a "press F" hint is drawn at this screen point.
    pub compact: Option<Vec2>,
}

pub struct JointMarker {
    pub index: usize,
    /// Where the dot is drawn (spread apart from overlapping dots).
    pub screen: Vec2,
    /// Where the joint really is on screen.
    pub anchor: Vec2,
    pub label: String,
}

/// Below this on-screen size the selection only shows joint dots.
const COMPACT_PX: f32 = 70.0;
/// Minimum distance between joint dots on screen.
const MARKER_SPACING: f32 = 18.0;

/// Spawns waiting for their prototype to finish loading.
#[derive(Resource, Default)]
pub struct PendingSpawns(pub Vec<PendingSpawn>);

/// Layout objects whose asset is not on disk yet: downloaded from the online
/// library, then spawned like any pending object.
#[derive(Resource, Default)]
pub struct PendingDownloads(pub Vec<PendingDownload>);

pub struct PendingDownload {
    /// Asset folder in the online library.
    pub folder: String,
    pub transform: Transform,
    pub name: Option<String>,
    /// The download was queued.
    pub requested: bool,
}

pub struct PendingSpawn {
    pub asset: PathBuf,
    pub transform: Transform,
    pub name: Option<String>,
    pub joints: Option<Vec<f32>>,
    pub select: bool,
    pub locked: bool,
}

/// Set by the UI each frame: whether egui owns the pointer / keyboard.
#[derive(Resource, Default)]
pub struct UiBlock {
    pub pointer: bool,
    pub keyboard: bool,
}

#[derive(Resource)]
pub struct ViewSettings {
    pub grid: bool,
    pub bounds: bool,
    pub snap: f32,
    pub sun_lux: f32,
    pub shadows: bool,
    pub env_intensity: f32,
    /// Placing and dragging refuse positions that cut into other objects.
    pub no_overlap: bool,
    /// Objects drop onto whatever is under them.
    pub gravity: bool,
    /// Draw coarser meshes when zoomed out and skip parts too small to see.
    pub sampling: bool,
    /// Skip parts hidden behind others (GPU occlusion culling).
    pub occlusion: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self { grid: true, bounds: true, snap: 0.25, sun_lux: 3500.0, shadows: true, env_intensity: 2200.0, no_overlap: true, gravity: true, sampling: true, occlusion: false }
    }
}

#[derive(Resource, Default)]
pub struct Status(pub String);

#[derive(Resource, Default)]
pub struct NameCounter(pub std::collections::HashMap<String, usize>);

/// Actions requested by the UI, applied by `apply_actions`.
#[derive(Resource, Default)]
pub struct Actions(pub Vec<Action>);

pub enum Action {
    Select(Option<Entity>),
    /// Ctrl+A: every placed object.
    SelectAll,
    Delete(Entity),
    Duplicate(Entity),
    Focus(Entity),
    ClearScene,
    SaveLayout(PathBuf),
    LoadLayout(PathBuf),
    ExportUsda(PathBuf),
    FrameAll,
    /// Let a lifted object go: it lands on what is under it.
    Drop(Entity),
    /// Lock or unlock these objects (see `PlacedObject::locked`).
    SetLocked(Vec<Entity>, bool),
}

// ---------------------------------------------------------------------------
// Setup

pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut gizmo_store: ResMut<GizmoConfigStore>,
) {
    let (cfg, _) = gizmo_store.config_mut::<HandleGizmos>();
    cfg.line.width = 3.5;
    cfg.depth_bias = -1.0;

    let env = images.add(studio_cubemap(128));
    commands.spawn((
        Camera3d::default(),
        // Of Bevy's tone curves, the closest to NVIDIA's own renders of these assets: right
        // overall brightness, whites that stay white, colors that don't drift toward gray.
        bevy::core_pipeline::tonemapping::Tonemapping::SomewhatBoringDisplayTransform,
        GeneratedEnvironmentMapLight { environment_map: env, intensity: 2200.0, ..default() },
        Transform::from_xyz(3.0, 2.5, 4.0).looking_at(Vec3::new(0.0, 0.5, 0.0), Vec3::Y),
        OrbitCamera { focus: Vec3::new(0.0, 0.5, 0.0), yaw: 0.65, pitch: 0.45, radius: 5.0 },
    ));
    commands.spawn((
        DirectionalLight { illuminance: 3500.0, shadow_maps_enabled: true, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.8, -0.9, 0.0)),
        Sun,
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::srgb(0.85, 0.9, 1.0), brightness: 80.0, ..default() });
    let [r, g, b] = crate::theme::CANVAS_SRGB;
    commands.insert_resource(ClearColor(Color::srgb(r, g, b)));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(200.0, 200.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            // Under the default lights this comes out close to the canvas color.
            base_color: Color::linear_rgb(0.81, 0.81, 0.79),
            perceptual_roughness: 0.95,
            reflectance: 0.2,
            ..default()
        })),
        Transform::from_xyz(0.0, -0.001, 0.0),
        Ground,
    ));
}

// ---------------------------------------------------------------------------
// Spawning

fn unique_name(counter: &mut NameCounter, asset: &Path) -> String {
    let base = asset
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .or_else(|| asset.file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "asset".into());
    let n = counter.0.entry(base.clone()).or_insert(0);
    *n += 1;
    format!("{base}_{n}")
}

fn spawn_parts(commands: &mut Commands, root: Entity, proto: &Prototype, ghost: bool) {
    let basis = commands.spawn((proto.basis, Visibility::Inherited, ChildOf(root))).id();
    for (i, part) in proto.parts.iter().enumerate() {
        let diagonal = (part.aabb.half_extents * 2.0).length();
        let error_of = |level: usize| part.lods.get(level).map(|l| l.error);
        let mut e = commands.spawn((
            Mesh3d(part.mesh.clone()),
            MeshMaterial3d(part.material.clone()),
            part.transform,
            PartOf(root),
            PartRest(i),
            ChildOf(basis),
            Name::new(part.name.clone()),
            part.aabb,
            NoAutoAabb,
            LodRange { error: 0.0, next_error: error_of(0), diagonal, triangles: part.triangles, object: root },
            VisibilityRange::abrupt(0.0, f32::MAX),
        ));
        if ghost || part.see_through {
            e.insert(bevy::light::NotShadowCaster);
        }
        if !ghost {
            // USD decks are often single-sided with either winding: hit both sides
            // (Bevy picking); our own casts go through the part's BVH.
            e.insert((RayCastBackfaces, PartBvh(part.bvh.clone())));
        }
        let part_entity = e.id();
        // Coarser versions follow the part (and its joint pose). They are for drawing
        // only: without `PartOf`, ray casts and scene queries pass them by.
        for (level, l) in part.lods.iter().enumerate() {
            let mut version = commands.spawn((
                Mesh3d(l.mesh.clone()),
                MeshMaterial3d(part.material.clone()),
                ChildOf(part_entity),
                part.aabb,
                NoAutoAabb,
                LodRange { error: l.error, next_error: error_of(level + 1), diagonal, triangles: l.triangles, object: root },
                VisibilityRange::abrupt(f32::MAX, f32::MAX),
                Pickable::IGNORE,
            ));
            if ghost || part.see_through {
                version.insert(bevy::light::NotShadowCaster);
            }
        }
    }
}

pub fn spawn_object(
    commands: &mut Commands,
    counter: &mut NameCounter,
    proto: &Arc<Prototype>,
    asset: &Path,
    transform: Transform,
    name: Option<String>,
    joints: Option<Vec<f32>>,
) -> Entity {
    let name = name.unwrap_or_else(|| unique_name(counter, asset));
    let mut joints = joints.unwrap_or_default();
    joints.resize(proto.joints.len(), 0.0);
    let root = commands
        .spawn((
            transform,
            Visibility::Visible,
            Name::new(name.clone()),
            PlacedObject { asset: asset.to_path_buf(), name, proto: proto.clone(), joints, locked: false },
        ))
        .id();
    spawn_parts(commands, root, proto, false);
    root
}

/// Mesh parts created per frame while a scene loads (at least one whole object).
const SPAWN_PARTS_PER_FRAME: usize = 1500;

pub fn process_pending(
    mut commands: Commands,
    mut pending: ResMut<PendingSpawns>,
    mut cache: ResMut<PrototypeCache>,
    mut counter: ResMut<NameCounter>,
    mut selection: ResMut<Selection>,
    mut status: ResMut<Status>,
) {
    if pending.0.is_empty() {
        return;
    }
    let mut keep = Vec::new();
    let mut parts_left = SPAWN_PARTS_PER_FRAME;
    for p in pending.0.drain(..) {
        cache.request(&p.asset);
        match cache.states.get(&p.asset) {
            // A big scene appears over a few frames rather than freezing one.
            Some(ProtoState::Ready(_)) if parts_left == 0 => keep.push(p),
            Some(ProtoState::Ready(proto)) => {
                let proto = proto.clone();
                parts_left = parts_left.saturating_sub(proto.parts.len());
                let e = spawn_object(&mut commands, &mut counter, &proto, &p.asset, p.transform, p.name, p.joints);
                if p.locked {
                    commands.entity(e).entry::<PlacedObject>().and_modify(|mut o| o.locked = true);
                }
                if p.select {
                    selection.selected = Some(e);
                }
            }
            Some(ProtoState::Failed(err)) => status.0 = format!("Failed to load {}: {err}", p.asset.display()),
            _ => keep.push(p),
        }
    }
    pending.0 = keep;
}

/// Keeps the placement ghost in sync with the chosen asset.
pub fn update_ghost(
    mut commands: Commands,
    mut place: ResMut<PlaceMode>,
    mut cache: ResMut<PrototypeCache>,
    mut status: ResMut<Status>,
) {
    let wanted = place.asset.clone();
    if let Some((e, path)) = &place.ghost
        && Some(path) != wanted.as_ref()
    {
        commands.entity(*e).despawn();
        place.ghost = None;
    }
    let Some(asset) = wanted else { return };
    if place.ghost.is_some() {
        return;
    }
    cache.request(&asset);
    match cache.states.get(&asset) {
        Some(ProtoState::Ready(proto)) => {
            let root = commands
                .spawn((Transform::from_xyz(0.0, -1000.0, 0.0), Visibility::Hidden, Ghost))
                .id();
            spawn_parts(&mut commands, root, proto, true);
            place.ghost = Some((root, asset));
        }
        Some(ProtoState::Failed(err)) => {
            status.0 = format!("Failed to load {}: {err}", asset.display());
            place.asset = None;
        }
        _ => {}
    }
}

/// Poses moving parts whenever an object's joint values change.
pub fn apply_joints(
    changed: Query<(Entity, &PlacedObject), Changed<PlacedObject>>,
    mut parts: Query<(&PartOf, &PartRest, &mut Transform)>,
) {
    let mut deltas = std::collections::HashMap::new();
    for (e, obj) in &changed {
        if !obj.proto.links.is_empty() {
            deltas.insert(e, (obj.proto.clone(), obj.proto.body_deltas(&obj.joints)));
        }
    }
    if deltas.is_empty() {
        return;
    }
    for (of, rest, mut t) in &mut parts {
        let Some((proto, d)) = deltas.get(&of.0) else { continue };
        let part = &proto.parts[rest.0];
        let m = match part.body.and_then(|b| d.get(b)) {
            Some(delta) => *delta * part.transform.to_matrix(),
            None => part.transform.to_matrix(),
        };
        *t = Transform::from_matrix(m);
    }
}

// ---------------------------------------------------------------------------
// Camera

pub fn orbit_camera(
    mut cam: Single<(&mut OrbitCamera, &mut Transform, &mut Projection)>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    block: Res<UiBlock>,
    handles: Res<HandleUi>,
    time: Res<Time>,
    mut drag_began_on_ui: Local<bool>,
) {
    let (ref mut orbit, ref mut transform, ref mut projection) = *cam;
    let d = motion.delta;
    let pointer_ok = !block.pointer;
    // Decided on the press: a camera drag may then run across the cards, but one
    // that starts on a card is not a camera drag.
    if mouse.just_pressed(MouseButton::Right) || mouse.just_pressed(MouseButton::Middle) {
        *drag_began_on_ui = !pointer_ok;
    }
    let dragging = !*drag_began_on_ui;
    let orbiting = dragging && mouse.pressed(MouseButton::Right) && !keys.pressed(KeyCode::ShiftLeft);
    let panning = dragging && (mouse.pressed(MouseButton::Middle) || (mouse.pressed(MouseButton::Right) && keys.pressed(KeyCode::ShiftLeft)));
    if pointer_ok || orbiting || panning {
        if orbiting {
            orbit.yaw -= d.x * 0.006;
            orbit.pitch = (orbit.pitch + d.y * 0.006).clamp(-1.5, 1.5);
        } else if panning {
            let right = transform.rotation * Vec3::X;
            let up = transform.rotation * Vec3::Y;
            let k = orbit.radius * 0.0015;
            orbit.focus += (-right * d.x + up * d.y) * k;
        }
    }
    if pointer_ok {
        let lines = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        // Over a joint dot the wheel moves the joint instead (see `interact`).
        if lines != 0.0 && !matches!(handles.hovered, Some(Handle::Joint(_))) {
            orbit.radius = (orbit.radius * 0.88f32.powf(lines)).clamp(0.05, 500.0);
        }
    }
    if !block.keyboard && !keys.pressed(KeyCode::ControlLeft) {
        let fwd = Vec3::new(-orbit.yaw.sin(), 0.0, -orbit.yaw.cos());
        let right = Vec3::new(orbit.yaw.cos(), 0.0, -orbit.yaw.sin());
        let mut mv = Vec3::ZERO;
        if keys.pressed(KeyCode::KeyW) { mv += fwd; }
        if keys.pressed(KeyCode::KeyS) { mv -= fwd; }
        if keys.pressed(KeyCode::KeyD) { mv += right; }
        if keys.pressed(KeyCode::KeyA) { mv -= right; }
        if keys.pressed(KeyCode::Space) { mv += Vec3::Y; }
        if keys.pressed(KeyCode::KeyC) { mv -= Vec3::Y; }
        let speed = orbit.radius.max(1.0) * if keys.pressed(KeyCode::ShiftLeft) { 2.5 } else { 0.8 };
        orbit.focus += mv.normalize_or_zero() * speed * time.delta_secs();
    }
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    transform.rotation = rot;
    transform.translation = orbit.focus + rot * Vec3::new(0.0, 0.0, orbit.radius);
    // Bevy's default 0.1 m near plane slices through small assets (and the
    // floor) once you zoom in on them; keep it a small fraction of the distance.
    if let Projection::Perspective(p) = &mut **projection {
        let near = (orbit.radius * 0.02).clamp(0.001, 0.1);
        if p.near != near {
            p.near = near;
        }
    }
}

fn focus_on(orbit: &mut OrbitCamera, center: Vec3, size: f32) {
    orbit.focus = center;
    orbit.radius = (size * 1.6).max(0.3);
}

// ---------------------------------------------------------------------------
// Geometry helpers (bounding boxes keep interaction cheap on huge meshes)

pub(crate) fn ray_plane(ray: Ray3d, point: Vec3, normal: Vec3) -> Option<Vec3> {
    let denom = ray.direction.dot(normal);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (point - ray.origin).dot(normal) / denom;
    (t > 0.0).then(|| ray.origin + ray.direction * t)
}

pub(crate) fn ray_plane_y(ray: Ray3d, y: f32) -> Option<Vec3> {
    ray_plane(ray, Vec3::new(0.0, y, 0.0), Vec3::Y)
}

/// Ray vs. an object's oriented bounding box; returns the hit distance.
fn ray_obb(ray: Ray3d, t: &Transform, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = t.to_matrix().inverse();
    let o = inv.transform_point3(ray.origin);
    let d = inv.transform_vector3(*ray.direction);
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if o[i] < min[i] || o[i] > max[i] {
                return None;
            }
        } else {
            let a = (min[i] - o[i]) / d[i];
            let b = (max[i] - o[i]) / d[i];
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

/// Triangle tree of a placed object's mesh part (mesh space).
#[derive(Component)]
pub struct PartBvh(pub Arc<Bvh>);

/// Mesh parts of placed objects, for ray casts through their BVHs.
pub type Parts<'w, 's> = Query<'w, 's, (&'static PartOf, &'static PartBvh, &'static GlobalTransform, &'static InheritedVisibility)>;

/// Nearest hit of `ray` on visible parts of placed objects passing `is_object` (not
/// `exclude`), from either side of a face: object, world point and unit normal.
fn cast_parts(parts: &Parts, is_object: &dyn Fn(Entity) -> bool, ray: Ray3d, exclude: Option<Entity>) -> Option<(Entity, Vec3, Vec3)> {
    let mut best: Option<(f32, Entity, Vec3)> = None;
    for (of, bvh, gt, vis) in parts {
        if !vis.get() || Some(of.0) == exclude || !is_object(of.0) {
            continue;
        }
        // Cast in mesh space; with an unnormalized local direction, t stays in world units.
        let inv = gt.affine().inverse();
        let o = inv.transform_point3(ray.origin);
        let d = inv.transform_vector3(*ray.direction);
        let limit = best.map_or(f32::MAX, |b| b.0);
        if let Some((t, n)) = bvh.0.cast(o, d, limit) {
            let n = (inv.matrix3.transpose() * n).normalize_or_zero();
            best = Some((t, of.0, n));
        }
    }
    best.map(|(t, e, n)| (e, ray.origin + *ray.direction * t, n))
}

/// Nearest hit of `ray` on the meshes of placed objects (not `exclude`): point and normal.
fn cast_objects(parts: &Parts, is_object: &dyn Fn(Entity) -> bool, ray: Ray3d, exclude: Option<Entity>) -> Option<(Vec3, Vec3)> {
    cast_parts(parts, is_object, ray, exclude).map(|(_, p, n)| (p, n))
}

/// Where the cursor aims for dropping things: the point on the cursor ray at the
/// height of the nearest surface it touches, and the height to search for support
/// from. A few slightly spread rays are used so wire decks and grates, whose gaps a
/// single ray slips through, still count. Ignores `exclude`.
fn aim(
    parts: &Parts,
    is_object: &dyn Fn(Entity) -> bool,
    ray: Ray3d,
    exclude: Option<Entity>,
) -> Option<(Vec3, f32)> {
    const SPREAD: f32 = 0.008; // radians, a few pixels
    let dir = *ray.direction;
    let u = dir.any_orthonormal_vector();
    let v = dir.cross(u);
    let mut best: Option<(f32, Vec3, Vec3)> = None;
    for k in 0..5 {
        let d = if k == 0 {
            dir
        } else {
            let a = (k - 1) as f32 * FRAC_PI_2;
            (dir + (u * a.cos() + v * a.sin()) * SPREAD).normalize()
        };
        let r = Ray3d::new(ray.origin, Dir3::new_unchecked(d));
        if let Some((p, n)) = cast_objects(parts, is_object, r, exclude) {
            let dist = p.distance(ray.origin);
            if best.is_none_or(|(b, _, _)| dist < b) {
                best = Some((dist, p, if n.dot(d) > 0.0 { -n } else { n }));
            }
        }
    }
    let Some((_, p, n)) = best else {
        return ray_plane_y(ray, 0.0).map(|q| (q, 0.0));
    };
    let q = ray_plane_y(ray, p.y).filter(|q| q.distance(p) < 0.5).unwrap_or(p);
    let start = if n.y > 0.5 {
        p.y + 0.05 // a deck or table top
    } else if n.y < -0.5 {
        p.y - 0.01 // underside of a shelf seen from below: the level under it
    } else {
        p.y + 0.15 // a beam or post: the deck is at most a beam's height above
    };
    Some((q, start))
}

/// Height of the highest surface under the footprint of box `mn..mx` placed at `t`
/// (x/z and yaw), searching down from `start`. Several rays across the footprint, so
/// a box resting on wires or a shelf edge is supported; 0 (the floor) if none.
fn support_y(
    parts: &Parts,
    is_object: &dyn Fn(Entity) -> bool,
    (mn, mx): (Vec3, Vec3),
    t: &Transform,
    start: f32,
    exclude: Option<Entity>,
) -> f32 {
    const N: usize = 5;
    let mut best = 0.0f32;
    for i in 0..N {
        for j in 0..N {
            let f = |k: usize| 0.05 + 0.9 * k as f32 / (N - 1) as f32;
            let local = Vec3::new(mn.x + (mx.x - mn.x) * f(i), mn.y, mn.z + (mx.z - mn.z) * f(j));
            let w = t.transform_point(local);
            let r = Ray3d::new(Vec3::new(w.x, start, w.z), Dir3::NEG_Y);
            if let Some((p, _)) = cast_objects(parts, is_object, r, exclude) {
                best = best.max(p.y);
            }
        }
    }
    best
}

/// Corners of a box after `t`, as a world-space AABB.
fn world_aabb(mn: Vec3, mx: Vec3, t: &Transform) -> (Vec3, Vec3) {
    (0..8).fold((Vec3::MAX, Vec3::MIN), |(lo, hi), k| {
        let c = Vec3::new(if k & 1 == 0 { mn.x } else { mx.x }, if k & 2 == 0 { mn.y } else { mx.y }, if k & 4 == 0 { mn.z } else { mx.z });
        let w = t.transform_point(c);
        (lo.min(w), hi.max(w))
    })
}

/// Whether a box `mn..mx` placed at `t` cuts into the mesh of another placed object
/// (not `exclude`). Resting on a shelf or standing side by side doesn't count: the
/// probes run slightly inside the box. Probes are segments along the box's vertical
/// edges and centre, and across it at three heights.
fn overlaps(
    parts: &Parts,
    objects: &Query<(Entity, &mut PlacedObject, &mut Transform), Without<Ghost>>,
    (mn, mx): (Vec3, Vec3),
    t: &Transform,
    exclude: Option<Entity>,
) -> bool {
    let (a0, a1) = world_aabb(mn, mx, t);
    let mut near = Vec::new();
    for (e, obj, ot) in objects.iter() {
        if Some(e) == exclude {
            continue;
        }
        let (b0, b1) = world_aabb(obj.aabb().0, obj.aabb().1, &ot);
        if a0.cmplt(b1).all() && b0.cmplt(a1).all() {
            // Something wholly inside the box would dodge every probe.
            if a0.cmple(b0).all() && b1.cmple(a1).all() {
                return true;
            }
            near.push(e);
        }
    }
    if near.is_empty() {
        return false;
    }
    let is_near = |e: Entity| near.contains(&e);
    let size = mx - mn;
    let inset = (size * 0.04).max(Vec3::splat(0.005)).min(size * 0.25);
    let (lo, hi) = (mn + inset, mx - inset);
    let at = |x: f32, y: f32, z: f32| t.transform_point(lo + (hi - lo) * Vec3::new(x, y, z));
    let mut segs = Vec::new();
    for (x, z) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)] {
        segs.push((at(x, 0.0, z), at(x, 1.0, z)));
    }
    let ring = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    for y in [0.0, 0.5, 1.0] {
        for k in 0..4 {
            let ((x0, z0), (x1, z1)) = (ring[k], ring[(k + 1) % 4]);
            segs.push((at(x0, y, z0), at(x1, y, z1)));
        }
        segs.push((at(0.0, y, 0.0), at(1.0, y, 1.0)));
        segs.push((at(1.0, y, 0.0), at(0.0, y, 1.0)));
    }
    segs.into_iter().any(|(a, b)| {
        let Ok(dir) = Dir3::new(b - a) else { return false };
        let len = a.distance(b);
        cast_objects(parts, &is_near, Ray3d::new(a, dir), None).is_some_and(|(p, _)| a.distance(p) < len)
    })
}

pub(crate) fn closest_param_on_line(ray: Ray3d, origin: Vec3, dir: Vec3) -> Option<f32> {
    let r = *ray.direction;
    let w = origin - ray.origin;
    let b = dir.dot(r);
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-4 {
        return None;
    }
    let d = dir.dot(w);
    let e = r.dot(w);
    Some((b * e - d) / denom)
}

pub(crate) fn snap(v: f32, step: f32) -> f32 {
    if step > 0.0 { (v / step).round() * step } else { v }
}

/// World-space pivot, unit axis and stage-to-world scale of a joint.
pub fn joint_frame(obj: &PlacedObject, t: &Transform, index: usize) -> Option<(Vec3, Vec3, f32)> {
    let j = obj.proto.joints.get(index)?;
    let deltas = obj.proto.body_deltas(&obj.joints);
    let parent = j.parent.and_then(|p| deltas.get(p).copied()).unwrap_or(Mat4::IDENTITY);
    let m = t.to_matrix() * obj.proto.basis.to_matrix() * parent;
    let axis = m.transform_vector3(j.axis);
    let s = axis.length().max(1e-9);
    Some((m.transform_point3(j.anchor), axis / s, s))
}

pub fn joint_label(obj: &PlacedObject, i: usize) -> String {
    let j = &obj.proto.joints[i];
    let v = obj.joints[i];
    match j.kind {
        JointKind::Revolute => format!("{} · {v:.0}°", j.name),
        _ => format!("{} · {:.3} m", j.name, v * obj.proto.unit),
    }
}

/// The rotate ring sits around the object's footprint, just above its base.
fn ring_geometry(obj: &PlacedObject, t: &Transform) -> (Vec3, f32) {
    let (mn, mx) = obj.aabb();
    let size = (mx - mn) * t.scale;
    let r = size.xz().length() * 0.5 + (size.length() * 0.15).clamp(0.01, 0.08);
    (t.translation + Vec3::Y * 0.01, r)
}

/// Length of the facing tick on the rotate ring.
fn ring_tick(obj: &PlacedObject, t: &Transform) -> f32 {
    let (mn, mx) = obj.aabb();
    (((mx - mn) * t.scale).length() * 0.2).clamp(0.02, 0.12)
}

fn lift_geometry(obj: &PlacedObject, t: &Transform) -> (Vec3, Vec3) {
    let (mn, mx) = obj.aabb();
    let top = t.transform_point(Vec3::new((mn.x + mx.x) * 0.5, mx.y, (mn.z + mx.z) * 0.5));
    let size = ((mx - mn) * t.scale).length();
    let gap = (size * 0.1).clamp(0.005, 0.05);
    let len = (size * 0.5).clamp(0.04, 0.5);
    (top + Vec3::Y * gap, top + Vec3::Y * (gap + len))
}

pub(crate) fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

// ---------------------------------------------------------------------------
// Pointer interaction: place, hover, select, move, handles, joints

#[allow(clippy::too_many_arguments)]
pub fn interact(
    mut commands: Commands,
    (mouse, keys, scroll): (Res<ButtonInput<MouseButton>>, Res<ButtonInput<KeyCode>>, Res<AccumulatedMouseScroll>),
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    parts: Parts,
    mut ghost_tf: Query<(&mut Transform, &mut Visibility), (With<Ghost>, Without<PlacedObject>)>,
    mut objects: Query<(Entity, &mut PlacedObject, &mut Transform), Without<Ghost>>,
    mut selection: ResMut<Selection>,
    mut place: ResMut<PlaceMode>,
    mut drag: ResMut<DragState>,
    mut handles: ResMut<HandleUi>,
    mut counter: ResMut<NameCounter>,
    cache: Res<PrototypeCache>,
    block: Res<UiBlock>,
    settings: Res<ViewSettings>,
    visibility: Query<&Visibility, With<PlacedObject>>,
) {
    let (cam, cam_gt) = *camera;
    let cursor = window.cursor_position();
    let ray = cursor.and_then(|c| cam.viewport_to_world(cam_gt, c).ok());
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let to_screen = |p: Vec3| cam.world_to_viewport(cam_gt, p).ok();

    // Rotation keys apply to the ghost while placing, otherwise to the selection.
    if !block.keyboard {
        let step = if shift { FRAC_PI_2 } else { PI / 12.0 };
        let mut d = 0.0;
        if keys.just_pressed(KeyCode::KeyQ) { d += step; }
        if keys.just_pressed(KeyCode::KeyE) { d -= step; }
        if d != 0.0 {
            if place.ghost.is_some() {
                place.yaw += d;
            } else {
                for sel in selection.all() {
                    if let Ok((_, o, mut t)) = objects.get_mut(sel)
                        && !o.locked
                    {
                        t.rotate_y(d);
                    }
                }
            }
        }
    }

    // ---- Placement mode
    if let Some((ghost, asset)) = place.ghost.clone() {
        handles.joints.clear();
        handles.hovered = None;
        selection.hovered = None;
        let Ok((mut gt, mut vis)) = ghost_tf.get_mut(ghost) else { return };
        // Drop onto the real surface under the cursor, so any shelf level works.
        let proto = cache.get(&asset);
        let is_object = |e: Entity| objects.contains(e);
        let hit = ray.filter(|_| !block.pointer).and_then(|ray| aim(&parts, &is_object, ray, None));
        match hit {
            Some((p, start)) => {
                let p = if ctrl { Vec3::new(snap(p.x, settings.snap), p.y, snap(p.z, settings.snap)) } else { p };
                gt.translation = p;
                gt.rotation = Quat::from_rotation_y(place.yaw);
                if let Some(proto) = &proto {
                    gt.translation.y = support_y(&parts, &is_object, (proto.aabb_min, proto.aabb_max), &gt, start, None);
                }
                *vis = Visibility::Visible;
            }
            None => *vis = Visibility::Hidden,
        }
        place.blocked = settings.no_overlap
            && hit.is_some()
            && proto.as_ref().is_some_and(|p| overlaps(&parts, &objects, (p.aabb_min, p.aabb_max), &gt, None));
        if !block.pointer && mouse.just_pressed(MouseButton::Left) && hit.is_some() && !place.blocked {
            if let Some(proto) = proto {
                let e = spawn_object(&mut commands, &mut counter, &proto, &asset, *gt, None, None);
                selection.set(Some(e));
            }
            if !(shift || place.sticky) {
                place.asset = None;
            }
        }
        if (!block.pointer && mouse.just_pressed(MouseButton::Right) && !mouse.pressed(MouseButton::Left))
            || (!block.keyboard && keys.just_pressed(KeyCode::Escape))
        {
            place.asset = None;
        }
        return;
    }

    // ---- Active drags
    if let Some(active) = &mut drag.active {
        if !mouse.pressed(MouseButton::Left) {
            // Gravity: a moved object falls onto what's under it; Alt-moved or lifted
            // objects stay where they were put. Locked members of the group didn't move.
            let unlocked = |g: &Entity| objects.get(*g).is_ok_and(|(_, o, _)| !o.locked);
            match drag.active.take() {
                Some(Drag::Move { entity, moved: true, .. }) if alt => {
                    commands.entity(entity).try_insert(Floating);
                    for g in selection.all().into_iter().filter(|g| *g != entity).filter(unlocked) {
                        commands.entity(g).try_insert(Floating);
                    }
                }
                Some(Drag::Move { entity, moved: true, .. }) => {
                    commands.entity(entity).try_remove::<Floating>();
                    drag.released = Some(entity);
                    // The rest of the group settles onto whatever is under it too.
                    for g in selection.all().into_iter().filter(|g| *g != entity).filter(unlocked) {
                        commands.entity(g).try_remove::<Floating>().try_insert(Falling(0.0));
                    }
                }
                Some(Drag::Handle { entity, handle: Handle::Lift, .. }) => {
                    commands.entity(entity).try_insert(Floating);
                }
                _ => {}
            }
            handles.active = None;
            return;
        }
        let Some(ray) = ray else { return };
        match active {
            Drag::Move { entity, offset, moved, press_pos } => {
                if !*moved && cursor.is_some_and(|c| c.distance(*press_pos) > 3.0) {
                    *moved = true;
                }
                let me = *entity;
                // Follow the surface under the cursor (any shelf level); Alt keeps
                // the current height.
                let is_object = |e: Entity| objects.contains(e);
                let aimed = if *moved { aim(&parts, &is_object, ray, Some(me)) } else { None };
                if let Some((q, start)) = aimed {
                    let mut target = q + *offset;
                    if ctrl {
                        target.x = snap(target.x, settings.snap);
                        target.z = snap(target.z, settings.snap);
                    }
                    let Ok((_, obj, t)) = objects.get(me) else { return };
                    let (bounds, current) = (obj.aabb(), *t);
                    target.y = if alt {
                        current.translation.y
                    } else {
                        support_y(&parts, &is_object, bounds, &current.with_translation(target), start, Some(me))
                    };
                    // Don't cut into other objects: take the target, else slide along one
                    // axis (so it glides along a wall or a neighbour), else stay put.
                    // A group moves as one: its members may sit right next to each other.
                    let target = if settings.no_overlap && selection.group.is_empty() {
                        let old = current.translation;
                        [target, Vec3::new(target.x, target.y, old.z), Vec3::new(old.x, target.y, target.z)]
                            .into_iter()
                            .find(|p| !overlaps(&parts, &objects, bounds, &current.with_translation(*p), Some(me)))
                    } else {
                        Some(target)
                    };
                    if let Some(target) = target
                        && let Ok((_, _, mut t)) = objects.get_mut(me)
                    {
                        let delta = target - t.translation;
                        t.translation = target;
                        // The rest of the selection keeps its place relative to this one.
                        if delta != Vec3::ZERO && selection.contains(me) {
                            for g in selection.all().into_iter().filter(|g| *g != me) {
                                if let Ok((_, go, mut gt)) = objects.get_mut(g)
                                    && !go.locked
                                {
                                    gt.translation += delta;
                                }
                            }
                        }
                    }
                }
            }
            Drag::Handle { entity, handle, start, ref_vec, ref_param, press_pos } => {
                let Ok((_, mut obj, mut t)) = objects.get_mut(*entity) else {
                    drag.active = None;
                    return;
                };
                match *handle {
                    Handle::Rotate => {
                        let (center, _) = ring_geometry(&obj, &t);
                        if let Some(p) = ray_plane_y(ray, center.y) {
                            let v = (p - center).with_y(0.0);
                            let ang = ref_vec.cross(v).y.atan2(ref_vec.dot(v));
                            let mut yaw = *start + ang;
                            if ctrl {
                                yaw = snap(yaw, PI / 12.0);
                            }
                            let (_, x, z) = t.rotation.to_euler(EulerRot::YXZ);
                            t.rotation = Quat::from_euler(EulerRot::YXZ, yaw, x, z);
                        }
                    }
                    Handle::Lift => {
                        if let Some(s) = closest_param_on_line(ray, Vec3::new(t.translation.x, 0.0, t.translation.z), Vec3::Y) {
                            let mut y = *start + (s - *ref_param);
                            if ctrl {
                                y = snap(y, settings.snap * 0.2);
                            }
                            t.translation.y = y.max(-10.0);
                        }
                    }
                    Handle::Joint(i) => {
                        let j = obj.proto.joints[i].clone();
                        let (lo, hi) = j.range(obj.proto.unit);
                        // Slider-like: the value follows the cursor along the screen
                        // direction it grows in (`ref_vec`), `ref_param` per pixel.
                        // Steady from any view, unlike an angle around the pivot,
                        // which jumps when grabbed on the pivot or seen edge-on.
                        let moved = cursor.unwrap_or(*press_pos) - *press_pos;
                        let value = *start + moved.dot(ref_vec.truncate()) * *ref_param;
                        let value = if j.lower.is_none() && j.upper.is_none() && j.kind == JointKind::Revolute {
                            value
                        } else {
                            value.clamp(lo, hi)
                        };
                        let value = if ctrl && j.kind == JointKind::Revolute { snap(value, 15.0) } else { value };
                        if (obj.joints[i] - value).abs() > 1e-6 {
                            obj.joints[i] = value;
                        }
                    }
                }
            }
        }
        refresh_joint_markers(&mut handles, selection.selected, &objects, &to_screen);
        return;
    }

    refresh_joint_markers(&mut handles, selection.selected, &objects, &to_screen);
    if block.pointer {
        selection.hovered = None;
        handles.hovered = None;
        return;
    }
    let (Some(ray), Some(cursor)) = (ray, cursor) else { return };

    // ---- Handle hover (selected object only)
    handles.hovered = None;
    if let Some(sel) = selection.selected
        && let Ok((_, obj, t)) = objects.get(sel)
    {
        let near = handles
            .joints
            .iter()
            .map(|m| (m.screen.distance(cursor), m.index))
            .filter(|(d, _)| *d < 16.0)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, i)) = near {
            handles.hovered = Some(Handle::Joint(i));
        } else if handles.compact.is_none() && !obj.locked {
            let (a, b) = lift_geometry(&obj, &t);
            if let (Some(sa), Some(sb)) = (to_screen(a), to_screen(b))
                && dist_to_segment(cursor, sa, sb) < 9.0
            {
                handles.hovered = Some(Handle::Lift);
            } else {
                let (c, r) = ring_geometry(&obj, &t);
                let pts: Vec<Vec2> = (0..=64)
                    .filter_map(|k| to_screen(c + Quat::from_rotation_y(k as f32 / 64.0 * TAU) * Vec3::X * r))
                    .collect();
                if pts.windows(2).any(|w| dist_to_segment(cursor, w[0], w[1]) < 8.0) {
                    handles.hovered = Some(Handle::Rotate);
                }
            }
        }
    }

    // ---- Wheel over a joint dot turns / slides it (the camera doesn't zoom then).
    if let (Some(Handle::Joint(i)), Some(sel)) = (handles.hovered, selection.selected)
        && let Ok((_, mut obj, _)) = objects.get_mut(sel)
    {
        let notches = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        if notches != 0.0 {
            let j = obj.proto.joints[i].clone();
            let (lo, hi) = j.range(obj.proto.unit);
            // 5° (Shift 1°) per notch; a slide moves 1/20 (Shift 1/100) of its travel.
            let step = match j.kind {
                JointKind::Revolute => if shift { 1.0 } else { 5.0 },
                _ => (hi - lo) / if shift { 100.0 } else { 20.0 },
            };
            let v = obj.joints[i] + notches * step;
            obj.joints[i] = if j.kind == JointKind::Revolute && j.lower.is_none() && j.upper.is_none() {
                (v + 180.0).rem_euclid(360.0) - 180.0
            } else {
                v.clamp(lo, hi)
            };
        }
    }

    // ---- Object hover via bounding boxes (cheap)
    let mut hovered: Option<(f32, Entity)> = None;
    for (e, obj, t) in objects.iter() {
        if visibility.get(e).is_ok_and(|v| *v == Visibility::Hidden) {
            continue;
        }
        let (mn, mx) = obj.aabb();
        if let Some(d) = ray_obb(ray, &t, mn, mx)
            && hovered.is_none_or(|(bd, _)| d < bd)
        {
            hovered = Some((d, e));
        }
    }
    selection.hovered = if handles.hovered.is_some() { selection.selected } else { hovered.map(|(_, e)| e) };

    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }

    // ---- Press on a handle
    if let (Some(h), Some(sel)) = (handles.hovered, selection.selected)
        && let Ok((_, obj, t)) = objects.get(sel)
    {
        let (start, ref_vec, ref_param) = match h {
            Handle::Rotate => {
                let (c, _) = ring_geometry(&obj, &t);
                let v = ray_plane_y(ray, c.y).map(|p| (p - c).with_y(0.0)).unwrap_or(Vec3::X);
                (t.rotation.to_euler(EulerRot::YXZ).0, v, 0.0)
            }
            Handle::Lift => {
                let s = closest_param_on_line(ray, Vec3::new(t.translation.x, 0.0, t.translation.z), Vec3::Y).unwrap_or(0.0);
                (t.translation.y, Vec3::ZERO, s)
            }
            Handle::Joint(i) => {
                let Some((anchor, axis, scale)) = joint_frame(&obj, &t, i) else { return };
                let cam_pos = cam_gt.translation();
                let size = (cam_pos - anchor).length() * 0.05;
                // Screen pixels moved by a world displacement `d` at `p`.
                let px = |p: Vec3, d: Vec3| match (to_screen(p), to_screen(p + d)) {
                    (Some(a), Some(b)) => b - a,
                    _ => Vec2::ZERO,
                };
                let (dir, per_px) = match obj.proto.joints[i].kind {
                    JointKind::Revolute => {
                        // Where a point on the hinge circle facing the camera (or above
                        // the pivot, looking down the axis) moves as the angle grows.
                        let flat = |v: Vec3| v - axis * v.dot(axis);
                        let view = cam_pos - anchor;
                        let lever = Some(flat(view)).filter(|v| v.length() > 0.2 * view.length()).unwrap_or_else(|| flat(*cam_gt.up())).normalize_or_zero();
                        let v = px(anchor + lever * size, axis.cross(lever) * size);
                        let dir = if v.length() > 0.5 { v.normalize() } else { Vec2::X };
                        (dir, 0.5)
                    }
                    _ => {
                        // Along the slide axis as drawn; straight into the screen: drag up.
                        let v = px(anchor, axis * size);
                        if v.length() > 2.0 { (v.normalize(), size / scale / v.length()) } else { (Vec2::NEG_Y, size * 0.04 / scale) }
                    }
                };
                (obj.joints[i], dir.extend(0.0), per_px)
            }
        };
        drag.active = Some(Drag::Handle { entity: sel, handle: h, start, ref_vec, ref_param, press_pos: cursor });
        handles.active = Some(h);
        return;
    }

    // ---- Press on an object: precise pick, then start moving it
    let is_object = |e: Entity| objects.contains(e);
    let picked = cast_parts(&parts, &is_object, ray, None).map(|(e, p, _)| (e, p));
    let picked = picked.or_else(|| hovered.map(|(d, e)| (e, ray.origin + ray.direction * d)));
    match picked {
        // Ctrl/Shift+click adds to or removes from the selection.
        Some((e, _)) if ctrl || shift => selection.toggle(e),
        Some((e, p)) => {
            // Pressing on a member of the selection keeps it (to move them all).
            if selection.contains(e) {
                if selection.selected != Some(e) {
                    if let Some(p) = selection.selected {
                        selection.group.push(p);
                    }
                    selection.group.retain(|x| *x != e);
                    selection.selected = Some(e);
                }
            } else {
                selection.set(Some(e));
            }
            // A locked object can be selected but not picked up.
            if let Ok((_, o, t)) = objects.get(e)
                && !o.locked
            {
                // Measure the grab offset where the cursor meets the object's base, which
                // is where the surface it rests on is, so it doesn't jump on the first move.
                let base = ray_plane_y(ray, t.translation.y).unwrap_or(p);
                let offset = (t.translation - base).with_y(0.0);
                drag.active = Some(Drag::Move { entity: e, offset, moved: false, press_pos: cursor });
            }
        }
        None if ctrl || shift => {}
        None => selection.set(None),
    }
}

fn refresh_joint_markers(
    handles: &mut HandleUi,
    selected: Option<Entity>,
    objects: &Query<(Entity, &mut PlacedObject, &mut Transform), Without<Ghost>>,
    to_screen: &impl Fn(Vec3) -> Option<Vec2>,
) {
    handles.joints.clear();
    handles.compact = None;
    let Some(sel) = selected else { return };
    let Ok((_, obj, t)) = objects.get(sel) else { return };

    // Tiny on screen: the ring and arrow would hide the object, so only show joint dots.
    let (mn, mx) = obj.aabb();
    let corners: Vec<Vec2> = (0..8)
        .filter_map(|k| {
            let c = Vec3::new(if k & 1 == 0 { mn.x } else { mx.x }, if k & 2 == 0 { mn.y } else { mx.y }, if k & 4 == 0 { mn.z } else { mx.z });
            to_screen(t.transform_point(c))
        })
        .collect();
    if corners.len() == 8 {
        let (lo, hi) = corners.iter().fold((Vec2::MAX, Vec2::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        if (hi - lo).max_element() < COMPACT_PX {
            handles.compact = Some(Vec2::new((lo.x + hi.x) * 0.5, hi.y));
        }
    }

    // Dozens of identical joints (every roller of a conveyor) are driven
    // together, not one by one: a dot each only buries the object.
    if obj.proto.joints.len() > 16 {
        return;
    }
    for i in 0..obj.proto.joints.len() {
        if let Some((anchor, _, _)) = joint_frame(&obj, &t, i)
            && let Some(screen) = to_screen(anchor)
        {
            handles.joints.push(JointMarker { index: i, screen, anchor: screen, label: joint_label(&obj, i) });
        }
    }
    // Spread out dots that sit on top of each other so each one can be grabbed.
    let n = handles.joints.len();
    for _ in 0..4 {
        for a in 0..n {
            for b in a + 1..n {
                let d = handles.joints[b].screen - handles.joints[a].screen;
                let len = d.length();
                if len < MARKER_SPACING {
                    let dir = if len > 1e-3 { d / len } else { Vec2::from_angle(b as f32 * 2.4) };
                    let push = dir * (MARKER_SPACING - len) * 0.5;
                    handles.joints[a].screen -= push;
                    handles.joints[b].screen += push;
                }
            }
        }
    }
}

pub fn keyboard_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    block: Res<UiBlock>,
    selection: Res<Selection>,
    place: Res<PlaceMode>,
    mut actions: ResMut<Actions>,
    objects: Query<&PlacedObject>,
) {
    if block.keyboard || place.ghost.is_some() {
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if ctrl && keys.just_pressed(KeyCode::KeyA) {
        actions.0.push(Action::SelectAll);
        return;
    }
    if let Some(sel) = selection.selected {
        if keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace) {
            for e in selection.all() {
                actions.0.push(Action::Delete(e));
            }
        }
        if ctrl && keys.just_pressed(KeyCode::KeyD) {
            for e in selection.all() {
                actions.0.push(Action::Duplicate(e));
            }
        }
        if ctrl && keys.just_pressed(KeyCode::KeyL) {
            let lock = !objects.get(sel).is_ok_and(|o| o.locked);
            actions.0.push(Action::SetLocked(selection.all(), lock));
        }
        if keys.just_pressed(KeyCode::KeyF) {
            actions.0.push(Action::Focus(sel));
        }
        if keys.just_pressed(KeyCode::Escape) {
            actions.0.push(Action::Select(None));
        }
    } else if keys.just_pressed(KeyCode::KeyF) {
        actions.0.push(Action::FrameAll);
    }
}

// ---------------------------------------------------------------------------
// Actions

#[allow(clippy::too_many_arguments)]
pub fn apply_actions(
    mut commands: Commands,
    mut actions: ResMut<Actions>,
    mut selection: ResMut<Selection>,
    mut pending: ResMut<PendingSpawns>,
    mut status: ResMut<Status>,
    mut counter: ResMut<NameCounter>,
    mut objects: Query<(Entity, &mut PlacedObject, &Transform)>,
    mut orbit: Single<&mut OrbitCamera>,
    mut downloads: ResMut<PendingDownloads>,
    mut undo: ResMut<crate::undo::UndoStack>,
) {
    for action in std::mem::take(&mut actions.0) {
        match action {
            Action::Select(e) => selection.set(e),
            Action::SelectAll => {
                let mut all: Vec<Entity> = objects.iter().map(|(e, _, _)| e).collect();
                let first = selection.selected.filter(|e| all.contains(e)).or_else(|| all.first().copied());
                all.retain(|e| Some(*e) != first);
                selection.selected = first;
                selection.group = all;
                status.0 = format!("{} objects selected", selection.all().len());
            }
            Action::Delete(e) => {
                if let Ok((_, o, _)) = objects.get(e)
                    && o.locked
                {
                    status.0 = format!("{} is locked: unlock it to delete it", o.name);
                    continue;
                }
                if objects.contains(e) {
                    commands.entity(e).despawn();
                }
                selection.group.retain(|x| *x != e);
                if selection.selected == Some(e) {
                    selection.selected = (!selection.group.is_empty()).then(|| selection.group.remove(0));
                }
            }
            Action::Duplicate(e) => {
                if let Ok((_, obj, t)) = objects.get(e) {
                    let (mn, mx) = obj.aabb();
                    let mut t = *t;
                    t.translation += t.rotation * Vec3::new((mx.x - mn.x).max(0.2) * t.scale.x * 1.1, 0.0, 0.0);
                    pending.0.push(PendingSpawn {
                        asset: obj.asset.clone(),
                        transform: t,
                        name: None,
                        joints: Some(obj.joints.clone()),
                        select: true,
                        locked: false,
                    });
                }
            }
            Action::Focus(e) => {
                if let Ok((_, obj, t)) = objects.get(e) {
                    let (mn, mx) = obj.aabb();
                    focus_on(&mut orbit, t.transform_point((mn + mx) * 0.5), ((mx - mn) * t.scale).length());
                }
            }
            Action::Drop(e) => {
                if let Ok((_, o, _)) = objects.get(e)
                    && o.locked
                {
                    status.0 = format!("{} is locked: unlock it to drop it", o.name);
                    continue;
                }
                commands.entity(e).try_remove::<Floating>().try_insert(DropNow);
            }
            Action::SetLocked(list, locked) => {
                let mut names = Vec::new();
                for e in list {
                    if let Ok((_, mut o, _)) = objects.get_mut(e)
                        && o.locked != locked
                    {
                        o.locked = locked;
                        names.push(o.name.clone());
                    }
                }
                let verb = if locked { "Locked" } else { "Unlocked" };
                match names.as_slice() {
                    [] => {}
                    [one] => status.0 = format!("{verb} {one}"),
                    many => status.0 = format!("{verb} {} objects", many.len()),
                }
            }
            Action::FrameAll => {
                let mut lo = Vec3::splat(f32::MAX);
                let mut hi = Vec3::splat(f32::MIN);
                for (_, obj, t) in &objects {
                    let (mn, mx) = obj.aabb();
                    for c in [mn, mx] {
                        let p = t.transform_point(c);
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
                if lo.x <= hi.x {
                    focus_on(&mut orbit, (lo + hi) * 0.5, (hi - lo).length());
                } else {
                    focus_on(&mut orbit, Vec3::new(0.0, 0.5, 0.0), 3.0);
                }
            }
            Action::ClearScene => {
                downloads.0.clear();
                for (e, _, _) in &objects {
                    commands.entity(e).despawn();
                }
                selection.set(None);
                counter.0.clear();
            }
            Action::SaveLayout(path) => {
                let list: Vec<serde_json::Value> = objects
                    .iter()
                    .map(|(_, o, t)| {
                        let mut v = serde_json::json!({
                            "name": o.name,
                            "asset": o.asset.to_string_lossy(),
                            "translation": t.translation.to_array(),
                            "rotation": t.rotation.to_array(),
                            "scale": t.scale.to_array(),
                            "joints": o.joints,
                        });
                        if o.locked {
                            v["locked"] = true.into();
                        }
                        v
                    })
                    .collect();
                let doc = serde_json::json!({ "format": "sense_sim_layout", "version": 2, "objects": list });
                status.0 = match crate::project_file::write(&path, &doc) {
                    Ok(()) => {
                        undo.mark_saved();
                        format!("Saved {} objects to {}", list.len(), path.display())
                    }
                    Err(e) => format!("Save failed: {e}"),
                };
            }
            Action::LoadLayout(path) => match load_layout(&path) {
                Ok((spawns, missing)) => {
                    for (e, _, _) in &objects {
                        commands.entity(e).despawn();
                    }
                    selection.set(None);
                    // The bar's spinner says while they load; this stays once they have.
                    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    status.0 = format!("Opened {name}: {} objects", spawns.len() + missing.len());
                    pending.0.extend(spawns);
                    downloads.0 = missing;
                }
                Err(e) => status.0 = format!("Load failed: {e:#}"),
            },
            Action::ExportUsda(path) => {
                status.0 = match export_usda(&path, objects.iter().map(|(_, o, t)| (o, *t))) {
                    Ok(n) => format!("Exported {n} objects to {}", path.display()),
                    Err(e) => format!("Export failed: {e:#}"),
                };
            }
        }
    }
}

/// Objects to spawn, and objects (`"download": folder` instead of `"asset"`)
/// whose asset still has to be downloaded.
fn load_layout(path: &Path) -> anyhow::Result<(Vec<PendingSpawn>, Vec<PendingDownload>)> {
    let v = crate::project_file::read(path)?;
    if crate::project_file::is_antenna_doc(&v) {
        anyhow::bail!("{} is an antenna project: it opens in Antenna, with File, Open.", path.display());
    }
    let arr = |o: &serde_json::Value, k: &str| -> Vec<f32> {
        o[k].as_array().map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect()).unwrap_or_default()
    };
    let mut out = Vec::new();
    let mut missing = Vec::new();
    for o in v["objects"].as_array().into_iter().flatten() {
        let t = Some(arr(o, "translation")).filter(|a| a.len() == 3).unwrap_or(vec![0.0; 3]);
        let r = Some(arr(o, "rotation")).filter(|a| a.len() == 4).unwrap_or(vec![0.0, 0.0, 0.0, 1.0]);
        let s = Some(arr(o, "scale")).filter(|a| a.len() == 3).unwrap_or(vec![1.0; 3]);
        let transform = Transform {
            translation: Vec3::new(t[0], t[1], t[2]),
            rotation: Quat::from_xyzw(r[0], r[1], r[2], r[3]).normalize(),
            scale: Vec3::new(s[0], s[1], s[2]),
        };
        let name = o["name"].as_str().map(str::to_string);
        match (o["asset"].as_str(), o["download"].as_str()) {
            (Some(asset), _) => out.push(PendingSpawn { asset: PathBuf::from(asset), transform, name, joints: Some(arr(o, "joints")), select: false, locked: o["locked"].as_bool().unwrap_or(false) }),
            (None, Some(folder)) => missing.push(PendingDownload { folder: folder.to_string(), transform, name, requested: false }),
            _ => {}
        }
    }
    Ok((out, missing))
}

/// Writes a Y-up USDA stage that references each placed asset, with the full
/// placement * basis (Z-up -> Y-up, units, bottom-centering) baked into one
/// matrix so it opens identically in usdview / Omniverse / Isaac Sim.
fn export_usda<'a>(path: &Path, objects: impl Iterator<Item = (&'a PlacedObject, Transform)>) -> anyhow::Result<usize> {
    use std::fmt::Write;
    let mut s = String::from("#usda 1.0\n(\n    defaultPrim = \"World\"\n    metersPerUnit = 1\n    upAxis = \"Y\"\n)\n\ndef Xform \"World\"\n{\n");
    let mut n = 0;
    let mut used = std::collections::HashSet::new();
    for (obj, t) in objects {
        let m = t.to_matrix() * obj.proto.basis.to_matrix();
        let mut name: String = obj.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        if name.is_empty() || name.chars().next().unwrap().is_ascii_digit() {
            name.insert(0, '_');
        }
        while !used.insert(name.clone()) {
            name.push('_');
        }
        let asset = obj.asset.to_string_lossy().replace('\\', "/");
        let c = m.to_cols_array_2d();
        let row = |r: [f32; 4]| format!("({}, {}, {}, {})", r[0], r[1], r[2], r[3]);
        writeln!(s, "    def Xform \"{name}\" (\n        prepend references = @{asset}@\n    )\n    {{")?;
        writeln!(s, "        matrix4d xformOp:transform = ( {}, {}, {}, {} )", row(c[0]), row(c[1]), row(c[2]), row(c[3]))?;
        writeln!(s, "        uniform token[] xformOpOrder = [\"xformOp:transform\"]\n    }}\n")?;
        n += 1;
    }
    s.push_str("}\n");
    std::fs::write(path, s)?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// Visuals

#[allow(clippy::too_many_arguments)]
pub fn draw_gizmos(
    mut gizmos: Gizmos,
    mut hg: Gizmos<HandleGizmos>,
    settings: Res<ViewSettings>,
    selection: Res<Selection>,
    place: Res<PlaceMode>,
    handles: Res<HandleUi>,
    objects: Query<(&PlacedObject, &Transform)>,
    ghosts: Query<(&GlobalTransform, &Visibility), With<Ghost>>,
    cache: Res<PrototypeCache>,
) {
    if settings.grid {
        let iso = Isometry3d::from_rotation(Quat::from_rotation_x(FRAC_PI_2));
        gizmos.grid(iso, UVec2::splat(40), Vec2::splat(0.5), Color::srgba(0.0, 0.02, 0.06, 0.07)).outer_edges();
        gizmos.grid(iso, UVec2::splat(8), Vec2::splat(2.5), Color::srgba(0.0, 0.02, 0.06, 0.13));
        gizmos.line(Vec3::ZERO, Vec3::X * 0.5, Color::srgb(0.9, 0.2, 0.2));
        gizmos.line(Vec3::ZERO, Vec3::Z * 0.5, Color::srgb(0.2, 0.4, 0.95));
    }
    let boxed = |g: &mut Gizmos<HandleGizmos>, mn: Vec3, mx: Vec3, t: &Transform, color: Color| {
        let local = Transform::from_translation((mn + mx) * 0.5).with_scale((mx - mn).max(Vec3::splat(0.01)));
        g.cube(t.mul_transform(local), color);
    };
    let amber = Color::srgb(0.96, 0.52, 0.04);
    let hot = Color::srgb(0.12, 0.42, 0.94);
    // A locked object's box is grey: selected, but not there to be moved.
    let box_color = |obj: &PlacedObject| if obj.locked { Color::srgb(0.42, 0.45, 0.5) } else { amber };
    let active_or_hover = |h: Handle| handles.active == Some(h) || (handles.active.is_none() && handles.hovered == Some(h));

    if let Some(sel) = selection.selected
        && let Ok((obj, t)) = objects.get(sel)
    {
        let (mn, mx) = obj.aabb();
        boxed(&mut hg, mn, mx, t, box_color(obj));

        // Rotate ring with a facing tick, and the lift arrow (hidden while the object is
        // tiny on screen, where they would cover it, and while it is locked).
        if handles.compact.is_none() && !obj.locked {
            let (c, r) = ring_geometry(obj, t);
            let ring_col = if active_or_hover(Handle::Rotate) { hot } else { amber.with_alpha(0.85) };
            hg.circle(Isometry3d::new(c, Quat::from_rotation_x(FRAC_PI_2)), r, ring_col).resolution(64);
            let fwd = (t.rotation * Vec3::Z).with_y(0.0).normalize_or_zero();
            hg.arrow(c + fwd * r, c + fwd * (r + ring_tick(obj, t)), ring_col);

            let (a, b) = lift_geometry(obj, t);
            let lift_col = if active_or_hover(Handle::Lift) { hot } else { Color::srgb(0.14, 0.66, 0.3) };
            hg.arrow(a, b, lift_col);
        }

        // Joints: rotation planes / slide axes for the hovered or dragged one.
        for i in 0..obj.proto.joints.len() {
            let h = Handle::Joint(i);
            if !active_or_hover(h) {
                continue;
            }
            let Some((anchor, axis, scale)) = joint_frame(obj, t, i) else { continue };
            let j = &obj.proto.joints[i];
            let size = ((mx - mn) * t.scale).length() * 0.12;
            match j.kind {
                JointKind::Revolute => {
                    hg.circle(Isometry3d::new(anchor, Quat::from_rotation_arc(Vec3::Z, axis)), size, hot).resolution(48);
                    hg.line(anchor - axis * size * 1.3, anchor + axis * size * 1.3, hot);
                }
                _ => {
                    let (lo, hi) = j.range(obj.proto.unit);
                    let v = obj.joints[i];
                    hg.line(anchor + axis * (lo - v) * scale, anchor + axis * (hi - v) * scale, hot);
                    hg.sphere(Isometry3d::from_translation(anchor), size * 0.15, hot);
                }
            }
        }
    }
    // The rest of a multi-selection: boxes only.
    for e in selection.group.iter().filter(|e| Some(**e) != selection.selected) {
        if let Ok((obj, t)) = objects.get(*e) {
            let (mn, mx) = obj.aabb();
            boxed(&mut hg, mn, mx, t, box_color(obj).with_alpha(0.7));
        }
    }
    if settings.bounds
        && let Some(h) = selection.hovered
        && !selection.contains(h)
        && let Ok((obj, t)) = objects.get(h)
    {
        let (mn, mx) = obj.aabb();
        let local = Transform::from_translation((mn + mx) * 0.5).with_scale((mx - mn).max(Vec3::splat(0.01)));
        gizmos.cube(t.mul_transform(local), Color::srgba(0.16, 0.44, 0.94, 0.85));
    }
    if let Some((g, asset)) = &place.ghost
        && let Ok((gt, vis)) = ghosts.get(*g)
        && *vis != Visibility::Hidden
        && let Some(p) = cache.get(asset)
    {
        let color = if place.blocked { Color::srgb(0.9, 0.2, 0.2) } else { Color::srgb(0.12, 0.5, 0.95) };
        boxed(&mut hg, p.aabb_min, p.aabb_max, &gt.compute_transform(), color);
    }
}

pub fn sync_lights(settings: Res<ViewSettings>, mut sun: Query<&mut DirectionalLight, With<Sun>>) {
    if !settings.is_changed() {
        return;
    }
    for mut l in &mut sun {
        l.illuminance = settings.sun_lux;
        l.shadow_maps_enabled = settings.shadows;
    }
}

// ---------------------------------------------------------------------------
// Image-based lighting

/// Builds a small procedural "studio" cubemap (soft sky dome, darker floor,
/// two softboxes) so metals and glossy plastics have something to reflect.
pub fn studio_cubemap(size: u32) -> Image {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};

    let radiance = |d: Vec3| -> Vec3 {
        let up = d.y;
        let sky = Vec3::new(0.80, 0.86, 0.95).lerp(Vec3::new(1.0, 1.0, 1.0), up.max(0.0));
        // A pale floor, like the one in the view. Metals show what they reflect, so a dark
        // floor would turn the lower half of every shiny part dark gray.
        let ground = Vec3::new(0.88, 0.84, 0.80);
        let horizon = smoothstep(-0.05, 0.08, up);
        let mut c = ground.lerp(sky, horizon) * 0.9;
        // Softboxes: key light high front-right, rim light behind-left. Kept modest: blurred
        // by a surface's roughness they spread far, and wash the color out of glossy paint.
        for (dir, power) in [(Vec3::new(0.6, 0.7, 0.4), 1.5), (Vec3::new(-0.7, 0.5, -0.5), 0.75)] {
            let k = d.dot(dir.normalize());
            c += Vec3::splat(smoothstep(0.93, 0.97, k) * power);
        }
        c
    };

    let mut data: Vec<u8> = Vec::with_capacity((size * size * 6 * 16) as usize);
    for face in 0..6 {
        for y in 0..size {
            for x in 0..size {
                let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                // wgpu cube face order and orientation: +X, -X, +Y, -Y, +Z, -Z.
                let d = match face {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                }
                .normalize();
                let c = radiance(d);
                for f in [c.x, c.y, c.z, 1.0f32] {
                    data.extend_from_slice(&f.to_le_bytes());
                }
            }
        }
    }
    let mut image = Image::new(
        Extent3d { width: size, height: size, depth_or_array_layers: 6 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Lighting reads `EnvironmentMapLight`, which Bevy fills in from the generated light
/// once, a frame or so after it appears. Its intensity has to be kept up here.
pub fn sync_environment(settings: Res<ViewSettings>, mut env: Query<&mut EnvironmentMapLight, With<GeneratedEnvironmentMapLight>>) {
    for mut light in &mut env {
        if light.intensity != settings.env_intensity {
            light.intensity = settings.env_intensity;
        }
    }
}

// ---------------------------------------------------------------------------
// Gravity

const GRAVITY: f32 = 9.81;

/// Drops objects onto whatever is under them: an object let go after a move, and
/// objects resting on (or above) something that moved or was deleted. Objects fall
/// straight down without tipping over. Lifted / Alt-moved objects (`Floating`) and the
/// object being held stay put.
#[allow(clippy::type_complexity)]
pub fn gravity(
    mut commands: Commands,
    time: Res<Time>,
    settings: Res<ViewSettings>,
    mut drag: ResMut<DragState>,
    parts: Parts,
    mut objects: Query<(Entity, &PlacedObject, &mut Transform, Has<Floating>, Option<&mut Falling>), Without<Ghost>>,
    mut removed: RemovedComponents<PlacedObject>,
    drop_now: Query<(), With<DropNow>>,
    parked: Query<(), With<crate::tabs::ParkedObject>>,
) {
    // Drop requests: fall now (gravity on) or land at once (gravity off).
    let drops: Vec<Entity> = objects.iter().filter(|(e, ..)| drop_now.contains(*e)).map(|(e, ..)| e).collect();
    if !drops.is_empty() {
        let set: std::collections::HashSet<Entity> = objects.iter().map(|(e, ..)| e).collect();
        let is_object = |e: Entity| set.contains(&e);
        for e in drops {
            commands.entity(e).remove::<DropNow>();
            if settings.gravity {
                commands.entity(e).insert(Falling(0.0));
            } else if let Ok((_, obj, mut t, _, _)) = objects.get_mut(e) {
                let support = support_y(&parts, &is_object, obj.aabb(), &t, t.translation.y + 0.01, Some(e));
                if support.is_finite() {
                    t.translation.y = support;
                }
            }
        }
    }
    let released = drag.released.take();
    // A tab being parked is not a deletion.
    let deleted = removed.read().filter(|e| !parked.contains(*e)).count() > 0;
    if !settings.gravity {
        for (e, _, _, _, falling) in &objects {
            if falling.is_some() {
                commands.entity(e).remove::<Falling>();
            }
        }
        return;
    }
    let held = drag.holding();

    // Wake objects that may have lost their support: anything whose footprint overlaps
    // an object that moved this frame and sits at or above that object's base.
    let mut moved: Vec<(Entity, Vec3, Vec3)> = Vec::new();
    let mut all: Vec<(Entity, Vec3, Vec3, bool)> = Vec::new();
    for (e, obj, t, floating, falling) in objects.iter_mut() {
        let (mn, mx) = obj.aabb();
        let (lo, hi) = world_aabb(mn, mx, &t);
        if t.is_changed() && !t.is_added() {
            moved.push((e, lo, hi));
        }
        // Locked mid-fall: it stays where it is now.
        if obj.locked && falling.is_some() {
            commands.entity(e).remove::<Falling>();
        }
        let can_fall = !floating && !obj.locked && falling.is_none() && Some(e) != held;
        all.push((e, lo, hi, can_fall));
    }
    for &(e, lo, hi, can_fall) in &all {
        if !can_fall {
            continue;
        }
        let wake = deleted
            || released == Some(e)
            || moved.iter().any(|&(m, mlo, mhi)| {
                m != e && lo.x < mhi.x && mlo.x < hi.x && lo.z < mhi.z && mlo.z < hi.z && lo.y >= mlo.y - 0.01
            });
        // Ceilings, roof beams and mounted fixtures hang: nothing under them holds them up.
        if wake && !objects.get(e).is_ok_and(|(_, o, ..)| crate::agent::assetgen::hangs(&o.asset)) {
            commands.entity(e).insert(Falling(0.0));
        }
    }

    // Move the falling ones down to their support.
    let set: std::collections::HashSet<Entity> = all.iter().map(|a| a.0).collect();
    let is_object = |e: Entity| set.contains(&e);
    let dt = time.delta_secs().min(1.0 / 30.0);
    let falling: Vec<(Entity, (Vec3, Vec3), Transform, f32)> = objects
        .iter()
        .filter(|(e, obj, ..)| Some(*e) != held && !obj.locked)
        .filter_map(|(e, obj, t, _, f)| f.map(|f| (e, obj.aabb(), *t, f.0)))
        .collect();
    for (e, bounds, t, v) in falling {
        let support = support_y(&parts, &is_object, bounds, &t, t.translation.y + 0.01, Some(e));
        let gap = t.translation.y - support;
        let Ok((_, _, mut tm, _, Some(mut f))) = objects.get_mut(e) else { continue };
        if gap <= 0.001 {
            commands.entity(e).remove::<Falling>();
            continue;
        }
        let v = v + GRAVITY * dt;
        let step = v * dt;
        if step >= gap {
            tm.translation.y = support;
            commands.entity(e).remove::<Falling>();
        } else {
            tm.translation.y -= step;
            f.0 = v;
        }
    }
}

#[cfg(test)]
pub(crate) mod lock_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    pub(crate) fn object(w: &mut World, name: &str, locked: bool) -> Entity {
        let proto = Arc::new(Prototype {
            parts: Vec::new(),
            joints: Vec::new(),
            links: Vec::new(),
            unit: 1.0,
            basis: Transform::IDENTITY,
            aabb_min: Vec3::ZERO,
            aabb_max: Vec3::ONE,
            triangles: 0,
            load_ms: 0.0,
            warnings: Vec::new(),
            detected: default(),
        });
        w.spawn((Transform::from_xyz(0.0, 3.0, 0.0), PlacedObject { asset: "a.usd".into(), name: name.into(), proto, joints: Vec::new(), locked })).id()
    }

    fn scene() -> World {
        let mut w = World::new();
        w.init_resource::<Actions>();
        w.init_resource::<Selection>();
        w.init_resource::<PendingSpawns>();
        w.init_resource::<PendingDownloads>();
        w.init_resource::<Status>();
        w.init_resource::<NameCounter>();
        w.init_resource::<crate::undo::UndoStack>();
        w.spawn(OrbitCamera { focus: Vec3::ZERO, yaw: 0.0, pitch: 0.0, radius: 1.0 });
        w
    }

    fn act(w: &mut World, actions: Vec<Action>) {
        w.resource_mut::<Actions>().0 = actions;
        w.run_system_once(apply_actions).unwrap();
        w.flush();
    }

    #[test]
    fn delete_and_drop_leave_a_locked_object_alone() {
        let mut w = scene();
        let (locked, free) = (object(&mut w, "kept", true), object(&mut w, "free", false));
        w.resource_mut::<Selection>().selected = Some(locked);
        act(&mut w, vec![Action::Delete(locked), Action::Delete(free), Action::Drop(locked)]);
        assert!(w.get_entity(locked).is_ok(), "a locked object must survive Delete");
        assert!(w.get_entity(free).is_err(), "an unlocked object is still deleted");
        assert!(w.get::<DropNow>(locked).is_none(), "a locked object must ignore Drop");
        assert_eq!(w.resource::<Selection>().selected, Some(locked), "a refused delete keeps the selection");
        assert!(w.resource::<Status>().0.contains("kept is locked"), "says why: {}", w.resource::<Status>().0);
    }

    #[test]
    fn set_locked_covers_the_list_and_can_be_reversed() {
        let mut w = scene();
        let (a, b) = (object(&mut w, "a", false), object(&mut w, "b", false));
        act(&mut w, vec![Action::SetLocked(vec![a, b], true)]);
        assert!(w.get::<PlacedObject>(a).unwrap().locked && w.get::<PlacedObject>(b).unwrap().locked);
        assert_eq!(w.resource::<Status>().0, "Locked 2 objects");
        act(&mut w, vec![Action::SetLocked(vec![a], false)]);
        assert!(!w.get::<PlacedObject>(a).unwrap().locked && w.get::<PlacedObject>(b).unwrap().locked);
        assert_eq!(w.resource::<Status>().0, "Unlocked a");
    }

    #[test]
    fn ctrl_l_follows_the_primary_object_and_covers_the_selection() {
        let mut w = scene();
        let (a, b) = (object(&mut w, "a", false), object(&mut w, "b", true));
        w.init_resource::<UiBlock>();
        w.init_resource::<PlaceMode>();
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);
        keys.press(KeyCode::KeyL);
        w.insert_resource(keys);
        {
            let mut selection = w.resource_mut::<Selection>();
            selection.selected = Some(a);
            selection.group = vec![b];
        }
        w.run_system_once(keyboard_shortcuts).unwrap();
        match w.resource::<Actions>().0.as_slice() {
            [Action::SetLocked(list, true)] => assert_eq!(list, &vec![a, b]),
            _ => panic!("Ctrl+L should ask to lock the selection"),
        }
    }

    #[test]
    fn a_duplicate_starts_unlocked() {
        let mut w = scene();
        let a = object(&mut w, "a", true);
        act(&mut w, vec![Action::Duplicate(a)]);
        assert!(!w.resource::<PendingSpawns>().0[0].locked);
    }

    #[test]
    fn the_lock_survives_save_and_load() {
        let mut w = scene();
        object(&mut w, "kept", true);
        object(&mut w, "free", false);
        let path = std::env::temp_dir().join(format!("sense_sim_lock_test_{}.json", std::process::id()));
        act(&mut w, vec![Action::SaveLayout(path.clone())]);
        let (spawns, _) = load_layout(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let locked = |n: &str| spawns.iter().find(|s| s.name.as_deref() == Some(n)).unwrap().locked;
        assert!(locked("kept"));
        assert!(!locked("free"));
    }

    /// The way the app really saves and opens one, ending in a file that isn't text.
    #[test]
    fn a_project_saves_and_opens_as_a_sesim_file() {
        let mut w = scene();
        object(&mut w, "Kệ hàng 3", true);
        let path = std::env::temp_dir().join(format!("sense_sim_project_test_{}.sesim", std::process::id()));
        act(&mut w, vec![Action::SaveLayout(path.clone())]);
        let bytes = std::fs::read(&path).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err(), "it isn't JSON on disk");
        let (spawns, _) = load_layout(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].name.as_deref(), Some("Kệ hàng 3"));
        assert!(spawns[0].locked);
    }

    #[test]
    fn a_layout_without_the_field_loads_unlocked() {
        let path = std::env::temp_dir().join(format!("sense_sim_lock_old_{}.json", std::process::id()));
        std::fs::write(&path, r#"{"format":"sense_sim_layout","version":2,"objects":[{"name":"old","asset":"a.usd"}]}"#).unwrap();
        let (spawns, _) = load_layout(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!spawns[0].locked);
    }

    #[test]
    fn gravity_leaves_a_locked_object_in_the_air() {
        let mut w = scene();
        w.init_resource::<Time>();
        w.init_resource::<ViewSettings>();
        w.init_resource::<DragState>();
        w.init_resource::<Messages<bevy::ecs::lifecycle::RemovedComponentEntity>>();
        let locked = object(&mut w, "kept", true);
        let free = object(&mut w, "free", false);
        for e in [locked, free] {
            w.entity_mut(e).insert(Falling(5.0));
        }
        w.resource_mut::<Time>().advance_by(std::time::Duration::from_millis(30));
        w.run_system_once(gravity).unwrap();
        w.flush();
        assert_eq!(w.get::<Transform>(locked).unwrap().translation.y, 3.0, "a locked object must not fall");
        assert!(w.get::<Falling>(locked).is_none(), "and must not stay marked as falling, which holds up undo");
        assert!(w.get::<Transform>(free).unwrap().translation.y < 3.0, "an unlocked one still falls");
    }
}


#[cfg(test)]
mod light_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn the_dome_setting_reaches_the_light_that_shading_reads() {
        let mut w = World::new();
        w.insert_resource(ViewSettings { env_intensity: 1234.0, ..default() });
        let light = w.spawn((GeneratedEnvironmentMapLight::default(), EnvironmentMapLight { intensity: 1.0, ..default() })).id();
        w.run_system_once(sync_environment).unwrap();
        assert_eq!(w.get::<EnvironmentMapLight>(light).unwrap().intensity, 1234.0);
    }
}