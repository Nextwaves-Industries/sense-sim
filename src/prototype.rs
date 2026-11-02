//! Background loading of USD assets into reusable Bevy mesh/material prototypes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crossbeam_channel::{Receiver, Sender, unbounded};
use rayon::prelude::*;

use crate::bvh::Bvh;
use crate::lod::{self, LodLevel, ProtoLod};
use crate::usd_load::{self, JointKind, MaterialData, TexRef};

/// A loaded asset ready to be instanced any number of times.
pub struct Prototype {
    pub parts: Vec<ProtoPart>,
    /// Movable joints (fixed joints only link bodies together).
    pub joints: Vec<ProtoJoint>,
    /// All joints including fixed ones, used to propagate motion.
    pub links: Vec<ProtoJoint>,
    /// Stage units to meters (for showing prismatic travel).
    pub unit: f32,
    /// Converts USD stage space to a Y-up, meters, bottom-centered local space.
    pub basis: Transform,
    /// Local-space bounds after `basis` (bottom sits at y = 0).
    pub aabb_min: Vec3,
    pub aabb_max: Vec3,
    pub triangles: usize,
    pub load_ms: f32,
    pub warnings: Vec<String>,
    /// Pipe openings and joint body sizes found on the mesh (see `plant::ports`).
    pub detected: crate::plant::ports::Detected,
}

pub struct ProtoPart {
    /// Physics body this part moves with, if any.
    pub body: Option<usize>,
    pub name: String,
    pub prim_path: String,
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
    pub transform: Transform,
    /// Triangles in mesh space, for fast ray casts.
    pub bvh: Arc<Bvh>,
    /// Bounds of the mesh, shared by all its versions (see `lod`).
    pub aabb: Aabb,
    pub triangles: u32,
    /// Coarser versions of `mesh` for zooming out, finest first.
    pub lods: Vec<ProtoLod>,
}

#[derive(Clone, Debug)]
pub struct ProtoJoint {
    pub name: String,
    pub kind: JointKind,
    pub parent: Option<usize>,
    pub child: usize,
    /// Stage-space pivot and unit axis at rest.
    pub anchor: Vec3,
    pub axis: Vec3,
    pub lower: Option<f32>,
    pub upper: Option<f32>,
}

impl ProtoJoint {
    /// Slider range: authored limits, else a sensible default.
    pub fn range(&self, unit: f32) -> (f32, f32) {
        match self.kind {
            JointKind::Revolute => (self.lower.unwrap_or(-180.0), self.upper.unwrap_or(180.0)),
            _ => (self.lower.unwrap_or(-1.0 / unit), self.upper.unwrap_or(1.0 / unit)),
        }
    }

    /// Rest-relative motion of the child body for joint value `q`.
    pub fn motion(&self, q: f32) -> Mat4 {
        match self.kind {
            JointKind::Revolute => {
                Mat4::from_translation(self.anchor)
                    * Mat4::from_axis_angle(self.axis, q.to_radians())
                    * Mat4::from_translation(-self.anchor)
            }
            JointKind::Prismatic => Mat4::from_translation(self.axis * q),
            JointKind::Fixed => Mat4::IDENTITY,
        }
    }
}

impl Prototype {
    /// Per-body stage-space deltas from rest, for movable joint values `q`
    /// (indexed like `joints`). Children inherit their parent body's motion.
    pub fn body_deltas(&self, q: &[f32]) -> Vec<Mat4> {
        let n = self.links.iter().map(|j| j.child.max(j.parent.unwrap_or(0)) + 1).max().unwrap_or(0);
        let mut d: Vec<Option<Mat4>> = vec![None; n];
        let is_child: Vec<bool> = (0..n).map(|b| self.links.iter().any(|j| j.child == b)).collect();
        for b in 0..n {
            if !is_child[b] {
                d[b] = Some(Mat4::IDENTITY);
            }
        }
        let mut movable = 0;
        let values: Vec<f32> = self
            .links
            .iter()
            .map(|j| {
                if j.kind == JointKind::Fixed {
                    0.0
                } else {
                    movable += 1;
                    q.get(movable - 1).copied().unwrap_or(0.0)
                }
            })
            .collect();
        for _ in 0..n {
            let mut progress = false;
            for (j, v) in self.links.iter().zip(&values) {
                if d[j.child].is_some() {
                    continue;
                }
                let parent = match j.parent {
                    Some(p) => match d[p] {
                        Some(m) => m,
                        None => continue,
                    },
                    None => Mat4::IDENTITY,
                };
                d[j.child] = Some(parent * j.motion(*v));
                progress = true;
            }
            if !progress {
                break;
            }
        }
        d.into_iter().map(|m| m.unwrap_or(Mat4::IDENTITY)).collect()
    }
}

pub enum ProtoState {
    Loading,
    Ready(Arc<Prototype>),
    Failed(String),
}

#[derive(Resource)]
pub struct PrototypeCache {
    pub states: HashMap<PathBuf, ProtoState>,
    pub max_texture: u32,
    tx: Sender<JobResult>,
    rx: Receiver<JobResult>,
    /// Waiting for a free loader, the latest last: it is the one being looked at.
    queue: Vec<PathBuf>,
    /// Loaders running; a layout can ask for hundreds of assets at once.
    running: usize,
    max_running: usize,
}

impl Default for PrototypeCache {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self { states: HashMap::new(), max_texture: 1024, tx, rx, queue: Vec::new(), running: 0, max_running: (cores / 3).clamp(2, 6) }
    }
}

impl PrototypeCache {
    pub fn get(&self, path: &Path) -> Option<Arc<Prototype>> {
        match self.states.get(path) {
            Some(ProtoState::Ready(p)) => Some(p.clone()),
            _ => None,
        }
    }

    pub fn loading_count(&self) -> usize {
        self.states.values().filter(|s| matches!(s, ProtoState::Loading)).count()
    }

    /// Starts a background load unless the asset is already loaded or loading.
    pub fn request(&mut self, path: &Path) {
        if self.states.contains_key(path) {
            return;
        }
        self.states.insert(path.to_path_buf(), ProtoState::Loading);
        self.queue.push(path.to_path_buf());
        self.start_loaders();
    }

    fn start_loaders(&mut self) {
        while self.running < self.max_running {
            let Some(path) = self.queue.pop() else { break };
            self.running += 1;
            let tx = self.tx.clone();
            let max_tex = self.max_texture;
            std::thread::spawn(move || {
                let t = std::time::Instant::now();
                let result = std::panic::catch_unwind(|| prepare(&path, max_tex))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("loader panicked")));
                let _ = tx.send(JobResult { path, result, ms: t.elapsed().as_secs_f32() * 1000.0 });
            });
        }
    }

    pub fn retry(&mut self, path: &Path) {
        if matches!(self.states.get(path), Some(ProtoState::Failed(_))) {
            self.states.remove(path);
        }
        self.request(path);
    }

    pub fn clear(&mut self) {
        self.states.retain(|_, s| matches!(s, ProtoState::Loading));
    }
}

struct JobResult {
    path: PathBuf,
    result: anyhow::Result<Prepared>,
    ms: f32,
}

/// Everything built off the main thread.
struct Prepared {
    meshes: Vec<PreparedPart>,
    links: Vec<ProtoJoint>,
    unit: f32,
    materials: Vec<PreparedMaterial>,
    images: Vec<Image>,
    basis: Transform,
    aabb_min: Vec3,
    aabb_max: Vec3,
    triangles: usize,
    warnings: Vec<String>,
    detected: crate::plant::ports::Detected,
}

struct PreparedPart {
    name: String,
    prim_path: String,
    mesh: Mesh,
    material: Option<usize>,
    transform: Transform,
    body: Option<usize>,
    bvh: Arc<Bvh>,
    aabb: Aabb,
    triangles: u32,
    lods: Vec<LodLevel>,
}

struct PreparedMaterial {
    data: MaterialData,
    base: Option<usize>,
    mr: Option<usize>,
    normal: Option<usize>,
    emissive: Option<usize>,
}

// ---------------------------------------------------------------------------
// Off-thread preparation

fn prepare(path: &Path, max_tex: u32) -> anyhow::Result<Prepared> {
    let loaded = usd_load::load(path)?;
    let mut warnings = loaded.warnings;
    if loaded.parts.is_empty() {
        anyhow::bail!("no renderable meshes found");
    }

    // Stage space -> Y-up meters.
    let unit = loaded.meters_per_unit;
    let rot = if loaded.z_up { Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2) } else { Quat::IDENTITY };
    let to_y_up = Mat4::from_scale_rotation_translation(Vec3::splat(unit), rot, Vec3::ZERO);

    // Bounds in Y-up space, to bottom-center the asset on its origin.
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for part in &loaded.parts {
        let m = to_y_up * Mat4::from_cols_array(&part.world);
        for p in &part.positions {
            let w = m.transform_point3(Vec3::from_array(*p));
            mn = mn.min(w);
            mx = mx.max(w);
        }
    }
    let offset = Vec3::new(-(mn.x + mx.x) * 0.5, -mn.y, -(mn.z + mx.z) * 0.5);
    let basis = Transform::from_matrix(Mat4::from_translation(offset) * to_y_up);

    // Textures: decode each (file, channel-packing) once, in parallel.
    let mut tex_jobs: Vec<TexJob> = Vec::new();
    let mut tex_index: HashMap<TexJob, usize> = HashMap::new();
    let mut add_job = |job: TexJob| -> usize {
        *tex_index.entry(job.clone()).or_insert_with(|| {
            tex_jobs.push(job);
            tex_jobs.len() - 1
        })
    };
    let mut materials = Vec::new();
    for m in &loaded.materials {
        let base = m.base_color_tex.as_ref().map(|t| add_job(TexJob::Color(t.file.clone(), true, m.opacity_tex.clone())));
        let mr = (m.metallic_tex.is_some() || m.roughness_tex.is_some()).then(|| {
            add_job(TexJob::Pack(
                m.occlusion_tex.as_ref().map(chan),
                m.roughness_tex.as_ref().map(chan),
                m.metallic_tex.as_ref().map(chan),
            ))
        });
        let normal = m.normal_tex.as_ref().map(|t| add_job(TexJob::Color(t.file.clone(), false, None)));
        let emissive = m.emissive_tex.as_ref().map(|t| add_job(TexJob::Color(t.file.clone(), true, None)));
        materials.push(PreparedMaterial { data: m.clone(), base, mr, normal, emissive });
    }
    let decoded: Vec<Result<Image, String>> = tex_jobs.par_iter().map(|j| j.run(max_tex)).collect();
    let mut images = Vec::with_capacity(decoded.len());
    for (i, r) in decoded.into_iter().enumerate() {
        match r {
            Ok(img) => images.push(img),
            Err(e) => {
                warnings.push(format!("texture {:?}: {e}", tex_jobs[i]));
                images.push(placeholder_image());
            }
        }
    }

    // Bodies are the prims joints connect; a part moves with the body whose
    // path is the longest prefix of its own.
    let mut bodies: Vec<String> = Vec::new();
    let body_index = |p: &str, bodies: &mut Vec<String>| -> usize {
        bodies.iter().position(|b| b == p).unwrap_or_else(|| {
            bodies.push(p.to_string());
            bodies.len() - 1
        })
    };
    let mut links = Vec::new();
    for j in &loaded.joints {
        let Some(child) = j.body1.as_deref() else { continue };
        let child = body_index(child, &mut bodies);
        let parent = j.body0.as_deref().map(|p| body_index(p, &mut bodies));
        let pretty = j.name.trim_start_matches("joint_").trim_end_matches("_joint_00").replace('_', " ");
        links.push(ProtoJoint {
            name: pretty,
            kind: j.kind,
            parent,
            child,
            anchor: Vec3::from_array(j.anchor),
            axis: Vec3::from_array(j.axis),
            lower: j.lower,
            upper: j.upper,
        });
    }
    let body_of = |prim: &str| -> Option<usize> {
        bodies
            .iter()
            .enumerate()
            .filter(|(_, b)| prim == b.as_str() || (prim.starts_with(b.as_str()) && prim.as_bytes().get(b.len()) == Some(&b'/')))
            .max_by_key(|(_, b)| b.len())
            .map(|(i, _)| i)
    };

    let part_body: Vec<Option<usize>> = loaded.parts.iter().map(|p| body_of(&p.prim_path)).collect();
    let detected = crate::plant::ports::detect(&loaded.parts, &part_body, &links, basis.to_matrix(), crate::plant::ports::wants_rims(path));

    let needs_tangents: Vec<bool> = materials.iter().map(|m| m.normal.is_some()).collect();
    let triangles = loaded.parts.iter().map(|p| p.indices.len() / 3).sum();
    let meshes = loaded
        .parts
        .into_par_iter()
        .map(|part| {
            let tangents = part.material.map(|i| needs_tangents[i]).unwrap_or(false);
            let transform = Transform::from_matrix(Mat4::from_cols_array(&part.world));
            let material = part.material;
            let name = part.name.clone();
            let prim_path = part.prim_path.clone();
            let body = body_of(&prim_path);
            let bvh = Arc::new(Bvh::new(&part.positions, &part.indices));
            let triangles = (part.indices.len() / 3) as u32;
            let mesh = build_mesh(part, tangents);
            let aabb = lod::mesh_bounds(&mesh).unwrap_or_default();
            let lods = lod::build_levels(&mesh);
            PreparedPart { name, prim_path, mesh, material, transform, body, bvh, aabb, triangles, lods }
        })
        .collect();

    Ok(Prepared { meshes, links, unit, materials, images, basis, aabb_min: mn + offset, aabb_max: mx + offset, triangles, warnings, detected })
}

/// Loads only what port detection needs (no textures or GPU meshes):
/// local bounds, detected rims/joint bodies, and movable joint names.
pub fn probe(path: &Path) -> anyhow::Result<(Vec3, Vec3, crate::plant::ports::Detected, Vec<String>)> {
    let loaded = usd_load::load(path)?;
    let unit = loaded.meters_per_unit;
    let rot = if loaded.z_up { Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2) } else { Quat::IDENTITY };
    let to_y_up = Mat4::from_scale_rotation_translation(Vec3::splat(unit), rot, Vec3::ZERO);
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for part in &loaded.parts {
        let m = to_y_up * Mat4::from_cols_array(&part.world);
        for p in &part.positions {
            let w = m.transform_point3(Vec3::from_array(*p));
            mn = mn.min(w);
            mx = mx.max(w);
        }
    }
    if mn.x > mx.x {
        anyhow::bail!("no renderable meshes found");
    }
    let offset = Vec3::new(-(mn.x + mx.x) * 0.5, -mn.y, -(mn.z + mx.z) * 0.5);
    let basis = Mat4::from_translation(offset) * to_y_up;
    let mut bodies: Vec<String> = Vec::new();
    let mut links = Vec::new();
    for j in &loaded.joints {
        let Some(child) = j.body1.as_deref() else { continue };
        let idx = |p: &str, bodies: &mut Vec<String>| bodies.iter().position(|b| b == p).unwrap_or_else(|| {
            bodies.push(p.to_string());
            bodies.len() - 1
        });
        let child = idx(child, &mut bodies);
        let parent = j.body0.as_deref().map(|p| idx(p, &mut bodies));
        let pretty = j.name.trim_start_matches("joint_").trim_end_matches("_joint_00").replace('_', " ");
        links.push(ProtoJoint { name: pretty, kind: j.kind, parent, child, anchor: Vec3::from_array(j.anchor), axis: Vec3::from_array(j.axis), lower: j.lower, upper: j.upper });
    }
    let body_of = |prim: &str| -> Option<usize> {
        bodies
            .iter()
            .enumerate()
            .filter(|(_, b)| prim == b.as_str() || (prim.starts_with(b.as_str()) && prim.as_bytes().get(b.len()) == Some(&b'/')))
            .max_by_key(|(_, b)| b.len())
            .map(|(i, _)| i)
    };
    let part_body: Vec<Option<usize>> = loaded.parts.iter().map(|p| body_of(&p.prim_path)).collect();
    let det = crate::plant::ports::detect(&loaded.parts, &part_body, &links, basis, crate::plant::ports::wants_rims(path));
    let names = links.iter().filter(|j| j.kind != JointKind::Fixed).map(|j| j.name.clone()).collect();
    Ok((mn + offset, mx + offset, det, names))
}

fn chan(t: &TexRef) -> (PathBuf, usize) {
    (t.file.clone(), t.channel)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum TexJob {
    /// File, sRGB, optional opacity source folded into alpha.
    Color(PathBuf, bool, Option<TexRef>),
    /// Repacked into Bevy's layout: R=occlusion, G=roughness, B=metallic.
    Pack(Option<(PathBuf, usize)>, Option<(PathBuf, usize)>, Option<(PathBuf, usize)>),
}

impl Eq for TexRef {}
impl PartialEq for TexRef {
    fn eq(&self, o: &Self) -> bool {
        self.file == o.file && self.channel == o.channel
    }
}
impl std::hash::Hash for TexRef {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.file.hash(h);
        self.channel.hash(h);
    }
}

fn open_scaled(path: &Path, max: u32) -> Result<image::RgbaImage, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let img = if img.width() > max || img.height() > max {
        img.resize(max, max, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok(img.to_rgba8())
}

impl TexJob {
    fn run(&self, max: u32) -> Result<Image, String> {
        match self {
            TexJob::Color(path, srgb, opacity) => {
                let mut img = open_scaled(path, max)?;
                if let Some(op) = opacity
                    && op.file != *path
                    && let Ok(a) = open_scaled(&op.file, max)
                {
                    let a = image::imageops::resize(&a, img.width(), img.height(), image::imageops::FilterType::Triangle);
                    for (p, q) in img.pixels_mut().zip(a.pixels()) {
                        p.0[3] = q.0[op.channel];
                    }
                }
                Ok(to_bevy(img, *srgb))
            }
            TexJob::Pack(occ, rough, metal) => {
                let mut cache: HashMap<PathBuf, image::RgbaImage> = HashMap::new();
                let mut size = None;
                for (p, _) in [occ, rough, metal].into_iter().flatten() {
                    if !cache.contains_key(p) {
                        let img = open_scaled(p, max)?;
                        size.get_or_insert((img.width(), img.height()));
                        cache.insert(p.clone(), img);
                    }
                }
                let (w, h) = size.unwrap_or((1, 1));
                let mut out = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 0, 255]));
                for (dst, src) in [(0usize, occ), (1, rough), (2, metal)] {
                    let Some((p, c)) = src else { continue };
                    let mut img = cache[p].clone();
                    if img.dimensions() != (w, h) {
                        img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
                    }
                    for (o, i) in out.pixels_mut().zip(img.pixels()) {
                        o.0[dst] = i.0[*c];
                    }
                }
                Ok(to_bevy(out, false))
            }
        }
    }
}

/// Uploads with a CPU-built mip chain and a repeating, anisotropic sampler.
fn to_bevy(img: image::RgbaImage, srgb: bool) -> Image {
    let (w, h) = img.dimensions();
    let mut data = img.as_raw().clone();
    let mut levels = 1u32;
    let mut cur = img;
    while cur.width() > 1 || cur.height() > 1 {
        let nw = (cur.width() / 2).max(1);
        let nh = (cur.height() / 2).max(1);
        cur = image::imageops::resize(&cur, nw, nh, image::imageops::FilterType::Triangle);
        data.extend_from_slice(cur.as_raw());
        levels += 1;
    }
    let format = if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
    let mut image = Image::new_uninit(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    image
}

fn placeholder_image() -> Image {
    to_bevy(image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 255, 255])), false)
}

fn build_mesh(part: usd_load::MeshPart, tangents: bool) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs);
    if let Some(c) = part.colors {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, c);
    }
    mesh.insert_indices(Indices::U32(part.indices));
    if tangents {
        let _ = mesh.generate_tangents();
    }
    mesh
}

// ---------------------------------------------------------------------------
// Main-thread finalization

/// Main-thread time per frame for turning loaded assets into meshes and materials.
const FINALIZE_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

pub fn receive_prototypes(
    mut cache: ResMut<PrototypeCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // At least one per frame; the rest wait if a frame has had its share.
    let started = std::time::Instant::now();
    while let Ok(job) = cache.rx.try_recv() {
        cache.running -= 1;
        let state = match job.result {
            Ok(p) => ProtoState::Ready(Arc::new(finalize(p, job.ms, &mut meshes, &mut images, &mut materials))),
            Err(e) => ProtoState::Failed(format!("{e:#}")),
        };
        cache.states.insert(job.path, state);
        if started.elapsed() > FINALIZE_BUDGET {
            break;
        }
    }
    cache.start_loaders();
}

fn finalize(
    p: Prepared,
    ms: f32,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
) -> Prototype {
    let image_handles: Vec<Handle<Image>> = p.images.into_iter().map(|i| images.add(i)).collect();
    let img = |i: Option<usize>| i.map(|i| image_handles[i].clone());
    let mat_handles: Vec<Handle<StandardMaterial>> = p
        .materials
        .iter()
        .map(|m| materials.add(standard_material(m, &img)))
        .collect();
    let default_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.6, 0.6),
        perceptual_roughness: 0.6,
        ..default()
    });

    let parts = p
        .meshes
        .into_iter()
        .map(|part| ProtoPart {
            name: part.name,
            body: part.body,
            prim_path: part.prim_path,
            mesh: meshes.add(part.mesh),
            material: part.material.map(|i| mat_handles[i].clone()).unwrap_or_else(|| default_mat.clone()),
            transform: part.transform,
            bvh: part.bvh,
            aabb: part.aabb,
            triangles: part.triangles,
            lods: part.lods.into_iter().map(|l| ProtoLod { mesh: meshes.add(l.mesh), error: l.error, triangles: l.triangles }).collect(),
        })
        .collect();
    Prototype {
        parts,
        joints: p.links.iter().filter(|j| j.kind != JointKind::Fixed).cloned().collect(),
        links: p.links,
        unit: p.unit,
        basis: p.basis,
        aabb_min: p.aabb_min,
        aabb_max: p.aabb_max,
        triangles: p.triangles,
        load_ms: ms,
        warnings: p.warnings,
        detected: p.detected,
    }
}

fn standard_material(m: &PreparedMaterial, img: &impl Fn(Option<usize>) -> Option<Handle<Image>>) -> StandardMaterial {
    let d = &m.data;
    let has_mr = m.mr.is_some();
    let alpha_mode = if d.opacity_threshold > 0.0 {
        AlphaMode::Mask(d.opacity_threshold)
    } else if d.opacity < 0.999 || d.opacity_tex.is_some() {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
    // UsdTransform2d is st' = R*S*st + T in USD's bottom-left UV space. Mesh
    // UVs were flipped with F(u, v) = (u, 1 - v) = D*uv + e, so the GPU-side
    // transform is F(R*S*F(uv) + T) = (D*R*S*D)*uv + D*(R*S*e + T) + e.
    let s = Vec2::from_array(d.uv_scale);
    let t = Vec2::from_array(d.uv_translate);
    let e = Vec2::Y;
    let flip = Mat2::from_diagonal(Vec2::new(1.0, -1.0));
    let rs = Mat2::from_angle(d.uv_rotation_deg.to_radians()) * Mat2::from_diagonal(s);
    let uv_transform = Affine2::from_mat2_translation(flip * rs * flip, flip * (rs * e + t) + e);
    StandardMaterial {
        base_color: Color::linear_rgba(d.base_color[0], d.base_color[1], d.base_color[2], d.opacity),
        base_color_texture: img(m.base),
        metallic: if d.metallic_tex.is_some() { 1.0 } else { d.metallic },
        perceptual_roughness: if d.roughness_tex.is_some() { 1.0 } else { d.roughness },
        metallic_roughness_texture: img(m.mr),
        occlusion_texture: if has_mr && d.occlusion_tex.is_some() { img(m.mr) } else { None },
        normal_map_texture: img(m.normal),
        emissive: LinearRgba::rgb(d.emissive[0], d.emissive[1], d.emissive[2]),
        emissive_texture: img(m.emissive),
        alpha_mode,
        uv_transform,
        double_sided: false,
        cull_mode: None,
        ..default()
    }
}
