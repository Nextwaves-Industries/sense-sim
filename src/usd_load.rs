//! Loads a composed USD stage into plain mesh/material data, independent of Bevy.
//!
//! Handles what SimReady assets use: Mesh prims (with GeomSubset material
//! partitions), UsdPreviewSurface material graphs (with OmniPBR as a fallback),
//! UsdUVTexture + UsdTransform2d, instancing via instance proxies, purpose and
//! visibility filtering, upAxis and metersPerUnit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use openusd::sdf::{self, Value};
use openusd::usd;
use openusd_schemas::geom::XformCache;

/// One drawable piece: triangle soup with a single material.
pub struct MeshPart {
    pub name: String,
    pub prim_path: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Vertex colors from primvars:displayColor (linear), if authored.
    pub colors: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
    pub material: Option<usize>,
    /// Column-major world matrix (stage space, before up-axis conversion).
    pub world: [f32; 16],
    pub double_sided: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TexRef {
    pub file: PathBuf,
    /// Which channel feeds a scalar input: 0=r 1=g 2=b 3=a.
    pub channel: usize,
    pub srgb: bool,
}

#[derive(Clone, Debug)]
pub struct MaterialData {
    pub name: String,
    pub base_color: [f32; 3],
    pub base_color_tex: Option<TexRef>,
    pub metallic: f32,
    pub metallic_tex: Option<TexRef>,
    pub roughness: f32,
    pub roughness_tex: Option<TexRef>,
    pub occlusion_tex: Option<TexRef>,
    pub normal_tex: Option<TexRef>,
    pub emissive: [f32; 3],
    pub emissive_tex: Option<TexRef>,
    pub opacity: f32,
    pub opacity_tex: Option<TexRef>,
    pub opacity_threshold: f32,
    pub uv_scale: [f32; 2],
    pub uv_translate: [f32; 2],
    pub uv_rotation_deg: f32,
}

impl Default for MaterialData {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: [0.18, 0.18, 0.18],
            base_color_tex: None,
            metallic: 0.0,
            metallic_tex: None,
            roughness: 0.5,
            roughness_tex: None,
            occlusion_tex: None,
            normal_tex: None,
            emissive: [0.0; 3],
            emissive_tex: None,
            opacity: 1.0,
            opacity_tex: None,
            opacity_threshold: 0.0,
            uv_scale: [1.0, 1.0],
            uv_translate: [0.0, 0.0],
            uv_rotation_deg: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JointKind {
    Revolute,
    Prismatic,
    Fixed,
}

/// A physics joint, resolved into stage space at rest pose.
#[derive(Clone, Debug)]
pub struct JointData {
    pub name: String,
    pub kind: JointKind,
    pub body0: Option<String>,
    pub body1: Option<String>,
    /// Pivot point (stage space).
    pub anchor: [f32; 3],
    /// Unit axis (stage space).
    pub axis: [f32; 3],
    /// Degrees for revolute, stage units for prismatic; None when unlimited.
    pub lower: Option<f32>,
    pub upper: Option<f32>,
}

pub struct LoadedUsd {
    pub parts: Vec<MeshPart>,
    pub materials: Vec<MaterialData>,
    pub joints: Vec<JointData>,
    pub z_up: bool,
    pub meters_per_unit: f32,
    pub prim_count: usize,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Value helpers

fn value(attr: &usd::Attribute) -> Option<Value> {
    attr.get::<Value>().ok().flatten()
}

fn as_str(v: &Value) -> Option<String> {
    match v {
        Value::Token(t) => Some(t.as_str().to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Double(d) => Some(*d as f32),
        Value::Int(i) => Some(*i as f32),
        Value::Bool(b) => Some(*b as u8 as f32),
        _ => None,
    }
}

fn as_vec2(v: &Value) -> Option<[f32; 2]> {
    match v {
        Value::Vec2f(v) => Some([v.x, v.y]),
        Value::Vec2d(v) => Some([v.x as f32, v.y as f32]),
        _ => None,
    }
}

fn as_vec3(v: &Value) -> Option<[f32; 3]> {
    match v {
        Value::Vec3f(v) => Some([v.x, v.y, v.z]),
        Value::Vec3d(v) => Some([v.x as f32, v.y as f32, v.z as f32]),
        Value::Float(f) => Some([*f; 3]),
        _ => None,
    }
}

fn as_vec3_array(v: Value) -> Option<Vec<[f32; 3]>> {
    match v {
        Value::Vec3fVec(v) => Some(v.into_iter().map(|p| [p.x, p.y, p.z]).collect()),
        Value::Vec3dVec(v) => Some(v.into_iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect()),
        _ => None,
    }
}

fn as_vec2_array(v: Value) -> Option<Vec<[f32; 2]>> {
    match v {
        Value::Vec2fVec(v) => Some(v.into_iter().map(|p| [p.x, p.y]).collect()),
        Value::Vec2dVec(v) => Some(v.into_iter().map(|p| [p.x as f32, p.y as f32]).collect()),
        _ => None,
    }
}

fn as_int_array(v: Value) -> Option<Vec<i32>> {
    match v {
        Value::IntVec(v) => Some(v),
        Value::UintVec(v) => Some(v.into_iter().map(|i| i as i32).collect()),
        _ => None,
    }
}

fn attr_str(prim: &usd::Prim, name: &str) -> Option<String> {
    value(&prim.attribute(name)).as_ref().and_then(as_str)
}

fn interpolation(attr: &usd::Attribute) -> Option<String> {
    attr.get_metadata::<Value>("interpolation").ok().flatten().as_ref().and_then(as_str)
}

// ---------------------------------------------------------------------------
// Primvar expansion

#[derive(Clone, Copy, PartialEq)]
enum Interp {
    Constant,
    Uniform,
    Vertex,
    FaceVarying,
}

/// Picks the interpolation from metadata when present, otherwise infers it
/// from the element count (metadata reads can be absent for schema fallbacks).
fn infer_interp(meta: Option<String>, len: usize, n_points: usize, n_faces: usize, n_corners: usize) -> Interp {
    match meta.as_deref() {
        Some("constant") => return Interp::Constant,
        Some("uniform") => return Interp::Uniform,
        Some("vertex") | Some("varying") => return Interp::Vertex,
        Some("faceVarying") => return Interp::FaceVarying,
        _ => {}
    }
    if len == n_corners {
        Interp::FaceVarying
    } else if len == n_points {
        Interp::Vertex
    } else if len == n_faces {
        Interp::Uniform
    } else {
        Interp::Constant
    }
}

/// A primvar resolved to per-element values plus optional index indirection.
struct Primvar<T: Copy> {
    values: Vec<T>,
    indices: Option<Vec<i32>>,
    interp: Interp,
}

impl<T: Copy> Primvar<T> {
    fn len(&self) -> usize {
        self.indices.as_ref().map_or(self.values.len(), |i| i.len())
    }

    fn at(&self, face: usize, point: usize, corner: usize) -> Option<T> {
        let i = match self.interp {
            Interp::Constant => 0,
            Interp::Uniform => face,
            Interp::Vertex => point,
            Interp::FaceVarying => corner,
        };
        let i = match &self.indices {
            Some(idx) => *idx.get(i)? as usize,
            None => i,
        };
        self.values.get(i).copied()
    }
}

fn read_primvar<T: Copy>(
    prim: &usd::Prim,
    name: &str,
    conv: fn(Value) -> Option<Vec<T>>,
    counts: (usize, usize, usize),
) -> Option<Primvar<T>> {
    let attr = prim.attribute(name);
    let values = conv(value(&attr)?)?;
    if values.is_empty() {
        return None;
    }
    let indices = value(&prim.attribute(format!("{name}:indices").as_str())).and_then(as_int_array);
    let meta = interpolation(&attr);
    let len = indices.as_ref().map_or(values.len(), |i| i.len());
    let interp = infer_interp(meta, len, counts.0, counts.1, counts.2);
    let pv = Primvar { values, indices, interp };
    let _ = pv.len();
    Some(pv)
}

fn find_uv_primvar(prim: &usd::Prim, counts: (usize, usize, usize)) -> Option<Primvar<[f32; 2]>> {
    for name in ["primvars:st", "primvars:st0", "primvars:UVMap", "primvars:uv", "primvars:map1"] {
        if let Some(pv) = read_primvar(prim, name, as_vec2_array, counts) {
            return Some(pv);
        }
    }
    // Any other float2[] primvar.
    let names = prim.property_names().ok()?;
    for n in names {
        let n = n.as_str();
        if n.starts_with("primvars:") && !n.ends_with(":indices") {
            if let Some(pv) = read_primvar(prim, n, as_vec2_array, counts) {
                return Some(pv);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Materials

struct MaterialCtx<'a> {
    stage: &'a usd::Stage,
    index: HashMap<String, Option<usize>>,
    materials: Vec<MaterialData>,
}

fn connected_source(attr: &usd::Attribute) -> Option<(sdf::Path, String)> {
    let conns = attr.connections().ok()?;
    let c = conns.into_iter().next()?;
    let output = c.property_suffix().to_string();
    Some((c.prim_path(), output))
}

fn channel_of(output: &str) -> usize {
    match output.rsplit(':').next().unwrap_or("") {
        "g" => 1,
        "b" => 2,
        "a" => 3,
        _ => 0,
    }
}

impl<'a> MaterialCtx<'a> {
    fn shader_id(&self, prim: &usd::Prim) -> Option<String> {
        attr_str(prim, "info:id").or_else(|| attr_str(prim, "info:mdl:sourceAsset:subIdentifier"))
    }

    /// Follows an input's connection through UsdUVTexture (and the
    /// UsdTransform2d feeding its st) to a texture file.
    fn texture_input(&self, shader: &usd::Prim, input: &str, uv: &mut Option<([f32; 2], [f32; 2], f32)>) -> Option<TexRef> {
        let mut attr = shader.attribute(format!("inputs:{input}").as_str());
        // Hop through pass-through connections (e.g. material interface inputs).
        for _ in 0..4 {
            let (src, output) = connected_source(&attr)?;
            let src_prim = self.stage.prim(src).ok()?;
            let id = self.shader_id(&src_prim).unwrap_or_default();
            if id == "UsdUVTexture" {
                let asset = src_prim.attribute("inputs:file").get::<sdf::AssetPath>().ok().flatten()?;
                let file = asset
                    .resolved_path()
                    .map(PathBuf::from)
                    .or_else(|| Some(PathBuf::from(&asset.authored_path)))?;
                let cs = attr_str(&src_prim, "inputs:sourceColorSpace").unwrap_or_else(|| "auto".into());
                if uv.is_none() {
                    *uv = self.uv_transform(&src_prim);
                }
                return Some(TexRef {
                    file,
                    channel: channel_of(&output),
                    srgb: cs == "sRGB" || (cs == "auto" && input == "diffuseColor"),
                });
            }
            // Material-level or NodeGraph interface input: keep following.
            attr = src_prim.attribute(output.as_str());
        }
        None
    }

    fn uv_transform(&self, tex: &usd::Prim) -> Option<([f32; 2], [f32; 2], f32)> {
        let (src, _) = connected_source(&tex.attribute("inputs:st"))?;
        let p = self.stage.prim(src).ok()?;
        if self.shader_id(&p).as_deref() != Some("UsdTransform2d") {
            return None;
        }
        let scale = value(&p.attribute("inputs:scale")).as_ref().and_then(as_vec2).unwrap_or([1.0, 1.0]);
        let tr = value(&p.attribute("inputs:translation")).as_ref().and_then(as_vec2).unwrap_or([0.0, 0.0]);
        let rot = value(&p.attribute("inputs:rotation")).as_ref().and_then(as_f32).unwrap_or(0.0);
        Some((scale, tr, rot))
    }

    fn input_value(&self, shader: &usd::Prim, input: &str) -> Option<Value> {
        value(&shader.attribute(format!("inputs:{input}").as_str()))
    }

    fn find_surface_shader(&self, mat: &usd::Prim) -> Option<usd::Prim> {
        for out in ["outputs:surface", "outputs:mdl:surface"] {
            let mut attr = mat.attribute(out);
            for _ in 0..4 {
                let Some((src, output)) = connected_source(&attr) else { break };
                let Ok(p) = self.stage.prim(src) else { break };
                if p.type_name().ok().flatten().map(|t| t.as_str() == "Shader").unwrap_or(false) {
                    return Some(p);
                }
                attr = p.attribute(output.as_str());
            }
        }
        None
    }

    fn find_child_shader(&self, mat: &usd::Prim, pred: impl Fn(&str) -> bool) -> Option<usd::Prim> {
        let mut stack = mat.children().ok()?;
        while let Some(p) = stack.pop() {
            if let Some(id) = self.shader_id(&p)
                && pred(&id)
            {
                return Some(p);
            }
            if let Ok(c) = p.children() {
                stack.extend(c);
            }
        }
        None
    }

    fn material_index(&mut self, path: &sdf::Path) -> Option<usize> {
        let key = path.as_str().to_string();
        if let Some(i) = self.index.get(&key) {
            return *i;
        }
        let r = self.build_material(path);
        let idx = r.map(|m| {
            self.materials.push(m);
            self.materials.len() - 1
        });
        self.index.insert(key, idx);
        idx
    }

    fn build_material(&self, path: &sdf::Path) -> Option<MaterialData> {
        let mat = self.stage.prim(path.clone()).ok()?;
        let mut m = MaterialData { name: path.name().unwrap_or("material").to_string(), ..Default::default() };

        let preview = self
            .find_surface_shader(&mat)
            .filter(|s| self.shader_id(s).as_deref() == Some("UsdPreviewSurface"))
            .or_else(|| self.find_child_shader(&mat, |id| id == "UsdPreviewSurface"));

        if let Some(s) = preview {
            let mut uv = None;
            if let Some(v) = self.input_value(&s, "diffuseColor").as_ref().and_then(as_vec3) {
                m.base_color = v;
            }
            m.base_color_tex = self.texture_input(&s, "diffuseColor", &mut uv);
            if m.base_color_tex.is_some() {
                m.base_color = [1.0; 3];
            }
            m.metallic = self.input_value(&s, "metallic").as_ref().and_then(as_f32).unwrap_or(0.0);
            m.metallic_tex = self.texture_input(&s, "metallic", &mut uv);
            m.roughness = self.input_value(&s, "roughness").as_ref().and_then(as_f32).unwrap_or(0.5);
            m.roughness_tex = self.texture_input(&s, "roughness", &mut uv);
            m.occlusion_tex = self.texture_input(&s, "occlusion", &mut uv);
            m.normal_tex = self.texture_input(&s, "normal", &mut uv);
            if let Some(v) = self.input_value(&s, "emissiveColor").as_ref().and_then(as_vec3) {
                m.emissive = v;
            }
            m.emissive_tex = self.texture_input(&s, "emissiveColor", &mut uv);
            m.opacity = self.input_value(&s, "opacity").as_ref().and_then(as_f32).unwrap_or(1.0);
            m.opacity_tex = self.texture_input(&s, "opacity", &mut uv);
            m.opacity_threshold = self.input_value(&s, "opacityThreshold").as_ref().and_then(as_f32).unwrap_or(0.0);
            if let Some((s, t, r)) = uv {
                m.uv_scale = s;
                m.uv_translate = t;
                m.uv_rotation_deg = r;
            }
            // SimReady ORM textures: occlusion lives in R of the roughness/metal map.
            if m.occlusion_tex.is_none()
                && let Some(t) = m.roughness_tex.as_ref().or(m.metallic_tex.as_ref())
                && t.file.to_string_lossy().to_ascii_lowercase().contains("orm")
            {
                m.occlusion_tex = Some(TexRef { channel: 0, ..t.clone() });
            }
            return Some(m);
        }

        // OmniPBR / MDL fallback.
        let mdl = self.find_child_shader(&mat, |id| id.starts_with("mdl:") || id.starts_with("OmniPBR") || id.contains("OmniPBR"))?;
        let tex = |name: &str, channel: usize, srgb: bool| -> Option<TexRef> {
            let a = mdl.attribute(format!("inputs:{name}").as_str()).get::<sdf::AssetPath>().ok().flatten()?;
            let file = a.resolved_path().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&a.authored_path));
            if a.authored_path.is_empty() {
                return None;
            }
            Some(TexRef { file, channel, srgb })
        };
        if let Some(c) = self.input_value(&mdl, "diffuse_color_constant").as_ref().and_then(as_vec3) {
            m.base_color = c;
        }
        m.base_color_tex = tex("diffuse_texture", 0, true);
        if m.base_color_tex.is_some() {
            m.base_color = [1.0; 3];
        }
        if let Some(orm) = tex("ORM_texture", 0, false) {
            m.occlusion_tex = Some(TexRef { channel: 0, ..orm.clone() });
            m.roughness_tex = Some(TexRef { channel: 1, ..orm.clone() });
            m.metallic_tex = Some(TexRef { channel: 2, ..orm });
            m.roughness = 1.0;
            m.metallic = 1.0;
        } else {
            m.roughness = self.input_value(&mdl, "reflection_roughness_constant").as_ref().and_then(as_f32).unwrap_or(0.5);
            m.metallic = self.input_value(&mdl, "metallic_constant").as_ref().and_then(as_f32).unwrap_or(0.0);
        }
        m.normal_tex = tex("normalmap_texture", 0, false);
        if let Some(s) = self.input_value(&mdl, "texture_scale").as_ref().and_then(as_vec2) {
            m.uv_scale = s;
        }
        if let Some(o) = self.input_value(&mdl, "opacity_constant").as_ref().and_then(as_f32)
            && self.input_value(&mdl, "enable_opacity").as_ref().and_then(as_f32).unwrap_or(0.0) > 0.5
        {
            m.opacity = o;
        }
        Some(m)
    }
}

/// Resolves material:binding on the prim or its nearest ancestor.
fn bound_material(stage: &usd::Stage, prim: &usd::Prim) -> Option<sdf::Path> {
    let mut cur = Some(prim.clone());
    while let Some(p) = cur {
        if p.path().is_abs_root() {
            break;
        }
        if let Ok(t) = p.relationship("material:binding").targets()
            && let Some(first) = t.into_iter().next()
        {
            return Some(first);
        }
        cur = p.parent();
    }
    let _ = stage;
    None
}

fn is_hidden(prim: &usd::Prim) -> bool {
    let mut cur = Some(prim.clone());
    while let Some(p) = cur {
        if p.path().is_abs_root() {
            break;
        }
        if attr_str(&p, "visibility").as_deref() == Some("invisible") {
            return true;
        }
        if matches!(attr_str(&p, "purpose").as_deref(), Some("guide") | Some("proxy")) {
            return true;
        }
        cur = p.parent();
    }
    false
}

// ---------------------------------------------------------------------------
// Mesh building

fn mat4_cols(m: &openusd::gf::Matrix4d) -> [f32; 16] {
    // USD stores row-major with row vectors; that is exactly column-major for
    // column-vector math, so the array maps straight across.
    let mut out = [0.0f32; 16];
    for (o, v) in out.iter_mut().zip(m.0.iter()) {
        *o = *v as f32;
    }
    out
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 1.0, 0.0] }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn build_mesh_parts(
    stage: &usd::Stage,
    prim: &usd::Prim,
    world: [f32; 16],
    mats: &mut MaterialCtx,
    out: &mut Vec<MeshPart>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let path = prim.path().as_str().to_string();
    let Some(points) = value(&prim.attribute("points")).and_then(as_vec3_array) else {
        return Ok(());
    };
    let Some(fvi) = value(&prim.attribute("faceVertexIndices")).and_then(as_int_array) else {
        return Ok(());
    };
    let Some(fvc) = value(&prim.attribute("faceVertexCounts")).and_then(as_int_array) else {
        return Ok(());
    };
    if points.is_empty() || fvi.is_empty() {
        return Ok(());
    }
    let counts = (points.len(), fvc.len(), fvi.len());
    let normals = read_primvar(prim, "primvars:normals", as_vec3_array, counts)
        .or_else(|| read_primvar(prim, "normals", as_vec3_array, counts));
    let uvs = find_uv_primvar(prim, counts);
    let colors = read_primvar(prim, "primvars:displayColor", as_vec3_array, counts);
    let left_handed = attr_str(prim, "orientation").as_deref() == Some("leftHanded");
    let double_sided = value(&prim.attribute("doubleSided")).as_ref().and_then(as_f32).unwrap_or(0.0) > 0.5;
    let has_texcoords = uvs.is_some();

    // Face -> material group. Group 0 is the mesh's own binding.
    let mut face_group = vec![0usize; fvc.len()];
    let mut group_mats: Vec<Option<usize>> = vec![bound_material(stage, prim).and_then(|p| mats.material_index(&p))];
    if let Ok(children) = prim.children() {
        for c in children {
            if c.type_name().ok().flatten().map(|t| t.as_str() != "GeomSubset").unwrap_or(true) {
                continue;
            }
            let family = attr_str(&c, "familyName").unwrap_or_default();
            if !family.is_empty() && family != "materialBind" {
                continue;
            }
            let Some(idx) = value(&c.attribute("indices")).and_then(as_int_array) else { continue };
            let mat = c
                .relationship("material:binding")
                .targets()
                .ok()
                .and_then(|t| t.into_iter().next())
                .and_then(|p| mats.material_index(&p));
            group_mats.push(mat);
            let g = group_mats.len() - 1;
            for f in idx {
                if let Some(slot) = face_group.get_mut(f as usize) {
                    *slot = g;
                }
            }
        }
    }

    struct Buf {
        pos: Vec<[f32; 3]>,
        nrm: Vec<[f32; 3]>,
        uv: Vec<[f32; 2]>,
        col: Vec<[f32; 4]>,
        idx: Vec<u32>,
    }
    let mut bufs: Vec<Buf> = (0..group_mats.len())
        .map(|_| Buf { pos: vec![], nrm: vec![], uv: vec![], col: vec![], idx: vec![] })
        .collect();

    let mut corner = 0usize;
    let mut bad = 0usize;
    let mut poly: Vec<(usize, usize)> = Vec::with_capacity(8); // (point, corner)
    for (face, &n) in fvc.iter().enumerate() {
        let n = n.max(0) as usize;
        poly.clear();
        for k in 0..n {
            let ci = corner + k;
            match fvi.get(ci) {
                Some(&p) if (p as usize) < points.len() => poly.push((p as usize, ci)),
                _ => bad += 1,
            }
        }
        corner += n;
        if poly.len() < 3 {
            continue;
        }
        let b = &mut bufs[face_group[face]];
        let base = b.pos.len() as u32;
        // Face normal for meshes without authored normals.
        let p0 = points[poly[0].0];
        let fnrm = normalize(cross(sub3(points[poly[1].0], p0), sub3(points[poly[2].0], p0)));
        let fnrm = if left_handed { [-fnrm[0], -fnrm[1], -fnrm[2]] } else { fnrm };
        for &(p, ci) in &poly {
            b.pos.push(points[p]);
            let nn = normals.as_ref().and_then(|pv| pv.at(face, p, ci)).map(normalize).unwrap_or(fnrm);
            b.nrm.push(nn);
            let uv = uvs.as_ref().and_then(|pv| pv.at(face, p, ci)).unwrap_or([0.0, 0.0]);
            // USD st has a bottom-left origin; GPU textures are top-left.
            b.uv.push([uv[0], 1.0 - uv[1]]);
            if let Some(c) = colors.as_ref().and_then(|pv| pv.at(face, p, ci)) {
                b.col.push([srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2]), 1.0]);
            }
        }
        for k in 1..(poly.len() as u32 - 1) {
            if left_handed {
                b.idx.extend_from_slice(&[base, base + k + 1, base + k]);
            } else {
                b.idx.extend_from_slice(&[base, base + k, base + k + 1]);
            }
        }
    }
    if bad > 0 {
        warnings.push(format!("{path}: {bad} out-of-range face vertex indices skipped"));
    }

    let name = prim.path().name().unwrap_or("mesh").to_string();
    for (g, b) in bufs.into_iter().enumerate() {
        if b.idx.is_empty() {
            continue;
        }
        let has_colors = b.col.len() == b.pos.len();
        out.push(MeshPart {
            name: if g == 0 { name.clone() } else { format!("{name}[{g}]") },
            prim_path: path.clone(),
            positions: b.pos,
            normals: b.nrm,
            uvs: b.uv,
            colors: (has_colors && !has_texcoords || has_colors && group_mats[g].is_none()).then_some(b.col),
            indices: b.idx,
            material: group_mats[g],
            world,
            double_sided,
        });
    }
    Ok(())
}

/// A recolored variant of an asset: `<variant>.usda` references the source for
/// other USD tools, and this sidecar holds the look. Loading the variant loads
/// the source and applies it.
pub struct Overrides {
    pub source: PathBuf,
    color: Option<[f32; 3]>,
    roughness: Option<f32>,
    metallic: Option<f32>,
    opacity: Option<f32>,
    keep_texture: bool,
}

impl Overrides {
    fn sidecar(path: &Path) -> PathBuf {
        path.with_extension("look.json")
    }

    pub fn write(path: &Path, body: &serde_json::Value) -> std::io::Result<()> {
        std::fs::write(Self::sidecar(path), body.to_string())
    }

    pub fn read(path: &Path) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(Self::sidecar(path)).ok()?).ok()?;
        let num = |k: &str| v[k].as_f64().map(|x| x as f32);
        let color = v["color"].as_array().filter(|a| a.len() == 3).map(|a| [0, 1, 2].map(|i| a[i].as_f64().unwrap_or(0.0) as f32));
        Some(Self {
            source: PathBuf::from(v["source"].as_str()?),
            color,
            roughness: num("roughness"),
            metallic: num("metallic"),
            opacity: num("opacity"),
            keep_texture: v["keep_texture"].as_bool().unwrap_or(false),
        })
    }

    fn apply(&self, l: &mut LoadedUsd) {
        // Meshes with only display colors get a material to carry the look.
        if l.parts.iter().any(|p| p.material.is_none()) {
            let i = l.materials.len();
            let first = l.parts.iter().find(|p| p.material.is_none()).and_then(|p| p.colors.as_ref()).and_then(|c| c.first());
            let mut m = MaterialData { name: "look".into(), ..Default::default() };
            if let Some(c) = first {
                m.base_color = [c[0], c[1], c[2]];
            }
            l.materials.push(m);
            for p in l.parts.iter_mut().filter(|p| p.material.is_none()) {
                p.material = Some(i);
                p.colors = None;
            }
        }
        for m in &mut l.materials {
            if let Some(c) = self.color {
                if self.keep_texture && m.base_color_tex.is_some() {
                    m.base_color = [0, 1, 2].map(|i| m.base_color[i] * c[i]);
                } else {
                    m.base_color = c;
                    m.base_color_tex = None;
                }
            }
            if let Some(r) = self.roughness {
                m.roughness = r;
                m.roughness_tex = None;
            }
            if let Some(v) = self.metallic {
                m.metallic = v;
                m.metallic_tex = None;
            }
            if let Some(o) = self.opacity {
                m.opacity = o;
                m.opacity_tex = None;
                m.opacity_threshold = 0.0;
            }
        }
    }
}

pub fn load(path: &Path) -> Result<LoadedUsd> {
    if let Some(o) = Overrides::read(path) {
        let mut l = load_stage(&o.source).with_context(|| format!("loading the source of {}", path.display()))?;
        o.apply(&mut l);
        return Ok(l);
    }
    load_stage(path)
}

fn load_stage(path: &Path) -> Result<LoadedUsd> {
    let path_str = path.to_string_lossy().replace('\\', "/");
    let stage = usd::Stage::builder()
        .schema_registry(openusd_schemas::schema_registry())
        .open(&path_str)
        .with_context(|| format!("opening {path_str}"))?;

    let mut warnings: Vec<String> = stage
        .composition_errors()
        .iter()
        .take(10)
        .map(|e| format!("composition: {e:?}"))
        .collect();

    let up = stage.stage_metadata("upAxis").ok().flatten().as_ref().and_then(as_str);
    let z_up = up.as_deref() != Some("Y");
    let meters_per_unit = stage
        .stage_metadata("metersPerUnit")
        .ok()
        .flatten()
        .as_ref()
        .and_then(as_f32)
        .filter(|m| *m > 0.0)
        .unwrap_or(if up.is_some() { 1.0 } else { 0.01 });

    let mut paths = Vec::new();
    stage.traverse(usd::PrimPredicate::DEFAULT_PROXIES, |p| paths.push(p.clone()))?;

    let mut xc = XformCache::new(None);
    let mut mats = MaterialCtx { stage: &stage, index: HashMap::new(), materials: Vec::new() };
    let mut parts = Vec::new();
    let mut joints = Vec::new();
    for p in &paths {
        let Ok(prim) = stage.prim(p.clone()) else { continue };
        let ty = prim.type_name().ok().flatten();
        let ty_str = ty.as_ref().map(|t| t.as_str()).unwrap_or("");
        if let Some(kind) = match ty_str {
            "PhysicsRevoluteJoint" => Some(JointKind::Revolute),
            "PhysicsPrismaticJoint" => Some(JointKind::Prismatic),
            "PhysicsFixedJoint" => Some(JointKind::Fixed),
            _ => None,
        } {
            match read_joint(&stage, &prim, kind, &mut xc) {
                Some(j) => joints.push(j),
                None => warnings.push(format!("{p}: joint could not be resolved")),
            }
            continue;
        }
        if ty_str != "Mesh" {
            continue;
        }
        if is_hidden(&prim) {
            continue;
        }
        let world = match xc.local_to_world_transform(&prim) {
            Ok(m) => mat4_cols(&m),
            Err(e) => {
                warnings.push(format!("{p}: xform error {e:?}"));
                continue;
            }
        };
        if let Err(e) = build_mesh_parts(&stage, &prim, world, &mut mats, &mut parts, &mut warnings) {
            warnings.push(format!("{p}: {e:#}"));
        }
    }

    Ok(LoadedUsd {
        parts,
        joints,
        materials: mats.materials,
        z_up,
        meters_per_unit,
        prim_count: paths.len(),
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Joints

fn rel_target(prim: &usd::Prim, name: &str) -> Option<sdf::Path> {
    prim.relationship(name).targets().ok()?.into_iter().next()
}

fn read_joint(stage: &usd::Stage, prim: &usd::Prim, kind: JointKind, xc: &mut XformCache) -> Option<JointData> {
    let body0 = rel_target(prim, "physics:body0");
    let body1 = rel_target(prim, "physics:body1");
    // The joint frame is authored relative to body0, or body1 when anchored to the world.
    let (frame_body, pos_attr, rot_attr) = match (&body0, &body1) {
        (Some(b), _) => (Some(b.clone()), "physics:localPos0", "physics:localRot0"),
        (None, Some(b)) => (Some(b.clone()), "physics:localPos1", "physics:localRot1"),
        _ => (None, "physics:localPos0", "physics:localRot0"),
    };
    let body_world = match &frame_body {
        Some(b) => {
            let bp = stage.prim(b.clone()).ok()?;
            let m = xc.local_to_world_transform(&bp).ok()?;
            mat4_cols(&m)
        }
        None => {
            let mut id = [0.0f32; 16];
            id[0] = 1.0;
            id[5] = 1.0;
            id[10] = 1.0;
            id[15] = 1.0;
            id
        }
    };
    let pos = value(&prim.attribute(pos_attr)).as_ref().and_then(as_vec3).unwrap_or([0.0; 3]);
    let rot = match value(&prim.attribute(rot_attr)) {
        Some(Value::Quatf(q)) => [q.x, q.y, q.z, q.w],
        Some(Value::Quatd(q)) => [q.x as f32, q.y as f32, q.z as f32, q.w as f32],
        _ => [0.0, 0.0, 0.0, 1.0],
    };
    let axis_local = match attr_str(prim, "physics:axis").as_deref() {
        Some("Y") => [0.0, 1.0, 0.0],
        Some("Z") => [0.0, 0.0, 1.0],
        _ => [1.0, 0.0, 0.0],
    };
    let axis_frame = quat_rotate(rot, axis_local);
    let m = body_world;
    let tp = |v: [f32; 3], w: f32| -> [f32; 3] {
        [
            m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12] * w,
            m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13] * w,
            m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14] * w,
        ]
    };
    let anchor = tp(pos, 1.0);
    let axis = normalize(tp(axis_frame, 0.0));
    let limit = |name: &str| {
        value(&prim.attribute(name)).as_ref().and_then(as_f32).filter(|v| v.is_finite() && v.abs() < 1e6)
    };
    Some(JointData {
        name: prim.path().name().unwrap_or("joint").to_string(),
        kind,
        body0: body0.map(|p| p.as_str().to_string()),
        body1: body1.map(|p| p.as_str().to_string()),
        anchor,
        axis,
        lower: limit("physics:lowerLimit"),
        upper: limit("physics:upperLimit"),
    })
}

fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let u = [x, y, z];
    let t = cross(u, v);
    let t = [2.0 * t[0], 2.0 * t[1], 2.0 * t[2]];
    let c = cross(u, t);
    [v[0] + w * t[0] + c[0], v[1] + w * t[1] + c[1], v[2] + w * t[2] + c[2]]
}
