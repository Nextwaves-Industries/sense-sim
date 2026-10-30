//! Photo -> measured scene: metric depth, floor and camera pose, detections,
//! per-object footprints, walls and floor extent. Results are kept per image
//! so placement and verification can refer back to them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use bevy::math::{Vec2, Vec3};
use image::RgbImage;
use serde_json::{Value, json};

use super::geometry::{self, DepthMap, Footprint, Frame, Intrinsics, PhotoCamera, Wall};
use super::models::{self, Detection};

#[derive(Clone, Debug)]
pub struct PhotoObject {
    pub id: usize,
    pub label: String,
    pub score: f32,
    pub bbox: [f32; 4],
    pub footprint: Option<Footprint>,
    /// Width across the view direction (reliable even when depth is noisy).
    pub lateral_width: f32,
    /// The measured long axis points along the view ray, so its yaw and
    /// length are mostly depth noise.
    pub radial: bool,
}

#[derive(Clone)]
pub struct PhotoScene {
    pub image_id: String,
    pub width: u32,
    pub height: u32,
    pub hfov_deg: f32,
    pub hfov_source: &'static str,
    pub camera: PhotoCamera,
    pub scale: f32,
    pub objects: Vec<PhotoObject>,
    pub walls: Vec<Wall>,
    pub floor: Option<[f32; 4]>,
    /// Ceiling height (m) when enough of it is visible.
    pub ceiling_y: Option<f32>,
    /// Building axis (radians): walls and long objects mostly align to it
    /// or to it + 90 degrees.
    pub dominant_yaw: f32,
    /// Placed object name -> detection id.
    pub placed: HashMap<String, usize>,
}

pub fn scenes() -> &'static Mutex<HashMap<String, PhotoScene>> {
    static S: OnceLock<Mutex<HashMap<String, PhotoScene>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Horizontal FOV from EXIF (35 mm equivalent focal length), if present.
pub fn exif_hfov(bytes: &[u8], width: u32, height: u32) -> Option<f32> {
    let exif = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)).ok()?;
    let f35 = exif.get_field(exif::Tag::FocalLengthIn35mmFilm, exif::In::PRIMARY)?.value.get_uint(0)? as f32;
    if f35 < 8.0 {
        return None;
    }
    // A 35 mm frame is 36 x 24 mm; the long side of the photo maps to 36 mm.
    let half = if width >= height { 18.0 } else { 12.0 };
    Some(2.0 * (half / f35).atan().to_degrees())
}

pub struct Request<'a> {
    pub image_id: &'a str,
    pub bytes: &'a [u8],
    pub labels: &'a [String],
    pub extra_boxes: &'a [(String, [f32; 4])],
    pub hfov_deg: Option<f32>,
    pub default_hfov: f32,
    /// (detection id, real width or height in m, is_height)
    pub known: Option<(usize, f32, bool)>,
    pub threshold: f32,
}

pub fn perceive(req: &Request) -> Result<PhotoScene> {
    let img: RgbImage = image::load_from_memory(req.bytes).context("decoding photo")?.to_rgb8();
    let (w, h) = img.dimensions();
    let (hfov, src) = match (req.hfov_deg, exif_hfov(req.bytes, w, h)) {
        (Some(f), _) => (f, "given"),
        (None, Some(f)) => (f, "EXIF"),
        (None, None) => (req.default_hfov, "assumed"),
    };
    let m = models::models();
    let depth = models::with(&m.depth, models::Depth::load, |d| d.run(&img, hfov))?;
    let mut dets: Vec<Detection> = if req.labels.is_empty() {
        Vec::new()
    } else {
        models::with(&m.detector, models::Detector::load, |d| d.run(&img, req.labels, req.threshold, req.threshold * 0.8))?
    };
    for (label, b) in req.extra_boxes {
        dets.push(Detection { label: label.clone(), score: 1.0, bbox: *b });
    }
    let mut scene = analyze(req.image_id, &depth, w, h, hfov, src, &dets, 1.0)?;
    if let Some((id, real, is_height)) = req.known
        && let Some(fp) = scene.objects.get(id).and_then(|o| o.footprint)
    {
        let measured = if is_height { fp.height } else { scene.objects[id].lateral_width };
        if measured > 0.01 {
            let s = real / measured;
            let scaled = DepthMap { width: depth.width, height: depth.height, z: depth.z.iter().map(|z| z * s).collect() };
            scene = analyze(req.image_id, &scaled, w, h, hfov, src, &dets, s)?;
        }
    }
    scenes().lock().unwrap().insert(req.image_id.to_string(), scene.clone());
    Ok(scene)
}

#[allow(clippy::too_many_arguments)]
fn analyze(image_id: &str, depth: &DepthMap, w: u32, h: u32, hfov: f32, src: &'static str, dets: &[Detection], scale: f32) -> Result<PhotoScene> {
    let k = Intrinsics::from_hfov(depth.width, depth.height, hfov);
    let floor = geometry::fit_floor(depth, &k).context("no floor visible in the lower part of the photo - the camera pose can't be recovered")?;
    let frame = Frame::new(&floor);
    let camera = PhotoCamera::new(frame, hfov, depth.width, depth.height);
    let mut objects = Vec::new();
    for (id, d) in dets.iter().enumerate() {
        let footprint = geometry::measure_object(depth, &k, &frame, d.bbox);
        let (lateral_width, radial) = footprint
            .map(|fp| {
                let view = (fp.center - Vec2::ZERO).normalize_or_zero();
                let across = Vec2::new(-view.y, view.x);
                let axis = Vec2::new(fp.yaw.cos(), -fp.yaw.sin());
                // Lateral extent from the 2D box width at the object's distance.
                let dist = fp.center.length().max(0.5);
                let lat = (d.bbox[2] - d.bbox[0]) * dist / camera.fx_norm;
                let radial = axis.dot(view).abs() > 0.9 && fp.size.x < fp.size.y.max(lat) * 6.0;
                let _ = across;
                (lat, radial)
            })
            .unwrap_or((0.0, false));
        objects.push(PhotoObject { id, label: d.label.clone(), score: d.score, bbox: d.bbox, footprint, lateral_width, radial });
    }
    let boxes: Vec<[f32; 4]> = dets.iter().map(|d| d.bbox).collect();
    let walls = geometry::fit_walls(depth, &k, &frame, &boxes);
    let floor_ext = geometry::floor_extent(depth, &k, &frame);
    let ceiling_y = geometry::fit_ceiling(depth, &k, &frame);
    let dominant_yaw = dominant_axis(&walls, &objects);
    let _ = (w, h);
    Ok(PhotoScene {
        image_id: image_id.to_string(),
        width: w,
        height: h,
        hfov_deg: hfov,
        hfov_source: src,
        camera,
        scale,
        objects,
        walls,
        floor: floor_ext,
        ceiling_y,
        dominant_yaw,
        placed: HashMap::new(),
    })
}

/// Building axis from the longest wall, else from confident elongated
/// objects, else straight ahead. Returned modulo 90 degrees.
fn dominant_axis(walls: &[Wall], objects: &[PhotoObject]) -> f32 {
    let quarter = std::f32::consts::FRAC_PI_2;
    if let Some(w) = walls.iter().max_by(|a, b| (a.b - a.a).length().partial_cmp(&(b.b - b.a).length()).unwrap()) {
        let d = w.b - w.a;
        return (-d.y).atan2(d.x).rem_euclid(quarter);
    }
    // Circular mean of 4x angles over confident, elongated footprints.
    let (mut s, mut c) = (0.0f32, 0.0f32);
    for o in objects {
        if let Some(fp) = o.footprint
            && !o.radial
            && fp.size.x > fp.size.y * 1.4
        {
            let wgt = fp.points as f32;
            s += (4.0 * fp.yaw).sin() * wgt;
            c += (4.0 * fp.yaw).cos() * wgt;
        }
    }
    if s == 0.0 && c == 0.0 { 0.0 } else { (s.atan2(c) / 4.0).rem_euclid(quarter) }
}

fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

impl PhotoScene {
    pub fn to_json(&self) -> Value {
        let cam = &self.camera;
        let eye = cam.eye();
        let target = eye + cam.forward() * 10.0;
        json!({
            "image_id": self.image_id,
            "camera": {
                "eye": [0.0, r2(eye.y), 0.0],
                "target": [r2(target.x), r2(target.y), r2(target.z)],
                "hfov_deg": r2(self.hfov_deg),
                "hfov_source": self.hfov_source,
                "height_m": r2(eye.y),
                "pitch_deg": r2(cam.forward().y.asin().to_degrees()),
            },
            "scale_applied": r2(self.scale),
            "building_axis_deg": r2(self.dominant_yaw.to_degrees()),
            "objects": self.objects.iter().map(|o| {
                let mut v = json!({ "id": o.id, "label": o.label, "score": r2(o.score), "box": o.bbox.map(r2) });
                match o.footprint {
                    Some(fp) => {
                        v["center_xz"] = json!([r2(fp.center.x), r2(fp.center.y)]);
                        v["width_m"] = json!(r2(o.lateral_width));
                        v["height_m"] = json!(r2(fp.height));
                        v["base_y"] = json!(r2(fp.base_y));
                        v["yaw_deg"] = if o.radial { Value::Null } else { json!(r2(fp.yaw.to_degrees())) };
                    }
                    None => v["unmeasured"] = json!(true),
                }
                v
            }).collect::<Vec<_>>(),
            "walls": self.walls.iter().map(|w| json!({
                "from_xz": [r2(w.a.x), r2(w.a.y)], "to_xz": [r2(w.b.x), r2(w.b.y)], "height_m": r2(w.height)
            })).collect::<Vec<_>>(),
            "floor_extent_xz": self.floor.map(|f| f.map(r2)),
            "ceiling_height_m": self.ceiling_y.map(r2),
            "notes": "Viewer frame: meters, Y up, floor y=0, camera above the origin looking towards -Z. width_m is the extent across the view; depth along the view is not measurable from one photo, so the asset's own depth is used. yaw_deg is null when it can't be measured (use building_axis_deg).",
        })
    }

    /// The photo with numbered detection boxes (green) and, for placed
    /// objects, their projected bounds (orange).
    pub fn overlay(&self, photo: &RgbImage, projected: &[(usize, [f32; 4])]) -> RgbImage {
        let mut img = photo.clone();
        for o in &self.objects {
            draw_rect(&mut img, o.bbox, [60, 220, 90]);
            draw_number(&mut img, o.id, o.bbox[0], o.bbox[1], [60, 220, 90]);
        }
        for (id, b) in projected {
            draw_rect(&mut img, *b, [255, 150, 30]);
            draw_number(&mut img, *id, b[2], b[3], [255, 150, 30]);
        }
        img
    }

    /// Projected 2D bounds of a world box in this photo.
    pub fn project(&self, corners: &[Vec3]) -> Option<[f32; 4]> {
        self.camera.project_bounds(corners)
    }
}

fn draw_rect(img: &mut RgbImage, b: [f32; 4], color: [u8; 3]) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let t = (w.max(h) / 400).max(2);
    let (x0, y0) = ((b[0] * w as f32) as i32, (b[1] * h as f32) as i32);
    let (x1, y1) = (((b[2] * w as f32) as i32).min(w - 1), ((b[3] * h as f32) as i32).min(h - 1));
    let mut put = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && x < w && y < h {
            img.put_pixel(x as u32, y as u32, image::Rgb(color));
        }
    };
    for k in 0..t {
        for x in x0..=x1 {
            put(x, y0 + k);
            put(x, y1 - k);
        }
        for y in y0..=y1 {
            put(x0 + k, y);
            put(x1 - k, y);
        }
    }
}

/// 3x5 bitmap digits, scaled, on a dark plate, so overlays can be read by
/// both people and vision models.
fn draw_number(img: &mut RgbImage, n: usize, u: f32, v: f32, color: [u8; 3]) {
    const DIGITS: [[u8; 5]; 10] = [
        [0b111, 0b101, 0b101, 0b101, 0b111],
        [0b010, 0b110, 0b010, 0b010, 0b111],
        [0b111, 0b001, 0b111, 0b100, 0b111],
        [0b111, 0b001, 0b111, 0b001, 0b111],
        [0b101, 0b101, 0b111, 0b001, 0b001],
        [0b111, 0b100, 0b111, 0b001, 0b111],
        [0b111, 0b100, 0b111, 0b101, 0b111],
        [0b111, 0b001, 0b010, 0b010, 0b010],
        [0b111, 0b101, 0b111, 0b101, 0b111],
        [0b111, 0b101, 0b111, 0b001, 0b111],
    ];
    let (w, h) = (img.width() as i32, img.height() as i32);
    let s = (w.max(h) / 250).max(3);
    let text = n.to_string();
    let (x0, y0) = ((u * w as f32) as i32, (v * h as f32) as i32);
    let pw = text.len() as i32 * 4 * s + s;
    let ph = 7 * s;
    for y in y0..(y0 + ph).min(h) {
        for x in x0..(x0 + pw).min(w) {
            if x >= 0 && y >= 0 {
                img.put_pixel(x as u32, y as u32, image::Rgb([15, 15, 15]));
            }
        }
    }
    for (ci, ch) in text.bytes().enumerate() {
        let d = DIGITS[(ch - b'0') as usize];
        for (row, bits) in d.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    for dy in 0..s {
                        for dx in 0..s {
                            let x = x0 + s + ci as i32 * 4 * s + col * s + dx;
                            let y = y0 + s + row as i32 * s + dy;
                            if x >= 0 && y >= 0 && x < w && y < h {
                                img.put_pixel(x as u32, y as u32, image::Rgb(color));
                            }
                        }
                    }
                }
            }
        }
    }
}
