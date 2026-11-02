//! Photo -> scene hypothesis. The assistant reads the photo and says what the
//! camera and the room are; object positions then follow from plain perspective
//! (a horizon, a camera height, where a box meets the floor, how big a thing is).
//! Detection finds the boxes. Depth builds nothing: [`check_depth`] compares the
//! finished layout with it afterwards and reports disagreements.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use bevy::math::{Vec2, Vec3};
use image::RgbImage;
use serde_json::{Value, json};

use super::geometry::{self, DepthMap, Floor, Footprint, Frame, Intrinsics, PhotoCamera};
use super::models::{self, Detection};
use crate::agent::assetgen::hex;

#[derive(Clone, Debug)]
pub struct PhotoObject {
    pub id: usize,
    pub label: String,
    pub score: f32,
    pub bbox: [f32; 4],
    /// Where it is, once perspective or the assistant's guess can say.
    pub footprint: Option<Footprint>,
    /// Width across the view.
    pub lateral_width: f32,
    /// Its long axis runs along the view, so its width says little about its length.
    pub radial: bool,
    /// Median color of the object's box (sRGB).
    pub color: Option<[u8; 3]>,
    /// The assistant's estimates for this object.
    pub guess: ObjectGuess,
    /// Why it is where it is, and how the estimates agree with each other.
    pub source: &'static str,
    pub checks: Value,
}

/// What the assistant estimates about one detection. Each field is optional.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ObjectGuess {
    pub height_m: Option<f32>,
    pub width_m: Option<f32>,
    /// Height of its underside: 0 on the floor, more on a shelf or hanging.
    pub base_y: Option<f32>,
    pub at_xz: Option<[f32; 2]>,
    pub yaw_deg: Option<f32>,
}

/// The assistant's reading of the camera: how high it is, and where the horizon
/// (eye level, where parallel floor lines converge) crosses the photo's edges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraGuess {
    pub height_m: f32,
    pub horizon_left_v: f32,
    pub horizon_right_v: f32,
}

impl Default for CameraGuess {
    /// A level handheld photo: only until the assistant has looked.
    fn default() -> Self {
        Self { height_m: 1.6, horizon_left_v: 0.5, horizon_right_v: 0.5 }
    }
}

/// Median sRGB color of each surface of the room, where it can be seen.
#[derive(Clone, Copy, Default, Debug)]
pub struct SurfaceColors {
    pub floor: Option<[u8; 3]>,
    pub walls: Option<[u8; 3]>,
    pub ceiling: Option<[u8; 3]>,
}

#[derive(Clone)]
pub struct PhotoScene {
    pub image_id: String,
    pub width: u32,
    pub height: u32,
    pub hfov_deg: f32,
    pub hfov_source: &'static str,
    pub camera: PhotoCamera,
    /// `None` until the assistant has said what the camera is.
    pub camera_guess: Option<CameraGuess>,
    pub objects: Vec<PhotoObject>,
    /// Ceiling height (m) as guessed.
    pub ceiling_y: Option<f32>,
    /// Which way the building runs (radians): long objects align to it or to it
    /// plus 90 degrees. 0 is square to the camera.
    pub dominant_yaw: f32,
    /// Placed object name -> detection id.
    pub placed: HashMap<String, usize>,
    pub colors: SurfaceColors,
    /// The assistant said this photo has no roof (outdoors).
    pub no_ceiling: bool,
    /// The assistant's estimate of the whole room, part of it out of view:
    /// [x0, z0, x1, z1].
    pub room: Option<[f32; 4]>,
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

/// The assistant's current hypothesis. What it leaves out stays as it was.
#[derive(Clone, Debug, Default)]
pub struct LayoutGuess {
    pub camera: Option<CameraGuess>,
    pub hfov_deg: Option<f32>,
    pub building_axis_deg: Option<f32>,
    pub objects: Vec<(usize, ObjectGuess)>,
}

pub struct Request<'a> {
    pub image_id: &'a str,
    pub bytes: &'a [u8],
    pub labels: &'a [String],
    pub extra_boxes: &'a [(String, [f32; 4])],
    pub default_hfov: f32,
    pub layout: &'a LayoutGuess,
    pub threshold: f32,
}

/// Finds the boxes in a photo and applies the hypothesis to them.
pub fn perceive(req: &Request) -> Result<PhotoScene> {
    let img: RgbImage = image::load_from_memory(req.bytes).context("decoding photo")?.to_rgb8();
    let (w, h) = img.dimensions();
    let (hfov, src) = match (req.layout.hfov_deg, exif_hfov(req.bytes, w, h)) {
        (Some(f), _) => (f, "your estimate"),
        (None, Some(f)) => (f, "EXIF"),
        (None, None) => (req.default_hfov, "assumed: estimate it"),
    };
    let mut dets: Vec<Detection> = if req.labels.is_empty() {
        Vec::new()
    } else {
        // One pass per label: phrases sharing a prompt compete for the score, and
        // in a busy photo most of them then fall below the threshold.
        let m = models::models();
        models::with(&m.detector, models::Detector::load, |d| {
            let mut per_label = Vec::with_capacity(req.labels.len());
            for l in req.labels {
                per_label.push(d.run(&img, std::slice::from_ref(l), req.threshold, req.threshold * 0.8)?);
            }
            Ok(merge_detections(per_label))
        })?
    };
    for (label, b) in req.extra_boxes {
        dets.push(Detection { label: label.clone(), score: 1.0, bbox: *b });
    }
    // Asking again keeps what was already worked out about the camera and room.
    let earlier = scenes().lock().unwrap().get(req.image_id).cloned();
    let objects = dets
        .iter()
        .enumerate()
        .map(|(id, d)| PhotoObject {
            id,
            label: d.label.clone(),
            score: d.score,
            bbox: d.bbox,
            footprint: None,
            lateral_width: 0.0,
            radial: false,
            color: box_color(&img, d.bbox),
            guess: earlier.as_ref().and_then(|e| e.objects.get(id)).filter(|o| o.label == d.label).map(|o| o.guess).unwrap_or_default(),
            source: "",
            checks: Value::Null,
        })
        .collect();
    let mut scene = PhotoScene {
        image_id: req.image_id.to_string(),
        width: w,
        height: h,
        hfov_deg: hfov,
        hfov_source: src,
        camera: camera_from(CameraGuess::default(), hfov, w, h),
        camera_guess: earlier.as_ref().and_then(|e| e.camera_guess),
        objects,
        ceiling_y: earlier.as_ref().and_then(|e| e.ceiling_y),
        dominant_yaw: earlier.as_ref().map(|e| e.dominant_yaw).unwrap_or(0.0),
        placed: HashMap::new(),
        colors: SurfaceColors::default(),
        no_ceiling: earlier.as_ref().is_some_and(|e| e.no_ceiling),
        room: earlier.as_ref().and_then(|e| e.room),
    };
    scene.apply(req.layout, &img);
    scenes().lock().unwrap().insert(req.image_id.to_string(), scene.clone());
    Ok(scene)
}

/// Revises the hypothesis without running any model.
pub fn set_layout(image_id: &str, bytes: &[u8], layout: &LayoutGuess) -> Result<PhotoScene> {
    let img: RgbImage = image::load_from_memory(bytes).context("decoding photo")?.to_rgb8();
    let mut all = scenes().lock().unwrap();
    let scene = all.get_mut(image_id).with_context(|| format!("no perceive_photo result for '{image_id}'"))?;
    scene.apply(layout, &img);
    Ok(scene.clone())
}

impl PhotoScene {
    fn apply(&mut self, layout: &LayoutGuess, img: &RgbImage) {
        if let Some(h) = layout.hfov_deg {
            self.hfov_deg = h;
            self.hfov_source = "your estimate";
        }
        if let Some(c) = layout.camera {
            self.camera_guess = Some(c);
        }
        if let Some(a) = layout.building_axis_deg {
            self.dominant_yaw = a.to_radians();
        }
        self.camera = camera_from(self.camera_guess.unwrap_or_default(), self.hfov_deg, self.width, self.height);
        for (id, g) in &layout.objects {
            if let Some(o) = self.objects.get_mut(*id) {
                // Only what was said changes.
                o.guess = ObjectGuess {
                    height_m: g.height_m.or(o.guess.height_m),
                    width_m: g.width_m.or(o.guess.width_m),
                    base_y: g.base_y.or(o.guess.base_y),
                    at_xz: g.at_xz.or(o.guess.at_xz),
                    yaw_deg: g.yaw_deg.or(o.guess.yaw_deg),
                };
            }
        }
        let (cam, axis) = (self.camera, self.dominant_yaw);
        for o in &mut self.objects {
            let r = locate(&cam, o.bbox, &o.guess, axis, crate::agent::placer::hangs_from_above(&o.label));
            o.footprint = r.footprint;
            o.lateral_width = r.footprint.map(|f| f.size.x).unwrap_or(0.0);
            o.source = r.source;
            o.checks = r.checks;
        }
        let boxes: Vec<[f32; 4]> = self.objects.iter().map(|o| o.bbox).collect();
        self.colors = surface_colors(img, &cam, &boxes);
    }
}

/// The camera a horizon and a height describe, by perspective alone.
pub fn camera_from(g: CameraGuess, hfov_deg: f32, width: u32, height: u32) -> PhotoCamera {
    let fx = 0.5 / (hfov_deg.to_radians() * 0.5).tan();
    let fy = fx * width as f32 / height.max(1) as f32;
    // Two rays along the horizon span the level plane through the camera; its
    // normal is "up" in camera space (x right, y down, z forward).
    let at = |u: f32, v: f32| Vec3::new((u - 0.5) / fx, (v - 0.5) / fy, 1.0);
    let mut n = at(0.0, g.horizon_left_v).cross(at(1.0, g.horizon_right_v)).normalize();
    if n.y > 0.0 {
        n = -n;
    }
    let floor = Floor { n, d: g.height_m.max(0.1), inlier_ratio: 1.0 };
    PhotoCamera::new(Frame::new(&floor), hfov_deg, width as usize, height as usize)
}

struct Located {
    footprint: Option<Footprint>,
    source: &'static str,
    checks: Value,
}

/// Disagreement between two estimates of one distance worth calling out.
const DISAGREE: f32 = 0.3;
/// A box this near the photo's bottom is probably cut off by it.
const CUT_OFF_V: f32 = 0.985;

/// Where a detection is, from perspective and the assistant's estimates.
fn locate(cam: &PhotoCamera, b: [f32; 4], g: &ObjectGuess, axis: f32, hanging: bool) -> Located {
    let (fx, fy) = (cam.fx_norm, cam.fx_norm * cam.aspect);
    let (uc, vb) = ((b[0] + b[2]) * 0.5, b[3]);
    let (bw, bh) = ((b[2] - b[0]).max(1e-3), (b[3] - b[1]).max(1e-3));
    let eye = cam.eye();
    let ray = cam.ray(uc, vb);
    // How far away it must be for its estimated size to fill its box.
    let by_size = g.height_m.filter(|_| b[1] > 0.01 && vb < CUT_OFF_V).map(|h| h * fy / bh).or_else(|| g.width_m.filter(|_| b[0] > 0.01 && b[2] < 0.99).map(|w| w * fx / bw));
    let contact = cam.plane_point(uc, vb, g.base_y.unwrap_or(0.0));
    let cut_off = vb >= CUT_OFF_V;

    let (point, source) = if let Some([x, z]) = g.at_xz {
        (Some(Vec3::new(x, g.base_y.unwrap_or(0.0), z)), "the position you gave")
    } else if g.base_y.is_some() && contact.is_some() {
        (contact, "its box bottom at the base_y you gave")
    } else if hanging || cut_off {
        (by_size.map(|d| eye + ray * d), "the size you gave")
    } else if contact.is_some() {
        (contact, "its box bottom on the floor")
    } else {
        (by_size.map(|d| eye + ray * d), "the size you gave")
    };

    let mut checks = json!({});
    if let (Some(s), Some(c)) = (by_size, contact.filter(|_| !hanging && !cut_off)) {
        let by_contact = (c - eye).dot(cam.forward());
        checks["distance_by_size_m"] = json!(r2(s));
        checks["distance_by_floor_contact_m"] = json!(r2(by_contact));
        if (s / by_contact.max(0.01) - 1.0).abs() > DISAGREE {
            checks["verdict"] = json!("your size and the floor contact disagree: the camera height, the horizon, this size, or the box is wrong");
        }
    }
    if let Some(h) = g.height_m.filter(|_| !hanging && !cut_off && b[1] > 0.01) {
        let hv = cam.horizon_v(uc);
        if vb > hv {
            checks["implied_camera_height_m"] = json!(r2(h * (vb - hv) / bh));
        }
    }
    let Some(p) = point else {
        return Located { footprint: None, source: "", checks };
    };
    let d = (p - eye).dot(cam.forward()).max(0.1);
    let width = g.width_m.unwrap_or(bw / fx * d);
    let height = g.height_m.unwrap_or(bh / fy * d);
    let base_y = if p.y.abs() < 0.05 { 0.0 } else { p.y };
    Located {
        footprint: Some(Footprint { center: Vec2::new(p.x, p.z), size: Vec2::new(width, 0.05), yaw: axis, base_y, height, points: 0 }),
        source,
        checks,
    }
}

/// Median of the ratios; 1 when there are none.
fn robust_scale(ratios: &[f32]) -> f32 {
    let mut r: Vec<f32> = ratios.iter().copied().filter(|v| v.is_finite() && *v > 0.0).collect();
    if r.is_empty() {
        return 1.0;
    }
    r.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = r.len() / 2;
    if r.len() % 2 == 1 { r[mid] } else { (r[mid - 1] + r[mid]) * 0.5 }
}

// ---------------------------------------------------------------------------
// Depth: verification only

/// One placed object for the depth check: its name, box and distance in the layout.
pub struct LayoutItem {
    pub name: String,
    pub bbox: [f32; 4],
    pub distance_m: f32,
}

/// Compares layout distances with depth readings. Depth has no reliable scale, so
/// only how the objects relate to each other counts: the median ratio is taken out.
fn depth_verdicts(items: &[(String, f32, f32)]) -> (f32, Vec<Value>, Vec<String>) {
    let s = robust_scale(&items.iter().map(|i| i.1 / i.2.max(0.01)).collect::<Vec<_>>());
    let (mut rows, mut flags) = (Vec::new(), Vec::new());
    for (name, layout, depth) in items {
        let expected = depth * s;
        let off = layout / expected.max(0.01) - 1.0;
        rows.push(json!({ "object": name, "layout_m": r2(*layout), "depth_scaled_m": r2(expected), "off_pct": (off * 100.0).round() }));
        if off.abs() > DISAGREE {
            flags.push(format!("{name}: {:.1} m in the layout, depth suggests about {:.1} m", layout, expected));
        }
    }
    (s, rows, flags)
}

/// Reads the photo's depth and reports where the layout disagrees. Touches nothing:
/// depth compresses with distance and misjudges glass and shelves, so a disagreement
/// is a reason to look again, never an instruction.
pub fn check_depth(image_id: &str, bytes: &[u8], items: &[LayoutItem]) -> Result<Value> {
    let scene = scenes().lock().unwrap().get(image_id).cloned().with_context(|| format!("no perceive_photo result for '{image_id}'"))?;
    let img: RgbImage = image::load_from_memory(bytes).context("decoding photo")?.to_rgb8();
    let m = models::models();
    let depth = models::with(&m.depth, models::Depth::load, |d| d.run(&img, scene.hfov_deg))?;
    let read: Vec<(String, f32, f32)> = items
        .iter()
        .filter_map(|it| Some((it.name.clone(), it.distance_m, median_depth(&depth, it.bbox)?)))
        .collect();
    let (scale, rows, flags) = depth_verdicts(&read);
    let k = Intrinsics::from_hfov(depth.width, depth.height, scene.hfov_deg);
    let floor = geometry::fit_floor(&depth, &k);
    let ceiling = floor.as_ref().and_then(|f| geometry::fit_ceiling(&depth, &k, &Frame::new(f)));
    Ok(json!({
        "objects": rows,
        "disagreements": flags,
        "depth_scale_to_layout": r2(scale),
        "camera_height_m": { "yours": r2(scene.camera.eye().y), "depth_scaled": floor.map(|f| r2(f.d * scale)) },
        "ceiling_height_m": { "yours": scene.ceiling_y.map(r2), "depth_scaled": ceiling.map(|c| r2(c * scale)) },
        "note": "Depth did not place anything and this check changes nothing. It compresses with distance and misjudges glass and dense shelves, so treat a disagreement as a reason to look at the photo again, not as the answer.",
    }))
}

fn median_depth(depth: &DepthMap, b: [f32; 4]) -> Option<f32> {
    let (w, h) = (depth.width as f32, depth.height as f32);
    let (mx, my) = ((b[2] - b[0]) * 0.25, (b[3] - b[1]) * 0.25);
    let (x0, x1) = (((b[0] + mx) * w) as usize, ((b[2] - mx) * w) as usize);
    let (y0, y1) = (((b[1] + my) * h) as usize, ((b[3] - my) * h) as usize);
    let mut z = Vec::new();
    for y in y0..y1.min(depth.height) {
        for x in x0..x1.min(depth.width) {
            let v = depth.at(x, y);
            if v.is_finite() && v > 0.05 {
                z.push(v);
            }
        }
    }
    if z.is_empty() {
        return None;
    }
    z.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(z[z.len() / 2])
}

/// Overlap above which two labels are taken to have found the same object.
const SAME_OBJECT_IOU: f32 = 0.8;

/// Joins the passes in label order. Where labels found the same object the
/// stronger one keeps it. Ids stay stable across runs with the same labels.
fn merge_detections(per_label: Vec<Vec<Detection>>) -> Vec<Detection> {
    let mut all: Vec<(usize, Detection)> = per_label.into_iter().enumerate().flat_map(|(i, v)| v.into_iter().map(move |d| (i, d))).collect();
    all.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<(usize, Detection)> = Vec::with_capacity(all.len());
    for (i, d) in all {
        if !kept.iter().any(|(_, k)| geometry::iou(k.bbox, d.bbox) > SAME_OBJECT_IOU) {
            kept.push((i, d));
        }
    }
    // A stable sort keeps each label's detections strongest first.
    kept.sort_by_key(|(i, _)| *i);
    kept.into_iter().map(|(_, d)| d).collect()
}

fn median(mut v: Vec<[u8; 3]>) -> Option<[u8; 3]> {
    if v.len() < 16 {
        return None;
    }
    Some([0, 1, 2].map(|c| {
        v.sort_unstable_by_key(|p| p[c]);
        v[v.len() / 2][c]
    }))
}

/// Median color of the middle of a detection box, away from its background.
fn box_color(img: &RgbImage, b: [f32; 4]) -> Option<[u8; 3]> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (mx, my) = ((b[2] - b[0]) * 0.2, (b[3] - b[1]) * 0.2);
    let (x0, x1) = (((b[0] + mx) * w) as u32, ((b[2] - mx) * w) as u32);
    let (y0, y1) = (((b[1] + my) * h) as u32, ((b[3] - my) * h) as u32);
    let step = ((x1.saturating_sub(x0)).max(y1.saturating_sub(y0)) / 64).max(1);
    let mut px = Vec::new();
    for y in (y0..y1.min(img.height())).step_by(step as usize) {
        for x in (x0..x1.min(img.width())).step_by(step as usize) {
            px.push(img.get_pixel(x, y).0);
        }
    }
    median(px)
}

/// Sorts the photo's pixels into floor, walls and ceiling by where they sit against
/// the horizon, skipping object boxes: well below it is floor, well above is ceiling,
/// and a band around it, the far end of the view, stands in for the walls.
fn surface_colors(img: &RgbImage, cam: &PhotoCamera, boxes: &[[f32; 4]]) -> SurfaceColors {
    const BAND: f32 = 0.08;
    let (mut floor, mut wall, mut ceiling) = (Vec::new(), Vec::new(), Vec::new());
    let step = (img.width().max(img.height()) / 140).max(1) as usize;
    for y in (0..img.height()).step_by(step) {
        for x in (0..img.width()).step_by(step) {
            let (u, v) = ((x as f32 + 0.5) / img.width() as f32, (y as f32 + 0.5) / img.height() as f32);
            if boxes.iter().any(|b| u >= b[0] && u <= b[2] && v >= b[1] && v <= b[3]) {
                continue;
            }
            let color = img.get_pixel(x, y).0;
            let above = cam.horizon_v(u) - v;
            if above < -BAND {
                floor.push(color);
            } else if above > BAND * 1.5 {
                ceiling.push(color);
            } else if above.abs() <= BAND {
                wall.push(color);
            }
        }
    }
    SurfaceColors { floor: median(floor), walls: median(wall), ceiling: median(ceiling) }
}

fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

impl PhotoScene {
    pub fn to_json(&self) -> Value {
        let cam = &self.camera;
        let eye = cam.eye();
        let target = eye + cam.forward() * 10.0;
        let implied: Vec<f32> = self.objects.iter().filter_map(|o| o.checks["implied_camera_height_m"].as_f64().map(|v| v as f32)).collect();
        json!({
            "image_id": self.image_id,
            "camera": {
                "source": if self.camera_guess.is_some() { "your estimate" } else { "PROVISIONAL: level, 1.6 m. Call set_photo_layout with what you see." },
                "eye": [0.0, r2(eye.y), 0.0],
                "target": [r2(target.x), r2(target.y), r2(target.z)],
                "hfov_deg": r2(self.hfov_deg),
                "hfov_source": self.hfov_source,
                "height_m": r2(eye.y),
                "pitch_deg": r2(cam.forward().y.asin().to_degrees()),
                "horizon_left_v": r2(cam.horizon_v(0.0)),
                "horizon_right_v": r2(cam.horizon_v(1.0)),
                "height_implied_by_your_sizes_m": (!implied.is_empty()).then(|| r2(robust_scale(&implied))),
            },
            "building_axis_deg": r2(self.dominant_yaw.to_degrees()),
            "objects": self.objects.iter().map(|o| {
                let mut v = json!({ "id": o.id, "label": o.label, "score": r2(o.score), "box": o.bbox.map(r2), "color": o.color.map(hex) });
                match o.footprint {
                    Some(fp) => {
                        v["position_xz"] = json!([r2(fp.center.x), r2(fp.center.y)]);
                        v["position_from"] = json!(o.source);
                        v["width_m"] = json!(r2(o.lateral_width));
                        v["height_m"] = json!(r2(fp.height));
                        v["base_y"] = json!(r2(fp.base_y));
                    }
                    None => v["needs_guess"] = json!("its box doesn't reach the floor: give height_m, width_m, base_y or at_xz"),
                }
                if o.checks.as_object().is_some_and(|c| !c.is_empty()) {
                    v["checks"] = o.checks.clone();
                }
                v
            }).collect::<Vec<_>>(),
            "ceiling_height_m": self.ceiling_y.map(r2),
            "room": self.room.map(|r| r.map(r2)),
            "surface_colors": {
                "floor": self.colors.floor.map(hex),
                "walls": self.colors.walls.map(hex),
                "ceiling": self.colors.ceiling.map(hex),
            },
            "notes": "Viewer frame: meters, Y up, floor y=0, camera above the origin looking towards -Z. Positions follow from your camera by perspective: if the camera is wrong, all of them are. Depth built none of this. The overlay's cyan line is your horizon; it should pass through where parallel floor and ceiling lines converge.",
        })
    }

    /// The photo with numbered detection boxes (green) and, for placed
    /// objects, their projected bounds (orange).
    pub fn overlay(&self, photo: &RgbImage, projected: &[(usize, [f32; 4])]) -> RgbImage {
        let mut img = photo.clone();
        // The horizon, so a wrong camera shows at a glance.
        let (w, h) = (img.width(), img.height());
        for x in 0..w {
            let v = self.camera.horizon_v((x as f32 + 0.5) / w as f32);
            if (0.0..1.0).contains(&v) {
                let y = (v * h as f32) as u32;
                for dy in 0..3 {
                    if y + dy < h {
                        img.put_pixel(x, y + dy, image::Rgb([40, 220, 240]));
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn det(label: &str, score: f32, x: f32) -> Detection {
        Detection { label: label.into(), score, bbox: [x, 0.0, x + 0.2, 0.5] }
    }

    #[test]
    fn labels_keep_their_order_and_own_detections() {
        let m = merge_detections(vec![vec![det("shelf", 0.4, 0.0), det("shelf", 0.3, 0.3)], vec![det("lamp", 0.9, 0.6)]]);
        let got: Vec<(&str, f32)> = m.iter().map(|d| (d.label.as_str(), d.score)).collect();
        assert_eq!(got, [("shelf", 0.4), ("shelf", 0.3), ("lamp", 0.9)], "a strong later label must not reorder ids");
    }

    #[test]
    fn the_same_object_under_two_labels_goes_to_the_stronger() {
        let m = merge_detections(vec![vec![det("cart", 0.3, 0.0)], vec![det("pallet display", 0.5, 0.0), det("pallet display", 0.4, 0.5)]]);
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|d| d.label == "pallet display"));
    }

    /// 4:3, 60 degrees: a level camera 2 m up unless a test says otherwise.
    fn cam(height_m: f32, left: f32, right: f32) -> PhotoCamera {
        camera_from(CameraGuess { height_m, horizon_left_v: left, horizon_right_v: right }, 60.0, 640, 480)
    }

    fn fy(c: &PhotoCamera) -> f32 {
        c.fx_norm * c.aspect
    }

    #[test]
    fn the_camera_comes_from_the_horizon_and_its_height() {
        let level = cam(2.0, 0.5, 0.5);
        assert!(level.forward().y.abs() < 1e-5, "{}", level.forward());
        assert!((level.eye().y - 2.0).abs() < 1e-5);
        // A horizon low in the photo means looking up, high means looking down.
        assert!(cam(2.0, 0.7, 0.7).forward().y > 0.1);
        assert!(cam(2.0, 0.3, 0.3).forward().y < -0.1);
        for (l, r) in [(0.5, 0.5), (0.3, 0.3), (0.62, 0.62), (0.45, 0.55)] {
            let c = cam(1.7, l, r);
            assert!((c.horizon_v(0.0) - l).abs() < 1e-3 && (c.horizon_v(1.0) - r).abs() < 1e-3, "{l} {r}: {} {}", c.horizon_v(0.0), c.horizon_v(1.0));
        }
    }

    #[test]
    fn a_box_on_the_floor_lands_by_perspective_alone() {
        let c = cam(2.0, 0.5, 0.5);
        // 10 m away: 2 m below eye level.
        let vb = 0.5 + fy(&c) * 2.0 / 10.0;
        let r = locate(&c, [0.45, 0.4, 0.55, vb], &ObjectGuess::default(), 0.0, false);
        let f = r.footprint.unwrap();
        assert!((f.center.y + 10.0).abs() < 1e-3 && f.center.x.abs() < 1e-3, "{:?}", f.center);
        assert_eq!(f.base_y, 0.0);
    }

    #[test]
    fn its_height_follows_from_the_same_perspective() {
        let c = cam(2.0, 0.5, 0.5);
        // A 3 m tall thing at 10 m: bottom 2 m below eye level, top 1 m above.
        let (vt, vb) = (0.5 - fy(&c) * 1.0 / 10.0, 0.5 + fy(&c) * 2.0 / 10.0);
        let f = locate(&c, [0.45, vt, 0.55, vb], &ObjectGuess::default(), 0.0, false).footprint.unwrap();
        assert!((f.height - 3.0).abs() < 1e-2, "{}", f.height);
    }

    #[test]
    fn a_known_size_exposes_a_wrong_camera_height() {
        // The real camera is 2 m up, seeing a 1.7 m person at 6 m; we guessed 1.2 m.
        let truth = cam(2.0, 0.5, 0.5);
        let (vb, vt) = (0.5 + fy(&truth) * 2.0 / 6.0, 0.5 + fy(&truth) * 0.3 / 6.0);
        let wrong = cam(1.2, 0.5, 0.5);
        let g = ObjectGuess { height_m: Some(1.7), ..Default::default() };
        let r = locate(&wrong, [0.45, vt, 0.55, vb], &g, 0.0, false);
        assert!((r.checks["implied_camera_height_m"].as_f64().unwrap() - 2.0).abs() < 0.01, "{}", r.checks);
        assert!(r.checks["verdict"].is_string(), "{}", r.checks);
    }

    #[test]
    fn a_hanging_thing_needs_a_guess_rather_than_a_made_up_floor_position() {
        let c = cam(2.0, 0.5, 0.5);
        let b = [0.3, 0.1, 0.7, 0.3];
        assert!(locate(&c, b, &ObjectGuess::default(), 0.0, true).footprint.is_none());
        let f = locate(&c, b, &ObjectGuess { height_m: Some(1.0), ..Default::default() }, 0.0, true).footprint.unwrap();
        // Filling 0.2 of the view with 1 m puts it fy / 0.2 m away, above eye level.
        assert!((f.center.y + fy(&c) / 0.2).abs() < 1e-2, "{:?}", f.center);
        assert!(f.base_y > 2.0, "{}", f.base_y);
    }

    #[test]
    fn a_box_cut_off_by_the_photo_doesnt_trust_its_bottom_edge() {
        let c = cam(2.0, 0.5, 0.5);
        let b = [0.4, 0.5, 0.6, 1.0];
        assert!(locate(&c, b, &ObjectGuess::default(), 0.0, false).footprint.is_none());
    }

    #[test]
    fn a_stated_position_wins() {
        let c = cam(2.0, 0.5, 0.5);
        let g = ObjectGuess { at_xz: Some([3.0, -7.0]), base_y: Some(1.0), ..Default::default() };
        let f = locate(&c, [0.4, 0.5, 0.6, 0.9], &g, 0.0, false).footprint.unwrap();
        assert_eq!(f.center, Vec2::new(3.0, -7.0));
        assert_eq!(f.base_y, 1.0);
    }

    #[test]
    fn depth_only_flags_what_is_out_of_line_with_the_rest() {
        // Depth reads half the layout everywhere, except one object it puts far off.
        let items = vec![("a".to_string(), 4.0, 2.0), ("b".to_string(), 8.0, 4.0), ("c".to_string(), 12.0, 6.0), ("d".to_string(), 5.0, 8.0)];
        let (s, rows, flags) = depth_verdicts(&items);
        assert!((s - 2.0).abs() < 0.01, "{s}");
        assert_eq!(rows.len(), 4);
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert!(flags[0].starts_with("d:"));
    }

    #[test]
    fn the_scale_ignores_one_wild_guess() {
        assert_eq!(robust_scale(&[1.5, 1.6, 9.0]), 1.6);
        assert_eq!(robust_scale(&[1.0, 2.0]), 1.5);
        assert_eq!(robust_scale(&[]), 1.0);
        assert_eq!(robust_scale(&[f32::NAN, -1.0]), 1.0);
    }
}
