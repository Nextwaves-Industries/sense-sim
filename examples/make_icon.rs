//! Renders assets/logo.svg to assets/logo.png (256 px) and a multi-size assets/icon.ico.
use resvg::{tiny_skia, usvg};

fn render(tree: &usvg::Tree, size: u32) -> image::RgbaImage {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
    let s = size as f32 / tree.size().width();
    resvg::render(tree, tiny_skia::Transform::from_scale(s, s), &mut pixmap.as_mut());
    // tiny-skia is premultiplied; image wants straight alpha.
    let mut img = image::RgbaImage::from_raw(size, size, pixmap.take()).unwrap();
    for p in img.pixels_mut() {
        let a = p.0[3] as f32 / 255.0;
        if a > 0.0 {
            for c in 0..3 {
                p.0[c] = ((p.0[c] as f32 / a).round()).min(255.0) as u8;
            }
        }
    }
    img
}

fn main() -> anyhow::Result<()> {
    let svg = std::fs::read("assets/logo.svg")?;
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())?;
    let mut opts = usvg::Options { resources_dir: Some("assets".into()), ..Default::default() };
    opts.fontdb_mut().load_system_fonts();
    let wm = usvg::Tree::from_data(&std::fs::read("assets/logo_wordmark.svg")?, &opts)?;
    let (w, h) = (1520u32, 512u32);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
    resvg::render(&wm, tiny_skia::Transform::from_scale(2.0, 2.0), &mut pixmap.as_mut());
    pixmap.save_png("assets/logo_wordmark.png")?;
    render(&tree, 110).save("assets/logo_small.png")?;
    render(&tree, 256).save("assets/logo.png")?;
    render(&tree, 1024).save("assets/logo_1024.png")?;
    let frames: Vec<image::codecs::ico::IcoFrame> = [16u32, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&s| {
            let img = render(&tree, s);
            image::codecs::ico::IcoFrame::as_png(img.as_raw(), s, s, image::ExtendedColorType::Rgba8).unwrap()
        })
        .collect();
    let f = std::fs::File::create("assets/icon.ico")?;
    image::codecs::ico::IcoEncoder::new(f).encode_images(&frames)?;
    println!("wrote assets/logo.png, assets/logo_1024.png, assets/icon.ico");
    Ok(())
}
