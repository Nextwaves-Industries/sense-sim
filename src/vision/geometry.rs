//! Geometry for turning a metric depth map of a photo into the viewer's world:
//! floor fit, camera pose, per-object point clouds and oriented footprints,
//! wall planes, and projection back into the photo.
//!
//! Frames: camera space is X right, Y down, Z forward. The world frame puts
//! the camera at (0, h, 0) looking towards -Z, Y up, floor at y = 0 - the
//! viewer's own convention, so results can be placed directly.

use anyhow::{Context, Result};
use bevy::math::{Vec2, Vec3};
use serde_json::{Value, json};
pub struct DepthMap {
    pub width: usize,
    pub height: usize,
    /// Metric depth (camera-space Z) per pixel, row-major.
    pub z: Vec<f32>,
}

impl DepthMap {
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.z[y.min(self.height - 1) * self.width + x.min(self.width - 1)]
    }

    /// Median of valid depths in a small window around a normalized point.
    pub fn sample(&self, u: f32, v: f32, radius: usize) -> Option<f32> {
        let cx = (u * self.width as f32) as isize;
        let cy = (v * self.height as f32) as isize;
        let r = radius as isize;
        let mut vals = Vec::new();
        for y in (cy - r)..=(cy + r) {
            for x in (cx - r)..=(cx + r) {
                if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
                    let z = self.at(x as usize, y as usize);
                    if z.is_finite() && z > 0.01 {
                        vals.push(z);
                    }
                }
            }
        }
        median(&mut vals)
    }
}

pub fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

pub struct Intrinsics {
    pub fx: f32,
    pub cx: f32,
    pub cy: f32,
}

impl Intrinsics {
    pub fn from_hfov(width: usize, height: usize, hfov_deg: f32) -> Self {
        let fx = (width as f32 * 0.5) / (hfov_deg.to_radians() * 0.5).tan();
        Self { fx, cx: width as f32 * 0.5, cy: height as f32 * 0.5 }
    }

    /// Pixel + depth -> camera-space point.
    pub fn unproject(&self, px: f32, py: f32, z: f32) -> Vec3 {
        Vec3::new((px - self.cx) / self.fx * z, (py - self.cy) / self.fx * z, z)
    }
}

/// Floor plane in camera space: `n · p + d = 0`, `n` pointing up (towards the
/// camera), `d` = camera height above the floor.
#[derive(Clone, Copy, Debug)]
pub struct Floor {
    pub n: Vec3,
    pub d: f32,
    pub inlier_ratio: f32,
}

/// RANSAC plane fit on the lower part of the image, restricted to roughly
/// horizontal planes below the camera.
pub fn fit_floor(depth: &DepthMap, k: &Intrinsics) -> Option<Floor> {
    let mut pts = Vec::new();
    let step = (depth.width.max(depth.height) / 120).max(1);
    for y in (depth.height * 11 / 20..depth.height).step_by(step) {
        for x in (0..depth.width).step_by(step) {
            let z = depth.at(x, y);
            if z.is_finite() && z > 0.05 {
                pts.push(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z));
            }
        }
    }
    if pts.len() < 30 {
        return None;
    }
    // Deterministic LCG so results are reproducible.
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut rand = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let mut best: Option<(usize, Vec3, f32)> = None;
    for _ in 0..400 {
        let (a, b, c) = (pts[rand(pts.len())], pts[rand(pts.len())], pts[rand(pts.len())]);
        let mut n = (b - a).cross(c - a);
        if n.length() < 1e-6 {
            continue;
        }
        n = n.normalize();
        if n.y > 0.0 {
            n = -n; // camera Y points down; floor normal points up = -Y
        }
        // Up to ~72 degrees of camera pitch (shots from a mezzanine).
        if n.dot(Vec3::NEG_Y) < 0.3 {
            continue;
        }
        let d = -n.dot(a);
        if d <= 0.05 {
            continue; // plane must be below the camera
        }
        let inliers = pts.iter().filter(|p| (n.dot(**p) + d).abs() < 0.02 * p.z.max(1.0)).count();
        if best.map(|(c, _, _)| inliers > c).unwrap_or(true) {
            best = Some((inliers, n, d));
        }
    }
    let (count, n, d) = best?;
    // Refine: least-squares-ish by re-centering on the inliers.
    let inl: Vec<Vec3> = pts.iter().copied().filter(|p| (n.dot(*p) + d).abs() < 0.02 * p.z.max(1.0)).collect();
    let centroid = inl.iter().copied().sum::<Vec3>() / inl.len().max(1) as f32;
    let d = -n.dot(centroid);
    Some(Floor { n, d, inlier_ratio: count as f32 / pts.len() as f32 })
}

/// World axes expressed in camera space.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub x: Vec3,
    pub y: Vec3,
    pub z: Vec3,
    pub h: f32,
}

impl Frame {
    pub fn new(floor: &Floor) -> Self {
        let y = floor.n;
        let fwd = (Vec3::Z - y * Vec3::Z.dot(y)).normalize();
        let z = -fwd;
        let x = y.cross(z);
        Self { x, y, z, h: floor.d }
    }

    pub fn to_world(&self, p: Vec3) -> Vec3 {
        Vec3::new(p.dot(self.x), p.dot(self.y) + self.h, p.dot(self.z))
    }
}

#[derive(Clone, Debug)]
pub struct BoxIn {
    pub label: String,
    /// Normalized [0, 1] image coordinates.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Turns the depth map and boxes into a JSON report (positions in meters).
/// `scale` multiplies all distances (from a user-given known dimension).
pub fn analyze(depth: &DepthMap, hfov_deg: f32, boxes: &[BoxIn], scale: f32) -> Result<Value> {
    let k = Intrinsics::from_hfov(depth.width, depth.height, hfov_deg);
    let floor = fit_floor(depth, &k).context("no floor plane found in the lower part of the photo")?;
    let f = Frame::new(&floor);
    let (w, h) = (depth.width as f32, depth.height as f32);

    let mut objects = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        let (x0, x1) = (b.x0.min(b.x1).clamp(0.0, 1.0), b.x0.max(b.x1).clamp(0.0, 1.0));
        let (y0, y1) = (b.y0.min(b.y1).clamp(0.0, 1.0), b.y0.max(b.y1).clamp(0.0, 1.0));
        let u = (x0 + x1) * 0.5;
        // Contact point: bottom-center of the box, nudged inwards.
        let vb = (y1 - (y1 - y0) * 0.04).max(y0);
        let r = (depth.width / 200).max(1);
        let Some(z_front) = depth.sample(u, vb, r) else {
            objects.push(json!({ "index": i, "label": b.label, "error": "no valid depth at the box bottom" }));
            continue;
        };
        // Where the bottom-center ray hits the floor (robust to the box
        // bottom being slightly above the floor contact).
        let ray = k.unproject(u * w, y1 * h, 1.0);
        let denom = floor.n.dot(ray);
        let foot_cam = if denom < -1e-4 { ray * (-floor.d / denom) } else { k.unproject(u * w, vb * h, z_front) };
        let foot = f.to_world(foot_cam);
        let zf = foot_cam.z.max(0.05);
        let left = f.to_world(k.unproject(x0 * w, y1 * h, zf));
        let right = f.to_world(k.unproject(x1 * w, y1 * h, zf));
        let top = f.to_world(k.unproject(u * w, y0 * h, zf));
        let width = (right - left).length();
        let height = (top.y - foot.y).max(0.0);
        let mut interior = Vec::new();
        for yy in 0..8 {
            for xx in 0..8 {
                let uu = x0 + (x1 - x0) * (xx as f32 + 0.5) / 8.0;
                let vv = y0 + (y1 - y0) * (yy as f32 + 0.5) / 8.0;
                if let Some(z) = depth.sample(uu, vv, 0) {
                    interior.push(z);
                }
            }
        }
        let median_depth = median(&mut interior).unwrap_or(zf);
        // Footprint center: push away from the camera by half the visible width.
        let away = Vec3::new(foot.x, 0.0, foot.z).normalize_or_zero();
        let center = foot + away * width * 0.5;
        let s = scale;
        objects.push(json!({
            "index": i,
            "label": b.label,
            "front_floor_xz": [r3(foot.x * s), r3(foot.z * s)],
            "center_floor_xz": [r3(center.x * s), r3(center.z * s)],
            "est_width_m": r3(width * s),
            "est_height_m": r3(height * s),
            "distance_m": r3(Vec3::new(foot.x, 0.0, foot.z).length() * s),
            "median_depth_m": r3(median_depth * s),
            "depth_consistency": r3((z_front / median_depth).min(median_depth / z_front)),
        }));
    }
    let cam_fwd = f.to_world(Vec3::Z) - Vec3::new(0.0, f.h, 0.0);
    let eye = Vec3::new(0.0, f.h * scale, 0.0);
    let target = eye + cam_fwd * 5.0;
    Ok(json!({
        "camera": {
            "eye": [0.0, r3(eye.y), 0.0],
            "target": [r3(target.x), r3(target.y), r3(target.z)],
            "height_m": r3(f.h * scale),
            "pitch_deg": r3(cam_fwd.y.asin().to_degrees()),
            "hfov_deg": hfov_deg,
        },
        "floor_fit_inlier_ratio": r3(floor.inlier_ratio),
        "scale_applied": scale,
        "objects": objects,
        "notes": "Viewer frame: Y up, meters, floor y=0, camera at the origin looking towards -Z. \
                  center_floor_xz is a heuristic footprint center; est sizes are the visible extent.",
    }))
}

pub fn r3(v: f32) -> f64 {
    (v as f64 * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------------------
// Photo camera: projection of world points back into the photo

/// Projects world points into the photo in normalized image coordinates
/// (u right, v down, both 0..1).
#[derive(Clone, Copy, Debug)]
pub struct PhotoCamera {
    pub frame: Frame,
    /// Focal length divided by image width.
    pub fx_norm: f32,
    /// Image width / height.
    pub aspect: f32,
}

impl PhotoCamera {
    pub fn new(frame: Frame, hfov_deg: f32, width: usize, height: usize) -> Self {
        let fx_norm = 0.5 / (hfov_deg.to_radians() * 0.5).tan();
        Self { frame, fx_norm, aspect: width as f32 / height.max(1) as f32 }
    }

    pub fn hfov_deg(&self) -> f32 {
        2.0 * (0.5 / self.fx_norm).atan().to_degrees()
    }

    pub fn eye(&self) -> Vec3 {
        Vec3::new(0.0, self.frame.h, 0.0)
    }

    /// Viewing direction in world space.
    pub fn forward(&self) -> Vec3 {
        Vec3::new(self.frame.x.z, self.frame.y.z, self.frame.z.z)
    }

    pub fn to_camera(&self, p: Vec3) -> Vec3 {
        let f = &self.frame;
        f.x * p.x + f.y * (p.y - f.h) + f.z * p.z
    }

    pub fn project(&self, p: Vec3) -> Option<Vec2> {
        let c = self.to_camera(p);
        if c.z < 0.05 {
            return None;
        }
        Some(Vec2::new(0.5 + self.fx_norm * c.x / c.z, 0.5 + self.fx_norm * self.aspect * c.y / c.z))
    }

    /// World direction through image point (u, v), scaled so that a point at
    /// `t` times it lies `t` along the camera's forward axis.
    pub fn ray(&self, u: f32, v: f32) -> Vec3 {
        let c = Vec3::new((u - 0.5) / self.fx_norm, (v - 0.5) / (self.fx_norm * self.aspect), 1.0);
        let f = &self.frame;
        Vec3::new(c.dot(f.x), c.dot(f.y), c.dot(f.z))
    }

    /// Where the ray through (u, v) meets the level plane at height `y`.
    pub fn plane_point(&self, u: f32, v: f32, y: f32) -> Option<Vec3> {
        let d = self.ray(u, v);
        let t = (y - self.frame.h) / d.y;
        (d.y.abs() > 1e-4 && t > 0.0).then(|| self.eye() + d * t)
    }

    /// Row (0 at the top) where the horizon crosses column `u`.
    pub fn horizon_v(&self, u: f32) -> f32 {
        let n = self.frame.y;
        let fy = self.fx_norm * self.aspect;
        let ny = if n.y.abs() < 1e-5 { -1e-5 } else { n.y };
        0.5 - fy * (n.z + n.x * (u - 0.5) / self.fx_norm) / ny
    }

    /// 2D bounds [u0, v0, u1, v1] of a set of world points (clipped to the image).
    pub fn project_bounds(&self, pts: &[Vec3]) -> Option<[f32; 4]> {
        let mut mn = Vec2::splat(f32::MAX);
        let mut mx = Vec2::splat(f32::MIN);
        let mut any = false;
        for p in pts {
            if let Some(q) = self.project(*p) {
                mn = mn.min(q);
                mx = mx.max(q);
                any = true;
            }
        }
        if !any {
            return None;
        }
        let b = [mn.x.clamp(0.0, 1.0), mn.y.clamp(0.0, 1.0), mx.x.clamp(0.0, 1.0), mx.y.clamp(0.0, 1.0)];
        (b[2] > b[0] && b[3] > b[1]).then_some(b)
    }
}

/// Intersection over union of two [x0, y0, x1, y1] boxes.
pub fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let iw = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let ih = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// The 8 corners of a box standing on `base_y` with footprint center `c`
/// (x, z), size `(sx, sz)` along its own axes, height `h`, rotated by `yaw`.
pub fn box_corners(c: Vec2, size: Vec2, yaw: f32, base_y: f32, h: f32) -> [Vec3; 8] {
    let (s, co) = yaw.sin_cos();
    // Rotation about +Y: local +X -> (cos, -sin) in (x, z); local +Z -> (sin, cos).
    let ax = Vec2::new(co, -s) * size.x * 0.5;
    let az = Vec2::new(s, co) * size.y * 0.5;
    let mut out = [Vec3::ZERO; 8];
    let mut i = 0;
    for (dx, dz) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let p = c + ax * dx + az * dz;
        out[i] = Vec3::new(p.x, base_y, p.y);
        out[i + 1] = Vec3::new(p.x, base_y + h, p.y);
        i += 2;
    }
    out
}

// ---------------------------------------------------------------------------
// Measuring objects

/// The measured extent of an object in the world (only what the camera sees).
#[derive(Clone, Copy, Debug)]
pub struct Footprint {
    /// Footprint center (x, z).
    pub center: Vec2,
    /// Extent along the object axes: x along `yaw`, y across.
    pub size: Vec2,
    /// Rotation of the long axis around Y (radians, viewer convention).
    pub yaw: f32,
    pub base_y: f32,
    pub height: f32,
    pub points: usize,
}

fn percentile(v: &mut [f32], p: f32) -> f32 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f32 * p).round() as usize]
}

/// World points of the depth map inside a normalized box, with their depth.
pub fn points_in_box(depth: &DepthMap, k: &Intrinsics, frame: &Frame, b: [f32; 4]) -> Vec<(Vec3, f32)> {
    let (w, h) = (depth.width as f32, depth.height as f32);
    let x0 = (b[0] * w).floor().max(0.0) as usize;
    let x1 = ((b[2] * w).ceil() as usize).min(depth.width);
    let y0 = (b[1] * h).floor().max(0.0) as usize;
    let y1 = ((b[3] * h).ceil() as usize).min(depth.height);
    let mut out = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let z = depth.at(x, y);
            if z.is_finite() && z > 0.05 && !depth.is_edge(x, y) {
                out.push((frame.to_world(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z)), z));
            }
        }
    }
    out
}

/// The object's visible surface inside a box: a region grown from a seed on
/// the object through depth-continuous neighbours, never crossing floor
/// pixels or depth edges, so the background behind it (even when connected
/// through the floor) is left out.
pub fn object_surface(depth: &DepthMap, k: &Intrinsics, frame: &Frame, b: [f32; 4]) -> Option<Vec<Vec3>> {
    let (w, h) = (depth.width as f32, depth.height as f32);
    let x0 = (b[0] * w).floor().max(0.0) as usize;
    let x1 = ((b[2] * w).ceil() as usize).min(depth.width);
    let y0 = (b[1] * h).floor().max(0.0) as usize;
    let y1 = ((b[3] * h).ceil() as usize).min(depth.height);
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        return None;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let idx = |x: usize, y: usize| (y - y0) * bw + (x - x0);
    // World point per pixel; None for invalid, floor and edge pixels.
    let mut pts: Vec<Option<(Vec3, f32)>> = vec![None; bw * bh];
    for y in y0..y1 {
        for x in x0..x1 {
            let z = depth.at(x, y);
            if !z.is_finite() || z <= 0.05 || depth.is_edge(x, y) {
                continue;
            }
            let p = frame.to_world(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z));
            if p.y > 0.04 + 0.008 * z {
                pts[idx(x, y)] = Some((p, z));
            }
        }
    }
    // Seed: the nearest-ish valid pixel in the central part of the box (the
    // object is in front of what shows through it).
    let mut central: Vec<(usize, usize, f32)> = Vec::new();
    for y in (y0 + bh / 4)..(y1 - bh / 4).max(y0 + bh / 4 + 1) {
        for x in (x0 + bw / 4)..(x1 - bw / 4).max(x0 + bw / 4 + 1) {
            if let Some((_, z)) = pts[idx(x, y)] {
                central.push((x, y, z));
            }
        }
    }
    if central.is_empty() {
        return None;
    }
    central.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    let (sx, sy, _) = central[central.len() / 4];
    // Flood fill through continuous depth.
    let mut seen = vec![false; bw * bh];
    let mut stack = vec![(sx, sy)];
    seen[idx(sx, sy)] = true;
    let mut out = Vec::new();
    while let Some((x, y)) = stack.pop() {
        let (p, z) = pts[idx(x, y)].unwrap();
        out.push(p);
        for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx < x0 as i32 || ny < y0 as i32 || nx >= x1 as i32 || ny >= y1 as i32 {
                continue;
            }
            let (nx, ny) = (nx as usize, ny as usize);
            let i = idx(nx, ny);
            if seen[i] {
                continue;
            }
            if let Some((_, nz)) = pts[i]
                && (nz - z).abs() < 0.035 * z
            {
                seen[i] = true;
                stack.push((nx, ny));
            }
        }
    }
    (out.len() >= 10).then_some(out)
}

impl DepthMap {
    /// True on depth discontinuities, where networks interpolate between
    /// foreground and background ("flying pixels" that smear objects along
    /// the view ray).
    pub fn is_edge(&self, x: usize, y: usize) -> bool {
        let z = self.at(x, y);
        let mut worst = 0.0f32;
        for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx < 0 || ny < 0 || nx as usize >= self.width || ny as usize >= self.height {
                continue;
            }
            let n = self.at(nx as usize, ny as usize);
            if n.is_finite() {
                worst = worst.max((n - z).abs());
            }
        }
        worst > 0.035 * z
    }
}

/// Measures the object inside a detection box: drops floor points and the
/// background behind the first depth discontinuity, then fits an oriented
/// footprint (PCA on the floor-plane projection) and a height.
pub fn measure_object(depth: &DepthMap, k: &Intrinsics, frame: &Frame, b: [f32; 4]) -> Option<Footprint> {
    let fg = object_surface(depth, k, frame, b)?;
    // PCA of the xz projection.
    let n = fg.len() as f32;
    let mean = fg.iter().fold(Vec2::ZERO, |a, p| a + Vec2::new(p.x, p.z)) / n;
    let (mut sxx, mut szz, mut sxz) = (0.0f32, 0.0f32, 0.0f32);
    for p in &fg {
        let d = Vec2::new(p.x, p.z) - mean;
        sxx += d.x * d.x;
        szz += d.y * d.y;
        sxz += d.x * d.y;
    }
    let theta = 0.5 * (2.0 * sxz).atan2(sxx - szz);
    let axis = Vec2::new(theta.cos(), theta.sin());
    let perp = Vec2::new(-axis.y, axis.x);
    let mut along: Vec<f32> = fg.iter().map(|p| (Vec2::new(p.x, p.z) - mean).dot(axis)).collect();
    let mut across: Vec<f32> = fg.iter().map(|p| (Vec2::new(p.x, p.z) - mean).dot(perp)).collect();
    let (a0, a1) = (percentile(&mut along, 0.04), percentile(&mut along, 0.96));
    let (c0, c1) = (percentile(&mut across, 0.04), percentile(&mut across, 0.96));
    let center = mean + axis * (a0 + a1) * 0.5 + perp * (c0 + c1) * 0.5;
    let mut ys: Vec<f32> = fg.iter().map(|p| p.y).collect();
    let top = percentile(&mut ys, 0.97);
    let bottom = percentile(&mut ys, 0.03);
    // An object whose lowest visible point is near the floor stands on it.
    let base_y = if bottom < 0.25 { 0.0 } else { bottom };
    // Viewer yaw: rotation about +Y maps +X to (cos, 0, -sin).
    let yaw = (-axis.y).atan2(axis.x);
    Some(Footprint {
        center,
        size: Vec2::new((a1 - a0).max(0.05), (c1 - c0).max(0.05)),
        yaw,
        base_y,
        height: (top - base_y).max(0.05),
        points: fg.len(),
    })
}

// ---------------------------------------------------------------------------
// Room shell

/// A wall segment on the floor plane, with its height.
#[derive(Clone, Copy, Debug)]
pub struct Wall {
    pub a: Vec2,
    pub b: Vec2,
    pub height: f32,
}

/// Vertical planes (walls) among the points that are neither floor nor
/// inside an object box.
pub fn fit_walls(depth: &DepthMap, k: &Intrinsics, frame: &Frame, exclude: &[[f32; 4]]) -> Vec<Wall> {
    let step = (depth.width.max(depth.height) / 140).max(1);
    let mut pts = Vec::new();
    for y in (0..depth.height).step_by(step) {
        for x in (0..depth.width).step_by(step) {
            let (u, v) = ((x as f32 + 0.5) / depth.width as f32, (y as f32 + 0.5) / depth.height as f32);
            if exclude.iter().any(|b| u >= b[0] && u <= b[2] && v >= b[1] && v <= b[3]) {
                continue;
            }
            let z = depth.at(x, y);
            if z.is_finite() && z > 0.05 {
                let p = frame.to_world(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z));
                if p.y > 0.3 {
                    pts.push(p);
                }
            }
        }
    }
    let total = pts.len();
    let mut walls = Vec::new();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rand = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    while walls.len() < 4 && pts.len() > total / 10 && pts.len() > 40 {
        let mut best: Option<(usize, Vec2, f32)> = None;
        for _ in 0..300 {
            let (p, q) = (pts[rand(pts.len())], pts[rand(pts.len())]);
            let d = Vec2::new(q.x - p.x, q.z - p.z);
            if d.length() < 0.5 {
                continue;
            }
            let n = Vec2::new(-d.y, d.x).normalize();
            let c = n.dot(Vec2::new(p.x, p.z));
            let tol = 0.08 + 0.01 * c.abs();
            let count = pts.iter().filter(|r| (n.dot(Vec2::new(r.x, r.z)) - c).abs() < tol).count();
            if best.map(|b| count > b.0).unwrap_or(true) {
                best = Some((count, n, c));
            }
        }
        let Some((count, n, c)) = best else { break };
        if count < total / 8 {
            break;
        }
        let tol = 0.08 + 0.01 * c.abs();
        let (inl, rest): (Vec<Vec3>, Vec<Vec3>) = pts.iter().partition(|r| (n.dot(Vec2::new(r.x, r.z)) - c).abs() < tol);
        let dir = Vec2::new(n.y, -n.x);
        let mut t: Vec<f32> = inl.iter().map(|r| dir.dot(Vec2::new(r.x, r.z))).collect();
        let mut ys: Vec<f32> = inl.iter().map(|r| r.y).collect();
        let (t0, t1) = (percentile(&mut t, 0.02), percentile(&mut t, 0.98));
        let base = n * c;
        walls.push(Wall { a: base + dir * t0, b: base + dir * t1, height: percentile(&mut ys, 0.98) });
        pts = rest;
    }
    walls
}

/// Ceiling height seen in the photo: the dominant horizontal level among the
/// points well above the camera (roof underside; lamps and trusses hanging
/// below it are outvoted). `None` when too little of the ceiling is visible.
pub fn fit_ceiling(depth: &DepthMap, k: &Intrinsics, frame: &Frame) -> Option<f32> {
    let step = (depth.width.max(depth.height) / 160).max(1);
    let mut ys = Vec::new();
    let mut total = 0usize;
    for y in (0..depth.height * 3 / 5).step_by(step) {
        for x in (0..depth.width).step_by(step) {
            let z = depth.at(x, y);
            if !z.is_finite() || z <= 0.05 || depth.is_edge(x, y) {
                continue;
            }
            total += 1;
            let p = frame.to_world(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z));
            if p.y > frame.h + 0.5 {
                ys.push(p.y);
            }
        }
    }
    if ys.len() < 40 || ys.len() * 20 < total {
        return None;
    }
    // Histogram mode at 10 cm, then the mean of the points near it.
    let (lo, hi) = ys.iter().fold((f32::MAX, f32::MIN), |(a, b), &y| (a.min(y), b.max(y)));
    let bins = (((hi - lo) / 0.1).ceil() as usize).max(1);
    let mut hist = vec![0usize; bins + 1];
    for &y in &ys {
        hist[((y - lo) / 0.1) as usize] += 1;
    }
    // Smooth over 3 bins so a sloped roof still forms one peak.
    let best = (0..hist.len())
        .max_by_key(|&i| hist[i] + if i > 0 { hist[i - 1] } else { 0 } + hist.get(i + 1).copied().unwrap_or(0))
        .unwrap();
    let level = lo + (best as f32 + 0.5) * 0.1;
    let near: Vec<f32> = ys.iter().copied().filter(|y| (y - level).abs() < 0.35).collect();
    let level = near.iter().sum::<f32>() / near.len().max(1) as f32;
    // The roof underside is the top envelope; lamps and purlins hang below it.
    let mut sorted = ys.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let h = level.max(sorted[(sorted.len() - 1) * 9 / 10]);
    (2.2..=40.0).contains(&h).then_some(h)
}

/// Floor extent [x0, z0, x1, z1] seen in the photo.
pub fn floor_extent(depth: &DepthMap, k: &Intrinsics, frame: &Frame) -> Option<[f32; 4]> {
    let step = (depth.width.max(depth.height) / 140).max(1);
    let (mut xs, mut zs) = (Vec::new(), Vec::new());
    for y in (0..depth.height).step_by(step) {
        for x in (0..depth.width).step_by(step) {
            let z = depth.at(x, y);
            if z.is_finite() && z > 0.05 {
                let p = frame.to_world(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z));
                if p.y.abs() < 0.05 + 0.01 * z {
                    xs.push(p.x);
                    zs.push(p.z);
                }
            }
        }
    }
    if xs.len() < 30 {
        return None;
    }
    Some([percentile(&mut xs, 0.02), percentile(&mut zs, 0.02), percentile(&mut xs, 0.98), percentile(&mut zs, 0.98)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Camera `cam_h` above a floor, pitched down by `pitch` deg, with a
    /// vertical wall (box front) at distance `wall_z` covering some pixels.
    fn synth(w: usize, h: usize, hfov: f32, cam_h: f32, pitch: f32, wall: Option<(usize, usize, usize, usize, f32)>) -> DepthMap {
        let k = Intrinsics::from_hfov(w, h, hfov);
        let p = pitch.to_radians();
        // Floor normal (up) in camera space for a camera pitched down by p.
        let n = Vec3::new(0.0, -p.cos(), -p.sin()); // camera pitched down
        let mut z = vec![f32::INFINITY; w * h];
        for y in 0..h {
            for x in 0..w {
                let ray = k.unproject(x as f32 + 0.5, y as f32 + 0.5, 1.0);
                let denom = n.dot(ray);
                if denom < -1e-4 {
                    z[y * w + x] = (ray * (-cam_h / denom)).z;
                }
                if let Some((x0, y0, x1, y1, wz)) = wall
                    && x >= x0
                    && x < x1
                    && y >= y0
                    && y < y1
                {
                    z[y * w + x] = wz;
                }
            }
        }
        DepthMap { width: w, height: h, z }
    }

    #[test]
    fn recovers_floor_and_camera() {
        let d = synth(320, 240, 65.0, 1.6, 15.0, None);
        let k = Intrinsics::from_hfov(320, 240, 65.0);
        let f = fit_floor(&d, &k).unwrap();
        assert!((f.d - 1.6).abs() < 0.05, "height {}", f.d);
        let pitch = (-f.n.z).atan2(-f.n.y).to_degrees();
        assert!((pitch - 15.0).abs() < 1.0, "pitch {pitch}");
        let b = BoxIn { label: "x".into(), x0: 0.4, y0: 0.7, x1: 0.6, y1: 0.9 };
        let out = analyze(&d, 65.0, &[b], 1.0).unwrap();
        let reported = out["camera"]["pitch_deg"].as_f64().unwrap();
        assert!((reported + 15.0).abs() < 1.0, "looking down must be a negative pitch, got {reported}");
        assert!(out["camera"]["target"][1].as_f64().unwrap() < 1.6, "target must be below the eye");
    }

    #[test]
    fn box_on_floor_lands_at_right_distance() {
        let (w, h, hfov, cam_h, pitch) = (320usize, 240usize, 65.0f32, 1.6f32, 15.0f32);
        // Pick a floor pixel and work out its true distance.
        let k = Intrinsics::from_hfov(w, h, hfov);
        let p = pitch.to_radians();
        let n = Vec3::new(0.0, -p.cos(), -p.sin()); // camera pitched down
        let (px, py) = (200.0f32, 200.0f32);
        let ray = k.unproject(px, py, 1.0);
        let hit = ray * (-cam_h / n.dot(ray));
        let truth = Vec3::new(hit.x, 0.0, hit.z - 0.0);
        let truth_dist_cam = Vec3::new(hit.x, 0.0, 0.0).length();
        let _ = truth_dist_cam;

        let d = synth(w, h, hfov, cam_h, pitch, None);
        let b = BoxIn { label: "box".into(), x0: (px - 10.0) / w as f32, y0: (py - 30.0) / h as f32, x1: (px + 10.0) / w as f32, y1: py / h as f32 };
        let out = analyze(&d, hfov, &[b], 1.0).unwrap();
        let o = &out["objects"][0];
        let fx = o["front_floor_xz"][0].as_f64().unwrap() as f32;
        let fz = o["front_floor_xz"][1].as_f64().unwrap() as f32;
        // Horizontal distance from the camera foot must match the ground truth.
        let horiz_truth = {
            let f = Floor { n, d: cam_h, inlier_ratio: 1.0 };
            let fr = Frame::new(&f);
            let wpt = fr.to_world(hit);
            assert!(wpt.y.abs() < 1e-3);
            Vec3::new(wpt.x, 0.0, wpt.z)
        };
        let got = Vec3::new(fx, 0.0, fz);
        assert!((got - horiz_truth).length() < 0.05 * horiz_truth.length(), "got {got:?} want {horiz_truth:?} ({truth:?})");
        assert!(fz < 0.0, "objects in front of the camera are at -Z");
        assert!(fx > 0.0, "right half of the image is +X");
    }

    #[test]
    fn scale_multiplies_distances() {
        let d = synth(160, 120, 65.0, 1.5, 20.0, None);
        let b = BoxIn { label: "b".into(), x0: 0.45, y0: 0.6, x1: 0.55, y1: 0.8 };
        let a = analyze(&d, 65.0, std::slice::from_ref(&b), 1.0).unwrap();
        let c = analyze(&d, 65.0, &[b], 2.0).unwrap();
        let da = a["objects"][0]["distance_m"].as_f64().unwrap_or_else(|| panic!("{a}"));
        let dc = c["objects"][0]["distance_m"].as_f64().unwrap();
        assert!((dc / da - 2.0).abs() < 0.01);
    }

    #[test]
    fn wall_height_estimate() {
        // A 1 m tall box face at 5 m: rows covering it get constant depth.
        let (w, h, hfov, cam_h) = (320usize, 240usize, 65.0f32, 1.6f32);
        let k = Intrinsics::from_hfov(w, h, hfov);
        // Level camera (pitch 0) makes the projection easy to reason about.
        let row = |y_world: f32| k.cy + (cam_h - y_world) / 5.0 * k.fx;
        let (top, bottom) = (row(1.0) as usize, row(0.0) as usize);
        let d = synth(w, h, hfov, cam_h, 0.0, Some((140, top, 180, bottom, 5.0)));
        let b = BoxIn { label: "crate".into(), x0: 140.0 / w as f32, y0: top as f32 / h as f32, x1: 180.0 / w as f32, y1: bottom as f32 / h as f32 };
        let out = analyze(&d, hfov, &[b], 1.0).unwrap();
        let ht = out["objects"][0]["est_height_m"].as_f64().unwrap();
        assert!((ht - 1.0).abs() < 0.1, "height {ht}");
    }

    // ---- Ray-cast renderer for synthetic scenes (world: Y up, camera at
    // (0, cam_h, 0) looking towards -Z, pitched down by `pitch` degrees).

    struct SceneBox {
        c: Vec2,
        size: Vec2,
        yaw: f32,
        h: f32,
    }

    fn ray_box(o: Vec3, d: Vec3, b: &SceneBox) -> Option<f32> {
        // Into box space: translate, then rotate by -yaw about Y.
        let (s, co) = (-b.yaw).sin_cos();
        let rot = |v: Vec3| Vec3::new(co * v.x + s * v.z, v.y, -s * v.x + co * v.z);
        let lo = rot(o - Vec3::new(b.c.x, 0.0, b.c.y));
        let ld = rot(d);
        let half = Vec3::new(b.size.x * 0.5, b.h * 0.5, b.size.y * 0.5);
        let lo = lo - Vec3::new(0.0, half.y, 0.0);
        let (mut t0, mut t1) = (0.0f32, f32::MAX);
        for i in 0..3 {
            let (oo, dd, hh) = (lo[i], ld[i], half[i]);
            if dd.abs() < 1e-8 {
                if oo.abs() > hh {
                    return None;
                }
                continue;
            }
            let (a, b2) = ((-hh - oo) / dd, (hh - oo) / dd);
            t0 = t0.max(a.min(b2));
            t1 = t1.min(a.max(b2));
        }
        (t0 <= t1 && t0 > 0.0).then_some(t0)
    }

    fn ray_wall(o: Vec3, d: Vec3, a: Vec2, b: Vec2, h: f32) -> Option<f32> {
        let dir = (b - a).normalize();
        let n = Vec2::new(-dir.y, dir.x);
        let denom = n.dot(Vec2::new(d.x, d.z));
        if denom.abs() < 1e-6 {
            return None;
        }
        let t = (n.dot(a) - n.dot(Vec2::new(o.x, o.z))) / denom;
        let p = o + d * t;
        let along = dir.dot(Vec2::new(p.x, p.z) - a);
        (t > 0.0 && p.y >= 0.0 && p.y <= h && along >= 0.0 && along <= (b - a).length()).then_some(t)
    }

    /// Returns the depth map and a mask of pixels that hit `boxes[0]`.
    fn render(w: usize, hgt: usize, hfov: f32, cam_h: f32, pitch: f32, boxes: &[SceneBox], walls: &[(Vec2, Vec2, f32)]) -> (DepthMap, Vec<bool>) {
        render_full(w, hgt, hfov, cam_h, pitch, boxes, walls, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_full(w: usize, hgt: usize, hfov: f32, cam_h: f32, pitch: f32, boxes: &[SceneBox], walls: &[(Vec2, Vec2, f32)], ceiling: Option<f32>) -> (DepthMap, Vec<bool>) {
        let k = Intrinsics::from_hfov(w, hgt, hfov);
        let p = pitch.to_radians();
        let fwd = Vec3::new(0.0, -p.sin(), -p.cos());
        let down = Vec3::new(0.0, -p.cos(), p.sin());
        let right = Vec3::X;
        let o = Vec3::new(0.0, cam_h, 0.0);
        let mut z = vec![f32::INFINITY; w * hgt];
        let mut mask = vec![false; w * hgt];
        for y in 0..hgt {
            for x in 0..w {
                let c = k.unproject(x as f32 + 0.5, y as f32 + 0.5, 1.0);
                let d = right * c.x + down * c.y + fwd * c.z;
                let mut best = f32::INFINITY;
                let mut first = false;
                if let Some(c) = ceiling
                    && d.y > 1e-6
                {
                    best = (c - o.y) / d.y;
                }
                if d.y < -1e-6 {
                    best = -o.y / d.y;
                }
                for (i, b) in boxes.iter().enumerate() {
                    if let Some(t) = ray_box(o, d, b)
                        && t < best
                    {
                        best = t;
                        first = i == 0;
                    }
                }
                for (a, b, h) in walls {
                    if let Some(t) = ray_wall(o, d, *a, *b, *h)
                        && t < best
                    {
                        best = t;
                        first = false;
                    }
                }
                // d has camera-space z = 1, so t is the camera depth.
                z[y * w + x] = best;
                mask[y * w + x] = first;
            }
        }
        (DepthMap { width: w, height: hgt, z }, mask)
    }

    fn mask_bounds(mask: &[bool], w: usize, h: usize) -> [f32; 4] {
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if mask[y * w + x] {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        [x0 as f32 / w as f32, y0 as f32 / h as f32, x1 as f32 / w as f32, y1 as f32 / h as f32]
    }

    fn angle_diff_mod_pi(a: f32, b: f32) -> f32 {
        let d = (a - b).rem_euclid(std::f32::consts::PI);
        d.min(std::f32::consts::PI - d)
    }

    #[test]
    fn projection_matches_rendered_object() {
        let (w, h, hfov) = (320, 240, 65.0);
        let b = SceneBox { c: Vec2::new(1.0, -6.0), size: Vec2::new(2.0, 0.8), yaw: 0.5, h: 1.5 };
        let (d, mask) = render(w, h, hfov, 2.0, 20.0, &[b], &[]);
        let k = Intrinsics::from_hfov(w, h, hfov);
        let floor = fit_floor(&d, &k).unwrap();
        let cam = PhotoCamera::new(Frame::new(&floor), hfov, w, h);
        let truth = mask_bounds(&mask, w, h);
        let corners = box_corners(Vec2::new(1.0, -6.0), Vec2::new(2.0, 0.8), 0.5, 0.0, 1.5);
        let proj = cam.project_bounds(&corners).unwrap();
        assert!(iou(proj, truth) > 0.85, "iou {} proj {proj:?} truth {truth:?}", iou(proj, truth));
        assert!((cam.frame.h - 2.0).abs() < 0.05);
        assert!(cam.forward().y < 0.0, "camera looks down");
    }

    #[test]
    fn measures_rotated_box() {
        let (w, h, hfov) = (480, 360, 65.0);
        let truth = SceneBox { c: Vec2::new(-0.8, -5.0), size: Vec2::new(2.4, 0.8), yaw: 0.6, h: 1.2 };
        let (d, mask) = render(w, h, hfov, 2.5, 25.0, &[truth], &[]);
        let k = Intrinsics::from_hfov(w, h, hfov);
        let floor = fit_floor(&d, &k).unwrap();
        let frame = Frame::new(&floor);
        let fp = measure_object(&d, &k, &frame, mask_bounds(&mask, w, h)).unwrap();
        assert!(angle_diff_mod_pi(fp.yaw, 0.6) < 5f32.to_radians(), "yaw {} want 0.6", fp.yaw);
        assert!((fp.height - 1.2).abs() < 0.12, "height {}", fp.height);
        assert!(fp.base_y == 0.0);
        assert!((fp.center - Vec2::new(-0.8, -5.0)).length() < 0.6, "center {:?}", fp.center);
        assert!(fp.size.x > 1.8 && fp.size.x < 2.6, "length {}", fp.size.x);
    }

    #[test]
    fn finds_back_wall_and_floor() {
        let (w, h, hfov) = (320, 240, 70.0);
        let wall = (Vec2::new(-12.0, -14.0), Vec2::new(12.0, -14.0), 6.0);
        let (d, _) = render(w, h, hfov, 1.7, 8.0, &[], &[wall]);
        let k = Intrinsics::from_hfov(w, h, hfov);
        let floor = fit_floor(&d, &k).unwrap();
        let frame = Frame::new(&floor);
        let walls = fit_walls(&d, &k, &frame, &[]);
        assert!(!walls.is_empty(), "no wall found");
        let wl = walls[0];
        let mid = (wl.a + wl.b) * 0.5;
        assert!((mid.y + 14.0).abs() < 0.5, "wall at z {}", mid.y);
        assert!((wl.b - wl.a).normalize().x.abs() > 0.98, "wall should run along x");
        let fe = floor_extent(&d, &k, &frame).unwrap();
        assert!(fe[1] < -10.0 && fe[3] < 0.0, "floor extent {fe:?}");
    }

    #[test]
    fn recovers_ceiling_height() {
        // Level-ish handheld shot of a 7.5 m hall with a back wall.
        let (w, h, hfov) = (320, 240, 75.0);
        let wall = (Vec2::new(-20.0, -25.0), Vec2::new(20.0, -25.0), 7.5);
        let (d, _) = render_full(w, h, hfov, 1.6, 3.0, &[], &[wall], Some(7.5));
        let k = Intrinsics::from_hfov(w, h, hfov);
        let frame = Frame::new(&fit_floor(&d, &k).unwrap());
        let c = fit_ceiling(&d, &k, &frame).expect("ceiling visible");
        assert!((c - 7.5).abs() < 0.2, "ceiling {c}");
        // Looking down at the floor only: no ceiling to report.
        let (d, _) = render_full(w, h, hfov, 1.6, 60.0, &[], &[], Some(7.5));
        let frame = Frame::new(&fit_floor(&d, &k).unwrap());
        assert!(fit_ceiling(&d, &k, &frame).is_none());
    }
}
