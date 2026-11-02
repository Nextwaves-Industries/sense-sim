//! Spike S2: run SolidCraft's whole UI headless and save what it drew as a PNG.
//!
//!     cargo run --release --example design_host --features design -- [out.png] [--cpu-viewport]

#[path = "../src/design/host.rs"]
mod host;

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gpu_viewport = !args.iter().any(|a| a == "--cpu-viewport");
    let out = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| "output/design_host.png".into());

    let mut session = solidcraft_engine::Session::default();
    session.run_script(&solidcraft_engine::sample::script()).map_err(|e| anyhow::anyhow!("sample script: {e}"))?;
    session.mark_saved();

    let size = [1600, 900];
    let t0 = Instant::now();
    let mut host = host::DesignHost::new(session, size, gpu_viewport)?;
    println!("started in {:.0} ms on {} (gpu viewport: {})", t0.elapsed().as_secs_f64() * 1000.0, host.adapter_name(), host.gpu_viewport());
    host.app.home.open = false;
    host.app.cam.set_view(solidcraft_engine::render::StandardView::Iso);
    host.app.fit_view();

    let mut costs = Vec::new();
    let mut shown = 0;
    let frames = 100;
    let loop_start = Instant::now();
    for i in 0..frames {
        let raw = egui::RawInput { focused: true, time: Some(i as f64 / 60.0), ..Default::default() };
        costs.push(host.frame(raw, 1.0).cost_ms);
        if host.take_pixels().is_some() {
            shown += 1;
        }
    }
    let total = loop_start.elapsed().as_secs_f64() * 1000.0;
    costs.sort_by(f64::total_cmp);
    println!("{frames} frames in {total:.0} ms; frame cost median {:.1} ms, p95 {:.1} ms, max {:.1} ms; {shown} read back", costs[costs.len() / 2], costs[costs.len() * 95 / 100], costs[costs.len() - 1]);

    // One more, to save.
    host.frame(egui::RawInput { focused: true, ..Default::default() }, 1.0);
    let (pixels, [w, h]) = host.wait_pixels().ok_or_else(|| anyhow::anyhow!("no frame was read back"))?;
    let lit = pixels.chunks_exact(4).filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8).count();
    println!("{w}x{h}: {:.0}% of pixels not black", 100.0 * lit as f64 / (w * h) as f64);
    image::save_buffer(&out, &pixels, w, h, image::ColorType::Rgba8)?;
    println!("saved {out}");
    host.shutdown();
    Ok(())
}
