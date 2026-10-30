//! Room-shell pieces (floor slab, walls, columns) as tiny generated USD files.
//! They go through the normal asset path, so selection, layouts, USDA export
//! and the self-check treat them like any other asset.

use std::path::PathBuf;

use anyhow::Result;
use bevy::math::{Vec2, Vec3};

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
            Self::Floor => [0.42, 0.43, 0.44],
            Self::Wall => [0.72, 0.73, 0.75],
            Self::Column => [0.16, 0.26, 0.52],
            Self::Block => [0.6, 0.6, 0.6],
            Self::Ceiling => [0.78, 0.79, 0.8],
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
    if !path.exists() {
        std::fs::create_dir_all(&dir)?;
        let text = if kind == Kind::Ceiling { ceiling_usda(size) } else { box_usda(kind, size) };
        std::fs::write(&path, text)?;
    }
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
}

/// The room shell implied by a perceived photo: the visible floor, the fitted
/// walls and detected columns.
pub fn shell_from_photo(sc: &PhotoScene) -> Vec<Piece> {
    let mut out = Vec::new();
    if let Some(f) = sc.floor {
        // Grow the visible floor a little so objects at its edge stand on it.
        let (w, d) = ((f[2] - f[0]).abs() + 2.0, (f[3] - f[1]).abs() + 2.0);
        out.push(Piece {
            kind: Kind::Floor,
            size: Vec3::new(w, 0.02, d),
            center: Vec2::new((f[0] + f[2]) * 0.5, (f[1] + f[3]) * 0.5),
            base_y: -0.02,
            yaw: 0.0,
        });
    }
    for w in &sc.walls {
        let d = w.b - w.a;
        let len = d.length();
        if len < 1.0 {
            continue;
        }
        out.push(Piece {
            kind: Kind::Wall,
            size: Vec3::new(len, w.height.max(2.5), 0.2),
            center: (w.a + w.b) * 0.5,
            base_y: 0.0,
            // Local +X along the wall: axis_x(yaw) = (cos, -sin).
            yaw: (-d.y).atan2(d.x),
        });
    }
    for o in &sc.objects {
        let l = o.label.to_ascii_lowercase();
        if !(l.contains("column") || l.contains("pillar") || l.contains("post")) {
            continue;
        }
        if let Some(fp) = o.footprint {
            let side = o.lateral_width.clamp(0.15, 1.5);
            // Columns run to the roof; the photo often shows only part of them.
            let height = sc.ceiling_y.unwrap_or(0.0).max(fp.base_y + fp.height).max(3.0);
            let view = fp.center.normalize_or_zero();
            out.push(Piece {
                kind: Kind::Column,
                size: Vec3::new(side, height, side),
                // The visible face is the front: center is half a side behind it.
                center: fp.center + view * side * 0.5,
                base_y: 0.0,
                yaw: sc.dominant_yaw,
            });
        }
    }
    if let (Some(h), Some(f)) = (sc.ceiling_y, sc.floor) {
        let (w, d) = ((f[2] - f[0]).abs() + 2.0, (f[3] - f[1]).abs() + 2.0);
        out.push(Piece {
            kind: Kind::Ceiling,
            size: Vec3::new(w, 0.0, d),
            center: Vec2::new((f[0] + f[2]) * 0.5, (f[1] + f[3]) * 0.5),
            base_y: h,
            yaw: 0.0,
        });
    }
    out
}

/// Ceiling over `lo..hi` (x, z) at `height`, plus optional roof beams along
/// x every `beam_spacing` meters, 0.6 m under the ceiling.
pub fn ceiling_pieces(lo: Vec2, hi: Vec2, height: f32, beams: Option<f32>) -> Vec<Piece> {
    let size = hi - lo;
    let center = (lo + hi) * 0.5;
    let mut out = vec![Piece { kind: Kind::Ceiling, size: Vec3::new(size.x, 0.0, size.y), center, base_y: height, yaw: 0.0 }];
    if let Some(sp) = beams.filter(|s| *s >= 1.0) {
        let mut z = lo.y + sp * 0.5;
        while z < hi.y {
            out.push(Piece { kind: Kind::Beam, size: Vec3::new(size.x, 0.4, 0.2), center: Vec2::new(center.x, z), base_y: height - 0.6, yaw: 0.0 });
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
