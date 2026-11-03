//! Loads a composed USD stage into plain mesh/material data, independent of Bevy.
//!
//! Handles what SimReady assets use: Mesh prims (with GeomSubset material
//! partitions), OmniPBR and OmniGlass materials (with UsdPreviewSurface graphs as
//! the fallback), UsdUVTexture + UsdTransform2d, UDIM texture sets, instancing via
//! instance proxies, purpose and visibility filtering, upAxis and metersPerUnit.
//!
//! A SimReady material usually carries both an MDL shader and a preview surface. The
//! MDL shader is the real material, the one the asset was made and reviewed with. The
//! preview is a converter's rough copy of it: glass comes out solid, and the normal map
//! direction and strength and the roughness settings are lost. So MDL is read first.

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
    /// How far the normal map tilts the surface; 1 is as painted.
    pub normal_strength: f32,
    /// The map's X and Y point against OpenGL's convention, which the renderer expects.
    /// DirectX maps flip Y.
    pub normal_flip: [bool; 2],
    pub emissive: [f32; 3],
    pub emissive_tex: Option<TexRef>,
    pub opacity: f32,
    pub opacity_tex: Option<TexRef>,
    pub opacity_threshold: f32,
    /// Strength of reflections head-on, as OmniPBR's `specular_level`: 0.5 reflects 4 %.
    pub specular_level: f32,
    /// See-through by nature, whatever `opacity` says: `base_color` is the glass's tint.
    pub glass: bool,
    /// Texture transform in USD's bottom-left UV space: st' = matrix * st + translate.
    /// The matrix is column-major.
    pub uv_matrix: [f32; 4],
    pub uv_translate: [f32; 2],
    /// Columns and rows of the texture's UDIM tiles; (1, 1) for a plain texture.
    pub udim_grid: (u32, u32),
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
            normal_strength: 1.0,
            normal_flip: [false, false],
            emissive: [0.0; 3],
            emissive_tex: None,
            opacity: 1.0,
            opacity_tex: None,
            opacity_threshold: 0.0,
            specular_level: 0.5,
            glass: false,
            uv_matrix: [1.0, 0.0, 0.0, 1.0],
            uv_translate: [0.0, 0.0],
            udim_grid: (1, 1),
        }
    }
}

impl MaterialData {
    fn textures(&self) -> impl Iterator<Item = &TexRef> {
        [&self.base_color_tex, &self.metallic_tex, &self.roughness_tex, &self.occlusion_tex, &self.normal_tex, &self.emissive_tex, &self.opacity_tex]
            .into_iter()
            .flatten()
    }

    /// Lays UDIM tiles out in one image: st then has to shrink to fit it.
    fn fit_udim_tiles(&mut self) {
        let grid = self.textures().filter_map(|t| udim_grid(&t.file)).fold((1, 1), |a, b| (a.0.max(b.0), a.1.max(b.1)));
        if grid == (1, 1) {
            return;
        }
        self.udim_grid = grid;
        let (u, v) = (1.0 / grid.0 as f32, 1.0 / grid.1 as f32);
        let m = self.uv_matrix;
        self.uv_matrix = [m[0] * u, m[1] * v, m[2] * u, m[3] * v];
        self.uv_translate = [self.uv_translate[0] * u, self.uv_translate[1] * v];
    }
}

/// `<UDIM>` in a texture path stands for a tile number: 1001 + column + 10 * row.
pub fn udim_tile(pattern: &Path, column: u32, row: u32) -> PathBuf {
    let name = pattern.file_name().unwrap_or_default().to_string_lossy().replace("<UDIM>", &(1001 + column + 10 * row).to_string());
    pattern.with_file_name(name)
}

/// Columns and rows of the UDIM tiles on disk for a `<UDIM>` path.
pub fn udim_grid(pattern: &Path) -> Option<(u32, u32)> {
    let name = pattern.file_name()?.to_str()?;
    let (head, tail) = name.split_once("<UDIM>")?;
    let (mut columns, mut rows) = (1, 1);
    for e in std::fs::read_dir(pattern.parent()?).ok()?.flatten() {
        let file = e.file_name();
        let Some(tile) = file.to_str().and_then(|f| f.strip_prefix(head)).and_then(|f| f.strip_suffix(tail)).and_then(|t| t.parse::<u32>().ok()) else { continue };
        if (1001..=1100).contains(&tile) {
            columns = columns.max((tile - 1001) % 10 + 1);
            rows = rows.max((tile - 1001) / 10 + 1);
        }
    }
    Some((columns, rows))
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

    /// Follows an input's connections to the UsdUVTexture feeding it, through any
    /// material or NodeGraph interface inputs on the way.
    fn uv_texture(&self, shader: &usd::Prim, input: &str) -> Option<(usd::Prim, String)> {
        let mut attr = shader.attribute(format!("inputs:{input}").as_str());
        for _ in 0..4 {
            let (src, output) = connected_source(&attr)?;
            let src_prim = self.stage.prim(src).ok()?;
            if self.shader_id(&src_prim).as_deref() == Some("UsdUVTexture") {
                return Some((src_prim, output));
            }
            attr = src_prim.attribute(output.as_str());
        }
        None
    }

    /// The texture file behind an input, and the UsdTransform2d feeding its st.
    fn texture_input(&self, shader: &usd::Prim, input: &str, uv: &mut Option<([f32; 2], [f32; 2], f32)>) -> Option<TexRef> {
        let (src_prim, output) = self.uv_texture(shader, input)?;
        let file = texture_file(&src_prim.attribute("inputs:file"))?;
        let cs = attr_str(&src_prim, "inputs:sourceColorSpace").unwrap_or_else(|| "auto".into());
        if uv.is_none() {
            *uv = self.uv_transform(&src_prim);
        }
        Some(TexRef { file, channel: channel_of(&output), srgb: cs == "sRGB" || (cs == "auto" && matches!(input, "diffuseColor" | "emissiveColor")) })
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

    /// An MDL input's attribute, or what it's connected to on the material.
    fn mdl_input(&self, shader: &usd::Prim, input: &str) -> usd::Attribute {
        let mut attr = shader.attribute(format!("inputs:{input}").as_str());
        for _ in 0..4 {
            let Some((src, output)) = connected_source(&attr) else { break };
            let Ok(p) = self.stage.prim(src) else { break };
            attr = p.attribute(output.as_str());
        }
        attr
    }

    fn mdl_float(&self, shader: &usd::Prim, input: &str, default: f32) -> f32 {
        value(&self.mdl_input(shader, input)).as_ref().and_then(as_f32).unwrap_or(default)
    }

    fn mdl_flag(&self, shader: &usd::Prim, input: &str, default: bool) -> bool {
        self.mdl_float(shader, input, default as u8 as f32) > 0.5
    }

    fn mdl_color(&self, shader: &usd::Prim, input: &str, default: [f32; 3]) -> [f32; 3] {
        value(&self.mdl_input(shader, input)).as_ref().and_then(as_vec3).unwrap_or(default)
    }

    fn mdl_vec2(&self, shader: &usd::Prim, input: &str, default: [f32; 2]) -> [f32; 2] {
        value(&self.mdl_input(shader, input)).as_ref().and_then(as_vec2).unwrap_or(default)
    }

    /// A texture input of an MDL shader. Its `colorSpace` metadata, when authored,
    /// beats the usual color space for this kind of map.
    fn mdl_texture(&self, shader: &usd::Prim, input: &str, srgb: bool) -> Option<TexRef> {
        let attr = self.mdl_input(shader, input);
        let file = texture_file(&attr)?;
        let srgb = match attr.get_metadata::<Value>("colorSpace").ok().flatten().as_ref().and_then(as_str).as_deref() {
            Some("sRGB") => true,
            Some("raw") => false,
            _ => srgb,
        };
        Some(TexRef { file, channel: 0, srgb })
    }

    fn surface_shader(&self, mat: &usd::Prim, output: &str) -> Option<usd::Prim> {
        let mut attr = mat.attribute(output);
        for _ in 0..4 {
            let (src, output) = connected_source(&attr)?;
            let p = self.stage.prim(src).ok()?;
            if p.type_name().ok().flatten().is_some_and(|t| t.as_str() == "Shader") {
                return Some(p);
            }
            attr = p.attribute(output.as_str());
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
        let known = |id: &str| id.contains("OmniPBR") || id.contains("OmniGlass");
        let mdl = self
            .surface_shader(&mat, "outputs:mdl:surface")
            .filter(|s| self.shader_id(s).is_some_and(|id| known(&id)))
            .or_else(|| self.find_child_shader(&mat, known));
        let preview = self
            .surface_shader(&mat, "outputs:surface")
            .filter(|s| self.shader_id(s).as_deref() == Some("UsdPreviewSurface"))
            .or_else(|| self.find_child_shader(&mat, |id| id == "UsdPreviewSurface"));
        if let Some(s) = mdl {
            if self.shader_id(&s).is_some_and(|id| id.contains("OmniGlass")) {
                self.read_omni_glass(&s, &mut m);
            } else {
                self.read_omni_pbr(&s, &mut m);
            }
        } else if let Some(s) = preview {
            self.read_preview_surface(&s, &mut m);
        } else {
            // Some other MDL material: most share OmniPBR's input names.
            let s = self.find_child_shader(&mat, |id| id.starts_with("mdl:"))?;
            self.read_omni_pbr(&s, &mut m);
        }
        m.fit_udim_tiles();
        Some(m)
    }

    fn read_preview_surface(&self, s: &usd::Prim, m: &mut MaterialData) {
        let mut uv = None;
        if let Some(v) = self.input_value(s, "diffuseColor").as_ref().and_then(as_vec3) {
            m.base_color = v;
        }
        m.base_color_tex = self.texture_input(s, "diffuseColor", &mut uv);
        if m.base_color_tex.is_some() {
            // UsdUVTexture's scale tints what it reads.
            m.base_color = self.uv_texture(s, "diffuseColor").and_then(|(t, _)| texture_scale(&t)).unwrap_or([1.0; 3]);
        }
        m.metallic = self.input_value(s, "metallic").as_ref().and_then(as_f32).unwrap_or(0.0);
        m.metallic_tex = self.texture_input(s, "metallic", &mut uv);
        m.roughness = self.input_value(s, "roughness").as_ref().and_then(as_f32).unwrap_or(0.5);
        m.roughness_tex = self.texture_input(s, "roughness", &mut uv);
        m.occlusion_tex = self.texture_input(s, "occlusion", &mut uv);
        m.normal_tex = self.texture_input(s, "normal", &mut uv);
        // The usual scale of (2, 2, 2) maps a texel to -1..1; a negative one points that axis the other way.
        if let Some(scale) = self.uv_texture(s, "normal").and_then(|(t, _)| texture_scale(&t)) {
            m.normal_flip = [scale[0] < 0.0, scale[1] < 0.0];
        }
        if let Some(v) = self.input_value(s, "emissiveColor").as_ref().and_then(as_vec3) {
            m.emissive = v;
        }
        m.emissive_tex = self.texture_input(s, "emissiveColor", &mut uv);
        m.opacity = self.input_value(s, "opacity").as_ref().and_then(as_f32).unwrap_or(1.0);
        m.opacity_tex = self.texture_input(s, "opacity", &mut uv);
        m.opacity_threshold = self.input_value(s, "opacityThreshold").as_ref().and_then(as_f32).unwrap_or(0.0);
        if let Some((scale, translate, degrees)) = uv {
            // UsdTransform2d: st' = R * S * st + T.
            let (sin, cos) = degrees.to_radians().sin_cos();
            m.uv_matrix = [cos * scale[0], sin * scale[0], -sin * scale[1], cos * scale[1]];
            m.uv_translate = translate;
        }
        // SimReady ORM textures: occlusion lives in R of the roughness/metal map.
        if m.occlusion_tex.is_none()
            && let Some(t) = m.roughness_tex.as_ref().or(m.metallic_tex.as_ref())
            && is_orm_file(&t.file)
        {
            m.occlusion_tex = Some(TexRef { channel: 0, ..t.clone() });
        }
    }

    /// OmniPBR.mdl, whose inputs mean what `OmniPBR_ClearCoat.mdl` does with them.
    fn read_omni_pbr(&self, s: &usd::Prim, m: &mut MaterialData) {
        m.base_color_tex = self.mdl_texture(s, "diffuse_texture", true);
        let base = if m.base_color_tex.is_some() { [1.0; 3] } else { self.mdl_color(s, "diffuse_color_constant", [0.2; 3]) };
        let tint = self.mdl_color(s, "diffuse_tint", [1.0; 3]);
        m.base_color = [0, 1, 2].map(|i| base[i] * tint[i]);

        let orm = if self.mdl_flag(s, "enable_ORM_texture", false) { self.mdl_texture(s, "ORM_texture", false) } else { None };
        let map = |channel: usize, own: &str| orm.as_ref().map(|t| TexRef { channel, ..t.clone() }).or_else(|| self.mdl_texture(s, own, false));
        // Each map counts as far as its influence says. Assets only use 0 and 1, and a
        // material with influence 0 has a map it doesn't want: the constant is the look.
        m.roughness = self.mdl_float(s, "reflection_roughness_constant", 0.5);
        if self.mdl_float(s, "reflection_roughness_texture_influence", 0.0) >= 0.5 {
            m.roughness_tex = map(1, "reflectionroughness_texture");
        }
        m.metallic = self.mdl_float(s, "metallic_constant", 0.0);
        if self.mdl_float(s, "metallic_texture_influence", 0.0) >= 0.5 {
            m.metallic_tex = map(2, "metallic_texture");
        }
        // A path tracer finds occlusion itself, so MDL leaves `ao_to_diffuse` at 0. A
        // rasterizer doesn't, so the map is still worth using.
        m.occlusion_tex = map(0, "ao_texture");
        m.specular_level = self.mdl_float(s, "specular_level", 0.5);

        self.read_mdl_normal(s, m, "normalmap_texture", "bump_factor");

        // A preview surface may glow where the real material doesn't: this switch decides.
        if self.mdl_flag(s, "enable_emission", false) {
            let intensity = self.mdl_float(s, "emissive_intensity", 40.0);
            m.emissive_tex = self.mdl_texture(s, "emissive_color_texture", true);
            let color = if m.emissive_tex.is_some() { [1.0; 3] } else { self.mdl_color(s, "emissive_color", [1.0, 0.1, 0.1]) };
            m.emissive = color.map(|c| c * intensity);
        }
        if self.mdl_flag(s, "enable_opacity", false) {
            m.opacity_threshold = self.mdl_float(s, "opacity_threshold", 0.0);
            if self.mdl_flag(s, "enable_opacity_texture", false) {
                m.opacity_tex = self.mdl_texture(s, "opacity_texture", false);
                // `base::mono_alpha` is the first of the mono modes.
                if self.mdl_float(s, "opacity_mode", 1.0) == 0.0
                    && let Some(t) = &mut m.opacity_tex
                {
                    t.channel = 3;
                }
            }
            if m.opacity_tex.is_none() {
                m.opacity = self.mdl_float(s, "opacity_constant", 1.0);
            }
        }
        self.read_mdl_uv(s, m);
    }

    /// OmniGlass.mdl: a clear surface that tints what's behind it and reflects what's
    /// around it. Its `cutout_opacity` only counts if `enable_opacity` turns it on.
    fn read_omni_glass(&self, s: &usd::Prim, m: &mut MaterialData) {
        m.glass = true;
        m.base_color_tex = self.mdl_texture(s, "glass_color_texture", true);
        m.base_color = if m.base_color_tex.is_some() { [1.0; 3] } else { self.mdl_color(s, "glass_color", [1.0; 3]) };
        m.roughness = self.mdl_float(s, "frosting_roughness", 0.0);
        if self.mdl_float(s, "roughness_texture_influence", 1.0) >= 0.5 {
            m.roughness_tex = self.mdl_texture(s, "roughness_texture", false);
        }
        // Head-on reflection of a dielectric, as a specular level (0.08 at level 1).
        let ior = self.mdl_float(s, "glass_ior", 1.491);
        m.specular_level = ((ior - 1.0) / (ior + 1.0)).powi(2) / 0.08;
        self.read_mdl_normal(s, m, "normal_map_texture", "normal_map_strength");
        if self.mdl_flag(s, "enable_opacity", false) {
            m.opacity = self.mdl_float(s, "cutout_opacity", 1.0);
        }
        self.read_mdl_uv(s, m);
    }

    fn read_mdl_normal(&self, s: &usd::Prim, m: &mut MaterialData, texture: &str, strength: &str) {
        m.normal_strength = self.mdl_float(s, strength, 1.0);
        if m.normal_strength > 0.0 {
            m.normal_tex = self.mdl_texture(s, texture, false);
        }
        // NVIDIA's note on `flip_tangent_v`: "true for dx, false for opengl".
        m.normal_flip = [self.mdl_flag(s, "flip_tangent_u", false), self.mdl_flag(s, "flip_tangent_v", true)];
    }

    /// `rotation_translation_scale` in OmniPBR_ClearCoat.mdl: st' = T + S * R(-angle) * st.
    fn read_mdl_uv(&self, s: &usd::Prim, m: &mut MaterialData) {
        let scale = self.mdl_vec2(s, "texture_scale", [1.0, 1.0]);
        let (sin, cos) = self.mdl_float(s, "texture_rotate", 0.0).to_radians().sin_cos();
        m.uv_matrix = [scale[0] * cos, -scale[1] * sin, scale[0] * sin, scale[1] * cos];
        m.uv_translate = self.mdl_vec2(s, "texture_translate", [0.0, 0.0]);
    }
}

/// UsdUVTexture's `scale` input: what each channel it reads is multiplied by.
fn texture_scale(texture: &usd::Prim) -> Option<[f32; 3]> {
    match value(&texture.attribute("inputs:scale"))? {
        Value::Vec4f(v) => Some([v.x, v.y, v.z]),
        Value::Vec4d(v) => Some([v.x as f32, v.y as f32, v.z as f32]),
        _ => None,
    }
}

/// Whether a file name marks an occlusion/roughness/metal map, as in `t_cart_orm.png`.
/// Not just "has orm in it": "platform" and "uniform" do too.
fn is_orm_file(file: &Path) -> bool {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
    stem.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| word == "orm")
}

/// `path` relative to a directory, with `..` worked out here: Windows doesn't work it
/// out for a `\\?\` directory, and the resolver hands those out.
fn join_relative(dir: &Path, path: &Path) -> PathBuf {
    let mut out = dir.to_path_buf();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// The texture file an asset-valued attribute names.
fn texture_file(attr: &usd::Attribute) -> Option<PathBuf> {
    let asset = attr.get::<sdf::AssetPath>().ok().flatten()?;
    if asset.authored_path.is_empty() {
        return None;
    }
    if let Some(path) = asset.resolved_path().filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    // The resolver only finds files that exist, which leaves out `<UDIM>` patterns and
    // files gone from where they should be. Anchor those to the layer authoring them.
    let authored = PathBuf::from(&asset.authored_path);
    let Some(site) = attr.property_stack().ok().and_then(|stack| stack.into_iter().next()) else { return Some(authored) };
    let layer = PathBuf::from(site.layer.replace('/', std::path::MAIN_SEPARATOR_STR));
    let Some(dir) = layer.parent() else { return Some(authored) };
    let file = join_relative(dir, &authored);
    if file.exists() || asset.authored_path.contains("<UDIM>") {
        return Some(file);
    }
    // Packaged assets keep a second copy of their textures in `source_assets`, and a
    // download cut short can leave only that one.
    let copy = file.parent()?.parent()?.join("source_assets").join(file.file_name()?);
    Some(if copy.exists() { copy } else { file })
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
            // A subset with no binding of its own takes its mesh's, as any prim does.
            let mat = c
                .relationship("material:binding")
                .targets()
                .ok()
                .and_then(|t| t.into_iter().next())
                .and_then(|p| mats.material_index(&p))
                .or(group_mats[0]);
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
    let mirrored = determinant3(&world) < 0.0;
    for (g, mut b) in bufs.into_iter().enumerate() {
        if b.idx.is_empty() {
            continue;
        }
        orient_triangles(&b.pos, &b.nrm, &mut b.idx, mirrored);
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

fn determinant3(m: &[f32; 16]) -> f32 {
    m[0] * (m[5] * m[10] - m[6] * m[9]) - m[4] * (m[1] * m[10] - m[2] * m[9]) + m[8] * (m[1] * m[6] - m[2] * m[5])
}

/// Winds each triangle to agree with its normals, as seen in the world (a mirrored
/// transform turns winding around). USD meshes come with either winding. The renderer
/// tells which side of a surface it's looking at by winding and turns the normal to
/// face the viewer, so a disagreement would light the outside of a part as its inside.
fn orient_triangles(positions: &[[f32; 3]], normals: &[[f32; 3]], indices: &mut [u32], mirrored: bool) {
    for t in indices.chunks_exact_mut(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize);
        let face = cross(sub3(positions[b], positions[a]), sub3(positions[c], positions[a]));
        let dot: f32 = (0..3).map(|k| face[k] * (normals[a][k] + normals[b][k] + normals[c][k])).sum();
        if (dot < 0.0) != mirrored {
            t.swap(1, 2);
        }
    }
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
                m.glass = false;
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


#[cfg(test)]
mod tests {
    use super::*;

    /// A one-quad stage bound to a material with the given shaders, in a fresh folder.
    fn stage(name: &str, shaders: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sensesim_usd_load_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("payloads")).unwrap();
        std::fs::create_dir_all(dir.join("Textures")).unwrap();
        std::fs::create_dir_all(dir.join("source_assets")).unwrap();
        let usda = format!(
            r#"#usda 1.0
(
    metersPerUnit = 1
    upAxis = "Z"
)
def Mesh "M" (
    prepend apiSchemas = ["MaterialBindingAPI"]
)
{{
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2, 3]
    texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
        interpolation = "vertex"
    )
    rel material:binding = </Mat>
}}
def Material "Mat"
{{
{shaders}
}}
"#
        );
        let path = dir.join("payloads/stage.usda");
        std::fs::write(&path, usda).unwrap();
        path
    }

    fn only_material(path: &Path) -> MaterialData {
        let l = load(path).unwrap();
        l.materials[l.parts[0].material.unwrap()].clone()
    }

    const PREVIEW: &str = r#"
    token outputs:surface.connect = </Mat/Preview.outputs:surface>
    def Shader "Preview"
    {
        token info:id = "UsdPreviewSurface"
        color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
        float inputs:metallic = 0
        float inputs:roughness = 0.9
        float inputs:opacity = 1
        token outputs:surface
    }"#;

    #[test]
    fn omnipbr_beats_the_preview_surface() {
        let shaders = format!(
            r#"{PREVIEW}
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {{
        token info:id = "mdl:OmniPBR"
        color3f inputs:diffuse_color_constant = (0.5, 0.25, 1)
        color3f inputs:diffuse_tint = (0.5, 1, 1)
        float inputs:reflection_roughness_constant = 0.3
        float inputs:reflection_roughness_texture_influence = 0
        float inputs:metallic_constant = 1
        float inputs:specular_level = 1
        float inputs:bump_factor = 0.1
        bool inputs:flip_tangent_u = 1
        bool inputs:flip_tangent_v = 0
        float2 inputs:texture_scale = (3, 2)
        token outputs:out
    }}"#
        );
        let m = only_material(&stage("omnipbr", &shaders));
        assert_eq!(m.base_color, [0.25, 0.25, 1.0], "the constant times the tint");
        assert_eq!((m.roughness, m.metallic, m.specular_level), (0.3, 1.0, 1.0));
        assert_eq!(m.normal_strength, 0.1);
        assert_eq!(m.normal_flip, [true, false]);
        assert_eq!(m.uv_matrix, [3.0, 0.0, 0.0, 2.0]);
        assert!(!m.glass);
    }

    #[test]
    fn a_map_with_no_influence_is_not_used() {
        let path = stage(
            "influence",
            r#"
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {
        token info:id = "mdl:OmniPBR"
        bool inputs:enable_ORM_texture = 1
        asset inputs:ORM_texture = @../Textures/t_orm.png@
        float inputs:reflection_roughness_texture_influence = 0
        float inputs:metallic_texture_influence = 1
        token outputs:out
    }"#,
        );
        std::fs::write(path.parent().unwrap().parent().unwrap().join("Textures/t_orm.png"), b"").unwrap();
        let m = only_material(&path);
        assert!(m.roughness_tex.is_none());
        assert_eq!(m.roughness, 0.5, "OmniPBR's default constant");
        assert_eq!(m.metallic_tex.as_ref().unwrap().channel, 2);
        assert_eq!(m.occlusion_tex.as_ref().unwrap().channel, 0);
    }

    #[test]
    fn normal_maps_default_to_directx_in_omnipbr_and_opengl_in_a_preview_surface() {
        let omni = r#"
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {
        token info:id = "mdl:OmniPBR"
        token outputs:out
    }"#;
        assert_eq!(only_material(&stage("flip_omni", omni)).normal_flip, [false, true]);
        assert_eq!(only_material(&stage("flip_preview", PREVIEW)).normal_flip, [false, false]);
    }

    #[test]
    fn a_preview_surface_can_say_its_normal_map_is_directx() {
        let path = stage(
            "flip_scale",
            r#"
    token outputs:surface.connect = </Mat/Preview.outputs:surface>
    def Shader "Preview"
    {
        token info:id = "UsdPreviewSurface"
        normal3f inputs:normal.connect = </Mat/N.outputs:rgb>
        token outputs:surface
    }
    def Shader "N"
    {
        token info:id = "UsdUVTexture"
        asset inputs:file = @../Textures/n.png@
        float4 inputs:scale = (2, -2, 2, 1)
        float4 inputs:bias = (-1, 1, -1, 0)
        float3 outputs:rgb
    }"#,
        );
        std::fs::write(path.parent().unwrap().parent().unwrap().join("Textures/n.png"), b"").unwrap();
        assert_eq!(only_material(&path).normal_flip, [false, true]);
    }

    #[test]
    fn omniglass_is_glass_even_when_its_preview_says_opaque() {
        let shaders = format!(
            r#"{PREVIEW}
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {{
        token info:id = "mdl:OmniGlass"
        color3f inputs:glass_color = (0.9, 0.5, 0.3)
        float inputs:frosting_roughness = 0.2
        float inputs:glass_ior = 1.5
        float inputs:cutout_opacity = 0.05
        token outputs:out
    }}"#
        );
        let m = only_material(&stage("glass", &shaders));
        assert!(m.glass);
        assert_eq!(m.base_color, [0.9, 0.5, 0.3]);
        assert_eq!(m.roughness, 0.2);
        assert!((m.specular_level - 0.5).abs() < 1e-5, "ior 1.5 reflects 4 %: {}", m.specular_level);
        assert_eq!(m.opacity, 1.0, "cutout only counts when enable_opacity is on");
    }

    #[test]
    fn emission_needs_its_switch() {
        let off = format!(
            r#"{PREVIEW}
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {{
        token info:id = "mdl:OmniPBR"
        color3f inputs:emissive_color = (1, 1, 1)
        float inputs:emissive_intensity = 5
        token outputs:out
    }}"#
        );
        assert_eq!(only_material(&stage("emit_off", &off)).emissive, [0.0; 3]);
        let on = off.replace("float inputs:emissive_intensity = 5", "float inputs:emissive_intensity = 5\n        bool inputs:enable_emission = 1");
        assert_eq!(only_material(&stage("emit_on", &on)).emissive, [5.0; 3]);
    }

    #[test]
    fn udim_sets_are_found_and_st_shrinks_to_fit_them() {
        let path = stage(
            "udim",
            r#"
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {
        token info:id = "mdl:OmniPBR"
        asset inputs:diffuse_texture = @../Textures/t_alb.<UDIM>.png@
        float2 inputs:texture_scale = (2, 2)
        token outputs:out
    }"#,
        );
        let textures = path.parent().unwrap().parent().unwrap().join("Textures");
        for tile in [1001, 1002, 1003, 1011] {
            std::fs::write(textures.join(format!("t_alb.{tile}.png")), b"").unwrap();
        }
        let m = only_material(&path);
        assert_eq!(m.udim_grid, (3, 2));
        assert_eq!(m.uv_matrix, [2.0 / 3.0, 0.0, 0.0, 1.0]);
        let file = m.base_color_tex.unwrap().file;
        assert!(udim_tile(&file, 2, 0).exists(), "{}", file.display());
        assert!(udim_tile(&file, 0, 1).exists(), "{}", file.display());
    }

    #[test]
    fn a_texture_missing_from_textures_is_found_in_source_assets() {
        let path = stage(
            "fallback",
            r#"
    token outputs:mdl:surface.connect = </Mat/Omni.outputs:out>
    def Shader "Omni"
    {
        token info:id = "mdl:OmniPBR"
        asset inputs:diffuse_texture = @../Textures/t_alb.png@
        token outputs:out
    }"#,
        );
        let copy = path.parent().unwrap().parent().unwrap().join("source_assets/t_alb.png");
        std::fs::write(&copy, b"").unwrap();
        assert!(only_material(&path).base_color_tex.unwrap().file.exists());
    }

    #[test]
    fn a_subset_with_no_binding_takes_its_meshs_material() {
        let path = stage(
            "subset",
            r#"
    token outputs:surface.connect = </Mat/Preview.outputs:surface>
    def Shader "Preview"
    {
        token info:id = "UsdPreviewSurface"
        color3f inputs:diffuseColor = (0.75, 0, 0)
        token outputs:surface
    }"#,
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let subset = r#"rel material:binding = </Mat>
    def GeomSubset "unbound"
    {
        uniform token elementType = "face"
        uniform token familyName = "materialBind"
        int[] indices = [0]
    }"#;
        std::fs::write(&path, text.replace("rel material:binding = </Mat>", subset)).unwrap();
        let l = load(&path).unwrap();
        assert!(!l.parts.is_empty());
        for p in &l.parts {
            assert_eq!(l.materials[p.material.expect("a material")].base_color, [0.75, 0.0, 0.0]);
        }
    }

    #[test]
    fn orm_files_are_told_by_their_name_not_a_substring() {
        for yes in ["t_cart_orm.png", "T_Metal_ORM.png", "t_orm_01.png"] {
            assert!(is_orm_file(Path::new(yes)), "{yes}");
        }
        for no in ["t_platform_rough.png", "t_uniform.png", "t_normal.png"] {
            assert!(!is_orm_file(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn triangles_are_wound_to_match_their_normals() {
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let up = [[0.0, 0.0, 1.0]; 3];
        let down = [[0.0, 0.0, -1.0]; 3];
        let mut i = [0, 1, 2];
        orient_triangles(&pos, &up, &mut i, false);
        assert_eq!(i, [0, 1, 2], "already agrees");
        orient_triangles(&pos, &down, &mut i, false);
        assert_eq!(i, [0, 2, 1]);
        let mut i = [0, 1, 2];
        orient_triangles(&pos, &up, &mut i, true);
        assert_eq!(i, [0, 2, 1], "a mirror turns the winding around");
    }
}