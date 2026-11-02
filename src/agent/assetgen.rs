//! Assets and materials the assistant makes itself: real PBR materials
//! (presets, colors, textures, crops of a reference photo), new assets composed
//! from primitives, photo-textured stand-ins for detections the library has no
//! match for, and recolored variants of existing assets.
//!
//! Everything is written as plain USD with UsdPreviewSurface materials, so it
//! goes through the normal asset path (selection, layouts, export, self-check).

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use bevy::math::{EulerRot, Quat, Vec2, Vec3};
use serde_json::{Value, json};

use super::images;
use super::vision::perceive::scenes;

const MAX_PARTS: usize = 5000;
/// Emissive strength at which a texture comes out at about its own brightness
/// under the viewport's exposure.
const UNLIT_GAIN: f32 = 1.0;

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into())).join("SenseSim")
}

pub fn custom_dir() -> PathBuf {
    data_dir().join("custom_assets")
}

fn hash_of(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

pub fn slug(name: &str) -> String {
    let s: String = name.trim().to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    s.split('_').filter(|t| !t.is_empty()).collect::<Vec<_>>().join("_")
}

// ---------------------------------------------------------------------------
// Materials

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A UsdPreviewSurface material. Colors are linear.
#[derive(Clone, Debug, PartialEq)]
pub struct Mat {
    pub color: [f32; 3],
    pub roughness: f32,
    pub metallic: f32,
    pub opacity: f32,
    pub emissive: [f32; 3],
    pub texture: Option<PathBuf>,
    /// Real-world size of one texture repeat in meters; 0 stretches it once
    /// over each face.
    pub tile_m: f32,
    /// A photo of the real thing already holds its light: show the texture as it
    /// is rather than lighting it a second time, which turns it flat and gray.
    pub unlit: bool,
}

impl Default for Mat {
    fn default() -> Self {
        Self { color: [0.6, 0.6, 0.6], roughness: 0.6, metallic: 0.0, opacity: 1.0, emissive: [0.0; 3], texture: None, tile_m: 0.0, unlit: false }
    }
}

fn linear(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(|c| srgb_to_linear(c as f32 / 255.0))
}

pub const PRESETS: [&str; 19] = [
    "white_paint",
    "wall_paint",
    "concrete",
    "polished_concrete",
    "vinyl_tile",
    "ceramic_tile",
    "ceiling_tile",
    "metal_deck",
    "wood",
    "plywood",
    "cardboard",
    "steel",
    "chrome",
    "aluminium",
    "black_metal",
    "plastic_white",
    "plastic_black",
    "glass",
    "led_panel",
];

impl Mat {
    pub fn from_srgb(rgb: [u8; 3]) -> Self {
        Self { color: linear(rgb), ..Default::default() }
    }

    pub fn preset(name: &str) -> Option<Self> {
        let m = |rgb: [u8; 3], roughness: f32, metallic: f32| Self { color: linear(rgb), roughness, metallic, ..Default::default() };
        Some(match name {
            "white_paint" => m([236, 237, 235], 0.55, 0.0),
            "wall_paint" => m([222, 220, 212], 0.8, 0.0),
            "concrete" => m([150, 149, 145], 0.9, 0.0),
            "polished_concrete" => m([168, 168, 164], 0.25, 0.0),
            "vinyl_tile" => Self { texture: grid_texture([214, 212, 204], [188, 186, 178]).ok(), tile_m: 0.6, ..m([255, 255, 255], 0.35, 0.0) },
            "ceramic_tile" => Self { texture: grid_texture([228, 226, 220], [150, 148, 142]).ok(), tile_m: 0.6, ..m([255, 255, 255], 0.2, 0.0) },
            "ceiling_tile" => Self { texture: grid_texture([238, 238, 234], [176, 176, 172]).ok(), tile_m: 0.6, ..m([255, 255, 255], 0.9, 0.0) },
            "metal_deck" => m([172, 176, 180], 0.5, 0.6),
            "wood" => m([150, 106, 66], 0.6, 0.0),
            "plywood" => m([196, 160, 112], 0.7, 0.0),
            "cardboard" => m([176, 136, 92], 0.85, 0.0),
            "steel" => m([168, 171, 175], 0.4, 1.0),
            "chrome" => m([230, 232, 235], 0.08, 1.0),
            "aluminium" => m([205, 208, 212], 0.35, 1.0),
            "black_metal" => m([34, 35, 37], 0.45, 0.6),
            "plastic_white" => m([240, 240, 238], 0.35, 0.0),
            "plastic_black" => m([24, 24, 26], 0.35, 0.0),
            "glass" => Self { opacity: 0.22, ..m([225, 238, 240], 0.05, 0.0) },
            "led_panel" => Self { emissive: [6.0, 6.0, 5.8], ..m([255, 255, 252], 0.4, 0.0) },
            _ => return None,
        })
    }

    /// Reads a material spec on top of `base`: a preset name, or an object with
    /// `preset`, `color`, `roughness`, `metallic`, `opacity`, `emissive`,
    /// `texture`, `tile_m` and `photo`.
    pub fn parse(v: &Value, base: Mat) -> Result<Self> {
        let preset = |n: &str| Mat::preset(n).ok_or_else(|| anyhow!("unknown material preset '{n}' (use one of {})", PRESETS.join(", ")));
        if let Some(n) = v.as_str() {
            return preset(n);
        }
        if !v.is_object() {
            return Ok(base);
        }
        let mut m = match v["preset"].as_str() {
            Some(n) => preset(n)?,
            None => base,
        };
        if !v["color"].is_null() {
            m.color = parse_color(&v["color"]).ok_or_else(|| anyhow!("color must be \"#rrggbb\" or [r, g, b] (0-1, sRGB)"))?;
        }
        if !v["emissive"].is_null() {
            m.emissive = parse_color(&v["emissive"]).ok_or_else(|| anyhow!("emissive must be \"#rrggbb\" or [r, g, b]"))?;
        }
        let unit = |k: &str| v[k].as_f64().map(|x| (x as f32).clamp(0.0, 1.0));
        m.roughness = unit("roughness").unwrap_or(m.roughness);
        m.metallic = unit("metallic").unwrap_or(m.metallic);
        m.opacity = unit("opacity").unwrap_or(m.opacity);
        m.tile_m = v["tile_m"].as_f64().map(|x| x.max(0.0) as f32).unwrap_or(m.tile_m);
        if let Some(p) = v["texture"].as_str() {
            let p = PathBuf::from(p);
            if !p.is_file() {
                bail!("texture file not found: {}", p.display());
            }
            m.texture = Some(p);
            m.color = [1.0; 3];
        }
        let photo = &v["photo"];
        if photo.is_object() {
            let (id, b) = photo_region(photo)?;
            if photo["as"].as_str() == Some("color") {
                m.color = linear(median_color(&id, b)?);
                m.texture = None;
            } else {
                m.texture = Some(crop_photo(&id, b)?);
                m.color = [1.0; 3];
                m.unlit = v["unlit"].as_bool().unwrap_or(true);
            }
        }
        Ok(m)
    }

    fn key(&self) -> String {
        format!("{self:?}")
    }
}

fn parse_color(v: &Value) -> Option<[f32; 3]> {
    if let Some(s) = v.as_str() {
        let s = s.trim_start_matches('#');
        if s.len() != 6 || !s.is_ascii() {
            return None;
        }
        let c = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        return Some(linear([c(0)?, c(2)?, c(4)?]));
    }
    let a = v.as_array().filter(|a| a.len() == 3)?;
    let c = |i: usize| a[i].as_f64().map(|x| srgb_to_linear((x as f32).clamp(0.0, 1.0)));
    Some([c(0)?, c(1)?, c(2)?])
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// A square tile with darker joints, written once.
fn grid_texture(fill: [u8; 3], joint: [u8; 3]) -> Result<PathBuf> {
    let dir = custom_dir().join("_textures");
    let path = dir.join(format!("grid_{}_{}.png", hex(fill).trim_start_matches('#'), hex(joint).trim_start_matches('#')));
    if !path.exists() {
        std::fs::create_dir_all(&dir)?;
        const N: u32 = 256;
        const LINE: u32 = 3;
        let img = image::RgbImage::from_fn(N, N, |x, y| image::Rgb(if x < LINE || y < LINE { joint } else { fill }));
        img.save(&path)?;
    }
    Ok(path)
}

/// `{image_id, box: [x0, y0, x1, y1]}` (normalized), or `{image_id, id}` for a
/// perceive_photo detection.
fn photo_region(v: &Value) -> Result<(String, [f32; 4])> {
    let id = v["image_id"].as_str().ok_or_else(|| anyhow!("photo needs image_id"))?.to_string();
    if let Some(a) = v["box"].as_array().filter(|a| a.len() == 4) {
        let c = |i: usize| a[i].as_f64().unwrap_or(0.0) as f32;
        return Ok((id, [c(0), c(1), c(2), c(3)]));
    }
    let det = v["id"].as_u64().ok_or_else(|| anyhow!("photo needs box [x0, y0, x1, y1] or a detection id"))? as usize;
    let scenes = scenes().lock().unwrap();
    let sc = scenes.get(&id).ok_or_else(|| anyhow!("no perceive_photo result for '{id}'"))?;
    let b = sc.objects.get(det).ok_or_else(|| anyhow!("no detection #{det} in '{id}'"))?.bbox;
    Ok((id, b))
}

fn photo_pixels(id: &str, b: [f32; 4]) -> Result<image::RgbImage> {
    let (data, _) = images::get(id).ok_or_else(|| anyhow!("unknown image_id '{id}'"))?;
    let img = image::load_from_memory(&data).context("decoding photo")?.to_rgb8();
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = (b[0].clamp(0.0, 1.0) * w) as u32;
    let y0 = (b[1].clamp(0.0, 1.0) * h) as u32;
    let x1 = (b[2].clamp(0.0, 1.0) * w) as u32;
    let y1 = (b[3].clamp(0.0, 1.0) * h) as u32;
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        bail!("the photo region {b:?} is empty");
    }
    Ok(image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image())
}

fn crop_photo(id: &str, b: [f32; 4]) -> Result<PathBuf> {
    let dir = custom_dir().join("_textures");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("photo_{}.png", hash_of(&format!("{id}{b:?}"))));
    let crop = photo_pixels(id, b)?;
    let crop = if crop.width().max(crop.height()) > 1024 { image::DynamicImage::ImageRgb8(crop).thumbnail(1024, 1024).to_rgb8() } else { crop };
    crop.save(&path)?;
    Ok(path)
}

/// Per-channel median of an image region: stable against labels and highlights.
pub fn median_rgb(img: &image::RgbImage) -> Option<[u8; 3]> {
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let mut ch: [Vec<u8>; 3] = Default::default();
    for p in img.pixels() {
        for c in 0..3 {
            ch[c].push(p[c]);
        }
    }
    Some([0, 1, 2].map(|c| {
        ch[c].sort_unstable();
        ch[c][ch[c].len() / 2]
    }))
}

fn median_color(id: &str, b: [f32; 4]) -> Result<[u8; 3]> {
    median_rgb(&photo_pixels(id, b)?).ok_or_else(|| anyhow!("empty photo region"))
}

// ---------------------------------------------------------------------------
// Geometry

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shape {
    Box,
    /// `size` = [x diameter, height, z diameter]; `top` scales the upper end
    /// (0 makes a cone).
    Cylinder { top: f32 },
    Sphere,
    /// One quad facing local +Z: `size` = [width, height, _].
    Panel,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub shape: Shape,
    pub size: Vec3,
    /// Center of the part.
    pub at: Vec3,
    pub rot: Quat,
    pub mat: Mat,
}

struct Poly {
    pos: Vec<Vec3>,
    nrm: Vec<Vec3>,
    st: Vec<Vec2>,
}

fn quad(c: Vec3, n: Vec3, u: Vec3, v: Vec3, su: f32, sv: f32, tile: f32) -> Poly {
    let (hu, hv) = (u * su * 0.5, v * sv * 0.5);
    let (tu, tv) = if tile > 0.0 { (su / tile, sv / tile) } else { (1.0, 1.0) };
    Poly {
        pos: vec![c - hu - hv, c + hu - hv, c + hu + hv, c - hu + hv],
        nrm: vec![n; 4],
        st: vec![Vec2::ZERO, Vec2::new(tu, 0.0), Vec2::new(tu, tv), Vec2::new(0.0, tv)],
    }
}

/// Polygons of a part in local space, counter-clockwise seen from outside.
fn polys(p: &Part) -> Vec<Poly> {
    let (s, tile) = (p.size, p.mat.tile_m);
    let h = s * 0.5;
    match p.shape {
        Shape::Panel => vec![quad(Vec3::ZERO, Vec3::Z, Vec3::X, Vec3::Y, s.x, s.y, tile)],
        Shape::Box => vec![
            quad(Vec3::Z * h.z, Vec3::Z, Vec3::X, Vec3::Y, s.x, s.y, tile),
            quad(-Vec3::Z * h.z, -Vec3::Z, -Vec3::X, Vec3::Y, s.x, s.y, tile),
            quad(Vec3::X * h.x, Vec3::X, -Vec3::Z, Vec3::Y, s.z, s.y, tile),
            quad(-Vec3::X * h.x, -Vec3::X, Vec3::Z, Vec3::Y, s.z, s.y, tile),
            quad(Vec3::Y * h.y, Vec3::Y, Vec3::X, -Vec3::Z, s.x, s.z, tile),
            quad(-Vec3::Y * h.y, -Vec3::Y, Vec3::X, Vec3::Z, s.x, s.z, tile),
        ],
        Shape::Cylinder { top } => {
            const N: usize = 20;
            let dir = |i: usize| {
                let a = i as f32 / N as f32 * std::f32::consts::TAU;
                Vec2::new(a.cos(), -a.sin())
            };
            let ring = |d: Vec2, y: f32, k: f32| Vec3::new(d.x * h.x * k, y, d.y * h.z * k);
            // Slope of a cone's side against its axis, for the side normals.
            let slope = (1.0 - top) * h.x.max(h.z) / s.y.max(1e-4);
            let normal = |d: Vec2| Vec3::new(d.x / h.x.max(1e-4), slope / h.x.max(h.z).max(1e-4), d.y / h.z.max(1e-4)).normalize_or_zero();
            let mut out = Vec::with_capacity(N + 2);
            for i in 0..N {
                let (a, b) = (dir(i), dir(i + 1));
                let (u0, u1) = (i as f32 / N as f32, (i + 1) as f32 / N as f32);
                out.push(Poly {
                    pos: vec![ring(a, -h.y, 1.0), ring(b, -h.y, 1.0), ring(b, h.y, top), ring(a, h.y, top)],
                    nrm: vec![normal(a), normal(b), normal(b), normal(a)],
                    st: vec![Vec2::new(u0, 0.0), Vec2::new(u1, 0.0), Vec2::new(u1, 1.0), Vec2::new(u0, 1.0)],
                });
            }
            let cap = |y: f32, k: f32, n: Vec3, rev: bool| {
                let mut ids: Vec<usize> = (0..N).collect();
                if rev {
                    ids.reverse();
                }
                let pos: Vec<Vec3> = ids.iter().map(|&i| ring(dir(i), y, k)).collect();
                let st = ids.iter().map(|&i| (dir(i) + Vec2::ONE) * 0.5).collect();
                Poly { nrm: vec![n; pos.len()], pos, st }
            };
            if top > 0.001 {
                out.push(cap(h.y, top, Vec3::Y, false));
            }
            out.push(cap(-h.y, 1.0, -Vec3::Y, true));
            out
        }
        Shape::Sphere => {
            const LON: usize = 20;
            const LAT: usize = 12;
            let pt = |i: usize, j: usize| {
                let a = i as f32 / LON as f32 * std::f32::consts::TAU;
                let b = (j as f32 / LAT as f32 - 0.5) * std::f32::consts::PI;
                Vec3::new(b.cos() * a.cos(), b.sin(), -b.cos() * a.sin())
            };
            let st = |i: usize, j: usize| Vec2::new(i as f32 / LON as f32, j as f32 / LAT as f32);
            let mut out = Vec::with_capacity(LON * LAT);
            for j in 0..LAT {
                for i in 0..LON {
                    let ids = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                    out.push(Poly {
                        pos: ids.iter().map(|&(a, b)| pt(a, b) * h).collect(),
                        nrm: ids.iter().map(|&(a, b)| (pt(a, b) / h.max(Vec3::splat(1e-4))).normalize_or_zero()).collect(),
                        st: ids.iter().map(|&(a, b)| st(a, b)).collect(),
                    });
                }
            }
            out
        }
    }
}

/// Bounds of the parts' polygons in asset space.
pub fn bounds(parts: &[Part]) -> Option<(Vec3, Vec3)> {
    let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in parts {
        for poly in polys(p) {
            for v in poly.pos {
                let w = p.at + p.rot * v;
                mn = mn.min(w);
                mx = mx.max(w);
            }
        }
    }
    (mn.x <= mx.x).then_some((mn, mx))
}

fn tuple(v: Vec3) -> String {
    format!("({:.5}, {:.5}, {:.5})", v.x, v.y, v.z)
}

/// The USD text of `parts`, plus the texture files to copy next to it as
/// (source, file name). Single-sided: meshes are closed or face one way on purpose.
fn usda_text(root: &str, parts: &[Part], offset: Vec3) -> (String, Vec<(PathBuf, String)>) {
    let mut mats: Vec<&Mat> = Vec::new();
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let mut body = String::new();
    for (k, p) in parts.iter().enumerate() {
        let mid = match mats.iter().position(|m| m.key() == p.mat.key()) {
            Some(i) => i,
            None => {
                mats.push(&p.mat);
                mats.len() - 1
            }
        };
        let (mut counts, mut idx, mut pts, mut nrm, mut st) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for poly in polys(p) {
            counts.push(poly.pos.len().to_string());
            for i in 0..poly.pos.len() {
                idx.push(pts.len().to_string());
                pts.push(tuple(p.at + p.rot * poly.pos[i] + offset));
                nrm.push(tuple(p.rot * poly.nrm[i]));
                st.push(format!("({:.4}, {:.4})", poly.st[i].x, poly.st[i].y));
            }
        }
        let _ = write!(
            body,
            r#"
    def Mesh "part_{k}"
    {{
        int[] faceVertexCounts = [{counts}]
        int[] faceVertexIndices = [{idx}]
        point3f[] points = [{pts}]
        normal3f[] normals = [{nrm}] (
            interpolation = "faceVarying"
        )
        texCoord2f[] primvars:st = [{st}] (
            interpolation = "faceVarying"
        )
        uniform token orientation = "rightHanded"
        uniform token subdivisionScheme = "none"
        uniform bool doubleSided = 0
        rel material:binding = </{root}/Looks/m{mid}>
    }}
"#,
            counts = counts.join(", "),
            idx = idx.join(", "),
            pts = pts.join(", "),
            nrm = nrm.join(", "),
            st = st.join(", "),
        );
    }
    let mut looks = String::new();
    for (i, m) in mats.iter().enumerate() {
        let col = |c: [f32; 3]| format!("({:.5}, {:.5}, {:.5})", c[0], c[1], c[2]);
        let path = format!("/{root}/Looks/m{i}");
        let mut tex = String::new();
        let mut diffuse = format!("color3f inputs:diffuseColor = {}", col(m.color));
        if let Some(t) = &m.texture {
            let ext = t.extension().and_then(|e| e.to_str()).unwrap_or("png");
            let name = format!("tex_{}.{ext}", hash_of(&t.to_string_lossy()));
            diffuse = if m.unlit {
                format!("color3f inputs:diffuseColor = (0, 0, 0)\n                color3f inputs:emissiveColor.connect = <{path}/tex.outputs:rgb>")
            } else {
                format!("color3f inputs:diffuseColor.connect = <{path}/tex.outputs:rgb>")
            };
            let _ = write!(
                tex,
                r#"
            def Shader "tex"
            {{
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @./{name}@
                float2 inputs:st.connect = <{path}/st.outputs:result>
                token inputs:sourceColorSpace = "sRGB"
                token inputs:wrapS = "repeat"
                token inputs:wrapT = "repeat"
                float3 outputs:rgb
            }}

            def Shader "st"
            {{
                uniform token info:id = "UsdPrimvarReader_float2"
                token inputs:varname = "st"
                float2 outputs:result
            }}
"#
            );
            if !files.iter().any(|(_, n)| *n == name) {
                files.push((t.clone(), name));
            }
        }
        let _ = write!(
            looks,
            r#"
        def Material "m{i}"
        {{
            token outputs:surface.connect = <{path}/surface.outputs:surface>

            def Shader "surface"
            {{
                uniform token info:id = "UsdPreviewSurface"
                {diffuse}
                color3f inputs:emissiveColor = {emissive}
                float inputs:roughness = {r:.3}
                float inputs:metallic = {mt:.3}
                float inputs:opacity = {o:.3}
                token outputs:surface
            }}
{tex}        }}
"#,
            emissive = col(if m.unlit && m.texture.is_some() { [UNLIT_GAIN; 3] } else { m.emissive }),
            r = m.roughness,
            mt = m.metallic,
            o = m.opacity,
        );
    }
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "{root}"
    metersPerUnit = 1
    upAxis = "Y"
)

def Xform "{root}" (
    kind = "component"
)
{{
    def Scope "Looks"
    {{{looks}    }}
{body}}}
"#
    );
    (text, files)
}

/// Writes `parts` to `path` if the content differs. Returns whether it changed.
pub fn write_usd(path: &Path, root: &str, parts: &[Part], offset: Vec3) -> Result<bool> {
    let (text, files) = usda_text(root, parts, offset);
    let dir = path.parent().context("asset path has no folder")?;
    std::fs::create_dir_all(dir)?;
    for (src, name) in &files {
        std::fs::copy(src, dir.join(name)).with_context(|| format!("copying texture {}", src.display()))?;
    }
    if std::fs::read_to_string(path).ok().as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    std::fs::write(path, text)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// create_asset

fn vec3_of(v: &Value) -> Option<Vec3> {
    let a = v.as_array().filter(|a| a.len() == 3)?;
    Some(Vec3::new(a[0].as_f64()? as f32, a[1].as_f64()? as f32, a[2].as_f64()? as f32))
}

fn parse_part(v: &Value, n: usize) -> Result<Part> {
    let shape = match v["shape"].as_str().unwrap_or("box") {
        "box" => Shape::Box,
        "cylinder" => Shape::Cylinder { top: v["top_scale"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32 },
        "cone" => Shape::Cylinder { top: 0.0 },
        "sphere" => Shape::Sphere,
        "panel" => Shape::Panel,
        other => bail!("part {n}: unknown shape '{other}' (box, cylinder, cone, sphere, panel)"),
    };
    let mut size = vec3_of(&v["size"]).ok_or_else(|| anyhow!("part {n}: size must be [x, y, z] in meters"))?;
    if shape == Shape::Panel {
        size.z = size.z.max(0.001);
    }
    if !size.is_finite() || size.min_element() <= 0.0 || size.max_element() > 500.0 {
        bail!("part {n}: size {size} must be positive and at most 500 m");
    }
    let at = vec3_of(&v["at"]).unwrap_or(Vec3::ZERO);
    if !at.is_finite() {
        bail!("part {n}: invalid position");
    }
    let r = vec3_of(&v["rotate_deg"]).unwrap_or(Vec3::ZERO);
    let rot = Quat::from_euler(EulerRot::YXZ, r.y.to_radians(), r.x.to_radians(), r.z.to_radians());
    let mat = Mat::parse(&v["material"], Mat::default()).with_context(|| format!("part {n}"))?;
    Ok(Part { shape, size, at, rot, mat })
}

/// A box of the detection's measured size in its photo color, with the photo
/// crop on the front. The front faces +Z, towards the photo camera at yaw 0.
fn parts_from_detection(v: &Value) -> Result<Vec<Part>> {
    let (id, bbox) = photo_region(v)?;
    let det = v["id"].as_u64().ok_or_else(|| anyhow!("from_detection needs a detection id"))? as usize;
    let (w, h) = {
        let scenes = scenes().lock().unwrap();
        let sc = scenes.get(&id).ok_or_else(|| anyhow!("no perceive_photo result for '{id}'"))?;
        let o = sc.objects.get(det).ok_or_else(|| anyhow!("no detection #{det} in '{id}'"))?;
        let guess = |k: &str| v[k].as_f64().filter(|x| *x > 0.0).map(|x| x as f32);
        let spot = super::placer::standin_spot(sc, o, guess("width_m"), guess("height_m")).ok_or_else(|| anyhow!("detection #{det} has no position yet: give it height_m, width_m, base_y or at_xz with set_photo_layout"))?;
        (spot.width.max(0.05), spot.height.max(0.05))
    };
    let d = v["depth_m"].as_f64().map(|x| x as f32).unwrap_or((w * 0.5).clamp(0.1, 1.0)).max(0.02);
    let body = Mat::from_srgb(median_color(&id, bbox)?);
    let face = Mat { texture: Some(crop_photo(&id, bbox)?), color: [1.0; 3], unlit: true, ..Default::default() };
    Ok(vec![
        Part { shape: Shape::Box, size: Vec3::new(w, h, d), at: Vec3::new(0.0, h * 0.5, 0.0), rot: Quat::IDENTITY, mat: body },
        Part { shape: Shape::Panel, size: Vec3::new(w, h, 0.001), at: Vec3::new(0.0, h * 0.5, d * 0.5 + 0.003), rot: Quat::IDENTITY, mat: face },
    ])
}

pub struct Created {
    pub slug: String,
    pub path: PathBuf,
    pub size: Vec3,
    pub parts: usize,
    /// The file already existed with different content.
    pub replaced: bool,
}

/// Builds the asset described by a `create_asset` call. Its origin ends up at
/// the bottom center of its bounds, like every other asset.
pub fn create(input: &Value) -> Result<Created> {
    let input = super::toolmaker::expand_root(input, &Default::default())?;
    let name = slug(input["name"].as_str().unwrap_or_default());
    if name.is_empty() {
        bail!("name is required");
    }
    let parts: Vec<Part> = if input["from_detection"].is_object() {
        parts_from_detection(&input["from_detection"])?
    } else {
        let list = input["parts"].as_array().filter(|a| !a.is_empty()).ok_or_else(|| anyhow!("give parts or from_detection"))?;
        if list.len() > MAX_PARTS {
            bail!("too many parts ({}, at most {MAX_PARTS})", list.len());
        }
        list.iter().enumerate().map(|(i, p)| parse_part(p, i)).collect::<Result<_>>()?
    };
    let (mn, mx) = bounds(&parts).context("the asset has no geometry")?;
    let offset = Vec3::new(-(mn.x + mx.x) * 0.5, -mn.y, -(mn.z + mx.z) * 0.5);
    // <name>/usd/<name>.usda: objects are named after the folder two levels up.
    let dir = custom_dir().join(&name).join("usd");
    let path = dir.join(format!("{name}.usda"));
    let existed = path.exists();
    let changed = write_usd(&path, &name, &parts, offset)?;
    let mount = input["mount"].as_str().unwrap_or("floor");
    if !matches!(mount, "floor" | "ceiling" | "wall") {
        bail!("mount must be floor, ceiling or wall");
    }
    let spec = json!({ "description": input["description"], "mount": mount, "input": input });
    std::fs::write(dir.join(format!("{name}.json")), serde_json::to_string_pretty(&spec)?)?;
    Ok(Created { slug: name, path, size: mx - mn, parts: parts.len(), replaced: existed && changed })
}

/// Assets made with `create_asset`: (name, USD path).
pub fn list_custom() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(custom_dir()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let usd = e.path().join("usd").join(format!("{name}.usda"));
        if !name.starts_with('_') && usd.is_file() {
            out.push((name, usd));
        }
    }
    out.sort();
    out
}

/// The size estimates (width, height) of a photo stand-in made with
/// `from_detection`; `None` if the asset isn't one.
pub fn standin_guess(asset: &Path) -> Option<(Option<f32>, Option<f32>)> {
    if !asset.starts_with(custom_dir()) {
        return None;
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(asset.with_extension("json")).ok()?).ok()?;
    let d = v["input"]["from_detection"].as_object()?;
    let guess = |k: &str| d.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
    Some((guess("width_m"), guess("height_m")))
}

pub fn is_photo_standin(asset: &Path) -> bool {
    standin_guess(asset).is_some()
}

/// Whether an asset hangs from the building rather than resting on what is
/// under it: ceilings, roof beams and fixtures made with a ceiling or wall mount.
pub fn hangs(asset: &Path) -> bool {
    use super::structures::Kind;
    let source = crate::usd_load::Overrides::read(asset).map(|o| o.source);
    let asset = source.as_deref().unwrap_or(asset);
    if matches!(Kind::of_asset(asset), Some(Kind::Ceiling | Kind::Beam)) {
        return true;
    }
    // Only assets made here have this file, so library assets stop at the path check.
    if !asset.starts_with(custom_dir()) {
        return false;
    }
    // Stand-ins are registered to the photo: they stay exactly where it shows them.
    if is_photo_standin(asset) {
        return true;
    }
    std::fs::read_to_string(asset.with_extension("json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| matches!(v["mount"].as_str(), Some("ceiling" | "wall")))
}

// ---------------------------------------------------------------------------
// Recolored variants

/// Writes a variant of `asset` with `spec` applied and returns its path. A
/// small USD referencing the original (valid for other USD tools) plus the
/// override sidecar `usd_load` applies when it loads the variant.
pub fn reskin(asset: &Path, spec: &Value) -> Result<PathBuf> {
    // A variant of a variant derives from the original.
    let sidecar = crate::usd_load::Overrides::read(asset);
    let source = sidecar.as_ref().map(|o| o.source.clone()).unwrap_or_else(|| asset.to_path_buf());
    let mut o = serde_json::Map::new();
    o.insert("source".into(), json!(source.to_string_lossy().replace('\\', "/")));
    let mat = Mat::parse(spec, Mat::default())?;
    if !spec["color"].is_null() || spec["preset"].is_string() || spec.is_string() {
        o.insert("color".into(), json!(mat.color));
    }
    for (k, v) in [("roughness", mat.roughness), ("metallic", mat.metallic), ("opacity", mat.opacity)] {
        if spec[k].is_number() || spec["preset"].is_string() || spec.is_string() {
            o.insert(k.into(), json!(v));
        }
    }
    if spec["keep_texture"].as_bool().unwrap_or(false) {
        o.insert("keep_texture".into(), json!(true));
    }
    if o.len() == 1 {
        bail!("give a material preset, color, roughness, metallic or opacity");
    }
    let body = Value::Object(o);
    let tag = hash_of(&body.to_string());
    let file = source.file_name().context("asset has no file name")?.to_string_lossy().to_string();
    let stem = source.file_stem().unwrap_or_default().to_string_lossy().to_string();
    // Generated assets are told apart by their folder (structures/<kind>/…), so
    // their variants stay beside them. Library assets get a folder of their own.
    let data = data_dir();
    let dir = match source.parent().filter(|p| p.starts_with(&data)) {
        Some(p) => PathBuf::from(format!("{}_{tag}", p.display())),
        None => data.join("reskins").join(format!("{stem}_{tag}")),
    };
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(Path::new(&file).with_extension("usda"));
    let root = slug(&stem);
    let root = if root.is_empty() { "asset".to_string() } else { root };
    let usda = format!(
        "#usda 1.0\n(\n    defaultPrim = \"{root}\"\n)\n\ndef Xform \"{root}\" (\n    prepend references = @{}@\n)\n{{\n}}\n",
        source.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&path, usda)?;
    crate::usd_load::Overrides::write(&path, &body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sensesim_assetgen_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn materials_parse_presets_colors_and_overrides() {
        let m = Mat::parse(&json!("steel"), Mat::default()).unwrap();
        assert_eq!(m.metallic, 1.0);
        let m = Mat::parse(&json!({ "preset": "steel", "color": "#ff0000", "roughness": 0.9 }), Mat::default()).unwrap();
        assert_eq!(m.color, [1.0, 0.0, 0.0]);
        assert_eq!((m.metallic, m.roughness), (1.0, 0.9));
        assert!(Mat::parse(&json!("unobtainium"), Mat::default()).is_err());
        assert!(Mat::parse(&json!({ "color": "#ggg" }), Mat::default()).is_err());
    }

    #[test]
    fn ceilings_and_beams_hang_but_ordinary_assets_do_not() {
        assert!(hangs(Path::new("C:/x/SenseSim/structures/ceiling/4.00x0.01x6.00/ceiling.usda")));
        assert!(hangs(Path::new("C:/x/SenseSim/structures/beam/4.00x0.40x0.20/beam.usda")));
        assert!(!hangs(Path::new("C:/x/SenseSim/structures/wall/4.00x3.00x0.20/wall.usda")));
        assert!(!hangs(Path::new("C:/library/rack_a01/usd/rack_a01.usd")));
    }

    #[test]
    fn unlit_textures_load_as_emissive_with_no_diffuse() {
        let dir = temp("unlit");
        let tex = dir.join("src.png");
        image::RgbImage::from_pixel(4, 4, image::Rgb([200, 10, 10])).save(&tex).unwrap();
        let parts = vec![Part { shape: Shape::Panel, size: Vec3::new(1.0, 1.0, 0.001), at: Vec3::ZERO, rot: Quat::IDENTITY, mat: Mat { texture: Some(tex), color: [1.0; 3], unlit: true, ..Default::default() } }];
        let path = dir.join("thing.usda");
        write_usd(&path, "thing", &parts, Vec3::ZERO).unwrap();
        let l = crate::usd_load::load(&path).unwrap();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        let m = &l.materials[l.parts[0].material.unwrap()];
        assert!(m.emissive_tex.as_ref().is_some_and(|t| t.srgb), "emissive texture, sRGB");
        assert!(m.base_color_tex.is_none());
        assert_eq!(m.base_color, [0.0; 3]);
        assert_eq!(m.emissive, [UNLIT_GAIN; 3], "both the factor and the connection are read");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_preset_resolves() {
        for p in PRESETS {
            assert!(Mat::preset(p).is_some(), "{p}");
        }
    }

    #[test]
    fn generated_usd_loads_with_materials_and_textures() {
        let dir = temp("load");
        let tex = dir.join("src.png");
        image::RgbImage::from_pixel(4, 4, image::Rgb([200, 10, 10])).save(&tex).unwrap();
        let steel = Mat::preset("steel").unwrap();
        let textured = Mat { texture: Some(tex), color: [1.0; 3], tile_m: 0.5, ..Default::default() };
        let parts = vec![
            Part { shape: Shape::Box, size: Vec3::new(2.0, 1.0, 0.5), at: Vec3::new(0.0, 0.5, 0.0), rot: Quat::IDENTITY, mat: steel.clone() },
            Part { shape: Shape::Cylinder { top: 1.0 }, size: Vec3::new(0.2, 1.0, 0.2), at: Vec3::new(0.0, 1.5, 0.0), rot: Quat::IDENTITY, mat: steel },
            Part { shape: Shape::Panel, size: Vec3::new(2.0, 1.0, 0.001), at: Vec3::new(0.0, 0.5, 0.26), rot: Quat::IDENTITY, mat: textured },
        ];
        let path = dir.join("thing.usda");
        write_usd(&path, "thing", &parts, Vec3::ZERO).unwrap();
        let l = crate::usd_load::load(&path).unwrap();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.parts.len(), 3);
        assert_eq!(l.materials.len(), 2);
        assert!(!l.z_up);
        assert_eq!(l.meters_per_unit, 1.0);
        let m0 = &l.materials[l.parts[0].material.unwrap()];
        assert_eq!(m0.metallic, 1.0);
        let m2 = &l.materials[l.parts[2].material.unwrap()];
        let t = m2.base_color_tex.as_ref().expect("texture is read");
        assert!(t.file.exists(), "texture copied next to the USD: {:?}", t.file);
        // UVs tile by real size: 2 m / 0.5 m = 4 repeats.
        assert!(l.parts[2].uvs.iter().any(|uv| (uv[0] - 4.0).abs() < 1e-3), "{:?}", l.parts[2].uvs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn faces_wind_towards_their_normals() {
        for shape in [Shape::Box, Shape::Panel, Shape::Cylinder { top: 1.0 }, Shape::Cylinder { top: 0.0 }] {
            let p = Part { shape, size: Vec3::new(1.0, 2.0, 3.0), at: Vec3::ZERO, rot: Quat::IDENTITY, mat: Mat::default() };
            for poly in polys(&p) {
                let g = (poly.pos[1] - poly.pos[0]).cross(poly.pos[2] - poly.pos[0]).normalize();
                assert!(g.dot(poly.nrm[0]) > 0.0, "{shape:?}: winding {g} against normal {}", poly.nrm[0]);
            }
        }
    }

    #[test]
    fn recolored_variant_overrides_the_source_materials() {
        let dir = temp("reskin");
        let parts = vec![Part { shape: Shape::Box, size: Vec3::ONE, at: Vec3::ZERO, rot: Quat::IDENTITY, mat: Mat::preset("wood").unwrap() }];
        let src = dir.join("thing.usda");
        write_usd(&src, "thing", &parts, Vec3::ZERO).unwrap();
        let v = reskin(&src, &json!({ "color": "#00ff00", "roughness": 0.1 })).unwrap();
        let l = crate::usd_load::load(&v).unwrap();
        assert_eq!(l.parts.len(), 1, "geometry comes from the source");
        let m = &l.materials[l.parts[0].material.unwrap()];
        assert_eq!(m.base_color, [0.0, 1.0, 0.0]);
        assert_eq!(m.roughness, 0.1);
        // A variant of the variant derives from the original, not a chain.
        let v2 = reskin(&v, &json!({ "color": "#0000ff" })).unwrap();
        assert_eq!(crate::usd_load::Overrides::read(&v2).unwrap().source.file_name().unwrap(), "thing.usda");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(v.parent().unwrap());
        let _ = std::fs::remove_dir_all(v2.parent().unwrap());
    }
}
