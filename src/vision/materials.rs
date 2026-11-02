//! Photo + detection box -> object mask -> material guess with a confidence.
//!
//! SAM 3 (the tracker export, prompted with a box) cuts the object out of its
//! box, then CLIP compares the cut-out with a few text prompts per class. The
//! guesses are priors for radio calibration and RFID tag choice, not
//! measurements.
//!
//! The SAM 3 weights are under Meta's SAM License: passing them on to a third
//! party needs a copy of that license, and it rules out some end uses (ITAR,
//! military, nuclear, espionage, weapons).
#![allow(dead_code)] // Not called from the app yet: see examples/vision_materials.rs.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use image::{Rgb, RgbImage, imageops::FilterType};
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorRef};

use super::models;
use super::runtime;

pub const SAM3_REPO: &str = "onnx-community/sam3-tracker-ONNX";
/// Which export of the repo to use: `onnx/<part>_<variant>.onnx`. fp16 because
/// q4f16 gave lower-quality masks, was slower on the CPU, and its one failing
/// DirectML run coincided with a GPU driver reset.
pub const SAM3_VARIANT: &str = "fp16";
/// Input side of the encoder (`preprocessor_config.json`): the image is
/// squashed to this square, with no padding, so normalized coordinates carry over.
const SAM3_SIZE: usize = 1008;

/// Softmax sharpness on cosine similarity. CLIP's own is about 100, which was
/// overconfident on hand-labelled photos: on the held-out ones, mean confidence
/// 0.66 at 58% right for site crops and 0.62 at 29% for product crops. 50 gave
/// 0.45 and 0.40. A guess is a prior: better low than falsely sure.
const LOGIT_SCALE: f32 = 50.0;
/// CLIP's mean colour: what the crop is padded and greyed with, 0 after normalization.
const GREY: [u8; 3] = [123, 117, 104];

/// Below these, a mask is not trusted and the plain box crop is used instead.
const MIN_MASK_IOU: f32 = 0.5;
const MIN_BOX_COVER: f32 = 0.05;
const MIN_MASK_IN_BOX: f32 = 0.5;

fn rt() -> Result<&'static runtime::Runtime> {
    runtime::init(crate::gpu::vision_device() != crate::gpu::VisionDevice::Cpu)
}

/// Fetches an export split into `.onnx` + `.onnx_data`. ONNX Runtime looks for
/// the sidecar next to the model, and `hf_file` keeps a repo's folders.
fn hf_model(repo: &str, file: &str) -> Result<PathBuf> {
    runtime::hf_file(repo, &format!("{file}_data"))?;
    runtime::hf_file(repo, file)
}

// ---------------------------------------------------------------------------
// Pure helpers

/// A box in pixels: `x0..x1`, `y0..y1`, never empty, always inside the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PxBox {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl PxBox {
    fn area(&self) -> usize {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }
}

/// `[x0, y0, x1, y1]` in order and inside 0..1, whatever came in.
fn sane_box(b: [f32; 4]) -> [f32; 4] {
    let c = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let (x0, y0, x1, y1) = (c(b[0]), c(b[1]), c(b[2]), c(b[3]));
    [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
}

fn box_to_pixels(b: [f32; 4], w: usize, h: usize) -> PxBox {
    let b = sane_box(b);
    let side = |lo: f32, hi: f32, n: usize| {
        let a = ((lo * n as f32).floor() as usize).min(n.saturating_sub(1));
        let z = ((hi * n as f32).ceil() as usize).clamp(a + 1, n.max(a + 1));
        (a, z)
    };
    let (x0, x1) = side(b[0], b[2], w);
    let (y0, y1) = side(b[1], b[3], h);
    PxBox { x0, y0, x1, y1 }
}

/// A normalized box in encoder pixels, as the prompt encoder takes it.
fn box_to_model(b: [f32; 4]) -> [f32; 4] {
    sane_box(b).map(|v| v * SAM3_SIZE as f32)
}

/// Encoder input: squash to the square, bilinear, scale to -1..1.
fn sam_pixels(img: &RgbImage) -> Vec<f32> {
    let n = SAM3_SIZE;
    let r = image::imageops::resize(img, n as u32, n as u32, FilterType::Triangle);
    let mut out = vec![0f32; 3 * n * n];
    for (i, p) in r.pixels().enumerate() {
        for c in 0..3 {
            out[c * n * n + i] = p.0[c] as f32 / 127.5 - 1.0;
        }
    }
    out
}

/// Bilinear resize of mask logits (half-pixel centres, like the reference
/// post-processing), thresholded at 0.
fn logits_to_mask(logits: &[f32], lw: usize, lh: usize, w: usize, h: usize) -> Vec<bool> {
    // For each output coordinate: the two source samples and the blend.
    let axis = |n_in: usize, n_out: usize| -> Vec<(usize, usize, f32)> {
        (0..n_out)
            .map(|i| {
                let s = ((i as f32 + 0.5) * n_in as f32 / n_out as f32 - 0.5).clamp(0.0, (n_in - 1) as f32);
                let a = s.floor() as usize;
                (a, (a + 1).min(n_in - 1), s - a as f32)
            })
            .collect()
    };
    let xs = axis(lw, w);
    let ys = axis(lh, h);
    let mut out = Vec::with_capacity(w * h);
    for &(y0, y1, fy) in &ys {
        for &(x0, x1, fx) in &xs {
            let top = logits[y0 * lw + x0] * (1.0 - fx) + logits[y0 * lw + x1] * fx;
            let bottom = logits[y1 * lw + x0] * (1.0 - fx) + logits[y1 * lw + x1] * fx;
            out.push(top * (1.0 - fy) + bottom * fy > 0.0);
        }
    }
    out
}

/// Index of the highest score, ignoring NaN.
fn best_index(scores: &[f32]) -> usize {
    let mut best = 0;
    let mut top = f32::NEG_INFINITY;
    for (i, &s) in scores.iter().enumerate() {
        if s > top {
            top = s;
            best = i;
        }
    }
    best
}

/// Why a mask should not be used for this box, if there is a reason.
fn mask_problem(m: &Mask, px: PxBox) -> Option<String> {
    if m.data.len() != m.width * m.height || px.x1 > m.width || px.y1 > m.height {
        return Some(format!("mask is {}x{}, box ends at {},{}", m.width, m.height, px.x1, px.y1));
    }
    if !(m.iou >= MIN_MASK_IOU) {
        return Some(format!("predicted IoU {:.2} below {MIN_MASK_IOU}", m.iou));
    }
    let (inside, total) = mask_counts(m, px);
    if total == 0 {
        return Some("mask is empty".into());
    }
    let cover = inside as f32 / px.area() as f32;
    if cover < MIN_BOX_COVER {
        return Some(format!("mask covers only {:.0}% of its box", cover * 100.0));
    }
    let in_box = inside as f32 / total as f32;
    if in_box < MIN_MASK_IN_BOX {
        return Some(format!("only {:.0}% of the mask is inside its box", in_box * 100.0));
    }
    None
}

/// Mask pixels inside the box, and in the whole image.
fn mask_counts(m: &Mask, px: PxBox) -> (usize, usize) {
    let total = m.data.iter().filter(|&&v| v).count();
    let mut inside = 0;
    for y in px.y0..px.y1 {
        inside += m.data[y * m.width + px.x0..y * m.width + px.x1].iter().filter(|&&v| v).count();
    }
    (inside, total)
}

/// Grows an image to a square with grey bars, so CLIP's centre crop loses nothing.
fn pad_square(img: RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    if w == h {
        return img;
    }
    let s = w.max(h);
    let mut out = RgbImage::from_pixel(s, s, Rgb(GREY));
    image::imageops::replace(&mut out, &img, ((s - w) / 2) as i64, ((s - h) / 2) as i64);
    out
}

/// What CLIP sees for a box: its crop, grey outside the mask if there is one.
fn clip_crop(img: &RgbImage, px: PxBox, mask: Option<&Mask>) -> RgbImage {
    let mut crop = image::imageops::crop_imm(img, px.x0 as u32, px.y0 as u32, (px.x1 - px.x0) as u32, (px.y1 - px.y0) as u32).to_image();
    if let Some(m) = mask {
        for (x, y, p) in crop.enumerate_pixels_mut() {
            if !m.data[(y as usize + px.y0) * m.width + x as usize + px.x0] {
                *p = Rgb(GREY);
            }
        }
    }
    pad_square(crop)
}

fn softmax(logits: &[f32]) -> Vec<f32> {
    // Relative to the largest, so nothing overflows however sharp the scale.
    let top = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let e: Vec<f32> = logits.iter().map(|l| (l - top).exp()).collect();
    let sum: f32 = e.iter().sum();
    e.iter().map(|v| v / sum).collect()
}

/// Class probabilities for an image embedding: softmax over each class's best
/// prompt, highest first.
fn score_classes(image: &[f32], classes: &[(String, Vec<Vec<f32>>)], scale: f32) -> Vec<(String, f32)> {
    let logits: Vec<f32> = classes.iter().map(|(_, prompts)| prompts.iter().map(|p| models::cosine(image, p)).fold(f32::NEG_INFINITY, f32::max) * scale).collect();
    let mut out: Vec<(String, f32)> = classes.iter().map(|(c, _)| c.clone()).zip(softmax(&logits)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

// ---------------------------------------------------------------------------
// SAM 3

/// An object mask in image pixels. `iou` is the model's own estimate of its
/// quality, not a measurement.
#[derive(Clone, Debug)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub data: Vec<bool>,
    pub iou: f32,
}

/// The encoder's output for the last image, so that several prompt sets on one
/// photo don't encode it again.
struct Encoded {
    key: u64,
    maps: Vec<(String, Vec<i64>, Vec<f32>)>,
}

pub struct Sam3 {
    encoder: Session,
    decoder: Session,
    last: Option<Encoded>,
    pub device: &'static str,
}

fn image_key(img: &RgbImage) -> u64 {
    let mut h = DefaultHasher::new();
    img.dimensions().hash(&mut h);
    img.as_raw().hash(&mut h);
    h.finish()
}

impl Sam3 {
    pub fn load() -> Result<Self> {
        Self::load_variant(SAM3_VARIANT)
    }

    /// Loads one export of the repo, by suffix ("fp16", "q4f16", ...; "" for fp32).
    pub fn load_variant(variant: &str) -> Result<Self> {
        let suffix = if variant.is_empty() { String::new() } else { format!("_{variant}") };
        let r = rt()?;
        let enc = r.load(&hf_model(SAM3_REPO, &format!("onnx/vision_encoder{suffix}.onnx"))?)?;
        let dec = r.load(&hf_model(SAM3_REPO, &format!("onnx/prompt_encoder_mask_decoder{suffix}.onnx"))?)?;
        let device = if enc.device == dec.device { enc.device } else { "mixed" };
        eprintln!("[vision] SAM 3 {variant}: encoder on {}, decoder on {}", enc.device, dec.device);
        Ok(Self { encoder: enc.session, decoder: dec.session, last: None, device })
    }

    /// The sessions' real inputs and outputs, for diagnostics.
    pub fn describe(&self) -> String {
        let mut s = String::new();
        for (name, sess) in [("encoder", &self.encoder), ("decoder", &self.decoder)] {
            for i in sess.inputs() {
                s += &format!("{name} in  {}: {}\n", i.name(), i.dtype());
            }
            for o in sess.outputs() {
                s += &format!("{name} out {}: {}\n", o.name(), o.dtype());
            }
        }
        s
    }

    fn encode(&mut self, img: &RgbImage) -> Result<()> {
        let key = image_key(img);
        if self.last.as_ref().is_some_and(|e| e.key == key) {
            return Ok(());
        }
        self.last = None;
        let n = SAM3_SIZE;
        let input = Tensor::from_array(([1usize, 3, n, n], sam_pixels(img)))?;
        let names: Vec<String> = self.encoder.outputs().iter().map(|o| o.name().to_string()).collect();
        let outputs = self.encoder.run(ort::inputs!["pixel_values" => input])?;
        let mut maps = Vec::with_capacity(names.len());
        for name in names {
            let (shape, data) = outputs[name.as_str()].try_extract_tensor::<f32>().with_context(|| format!("encoder output {name}"))?;
            maps.push((name, shape.to_vec(), data.to_vec()));
        }
        self.last = Some(Encoded { key, maps });
        Ok(())
    }

    /// One mask per normalized `[x0, y0, x1, y1]` box, of the whole image.
    pub fn segment(&mut self, img: &RgbImage, boxes: &[[f32; 4]]) -> Result<Vec<Mask>> {
        if boxes.is_empty() {
            return Ok(Vec::new());
        }
        self.encode(img)?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let Self { decoder, last, .. } = self;
        let maps = &last.as_ref().context("no encoder output")?.maps;
        let mut masks = Vec::with_capacity(boxes.len());
        for b in boxes {
            // Box only: no points, as an empty tensor (as Transformers.js does).
            let mut inputs = ort::inputs![
                "input_points" => Tensor::from_array(([1usize, 1, 0, 2], Vec::<f32>::new()))?,
                "input_labels" => Tensor::from_array(([1usize, 1, 0], Vec::<i64>::new()))?,
                "input_boxes" => Tensor::from_array(([1usize, 1, 4], box_to_model(*b).to_vec()))?,
            ];
            for (name, shape, data) in maps {
                inputs.push((name.as_str().into(), SessionInputValue::from(TensorRef::from_array_view((shape.clone(), data.as_slice()))?)));
            }
            let outputs = decoder.run(inputs)?;
            let (mshape, logits) = outputs["pred_masks"].try_extract_tensor::<f32>()?;
            let (_, ious) = outputs["iou_scores"].try_extract_tensor::<f32>()?;
            let d = mshape.len();
            if d < 3 {
                bail!("pred_masks has shape {mshape:?}");
            }
            let (lh, lw) = (mshape[d - 2] as usize, mshape[d - 1] as usize);
            if lh == 0 || lw == 0 || logits.len() != ious.len() * lw * lh {
                bail!("pred_masks {mshape:?} doesn't fit {} scores", ious.len());
            }
            let best = best_index(ious);
            let data = logits_to_mask(&logits[best * lw * lh..(best + 1) * lw * lh], lw, lh, w, h);
            masks.push(Mask { width: w, height: h, data, iou: ious[best] });
        }
        Ok(masks)
    }
}

// ---------------------------------------------------------------------------
// Material classes

/// One material guess. A prior, not a measurement.
#[derive(Clone, Debug)]
pub struct Guess {
    pub class: String,
    pub confidence: f32,
    /// Every class with its probability, highest first.
    pub scores: Vec<(String, f32)>,
    /// True if CLIP saw the masked object, false if it saw the plain box crop.
    pub masked: bool,
    /// Share of the image behind the guess: mask inside the box when masked,
    /// else the box.
    pub area: f32,
}

/// Class id -> several CLIP prompts. Keep the same number of prompts per class:
/// a class scores with its best prompt, so more prompts means higher scores.
#[derive(Clone, Debug)]
pub struct PromptSet {
    pub classes: Vec<(String, Vec<String>)>,
}

fn prompt_set(classes: &[(&str, [&str; 4])]) -> PromptSet {
    PromptSet { classes: classes.iter().map(|(id, ps)| (id.to_string(), ps.iter().map(|p| p.to_string()).collect())).collect() }
}

/// What an obstruction is made of: the `Material` classes of `rtls::radio`.
pub fn site_materials() -> PromptSet {
    prompt_set(&[
        ("thin", ["a cardboard box", "a sheet of paper or cardboard", "thin plastic film or a plastic bag", "fabric or cloth"]),
        ("wood", ["a wooden pallet", "an object made of wood", "a plywood board", "wooden planks"]),
        ("glass", ["a glass window", "an object made of glass", "a transparent glass panel", "a glass door"]),
        // No catch-all like "a photo of an object": it wins for every cut-out on grey.
        ("generic", ["a painted drywall partition", "a plastered interior wall", "a painted wall", "an object made of mixed materials"]),
        ("concrete", ["a concrete wall", "a concrete pillar", "a concrete floor", "a brick or masonry wall"]),
        ("metal", ["an object made of metal", "a steel rack, column or beam", "a car, forklift or other metal vehicle", "a painted steel machine or tank"]),
        ("liquid", ["a container full of liquid", "bottles of drink", "a jar or jug of liquid", "a body of water"]),
    ])
}

/// What a packaged product is or holds, for choosing an RFID tag.
pub fn product_contents() -> PromptSet {
    prompt_set(&[
        ("dry_goods", ["a package of dry food such as pasta or cereal", "a sack of flour or rice", "a pack of paper towels or tissues", "packets of dry goods on a shelf"]),
        ("cardboard", ["an empty cardboard box", "a stack of flattened cardboard", "a corrugated cardboard carton", "a brown cardboard shipping box"]),
        ("plastic", ["a plastic container", "a plastic crate", "a product made of hard plastic", "a shrink-wrapped plastic bundle"]),
        ("water_based", ["bottles of water", "bottles of soft drink", "a carton of milk or juice", "fresh fruit and vegetables"]),
        ("oily", ["a bottle of cooking oil", "a can of motor oil", "a jar of mayonnaise or butter", "a jug of lubricant"]),
        ("frozen", ["a bag of frozen food", "frozen food in a freezer", "a frosted package covered in ice", "frozen meat or fish"]),
        ("metal_can", ["a metal tin can of food", "an aluminium drink can", "an aerosol spray can", "a steel paint can"]),
        ("foil_lined", ["a foil-lined bag of crisps", "a shiny metallic foil pouch", "a vacuum-sealed foil coffee bag", "a foil blister pack"]),
    ])
}

/// SAM 3 for material crops, loaded on first use.
static SAM: Mutex<Option<Sam3>> = Mutex::new(None);
/// Prompt embeddings, computed once per prompt.
static PROMPTS: Mutex<BTreeMap<String, Vec<f32>>> = Mutex::new(BTreeMap::new());

/// Makes `classify` use this instance instead of loading its own.
pub fn share_sam(sam: Sam3) {
    *SAM.lock().unwrap_or_else(|e| e.into_inner()) = Some(sam);
}

/// A usable mask per box, or `None` with the reason on stderr. Never fails:
/// without a mask `classify` uses the plain crop.
fn masks_or_none(img: &RgbImage, boxes: &[[f32; 4]]) -> Vec<Option<Mask>> {
    let run = || -> Result<Vec<Mask>> {
        let mut g = SAM.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(Sam3::load().context("loading SAM 3")?);
        }
        g.as_mut().unwrap().segment(img, boxes)
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    match run() {
        Ok(masks) => masks
            .into_iter()
            .zip(boxes)
            .enumerate()
            .map(|(i, (m, b))| match mask_problem(&m, box_to_pixels(*b, w, h)) {
                Some(why) => {
                    eprintln!("[vision] box {i}: poor mask, using the plain crop: {why}");
                    None
                }
                None => Some(m),
            })
            .collect(),
        Err(e) => {
            eprintln!("[vision] no masks, using plain crops: {e:#}");
            // Whatever broke may have left the sessions dead: load afresh next time.
            *SAM.lock().unwrap_or_else(|e| e.into_inner()) = None;
            boxes.iter().map(|_| None).collect()
        }
    }
}

/// Drops every loaded model. A lost GPU device (DXGI 887A0005, seen after a
/// driver reset) kills all DirectML sessions of the process, and a new one
/// only gets a working device once the old ones are gone.
fn drop_sessions() {
    *SAM.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let m = models::models();
    *m.clip.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *m.detector.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *m.depth.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Tries twice, resetting in between.
fn retry_once<T>(mut attempt: impl FnMut() -> Result<T>, reset: impl FnOnce()) -> Result<T> {
    attempt().or_else(|e| {
        eprintln!("[vision] retrying after a failure: {e:#}");
        reset();
        attempt()
    })
}

/// Unit-length text embeddings for a prompt set, by class.
fn prompt_embeddings(clip: &mut models::Clip, prompts: &PromptSet) -> Result<Vec<(String, Vec<Vec<f32>>)>> {
    let mut cache = PROMPTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::with_capacity(prompts.classes.len());
    for (id, texts) in &prompts.classes {
        let mut embeds = Vec::with_capacity(texts.len());
        for t in texts {
            if !cache.contains_key(t) {
                cache.insert(t.clone(), clip.text(t)?);
            }
            embeds.push(cache[t].clone());
        }
        out.push((id.clone(), embeds));
    }
    Ok(out)
}

/// Guesses what is inside each normalized `[x0, y0, x1, y1]` box. With
/// `use_sam`, CLIP sees the object with its surroundings greyed out; if SAM 3
/// can't load or gives a poor mask, it sees the plain crop (`masked: false`).
/// On the test photos the mask did not make the guess better than the plain
/// crop; its value is the outline and `area`.
pub fn classify(img: &RgbImage, boxes: &[[f32; 4]], prompts: &PromptSet, use_sam: bool) -> Result<Vec<Guess>> {
    if boxes.is_empty() {
        return Ok(Vec::new());
    }
    if prompts.classes.is_empty() || prompts.classes.iter().any(|(_, p)| p.is_empty()) {
        bail!("every material class needs at least one prompt");
    }
    let (w, h) = (img.width() as usize, img.height() as usize);
    let masks: Vec<Option<Mask>> = if use_sam { masks_or_none(img, boxes) } else { boxes.iter().map(|_| None).collect() };
    let pxs: Vec<PxBox> = boxes.iter().map(|b| box_to_pixels(*b, w, h)).collect();
    let crops: Vec<RgbImage> = pxs.iter().zip(&masks).map(|(px, m)| clip_crop(img, *px, m.as_ref())).collect();
    let (images, classes) = retry_once(|| models::with(&models::models().clip, models::Clip::load, |clip| Ok((clip.images(&crops)?, prompt_embeddings(clip, prompts)?))), drop_sessions)?;
    let image_area = (w * h).max(1) as f32;
    Ok(images
        .iter()
        .zip(pxs.iter().zip(&masks))
        .map(|(embed, (px, m))| {
            let scores = score_classes(embed, &classes, LOGIT_SCALE);
            let pixels = m.as_ref().map_or(px.area(), |m| mask_counts(m, *px).0);
            Guess { class: scores[0].0.clone(), confidence: scores[0].1, scores, masked: m.is_some(), area: pixels as f32 / image_area }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(w: usize, h: usize, iou: f32, on: impl Fn(usize, usize) -> bool) -> Mask {
        let mut data = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                data.push(on(x, y));
            }
        }
        Mask { width: w, height: h, data, iou }
    }

    #[test]
    fn boxes_map_to_pixels_inside_the_image() {
        let px = box_to_pixels([0.25, 0.5, 0.75, 1.0], 100, 40);
        assert_eq!(px, PxBox { x0: 25, y0: 20, x1: 75, y1: 40 });
        // Partial pixels are kept, not rounded away.
        let px = box_to_pixels([0.101, 0.0, 0.205, 1.0], 100, 10);
        assert_eq!((px.x0, px.x1), (10, 21));
    }

    #[test]
    fn bad_boxes_still_give_a_pixel_inside_the_image() {
        assert_eq!(box_to_pixels([0.75, 0.5, 0.25, 0.1], 100, 100), PxBox { x0: 25, y0: 10, x1: 75, y1: 50 });
        assert_eq!(box_to_pixels([-1.0, -1.0, 2.0, 2.0], 10, 10), PxBox { x0: 0, y0: 0, x1: 10, y1: 10 });
        assert_eq!(box_to_pixels([1.0, 1.0, 1.0, 1.0], 10, 10), PxBox { x0: 9, y0: 9, x1: 10, y1: 10 });
        assert_eq!(box_to_pixels([f32::NAN, 0.5, f32::NAN, 0.5], 10, 10), PxBox { x0: 0, y0: 5, x1: 1, y1: 6 });
    }

    #[test]
    fn boxes_scale_to_the_encoder_square() {
        assert_eq!(box_to_model([0.0, 0.25, 0.5, 1.0]), [0.0, 252.0, 504.0, 1008.0]);
        assert_eq!(box_to_model([0.5, 1.5, 0.0, -0.5]), [0.0, 0.0, 504.0, 1008.0]);
    }

    #[test]
    fn encoder_pixels_span_minus_one_to_one() {
        let v = sam_pixels(&RgbImage::from_pixel(4, 2, Rgb([0, 255, 0])));
        let n = SAM3_SIZE * SAM3_SIZE;
        assert_eq!(v.len(), 3 * n);
        assert_eq!((v[0], v[n], v[2 * n]), (-1.0, 1.0, -1.0));
    }

    #[test]
    fn mask_resize_keeps_the_edge_where_it_was() {
        // Left half on, right half off, on a 4x2 grid.
        let logits = [5.0, 5.0, -5.0, -5.0, 5.0, 5.0, -5.0, -5.0];
        let m = logits_to_mask(&logits, 4, 2, 40, 20);
        assert_eq!(m.len(), 800);
        for y in 0..20 {
            assert!(m[y * 40 + 19]);
            assert!(!m[y * 40 + 20]);
        }
    }

    #[test]
    fn mask_resize_handles_same_size_and_downscale() {
        let logits = [1.0, -1.0, -1.0, 1.0];
        assert_eq!(logits_to_mask(&logits, 2, 2, 2, 2), vec![true, false, false, true]);
        // 1x1 output samples the centre, where the logits cancel out to 0.
        assert_eq!(logits_to_mask(&logits, 2, 2, 1, 1), vec![false]);
        assert_eq!(logits_to_mask(&[1.0], 1, 1, 3, 2), vec![true; 6]);
    }

    #[test]
    fn best_index_ignores_nan() {
        assert_eq!(best_index(&[0.1, f32::NAN, 0.9, 0.2]), 2);
        assert_eq!(best_index(&[f32::NAN, 0.1]), 1);
    }

    #[test]
    fn softmax_is_a_distribution_and_survives_sharp_scales() {
        let p = softmax(&[1.0, 2.0, 3.0]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(p[2] > p[1] && p[1] > p[0]);
        let p = softmax(&[1000.0, 0.0]);
        assert!(p[0] > 0.999 && p[1] >= 0.0 && p.iter().all(|v| v.is_finite()));
        assert_eq!(softmax(&[0.3, 0.3]), vec![0.5, 0.5]);
    }

    #[test]
    fn a_class_scores_with_its_best_prompt() {
        let classes = vec![("a".to_string(), vec![vec![1.0, 0.0], vec![0.0, 1.0]]), ("b".to_string(), vec![vec![0.5, 0.5]])];
        let s = score_classes(&[1.0, 0.0], &classes, 10.0);
        assert_eq!(s[0].0, "a");
        assert_eq!(s[1].0, "b");
        let expect = softmax(&[10.0, 5.0]);
        assert!((s[0].1 - expect[0]).abs() < 1e-6);
    }

    #[test]
    fn masking_greys_only_outside_the_mask() {
        let img = RgbImage::from_pixel(10, 10, Rgb([200, 10, 10]));
        let m = mask(10, 10, 0.9, |x, _| x < 5);
        let c = clip_crop(&img, PxBox { x0: 3, y0: 2, x1: 7, y1: 6 }, Some(&m));
        assert_eq!(c.dimensions(), (4, 4));
        assert_eq!(c.get_pixel(1, 1).0, [200, 10, 10]); // image x = 4, inside
        assert_eq!(c.get_pixel(2, 1).0, GREY); // image x = 5, outside
        let plain = clip_crop(&img, PxBox { x0: 3, y0: 2, x1: 7, y1: 6 }, None);
        assert!(plain.pixels().all(|p| p.0 == [200, 10, 10]));
    }

    #[test]
    fn long_crops_are_padded_to_a_square_not_cut() {
        let img = RgbImage::from_pixel(20, 4, Rgb([1, 2, 3]));
        let c = clip_crop(&img, PxBox { x0: 0, y0: 0, x1: 20, y1: 4 }, None);
        assert_eq!(c.dimensions(), (20, 20));
        assert_eq!(c.get_pixel(0, 0).0, GREY);
        assert_eq!(c.get_pixel(0, 8).0, [1, 2, 3]);
        assert_eq!(c.get_pixel(19, 11).0, [1, 2, 3]);
        assert_eq!(c.get_pixel(19, 12).0, GREY);
    }

    #[test]
    fn poor_masks_are_rejected() {
        let px = PxBox { x0: 0, y0: 0, x1: 10, y1: 10 };
        assert!(mask_problem(&mask(20, 20, 0.9, |x, y| x < 8 && y < 8), px).is_none());
        assert!(mask_problem(&mask(20, 20, 0.3, |x, y| x < 8 && y < 8), px).unwrap().contains("IoU"));
        assert!(mask_problem(&mask(20, 20, f32::NAN, |x, y| x < 8 && y < 8), px).unwrap().contains("IoU"));
        assert!(mask_problem(&mask(20, 20, 0.9, |_, _| false), px).unwrap().contains("empty"));
        assert!(mask_problem(&mask(20, 20, 0.9, |x, y| x < 2 && y < 2), px).unwrap().contains("covers"));
        // Fills the box but mostly spills out of it: it found the background.
        assert!(mask_problem(&mask(20, 20, 0.9, |_, _| true), px).unwrap().contains("inside"));
        assert!(mask_problem(&mask(5, 5, 0.9, |_, _| true), px).unwrap().contains("box ends"));
    }

    #[test]
    fn mask_counts_separate_inside_from_total() {
        let m = mask(10, 10, 0.9, |x, _| x < 5);
        assert_eq!(mask_counts(&m, PxBox { x0: 3, y0: 0, x1: 7, y1: 2 }), (4, 50));
    }

    #[test]
    fn retry_once_resets_between_the_two_attempts() {
        let mut calls = 0;
        let mut resets = 0;
        let r = retry_once(
            || {
                calls += 1;
                if calls == 1 { bail!("device lost") } else { Ok(7) }
            },
            || resets += 1,
        );
        assert_eq!(r.unwrap(), 7);
        assert_eq!((calls, resets), (2, 1));
        let mut calls = 0;
        assert!(
            retry_once(
                || -> Result<()> {
                    calls += 1;
                    bail!("still lost")
                },
                || {}
            )
            .is_err()
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn prompt_sets_have_the_agreed_ids_and_equal_prompt_counts() {
        let ids = |p: &PromptSet| p.classes.iter().map(|(c, _)| c.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&site_materials()), ["thin", "wood", "glass", "generic", "concrete", "metal", "liquid"]);
        assert_eq!(ids(&product_contents()), ["dry_goods", "cardboard", "plastic", "water_based", "oily", "frozen", "metal_can", "foil_lined"]);
        for set in [site_materials(), product_contents()] {
            let n = set.classes[0].1.len();
            assert!(n > 1 && set.classes.iter().all(|(_, p)| p.len() == n));
        }
    }
}
