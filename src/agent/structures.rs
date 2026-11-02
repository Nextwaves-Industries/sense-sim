//! Room-shell pieces (floor slab, walls, columns) as tiny generated USD files.
//! They go through the normal asset path, so selection, layouts, USDA export
//! and the self-check treat them like any other asset.

use std::path::PathBuf;

use anyhow::Result;
use bevy::math::{Quat, Vec2, Vec3};

use super::assetgen::{self, Mat, Part, Shape};
use super::vision::perceive::PhotoScene;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Floor,
    Wall,
    Column,
    Block,
    /// A downward-facing plane: visible from inside the hall, culled (so
    /// see-through) from above.
    Ceiling,
    /// Roof truss / beam under the ceiling.
    Beam,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Self::Floor, Self::Wall, Self::Column, Self::Block, Self::Ceiling, Self::Beam];

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "floor" | "slab" => Some(Self::Floor),
            "wall" => Some(Self::Wall),
            "column" | "pillar" | "post" => Some(Self::Column),
            "box" | "block" => Some(Self::Block),
            "ceiling" | "roof" => Some(Self::Ceiling),
            "beam" | "truss" => Some(Self::Beam),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Floor => "floor",
            Self::Wall => "wall",
            Self::Column => "column",
            Self::Block => "block",
            Self::Ceiling => "ceiling",
            Self::Beam => "beam",
        }
    }

    /// Linear-ish display color.
    fn color(self) -> [f32; 3] {
        match self {
            Self::Floor => [0.76, 0.77, 0.78],
            Self::Wall => [0.9, 0.905, 0.915],
            Self::Column => [0.16, 0.26, 0.52],
            Self::Block => [0.72, 0.72, 0.73],
            Self::Ceiling => [0.9, 0.905, 0.91],
            Self::Beam => [0.18, 0.2, 0.22],
        }
    }

    /// The structure kind of a placed asset, from its generated path
    /// (`…/structures/<kind>/<size>/<kind>.usda`).
    pub fn of_asset(asset: &std::path::Path) -> Option<Self> {
        let s = asset.to_string_lossy().replace('\\', "/");
        let i = s.find("/structures/")?;
        let kind = s[i + "/structures/".len()..].split('/').next()?;
        Self::ALL.into_iter().find(|k| k.name() == kind)
    }
}

/// Writes (once) a box mesh of `size` meters and returns its path:
/// `%LOCALAPPDATA%/SenseSim/structures/<kind>/<WxHxD>/<kind>.usda`, so placed
/// copies are named `<kind>_N`.
pub fn usd_for(kind: Kind, size: Vec3) -> Result<PathBuf> {
    let size = size.max(Vec3::splat(0.01));
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base)
        .join("SenseSim")
        .join("structures")
        .join(kind.name())
        .join(format!("{:.2}x{:.2}x{:.2}", size.x, size.y, size.z));
    let path = dir.join(format!("{}.usda", kind.name()));
    // Compared, not just checked for existence: a file from an older version
    // keeps its colours otherwise.
    let text = if kind == Kind::Ceiling { ceiling_usda(size) } else { box_usda(kind, size) };
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&path, text)?;
    }
    Ok(path)
}

const CEILING_BOUNCE: f32 = 0.5;

/// Like [`usd_for`] with a real material (color, texture, roughness…). Stays
/// under `structures/<kind>/` so the piece still counts as that kind.
pub fn usd_with_material(kind: Kind, size: Vec3, mat: &Mat) -> Result<PathBuf> {
    let size = size.max(Vec3::splat(0.01));
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let tag = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{mat:?}").hash(&mut h);
        h.finish() as u32
    };
    let dir = PathBuf::from(base)
        .join("SenseSim")
        .join("structures")
        .join(kind.name())
        .join(format!("{:.2}x{:.2}x{:.2}_{tag:08x}", size.x, size.y, size.z));
    let path = dir.join(format!("{}.usda", kind.name()));
    let part = if kind == Kind::Ceiling {
        // Same facing as the plain ceiling: visible from below only. Only faint
        // ambient light reaches a surface facing down, so a white ceiling would
        // render mid-gray; a little glow stands in for light bounced off the floor.
        let bounce = mat.color.map(|c| c * CEILING_BOUNCE);
        let emissive = if mat.emissive == [0.0; 3] && mat.texture.is_none() { bounce } else { mat.emissive };
        Part {
            shape: Shape::Panel,
            size: Vec3::new(size.x, size.z, 0.001),
            at: Vec3::ZERO,
            rot: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            mat: Mat { emissive, ..mat.clone() },
        }
    } else {
        Part { shape: Shape::Box, size, at: Vec3::new(0.0, size.y * 0.5, 0.0), rot: Quat::IDENTITY, mat: mat.clone() }
    };
    assetgen::write_usd(&path, kind.name(), &[part], Vec3::ZERO)?;
    Ok(path)
}

/// One quad at y = 0 facing down (single-sided), centered in x/z.
fn ceiling_usda(s: Vec3) -> String {
    let (hx, hz) = (s.x * 0.5, s.z * 0.5);
    let c = Kind::Ceiling.color();
    let color = format!("({}, {}, {})", c[0], c[1], c[2]);
    // Wound clockwise seen from above = counter-clockwise from below: the
    // front face points down.
    format!(
        r#"#usda 1.0
(
    defaultPrim = "ceiling"
    metersPerUnit = 1
    upAxis = "Y"
)

def Xform "ceiling" (
    kind = "component"
)
{{
    def Mesh "geometry"
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [({x0}, 0, {z0}), ({x1}, 0, {z0}), ({x1}, 0, {z1}), ({x0}, 0, {z1})]
        normal3f[] normals = [(0, -1, 0), (0, -1, 0), (0, -1, 0), (0, -1, 0)] (
            interpolation = "vertex"
        )
        color3f[] primvars:displayColor = [{color}, {color}, {color}, {color}] (
            interpolation = "vertex"
        )
        uniform token orientation = "rightHanded"
        uniform token subdivisionScheme = "none"
        uniform bool doubleSided = 0
    }}
}}
"#,
        x0 = -hx,
        x1 = hx,
        z0 = -hz,
        z1 = hz,
    )
}

/// A Y-up, meter-scale box standing on y = 0, centered in x/z.
fn box_usda(kind: Kind, s: Vec3) -> String {
    let (hx, hz, h) = (s.x * 0.5, s.z * 0.5, s.y);
    let p = [
        [-hx, 0.0, -hz],
        [hx, 0.0, -hz],
        [hx, 0.0, hz],
        [-hx, 0.0, hz],
        [-hx, h, -hz],
        [hx, h, -hz],
        [hx, h, hz],
        [-hx, h, hz],
    ];
    // Counter-clockwise seen from outside (right-handed).
    let faces = [[0, 1, 2, 3], [4, 7, 6, 5], [0, 4, 5, 1], [1, 5, 6, 2], [2, 6, 7, 3], [3, 7, 4, 0]];
    let pts: Vec<String> = p.iter().map(|v| format!("({}, {}, {})", v[0], v[1], v[2])).collect();
    let idx: Vec<String> = faces.iter().flatten().map(|i| i.to_string()).collect();
    let c = kind.color();
    let color = format!("({}, {}, {})", c[0], c[1], c[2]);
    let colors = vec![color; 8].join(", ");
    format!(
        r#"#usda 1.0
(
    defaultPrim = "{name}"
    metersPerUnit = 1
    upAxis = "Y"
)

def Xform "{name}" (
    kind = "component"
)
{{
    def Mesh "geometry"
    {{
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [{idx}]
        point3f[] points = [{pts}]
        color3f[] primvars:displayColor = [{colors}] (
            interpolation = "vertex"
        )
        uniform token orientation = "rightHanded"
        uniform token subdivisionScheme = "none"
        uniform bool doubleSided = 1
    }}
}}
"#,
        name = kind.name(),
        idx = idx.join(", "),
        pts = pts.join(", "),
    )
}

/// A piece of shell to place: kind, size (x = length, y = height, z = depth),
/// floor position (x, z), base height and yaw.
#[derive(Clone, Debug)]
pub struct Piece {
    pub kind: Kind,
    pub size: Vec3,
    pub center: Vec2,
    pub base_y: f32,
    pub yaw: f32,
    /// `None`: the kind's plain default look.
    pub material: Option<Mat>,
}

/// Floor-plan bounds of the room: the assistant's estimate, reaching the camera
/// that stood in it and anything placed outside the estimate.
fn shell_extent(sc: &PhotoScene) -> Option<(Vec2, Vec2)> {
    let f = sc.room?;
    let mut pts = vec![Vec2::ZERO, Vec2::new(f[0], f[1]), Vec2::new(f[2], f[3])];
    pts.extend(sc.objects.iter().filter_map(|o| o.footprint).map(|f| f.center));
    let lo = pts.iter().copied().reduce(Vec2::min)?;
    let hi = pts.iter().copied().reduce(Vec2::max)?;
    Some((lo, hi))
}

/// Ceiling height for a photo, and where it came from: the assistant's estimate,
/// or if it has none, what must fit under it.
pub fn ceiling_height(sc: &PhotoScene) -> (f32, &'static str) {
    let tops = sc.objects.iter().filter_map(|o| o.footprint).map(|f| f.base_y + f.height).fold(0.0f32, f32::max);
    match sc.ceiling_y {
        Some(h) if h >= tops => (h, "your estimate"),
        Some(_) => (tops + 0.3, "your estimate, raised above the tallest object"),
        // The photo was taken from under it, too.
        None => ((tops + 0.8).max(sc.camera.eye().y + 0.5).max(3.0), "assumed from what stands under it: estimate it"),
    }
}

/// The room shell implied by a perceived photo: floor, walls, columns and a
/// ceiling (unless the photo was declared to have none), in the colors the
/// photo shows.
pub fn shell_from_photo(sc: &PhotoScene) -> Vec<Piece> {
    let mut out = Vec::new();
    let extent = shell_extent(sc);
    let look = |c: Option<[u8; 3]>| c.map(Mat::from_srgb);
    if let Some((lo, hi)) = extent {
        // Grow it a little so objects at the edge stand on it.
        let size = hi - lo + Vec2::splat(2.0);
        let center = (lo + hi) * 0.5;
        out.push(Piece {
            kind: Kind::Floor,
            size: Vec3::new(size.x, 0.02, size.y),
            center,
            base_y: -0.02,
            yaw: 0.0,
            material: look(sc.colors.floor),
        });
    }
    // Close the room, leaving the side behind the camera open.
    if let Some((lo, hi)) = extent {
        let (lo, hi) = (lo - Vec2::ONE, hi + Vec2::ONE);
        let (size, center) = (hi - lo, (lo + hi) * 0.5);
        let height = ceiling_height(sc).0;
        let wall = |length: f32, at: Vec2, yaw: f32| Piece {
            kind: Kind::Wall,
            size: Vec3::new(length, height, 0.2),
            center: at,
            base_y: 0.0,
            yaw,
            material: look(sc.colors.walls),
        };
        let quarter = std::f32::consts::FRAC_PI_2;
        out.push(wall(size.x, Vec2::new(center.x, lo.y), 0.0));
        out.push(wall(size.y, Vec2::new(lo.x, center.y), quarter));
        // Turned the other way, so that its front face looks into the room too.
        out.push(wall(size.y, Vec2::new(hi.x, center.y), -quarter));
    }
    for o in &sc.objects {
        let l = o.label.to_ascii_lowercase();
        if !(l.contains("column") || l.contains("pillar") || l.contains("post")) {
            continue;
        }
        if let Some(fp) = o.footprint {
            let side = o.lateral_width.clamp(0.15, 1.5);
            // Columns run to the roof; the photo often shows only part of them.
            let height = ceiling_height(sc).0.max(fp.base_y + fp.height);
            let view = fp.center.normalize_or_zero();
            out.push(Piece {
                kind: Kind::Column,
                size: Vec3::new(side, height, side),
                // The visible face is the front: center is half a side behind it.
                center: fp.center + view * side * 0.5,
                base_y: 0.0,
                yaw: sc.dominant_yaw,
                material: look(o.color),
            });
        }
    }
    if let Some((lo, hi)) = extent.filter(|_| !sc.no_ceiling) {
        let size = hi - lo + Vec2::splat(2.0);
        out.push(Piece {
            kind: Kind::Ceiling,
            size: Vec3::new(size.x, 0.0, size.y),
            center: (lo + hi) * 0.5,
            base_y: ceiling_height(sc).0,
            yaw: 0.0,
            material: look(sc.colors.ceiling),
        });
    }
    out
}

/// Longest side of a texture projected onto the shell.
const PROJECTED_MAX: u32 = 1536;
const PROJECTED_PER_M: f32 = 48.0;

/// The world point of a texel (s, t in 0..1, t = 0 at the top row) on the inward
/// face of a shell piece. Rows and columns follow the generated meshes' UVs.
fn shell_texel(p: &Piece, s: f32, t: f32) -> Option<Vec3> {
    let c = p.center;
    match p.kind {
        Kind::Floor => Some(Vec3::new(c.x + (s - 0.5) * p.size.x, 0.0, c.y + (t - 0.5) * p.size.z)),
        Kind::Ceiling => Some(Vec3::new(c.x + (s - 0.5) * p.size.x, p.base_y, c.y - (t - 0.5) * p.size.z)),
        Kind::Wall => {
            // Local +X and the front (+Z) face looks into the room.
            let axis = Vec2::new(p.yaw.cos(), -p.yaw.sin());
            let at = c + axis * (s - 0.5) * p.size.x;
            let front = Vec2::new(p.yaw.sin(), p.yaw.cos()) * p.size.z * 0.5;
            Some(Vec3::new(at.x + front.x, (1.0 - t) * p.size.y, at.y + front.y))
        }
        _ => None,
    }
}

/// What the photo shows on a shell piece: each texel takes the color of the photo
/// pixel that sees it from the photo camera, so the piece looks right from there
/// whatever size the room was guessed to be. Texels the photo doesn't see get `fill`,
/// and so do those it sees through an object: painted on the floor behind it, a
/// shopper or a shelf would smear across the room from any other viewpoint.
pub fn project_photo(sc: &PhotoScene, photo: &image::RgbImage, p: &Piece, fill: [u8; 3]) -> Option<image::RgbImage> {
    let placed: std::collections::HashSet<usize> = sc.placed.values().copied().collect();
    let in_the_way: Vec<[f32; 4]> =
        sc.objects.iter().filter(|o| placed.contains(&o.id) || o.label.to_ascii_lowercase().contains("person")).map(|o| o.bbox).collect();
    let (len_u, len_v) = match p.kind {
        Kind::Floor | Kind::Ceiling => (p.size.x, p.size.z),
        Kind::Wall => (p.size.x, p.size.y),
        _ => return None,
    };
    let side = |m: f32| ((m * PROJECTED_PER_M) as u32).clamp(16, PROJECTED_MAX);
    let (w, h) = (side(len_u), side(len_v));
    let (pw, ph) = (photo.width(), photo.height());
    let mut any = false;
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let world = shell_texel(p, (x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
        let hit = world.and_then(|q| sc.camera.project(q)).filter(|uv| (0.0..1.0).contains(&uv.x) && (0.0..1.0).contains(&uv.y));
        let seen = hit.is_some();
        any |= seen;
        let hit = hit.filter(|uv| !in_the_way.iter().any(|b| uv.x >= b[0] && uv.x <= b[2] && uv.y >= b[1] && uv.y <= b[3]));
        match hit {
            Some(uv) => {
                *photo.get_pixel(((uv.x * pw as f32) as u32).min(pw - 1), ((uv.y * ph as f32) as u32).min(ph - 1))
            }
            None => image::Rgb(fill),
        }
    });
    any.then_some(img)
}

/// Saves a projection and returns the material showing it.
pub fn projected_material(img: &image::RgbImage, tag: &str) -> Result<Mat> {
    let dir = assetgen::custom_dir().join("_textures");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("projected_{tag}.png"));
    img.save(&path)?;
    Ok(Mat { texture: Some(path), color: [1.0; 3], unlit: true, ..Default::default() })
}

/// Roof beams under a photo shell's ceiling.
pub fn beams_for_photo(sc: &PhotoScene, spacing: f32) -> Vec<Piece> {
    match shell_extent(sc).filter(|_| !sc.no_ceiling) {
        Some((lo, hi)) => {
            let (lo, hi) = (lo - Vec2::ONE, hi + Vec2::ONE);
            // The first piece is the ceiling, which the shell already has.
            let mut beams: Vec<Piece> = ceiling_pieces(lo, hi, ceiling_height(sc).0, Some(spacing)).into_iter().skip(1).collect();
            // In the photo's own roof color, darker, not near-black bars across it.
            if let Some(c) = sc.colors.ceiling {
                for b in &mut beams {
                    b.material = Some(Mat::from_srgb(c.map(|v| (v as f32 * 0.7) as u8)));
                }
            }
            beams
        }
        None => Vec::new(),
    }
}

/// Ceiling over `lo..hi` (x, z) at `height`, plus optional roof beams along
/// x every `beam_spacing` meters, 0.6 m under the ceiling.
pub fn ceiling_pieces(lo: Vec2, hi: Vec2, height: f32, beams: Option<f32>) -> Vec<Piece> {
    let size = hi - lo;
    let center = (lo + hi) * 0.5;
    let mut out = vec![Piece { kind: Kind::Ceiling, size: Vec3::new(size.x, 0.0, size.y), center, base_y: height, yaw: 0.0, material: None }];
    if let Some(sp) = beams.filter(|s| *s >= 1.0) {
        let mut z = lo.y + sp * 0.5;
        while z < hi.y {
            out.push(Piece { kind: Kind::Beam, size: Vec3::new(size.x, 0.4, 0.2), center: Vec2::new(center.x, z), base_y: height - 0.6, yaw: 0.0, material: None });
            z += sp;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Mounting points for radio infrastructure (anchors, beacons, readers)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MountKind {
    Ceiling,
    Wall,
    Column,
}

#[derive(Clone, Copy, Debug)]
pub struct MountPoint {
    pub pos: Vec3,
    /// Unit direction the device faces (antenna boresight / surface normal).
    pub facing: Vec3,
    pub kind: MountKind,
}

#[derive(Clone, Debug)]
pub struct MountOpts {
    /// Grid / along-wall spacing in meters.
    pub spacing_m: f32,
    /// Height for wall and column mounts (clamped under the ceiling).
    pub mount_height_m: f32,
    /// Distance below the ceiling for ceiling mounts.
    pub ceiling_drop_m: f32,
    /// Gap between a wall/column surface and the device.
    pub standoff_m: f32,
    pub ceiling: bool,
    pub walls: bool,
    pub columns: bool,
}

impl Default for MountOpts {
    fn default() -> Self {
        Self { spacing_m: 3.0, mount_height_m: 2.5, ceiling_drop_m: 0.3, standoff_m: 0.05, ceiling: true, walls: true, columns: true }
    }
}

/// A structure in the world: its kind, center, local half extents and yaw.
struct Placed {
    kind: Kind,
    center: Vec3,
    half: Vec3,
    rot: bevy::math::Quat,
    min: Vec3,
    max: Vec3,
}

/// Candidate places to mount radio infrastructure on the room shell built
/// with `add_structure`: a grid under the ceiling, the faces of columns and
/// the inner faces of walls. Empty when the scene has no such structures.
pub fn mount_points(world: &mut bevy::prelude::World, opts: &MountOpts) -> Vec<MountPoint> {
    use bevy::prelude::Transform;
    let mut placed = Vec::new();
    let mut others_center = Vec3::ZERO;
    let mut others = 0.0f32;
    {
        let mut q = world.query::<(&crate::scene::PlacedObject, &Transform)>();
        for (o, t) in q.iter(world) {
            let (a, b) = (o.proto.aabb_min, o.proto.aabb_max);
            let local_c = (a + b) * 0.5;
            let half = (b - a) * 0.5 * t.scale;
            let center = t.transform_point(local_c);
            let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for x in [a.x, b.x] {
                for y in [a.y, b.y] {
                    for z in [a.z, b.z] {
                        let p = t.transform_point(Vec3::new(x, y, z));
                        mn = mn.min(p);
                        mx = mx.max(p);
                    }
                }
            }
            match Kind::of_asset(&o.asset) {
                Some(kind) => placed.push(Placed { kind, center, half, rot: t.rotation, min: mn, max: mx }),
                None => {
                    others_center += center;
                    others += 1.0;
                }
            }
        }
    }
    // Room interior reference for wall facing: scene objects, else the shell.
    let interior = if others > 0.0 {
        others_center / others
    } else if !placed.is_empty() {
        placed.iter().map(|p| p.center).sum::<Vec3>() / placed.len() as f32
    } else {
        Vec3::ZERO
    };
    mounts_for(&placed, interior, opts)
}

fn mounts_for(placed: &[Placed], interior: Vec3, opts: &MountOpts) -> Vec<MountPoint> {
    let ceiling_h = placed.iter().filter(|p| p.kind == Kind::Ceiling).map(|p| p.max.y).fold(None, |a: Option<f32>, y| Some(a.map_or(y, |a| a.min(y))));
    // Wall and column mounts stay under the ceiling.
    let mount_y = match ceiling_h {
        Some(h) => opts.mount_height_m.min(h - opts.ceiling_drop_m).max(0.5),
        None => opts.mount_height_m,
    };
    let spacing = opts.spacing_m.max(0.5);
    let mut out = Vec::new();
    for p in placed {
        match p.kind {
            Kind::Ceiling if opts.ceiling => {
                let y = p.max.y - opts.ceiling_drop_m;
                let mut z = p.min.z + spacing * 0.5;
                while z < p.max.z {
                    let mut x = p.min.x + spacing * 0.5;
                    while x < p.max.x {
                        out.push(MountPoint { pos: Vec3::new(x, y, z), facing: Vec3::NEG_Y, kind: MountKind::Ceiling });
                        x += spacing;
                    }
                    z += spacing;
                }
            }
            Kind::Column if opts.columns => {
                for n in [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z] {
                    let normal = p.rot * n;
                    let off = (p.half * n.abs()).length() + opts.standoff_m;
                    let pos = Vec3::new(p.center.x, mount_y, p.center.z) + normal * off;
                    out.push(MountPoint { pos, facing: normal, kind: MountKind::Column });
                }
            }
            Kind::Wall if opts.walls => {
                // Local X runs along the wall; the inner face is the ±Z side facing the interior.
                let along = p.rot * Vec3::X;
                let mut normal = p.rot * Vec3::Z;
                if normal.dot(interior - p.center) < 0.0 {
                    normal = -normal;
                }
                let len = p.half.x * 2.0;
                let n = ((len / spacing).floor() as usize).max(1);
                for k in 0..n {
                    let t = -p.half.x + len * (k as f32 + 0.5) / n as f32;
                    let base = Vec3::new(p.center.x, mount_y, p.center.z) + along * t;
                    out.push(MountPoint { pos: base + normal * (p.half.z + opts.standoff_m), facing: normal, kind: MountKind::Wall });
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_valid_box_files() {
        let p = usd_for(Kind::Column, Vec3::new(0.4, 6.0, 0.4)).unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("faceVertexCounts = [4, 4, 4, 4, 4, 4]"));
        assert!(txt.contains("(0.2, 6, 0.2)"));
        assert!(p.to_string_lossy().contains("column"));
        // Same size, same file.
        assert_eq!(p, usd_for(Kind::Column, Vec3::new(0.4, 6.0, 0.4)).unwrap());
    }

    #[test]
    fn loads_with_the_usd_loader() {
        let p = usd_for(Kind::Wall, Vec3::new(10.0, 3.0, 0.2)).unwrap();
        let l = crate::usd_load::load(&p).unwrap();
        assert_eq!(l.parts.len(), 1);
        let ys: Vec<f32> = l.parts[0].positions.iter().map(|p| p[1]).collect();
        assert!((ys.iter().cloned().fold(f32::MIN, f32::max) - 3.0).abs() < 1e-5, "height 3 m");
        assert!(ys.iter().cloned().fold(f32::MAX, f32::min).abs() < 1e-5, "stands on y = 0");
        assert!(!l.z_up);
        assert!((l.meters_per_unit - 1.0).abs() < 1e-6);
    }

    fn photo_scene() -> PhotoScene {
        use crate::agent::vision::geometry::{Floor, Frame, PhotoCamera};
        let floor = Floor { n: Vec3::new(0.0, -0.97, -0.24).normalize(), d: 1.6, inlier_ratio: 1.0 };
        PhotoScene {
            image_id: "t".into(),
            width: 640,
            height: 480,
            hfov_deg: 65.0,
            hfov_source: "given",
            camera: PhotoCamera::new(Frame::new(&floor), 65.0, 640, 480),
            camera_guess: None,
            objects: vec![],
            ceiling_y: None,
            room: Some([-4.0, -9.0, 4.0, -1.0]),
            dominant_yaw: 0.0,
            placed: Default::default(),
            colors: Default::default(),
            no_ceiling: false,
        }
    }

    fn ceilings(sc: &PhotoScene) -> Vec<Piece> {
        shell_from_photo(sc).into_iter().filter(|p| p.kind == Kind::Ceiling).collect()
    }

    #[test]
    fn a_photo_shell_has_a_ceiling_even_when_none_was_measured() {
        let mut sc = photo_scene();
        let c = ceilings(&sc);
        assert_eq!(c.len(), 1, "the ceiling must not depend on it being visible");
        assert_eq!(c[0].base_y, 3.0);
        assert!(ceiling_height(&sc).1.starts_with("assumed"));
        sc.ceiling_y = Some(5.5);
        assert_eq!(ceilings(&sc)[0].base_y, 5.5);
        assert_eq!(ceiling_height(&sc).1, "your estimate");
    }

    #[test]
    fn the_room_is_as_big_as_it_was_guessed() {
        let mut sc = photo_scene();
        sc.room = Some([-20.0, -45.0, 12.0, 0.0]);
        let c = &ceilings(&sc)[0];
        assert!(c.size.z >= 45.0 && c.size.x >= 32.0, "{:?}", c.size);
        assert!((c.center.y + 22.5).abs() < 1e-4, "{:?}", c.center);
    }

    #[test]
    fn no_room_guess_builds_no_shell() {
        let mut sc = photo_scene();
        sc.room = None;
        assert!(shell_from_photo(&sc).iter().all(|p| !matches!(p.kind, Kind::Floor | Kind::Wall | Kind::Ceiling)));
    }

    #[test]
    fn a_room_is_closed_except_behind_the_camera() {
        let sc = photo_scene();
        let walls: Vec<Piece> = shell_from_photo(&sc).into_iter().filter(|p| p.kind == Kind::Wall).collect();
        assert_eq!(walls.len(), 3);
        // Floor -4..4 by -9..-1, reaching the camera at z = 0, plus a meter all round.
        assert!(walls.iter().any(|w| (w.center.y + 10.0).abs() < 1e-4 && w.yaw == 0.0), "back wall");
        assert!(walls.iter().all(|w| w.center.y < 0.1), "nothing behind the camera");
    }

    #[test]
    fn photo_beams_hang_under_the_ceiling_across_the_room() {
        let sc = photo_scene();
        let beams = beams_for_photo(&sc, 3.0);
        assert!(!beams.is_empty());
        assert!(beams.iter().all(|b| b.kind == Kind::Beam && b.base_y < ceiling_height(&sc).0));
    }

    fn projected_sees_itself(p: &Piece, sc: &PhotoScene) {
        // A photo whose color encodes where each pixel is.
        let photo = image::RgbImage::from_fn(256, 256, |x, y| image::Rgb([x as u8, y as u8, 0]));
        let img = project_photo(sc, &photo, p, [0, 0, 255]).expect("part of it is in view");
        let mut checked = 0;
        for (x, y, px) in img.enumerate_pixels() {
            if px[2] == 255 {
                continue;
            }
            let q = shell_texel(p, (x as f32 + 0.5) / img.width() as f32, (y as f32 + 0.5) / img.height() as f32).unwrap();
            let uv = sc.camera.project(q).unwrap();
            assert!((uv.x * 256.0 - px[0] as f32).abs() < 2.0 && (uv.y * 256.0 - px[1] as f32).abs() < 2.0, "{uv} vs {px:?}");
            checked += 1;
        }
        assert!(checked > 0);
    }

    #[test]
    fn projected_shell_pieces_show_what_the_photo_shows_there() {
        let mut sc = photo_scene();
        sc.ceiling_y = Some(4.0);
        sc.room = Some([-8.0, -30.0, 8.0, -1.0]);
        for p in shell_from_photo(&sc) {
            projected_sees_itself(&p, &sc);
        }
    }

    #[test]
    fn objects_in_the_way_are_not_painted_onto_the_floor() {
        use crate::agent::vision::perceive::PhotoObject;
        let mut sc = photo_scene();
        sc.objects.push(PhotoObject { id: 0, label: "person".into(), score: 1.0, bbox: [0.4, 0.6, 0.6, 0.95], footprint: None, lateral_width: 0.5, radial: false, color: None, guess: Default::default(), source: "", checks: serde_json::Value::Null });
        let photo = image::RgbImage::from_pixel(64, 64, image::Rgb([200, 0, 0]));
        let floor = shell_from_photo(&sc).into_iter().find(|p| p.kind == Kind::Floor).unwrap();
        let img = project_photo(&sc, &photo, &floor, [0, 0, 255]).unwrap();
        let (mut filled_in_box, mut painted) = (0, 0);
        for (x, y, px) in img.enumerate_pixels() {
            let q = shell_texel(&floor, (x as f32 + 0.5) / img.width() as f32, (y as f32 + 0.5) / img.height() as f32).unwrap();
            let Some(uv) = sc.camera.project(q).filter(|uv| (0.0..1.0).contains(&uv.x) && (0.0..1.0).contains(&uv.y)) else { continue };
            if (0.42..0.58).contains(&uv.x) && (0.62..0.93).contains(&uv.y) {
                assert_eq!(px[2], 255, "painted through the person at {uv}");
                filled_in_box += 1;
            } else if px[0] == 200 {
                painted += 1;
            }
        }
        assert!(filled_in_box > 0 && painted > 0);
    }

    #[test]
    fn side_walls_face_into_the_room() {
        let walls: Vec<Piece> = shell_from_photo(&photo_scene()).into_iter().filter(|p| p.kind == Kind::Wall).collect();
        for w in &walls {
            let inner = shell_texel(w, 0.5, 0.5).unwrap();
            let outer_center = Vec2::new(w.center.x, w.center.y);
            let toward_room = Vec2::new(0.0, -5.0) - outer_center;
            assert!((Vec2::new(inner.x, inner.z) - outer_center).dot(toward_room) > 0.0, "{:?}", w.center);
        }
    }

    #[test]
    fn outdoor_photos_can_skip_the_ceiling() {
        let mut sc = photo_scene();
        sc.no_ceiling = true;
        assert!(ceilings(&sc).is_empty());
    }

    #[test]
    fn the_ceiling_clears_the_tallest_object_and_covers_the_walls() {
        use crate::agent::vision::geometry::Footprint;
        use crate::agent::vision::perceive::PhotoObject;
        let mut sc = photo_scene();
        sc.ceiling_y = Some(2.5);
        sc.room = Some([-12.0, -20.0, 12.0, -1.0]);
        sc.objects.push(PhotoObject {
            id: 0,
            label: "shelf".into(),
            score: 1.0,
            bbox: [0.4, 0.4, 0.6, 0.8],
            footprint: Some(Footprint { center: Vec2::new(0.0, -5.0), size: Vec2::ONE, yaw: 0.0, base_y: 0.0, height: 3.2, points: 100 }),
            lateral_width: 1.0,
            radial: false,
            color: None,
            guess: Default::default(),
            source: "",
            checks: serde_json::Value::Null,
        });
        let c = &ceilings(&sc)[0];
        assert!((c.base_y - 3.5).abs() < 1e-5, "{}", c.base_y);
        assert!(c.size.x >= 24.0 && c.size.z >= 19.0, "{:?}", c.size);
    }

    #[test]
    fn the_shell_takes_its_colors_from_the_photo() {
        let mut sc = photo_scene();
        sc.colors.floor = Some([10, 20, 30]);
        sc.colors.ceiling = Some([200, 200, 200]);
        let shell = shell_from_photo(&sc);
        let floor = shell.iter().find(|p| p.kind == Kind::Floor).unwrap();
        assert_eq!(floor.material, Some(Mat::from_srgb([10, 20, 30])));
        assert_eq!(ceilings(&sc)[0].material, Some(Mat::from_srgb([200, 200, 200])));
    }

    #[test]
    fn a_material_ceiling_still_faces_down_and_stays_a_ceiling() {
        let p = usd_with_material(Kind::Ceiling, Vec3::new(4.0, 0.0, 6.0), &Mat::from_srgb([200, 200, 200])).unwrap();
        assert_eq!(Kind::of_asset(&p), Some(Kind::Ceiling));
        let l = crate::usd_load::load(&p).unwrap();
        let part = &l.parts[0];
        assert!(part.positions.iter().all(|v| v[1].abs() < 1e-4));
        assert!(part.normals.iter().all(|n| n[1] < -0.99), "{:?}", part.normals);
    }

    #[test]
    fn ceiling_is_one_downward_quad() {
        let p = usd_for(Kind::Ceiling, Vec3::new(20.0, 0.0, 30.0)).unwrap();
        assert_eq!(Kind::of_asset(&p), Some(Kind::Ceiling));
        let l = crate::usd_load::load(&p).unwrap();
        assert_eq!(l.parts.len(), 1);
        let part = &l.parts[0];
        assert_eq!(part.indices.len(), 6, "one quad = two triangles");
        // Winding gives a downward normal: (b - a) x (c - a) points to -Y.
        let v = |i: u32| Vec3::from_array(part.positions[i as usize]);
        let (a, b, c) = (v(part.indices[0]), v(part.indices[1]), v(part.indices[2]));
        assert!((b - a).cross(c - a).y < 0.0, "front face must point down");
        assert!(part.positions.iter().all(|p| p[1].abs() < 1e-6));
    }

    fn placed(kind: Kind, center: Vec3, half: Vec3, yaw: f32) -> Placed {
        Placed { kind, center, half, rot: bevy::math::Quat::from_rotation_y(yaw), min: center - half, max: center + half }
    }

    #[test]
    fn mount_points_on_shell() {
        let ceiling = Placed {
            kind: Kind::Ceiling,
            center: Vec3::new(0.0, 8.0, -10.0),
            half: Vec3::new(6.0, 0.0, 6.0),
            rot: bevy::math::Quat::IDENTITY,
            min: Vec3::new(-6.0, 8.0, -16.0),
            max: Vec3::new(6.0, 8.0, -4.0),
        };
        let column = placed(Kind::Column, Vec3::new(2.0, 4.0, -10.0), Vec3::new(0.2, 4.0, 0.2), 0.0);
        // Wall along x at z = -16, 12 m long; the room is on its +z side.
        let wall = placed(Kind::Wall, Vec3::new(0.0, 4.0, -16.0), Vec3::new(6.0, 4.0, 0.1), 0.0);
        let opts = MountOpts { spacing_m: 3.0, ..Default::default() };
        let m = mounts_for(&[ceiling, column, wall], Vec3::new(0.0, 0.0, -10.0), &opts);
        let of = |k: MountKind| m.iter().filter(move |p| p.kind == k).collect::<Vec<_>>();
        let c = of(MountKind::Ceiling);
        assert_eq!(c.len(), 16, "4 x 4 grid under a 12 x 12 m ceiling");
        assert!(c.iter().all(|p| (p.pos.y - 7.7).abs() < 1e-5 && p.facing == Vec3::NEG_Y));
        let col = of(MountKind::Column);
        assert_eq!(col.len(), 4);
        for p in &col {
            assert!((p.pos.y - 2.5).abs() < 1e-5);
            let out = Vec2::new(p.pos.x - 2.0, p.pos.z + 10.0);
            assert!((out.length() - 0.25).abs() < 1e-4, "5 cm off a 0.4 m column");
            assert!(out.normalize().dot(Vec2::new(p.facing.x, p.facing.z)) > 0.999, "faces outwards");
        }
        let w = of(MountKind::Wall);
        assert_eq!(w.len(), 4, "every 3 m along 12 m");
        assert!(w.iter().all(|p| p.facing.z > 0.99 && (p.pos.z + 15.85).abs() < 1e-4), "inner face, facing into the room");
    }

    #[test]
    fn mounts_stay_under_a_low_ceiling() {
        let ceiling = placed(Kind::Ceiling, Vec3::new(0.0, 2.4, 0.0), Vec3::new(3.0, 0.0, 3.0), 0.0);
        let column = placed(Kind::Column, Vec3::ZERO, Vec3::new(0.2, 1.2, 0.2), 0.0);
        let m = mounts_for(&[ceiling, column], Vec3::new(5.0, 0.0, 5.0), &MountOpts::default());
        assert!(m.iter().filter(|p| p.kind == MountKind::Column).all(|p| (p.pos.y - 2.1).abs() < 1e-5));
    }
}
