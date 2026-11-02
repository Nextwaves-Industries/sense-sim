//! Objects in the scene as bodies to simulate: a USD asset has no faces of its own, so its mesh is
//! welded and split along its creases, and faces are picked by clicking them in the 3D view.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use sim_core::geometry::classify::distance_to_triangle;
use sim_core::geometry::regions::{self, Regions};
use sim_core::study::FaceRef;

use super::SimState;
use crate::bvh::Bvh;
use crate::prototype::Prototype;
use crate::scene::{OrbitCamera, PlacedObject};

/// A scene object, read as a surface in the solvers' frame.
pub struct SceneBody {
    pub regions: Arc<Regions>,
    bvh: Arc<Bvh>,
    /// What it was read from: read again when this changes.
    stamp: (Transform, PathBuf),
}

#[derive(Default)]
pub struct SceneBodies {
    /// The objects in the scene, for choosing from.
    pub names: Vec<String>,
    pub bodies: HashMap<String, SceneBody>,
    /// Why a body the studies ask for couldn't be read.
    pub errors: HashMap<String, String>,
}

/// From the scene (Y up) to the solvers (Z up). The inverse of what `view` draws results with.
pub fn to_solver(p: Vec3) -> [f64; 3] {
    [f64::from(p.x), f64::from(-p.z), f64::from(p.y)]
}

/// An object's whole surface, in place, in the solvers' frame.
pub fn read_object(name: &str, proto: &Prototype, transform: &Transform) -> Result<Regions> {
    let object = transform.to_matrix() * proto.basis.to_matrix();
    let (mut points, mut triangles) = (Vec::new(), Vec::new());
    for part in &proto.parts {
        let m = object * part.transform.to_matrix();
        for tri in part.bvh.triangles() {
            let at = points.len() as u32;
            for p in tri {
                points.push(to_solver(m.transform_point3(*p)));
            }
            triangles.push([at, at + 1, at + 2]);
        }
    }
    regions::from_triangles(name, &points, &triangles, regions::CREASE_DEGREES)
}

/// Keeps the list of objects, and reads the ones studies are using.
pub fn sync(mut state: ResMut<SimState>, objects: Query<(&PlacedObject, &Transform)>) {
    if !state.panel_open && state.studies.is_empty() {
        return;
    }
    let mut names: Vec<String> = objects.iter().map(|(o, _)| o.name.clone()).collect();
    names.sort();
    if names != state.scene.names {
        state.scene.names = names;
    }
    let wanted: Vec<String> = state.studies.iter().filter_map(|s| s.scene_object().map(str::to_string)).collect();
    for name in wanted {
        let Some((object, transform)) = objects.iter().find(|(o, _)| o.name == name) else {
            state.scene.bodies.remove(&name);
            state.scene.errors.insert(name.clone(), format!("{name} is no longer in the scene."));
            continue;
        };
        let stamp = (*transform, object.asset.clone());
        if state.scene.bodies.get(&name).is_some_and(|b| b.stamp == stamp) {
            continue;
        }
        match read_object(&name, &object.proto, transform) {
            Ok(regions) => {
                let positions: Vec<[f32; 3]> = regions.partition.positions.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
                let indices: Vec<u32> = regions.partition.triangles.iter().flatten().copied().collect();
                let bvh = Arc::new(Bvh::new(&positions, &indices));
                state.scene.errors.remove(&name);
                state.scene.bodies.insert(name, SceneBody { regions: Arc::new(regions), bvh, stamp });
            }
            Err(e) => {
                state.scene.errors.insert(name, format!("{e:#}"));
            }
        }
    }
}

#[derive(Component)]
pub struct PickMarker;

/// What the highlight shows now.
#[derive(Default, PartialEq, Clone)]
pub struct Shown {
    picked: Vec<u32>,
    hovered: Option<u32>,
    body: String,
}

/// The face under a ray in the solvers' frame, and where on it.
fn face_at(body: &SceneBody, origin: [f64; 3], direction: [f64; 3]) -> Option<FaceRef> {
    let f = |v: [f64; 3]| Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32);
    let (t, _) = body.bvh.cast(f(origin), f(direction), 1e6)?;
    let hit: [f64; 3] = std::array::from_fn(|k| origin[k] + direction[k] * f64::from(t));
    let part = &body.regions.partition;
    let nearest = (0..part.triangles.len()).min_by(|a, b| {
        let d = |i: usize| {
            let [p, q, r] = part.triangle(i);
            distance_to_triangle(hit, p, q, r)
        };
        d(*a).total_cmp(&d(*b))
    })?;
    Some(FaceRef { face: part.tri_face[nearest], point: hit, second: false })
}

/// Where a test's pointer is, in the window. Moving the real one would take it from whoever is using
/// this desktop, and it can't reach a hidden window anyway.
#[derive(Resource)]
pub struct TestPointer(pub Vec2);

/// While a condition's faces are being picked on a scene object: clicks choose them, and what's chosen
/// and what's under the pointer are lit up.
#[allow(clippy::too_many_arguments)]
pub fn pick(
    mut state: ResMut<SimState>,
    mouse: Res<ButtonInput<MouseButton>>,
    test_pointer: Option<Res<TestPointer>>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    markers: Query<Entity, With<PickMarker>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut shown: Local<Shown>,
    mut material: Local<Option<Handle<StandardMaterial>>>,
) {
    let Some((study, condition)) = state.scene_pick else {
        if !markers.is_empty() {
            for e in &markers {
                commands.entity(e).despawn();
            }
            *shown = Shown::default();
        }
        return;
    };
    let Some(name) = state.studies.get(study).and_then(|s| s.scene_object()).map(str::to_string) else {
        state.scene_pick = None;
        return;
    };
    let Some(body) = state.scene.bodies.get(&name) else { return };

    let (cam, cam_gt) = *camera;
    let pointer = test_pointer.map(|p| p.0).or_else(|| window.cursor_position());
    let ray = pointer.filter(|_| !state.pointer_over_ui).and_then(|c| cam.viewport_to_world(cam_gt, c).ok());
    let under = ray.and_then(|r| face_at(body, to_solver(r.origin), to_solver(*r.direction)));

    let clicked = mouse.just_pressed(MouseButton::Left);
    let regions = body.regions.clone();
    if clicked
        && let Some(hit) = under.clone()
        && let Some(c) = state.studies[study].conditions.get_mut(condition)
    {
        // A second click takes a face back off.
        if let Some(at) = c.faces.iter().position(|f| f.face == hit.face) {
            c.faces.remove(at);
        } else {
            c.faces.push(hit);
        }
    }

    let picked: Vec<u32> = state.studies[study].conditions.get(condition).map_or_else(Vec::new, |c| c.faces.iter().map(|f| f.face).collect());
    let now = Shown { picked, hovered: under.map(|h| h.face), body: name };
    if now == *shown {
        return;
    }
    for e in &markers {
        commands.entity(e).despawn();
    }
    let part = &regions.partition;
    let (mut positions, mut colours): (Vec<[f32; 3]>, Vec<[f32; 4]>) = (Vec::new(), Vec::new());
    for (i, face) in part.tri_face.iter().enumerate() {
        let colour = if now.picked.contains(face) {
            [1.0, 0.45, 0.1, 0.75]
        } else if now.hovered == Some(*face) {
            [1.0, 0.8, 0.35, 0.45]
        } else {
            continue;
        };
        for p in part.triangle(i) {
            positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            colours.push(colour);
        }
    }
    if !positions.is_empty() {
        let count = positions.len();
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; count])
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
            .with_inserted_indices(Indices::U32((0..count as u32).collect()));
        let handle = material
            .get_or_insert_with(|| materials.add(StandardMaterial { base_color: Color::WHITE, unlit: true, cull_mode: None, double_sided: true, alpha_mode: AlphaMode::Blend, depth_bias: 200.0, ..default() }))
            .clone();
        commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(handle), Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)), PickMarker));
    }
    *shown = now;
}

/// A study's result, drawn on the object's own surface, would fight with the object: it steps aside
/// while the result is shown.
pub fn step_aside(state: Res<SimState>, mut objects: Query<(Entity, &PlacedObject, &mut Visibility)>, mut hidden: Local<Option<Entity>>) {
    let wanted = state.view.as_ref().and_then(|v| v.hides.as_deref());
    let target = wanted.and_then(|name| objects.iter().find(|(_, o, _)| o.name == name).map(|(e, _, _)| e));
    if target == *hidden {
        return;
    }
    if let Some(e) = *hidden
        && let Ok((_, _, mut v)) = objects.get_mut(e)
    {
        *v = Visibility::Inherited;
    }
    if let Some(e) = target
        && let Ok((_, _, mut v)) = objects.get_mut(e)
    {
        *v = Visibility::Hidden;
    }
    *hidden = target;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ray_finds_the_face_it_hits_and_where() {
        // A box from 0 to 1, as bare triangles.
        let c = |x: f64, y: f64, z: f64| [x, y, z];
        let quads = [
            [c(0., 0., 0.), c(0., 1., 0.), c(1., 1., 0.), c(1., 0., 0.)],
            [c(0., 0., 1.), c(1., 0., 1.), c(1., 1., 1.), c(0., 1., 1.)],
            [c(0., 0., 0.), c(1., 0., 0.), c(1., 0., 1.), c(0., 0., 1.)],
            [c(0., 1., 0.), c(0., 1., 1.), c(1., 1., 1.), c(1., 1., 0.)],
            [c(0., 0., 0.), c(0., 0., 1.), c(0., 1., 1.), c(0., 1., 0.)],
            [c(1., 0., 0.), c(1., 1., 0.), c(1., 1., 1.), c(1., 0., 1.)],
        ];
        let (mut p, mut t) = (Vec::new(), Vec::new());
        for q in &quads {
            for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                let at = p.len() as u32;
                p.extend_from_slice(&tri);
                t.push([at, at + 1, at + 2]);
            }
        }
        let regions = regions::from_triangles("box", &p, &t, 30.0).unwrap();
        let positions: Vec<[f32; 3]> = regions.partition.positions.iter().map(|v| [v[0] as f32, v[1] as f32, v[2] as f32]).collect();
        let indices: Vec<u32> = regions.partition.triangles.iter().flatten().copied().collect();
        let body = SceneBody { bvh: Arc::new(Bvh::new(&positions, &indices)), regions: Arc::new(regions), stamp: (Transform::default(), PathBuf::new()) };

        let hit = face_at(&body, [5.0, 0.3, 0.6], [-1.0, 0.0, 0.0]).expect("it hits the box");
        assert!((hit.point[0] - 1.0).abs() < 1e-5 && (hit.point[1] - 0.3).abs() < 1e-5 && (hit.point[2] - 0.6).abs() < 1e-5, "{:?}", hit.point);
        let part = &body.regions.partition;
        let on_x1 = (0..part.triangles.len()).find(|i| part.triangle(*i).iter().all(|p| (p[0] - 1.0).abs() < 1e-9)).unwrap();
        assert_eq!(hit.face, part.tri_face[on_x1], "the face at x = 1");

        let from_above = face_at(&body, [0.5, 0.5, 9.0], [0.0, 0.0, -1.0]).expect("it hits the top");
        assert_ne!(from_above.face, hit.face);
        assert!(face_at(&body, [5.0, 5.0, 5.0], [1.0, 0.0, 0.0]).is_none(), "a ray that misses");
    }

    #[test]
    fn the_solvers_frame_undoes_how_results_are_drawn() {
        // Results are drawn with this rotation, so a point must come back to itself through it.
        let root = Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));
        let solver = Vec3::new(1.0, 2.0, 3.0);
        let scene = root.transform_point(solver);
        let back = to_solver(scene);
        assert!((back[0] - 1.0).abs() < 1e-5 && (back[1] - 2.0).abs() < 1e-5 && (back[2] - 3.0).abs() < 1e-5, "{back:?}");
        // Up in the scene is up for the solvers.
        assert_eq!(to_solver(Vec3::Y), [0.0, 0.0, 1.0]);
    }
}
