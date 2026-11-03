//! A picture of an antenna, the way it's meant to be read. A real model read a sideways photo as it
//! stood, called its left side -y and its top +x, and drew the mirror image. It couldn't see that when
//! it checked, with a sideways sketch to hold against an upright drawing. So it turns the picture
//! first, and what it drew comes back underneath it, the same way up.

use std::sync::Arc;

use bevy::prelude::*;
use image::RgbImage;
use serde_json::{Value, json};

use super::Studio;
use crate::agent::llm::Block;
use crate::agent::tools::ToolOutput;
use crate::agent::{Agent, ChatItem, images};

/// Enough to see a pen stroke, and small enough not to weigh on every later call to the model.
const LONG_SIDE: u32 = 1500;
/// A stored photo is at most 1600 px: past this, enlarging shows nothing new.
const MOST_ENLARGED: f64 = 4.0;

/// The picture as it was last looked at, which later drawings come back under.
pub struct Sketch {
    /// The image it was made from.
    pub source: String,
    /// This view in the chat, which says the view belongs to the chat that's open.
    pub shown: String,
    pub img: Arc<RgbImage>,
}

/// Turns a picture clockwise by a whole number of quarters, then cuts out `crop`: left, top, right and
/// bottom, as shares of the picture once it's turned. Never a flip, so nothing can come out mirrored.
pub fn turned_crop(photo: &RgbImage, turn: u64, crop: Option<[f64; 4]>) -> Result<RgbImage, String> {
    let turned = match turn {
        0 => photo.clone(),
        90 => image::imageops::rotate90(photo),
        180 => image::imageops::rotate180(photo),
        270 => image::imageops::rotate270(photo),
        other => return Err(format!("turn is 0, 90, 180 or 270 degrees clockwise, not {other}.")),
    };
    let (w, h) = turned.dimensions();
    let [left, top, right, bottom] = crop.unwrap_or([0.0, 0.0, 1.0, 1.0]).map(|v| v.clamp(0.0, 1.0));
    let (x0, x1) = ((left * w as f64) as u32, (right * w as f64).ceil() as u32);
    let (y0, y1) = ((top * h as f64) as u32, (bottom * h as f64).ceil() as u32);
    if x1 <= x0 || y1 <= y0 {
        return Err(format!("box [{left}, {top}, {right}, {bottom}] holds nothing: it's left, top, right, bottom, as shares from 0 to 1 of the picture once it's turned."));
    }
    let cut = image::imageops::crop_imm(&turned, x0, y0, x1 - x0, y1 - y0).to_image();
    let scale = (LONG_SIDE as f64 / cut.width().max(cut.height()) as f64).min(MOST_ENLARGED);
    if (scale - 1.0).abs() < 0.01 {
        return Ok(cut);
    }
    Ok(image::imageops::resize(&cut, (cut.width() as f64 * scale).round() as u32, (cut.height() as f64 * scale).round() as u32, image::imageops::FilterType::CatmullRom))
}

fn jpeg(img: &RgbImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img.clone()).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)).ok()?;
    Some(out.into_inner())
}

pub fn view_tool(world: &mut World, i: &Value) -> ToolOutput {
    let id = i["image_id"].as_str().unwrap_or("");
    let Some((data, _)) = images::get(id) else { return ToolOutput::err(format!("No picture '{id}': the chat gives each one an image_id.")) };
    let Ok(photo) = image::load_from_memory(&data) else { return ToolOutput::err(format!("Picture '{id}' can't be read.")) };
    let crop = match i.get("box").filter(|b| !b.is_null()) {
        None => None,
        Some(b) => match b.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<_>>()).and_then(|a| <[f64; 4]>::try_from(a).ok()) {
            Some(b) => Some(b),
            None => return ToolOutput::err("box is four numbers: left, top, right, bottom, as shares from 0 to 1 of the picture once it's turned."),
        },
    };
    let view = match turned_crop(&photo.to_rgb8(), i["turn"].as_u64().unwrap_or(0), crop) {
        Ok(v) => v,
        Err(e) => return ToolOutput::err(e),
    };
    let Some(bytes) = jpeg(&view) else { return ToolOutput::err("The view couldn't be encoded.") };
    let bytes = Arc::new(bytes);
    let shown = images::put(bytes.clone(), "image/jpeg");
    if let Some(mut agent) = world.get_resource_mut::<Agent>() {
        agent.transcript.push(ChatItem::Image(shown.clone()));
    }
    let (wide, tall) = view.dimensions();
    let mut st = world.resource_mut::<Studio>();
    st.sketch = Some(Sketch { source: id.to_string(), shown, img: Arc::new(view) });
    st.outline_asked = false;
    let said = json!({
        "ok": true,
        "size_px": [wide, tall],
        "note": "Read the antenna from this view, not the original. If the writing isn't the right way up, or the antenna doesn't fill it, call again with another turn or box. Across this view is x and up it is y. List every label written on it, exactly as written, because each becomes a variable of that name. A closed outline is the edge of filled metal. antenna_draw now brings its drawing back under this view."
    });
    ToolOutput { content: vec![Block::Text(said.to_string()), Block::Image { data: bytes, mime: "image/jpeg".into() }], is_error: false }
}

/// The view of the sketch, if it was made for the chat that's open. A sketch from an earlier chat has
/// nothing to do with what's being drawn now.
pub fn current(world: &World) -> Option<Arc<RgbImage>> {
    let sketch = world.resource::<Studio>().sketch.as_ref()?;
    match world.get_resource::<Agent>() {
        Some(a) if !a.transcript.iter().any(|c| matches!(c, ChatItem::Image(id) if *id == sketch.shown)) => None,
        _ => Some(sketch.img.clone()),
    }
}

/// A picture attached to this chat that hasn't been looked at, the first time it's asked. It may have
/// nothing to do with the drawing, so a second try goes through.
pub fn unread(world: &mut World) -> Option<String> {
    let attached = world.get_resource::<Agent>()?.transcript.iter().rev().find_map(|c| match c {
        ChatItem::User { images, .. } => images.last().cloned(),
        _ => None,
    })?;
    let mut st = world.resource_mut::<Studio>();
    if st.sketch.as_ref().is_some_and(|s| s.source == attached) || st.sketch_asked.as_deref() == Some(attached.as_str()) {
        return None;
    }
    st.sketch_asked = Some(attached.clone());
    Some(attached)
}

/// Parts drawn as a strip along a path that ends where it began.
fn closed_strips(i: &Value) -> Vec<String> {
    i["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"] == "strip")
        .filter(|p| p["path"].as_array().is_some_and(|path| path.len() > 3 && path.first() == path.last()))
        .map(|p| p["name"].as_str().unwrap_or("a strip").to_string())
        .collect()
}

/// A closed path drawn from a sketch, the first time. In a sketch that's the edge of filled metal: a
/// real run drew a user's filled tag as 1 mm wires, and that's another antenna. It may really be a
/// wire loop, so a second try goes through.
pub fn wire_for_an_outline(world: &mut World, i: &Value) -> Option<String> {
    current(world)?;
    let closed = closed_strips(i);
    let mut st = world.resource_mut::<Studio>();
    if closed.is_empty() || st.outline_asked {
        return None;
    }
    st.outline_asked = true;
    Some(format!("Nothing was drawn. {} runs along a closed outline, as a wire following the line. In a sketch a closed outline is the edge of filled metal, so draw it as kind polygon, with pts along the outline. If this one really is a wire loop, because a trace width is written on it or the user said so, send the same call again.", closed.join(", ")))
}

/// The sketch on top and the drawing under it, both as wide as the drawing: what's in the same corner
/// of each belongs together, so a mirror image shows. A sketch isn't to scale, so they aren't overlaid.
pub fn beside(sketch: &RgbImage, drawing_png: &[u8]) -> Option<Vec<u8>> {
    const BAND: u32 = 8;
    let drawing = image::load_from_memory(drawing_png).ok()?.to_rgb8();
    let wide = drawing.width();
    let tall = (sketch.height() as f64 * wide as f64 / sketch.width().max(1) as f64).round().max(1.0) as u32;
    let top = image::imageops::resize(sketch, wide, tall, image::imageops::FilterType::CatmullRom);
    let mut both = RgbImage::from_pixel(wide, tall + BAND + drawing.height(), image::Rgb([60, 60, 60]));
    image::imageops::replace(&mut both, &top, 0, 0);
    image::imageops::replace(&mut both, &drawing, 0, i64::from(tall + BAND));
    jpeg(&both)
}

/// Where the pen went: much darker than the page around it, in strokes long enough not to be the
/// crossings of the printed grid. A grid line is fainter than a pen and breaks up at this threshold.
struct Ink {
    w: usize,
    h: usize,
    on: Vec<bool>,
}

fn pen_strokes(sketch: &RgbImage) -> Option<Ink> {
    const LONG_SIDE: u32 = 1000;
    let long = sketch.width().max(sketch.height());
    let small;
    let img = if long > LONG_SIDE {
        small = image::imageops::resize(sketch, sketch.width() * LONG_SIDE / long, sketch.height() * LONG_SIDE / long, image::imageops::FilterType::Triangle);
        &small
    } else {
        sketch
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 8 || h < 8 {
        return None;
    }
    let grey: Vec<f64> = img.pixels().map(|p| 0.3 * p.0[0] as f64 + 0.59 * p.0[1] as f64 + 0.11 * p.0[2] as f64).collect();
    // A running sum, for the mean of the page around each pixel.
    let mut sum = vec![0.0; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            sum[(y + 1) * (w + 1) + x + 1] = grey[y * w + x] + sum[y * (w + 1) + x + 1] + sum[(y + 1) * (w + 1) + x] - sum[y * (w + 1) + x];
        }
    }
    let reach = w.max(h) / 40;
    let dark: Vec<f64> = (0..w * h)
        .map(|k| {
            let (x, y) = (k % w, k / w);
            let (x0, x1, y0, y1) = (x.saturating_sub(reach), (x + reach + 1).min(w), y.saturating_sub(reach), (y + reach + 1).min(h));
            let mean = (sum[y1 * (w + 1) + x1] - sum[y0 * (w + 1) + x1] - sum[y1 * (w + 1) + x0] + sum[y0 * (w + 1) + x0]) / ((x1 - x0) * (y1 - y0)) as f64;
            (mean - grey[k]).max(0.0)
        })
        .collect();
    let mut sorted = dark.clone();
    let top = (sorted.len() as f64 * 0.998) as usize;
    let (_, &mut darkest, _) = sorted.select_nth_unstable_by(top.min(dark.len() - 1), f64::total_cmp);
    if darkest < 20.0 {
        return None;
    }
    let mut on: Vec<bool> = dark.iter().map(|d| *d > darkest * 0.45).collect();
    // Only long pieces, and the box they fill: the rest of the crop isn't antenna.
    let shortest = (w.max(h) as f64 * 0.025) as usize;
    let mut seen = vec![false; w * h];
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    for start in 0..w * h {
        if !on[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut piece = vec![start];
        let (mut x0, mut y0, mut x1, mut y1) = (start % w, start / w, start % w, start / w);
        let mut next = 0;
        while next < piece.len() {
            let k = piece[next];
            next += 1;
            let (x, y) = (k % w, k / w);
            (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let n = ny * w + nx;
                    if on[n] && !seen[n] {
                        seen[n] = true;
                        piece.push(n);
                    }
                }
            }
        }
        if (x1 - x0).max(y1 - y0) < shortest {
            piece.iter().for_each(|&k| on[k] = false);
        } else {
            (left, right, top, bottom) = (left.min(x0), right.max(x1), top.min(y0), bottom.max(y1));
        }
    }
    if right <= left || bottom <= top {
        return None;
    }
    let (cw, ch) = (right - left + 1, bottom - top + 1);
    let mut cut = vec![false; cw * ch];
    for y in 0..ch {
        cut[y * cw..(y + 1) * cw].copy_from_slice(&on[(y + top) * w + left..(y + top) * w + left + cw]);
    }
    Some(Ink { w: cw, h: ch, on: cut })
}

/// Everything within `by` cells of something set.
fn grown(on: &[bool], w: usize, h: usize, by: usize) -> Vec<bool> {
    let mut out = vec![false; on.len()];
    for k in (0..on.len()).filter(|k| on[*k]) {
        let (x, y) = (k % w, k / w);
        for ny in y.saturating_sub(by)..=(y + by).min(h - 1) {
            out[ny * w + x.saturating_sub(by)..=ny * w + (x + by).min(w - 1)].fill(true);
        }
    }
    out
}

/// What it says when a drawing fits the pen strokes far better turned over, which is what a mirror
/// image does. The two are only compared, never scored against a pass mark: a good reading of a sketch
/// doesn't follow its wobble, so a symmetric drawing of a lopsided sketch fits it only about as well
/// either way, and says nothing. On the real photo, a real model's lopsided drawing scored 31 as drawn
/// and 75 with its top and bottom swapped. That cuts both ways: the filled symmetric drawing, turned
/// over on purpose, wasn't caught, so this catches a drawing that follows the pen and not a symmetric
/// one upside down. A crop with an arrow running well outside the antenna fits nothing, and says nothing.
pub fn mirror_hint(sketch: &RgbImage, metal: &[Vec<[f64; 2]>]) -> Option<String> {
    let ink = pen_strokes(sketch)?;
    let (w, h) = (ink.w, ink.h);
    let every = || metal.iter().flatten();
    let (x0, x1) = every().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q[0]), b.max(q[0])));
    let (y0, y1) = every().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| (a.min(q[1]), b.max(q[1])));
    if !(x1 > x0 && y1 > y0) {
        return None;
    }
    let mut drawn = vec![false; w * h];
    for shape in metal {
        let points: Vec<(f64, f64)> = shape.iter().map(|q| ((q[0] - x0) / (x1 - x0) * w as f64, (y1 - q[1]) / (y1 - y0) * h as f64)).collect();
        super::tagjob::fill_outline(&points, w as u32, h as u32, |c, r| drawn[r as usize * w + c as usize] = true);
    }
    // The pen draws the edge of the metal, and the inside of a filled part has no ink to be near.
    let edge: Vec<bool> = (0..w * h)
        .map(|k| {
            let (x, y) = (k % w, k / w);
            drawn[k] && (x == 0 || y == 0 || x == w - 1 || y == h - 1 || !drawn[k - 1] || !drawn[k + 1] || !drawn[k - w] || !drawn[k + w])
        })
        .collect();
    let by = ((w as f64 * 0.008).round() as usize).max(1);
    let near_ink = grown(&ink.on, w, h, by);
    let fit = |left_right: bool, top_bottom: bool| {
        let turned: Vec<bool> = (0..w * h)
            .map(|k| {
                let (x, y) = (k % w, k / w);
                edge[(if top_bottom { h - 1 - y } else { y }) * w + if left_right { w - 1 - x } else { x }]
            })
            .collect();
        let share = |of: &[bool], near: &[bool]| {
            let all = of.iter().filter(|v| **v).count();
            100.0 * of.iter().zip(near).filter(|(a, b)| **a && **b).count() as f64 / all.max(1) as f64
        };
        let (metal_on_ink, ink_on_metal) = (share(&turned, &near_ink), share(&ink.on, &grown(&turned, w, h, by)));
        2.0 * metal_on_ink * ink_on_metal / (metal_on_ink + ink_on_metal).max(1e-9)
    };
    let as_drawn = fit(false, false);
    let options = [("its top and bottom swapped", fit(false, true)), ("its left and right swapped", fit(true, false)), ("turned half way round", fit(true, true))];
    // On a drawing symmetric left to right, a half turn and a swap of top and bottom are the same
    // picture, and the swap is what to tell it.
    let best = options.iter().map(|o| o.1).fold(f64::MIN, f64::max);
    let (name, turned) = options.into_iter().find(|o| o.1 >= best - 1.0)?;
    (turned >= as_drawn * 1.4 && turned >= as_drawn + 10.0).then(|| format!("This drawing fits the pen strokes in the sketch far better with {name}: {turned:.0} against {as_drawn:.0} as drawn, out of 100. It looks like a mirror image of what you meant to draw. In the view up is +y and across is +x: go through the top run of the sketch against the top run of the drawing, and redraw it."))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths in pen on grid paper, across 69 mm by 19, leaning a little.
    fn in_pen(paths: &[&[[f64; 2]]]) -> RgbImage {
        let (w, h) = (1200_u32, 360_u32);
        let mut img = RgbImage::from_fn(w, h, |x, y| if x % 60 == 0 || y % 60 == 0 { image::Rgb([190, 190, 200]) } else { image::Rgb([235, 235, 225]) });
        // On the paper: y runs down, with a margin, and the whole thing leans a touch.
        let place = |p: [f64; 2]| (30.0 + (p[0] + 34.5) / 69.0 * 1140.0, 30.0 + (9.5 - p[1]) / 19.0 * 290.0 + p[0] * 0.1);
        for path in paths {
            for pair in path.windows(2) {
                let (a, b) = (place(pair[0]), place(pair[1]));
                for t in 0..2000 {
                    let t = t as f64 / 2000.0;
                    let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        img.put_pixel((x as u32 + dx).min(w - 1), (y as u32 + dy).min(h - 1), image::Rgb([40, 40, 90]));
                    }
                }
            }
        }
        img
    }

    fn flipped(metal: &[Vec<[f64; 2]>], x: f64, y: f64) -> Vec<Vec<[f64; 2]>> {
        metal.iter().map(|s| s.iter().map(|q| [q[0] * x, q[1] * y]).collect()).collect()
    }

    /// A user's tag: each half a closed outline of filled metal, both drawn in pen.
    #[test]
    fn a_filled_part_is_held_against_the_pen_by_its_edge() {
        let half: Vec<[f64; 2]> = vec![[0.5, -0.5], [0.5, 9.5], [10.0, 9.5], [10.0, -0.5], [18.5, -0.5], [18.5, 9.5], [34.5, 9.5], [34.5, -9.5], [30.0, -9.5], [30.0, -0.5], [24.5, -0.5], [24.5, -9.5], [4.5, -9.5], [4.5, -0.5], [0.5, -0.5]];
        let other: Vec<[f64; 2]> = half.iter().map(|q| [-q[0], q[1]]).collect();
        let img = in_pen(&[half.as_slice(), other.as_slice()]);
        let metal: Vec<Vec<[f64; 2]>> = [&half, &other].iter().map(|s| s.iter().map(|q| [q[0] * 1e-3, q[1] * 1e-3]).collect()).collect();
        assert_eq!(mirror_hint(&img, &metal), None, "drawn as sketched");
        let said = mirror_hint(&img, &flipped(&metal, 1.0, -1.0)).expect("top and bottom swapped");
        assert!(said.contains("top and bottom swapped"), "{said}");
    }

    /// The two lopsided lines a real model drew, in pen on grid paper, leaning a little.
    fn a_sketch() -> (RgbImage, Vec<Vec<[f64; 2]>>) {
        let paths: [&[[f64; 2]]; 4] = [
            &[[34.5, -1.5], [23.0, -1.5], [23.0, 2.7], [17.5, 2.7], [17.5, -4.2], [7.5, -4.2], [7.5, -1.0], [4.5, -1.0], [4.5, -5.8], [-3.0, -5.8], [-3.0, 1.0], [-15.3, 1.0], [-15.3, -9.5], [-34.5, -9.5]],
            &[[34.5, 9.5], [30.8, 9.5], [30.8, 4.5], [27.3, 4.5], [27.3, 9.5], [12.2, 9.5], [12.2, 1.0], [2.6, 1.0], [2.6, 7.2], [-21.8, 7.2], [-21.8, 1.0], [-29.8, 1.0], [-29.8, 9.5], [-34.5, 9.5]],
            &[[34.5, -1.5], [34.5, 9.5]],
            &[[-34.5, -9.5], [-34.5, 9.5]],
        ];
        let img = in_pen(&paths);
        // Each leg as a strip of metal 1 mm wide, in metres.
        let metal = paths
            .iter()
            .flat_map(|path| path.windows(2))
            .map(|pair| {
                let (x0, x1) = (pair[0][0].min(pair[1][0]) * 1e-3, pair[0][0].max(pair[1][0]) * 1e-3);
                let (y0, y1) = (pair[0][1].min(pair[1][1]) * 1e-3, pair[0][1].max(pair[1][1]) * 1e-3);
                vec![[x0 - 5e-4, y0 - 5e-4], [x1 + 5e-4, y0 - 5e-4], [x1 + 5e-4, y1 + 5e-4], [x0 - 5e-4, y1 + 5e-4]]
            })
            .collect();
        (img, metal)
    }

    #[test]
    fn a_mirror_image_shows_against_the_pen_strokes_and_the_right_way_up_says_nothing() {
        let (img, metal) = a_sketch();
        assert_eq!(mirror_hint(&img, &metal), None, "drawn as sketched");
        let said = mirror_hint(&img, &flipped(&metal, 1.0, -1.0)).expect("top and bottom swapped");
        assert!(said.contains("top and bottom swapped"), "{said}");
        let said = mirror_hint(&img, &flipped(&metal, -1.0, 1.0)).expect("left and right swapped");
        assert!(said.contains("left and right swapped"), "{said}");
    }

    /// A blank page or a bare grid has no strokes to hold a drawing against.
    #[test]
    fn with_no_pen_on_the_page_it_says_nothing() {
        let (_, metal) = a_sketch();
        assert_eq!(mirror_hint(&RgbImage::from_pixel(600, 200, image::Rgb([235, 235, 225])), &metal), None);
    }

    /// Red in the top left corner, of a picture twice as wide as it's tall.
    fn cornered() -> RgbImage {
        RgbImage::from_fn(40, 20, |x, y| if x < 10 && y < 5 { image::Rgb([250, 0, 0]) } else { image::Rgb([250, 250, 250]) })
    }

    fn red_at(img: &RgbImage, x: f64, y: f64) -> bool {
        let p = img.get_pixel((x * img.width() as f64) as u32, (y * img.height() as f64) as u32).0;
        p[0] > 180 && p[1] < 80
    }

    /// A flip would keep the corner on the same edge: this is what tells a turn from a mirror image.
    #[test]
    fn a_picture_is_turned_and_never_flipped() {
        let quarter = turned_crop(&cornered(), 90, None).unwrap();
        assert!(quarter.height() > quarter.width(), "{:?}", quarter.dimensions());
        assert!(red_at(&quarter, 0.9, 0.05), "a quarter clockwise takes the top left to the top right");
        assert!(!red_at(&quarter, 0.1, 0.05));
        assert!(red_at(&turned_crop(&cornered(), 180, None).unwrap(), 0.9, 0.9));
        assert!(red_at(&turned_crop(&cornered(), 270, None).unwrap(), 0.1, 0.9));
        assert!(turned_crop(&cornered(), 45, None).unwrap_err().contains("clockwise"));
    }

    #[test]
    fn a_box_is_cut_from_the_turned_picture_and_enlarged() {
        // The top right quarter of the turned picture, where the red went.
        let cut = turned_crop(&cornered(), 90, Some([0.5, 0.0, 1.0, 0.5])).unwrap();
        assert!(red_at(&cut, 0.9, 0.1), "still in the top right of the cut");
        assert_eq!(cut.dimensions(), (40, 80), "10 by 20, as far as it's enlarged");
        assert!(turned_crop(&cornered(), 0, Some([0.6, 0.0, 0.2, 1.0])).unwrap_err().contains("holds nothing"));
    }

    #[test]
    fn the_sketch_goes_on_top_of_the_drawing_at_its_width() {
        let drawing = RgbImage::from_pixel(200, 50, image::Rgb([0, 0, 250]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(drawing).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let both = beside(&cornered(), &png.into_inner()).unwrap();
        let img = image::load_from_memory(&both).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (200, 100 + 8 + 50));
        assert!(red_at(&img, 0.05, 0.05), "the sketch's corner stays its corner");
        let low = img.get_pixel(100, 150).0;
        assert!(low[2] > 200 && low[0] < 60, "the drawing's underneath: {low:?}");
    }
}
