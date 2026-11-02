//! Draws a result in the 3D view: the surface coloured by a field, optionally deformed, with mode
//! shapes animated. The field's value goes in the mesh's texture coordinate and samples a colour
//! ramp on an unlit material, which interpolates values correctly across triangles without any
//! custom shader.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::DVec3;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crossbeam_channel::{Receiver, TryRecvError, unbounded};
use sim_core::post;
use sim_core::results::{Field, Location, ResultSet, ResultStep, StepKind};

use crate::scene::OrbitCamera;

/// The result colour scale: blue through cyan, green and yellow to red. `t` in 0..=1.
pub fn colour(t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let stops: [[f32; 3]; 5] = [[0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 0.0]];
    let at = t * 4.0;
    let i = (at as usize).min(3);
    let f = at - i as f32;
    let (a, b) = (stops[i], stops[i + 1]);
    [(255.0 * (a[0] + (b[0] - a[0]) * f)) as u8, (255.0 * (a[1] + (b[1] - a[1]) * f)) as u8, (255.0 * (a[2] + (b[2] - a[2]) * f)) as u8]
}

/// What's being looked at.
pub struct View {
    pub job: u64,
    pub set: Arc<ResultSet>,
    pub step: usize,
    pub field: usize,
    /// How much displacement is exaggerated. Zero: undeformed.
    pub deform: f32,
    /// Animate between plus and minus the displacement, for mode shapes.
    pub animate: bool,
    /// The scale the result is first drawn with, so its deformation shows without being absurd.
    pub auto_scale: f32,
    /// With a cut to look at: show the walls too? They hide it in an internal flow.
    pub show_walls: bool,
    pub dirty: bool,
    pub framed: bool,
    pub entity: Option<Entity>,
    pub mesh: Option<Handle<Mesh>>,
    /// The field's range on this step: (min, max).
    pub range: (f32, f32),
    /// A scene object that steps aside while this is shown: the result is drawn where it stands.
    pub hides: Option<String>,
    /// What's asked for inside the volume.
    pub looks: Looks,
    /// This isn't a result but the check of a study's faces, and here's what each colour covers.
    pub legend: Option<Vec<sim_core::check::Coverage>>,
    /// What's on screen now, and how it was made.
    built: Option<Request>,
    /// Being worked out on a thread, and for what.
    working: Option<(Request, Receiver<Built>)>,
}

/// Ways to look inside a volume, drawn over the result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Looks {
    /// A cut across this axis, at this share of the volume's size.
    pub cut: Option<(usize, f32)>,
    /// A surface where the field has this share of its range.
    pub iso: Option<f32>,
    /// How many streamlines.
    pub lines: Option<usize>,
}

/// All that decides what's drawn inside the volume.
#[derive(Debug, Clone, PartialEq)]
struct Request {
    looks: Looks,
    step: usize,
    field: usize,
    deform: f32,
}

/// Geometry ready for a mesh.
#[derive(Default)]
struct Built {
    positions: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl Built {
    fn add_slice(&mut self, s: &post::Slice, colour: &[f32], range: (f32, f32), shift: Option<&Field>, amount: f32, nodes: usize) {
        let first = self.positions.len() as u32;
        let offsets: Vec<[f32; 3]> = match shift {
            Some(d) if d.len() == nodes && d.stride() >= 3 => s
                .weights
                .iter()
                .map(|(a, b, f)| {
                    let (u, v) = (d.at(*a as usize), d.at(*b as usize));
                    std::array::from_fn(|k| (u[k] + (v[k] - u[k]) * f) * amount)
                })
                .collect(),
            _ => vec![[0.0; 3]; s.points.len()],
        };
        let values = s.sample(colour);
        let span = (range.1 - range.0).max(f32::MIN_POSITIVE);
        for ((p, o), value) in s.points.iter().zip(&offsets).zip(&values) {
            self.positions.push([p[0] as f32 + o[0], p[1] as f32 + o[1], p[2] as f32 + o[2]]);
            self.uv.push([if value.is_finite() { (value - range.0) / span } else { 0.0 }, 0.5]);
        }
        self.indices.extend(s.triangles.iter().flatten().map(|i| i + first));
    }

    /// Streamlines as thin crossed strips: they stay visible from any side with no lighting to match.
    fn add_lines(&mut self, lines: &[post::Streamline], peak: f32, width: f64) {
        for line in lines {
            for (pair, speeds) in line.points.windows(2).zip(line.speeds.windows(2)) {
                let (a, b) = (DVec3::from_array(pair[0]), DVec3::from_array(pair[1]));
                let along = (b - a).normalize_or_zero();
                if along == DVec3::ZERO {
                    continue;
                }
                let side = along.cross(if along.z.abs() < 0.9 { DVec3::Z } else { DVec3::X }).normalize() * width;
                let other = along.cross(side).normalize() * width;
                let uv = |s: f32| [(s / peak.max(f32::MIN_POSITIVE)).clamp(0.0, 1.0), 0.5];
                for offset in [side, other] {
                    let first = self.positions.len() as u32;
                    for (p, s) in [(a - offset, speeds[0]), (a + offset, speeds[0]), (b + offset, speeds[1]), (b - offset, speeds[1])] {
                        self.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                        self.uv.push(uv(s));
                    }
                    self.indices.extend_from_slice(&[first, first + 1, first + 2, first, first + 2, first + 3]);
                }
            }
        }
    }
}

/// Works out what `request` asks for. On its own thread: a big volume takes a moment.
fn build(set: &ResultSet, request: &Request) -> Built {
    let mut built = Built::default();
    let Some(step) = set.steps.get(request.step) else { return built };
    let mesh = &set.mesh;
    let nodes = mesh.nodes.len();
    let (lo, hi) = bounds(&mesh.nodes);
    let field = step.fields.get(request.field).filter(|f| f.location == Location::Node && f.len() == nodes);
    if let Some(f) = field {
        let colour = f.scalar();
        let range = f.range().unwrap_or((0.0, 1.0));
        let shift = step.field("DISP");
        if let Some((axis, at)) = request.looks.cut {
            let mut normal = [0.0; 3];
            normal[axis] = 1.0;
            let mut origin = [0.0; 3];
            origin[axis] = f64::from(lo[axis] + (hi[axis] - lo[axis]) * at);
            let s = post::slice(mesh, &post::plane_distance(mesh, origin, normal), 0.0);
            built.add_slice(&s, &colour, range, shift, request.deform, nodes);
        }
        if let Some(level) = request.looks.iso {
            let scalar: Vec<f64> = colour.iter().map(|v| f64::from(*v)).collect();
            let s = post::slice(mesh, &scalar, f64::from(range.0 + (range.1 - range.0) * level));
            built.add_slice(&s, &colour, range, shift, request.deform, nodes);
        }
    }
    if let Some(count) = request.looks.lines
        && let Some(u) = velocity_of(step, nodes)
    {
        let seeds = post::default_seeds(mesh, u, count);
        let lines = post::streamlines(mesh, u, &seeds, 2400);
        let width = f64::from((hi - lo).length()) * 0.0012;
        built.add_lines(&lines, u.range().map_or(1.0, |r| r.1), width);
    }
    built
}

fn velocity_of(step: &ResultStep, nodes: usize) -> Option<&Field> {
    step.field("U").filter(|f| f.location == Location::Node && f.stride() == 3 && f.len() == nodes)
}

impl View {
    pub fn new(job: u64, set: Arc<ResultSet>) -> Self {
        let mut v = Self {
            job,
            set,
            step: 0,
            field: 0,
            deform: 1.0,
            animate: false,
            auto_scale: 1.0,
            show_walls: true,
            dirty: true,
            framed: false,
            entity: None,
            mesh: None,
            range: (0.0, 1.0),
            hides: None,
            looks: Looks::default(),
            legend: None,
            built: None,
            working: None,
        };
        // Open on the last step, the end state of most runs, on the field most people want first.
        let kind = v.set.steps.first().map(|s| s.kind);
        v.step = if kind == Some(StepKind::Mode) { 0 } else { v.set.steps.len().saturating_sub(1) };
        v.field = v.default_field();
        v.auto_scale = v.deformation_scale();
        v.deform = v.auto_scale;
        v.animate = kind == Some(StepKind::Mode);
        v.show_walls = walls_start_shown(&v.set.mesh);
        v
    }

    fn default_field(&self) -> usize {
        let Some(step) = self.set.steps.get(self.step) else { return 0 };
        for wanted in ["AMPLITUDE", "STRESS", "NDTEMP", "T", "U", "magnetic flux density", "alpha.water", "p", "DISP"] {
            if let Some(i) = step.fields.iter().position(|f| f.name == wanted) {
                return i;
            }
        }
        0
    }

    /// The displacement field that deforms the shape on this step, if there is one.
    pub fn displacement(&self) -> Option<&sim_core::results::Field> {
        self.set.steps.get(self.step)?.field("DISP")
    }

    /// The velocity on this step, if streamlines can follow one.
    pub fn velocity(&self) -> Option<&Field> {
        velocity_of(self.set.steps.get(self.step)?, self.set.mesh.nodes.len())
    }

    /// The top of the speed scale streamlines are coloured on.
    pub fn velocity_peak(&self) -> Option<f32> {
        self.velocity()?.range().map(|r| r.1)
    }

    /// Something asked for inside the volume is still being worked out.
    pub fn looks_pending(&self) -> bool {
        self.working.is_some()
    }

    fn request(&self) -> Request {
        Request { looks: self.looks.clone(), step: self.step, field: self.field, deform: if self.animate { 0.0 } else { self.deform } }
    }

    /// A scale making the largest displacement a tenth of the part's size.
    fn deformation_scale(&self) -> f32 {
        let Some(d) = self.displacement() else { return 0.0 };
        let Some((_, max)) = d.range() else { return 0.0 };
        if max <= 0.0 {
            return 0.0;
        }
        let (lo, hi) = bounds(&self.set.mesh.nodes);
        let size = (hi - lo).length();
        // A real deformation that's already large shows at true scale.
        (0.1 * size / max).max(1.0)
    }
}

/// Whether a result opens with its walls showing. They'd hide a flow they enclose, like a pipe's, so
/// those start hidden. A body standing in a flow hides nothing, and the view needs it to make sense.
fn walls_start_shown(mesh: &sim_core::results::ResultMesh) -> bool {
    let Some(start) = mesh.cut_start else { return true };
    let corners = |tris: &[[u32; 3]]| -> ([f64; 3], [f64; 3]) {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for n in tris.iter().flatten() {
            let p = mesh.nodes[*n as usize];
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    };
    let (walls, cut) = mesh.surface.split_at(start.min(mesh.surface.len()));
    let (wall_lo, wall_hi) = corners(walls);
    let (cut_lo, cut_hi) = corners(cut);
    // The cut reaches well beyond the walls, so they aren't around it.
    let margin = (0..3).map(|k| wall_hi[k] - wall_lo[k]).fold(0.0, f64::max) * 0.02;
    (0..3).any(|k| cut_lo[k] < wall_lo[k] - margin || cut_hi[k] > wall_hi[k] + margin)
}

fn bounds(nodes: &[[f64; 3]]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for n in nodes {
        let p = Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

#[derive(Resource, Default)]
pub struct ViewAssets {
    material: Option<Handle<StandardMaterial>>,
}

#[derive(Component)]
pub struct ResultMeshMarker;

/// What's drawn inside the volume: cuts, isosurfaces, streamlines.
#[derive(Component)]
pub struct LooksMarker;

fn ramp_image() -> Image {
    let mut data = Vec::with_capacity(256 * 4);
    for i in 0..256 {
        let [r, g, b] = colour(i as f32 / 255.0);
        data.extend_from_slice(&[r, g, b, 255]);
    }
    let mut image = Image::new(Extent3d { width: 256, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, data, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor { address_mode_u: ImageAddressMode::ClampToEdge, address_mode_v: ImageAddressMode::ClampToEdge, mag_filter: ImageFilterMode::Linear, min_filter: ImageFilterMode::Linear, ..default() });
    image
}

/// Positions and normals of the deformed surface, and each node's place on the colour scale.
fn surface_data(v: &View, amount: f32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 2]>, (f32, f32)) {
    let set = &v.set;
    let n = set.mesh.nodes.len();
    let mut pos: Vec<[f32; 3]> = set.mesh.nodes.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    if amount != 0.0 && let Some(d) = v.displacement() {
        for (i, p) in pos.iter_mut().enumerate().take(d.len()) {
            let u = d.at(i);
            if u.len() >= 3 && u[0].is_finite() && u[1].is_finite() && u[2].is_finite() {
                p[0] += u[0] * amount;
                p[1] += u[1] * amount;
                p[2] += u[2] * amount;
            }
        }
    }
    let mut normals = vec![Vec3::ZERO; n];
    for t in &set.mesh.surface {
        let (a, b, c) = (Vec3::from(pos[t[0] as usize]), Vec3::from(pos[t[1] as usize]), Vec3::from(pos[t[2] as usize]));
        let f = (b - a).cross(c - a);
        for i in t {
            normals[*i as usize] += f;
        }
    }
    let normals = normals.into_iter().map(|v| v.normalize_or_zero().to_array()).collect();

    let field = set.steps.get(v.step).and_then(|s| s.fields.get(v.field));
    let mut range = (0.0, 1.0);
    let mut uv = vec![[0.0f32, 0.5]; n];
    if let Some(f) = field {
        let values = f.scalar();
        if let Some(r) = f.range() {
            range = r;
        }
        let span = (range.1 - range.0).max(f32::MIN_POSITIVE);
        for (i, value) in values.iter().enumerate().take(n) {
            uv[i] = [if value.is_finite() { (value - range.0) / span } else { 0.0 }, 0.5];
        }
    }
    (pos, normals, uv, range)
}

pub fn sync(
    mut commands: Commands,
    mut state: ResMut<super::SimState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut assets: ResMut<ViewAssets>,
    time: Res<Time>,
    mut camera: Single<&mut OrbitCamera>,
    markers: Query<Entity, With<ResultMeshMarker>>,
    looks: Query<Entity, With<LooksMarker>>,
) {
    let Some(v) = state.view.as_mut() else {
        for e in markers.iter().chain(looks.iter()) {
            commands.entity(e).despawn();
        }
        return;
    };
    let material = assets
        .material
        .get_or_insert_with(|| {
            let ramp = images.add(ramp_image());
            materials.add(StandardMaterial { base_color_texture: Some(ramp), unlit: true, cull_mode: None, double_sided: true, ..default() })
        })
        .clone();
    let root = Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));

    // Inside the volume: one piece of work at a time, and the newest request wins.
    let want = v.request();
    if let Some((asked, rx)) = &v.working {
        match rx.try_recv() {
            Ok(built) => {
                let asked = asked.clone();
                for e in &looks {
                    commands.entity(e).despawn();
                }
                if !built.indices.is_empty() {
                    let count = built.positions.len();
                    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, built.positions)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; count])
                        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, built.uv)
                        .with_inserted_indices(Indices::U32(built.indices));
                    commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone()), root, LooksMarker));
                }
                v.built = Some(asked);
                v.working = None;
            }
            Err(TryRecvError::Empty) => {}
            // Its thread stopped without an answer. Not worth asking again and again.
            Err(TryRecvError::Disconnected) => {
                v.built = Some(asked.clone());
                v.working = None;
            }
        }
    }
    if v.working.is_none() && v.built.as_ref() != Some(&want) {
        if want.looks == Looks::default() {
            for e in &looks {
                commands.entity(e).despawn();
            }
            v.built = Some(want);
        } else {
            let (tx, rx) = unbounded();
            let (set, request) = (v.set.clone(), want.clone());
            std::thread::spawn(move || {
                let _ = tx.send(build(&set, &request));
            });
            v.working = Some((want, rx));
        }
    }

    let animating = v.animate && v.deform != 0.0;
    if !v.dirty && !animating {
        return;
    }
    let amount = if animating { v.deform * (time.elapsed_secs() * 3.0).sin() } else { v.deform };
    let (pos, normals, uv, range) = surface_data(v, amount);
    v.range = range;
    if v.dirty || v.mesh.is_none() {
        let first = if v.show_walls { 0 } else { v.set.mesh.cut_start.unwrap_or(0) };
        let indices: Vec<u32> = v.set.mesh.surface[first..].iter().flatten().copied().collect();
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_indices(Indices::U32(indices));
        let handle = meshes.add(mesh);
        for e in &markers {
            commands.entity(e).despawn();
        }
        // The solvers work in Z up; the scene is Y up.
        let entity = commands.spawn((Mesh3d(handle.clone()), MeshMaterial3d(material), root, ResultMeshMarker)).id();
        v.entity = Some(entity);
        v.mesh = Some(handle);
        if !v.framed {
            let (lo, hi) = bounds(&v.set.mesh.nodes);
            let centre = root.transform_point((lo + hi) * 0.5);
            camera.focus = centre;
            camera.radius = ((hi - lo).length() * 1.4).max(0.05);
            v.framed = true;
        }
    } else if let Some(mut mesh) = v.mesh.as_ref().and_then(|h| meshes.get_mut(h)) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    }
    v.dirty = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walls_hide_only_a_flow_they_enclose() {
        use sim_core::results::ResultMesh;
        // Walls round the unit cube, with four more points to make cuts from.
        let nodes = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.2, 0.2, 0.5], [0.8, 0.2, 0.5], [0.8, 0.8, 0.5], [-5.0, -5.0, 0.5], [5.0, -5.0, 0.5], [5.0, 5.0, 0.5]];
        let inside = ResultMesh { nodes: nodes.clone(), tets: vec![], surface: vec![[0, 1, 2], [3, 4, 5]], cut_start: Some(1) };
        assert!(!walls_start_shown(&inside), "a cut inside the walls");
        let outside = ResultMesh { nodes: nodes.clone(), tets: vec![], surface: vec![[0, 1, 2], [6, 7, 8]], cut_start: Some(1) };
        assert!(walls_start_shown(&outside), "a body in the middle of a wider cut");
        let no_cut = ResultMesh { nodes, tets: vec![], surface: vec![[0, 1, 2]], cut_start: None };
        assert!(walls_start_shown(&no_cut), "nothing to hide");
    }

    #[test]
    fn the_scale_runs_blue_to_red() {
        assert_eq!(colour(0.0), [0, 0, 255]);
        assert_eq!(colour(1.0), [255, 0, 0]);
        assert_eq!(colour(0.5), [0, 255, 0]);
        assert_eq!(colour(-3.0), [0, 0, 255], "clamped");
        assert_eq!(colour(9.0), [255, 0, 0], "clamped");
    }
}
