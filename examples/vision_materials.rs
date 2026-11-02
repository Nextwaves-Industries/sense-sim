//! Detects objects in a photo, segments them with SAM 3 and guesses what they
//! are made of, with and without the mask. Prints a table and writes
//! `<photo>.masks.png`.
//! `cargo run --release --example vision_materials -- photo.png [--no-sam] [label ...]`
//! Also: `--cpu` (no GPU), `--inspect` (print the SAM 3 tensors),
//! `--variant q4f16` (another SAM 3 export), `--scores` (print every class),
//! `--thr 0.3` (detection threshold), `--max 16` (keep the best detections).
#![allow(dead_code)]

#[path = "../src/vision/geometry.rs"]
mod geometry;
#[path = "../src/vision/materials.rs"]
mod materials;
#[path = "../src/vision/models.rs"]
mod models;
#[path = "../src/vision/runtime.rs"]
mod runtime;

/// What `src/gpu.rs` gives the vision code, without Bevy and the rest of the app.
mod gpu {
    use std::sync::atomic::{AtomicBool, Ordering};

    pub static FORCE_CPU: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum VisionDevice {
        Gpu,
        LowPower,
        Cpu,
    }

    pub fn vision_device() -> VisionDevice {
        if FORCE_CPU.load(Ordering::Relaxed) { VisionDevice::Cpu } else { VisionDevice::Gpu }
    }

    pub fn set_vision_on_gpu(_: bool) {}
}

use std::time::Instant;

use image::{Rgb, RgbImage};
use materials::{Guess, Mask};

const COLORS: [[u8; 3]; 8] = [[230, 60, 60], [60, 200, 90], [70, 120, 240], [240, 190, 40], [200, 80, 220], [50, 210, 210], [245, 130, 50], [160, 220, 60]];

/// 3x5 digits, one row per 3 bits, so the overlay can be matched to the table.
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

fn fill(img: &mut RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3]) {
    for y in y0.max(0)..y1.min(img.height() as i32) {
        for x in x0.max(0)..x1.min(img.width() as i32) {
            img.put_pixel(x as u32, y as u32, Rgb(c));
        }
    }
}

fn outline(img: &mut RgbImage, b: [f32; 4], c: [u8; 3]) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (x0, y0, x1, y1) = ((b[0] * w) as i32, (b[1] * h) as i32, (b[2] * w) as i32, (b[3] * h) as i32);
    fill(img, x0, y0, x1, y0 + 2, c);
    fill(img, x0, y1 - 2, x1, y1, c);
    fill(img, x0, y0, x0 + 2, y1, c);
    fill(img, x1 - 2, y0, x1, y1, c);
}

/// The row number on a dark tag at the box corner.
fn number(img: &mut RgbImage, n: usize, b: [f32; 4], c: [u8; 3]) {
    let s = (img.width() as i32 / 300).max(3);
    let text = n.to_string();
    let (tx, ty) = ((b[0] * img.width() as f32) as i32, (b[1] * img.height() as f32) as i32);
    fill(img, tx, ty, tx + (4 * text.len() as i32 + 1) * s, ty + 7 * s, [0, 0, 0]);
    for (i, ch) in text.bytes().enumerate() {
        let d = DIGITS[(ch - b'0') as usize];
        for (r, row) in d.iter().enumerate() {
            for col in 0..3 {
                if row & (0b100 >> col) != 0 {
                    let (x, y) = (tx + s * (1 + 4 * i as i32 + col), ty + s * (1 + r as i32));
                    fill(img, x, y, x + s, y + s, c);
                }
            }
        }
    }
}

fn tint(img: &mut RgbImage, m: &Mask, c: [u8; 3]) {
    for y in 0..m.height {
        for x in 0..m.width {
            if !m.data[y * m.width + x] {
                continue;
            }
            // Solid on the edge, see-through inside.
            let at = |dx: i32, dy: i32| {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                nx >= 0 && ny >= 0 && (nx as usize) < m.width && (ny as usize) < m.height && m.data[ny as usize * m.width + nx as usize]
            };
            let edge = !(at(-1, 0) && at(1, 0) && at(0, -1) && at(0, 1));
            let p = img.get_pixel_mut(x as u32, y as u32);
            for ch in 0..3 {
                p.0[ch] = if edge { c[ch] } else { (p.0[ch] as u16 / 2 + c[ch] as u16 / 2) as u8 };
            }
        }
    }
}

fn top(g: &Guess) -> String {
    let second = g.scores.get(1).map_or(String::new(), |(c, p)| format!(" ({c} {p:.2})"));
    format!("{} {:.2}{second}", g.class, g.confidence)
}

fn all(g: &Guess) -> String {
    g.scores.iter().map(|(c, p)| format!("{c}={p:.3}")).collect::<Vec<_>>().join(" ")
}

/// Masks for the overlay and the table, timed. `classify` then shares this
/// instance, with the photo already encoded.
fn segment_timed(variant: &str, img: &RgbImage, boxes: &[[f32; 4]], inspect: bool) -> anyhow::Result<Vec<Mask>> {
    let t = Instant::now();
    let mut sam = materials::Sam3::load_variant(variant)?;
    println!("SAM 3 {variant} on {} (load {:.1}s)", sam.device, t.elapsed().as_secs_f32());
    if inspect {
        print!("{}", sam.describe());
    }
    let mut masks = Vec::new();
    if !boxes.is_empty() {
        let t = Instant::now();
        masks = sam.segment(img, &boxes[..1])?;
        println!("first box, with encoder: {:.2}s", t.elapsed().as_secs_f32());
        let t = Instant::now();
        masks.extend(sam.segment(img, &boxes[1..])?);
        println!("{} more boxes, decoder only: {:.3}s", boxes.len() - 1, t.elapsed().as_secs_f32());
    }
    materials::share_sam(sam);
    Ok(masks)
}

fn main() -> anyhow::Result<()> {
    let mut path = None;
    let mut labels = Vec::new();
    let (mut use_sam, mut inspect, mut print_scores) = (true, false, false);
    let mut variant = materials::SAM3_VARIANT.to_string();
    let mut thr = 0.3f32;
    let mut max_dets = 16usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-sam" => use_sam = false,
            "--cpu" => gpu::FORCE_CPU.store(true, std::sync::atomic::Ordering::Relaxed),
            "--inspect" => inspect = true,
            "--scores" => print_scores = true,
            "--variant" => variant = args.next().expect("--variant name"),
            "--thr" => thr = args.next().and_then(|s| s.parse().ok()).expect("--thr number"),
            "--max" => max_dets = args.next().and_then(|s| s.parse().ok()).expect("--max number"),
            _ if path.is_none() => path = Some(a),
            _ => labels.push(a),
        }
    }
    let path = path.expect("usage: vision_materials photo.png [--no-sam] [label ...]");
    if labels.is_empty() {
        labels = ["pallet rack", "pallet", "cardboard box", "forklift", "column", "wall", "door", "window", "barrel", "bottle"].iter().map(|s| s.to_string()).collect();
    }
    let img = image::open(&path)?.to_rgb8();
    println!("photo {path}: {}x{}", img.width(), img.height());

    let t = Instant::now();
    let mut det = models::Detector::load()?;
    println!("detector on {} (load {:.1}s)", det.device, t.elapsed().as_secs_f32());
    let t = Instant::now();
    // One pass per label: this Grounding-DINO export finds far less when given
    // many labels at once (7 labels gave 1 detection where one label gave 18).
    let mut dets = Vec::new();
    for l in &labels {
        dets.extend(det.run(&img, std::slice::from_ref(l), thr, thr * 0.8)?);
    }
    let found = dets.len();
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    // With one label per pass, the same box often comes back under several labels.
    let mut kept: Vec<models::Detection> = Vec::new();
    for d in dets {
        if kept.iter().all(|k| geometry::iou(k.bbox, d.bbox) < 0.8) {
            kept.push(d);
        }
    }
    let mut dets = kept;
    dets.truncate(max_dets);
    println!("{found} detections, keeping {}, in {:.2}s ({} labels)", dets.len(), t.elapsed().as_secs_f32(), labels.len());
    drop(det);
    let boxes: Vec<[f32; 4]> = dets.iter().map(|d| d.bbox).collect();

    let mut overlay = img.clone();
    let mut masks: Vec<Option<Mask>> = boxes.iter().map(|_| None).collect();
    if use_sam {
        match segment_timed(&variant, &img, &boxes, inspect) {
            Ok(found) => masks = found.into_iter().map(Some).collect(),
            // `classify` tries again below, and falls back to the plain crops.
            Err(e) => println!("no SAM 3 masks: {e:#}"),
        }
    }
    for (i, m) in masks.iter().enumerate() {
        if let Some(m) = m {
            tint(&mut overlay, m, COLORS[i % COLORS.len()]);
        }
    }
    for (i, b) in boxes.iter().enumerate() {
        outline(&mut overlay, *b, COLORS[i % COLORS.len()]);
    }
    for (i, b) in boxes.iter().enumerate() {
        number(&mut overlay, i, *b, COLORS[i % COLORS.len()]);
    }

    let sets = [("site", materials::site_materials()), ("product", materials::product_contents())];
    for (name, set) in &sets {
        let t = Instant::now();
        let plain = materials::classify(&img, &boxes, set, false)?;
        println!("\n{name} classes, box crop: {:.2}s", t.elapsed().as_secs_f32());
        let t = Instant::now();
        let cut = if use_sam { materials::classify(&img, &boxes, set, true)? } else { Vec::new() };
        if use_sam {
            println!("{name} classes, masked: {:.2}s", t.elapsed().as_secs_f32());
        }
        println!("{:>2}  {:<16} {:>4} {:>4} {:>5} {:>5}  {:<32} {:<32}", "#", "label", "det", "iou", "fill", "area", "masked", "box crop");
        for (i, d) in dets.iter().enumerate() {
            // fill: how much of its box the mask takes up.
            let c = cut.get(i);
            let box_area = ((d.bbox[2] - d.bbox[0]) * (d.bbox[3] - d.bbox[1])).max(1e-6);
            let (iou, fill) = match (&masks[i], c) {
                (Some(m), Some(g)) if g.masked => (format!("{:.2}", m.iou), format!("{:.2}", g.area / box_area)),
                (Some(m), _) => (format!("{:.2}", m.iou), "-".into()),
                _ => ("-".into(), "-".into()),
            };
            let cell = match c {
                Some(g) if g.masked => top(g),
                Some(_) => "(plain crop)".to_string(),
                None => "-".to_string(),
            };
            let area = c.map_or(plain[i].area, |g| g.area);
            let changed = if c.is_some_and(|g| g.masked && g.class != plain[i].class) { "<- differs" } else { "" };
            println!("{i:>2}  {:<16} {:.2} {iou:>4} {fill:>5} {area:>5.3}  {cell:<32} {:<32} {changed}", d.label, d.score, top(&plain[i]));
            if print_scores {
                println!("    bbox:   {:.4} {:.4} {:.4} {:.4}", d.bbox[0], d.bbox[1], d.bbox[2], d.bbox[3]);
                if let Some(g) = c {
                    println!("    masked: {}", all(g));
                }
                println!("    box:    {}", all(&plain[i]));
            }
        }
    }

    let out = format!("{path}.masks.png");
    overlay.save(&out)?;
    println!("\nwrote {out}");
    Ok(())
}
