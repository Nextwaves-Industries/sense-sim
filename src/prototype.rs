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
    /// Glass and the like let light through, so they cast no shadow.
    pub see_through: bool,
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
    see_through: bool,
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

    // Textures: decode each (file, channel-packing, tile layout) once, in parallel.
    let mut tex_jobs: Vec<(TexJob, (u32, u32))> = Vec::new();
    let mut tex_index: HashMap<(TexJob, (u32, u32)), usize> = HashMap::new();
    let mut add_job = |job: TexJob, grid: (u32, u32)| -> usize {
        *tex_index.entry((job.clone(), grid)).or_insert_with(|| {
            tex_jobs.push((job, grid));
            tex_jobs.len() - 1
        })
    };
    let mut materials = Vec::new();
    for m in &loaded.materials {
        let grid = m.udim_grid;
        let base = m.base_color_tex.as_ref().map(|t| {
            let job = if m.glass { TexJob::Glass(t.file.clone(), t.srgb) } else { TexJob::Color(t.file.clone(), t.srgb, m.opacity_tex.clone()) };
            add_job(job, grid)
        });
        let mr = (m.metallic_tex.is_some() || m.roughness_tex.is_some() || m.occlusion_tex.is_some()).then(|| {
            add_job(
                TexJob::Pack(
                    m.occlusion_tex.as_ref().map(chan),
                    m.roughness_tex.as_ref().map(chan),
                    m.metallic_tex.as_ref().map(chan),
                ),
                grid,
            )
        });
        let normal = m.normal_tex.as_ref().map(|t| add_job(TexJob::Normal(t.file.clone(), m.normal_flip, m.normal_strength.to_bits()), grid));
        let emissive = m.emissive_tex.as_ref().map(|t| add_job(TexJob::Color(t.file.clone(), t.srgb, None), grid));
        materials.push(PreparedMaterial { data: m.clone(), base, mr, normal, emissive });
    }
    let decoded: Vec<Result<Image, String>> = tex_jobs.par_iter().map(|(j, grid)| j.run(max_tex, *grid)).collect();
    let mut images = Vec::with_capacity(decoded.len());
    for (i, r) in decoded.into_iter().enumerate() {
        match r {
            Ok(img) => images.push(img),
            Err(e) => {
                warnings.push(format!("texture {:?}: {e}", tex_jobs[i].0));
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
    let see_through: Vec<bool> = materials.iter().map(|m| is_see_through(&m.data)).collect();
    let triangles = loaded.parts.iter().map(|p| p.indices.len() / 3).sum();
    let meshes = loaded
        .parts
        .into_par_iter()
        .map(|part| {
            let tangents = part.material.map(|i| needs_tangents[i]).unwrap_or(false);
            let see_through = part.material.is_some_and(|i| see_through[i]);
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
            PreparedPart { name, prim_path, mesh, material, transform, body, bvh, aabb, triangles, lods, see_through }
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
    /// File, X and Y pointing against OpenGL's convention, strength (as `f32` bits).
    Normal(PathBuf, [bool; 2], u32),
    /// A glass's color map (file, sRGB), baked into how it draws: see `glass_map`.
    Glass(PathBuf, bool),
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

/// Widest side a texture may have on the GPU.
const MAX_ATLAS_SIDE: u32 = 8192;

/// Opens a texture for a material whose textures are laid out in `grid` UDIM tiles.
fn open_scaled(path: &Path, max: u32, grid: (u32, u32)) -> Result<image::RgbaImage, String> {
    if grid != (1, 1) {
        return open_tiles(path, max, grid);
    }
    if path.to_string_lossy().contains("<UDIM>") {
        return open_tiles(path, max, (1, 1));
    }
    let img = image::open(path).map_err(|e| e.to_string())?;
    let img = if img.width() > max || img.height() > max {
        img.resize(max, max, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok(img.to_rgba8())
}

/// Lays UDIM tiles out in one image, the way `MaterialData::fit_udim_tiles` expects: a
/// tile's column and row, from the bottom like USD's st, are its place in the image. A
/// plain texture in the same material fills every tile. The image gets the pixels of
/// four textures at most, shared out between its tiles.
fn open_tiles(path: &Path, max: u32, (columns, rows): (u32, u32)) -> Result<image::RgbaImage, String> {
    let budget = (max as f32 * 2.0).powi(2);
    let side = ((budget / (columns * rows) as f32).sqrt() as u32).min(max).min(MAX_ATLAS_SIDE / columns.max(rows)).max(1);
    let plain = !path.to_string_lossy().contains("<UDIM>");
    let shared = if plain { Some(image::open(path).map_err(|e| e.to_string())?.resize_exact(side, side, image::imageops::FilterType::Triangle)) } else { None };
    let mut atlas = image::RgbaImage::from_pixel(columns * side, rows * side, image::Rgba([128, 128, 128, 255]));
    let mut found = false;
    for row in 0..rows {
        for column in 0..columns {
            let tile = match &shared {
                Some(tile) => tile.clone(),
                // A layout often leaves some tiles out: nothing samples them.
                None => match image::open(usd_load::udim_tile(path, column, row)) {
                    Ok(tile) => tile.resize_exact(side, side, image::imageops::FilterType::Triangle),
                    Err(_) => continue,
                },
            };
            found = true;
            image::imageops::replace(&mut atlas, &tile.to_rgba8(), (column * side) as i64, ((rows - 1 - row) * side) as i64);
        }
    }
    if found { Ok(atlas) } else { Err(format!("no UDIM tiles for {}", path.display())) }
}

/// Bakes a normal map's direction and strength into its pixels, the way OmniPBR reads
/// one: normalize(tangent * x * strength + bitangent * y * strength + normal * z).
fn adjust_normal_map(img: &mut image::RgbaImage, flip: [bool; 2], strength: f32) {
    if flip == [false, false] && strength == 1.0 {
        return;
    }
    let scale = [if flip[0] { -strength } else { strength }, if flip[1] { -strength } else { strength }];
    for p in img.pixels_mut() {
        let x = (p.0[0] as f32 / 127.5 - 1.0) * scale[0];
        let y = (p.0[1] as f32 / 127.5 - 1.0) * scale[1];
        let z = p.0[2] as f32 / 127.5 - 1.0;
        let n = Vec3::new(x, y, z).normalize_or(Vec3::Z);
        p.0[0] = ((n.x + 1.0) * 127.5).round() as u8;
        p.0[1] = ((n.y + 1.0) * 127.5).round() as u8;
        p.0[2] = ((n.z + 1.0) * 127.5).round() as u8;
    }
}

/// Bakes a glass's color map into premultiplied color and cover, pixel by pixel, so a
/// windscreen's dark band hides what's behind it and its clear part doesn't. Comes out in
/// sRGB whichever way the map was stored.
fn glass_map(img: &mut image::RgbaImage, srgb: bool) {
    let to_linear = |c: u8| {
        let c = c as f32 / 255.0;
        if srgb { c.powf(2.2) } else { c }
    };
    for p in img.pixels_mut() {
        let color = [to_linear(p.0[0]), to_linear(p.0[1]), to_linear(p.0[2])];
        let cover = glass_cover(color);
        for k in 0..3 {
            p.0[k] = ((color[k] * cover * GLASS_TINT).powf(1.0 / 2.2) * 255.0).round() as u8;
        }
        p.0[3] = (cover * 255.0).round() as u8;
    }
}

impl TexJob {
    fn run(&self, max: u32, grid: (u32, u32)) -> Result<Image, String> {
        match self {
            TexJob::Color(path, srgb, opacity) => {
                let mut img = open_scaled(path, max, grid)?;
                if let Some(op) = opacity
                    && (op.file != *path || op.channel != 3)
                    && let Ok(a) = open_scaled(&op.file, max, grid)
                {
                    let a = image::imageops::resize(&a, img.width(), img.height(), image::imageops::FilterType::Triangle);
                    for (p, q) in img.pixels_mut().zip(a.pixels()) {
                        p.0[3] = q.0[op.channel];
                    }
                }
                Ok(to_bevy(img, *srgb))
            }
            TexJob::Normal(path, flip, strength) => {
                let mut img = open_scaled(path, max, grid)?;
                adjust_normal_map(&mut img, *flip, f32::from_bits(*strength));
                Ok(to_bevy(img, false))
            }
            TexJob::Glass(path, srgb) => {
                let mut img = open_scaled(path, max, grid)?;
                glass_map(&mut img, *srgb);
                Ok(to_bevy(img, true))
            }
            TexJob::Pack(occ, rough, metal) => {
                let mut cache: HashMap<PathBuf, image::RgbaImage> = HashMap::new();
                let mut size = None;
                for (p, _) in [occ, rough, metal].into_iter().flatten() {
                    if !cache.contains_key(p) {
                        let img = open_scaled(p, max, grid)?;
                        size.get_or_insert((img.width(), img.height()));
                        cache.insert(p.clone(), img);
                    }
                }
                let (w, h) = size.unwrap_or((1, 1));
                // A channel with no map stays at full, leaving the material's constant in charge.
                let mut out = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
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
            see_through: part.see_through,
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

/// Light falling on a surface is about this much stronger than its color, so a glass's
/// tint is scaled by this to add about as much as it takes away.
const GLASS_TINT: f32 = 0.6;

/// Whether a material shows what's behind it evenly, rather than through cut-out shapes.
fn is_see_through(d: &MaterialData) -> bool {
    d.glass || (d.opacity < 0.999 && d.opacity_threshold <= 0.0 && d.opacity_tex.is_none())
}

/// How much glass of this color hides what's behind it: clear glass barely does and
/// dark glass mostly does. Never quite nothing, so a clear pane still dims its background a little.
fn glass_cover(color: [f32; 3]) -> f32 {
    let brightness = 0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2];
    (1.0 - brightness).clamp(0.04, 0.92)
}

/// The base color and alpha mode a material draws with.
fn surface_color(m: &PreparedMaterial) -> (Color, AlphaMode) {
    let d = &m.data;
    let [r, g, b] = d.base_color;
    if d.glass {
        // Glass has no diffuse color of its own: what's behind it shows through, dimmed
        // by the cover and tinted by a little of its color. Premultiplied blending adds
        // reflections on top at full strength, as on real glass; plain blending would
        // fade them too. A color map has all this baked in (`glass_map`).
        let (tint, cover) = if m.base.is_some() {
            (d.opacity, d.opacity)
        } else {
            let cover = glass_cover(d.base_color) * d.opacity;
            (cover * GLASS_TINT, cover)
        };
        return (Color::linear_rgba(r * tint, g * tint, b * tint, cover), AlphaMode::Premultiplied);
    }
    if is_see_through(d) {
        let a = d.opacity;
        return (Color::linear_rgba(r * a, g * a, b * a, a), AlphaMode::Premultiplied);
    }
    let mode = if d.opacity_threshold > 0.0 {
        AlphaMode::Mask(d.opacity_threshold)
    } else if d.opacity_tex.is_some() {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
    (Color::linear_rgba(r, g, b, d.opacity), mode)
}

fn standard_material(m: &PreparedMaterial, img: &impl Fn(Option<usize>) -> Option<Handle<Image>>) -> StandardMaterial {
    let d = &m.data;
    let has_mr = m.mr.is_some();
    let (base_color, alpha_mode) = surface_color(m);
    // Materials transform st as st' = M*st + T in USD's bottom-left UV space. Mesh
    // UVs were flipped with F(u, v) = (u, 1 - v) = D*uv + e, so the GPU-side
    // transform is F(M*F(uv) + T) = (D*M*D)*uv + D*(M*e + T) + e.
    let t = Vec2::from_array(d.uv_translate);
    let e = Vec2::Y;
    let flip = Mat2::from_diagonal(Vec2::new(1.0, -1.0));
    let matrix = Mat2::from_cols_array(&d.uv_matrix);
    let uv_transform = Affine2::from_mat2_translation(flip * matrix * flip, flip * (matrix * e + t) + e);
    StandardMaterial {
        base_color,
        base_color_texture: img(m.base),
        metallic: if d.metallic_tex.is_some() { 1.0 } else { d.metallic },
        perceptual_roughness: if d.roughness_tex.is_some() { 1.0 } else { d.roughness },
        // OmniPBR reflects 0.08 * level head-on; Bevy reflects 0.16 * reflectance^2.
        reflectance: (d.specular_level.max(0.0) * 0.5).sqrt(),
        metallic_roughness_texture: img(m.mr),
        occlusion_texture: if has_mr && d.occlusion_tex.is_some() { img(m.mr) } else { None },
        normal_map_texture: img(m.normal),
        emissive: LinearRgba::rgb(d.emissive[0], d.emissive[1], d.emissive[2]),
        emissive_texture: img(m.emissive),
        alpha_mode,
        uv_transform,
        // Both sides draw, each shaded with the normal facing the viewer. This relies
        // on triangles being wound to match their normals (`usd_load::orient_triangles`).
        double_sided: true,
        cull_mode: None,
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(data: MaterialData) -> PreparedMaterial {
        PreparedMaterial { data, base: None, mr: None, normal: None, emissive: None }
    }

    #[test]
    fn a_directx_normal_map_is_baked_to_opengl() {
        // Leaning right and up in the image's own convention.
        let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba([200, 200, 200, 255]));
        adjust_normal_map(&mut img, [false, true], 1.0);
        let p = img.get_pixel(0, 0).0;
        assert!(p[0] > 127 && p[1] < 127 && p[2] > 127, "{p:?}");
    }

    #[test]
    fn normal_strength_flattens_a_map_as_omnipbr_does() {
        let lean = |strength: f32| {
            let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba([230, 128, 180, 255]));
            adjust_normal_map(&mut img, [false, false], strength);
            img.get_pixel(0, 0).0[0] as f32 - 127.5
        };
        assert!(lean(0.1) < lean(0.5) && lean(0.5) < lean(1.0));
        // Only x and y scale: z stays, then the result is normalized.
        let (x, z) = (230.0 / 127.5 - 1.0, 180.0 / 127.5 - 1.0);
        let expected = 0.1 * x / ((0.1 * x as f32).powi(2) + z * z).sqrt() * 127.5;
        assert!((lean(0.1) - expected).abs() < 1.0, "{} vs {expected}", lean(0.1));
    }

    #[test]
    fn a_normal_map_with_nothing_to_do_is_left_as_it_is() {
        let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 40]));
        adjust_normal_map(&mut img, [false, false], 1.0);
        assert_eq!(img.get_pixel(0, 0).0, [10, 20, 30, 40]);
    }

    #[test]
    fn a_roughness_map_alone_keeps_the_constant_metallic() {
        let dir = std::env::temp_dir().join(format!("sensesim_pack_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rough.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([90, 90, 90, 255])).save(&file).unwrap();
        let job = TexJob::Pack(None, Some((file, 0)), None);
        let out = job.run(64, (1, 1)).unwrap();
        let data = out.data.unwrap();
        assert_eq!(&data[..4], &[255, 90, 255, 255], "no occlusion, the map's roughness, metallic left to its constant");
    }

    #[test]
    fn udim_tiles_land_where_st_will_read_them() {
        let dir = std::env::temp_dir().join(format!("sensesim_udim_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (tile, shade) in [(1001, 10u8), (1002, 20), (1011, 30)] {
            image::RgbaImage::from_pixel(4, 4, image::Rgba([shade, shade, shade, 255])).save(dir.join(format!("t.{tile}.png"))).unwrap();
        }
        let img = open_scaled(&dir.join("t.<UDIM>.png"), 4, (2, 2)).unwrap();
        assert_eq!(img.dimensions(), (8, 8));
        // Images start at the top; USD's first row is at the bottom.
        assert_eq!(img.get_pixel(1, 6).0[0], 10, "1001: bottom left");
        assert_eq!(img.get_pixel(6, 6).0[0], 20, "1002: bottom right");
        assert_eq!(img.get_pixel(1, 1).0[0], 30, "1011: top left");
    }

    #[test]
    fn a_plain_texture_fills_every_tile() {
        let dir = std::env::temp_dir().join(format!("sensesim_udim_plain_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plain.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([77, 77, 77, 255])).save(&file).unwrap();
        let img = open_scaled(&file, 4, (2, 1)).unwrap();
        assert_eq!(img.dimensions(), (8, 4));
        assert!(img.pixels().all(|p| p.0[0] == 77));
    }

    #[test]
    fn glass_is_see_through_and_keeps_its_reflections() {
        let (color, mode) = surface_color(&prepared(MaterialData { glass: true, base_color: [1.0; 3], ..default_material() }));
        assert!(matches!(mode, AlphaMode::Premultiplied));
        assert!(color.alpha() <= 0.05, "clear glass barely covers: {}", color.alpha());
        assert!(color.to_linear().red <= 0.05, "and adds next to nothing of its own");
        let dark = surface_color(&prepared(MaterialData { glass: true, base_color: [0.03; 3], ..default_material() })).0;
        assert!(dark.alpha() > 0.8, "dark glass mostly covers: {}", dark.alpha());
    }

    #[test]
    fn a_glass_color_map_sets_its_cover_pixel_by_pixel() {
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 0, image::Rgba([20, 20, 20, 255]));
        glass_map(&mut img, true);
        let (clear, dark) = (img.get_pixel(0, 0).0, img.get_pixel(1, 0).0);
        assert!(clear[3] < 20, "a clear pixel barely covers: {clear:?}");
        assert!(dark[3] > 220, "a dark pixel mostly covers: {dark:?}");
        assert!(dark[0] < 40, "and stays dark: {dark:?}");
    }

    #[test]
    fn glass_with_a_color_map_leaves_it_in_charge() {
        let data = MaterialData { glass: true, base_color: [1.0; 3], ..default_material() };
        let (color, mode) = surface_color(&PreparedMaterial { base: Some(0), ..prepared(data) });
        assert!(matches!(mode, AlphaMode::Premultiplied));
        assert_eq!(color.alpha(), 1.0);
        assert_eq!(color.to_linear().red, 1.0);
    }

    #[test]
    fn solid_materials_stay_opaque_and_cutouts_keep_their_shapes() {
        assert!(matches!(surface_color(&prepared(default_material())).1, AlphaMode::Opaque));
        let cutout = MaterialData { opacity_threshold: 0.5, ..default_material() };
        assert!(matches!(surface_color(&prepared(cutout)).1, AlphaMode::Mask(_)));
        assert!(!is_see_through(&MaterialData { opacity: 0.3, opacity_threshold: 0.5, ..default_material() }));
        assert!(is_see_through(&MaterialData { opacity: 0.3, ..default_material() }));
    }

    #[test]
    fn specular_level_means_the_same_reflection_in_bevy() {
        let material = |level: f32| standard_material(&prepared(MaterialData { specular_level: level, ..default_material() }), &|_| None);
        assert!((material(0.5).reflectance - 0.5).abs() < 1e-5, "Bevy's default is OmniPBR's default");
        let f0 = 0.16 * material(1.0).reflectance.powi(2);
        assert!((f0 - 0.08).abs() < 1e-5, "{f0}");
    }

    fn default_material() -> MaterialData {
        MaterialData::default()
    }
}
