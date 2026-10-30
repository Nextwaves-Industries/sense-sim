//! The three vision models, with their pre- and post-processing taken from
//! each model's published `preprocessor_config.json`:
//! - Metric3D v2 (ViT-S): metric depth, canonical focal 1000 px;
//! - Grounding-DINO tiny: open-vocabulary detection from a text prompt;
//! - CLIP ViT-B/32: image and text embeddings for asset matching.

use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use image::{RgbImage, imageops::FilterType};
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::geometry::DepthMap;
use super::runtime;

pub const DEPTH_REPO: &str = "onnx-community/metric3d-vit-small";
pub const DETECT_REPO: &str = "onnx-community/grounding-dino-tiny-ONNX";
pub const CLIP_REPO: &str = "Xenova/clip-vit-base-patch32";

fn rt() -> Result<&'static runtime::Runtime> {
    runtime::init(crate::gpu::vision_device() != crate::gpu::VisionDevice::Cpu)
}

/// Planar CHW floats from an RGB image, mapped per channel by `f`.
fn chw(img: &RgbImage, f: impl Fn(usize, u8) -> f32) -> Vec<f32> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut out = vec![0f32; 3 * w * h];
    for (i, p) in img.pixels().enumerate() {
        for c in 0..3 {
            out[c * w * h + i] = f(c, p.0[c]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Depth

pub struct Depth {
    session: Session,
    pub device: &'static str,
}

impl Depth {
    pub fn load() -> Result<Self> {
        let l = rt()?.load(&runtime::hf_file(DEPTH_REPO, "onnx/model.onnx")?)?;
        Ok(Self { session: l.session, device: l.device })
    }

    /// Metric depth of `img` for a camera with horizontal FOV `hfov_deg`.
    pub fn run(&mut self, img: &RgbImage, hfov_deg: f32) -> Result<DepthMap> {
        // DPT-style resize: keep the aspect ratio, scale towards 518 px using
        // whichever side needs the smaller change, round to multiples of 14.
        let (w, h) = (img.width() as f32, img.height() as f32);
        let (sw, sh) = (518.0 / w, 518.0 / h);
        let s = if (1.0 - sw).abs() < (1.0 - sh).abs() { sw } else { sh };
        let nw = (((w * s) / 14.0).round() * 14.0).max(14.0) as u32;
        let nh = (((h * s) / 14.0).round() * 14.0).max(14.0) as u32;
        let resized = image::imageops::resize(img, nw, nh, FilterType::CatmullRom);
        // The exported graph normalizes internally (no rescale/normalize in the config).
        let data = chw(&resized, |_, v| v as f32);
        let input = Tensor::from_array(([1usize, 3, nh as usize, nw as usize], data))?;
        let outputs = self.session.run(ort::inputs!["pixel_values" => input])?;
        let (shape, depth) = outputs["predicted_depth"].try_extract_tensor::<f32>()?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        let (dh, dw) = (dims[dims.len() - 2], dims[dims.len() - 1]);
        // Canonical camera has a 1000 px focal length at the network input size.
        let fx_in = (nw as f32 * 0.5) / (hfov_deg.to_radians() * 0.5).tan();
        let k = fx_in / 1000.0;
        Ok(DepthMap { width: dw, height: dh, z: depth.iter().map(|d| d * k).collect() })
    }
}

// ---------------------------------------------------------------------------
// Detection

#[derive(Clone, Debug)]
pub struct Detection {
    pub label: String,
    pub score: f32,
    /// Normalized [x0, y0, x1, y1].
    pub bbox: [f32; 4],
}

pub struct Detector {
    session: Session,
    tokenizer: Tokenizer,
    pub device: &'static str,
}

const SIZE: usize = 800;
const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

impl Detector {
    pub fn load() -> Result<Self> {
        // fp32: the fp16 export trips DirectML's graph fusion.
        let l = rt()?.load(&runtime::hf_file(DETECT_REPO, "onnx/model.onnx")?)?;
        let tok = Tokenizer::from_file(runtime::hf_file(DETECT_REPO, "tokenizer.json")?).map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
        Ok(Self { session: l.session, tokenizer: tok, device: l.device })
    }

    /// Finds every instance of the given labels. Thresholds as in the
    /// reference post-processing (box 0.3, text 0.25 are good defaults).
    pub fn run(&mut self, img: &RgbImage, labels: &[String], box_threshold: f32, text_threshold: f32) -> Result<Vec<Detection>> {
        let labels: Vec<String> = labels.iter().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect();
        if labels.is_empty() {
            bail!("no labels to detect");
        }
        let prompt = format!("{} .", labels.join(" . "));
        let enc = self.tokenizer.encode(prompt.as_str(), true).map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        if ids.len() > 256 {
            bail!("too many labels for one detection pass");
        }
        // Token -> phrase index; separators and special tokens map to none.
        let mut phrase_of = Vec::with_capacity(ids.len());
        let mut phrase = 0usize;
        for t in enc.get_tokens() {
            match t.as_str() {
                "[CLS]" | "[SEP]" | "[PAD]" => phrase_of.push(None),
                "." => {
                    phrase_of.push(None);
                    phrase += 1;
                }
                _ => phrase_of.push(Some(phrase)),
            }
        }
        let n = ids.len();
        let resized = image::imageops::resize(img, SIZE as u32, SIZE as u32, FilterType::Triangle);
        let pixels = chw(&resized, |c, v| (v as f32 / 255.0 - IMAGENET_MEAN[c]) / IMAGENET_STD[c]);
        let inputs = ort::inputs![
            "pixel_values" => Tensor::from_array(([1usize, 3, SIZE, SIZE], pixels))?,
            "input_ids" => Tensor::from_array(([1usize, n], ids))?,
            "token_type_ids" => Tensor::from_array(([1usize, n], vec![0i64; n]))?,
            "attention_mask" => Tensor::from_array(([1usize, n], vec![1i64; n]))?,
            "pixel_mask" => Tensor::from_array(([1usize, SIZE, SIZE], vec![1i64; SIZE * SIZE]))?,
        ];
        let outputs = self.session.run(inputs)?;
        let (lshape, logits) = outputs["logits"].try_extract_tensor::<f32>()?;
        let (_, boxes) = outputs["pred_boxes"].try_extract_tensor::<f32>()?;
        let queries = lshape[1] as usize;
        let width = lshape[2] as usize;
        let sigmoid = |x: f32| 1.0 / (1.0 + (-x).exp());
        let mut dets = Vec::new();
        for q in 0..queries {
            let row = &logits[q * width..q * width + n.min(width)];
            let mut best_tok = 0.0f32;
            let mut per_phrase = vec![0.0f32; labels.len()];
            for (j, &l) in row.iter().enumerate() {
                let p = sigmoid(l);
                best_tok = best_tok.max(p);
                if let Some(Some(ph)) = phrase_of.get(j)
                    && *ph < labels.len()
                {
                    per_phrase[*ph] = per_phrase[*ph].max(p);
                }
            }
            if best_tok < box_threshold {
                continue;
            }
            let (li, ls) = per_phrase.iter().enumerate().fold((0, 0.0f32), |a, (i, &s)| if s > a.1 { (i, s) } else { a });
            if ls < text_threshold {
                continue;
            }
            let b = &boxes[q * 4..q * 4 + 4];
            let (cx, cy, bw, bh) = (b[0], b[1], b[2], b[3]);
            let bbox = [(cx - bw / 2.0).clamp(0.0, 1.0), (cy - bh / 2.0).clamp(0.0, 1.0), (cx + bw / 2.0).clamp(0.0, 1.0), (cy + bh / 2.0).clamp(0.0, 1.0)];
            dets.push(Detection { label: labels[li].clone(), score: best_tok, bbox });
        }
        Ok(nms(dets, 0.5))
    }
}

/// Per-label non-maximum suppression.
pub fn nms(mut dets: Vec<Detection>, iou_threshold: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut keep: Vec<Detection> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| k.label != d.label || super::geometry::iou(k.bbox, d.bbox) < iou_threshold) {
            keep.push(d);
        }
    }
    keep
}

// ---------------------------------------------------------------------------
// CLIP

pub struct Clip {
    vision: Session,
    text: Session,
    tokenizer: Tokenizer,
    pub device: &'static str,
}

const CLIP_MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
const CLIP_STD: [f32; 3] = [0.26862954, 0.26130258, 0.27577711];

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    v.iter_mut().for_each(|x| *x /= n);
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl Clip {
    pub fn load() -> Result<Self> {
        let r = rt()?;
        let v = r.load(&runtime::hf_file(CLIP_REPO, "onnx/vision_model_fp16.onnx")?)?;
        let t = r.load(&runtime::hf_file(CLIP_REPO, "onnx/text_model_fp16.onnx")?)?;
        let tok = Tokenizer::from_file(runtime::hf_file(CLIP_REPO, "tokenizer.json")?).map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
        Ok(Self { vision: v.session, text: t.session, tokenizer: tok, device: v.device })
    }

    fn preprocess(img: &RgbImage) -> Vec<f32> {
        // Shortest edge to 224 (bicubic), then center crop 224x224.
        let (w, h) = (img.width() as f32, img.height() as f32);
        let s = 224.0 / w.min(h);
        let (nw, nh) = (((w * s).round() as u32).max(224), ((h * s).round() as u32).max(224));
        let r = image::imageops::resize(img, nw, nh, FilterType::CatmullRom);
        let crop = image::imageops::crop_imm(&r, (nw - 224) / 2, (nh - 224) / 2, 224, 224).to_image();
        chw(&crop, |c, v| (v as f32 / 255.0 - CLIP_MEAN[c]) / CLIP_STD[c])
    }

    /// Unit-length embeddings for a batch of images.
    pub fn images(&mut self, imgs: &[RgbImage]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(imgs.len());
        for chunk in imgs.chunks(16) {
            let mut data = Vec::with_capacity(chunk.len() * 3 * 224 * 224);
            for img in chunk {
                data.extend(Self::preprocess(img));
            }
            let input = Tensor::from_array(([chunk.len(), 3usize, 224, 224], data))?;
            let outputs = self.vision.run(ort::inputs!["pixel_values" => input])?;
            let (shape, e) = outputs["image_embeds"].try_extract_tensor::<f32>()?;
            let d = shape[1] as usize;
            for i in 0..chunk.len() {
                let mut v = e[i * d..(i + 1) * d].to_vec();
                normalize(&mut v);
                out.push(v);
            }
        }
        Ok(out)
    }

    pub fn text(&mut self, text: &str) -> Result<Vec<f32>> {
        let enc = self.tokenizer.encode(text, true).map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
        let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        ids.truncate(77);
        let n = ids.len();
        let outputs = self.text.run(ort::inputs!["input_ids" => Tensor::from_array(([1usize, n], ids))?])?;
        let (_, e) = outputs["text_embeds"].try_extract_tensor::<f32>()?;
        let mut v = e.to_vec();
        normalize(&mut v);
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Shared, lazily loaded instances

pub struct Models {
    pub depth: Mutex<Option<Depth>>,
    pub detector: Mutex<Option<Detector>>,
    pub clip: Mutex<Option<Clip>>,
}

pub fn models() -> &'static Models {
    static M: OnceLock<Models> = OnceLock::new();
    M.get_or_init(|| Models { depth: Mutex::new(None), detector: Mutex::new(None), clip: Mutex::new(None) })
}

/// Runs `f` with the model in `slot`, loading it first if needed.
pub fn with<T, R>(slot: &Mutex<Option<T>>, load: impl FnOnce() -> Result<T>, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
    let mut g = slot.lock().unwrap();
    if g.is_none() {
        *g = Some(load().context("loading vision model")?);
    }
    f(g.as_mut().unwrap())
}
