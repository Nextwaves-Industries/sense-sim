//! Devices in the viewport: meshes, picking, dragging, placing, and the
//! overlay (links, ranges, trails, estimates, beams, zones).

use std::f32::consts::TAU;

use bevy::picking::mesh_picking::ray_cast::RayCastBackfaces;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use super::device::{Device, DeviceKind, Motion, Profiles, Zone, next_id};
use super::{DevicePlace, LinkKind, RtlsConfig, RtlsSelection, RtlsState};
use crate::scene::{HandleGizmos, OrbitCamera, PartOf, PlaceMode, Selection, UiBlock};

/// Materials of the device models: shared housing colours, plus a coloured
/// accent per kind (grey while the device is switched off).
#[derive(Resource)]
pub struct DeviceAssets {
    /// Model per (kind, variant).
    parts: Vec<(DeviceKind, Variant, Vec<(Handle<Mesh>, Mat, Transform)>)>,
    fixed: Vec<(Mat, Handle<StandardMaterial>)>,
    accents: Vec<(DeviceKind, Handle<StandardMaterial>, Handle<StandardMaterial>)>,
    disabled: Handle<StandardMaterial>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mat {
    /// White ABS / PC housing, label face stock.
    Body,
    /// Light grey radome / front cover.
    Shell,
    /// Dark grey plastic, buttons.
    Dark,
    /// Black anodised aluminium (Nextwaves NR155 housing).
    Anodized,
    /// Black acrylic antenna face (Nextwaves UltraSlim panels).
    Gloss,
    /// Bare aluminium / nickel: frames, connectors, label antenna foil.
    Metal,
    /// Cables, rubber feet.
    Rubber,
    /// Red connector cap.
    Red,
    /// Nextwaves logo blue.
    Logo,
    /// White silkscreen print.
    Print,
    Accent,
    Led,
}

const MATS: [Mat; 12] = [Mat::Body, Mat::Shell, Mat::Dark, Mat::Anodized, Mat::Gloss, Mat::Metal, Mat::Rubber, Mat::Red, Mat::Logo, Mat::Print, Mat::Accent, Mat::Led];

/// Which hardware a device's profile stands for, when a kind has several.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Default,
    /// Nextwaves NR155 with a tall UltraSlim CP6226 panel (dock doors).
    Portal,
    /// Nextwaves near-field desktop reader.
    Desktop,
}

const VARIANTS: [(DeviceKind, Variant); 2] = [(DeviceKind::RfidReader, Variant::Portal), (DeviceKind::RfidReader, Variant::Desktop)];

fn variant_of(d: &Device) -> Variant {
    let p = d.profile.to_ascii_lowercase();
    match d.kind {
        DeviceKind::RfidReader if p.contains("desk") || p.contains("near") => Variant::Desktop,
        DeviceKind::RfidReader if p.contains("portal") || p.contains("6226") => Variant::Portal,
        _ => Variant::Default,
    }
}

#[derive(Component)]
pub struct DeviceVisual(Variant);

/// A mesh piece of a device model.
#[derive(Component)]
pub struct DevicePart {
    accent: Option<bool>,
}

/// Box of `size` centred on the origin, its corners in the XY plane rounded
/// with radius `r` and its front/back (±Z) edges chamfered by `bevel` - the
/// shape of a moulded plastic housing.
fn rounded_box(size: Vec3, r: f32, bevel: f32) -> Mesh {
    let (hx, hy, hz) = (size.x * 0.5, size.y * 0.5, size.z * 0.5);
    let r = r.clamp(0.0005, hx.min(hy));
    let bevel = bevel.clamp(0.0, (hz * 0.9).min(r * 0.9));
    // Outline (counter-clockwise seen from +Z): corner centres and directions.
    const SEG: usize = 6;
    let mut outline: Vec<(Vec2, Vec2)> = Vec::new();
    for (i, c) in [Vec2::new(hx - r, hy - r), Vec2::new(-hx + r, hy - r), Vec2::new(-hx + r, -hy + r), Vec2::new(hx - r, -hy + r)].into_iter().enumerate() {
        for s in 0..=SEG {
            let a = (i as f32 + s as f32 / SEG as f32) * std::f32::consts::FRAC_PI_2;
            outline.push((c, Vec2::new(a.cos(), a.sin())));
        }
    }
    let n = outline.len();
    // Rings from back to front: (inset, z).
    let rings = [(bevel, -hz), (0.0, -hz + bevel), (0.0, hz - bevel), (bevel, hz)];
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut nor: Vec<[f32; 3]> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let ring_pt = |(inset, z): (f32, f32), (c, d): (Vec2, Vec2)| (c + d * (r - inset)).extend(z);
    for w in rings.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.1 - b.1).abs() < 1e-6 {
            continue;
        }
        // Normal in the (outward, z) plane, perpendicular to the band.
        let t = Vec2::new(a.0 - b.0, b.1 - a.1);
        let nn = Vec2::new(t.y, -t.x).normalize();
        let base = pos.len() as u32;
        for &o in &outline {
            for ring in [a, b] {
                pos.push(ring_pt(ring, o).to_array());
                nor.push((o.1 * nn.x).extend(nn.y).normalize().to_array());
            }
        }
        for i in 0..n as u32 - 1 {
            let (a0, b0, a1, b1) = (base + 2 * i, base + 2 * i + 1, base + 2 * i + 2, base + 2 * i + 3);
            idx.extend([a0, a1, b1, a0, b1, b0]);
        }
        let (a0, b0) = (base + 2 * (n as u32 - 1), base + 2 * (n as u32 - 1) + 1);
        idx.extend([a0, base, base + 1, a0, base + 1, b0]);
    }
    // Flat caps.
    for (ring, sign) in [(rings[0], -1.0f32), (rings[3], 1.0)] {
        let centre = pos.len() as u32;
        pos.push([0.0, 0.0, ring.1]);
        nor.push([0.0, 0.0, sign]);
        for &o in &outline {
            pos.push(ring_pt(ring, o).to_array());
            nor.push([0.0, 0.0, sign]);
        }
        for i in 0..n as u32 {
            let (p, q) = (centre + 1 + i, centre + 1 + (i + 1) % n as u32);
            if sign > 0.0 { idx.extend([centre, p, q]) } else { idx.extend([centre, q, p]) }
        }
    }
    let uv = vec![[0.0f32, 0.0]; pos.len()];
    Mesh::new(bevy::mesh::PrimitiveTopology::TriangleList, bevy::asset::RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nor)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_indices(bevy::mesh::Indices::U32(idx))
}

/// A cable / rod of radius `r` through `points`, with rounded joints.
fn tube(points: &[Vec3], r: f32) -> Vec<Mesh> {
    let mut out = Vec::new();
    for w in points.windows(2) {
        let d = w[1] - w[0];
        let rot = Quat::from_rotation_arc(Vec3::Y, d.normalize());
        out.push(Mesh::from(Cylinder::new(r, d.length())).transformed_by(Transform::from_translation((w[0] + w[1]) * 0.5).with_rotation(rot)));
    }
    for p in &points[1..points.len() - 1] {
        out.push(Sphere::new(r).mesh().uv(10, 6).transformed_by(Transform::from_translation(*p)));
    }
    out
}

/// Models of real hardware, in the device frame: the antenna / front faces
/// -Z (the device's facing), the mounting surface is behind (+Z, 6 cm away
/// for fixed devices). One merged mesh per material.
fn model(kind: DeviceKind, variant: Variant, meshes: &mut Assets<Mesh>) -> Vec<(Handle<Mesh>, Mat, Transform)> {
    let mut parts: Vec<(Mesh, Mat)> = Vec::new();
    let mut add = |mesh: Mesh, mat: Mat, t: Transform| parts.push((mesh.transformed_by(t), mat));
    let at = |x: f32, y: f32, z: f32| Transform::from_xyz(x, y, z);
    let along_z = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
    let rbox = |x: f32, y: f32, z: f32, r: f32, b: f32| rounded_box(Vec3::new(x, y, z), r, b);
    let disc = |r: f32, h: f32| Mesh::from(Cylinder::new(r, h));
    let ball = |r: f32| Sphere::new(r).mesh().uv(12, 8);
    match kind {
        // Ceiling/wall anchor (Sewio / Nextwaves style): moulded white housing
        // on a wall bracket, grey antenna window, status LED, PoE cable gland.
        DeviceKind::UwbAnchor => {
            add(rbox(0.085, 0.13, 0.005, 0.01, 0.0015), Mat::Dark, at(0.0, 0.0, 0.0555));
            add(rbox(0.12, 0.155, 0.036, 0.022, 0.007), Mat::Body, at(0.0, 0.0, 0.035));
            add(rbox(0.088, 0.088, 0.004, 0.016, 0.0015), Mat::Shell, at(0.0, 0.018, 0.0165));
            add(rbox(0.046, 0.006, 0.0015, 0.003, 0.0005), Mat::Accent, at(0.0, -0.046, 0.0168));
            add(ball(0.0035), Mat::Led, at(0.0, -0.06, 0.0172));
            add(disc(0.0075, 0.014), Mat::Dark, at(0.0, -0.083, 0.036));
            for m in tube(&[Vec3::new(0.0, -0.09, 0.036), Vec3::new(0.0, -0.11, 0.045), Vec3::new(0.0, -0.13, 0.054), Vec3::new(0.0, -0.4, 0.055)], 0.0035) {
                add(m, Mat::Rubber, Transform::IDENTITY);
            }
        }
        // Worker badge (credit-card size) on a clip: white case, coloured
        // front, button, LED, lanyard slot.
        DeviceKind::UwbTag => {
            add(rbox(0.054, 0.086, 0.009, 0.007, 0.0025), Mat::Body, Transform::IDENTITY);
            add(rbox(0.046, 0.058, 0.0012, 0.005, 0.0004), Mat::Accent, at(0.0, -0.008, -0.0047));
            add(disc(0.0055, 0.0016), Mat::Dark, at(0.0, 0.029, -0.0045).with_rotation(along_z));
            add(ball(0.0018), Mat::Led, at(0.016, 0.031, -0.0045));
            add(rbox(0.018, 0.0035, 0.0094, 0.0017, 0.0), Mat::Dark, at(0.0, 0.0385, 0.0));
            add(rbox(0.012, 0.04, 0.0015, 0.003, 0.0005), Mat::Metal, at(0.0, 0.016, 0.0055));
        }
        // Round beacon puck with a domed top on an adhesive pad.
        DeviceKind::BleNode => {
            add(disc(0.03, 0.003), Mat::Dark, at(0.0, 0.0, 0.0575).with_rotation(along_z));
            add(disc(0.042, 0.01), Mat::Body, at(0.0, 0.0, 0.051).with_rotation(along_z));
            add(ball(0.042), Mat::Body, at(0.0, 0.0, 0.046).with_scale(Vec3::new(1.0, 1.0, 0.33)));
            add(Torus::new(0.0395, 0.0425).into(), Mat::Accent, at(0.0, 0.0, 0.046).with_rotation(along_z));
            add(ball(0.003), Mat::Led, at(0.0, 0.0, 0.032));
        }
        // Key fob: rounded case, coloured face, button, eyelet and key ring.
        DeviceKind::BleTag => {
            add(rbox(0.036, 0.05, 0.011, 0.013, 0.0035), Mat::Body, Transform::IDENTITY);
            add(rbox(0.03, 0.036, 0.001, 0.01, 0.0003), Mat::Accent, at(0.0, -0.003, -0.0055));
            add(disc(0.007, 0.0016), Mat::Dark, at(0.0, -0.003, -0.006).with_rotation(along_z));
            add(disc(0.0035, 0.0115), Mat::Dark, at(0.0, 0.019, 0.0).with_rotation(along_z));
            add(Torus::new(0.0095, 0.011).into(), Mat::Metal, at(0.0, 0.03, 0.0).with_rotation(Quat::from_rotation_y(0.4) * along_z));
        }
        // Nextwaves near-field desktop reader: 155 x 155 x 24 mm matte black
        // rounded box (lid over a base, rubber feet), USB-C and a status LED
        // on one edge. Its reading face is the front (-Z): lay it face up.
        DeviceKind::RfidReader if variant == Variant::Desktop => {
            let (zc, d) = (0.048, 0.024);
            add(rbox(0.155, 0.155, d - 0.006, 0.02, 0.002), Mat::Dark, at(0.0, 0.0, zc + 0.003));
            add(rbox(0.153, 0.153, 0.0065, 0.019, 0.0028), Mat::Anodized, at(0.0, 0.0, zc - d * 0.5 + 0.0032));
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                add(disc(0.007, 0.003), Mat::Rubber, at(x * 0.058, y * 0.058, zc + d * 0.5 + 0.0012).with_rotation(along_z));
            }
            add(rbox(0.009, 0.004, 0.0032, 0.0016, 0.0), Mat::Rubber, at(0.015, -0.0776, zc + 0.002));
            add(ball(0.0012), Mat::Led, at(-0.01, -0.0776, zc + 0.002));
            add(rbox(0.04, 0.004, 0.0003, 0.001, 0.0), Mat::Print, at(0.0, -0.06, zc - d * 0.5 - 0.0001));
            for m in tube(&[Vec3::new(0.015, -0.083, zc + 0.002), Vec3::new(0.015, -0.11, zc + 0.006), Vec3::new(0.03, -0.3, zc + 0.01)], 0.0022) {
                add(m, Mat::Rubber, Transform::IDENTITY);
            }
        }
        // Nextwaves fixed reader: UltraSlim panel antenna (black acrylic face
        // in a thin aluminium frame, corner screws, logo, red SMA connector)
        // on a wall spacer - CP2525 (258 x 258 x 14.5 mm) or, for portals,
        // CP6226 (260 x 620 x 11 mm) - and an NR155 reader below it:
        // 155 x 155 x 32 mm black anodised extrusion with side fins, mounting
        // flanges, four RP-TNC ports (ANT1-4) on top, LEDs, DC + Ethernet
        // underneath, coax from the antenna to ANT1.
        DeviceKind::RfidReader => {
            let (pw, ph, pt) = if variant == Variant::Portal { (0.26, 0.62, 0.011) } else { (0.258, 0.258, 0.0145) };
            let pz = 0.046 - pt * 0.5;
            let front = pz - pt * 0.5;
            add(rbox(0.1, 0.1, 0.014, 0.004, 0.001), Mat::Anodized, at(0.0, 0.0, 0.053));
            add(rbox(pw - 0.004, ph - 0.004, pt, 0.003, 0.0012), Mat::Gloss, at(0.0, 0.0, pz));
            add(rbox(pw, ph, pt * 0.7, 0.004, 0.0), Mat::Metal, at(0.0, 0.0, pz));
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                add(disc(0.004, 0.0008), Mat::Dark, at(x * (pw * 0.5 - 0.016), y * (ph * 0.5 - 0.016), front - 0.0002).with_rotation(along_z));
            }
            // Logo: striped globe and the word mark, bottom right.
            let (lx, ly) = (pw * 0.5 - 0.05, -ph * 0.5 + 0.038);
            add(disc(0.0085, 0.0006), Mat::Logo, at(lx, ly + 0.012, front - 0.0002).with_rotation(along_z));
            add(rbox(0.05, 0.0055, 0.0005, 0.001, 0.0), Mat::Print, at(lx, ly, front - 0.0002));
            let cy = -ph * 0.5 - 0.006;
            add(disc(0.0042, 0.012), Mat::Red, at(0.0, cy, pz));
            let ry = -ph * 0.5 - 0.17;
            let (rz, top) = (0.041, ry + 0.0775);
            add(rbox(0.155, 0.155, 0.032, 0.003, 0.0015), Mat::Anodized, at(0.0, ry, rz));
            for s in [-1.0f32, 1.0] {
                for i in 0..9 {
                    add(Cuboid::new(0.0016, 0.148, 0.0014).into(), Mat::Anodized, at(s * 0.0783, ry, rz - 0.012 + i as f32 * 0.003));
                }
                add(rbox(0.024, 0.13, 0.0022, 0.002, 0.0), Mat::Anodized, at(s * 0.088, ry, 0.0589));
                for y in [-0.045, 0.045] {
                    add(disc(0.0035, 0.002), Mat::Metal, at(s * 0.091, ry + y, 0.0572).with_rotation(along_z));
                }
            }
            for i in 0..4 {
                let x = -0.0525 + i as f32 * 0.035;
                add(Cylinder::new(0.0078, 0.004).mesh().resolution(6).build(), Mat::Metal, at(x, top + 0.002, rz));
                add(disc(0.0056, 0.016), Mat::Metal, at(x, top + 0.012, rz));
                add(rbox(0.009, 0.0022, 0.0003, 0.0008, 0.0), Mat::Print, at(x, top - 0.008, rz - 0.0161));
                add(ball(0.0016), Mat::Led, at(0.052 - i as f32 * 0.009, ry - 0.064, rz - 0.016));
            }
            add(disc(0.0115, 0.0005), Mat::Print, at(0.0, ry + 0.012, rz - 0.0161).with_rotation(along_z));
            add(rbox(0.07, 0.0045, 0.0003, 0.001, 0.0), Mat::Print, at(0.0, ry - 0.01, rz - 0.0161));
            add(disc(0.0065, 0.012), Mat::Metal, at(-0.0525, top + 0.026, rz));
            let coax = [Vec3::new(0.0, cy - 0.006, pz), Vec3::new(0.0, cy - 0.03, 0.052), Vec3::new(-0.045, cy - 0.07, 0.054), Vec3::new(-0.0525, top + 0.05, 0.047), Vec3::new(-0.0525, top + 0.032, rz)];
            for m in tube(&coax, 0.0035) {
                add(m, Mat::Rubber, Transform::IDENTITY);
            }
            let bottom = ry - 0.0775;
            add(rbox(0.016, 0.006, 0.014, 0.001, 0.0), Mat::Metal, at(0.03, bottom - 0.003, rz));
            add(disc(0.005, 0.01), Mat::Dark, at(0.055, bottom - 0.005, rz));
            for (x, r) in [(0.03, 0.003), (0.055, 0.0025)] {
                for m in tube(&[Vec3::new(x, bottom - 0.006, rz), Vec3::new(x, bottom - 0.03, 0.05), Vec3::new(x, bottom - 0.4, 0.055)], r) {
                    add(m, Mat::Rubber, Transform::IDENTITY);
                }
            }
        }
        // Nextwaves pre-adhesive label, 125 x 22 mm white face, with the
        // Quanray Qstar 7U wet inlay (95 x 8 mm): two long hollow arms and the
        // meandered strap around the chip in the middle.
        DeviceKind::RfidTag => {
            let (t, z) = (0.00012, -0.00022);
            add(rbox(0.125, 0.022, 0.0003, 0.0025, 0.0), Mat::Body, Transform::IDENTITY);
            let mut foil = |x: f32, y: f32, w: f32, h: f32| add(Cuboid::new(w, h, t).into(), Mat::Metal, at(x, y, z));
            let (hy, lw) = (0.004, 0.0008);
            for s in [-1.0f32, 1.0] {
                // Hollow arm from x = 13 to 47.5 mm.
                let (x0, x1) = (0.013, 0.0475);
                foil(s * (x0 + x1) * 0.5, hy - lw * 0.5, x1 - x0, lw);
                foil(s * (x0 + x1) * 0.5, -hy + lw * 0.5, x1 - x0, lw);
                foil(s * (x1 - lw * 0.5), 0.0, lw, 2.0 * hy);
                foil(s * (x0 + 0.004), 0.0, lw, 2.0 * hy - 2.0 * lw);
                // Meander from the arm in to the strap.
                for k in 0..3 {
                    let x = s * (0.0115 - k as f32 * 0.0022);
                    foil(x, 0.0, 0.0006, 0.0062);
                    let y = if k % 2 == 0 { 0.0031 } else { -0.0031 };
                    foil(x - s * 0.0011, y, 0.0028, 0.0006);
                }
                foil(s * 0.0035, 0.0012, 0.0036, 0.0006);
            }
            // Strap loop around the chip.
            foil(0.0, -0.0022, 0.009, 0.0006);
            foil(-0.0045, -0.0005, 0.0006, 0.0034);
            foil(0.0045, -0.0005, 0.0006, 0.0034);
            add(Cuboid::new(0.0008, 0.0008, 0.0003).into(), Mat::Dark, at(0.0, 0.0012, z - 0.0002));
        }
    }
    MATS.iter()
        .filter_map(|&mat| {
            let mut it = parts.iter().filter(|(_, m)| *m == mat).map(|(mesh, _)| mesh);
            let mut merged = it.next()?.clone();
            for mesh in it {
                let _ = merged.merge(mesh);
            }
            Some((meshes.add(merged), mat, Transform::IDENTITY))
        })
        .collect()
}

pub fn setup_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let parts = DeviceKind::ALL
        .iter()
        .map(|k| (*k, Variant::Default))
        .chain(VARIANTS)
        .map(|(k, v)| (k, v, model(k, v, &mut meshes)))
        .collect();
    let mut mat = |c: Color, metallic: f32, roughness: f32| materials.add(StandardMaterial { base_color: c, metallic, perceptual_roughness: roughness, ..default() });
    let fixed = vec![
        (Mat::Body, mat(Color::srgb(0.93, 0.93, 0.91), 0.0, 0.45)),
        (Mat::Shell, mat(Color::srgb(0.78, 0.79, 0.8), 0.0, 0.6)),
        (Mat::Dark, mat(Color::srgb(0.06, 0.06, 0.065), 0.0, 0.55)),
        (Mat::Anodized, mat(Color::srgb(0.05, 0.05, 0.055), 0.7, 0.42)),
        (Mat::Gloss, mat(Color::srgb(0.02, 0.025, 0.025), 0.0, 0.12)),
        (Mat::Metal, mat(Color::srgb(0.8, 0.81, 0.83), 1.0, 0.32)),
        (Mat::Rubber, mat(Color::srgb(0.03, 0.03, 0.035), 0.0, 0.75)),
        (Mat::Red, mat(Color::srgb(0.75, 0.06, 0.05), 0.0, 0.4)),
        (Mat::Logo, mat(Color::srgb(0.1, 0.55, 0.78), 0.0, 0.35)),
        (Mat::Print, mat(Color::srgb(0.85, 0.86, 0.86), 0.0, 0.6)),
    ];
    let accents = DeviceKind::ALL
        .iter()
        .map(|k| {
            let c = k.color();
            let accent = materials.add(StandardMaterial { base_color: c, emissive: c.to_linear() * 0.25, perceptual_roughness: 0.45, ..default() });
            let led = materials.add(StandardMaterial { base_color: c, emissive: c.to_linear() * 4.0, unlit: true, ..default() });
            (*k, accent, led)
        })
        .collect();
    let disabled = materials.add(StandardMaterial { base_color: Color::srgb(0.35, 0.35, 0.38), perceptual_roughness: 0.8, ..default() });
    commands.insert_resource(DeviceAssets { parts, fixed, accents, disabled });
}

pub fn ensure_visuals(
    mut commands: Commands,
    assets: Option<Res<DeviceAssets>>,
    added: Query<(Entity, &Device), Without<DeviceVisual>>,
    changed: Query<(Entity, &Device, &DeviceVisual, &Children), Changed<Device>>,
    mut pieces: Query<(&DevicePart, &mut MeshMaterial3d<StandardMaterial>)>,
) {
    let Some(a) = assets else { return };
    let accent = |d: &Device, led: bool| -> Handle<StandardMaterial> {
        if !d.enabled {
            return a.disabled.clone();
        }
        let (_, acc, l) = a.accents.iter().find(|(k, _, _)| *k == d.kind).unwrap();
        if led { l.clone() } else { acc.clone() }
    };
    let spawn = |commands: &mut Commands, e: Entity, d: &Device| {
        let v = variant_of(d);
        let parts = &a.parts.iter().find(|(k, pv, _)| *k == d.kind && *pv == v).unwrap().2;
        let pieces: Vec<_> = parts
            .iter()
            .map(|(mesh, mat, t)| {
                let (material, part) = match mat {
                    Mat::Accent => (accent(d, false), DevicePart { accent: Some(false) }),
                    Mat::Led => (accent(d, true), DevicePart { accent: Some(true) }),
                    m => (a.fixed.iter().find(|(f, _)| f == m).unwrap().1.clone(), DevicePart { accent: None }),
                };
                (Mesh3d(mesh.clone()), MeshMaterial3d(material), *t, part, bevy::picking::Pickable::IGNORE)
            })
            .collect();
        // Silenced: Auto deploy may remove the device before this applies.
        commands.entity(e).queue_silenced(move |mut ent: EntityWorldMut| {
            ent.insert((DeviceVisual(v), Visibility::Visible)).with_children(|c| {
                for p in pieces {
                    c.spawn(p);
                }
            });
        });
    };
    for (e, d) in &added {
        spawn(&mut commands, e, d);
    }
    for (e, d, vis, children) in &changed {
        // A new profile can mean different hardware: rebuild the model.
        if variant_of(d) != vis.0 {
            for c in children.iter() {
                if pieces.contains(c) {
                    commands.entity(c).despawn();
                }
            }
            spawn(&mut commands, e, d);
            continue;
        }
        // Switching a device off greys its accents.
        for c in children.iter() {
            if let Ok((p, mut m)) = pieces.get_mut(c)
                && let Some(led) = p.accent
            {
                let want = accent(d, led);
                if m.0 != want {
                    m.0 = want;
                }
            }
        }
    }
}

/// USD meshes can be single-sided: let radio rays hit both faces.
pub fn raycast_backfaces(mut commands: Commands, parts: Query<Entity, (With<PartOf>, Without<RayCastBackfaces>)>) {
    for e in &parts {
        commands.entity(e).insert(RayCastBackfaces);
    }
}

pub fn spawn_device(commands: &mut Commands, device: Device, transform: Transform) -> Entity {
    let name = Name::new(format!("{} {}", device.kind.label(), device.id));
    commands.spawn((device, transform, name)).id()
}

fn ray_plane_y(ray: Ray3d, y: f32) -> Option<Vec3> {
    let d = ray.direction.y;
    if d.abs() < 1e-6 {
        return None;
    }
    let t = (y - ray.origin.y) / d;
    (t > 0.0).then(|| ray.origin + ray.direction * t)
}

#[derive(Default)]
pub struct PointerDrag {
    entity: Option<Entity>,
    offset: Vec3,
    plane_y: f32,
    vertical: bool,
    /// Cursor at press; nothing moves until it has travelled a few pixels,
    /// so a click only selects.
    press: Vec2,
    moved: bool,
}

/// Runs before the scene's `interact`: places, picks and drags devices and
/// consumes the click so the scene does not also act on it.
#[allow(clippy::too_many_arguments)]
pub fn device_pointer(
    mut commands: Commands,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform, &mut OrbitCamera)>,
    mut devices: Query<(Entity, &mut Device, &mut Transform), Without<OrbitCamera>>,
    parts: super::sim::OccluderParts,
    objects: Query<(Entity, &crate::scene::PlacedObject, &Transform), Without<Device>>,
    block: Res<UiBlock>,
    usd_place: Res<PlaceMode>,
    mut place: ResMut<DevicePlace>,
    mut rsel: ResMut<RtlsSelection>,
    mut selection: ResMut<Selection>,
    profiles: Res<Profiles>,
    mut drag: Local<PointerDrag>,
) {
    let (cam, cam_gt, mut orbit) = camera.into_inner();
    let cursor = window.cursor_position();
    let ray = cursor.and_then(|c| cam.viewport_to_world(cam_gt, c).ok());
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if usd_place.ghost.is_some() {
        place.profile = None;
        place.waypoints_for = None;
    }

    // ---- Placing devices
    if let Some(pid) = place.profile.clone() {
        let Some(profile) = profiles.get(&pid).cloned() else {
            place.profile = None;
            return;
        };
        place.preview = ray.filter(|_| !block.pointer).and_then(|ray| {
            let occ = super::sim::Occluders::build(&parts, &objects);
            match occ.nearest(ray, 500.0) {
                Some((t, n)) => {
                    let point = ray.origin + *ray.direction * t;
                    // Face the viewer whichever side of the face was hit.
                    let n = if n.dot(*ray.direction) > 0.0 { -n } else { n };
                    if n.y > 0.7 {
                        // On top of something (shelf, table): sit on it.
                        Some(point + Vec3::Y * 0.05)
                    } else {
                        // On a wall/side: mount on the face.
                        Some(point + n * 0.06)
                    }
                }
                None => ray_plane_y(ray, 0.0).map(|p| Vec3::new(p.x, profile.mount_height_m, p.z)),
            }
        });
        if !block.pointer && mouse.just_pressed(MouseButton::Left) {
            mouse.clear_just_pressed(MouseButton::Left);
            if let Some(p) = place.preview {
                let id = next_id(profile.kind, devices.iter().map(|(_, d, _)| d.id.as_str()).collect::<Vec<_>>().into_iter());
                let address = if profile.kind == DeviceKind::RfidTag { super::device::epc_for(&id) } else { String::new() };
                // Face the camera so readers point into the room.
                let mut t = Transform::from_translation(p);
                let to_cam = (cam_gt.translation() - p).with_y(0.0);
                if to_cam.length_squared() > 1e-4 {
                    t.look_to(-to_cam.normalize(), Vec3::Y);
                }
                let e = spawn_device(
                    &mut commands,
                    Device { id, kind: profile.kind, profile: profile.id.clone(), enabled: true, motion: Motion::Static, address, auto: false, radio: Default::default() },
                    t,
                );
                rsel.device = Some(e);
                selection.selected = None;
            }
            if !(shift || place.sticky) {
                place.profile = None;
            }
        }
        if (!block.pointer && mouse.just_pressed(MouseButton::Right)) || (!block.keyboard && keys.just_pressed(KeyCode::Escape)) {
            mouse.clear_just_pressed(MouseButton::Right);
            place.profile = None;
        }
        return;
    }
    place.preview = None;

    // ---- Adding waypoints by clicking the floor
    if let Some(tag) = place.waypoints_for {
        let Ok((_, mut dev, t)) = devices.get_mut(tag) else {
            place.waypoints_for = None;
            return;
        };
        let y = t.translation.y;
        place.preview = ray.filter(|_| !block.pointer).and_then(|r| ray_plane_y(r, y));
        if !block.pointer && mouse.just_pressed(MouseButton::Left) {
            mouse.clear_just_pressed(MouseButton::Left);
            if let Some(p) = place.preview {
                match &mut dev.motion {
                    Motion::Path { points, .. } => points.push(p.to_array()),
                    m => *m = Motion::Path { points: vec![t.translation.to_array(), p.to_array()], speed: 1.2, ping_pong: false },
                }
            }
        }
        if (!block.pointer && mouse.just_pressed(MouseButton::Right)) || (!block.keyboard && (keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::Enter))) {
            mouse.clear_just_pressed(MouseButton::Right);
            place.waypoints_for = None;
        }
        return;
    }

    // ---- Dragging
    if let Some(e) = drag.entity {
        if !mouse.pressed(MouseButton::Left) {
            drag.entity = None;
            return;
        }
        if !drag.moved {
            if cursor.is_none_or(|c| c.distance(drag.press) < 4.0) {
                return;
            }
            drag.moved = true;
            // A tag put somewhere by hand stays there: stop the walk / wander /
            // ride it was doing, which would otherwise carry it away again.
            if let Ok((_, mut d, _)) = devices.get_mut(e)
                && !matches!(d.motion, Motion::Static)
            {
                d.motion = Motion::Static;
            }
        }
        let mobile = devices.get(e).is_ok_and(|(_, d, _)| d.kind.is_mobile());
        // Worn / carried tags (UWB, BLE) dropped on the floor go to their carry
        // height (a badge on a person), not onto the floor; RFID labels stick.
        let carry = devices
            .get(e)
            .ok()
            .filter(|(_, d, _)| matches!(d.kind, DeviceKind::UwbTag | DeviceKind::BleTag))
            .map(|(_, d, _)| profiles.resolve(&d.profile, d.kind).mount_height_m);
        // Snap onto whatever is under the cursor (rack tops, shelf faces,
        // walls, ceilings); Shift drags on a plane at the current height.
        let snap = ray.filter(|_| !shift && !drag.vertical).and_then(|ray| {
            let (hit_t, n) = super::sim::Occluders::build(&parts, &objects).nearest(ray, 500.0)?;
            let n = if n.dot(*ray.direction) > 0.0 { -n } else { n };
            Some((ray.origin + *ray.direction * hit_t, n))
        });
        if let (Some(ray), Ok((_, _, mut t))) = (ray, devices.get_mut(e)) {
            if drag.vertical {
                // Height along a vertical line through the device.
                let to = (ray.origin - t.translation).with_y(0.0);
                if let Ok(n) = Dir3::new(to) {
                    let denom = ray.direction.dot(*n);
                    if denom.abs() > 1e-4 {
                        let s = (t.translation - ray.origin).dot(*n) / denom;
                        let p = ray.origin + *ray.direction * s;
                        t.translation.y = p.y.clamp(0.0, 50.0);
                    }
                }
            } else if let Some((hit, n)) = snap {
                if n.y > 0.7 {
                    // On top: sit on it - unless it is the floor and the tag is carried.
                    let lift = match carry {
                        Some(h) if hit.y < 0.25 => h.max(0.03),
                        _ if mobile => 0.03,
                        _ => 0.06,
                    };
                    t.translation = hit + Vec3::Y * lift;
                } else {
                    // On a face or under a ceiling: mount on it, facing away.
                    t.translation = hit + n * if mobile { 0.012 } else { 0.06 };
                    let up = if n.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
                    t.look_to(n, up);
                }
            } else if let Some(p) = ray_plane_y(ray, drag.plane_y) {
                let mut q = p + drag.offset;
                if ctrl {
                    q.x = (q.x / 0.25).round() * 0.25;
                    q.z = (q.z / 0.25).round() * 0.25;
                }
                t.translation.x = q.x;
                t.translation.z = q.z;
            }
        }
        return;
    }

    // ---- Hover / pick (screen space: devices are small)
    rsel.hovered = None;
    if block.pointer {
        return;
    }
    if let Some(c) = cursor {
        let mut best: Option<(f32, Entity)> = None;
        for (e, _, t) in devices.iter() {
            if let Ok(s) = cam.world_to_viewport(cam_gt, t.translation) {
                let d = s.distance(c);
                if d < 14.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, e));
                }
            }
        }
        rsel.hovered = best.map(|(_, e)| e);
    }
    if mouse.just_pressed(MouseButton::Left) {
        if let (Some(e), Some(ray)) = (rsel.hovered, ray) {
            mouse.clear_just_pressed(MouseButton::Left);
            rsel.device = Some(e);
            rsel.zone = None;
            selection.selected = None;
            if let Ok((_, _, t)) = devices.get(e) {
                let y = t.translation.y;
                let p = ray_plane_y(ray, y).unwrap_or(t.translation);
                *drag = PointerDrag { entity: Some(e), offset: (t.translation - p).with_y(0.0), plane_y: y, vertical: alt, press: cursor.unwrap_or_default(), moved: false };
            }
        } else {
            rsel.device = None;
        }
    }

    // ---- Keys for the selected device
    if block.keyboard {
        return;
    }
    if let Some(sel) = rsel.device {
        if keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace) {
            commands.entity(sel).despawn();
            rsel.device = None;
        } else if keys.just_pressed(KeyCode::Escape) {
            rsel.device = None;
        } else if ctrl && keys.just_pressed(KeyCode::KeyD) {
            if let Ok((_, d, t)) = devices.get(sel) {
                let mut d = d.clone();
                d.id = next_id(d.kind, devices.iter().map(|(_, d, _)| d.id.as_str()).collect::<Vec<_>>().into_iter());
                if d.kind == DeviceKind::RfidTag {
                    d.address = super::device::epc_for(&d.id);
                }
                let mut t = *t;
                t.translation.x += 0.5;
                rsel.device = Some(spawn_device(&mut commands, d, t));
            }
        } else if keys.just_pressed(KeyCode::KeyG)
            && let Ok((_, _, t)) = devices.get(sel)
        {
            orbit.focus = t.translation;
            orbit.radius = orbit.radius.min(6.0);
        } else if !ctrl && (keys.just_pressed(KeyCode::KeyQ) || keys.just_pressed(KeyCode::KeyE))
            && let Ok((_, _, mut t)) = devices.get_mut(sel)
        {
            let step = if shift { std::f32::consts::FRAC_PI_2 } else { std::f32::consts::PI / 12.0 };
            t.rotate_y(if keys.just_pressed(KeyCode::KeyQ) { step } else { -step });
        }
    }
}

/// Selecting a USD object deselects the device and vice versa.
pub fn sync_selection(selection: Res<Selection>, mut rsel: ResMut<RtlsSelection>, devices: Query<(), With<Device>>, zones: Query<(), With<Zone>>) {
    if selection.is_changed() && selection.selected.is_some() {
        rsel.device = None;
        rsel.zone = None;
    }
    if rsel.device.is_some_and(|e| !devices.contains(e)) {
        rsel.device = None;
    }
    if rsel.zone.is_some_and(|e| !zones.contains(e)) {
        rsel.zone = None;
    }
}

fn boxed(g: &mut Gizmos, min: Vec3, max: Vec3, color: Color) {
    let c = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let (a, b) = (min, max);
    let pts = [c(a.x, a.y, a.z), c(b.x, a.y, a.z), c(b.x, a.y, b.z), c(a.x, a.y, b.z), c(a.x, b.y, a.z), c(b.x, b.y, a.z), c(b.x, b.y, b.z), c(a.x, b.y, b.z)];
    for (i, j) in [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)] {
        g.line(pts[i], pts[j], color);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    mut g: Gizmos,
    mut hg: Gizmos<HandleGizmos>,
    cfg: Res<RtlsConfig>,
    state: Res<RtlsState>,
    profiles: Res<Profiles>,
    rsel: Res<RtlsSelection>,
    place: Res<DevicePlace>,
    devices: Query<(Entity, &Device, &Transform)>,
    zones: Query<(Entity, &Zone)>,
    hide: Res<crate::ui::HideOverlays>,
) {
    if hide.0 > 0 {
        return;
    }
    let s = &cfg.settings;
    let pos_of = |e: Entity| devices.get(e).ok().map(|(_, _, t)| t.translation);
    let green = Color::srgb(0.3, 0.95, 0.4);
    let red = Color::srgb(1.0, 0.3, 0.25);
    let amber = Color::srgb(1.0, 0.75, 0.2);
    let grey = Color::srgba(0.7, 0.7, 0.75, 0.35);

    // Infrastructure markers and RFID beams.
    for (e, d, t) in &devices {
        let p = t.translation;
        let col = if d.enabled { d.kind.color() } else { Color::srgb(0.4, 0.4, 0.4) };
        if !d.kind.is_mobile() {
            // Mast to the floor so height is readable.
            g.line(p, Vec3::new(p.x, 0.0, p.z), col.with_alpha(0.35));
            g.circle(Isometry3d::new(Vec3::new(p.x, 0.005, p.z), Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), 0.12, col.with_alpha(0.5));
        }
        if s.show_beams && d.kind == DeviceKind::RfidReader {
            let prof = profiles.for_device(d);
            let half = (prof.beam_deg.to_radians() * 0.5).min(1.5);
            let range = (prof.max_range_m * 0.6).min(8.0);
            let fwd = *t.forward();
            let up = *t.up();
            let right = *t.right();
            let ring_c = p + fwd * range * half.cos();
            let ring_r = range * half.sin();
            for k in 0..8 {
                let a = k as f32 / 8.0 * TAU;
                let dir = (fwd * half.cos() + (right * a.cos() + up * a.sin()) * half.sin()).normalize();
                g.line(p, p + dir * range, col.with_alpha(0.25));
            }
            g.circle(Isometry3d::new(ring_c, Quat::from_rotation_arc(Vec3::Z, fwd)), ring_r, col.with_alpha(0.4));
        }
        if rsel.device == Some(e) || rsel.hovered == Some(e) {
            let c = if rsel.device == Some(e) { Color::srgb(1.0, 0.69, 0.13) } else { Color::WHITE };
            hg.sphere(Isometry3d::from_translation(p), 0.16, c);
        }
    }

    // Tags: truth, estimate, error, links, trails.
    for (e, track) in &state.tracks {
        let Ok((_, dev, t)) = devices.get(*e) else { continue };
        let truth = t.translation;
        let selected = rsel.device == Some(*e);
        if s.show_truth {
            g.sphere(Isometry3d::from_translation(truth), 0.05, Color::WHITE.with_alpha(0.8));
        }
        if let Some(est) = track.est {
            let col = if track.lost { red } else { dev.kind.color() };
            let r = 0.12;
            g.line(est - Vec3::X * r, est + Vec3::X * r, col);
            g.line(est - Vec3::Z * r, est + Vec3::Z * r, col);
            g.circle(Isometry3d::new(est, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), r * 0.8, col);
            if s.show_truth {
                g.line(truth, est, Color::srgba(1.0, 0.4, 0.8, 0.8));
            }
        }
        if s.show_trails && track.trail.len() > 1 {
            g.linestrip(track.trail.iter().copied(), dev.kind.color().with_alpha(0.45));
        }
        if s.show_links || selected {
            for l in &track.links {
                let Some(a) = pos_of(l.other) else { continue };
                let col = match (l.value.is_some(), l.walls, l.used) {
                    (false, _, _) => grey,
                    (true, 0, true) => green,
                    (true, 0, false) => amber,
                    (true, _, _) => red,
                };
                if l.value.is_none() && !selected {
                    continue;
                }
                g.line(truth, a, col.with_alpha(if selected { 0.95 } else { 0.5 }));
            }
        }
        if (s.show_ranges || selected) && dev.kind == DeviceKind::UwbTag {
            for l in &track.links {
                if let (LinkKind::Range, Some(r), Some(a)) = (l.kind, l.value, pos_of(l.other)) {
                    // Horizontal circle at tag height with the measured range.
                    let dy = a.y - truth.y;
                    let rh = (r * r - dy * dy).max(0.0).sqrt();
                    let col = if l.walls > 0 { red } else { green };
                    g.circle(Isometry3d::new(Vec3::new(a.x, truth.y, a.z), Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), rh, col.with_alpha(0.35));
                }
            }
        }
    }

    // Paths of mobile devices (always for the selected one).
    for (e, d, t) in &devices {
        let selected = rsel.device == Some(e);
        if let Motion::Path { points, .. } = &d.motion
            && (selected || s.show_trails)
            && points.len() > 1
        {
            let col = if selected { Color::srgb(1.0, 0.69, 0.13) } else { Color::srgba(1.0, 0.69, 0.13, 0.3) };
            let mut pts: Vec<Vec3> = points.iter().map(|p| Vec3::from(*p)).collect();
            pts.push(pts[0]);
            g.linestrip(pts.iter().copied(), col);
            for p in &pts {
                g.sphere(Isometry3d::from_translation(*p), 0.04, col);
            }
        }
        if let Motion::Wander { center, radius, .. } = &d.motion
            && selected
        {
            g.circle(Isometry3d::new(Vec3::from(*center).with_y(t.translation.y), Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), *radius, amber);
        }
    }

    if s.show_zones {
        for (e, z) in &zones {
            let n = state.zone_occupancy.get(&z.name).copied().unwrap_or(0);
            let base = if z.restricted { red } else { Color::srgb(0.4, 0.75, 1.0) };
            let col = if n > 0 { base } else { base.with_alpha(0.45) };
            boxed(&mut g, Vec3::from(z.min), Vec3::from(z.max), col);
            if rsel.zone == Some(e) {
                boxed(&mut g, Vec3::from(z.min) - Vec3::splat(0.03), Vec3::from(z.max) + Vec3::splat(0.03), Color::srgb(1.0, 0.69, 0.13));
            }
        }
    }

    if let Some(p) = place.preview {
        let c = Color::srgb(1.0, 0.69, 0.13);
        hg.sphere(Isometry3d::from_translation(p), 0.1, c);
        hg.line(p, Vec3::new(p.x, 0.0, p.z), c.with_alpha(0.5));
    }
}
