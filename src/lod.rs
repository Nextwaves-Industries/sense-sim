//! Lazy rendering: draw only what can be seen, at the detail the screen can show.
//!
//! Bevy already skips parts outside the view (frustum culling). On top of that:
//!
//! - **Sampling.** Each dense mesh part gets coarser versions, made by keeping one
//!   real vertex per grid cell. A coarser version is drawn only from where its
//!   error is below [`MAX_ERROR_PX`] pixels, so zooming out costs less without
//!   looking different.
//! - **Tiny parts.** A part smaller than [`MIN_PART_PX`] pixels isn't drawn.
//! - **Occlusion.** Parts hidden behind others can be skipped on the GPU.
//! - **Attention.** Selected and held objects always stay in full detail.
//! - **Frame rate.** If the scene is still too heavy, [`Governor`] allows a coarser
//!   picture until the frame rate recovers.
//!
//! All of this only changes what is drawn. Each part keeps its full mesh and BVH,
//! so picking, placement, RF and sensor ray casts always see the exact geometry.
//!
//! Versions are separate child entities switched by Bevy's `VisibilityRange`, so
//! the shadow passes follow the same choice. A range is measured from the part's
//! center, and every version of a part shares one `Aabb`, so they all switch at
//! exactly the same distance and no gap can open between them.

use std::any::TypeId;
use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::{VisibilityRange, VisibleEntities};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::occlusion_culling::OcclusionCulling;

use crate::gpu::Gpu;
use crate::scene::{DragState, OrbitCamera, Selection, ViewSettings};

/// A coarser version may be off by at most this many pixels when it is shown.
const MAX_ERROR_PX: f32 = 1.0;
/// Parts smaller than this many pixels across are not drawn.
const MIN_PART_PX: f32 = 1.0;
/// Parts with fewer triangles are cheap enough to always draw in full.
const MIN_TRIANGLES: usize = 300;
/// Grid cells per part diagonal, one per version, finest first.
const LEVELS: [f32; 4] = [192.0, 64.0, 20.0, 6.0];
/// A version must have at most this share of the previous one's triangles.
const MIN_REDUCTION: f32 = 0.6;
/// Range distances are rounded to this many steps per doubling, so that scenes
/// share a limited number of distinct ranges on the GPU.
const STEPS_PER_OCTAVE: f32 = 8.0;

/// A simplified mesh, before upload.
pub struct LodLevel {
    pub mesh: Mesh,
    /// How far any vertex may have moved, in mesh units.
    pub error: f32,
    pub triangles: u32,
}

/// A simplified version of a prototype part.
pub struct ProtoLod {
    pub mesh: Handle<Mesh>,
    pub error: f32,
    pub triangles: u32,
}

/// When a part's version (the full mesh or a [`ProtoLod`]) is drawn.
#[derive(Component, Clone, Copy)]
pub struct LodRange {
    /// Error of this version (0 for the full mesh), in mesh units.
    pub error: f32,
    /// Error of the next coarser version, or `None` if this is the coarsest.
    pub next_error: Option<f32>,
    /// Diagonal of the part's bounds, in mesh units.
    pub diagonal: f32,
    pub triangles: u32,
    /// The placed object this part belongs to.
    pub object: Entity,
}

/// Triangles in the scene and how many the camera draws now, for the status bar.
#[derive(Resource, Default, Clone, Copy)]
pub struct RenderStats {
    pub total: u64,
    pub drawn: u64,
}

// ---------------------------------------------------------------------------
// Sampling

/// Bounds of a mesh's vertices, if it has any.
pub fn mesh_bounds(mesh: &Mesh) -> Option<Aabb> {
    let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { return None };
    let (mn, mx) = positions_bounds(positions)?;
    Some(Aabb::from_min_max(mn, mx))
}

fn positions_bounds(positions: &[[f32; 3]]) -> Option<(Vec3, Vec3)> {
    let first = Vec3::from_array(*positions.first()?);
    Some(positions.iter().fold((first, first), |(lo, hi), p| {
        let p = Vec3::from_array(*p);
        (lo.min(p), hi.max(p))
    }))
}

/// Which of 81 patches of directions a normal falls in (a grid over the octahedral
/// map, about 30 degrees wide). Only vertices facing about the same way merge. This
/// keeps the two faces of a thin plate apart, and matters for shading: a big flat
/// face has few vertices, so one taking a bevel's normal would shade all of it wrong.
/// The grid is odd so that the common +X, +Y and +Z normals sit mid-patch, where
/// rounding noise can't scatter a flat face over several patches.
fn normal_bucket(n: [f32; 3]) -> u8 {
    const BINS: f32 = 9.0;
    let n = Vec3::from_array(n);
    let n = n / n.abs().element_sum().max(1e-6);
    let (u, v) = if n.z >= 0.0 {
        (n.x, n.y)
    } else {
        ((1.0 - n.y.abs()) * n.x.signum(), (1.0 - n.x.abs()) * n.y.signum())
    };
    let bin = |x: f32| (((x + 1.0) * 0.5 * BINS) as u8).min(BINS as u8 - 1);
    bin(u) * BINS as u8 + bin(v)
}

/// Keeps the vertex nearest each grid cell's center and rewires the triangles to
/// them, dropping the ones that collapse or repeat. Returns the kept vertices (as
/// indices into the input) and the new triangle list. Kept vertices are real ones,
/// so their normals and UVs stay valid and the result stays inside the bounds.
fn cluster(positions: &[[f32; 3]], normals: Option<&[[f32; 3]]>, indices: &[u32], origin: Vec3, cell: f32) -> (Vec<u32>, Vec<u32>) {
    let mut cells: HashMap<(i32, i32, i32, u8), (u32, f32)> = HashMap::new();
    let mut key_of = Vec::with_capacity(positions.len());
    for (i, p) in positions.iter().enumerate() {
        let g = (Vec3::from_array(*p) - origin) / cell;
        let bucket = normals.and_then(|n| n.get(i)).map_or(0, |n| normal_bucket(*n));
        let c = g.floor();
        let key = (c.x as i32, c.y as i32, c.z as i32, bucket);
        let d = (g - (c + Vec3::splat(0.5))).length_squared();
        let entry = cells.entry(key).or_insert((i as u32, d));
        if d < entry.1 {
            *entry = (i as u32, d);
        }
        key_of.push(key);
    }

    let mut kept = Vec::new();
    let mut new_index = vec![u32::MAX; positions.len()];
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in indices.chunks_exact(3) {
        let reps = [0, 1, 2].map(|k| cells[&key_of[t[k] as usize]].0);
        // Collapsed: fewer than three different points.
        let [a, b, c] = reps.map(|r| Vec3::from_array(positions[r as usize]));
        if a == b || b == c || a == c {
            continue;
        }
        // Same triangle again: start each at its lowest vertex, keeping the winding.
        let low = (0..3).min_by_key(|&k| reps[k]).unwrap_or(0);
        if !seen.insert([reps[low], reps[(low + 1) % 3], reps[(low + 2) % 3]]) {
            continue;
        }
        for r in reps {
            let slot = &mut new_index[r as usize];
            if *slot == u32::MAX {
                *slot = kept.len() as u32;
                kept.push(r);
            }
            out.push(*slot);
        }
    }
    (kept, out)
}

fn pick<T: Copy>(values: &[T], kept: &[u32]) -> Vec<T> {
    kept.iter().map(|&i| values[i as usize]).collect()
}

/// `mesh` reduced to the vertices `kept` and the triangles `indices`. Never read on
/// the CPU (ray casts use the full mesh), so it lives on the GPU only.
fn sampled_mesh(mesh: &Mesh, kept: &[u32], indices: Vec<u32>) -> Mesh {
    let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    for attribute in [Mesh::ATTRIBUTE_POSITION, Mesh::ATTRIBUTE_NORMAL, Mesh::ATTRIBUTE_UV_0, Mesh::ATTRIBUTE_COLOR, Mesh::ATTRIBUTE_TANGENT] {
        let values = match mesh.attribute(attribute) {
            Some(VertexAttributeValues::Float32x2(v)) => VertexAttributeValues::Float32x2(pick(v, kept)),
            Some(VertexAttributeValues::Float32x3(v)) => VertexAttributeValues::Float32x3(pick(v, kept)),
            Some(VertexAttributeValues::Float32x4(v)) => VertexAttributeValues::Float32x4(pick(v, kept)),
            _ => continue,
        };
        out.insert_attribute(attribute, values);
    }
    out.insert_indices(Indices::U32(indices));
    out
}

/// Builds the coarser versions of a mesh, finest first. Empty for meshes that are
/// already cheap or that sampling doesn't reduce.
pub fn build_levels(mesh: &Mesh) -> Vec<LodLevel> {
    let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { return Vec::new() };
    let Some(Indices::U32(indices)) = mesh.indices() else { return Vec::new() };
    if indices.len() / 3 < MIN_TRIANGLES {
        return Vec::new();
    }
    let Some((mn, mx)) = positions_bounds(positions) else { return Vec::new() };
    let diagonal = (mx - mn).length();
    if diagonal <= 0.0 {
        return Vec::new();
    }
    let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
        Some(VertexAttributeValues::Float32x3(n)) => Some(n.as_slice()),
        _ => None,
    };
    let mut levels = Vec::new();
    let mut previous = indices.len() / 3;
    for cells in LEVELS {
        let cell = diagonal / cells;
        let (kept, simplified) = cluster(positions, normals, indices, mn, cell);
        let triangles = simplified.len() / 3;
        if triangles == 0 {
            break;
        }
        if triangles as f32 > previous as f32 * MIN_REDUCTION {
            continue;
        }
        // A kept vertex is at most a cell's diagonal from any vertex it replaces.
        levels.push(LodLevel { mesh: sampled_mesh(mesh, &kept, simplified), error: cell * 3f32.sqrt(), triangles: triangles as u32 });
        previous = triangles;
    }
    levels
}

// ---------------------------------------------------------------------------
// Choosing what to draw

/// Rounds a distance to the shared steps (about 4% either way at most, small next
/// to the error budget).
fn quantize(distance: f32) -> f32 {
    if distance <= 0.0 {
        return 0.0;
    }
    ((distance.log2() * STEPS_PER_OCTAVE).round() / STEPS_PER_OCTAVE).exp2()
}

/// How much coarser than the lossless budget drawing may get, step by step.
const GOVERNOR_STEPS: [f32; 4] = [1.0, 1.5, 2.0, 3.0];
/// Below this frame rate, sustained, detail goes down.
const SLOW_FPS: f32 = 28.0;
/// At this frame rate, sustained, detail comes back.
const GOOD_FPS: f32 = 45.0;
/// Seconds of a slow frame rate before going coarser.
const SLOW_HOLD: f32 = 1.0;
/// Seconds of a good frame rate before going finer, at first.
const GOOD_HOLD: f32 = 4.0;
/// Longest wait before retrying finer, after finer keeps turning out too slow.
const GOOD_HOLD_MAX: f32 = 60.0;
/// Going coarser this soon after going finer means finer was too much.
const REGRET_WINDOW: f32 = 6.0;

/// Trades detail for frame rate when the scene is too heavy for this GPU, and takes
/// it back once there is room. Waits longer each time a finer step proves too slow,
/// so detail doesn't pump up and down.
#[derive(Resource)]
pub struct Governor {
    step: usize,
    slow_for: f32,
    good_for: f32,
    good_hold: f32,
    since_finer: f32,
}

impl Default for Governor {
    fn default() -> Self {
        Self { step: 0, slow_for: 0.0, good_for: 0.0, good_hold: GOOD_HOLD, since_finer: f32::MAX }
    }
}

impl Governor {
    /// The current allowance: 1 is lossless.
    pub fn budget(&self) -> f32 {
        GOVERNOR_STEPS[self.step]
    }

    /// One frame at `fps`. A frame cap lowers the targets, as frames held back by
    /// the cap aren't a sign of a heavy scene.
    fn tick(&mut self, fps: f32, cap: u32, dt: f32) {
        let (slow, good) = if cap > 0 { (SLOW_FPS.min(cap as f32 * 0.8), GOOD_FPS.min(cap as f32 * 0.95)) } else { (SLOW_FPS, GOOD_FPS) };
        self.since_finer += dt;
        self.slow_for = if fps < slow { self.slow_for + dt } else { 0.0 };
        self.good_for = if fps >= good { self.good_for + dt } else { 0.0 };
        if self.slow_for >= SLOW_HOLD && self.step + 1 < GOVERNOR_STEPS.len() {
            if self.since_finer < REGRET_WINDOW {
                self.good_hold = (self.good_hold * 2.0).min(GOOD_HOLD_MAX);
            }
            self.step += 1;
            self.slow_for = 0.0;
            self.good_for = 0.0;
        } else if self.good_for >= self.good_hold && self.step > 0 {
            self.step -= 1;
            self.good_for = 0.0;
            self.since_finer = 0.0;
        }
    }
}

/// Feeds the governor. Off when sampling is off, and in scripted runs so their
/// screenshots don't depend on how fast the machine happens to be.
pub fn update_governor(mut governor: ResMut<Governor>, settings: Res<ViewSettings>, gpu: Res<Gpu>, time: Res<Time>) {
    if !settings.sampling || gpu.headless() {
        if governor.step != 0 {
            *governor = Governor::default();
        }
        return;
    }
    let fps = gpu.fps();
    // No reading yet right after starting.
    if fps > 0.0 {
        governor.tick(fps, gpu.fps_cap(), time.delta_secs());
    }
}

/// Distance at which a length of `size` world units covers `px` pixels. The part's
/// `radius` is added since ranges measure from its center, and its near side can
/// be that much closer.
fn distance_for(size: f32, px: f32, pixels_per_tangent: f32, radius: f32) -> f32 {
    quantize(size * pixels_per_tangent / px + radius)
}

/// The range this version of a part is drawn in. `scale` converts mesh units to
/// world units; `pixels_per_tangent` is the size on screen of a length equal to its
/// distance from the camera.
fn range_for(lod: &LodRange, scale: f32, pixels_per_tangent: f32) -> VisibilityRange {
    let radius = lod.diagonal * scale * 0.5;
    let start = if lod.error > 0.0 { distance_for(lod.error * scale, MAX_ERROR_PX, pixels_per_tangent, radius) } else { 0.0 };
    let end = match lod.next_error {
        Some(next) => distance_for(next * scale, MAX_ERROR_PX, pixels_per_tangent, radius),
        None => distance_for(lod.diagonal * scale, MIN_PART_PX, pixels_per_tangent, radius),
    };
    VisibilityRange::abrupt(start, end)
}

/// With sampling off: the full mesh at any distance, the other versions never.
fn range_when_off(lod: &LodRange) -> VisibilityRange {
    if lod.error > 0.0 { VisibilityRange::abrupt(f32::MAX, f32::MAX) } else { VisibilityRange::abrupt(0.0, f32::MAX) }
}

/// Largest scale of a transform.
fn max_scale(gt: &GlobalTransform) -> f32 {
    let m = gt.affine().matrix3;
    m.x_axis.length().max(m.y_axis.length()).max(m.z_axis.length())
}

/// Keeps the ranges in step with the viewport, the setting, the governor, the
/// selection and each part's scale. Selected and held objects stay in full detail,
/// since those are the ones being looked at closely. Runs on every part only when
/// one of those changes; otherwise just on new or moved ones, and a move that
/// doesn't change the scale writes nothing.
#[allow(clippy::too_many_arguments)]
pub fn update_ranges(
    settings: Res<ViewSettings>,
    governor: Res<Governor>,
    selection: Res<Selection>,
    drag: Res<DragState>,
    camera: Single<(&Camera, &Projection), With<OrbitCamera>>,
    mut parts: Query<(Ref<LodRange>, Ref<GlobalTransform>, &mut VisibilityRange)>,
    mut last: Local<(f32, bool, Vec<Entity>)>,
) {
    let (cam, projection) = *camera;
    let Some(size) = cam.physical_viewport_size() else { return };
    let Projection::Perspective(p) = projection else { return };
    // Allowing `budget` times the error is the same as a screen `budget` times smaller.
    let pixels_per_tangent = size.y as f32 / (2.0 * (p.fov * 0.5).tan()) / governor.budget();
    let mut focus = selection.all();
    focus.extend(drag.holding());
    focus.sort_unstable();
    focus.dedup();
    let state = (pixels_per_tangent, settings.sampling, focus);
    let all = *last != state;
    if all {
        *last = state;
    }
    let focus = &last.2;
    for (lod, gt, mut range) in &mut parts {
        if !all && !lod.is_added() && !gt.is_changed() {
            continue;
        }
        let full = !settings.sampling || focus.binary_search(&lod.object).is_ok();
        let wanted = if full { range_when_off(&lod) } else { range_for(&lod, max_scale(&gt), pixels_per_tangent) };
        range.set_if_neq(wanted);
    }
}

/// Frames after a mesh entity appears at which it is registered again.
const RETRY_FRAMES: [u32; 3] = [4, 30, 120];

/// Works around a Bevy 0.20 bug that made random objects stay invisible: the renderer
/// registers a mesh entity once, and if the mesh isn't uploaded yet at that moment it
/// skips the entity without trying again (`specialize_material_meshes`). Marking the
/// entity changed makes it register again, so each new one is marked a few times while
/// its mesh arrives. The main world can't see when an upload is done, hence the timing.
pub fn retry_new_meshes(
    mut meshes: ParamSet<(Query<Entity, Added<Mesh3d>>, Query<&mut Mesh3d>)>,
    mut waiting: Local<Vec<(Entity, u32)>>,
    mut frame: Local<u32>,
) {
    *frame = frame.wrapping_add(1);
    let now = *frame;
    waiting.extend(meshes.p0().iter().map(|e| (e, now)));
    waiting.retain(|&(entity, born)| {
        let age = now.wrapping_sub(born);
        if RETRY_FRAMES.contains(&age)
            && let Ok(mut mesh) = meshes.p1().get_mut(entity)
        {
            mesh.set_changed();
        }
        age < RETRY_FRAMES[RETRY_FRAMES.len() - 1]
    });
}

/// Turns GPU occlusion culling on the camera on or off.
pub fn sync_occlusion(mut commands: Commands, settings: Res<ViewSettings>, camera: Single<(Entity, Has<OcclusionCulling>), With<OrbitCamera>>) {
    let (entity, on) = *camera;
    if settings.occlusion != on {
        if settings.occlusion {
            commands.entity(entity).insert((DepthPrepass, OcclusionCulling));
        } else {
            commands.entity(entity).remove::<(DepthPrepass, OcclusionCulling)>();
        }
    }
}

/// Counts triangles twice a second. Drawn is what passes the camera's CPU checks;
/// the GPU may skip more with occlusion culling.
pub fn update_stats(
    time: Res<Time>,
    mut timer: Local<f32>,
    mut stats: ResMut<RenderStats>,
    camera: Single<&VisibleEntities, With<OrbitCamera>>,
    versions: Query<(&LodRange, &InheritedVisibility)>,
) {
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 0.5;
    // Visible only: scenes in other tabs are hidden, not gone.
    stats.total = versions.iter().filter(|(v, vis)| vis.get() && v.error == 0.0).map(|(v, _)| u64::from(v.triangles)).sum();
    stats.drawn = camera.iter(TypeId::of::<Mesh3d>()).filter_map(|e| versions.get(*e).ok()).map(|(v, _)| u64::from(v.triangles)).sum();
}

/// `1234567` as `1.2M`.
pub fn short_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat `n` x `n` grid of quads over a unit square, facing +Z.
    fn grid(n: u32) -> Mesh {
        let side = n + 1;
        let positions: Vec<[f32; 3]> = (0..side * side).map(|i| [(i % side) as f32 / n as f32, (i / side) as f32 / n as f32, 0.0]).collect();
        let normals = vec![[0.0, 0.0, 1.0]; positions.len()];
        let uvs: Vec<[f32; 2]> = positions.iter().map(|p| [p[0], p[1]]).collect();
        let mut indices = Vec::new();
        for y in 0..n {
            for x in 0..n {
                let a = y * side + x;
                indices.extend([a, a + 1, a + side, a + 1, a + side + 1, a + side]);
            }
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_indices(Indices::U32(indices));
        mesh
    }

    fn positions(mesh: &Mesh) -> &[[f32; 3]] {
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(VertexAttributeValues::Float32x3(p)) => p,
            _ => panic!("no positions"),
        }
    }

    #[test]
    fn small_meshes_get_no_levels() {
        assert!(build_levels(&grid(4)).is_empty());
    }

    #[test]
    fn levels_get_coarser_and_coarser() {
        let levels = build_levels(&grid(300));
        assert!(levels.len() >= 2, "got {} levels", levels.len());
        let mut triangles = 300 * 300 * 2;
        let mut error = 0.0;
        for l in &levels {
            assert!((l.triangles as f32) <= triangles as f32 * MIN_REDUCTION, "{} from {triangles}", l.triangles);
            assert!(l.error > error);
            triangles = l.triangles;
            error = l.error;
        }
    }

    #[test]
    fn sampling_keeps_real_vertices_and_all_attributes() {
        let mesh = grid(300);
        let original: HashSet<[u32; 3]> = positions(&mesh).iter().map(|p| p.map(f32::to_bits)).collect();
        for l in build_levels(&mesh) {
            let p = positions(&l.mesh);
            assert!(p.iter().all(|v| original.contains(&v.map(f32::to_bits))), "a vertex moved off the mesh");
            assert_eq!(l.mesh.count_vertices(), p.len());
            assert!(l.mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some() && l.mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
            let Some(Indices::U32(i)) = l.mesh.indices() else { panic!("no indices") };
            assert_eq!(i.len() as u32 / 3, l.triangles);
            assert!(i.iter().all(|&v| (v as usize) < p.len()));
        }
    }

    #[test]
    fn sampling_covers_the_whole_surface() {
        // A coarse version of the square must still reach within its error of the corners.
        for l in build_levels(&grid(300)) {
            let p = positions(&l.mesh);
            for corner in [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(1.0, 1.0, 0.0)] {
                let nearest = p.iter().map(|v| Vec3::from_array(*v).distance(corner)).fold(f32::MAX, f32::min);
                assert!(nearest <= l.error, "corner {corner} is {nearest} away, error {}", l.error);
            }
        }
    }

    #[test]
    fn the_two_faces_of_a_thin_plate_stay_apart() {
        // Two coincident-looking layers with opposite normals, much thinner than a cell.
        let square = |z: f32| [[0.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]];
        let positions: Vec<[f32; 3]> = square(0.0).into_iter().chain(square(0.0001)).collect();
        let normals = [[0.0, 0.0, -1.0], [0.0, 0.0, -1.0], [0.0, 0.0, -1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]];
        let (kept, triangles) = cluster(&positions, Some(&normals), &[0, 1, 2, 3, 4, 5], Vec3::ZERO, 0.25);
        assert_eq!(kept.len(), 6);
        assert_eq!(triangles.len(), 6);
    }

    #[test]
    fn new_meshes_are_registered_again_while_they_upload() {
        #[derive(Resource, Default)]
        struct Seen(Vec<u32>);
        let mut app = App::new();
        app.init_resource::<Seen>().add_systems(Update, (retry_new_meshes, |q: Query<(), Changed<Mesh3d>>, mut seen: ResMut<Seen>, mut frame: Local<u32>| {
            *frame += 1;
            if !q.is_empty() {
                seen.0.push(*frame);
            }
        }).chain());
        app.world_mut().spawn(Mesh3d(Default::default()));
        for _ in 0..200 {
            app.update();
        }
        let first = 1;
        let expected: Vec<u32> = std::iter::once(first).chain(RETRY_FRAMES.iter().map(|r| first + r)).collect();
        assert_eq!(app.world().resource::<Seen>().0, expected);
    }

    fn lod(error: f32, next_error: Option<f32>) -> LodRange {
        LodRange { error, next_error, diagonal: 1.0, triangles: 1, object: Entity::PLACEHOLDER }
    }

    /// Runs the governor at a steady frame rate for a while.
    fn run(g: &mut Governor, fps: f32, cap: u32, seconds: f32) {
        for _ in 0..(seconds * 10.0) as u32 {
            g.tick(fps, cap, 0.1);
        }
    }

    #[test]
    fn governor_gives_up_detail_when_slow_and_takes_it_back() {
        let mut g = Governor::default();
        run(&mut g, 60.0, 0, 30.0);
        assert_eq!(g.budget(), 1.0, "a fast scene stays lossless");
        run(&mut g, 15.0, 0, 1.2);
        assert_eq!(g.budget(), 1.5);
        run(&mut g, 15.0, 0, 30.0);
        assert_eq!(g.budget(), *GOVERNOR_STEPS.last().unwrap(), "stops at the coarsest step");
        run(&mut g, 60.0, 0, 30.0);
        assert_eq!(g.budget(), 1.0, "recovers");
    }

    #[test]
    fn governor_ignores_a_short_hitch_and_the_middle_ground() {
        let mut g = Governor::default();
        run(&mut g, 10.0, 0, 0.5);
        run(&mut g, 60.0, 0, 1.0);
        run(&mut g, 10.0, 0, 0.5);
        assert_eq!(g.budget(), 1.0, "two short hitches");
        run(&mut g, 15.0, 0, 1.2);
        run(&mut g, 35.0, 0, 30.0);
        assert_eq!(g.budget(), 1.5, "neither slow nor good: holds");
    }

    #[test]
    fn governor_waits_longer_after_a_finer_step_proves_too_slow() {
        let mut g = Governor::default();
        run(&mut g, 15.0, 0, 1.2);
        run(&mut g, 60.0, 0, GOOD_HOLD + 0.2);
        assert_eq!(g.budget(), 1.0);
        // Back to full detail is too heavy.
        run(&mut g, 15.0, 0, 1.2);
        assert_eq!(g.budget(), 1.5);
        run(&mut g, 60.0, 0, GOOD_HOLD + 0.2);
        assert_eq!(g.budget(), 1.5, "the first wait isn't enough any more");
        run(&mut g, 60.0, 0, GOOD_HOLD);
        assert_eq!(g.budget(), 1.0);
    }

    #[test]
    fn governor_does_not_blame_the_scene_for_a_frame_cap() {
        let mut g = Governor::default();
        run(&mut g, 10.0, 10, 30.0);
        assert_eq!(g.budget(), 1.0, "10 fps is what was asked for");
        run(&mut g, 4.0, 10, 1.2);
        assert_eq!(g.budget(), 1.5, "well under the cap is slow");
        run(&mut g, 10.0, 10, 30.0);
        assert_eq!(g.budget(), 1.0);
    }

    #[test]
    fn neighbouring_versions_share_their_boundary() {
        let (full, coarse) = (lod(0.0, Some(0.02)), lod(0.02, None));
        let (a, b) = (range_for(&full, 1.0, 1300.0), range_for(&coarse, 1.0, 1300.0));
        assert_eq!(a.start_margin.start, 0.0);
        assert_eq!(a.end_margin.end, b.start_margin.start, "a gap or an overlap between versions");
        assert!(b.end_margin.end > b.start_margin.start);
        assert!(a.is_abrupt() && b.is_abrupt());
    }

    #[test]
    fn a_coarser_version_appears_only_once_its_error_is_under_the_budget() {
        // 2 cm of error at 1300 px per tangent: 26 px at 1 m, 1 px at 26 m.
        let start = range_for(&lod(0.02, None), 1.0, 1300.0).start_margin.start;
        let radius = 0.5;
        let exact = 0.02 * 1300.0 / MAX_ERROR_PX + radius;
        assert!((start - exact).abs() < exact * 0.05, "{start} vs {exact}");
    }

    #[test]
    fn tiny_part_is_dropped_only_below_a_pixel() {
        // A 1 m part at 1300 px per tangent is 1 px across from 1300 m.
        let end = range_for(&lod(0.0, None), 1.0, 1300.0).end_margin.end;
        assert!((end - 1300.5).abs() < 1300.5 * 0.05, "{end}");
    }

    #[test]
    fn world_scale_moves_the_ranges() {
        let small = range_for(&lod(0.0, None), 1.0, 1300.0).end_margin.end;
        let big = range_for(&lod(0.0, None), 10.0, 1300.0).end_margin.end;
        assert!((big / small - 10.0).abs() < 1.0, "{small} {big}");
    }

    #[test]
    fn off_means_the_full_mesh_everywhere() {
        let (full, coarse) = (range_when_off(&lod(0.0, Some(0.1))), range_when_off(&lod(0.1, None)));
        assert!(full.is_visible_at_all(0.0) && full.is_visible_at_all(1e6));
        assert!(!coarse.is_visible_at_all(0.0) && !coarse.is_visible_at_all(1e6));
    }

    #[test]
    fn distances_share_steps() {
        let d = quantize(10.0);
        assert!((d - 10.0).abs() < 10.0 * 0.05);
        assert_eq!(quantize(d), d);
        assert_eq!(quantize(0.0), 0.0);
    }

    #[test]
    fn counts_read_short() {
        assert_eq!(short_count(950), "950");
        assert_eq!(short_count(1_250), "1.2k");
        assert_eq!(short_count(3_400_000), "3.4M");
    }
}
