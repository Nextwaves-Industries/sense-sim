//! Runs depth + detection on a photo and prints what the assistant would
//! measure: camera pose, detections and their footprints. Writes
//! `<photo>.overlay.png` with detection boxes and `<photo>.depth.png`.
//! `cargo run --example vision_photo -- photo.jpg [hfov_deg] [labels...]`
#[path = "../src/vision/geometry.rs"]
mod geometry;
#[path = "../src/vision/models.rs"]
mod models;
#[path = "../src/vision/runtime.rs"]
mod runtime;

use geometry::*;

fn rect(img: &mut image::RgbImage, b: [f32; 4], color: [u8; 3]) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (x0, y0) = ((b[0] * w) as i32, (b[1] * h) as i32);
    let (x1, y1) = (((b[2] * w) as i32).min(w as i32 - 1), ((b[3] * h) as i32).min(h as i32 - 1));
    for t in 0..3 {
        for x in x0..=x1 {
            for y in [y0 + t, y1 - t] {
                if x >= 0 && y >= 0 && (y as u32) < img.height() {
                    img.put_pixel(x as u32, y as u32, image::Rgb(color));
                }
            }
        }
        for y in y0..=y1 {
            for x in [x0 + t, x1 - t] {
                if x >= 0 && y >= 0 && (x as u32) < img.width() {
                    img.put_pixel(x as u32, y as u32, image::Rgb(color));
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("photo path");
    let hfov: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(65.0);
    let mut labels: Vec<String> = args.collect();
    if labels.is_empty() {
        labels = ["pallet rack", "pallet", "cardboard box", "forklift", "column", "person"].iter().map(|s| s.to_string()).collect();
    }
    let img = image::open(&path)?.to_rgb8();
    println!("photo {}x{}, hfov {hfov}", img.width(), img.height());

    let t = std::time::Instant::now();
    let mut depth = models::Depth::load()?;
    println!("depth model on {} (load {:.1}s)", depth.device, t.elapsed().as_secs_f32());
    let t = std::time::Instant::now();
    let d = depth.run(&img, hfov)?;
    println!("depth {}x{} in {:.2}s", d.width, d.height, t.elapsed().as_secs_f32());
    let mut zs: Vec<f32> = d.z.iter().copied().filter(|z| z.is_finite()).collect();
    zs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("depth range: p5 {:.1} m, median {:.1} m, p95 {:.1} m", zs[zs.len() / 20], zs[zs.len() / 2], zs[zs.len() * 19 / 20]);

    let k = Intrinsics::from_hfov(d.width, d.height, hfov);
    let floor = fit_floor(&d, &k).ok_or_else(|| anyhow::anyhow!("no floor found"))?;
    let frame = Frame::new(&floor);
    let cam = PhotoCamera::new(frame, hfov, d.width, d.height);
    println!(
        "camera height {:.2} m, pitch {:.1} deg, floor inliers {:.0}%",
        frame.h,
        cam.forward().y.asin().to_degrees(),
        floor.inlier_ratio * 100.0
    );
    match fit_ceiling(&d, &k, &frame) {
        Some(c) => println!("ceiling height {c:.2} m"),
        None => println!("ceiling: not visible"),
    }
    {
        let mut ys = Vec::new();
        for y in 0..d.height * 3 / 5 {
            for x in 0..d.width {
                let z = d.z[y * d.width + x];
                if z.is_finite() && z > 0.05 {
                    let p = frame.to_world(k.unproject(x as f32 + 0.5, y as f32 + 0.5, z));
                    if p.y > frame.h + 0.5 { ys.push(p.y); }
                }
            }
        }
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |f: f32| ys[((ys.len() - 1) as f32 * f) as usize];
        println!("upper points {}: p50 {:.2} p75 {:.2} p85 {:.2} p95 {:.2} p99 {:.2}", ys.len(), q(0.5), q(0.75), q(0.85), q(0.95), q(0.99));
    }

    let t = std::time::Instant::now();
    let mut det = models::Detector::load()?;
    println!("detector on {} (load {:.1}s)", det.device, t.elapsed().as_secs_f32());
    let t = std::time::Instant::now();
    let thr: f32 = std::env::var("DET_THR").ok().and_then(|s| s.parse().ok()).unwrap_or(0.3);
    let dets = det.run(&img, &labels, thr, thr * 0.8)?;
    println!("{} detections in {:.2}s", dets.len(), t.elapsed().as_secs_f32());

    let mut overlay = img.clone();
    let mut boxes = Vec::new();
    for dd in &dets {
        boxes.push(dd.bbox);
        rect(&mut overlay, dd.bbox, [40, 230, 90]);
        match measure_object(&d, &k, &frame, dd.bbox) {
            Some(fp) => println!(
                "  {:<14} {:.2}  at ({:6.2}, {:6.2})  {:.2} x {:.2} m, h {:.2} m, yaw {:5.1} deg, base {:.2}",
                dd.label,
                dd.score,
                fp.center.x,
                fp.center.y,
                fp.size.x,
                fp.size.y,
                fp.height,
                fp.yaw.to_degrees(),
                fp.base_y
            ),
            None => println!("  {:<14} {:.2}  (not measurable)", dd.label, dd.score),
        }
    }
    let walls = fit_walls(&d, &k, &frame, &boxes);
    for w in &walls {
        println!("wall from ({:.1}, {:.1}) to ({:.1}, {:.1}), height {:.1} m", w.a.x, w.a.y, w.b.x, w.b.y, w.height);
    }
    if let Some(fe) = floor_extent(&d, &k, &frame) {
        println!("floor seen: x {:.1}..{:.1}, z {:.1}..{:.1}", fe[0], fe[2], fe[1], fe[3]);
    }
    overlay.save(format!("{path}.overlay.png"))?;
    let max = zs[zs.len() * 19 / 20];
    let dimg = image::GrayImage::from_fn(d.width as u32, d.height as u32, |x, y| {
        let z = d.z[y as usize * d.width + x as usize];
        image::Luma([(255.0 * (1.0 - (z / max).clamp(0.0, 1.0))) as u8])
    });
    dimg.save(format!("{path}.depth.png"))?;
    Ok(())
}
