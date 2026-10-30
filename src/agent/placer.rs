//! Deterministic placement of an asset from a photo measurement: position,
//! yaw and (only when clearly needed) scale, computed from the measured
//! footprint, the asset's real dimensions and the building axis.

use std::f32::consts::{FRAC_PI_2, PI};

use bevy::math::{Vec2, Vec3};

use super::vision::perceive::{PhotoObject, PhotoScene};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub translation: Vec3,
    pub yaw: f32,
    pub scale: f32,
}

/// Unit vector of local +X after a viewer yaw (rotation about +Y).
fn axis_x(yaw: f32) -> Vec2 {
    Vec2::new(yaw.cos(), -yaw.sin())
}

/// Unit vector of local +Z after a viewer yaw.
fn axis_z(yaw: f32) -> Vec2 {
    Vec2::new(yaw.sin(), yaw.cos())
}

/// Extent of an asset footprint (dims.x by dims.z, rotated by yaw) along `dir`.
fn extent_along(dims: Vec3, yaw: f32, dir: Vec2) -> f32 {
    dims.x * axis_x(yaw).dot(dir).abs() + dims.z * axis_z(yaw).dot(dir).abs()
}

fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(2.0 * PI) - PI
}

/// Angle difference modulo 90 degrees, in [0, 45].
fn diff_mod_quarter(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(FRAC_PI_2);
    d.min(FRAC_PI_2 - d)
}

/// Where the ray through normalized image point (u, v) meets the floor.
pub fn floor_point(scene: &PhotoScene, u: f32, v: f32) -> Option<Vec2> {
    let cam = &scene.camera;
    let c = Vec3::new((u - 0.5) / cam.fx_norm, (v - 0.5) / (cam.fx_norm * cam.aspect), 1.0);
    let f = &cam.frame;
    let dir = Vec3::new(c.dot(f.x), c.dot(f.y), c.dot(f.z));
    if dir.y >= -1e-4 {
        return None;
    }
    let t = f.h / -dir.y;
    Some(Vec2::new(dir.x * t, dir.z * t))
}

/// Computes the transform for `asset_dims` (bounding box size, Y-up meters)
/// standing in for photo object `obj`. `yaw_override` (radians) wins over
/// the measurement.
pub fn fit(scene: &PhotoScene, obj: &PhotoObject, asset_dims: Vec3, yaw_override: Option<f32>) -> Fit {
    let dims = asset_dims.max(Vec3::splat(0.01));
    let fp = obj.footprint;
    // Front-biased center of what the camera saw; fall back to where the box
    // bottom meets the floor.
    let seen = fp.map(|f| f.center).or_else(|| floor_point(scene, (obj.bbox[0] + obj.bbox[2]) * 0.5, obj.bbox[3])).unwrap_or(Vec2::new(0.0, -5.0));
    let view = seen.normalize_or(Vec2::new(0.0, -1.0));
    let across = Vec2::new(-view.y, view.x);
    let axis = scene.dominant_yaw;

    // Scale only when height or width is clearly off.
    let mut scale = 1.0;
    if let Some(f) = fp {
        let rh = f.height / dims.y;
        if !(0.67..=1.5).contains(&rh) && f.base_y == 0.0 && obj.bbox[1] > 0.02 {
            scale = rh.clamp(0.5, 2.0);
        }
    }
    let dims_s = dims * scale;

    let yaw = if let Some(y) = yaw_override {
        y
    } else {
        // Candidate yaws: the measured axis (if trustworthy, snapped to the
        // building axis when close), else the building axis and its normal.
        let measured = fp.filter(|f| !obj.radial && f.size.x > f.size.y * 1.3).map(|f| {
            if diff_mod_quarter(f.yaw, axis) < 15f32.to_radians() {
                let k = ((f.yaw - axis) / FRAC_PI_2).round();
                axis + k * FRAC_PI_2
            } else {
                f.yaw
            }
        });
        let mut cands: Vec<f32> = match measured {
            // The measured long axis: put the asset's long side along it.
            Some(y) => {
                if dims.z > dims.x {
                    vec![y + FRAC_PI_2, y - FRAC_PI_2]
                } else {
                    vec![y, y + PI]
                }
            }
            None => vec![axis, axis + FRAC_PI_2, axis + PI, axis - FRAC_PI_2],
        };
        // Prefer the orientation whose width across the view matches the photo,
        // then the one that faces the camera (+Z towards it).
        let target = obj.lateral_width;
        cands.sort_by(|a, b| {
            let ea = (extent_along(dims_s, *a, across) - target).abs();
            let eb = (extent_along(dims_s, *b, across) - target).abs();
            let fa = axis_z(*a).dot(-view);
            let fb = axis_z(*b).dot(-view);
            let score_a = if target > 0.0 { ea } else { 0.0 } - 0.05 * fa;
            let score_b = if target > 0.0 { eb } else { 0.0 } - 0.05 * fb;
            score_a.partial_cmp(&score_b).unwrap()
        });
        cands[0]
    };
    let yaw = wrap(yaw);

    // The camera saw the front face: push the center back by half of the
    // depth it couldn't see.
    let asset_depth = extent_along(dims_s, yaw, view);
    let seen_depth = fp.map(|f| {
        let a = axis_x(f.yaw);
        let b = Vec2::new(-a.y, a.x);
        f.size.x * a.dot(view).abs() + f.size.y * b.dot(view).abs()
    });
    let push = match seen_depth {
        Some(d) if !obj.radial => ((asset_depth - d) * 0.5).max(0.0),
        _ => asset_depth * 0.5,
    };
    let c = match fp {
        Some(_) => seen + view * push,
        // Floor point of the box bottom is the front edge.
        None => seen + view * asset_depth * 0.5,
    };
    // Tall things (columns, racks, machines) stand on the floor even when
    // their bottom is hidden behind something; only small objects sit higher.
    let base = fp.filter(|f| f.height < 1.5).map(|f| f.base_y).unwrap_or(0.0);
    Fit { translation: Vec3::new(c.x, base, c.y), yaw, scale }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::vision::geometry::{Floor, Footprint, Frame, PhotoCamera};

    fn scene(axis: f32) -> PhotoScene {
        let floor = Floor { n: Vec3::new(0.0, -0.97, -0.24).normalize(), d: 1.6, inlier_ratio: 1.0 };
        PhotoScene {
            image_id: "t".into(),
            width: 640,
            height: 480,
            hfov_deg: 65.0,
            hfov_source: "given",
            camera: PhotoCamera::new(Frame::new(&floor), 65.0, 640, 480),
            scale: 1.0,
            objects: vec![],
            walls: vec![],
            floor: None,
            ceiling_y: None,
            dominant_yaw: axis,
            placed: Default::default(),
        }
    }

    fn obj(center: Vec2, size: Vec2, yaw: f32, lateral: f32, radial: bool) -> PhotoObject {
        PhotoObject {
            id: 0,
            label: "x".into(),
            score: 1.0,
            bbox: [0.4, 0.4, 0.6, 0.8],
            footprint: Some(Footprint { center, size, yaw, base_y: 0.0, height: 2.0, points: 500 }),
            lateral_width: lateral,
            radial,
        }
    }

    #[test]
    fn long_rack_along_measured_axis_snapped() {
        let s = scene(0.0);
        // A rack row measured at 4 degrees off the building axis.
        let o = obj(Vec2::new(0.0, -8.0), Vec2::new(5.0, 0.5), 4f32.to_radians(), 5.0, false);
        // Asset long along X.
        let f = fit(&s, &o, Vec3::new(2.5, 2.0, 1.0), None);
        assert!(diff_mod_quarter(f.yaw, 0.0) < 1e-4, "snapped yaw {}", f.yaw.to_degrees());
        assert!(axis_x(f.yaw).x.abs() > 0.99, "long side along x");
        // Pushed back: the unseen half of the depth.
        assert!(f.translation.z < -8.0);
        assert_eq!(f.scale, 1.0);
    }

    #[test]
    fn asset_long_in_z_is_turned() {
        let s = scene(0.0);
        let o = obj(Vec2::new(2.0, -6.0), Vec2::new(3.0, 0.6), 0.0, 3.0, false);
        let f = fit(&s, &o, Vec3::new(0.8, 1.0, 3.0), None);
        // Local Z (long side) must lie along world X.
        assert!(axis_z(f.yaw).x.abs() > 0.99, "yaw {}", f.yaw.to_degrees());
    }

    #[test]
    fn radial_measurement_uses_building_axis_and_width() {
        let s = scene(10f32.to_radians());
        // Depth noise made it look long along the view ray; true width across is 0.8.
        let o = obj(Vec2::new(0.0, -9.0), Vec2::new(3.0, 0.4), FRAC_PI_2, 0.8, true);
        let f = fit(&s, &o, Vec3::new(2.0, 1.5, 0.8), None);
        assert!(diff_mod_quarter(f.yaw, 10f32.to_radians()) < 1e-4);
        // 2.0 x 0.8 asset whose width across the view should be ~0.8: long side along the view.
        let across = extent_along(Vec3::new(2.0, 1.5, 0.8), f.yaw, Vec2::new(1.0, 0.0));
        assert!((across - 0.8).abs() < 0.4, "across {across}");
    }

    #[test]
    fn scale_only_when_far_off() {
        let s = scene(0.0);
        let o = obj(Vec2::new(0.0, -6.0), Vec2::new(1.0, 1.0), 0.0, 1.0, false);
        assert_eq!(fit(&s, &o, Vec3::new(1.0, 1.7, 1.0), None).scale, 1.0);
        let f = fit(&s, &o, Vec3::new(1.0, 0.5, 1.0), None);
        assert!((f.scale - 2.0).abs() < 1e-4);
    }

    #[test]
    fn floor_point_round_trip() {
        let s = scene(0.0);
        let p = Vec3::new(1.2, 0.0, -7.0);
        let uv = s.camera.project(p).unwrap();
        let q = floor_point(&s, uv.x, uv.y).unwrap();
        assert!((q - Vec2::new(p.x, p.z)).length() < 1e-3, "{q:?}");
    }
}
