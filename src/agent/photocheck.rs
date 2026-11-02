//! Verifying a photo rebuild against the photo. The builder can't be trusted to
//! grade its own guesses, so two things it doesn't control judge them: a measured
//! photo-against-render comparison, and an independent reviewer (a fresh LLM call
//! that sees only the images, never the builder's reasoning).

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use image::RgbImage;
use serde_json::{Value, json};

use super::llm::{Block, Message, Provider};

const COLS: u32 = 6;
const ROWS: u32 = 4;
/// Width of one pane of the comparison image.
const PANE_W: u32 = 640;
const PANE_MAX_H: u32 = 480;

/// Review rounds per request: each costs an LLM call with an image.
pub const MAX_REVIEWS: usize = 3;
/// Reviewer score (out of 10) a rebuild needs, with no major difference left.
const PASS_SCORE: f32 = 7.0;

pub const REVIEWER_SYSTEM: &str = r#"You are an independent reviewer of 3D scene reconstructions. You did not build this one and nothing you say needs to be polite to its builder.

The image has four panes. Top left: the real photo. Top right: the 3D rebuild, rendered from the photo's camera. Bottom left: both blended, which shows misplaced edges as doubled outlines. Bottom right: the photo with red over the regions that differ most in color.

A second image, if there is one, shows the same rebuild from a viewpoint a few meters from the photo's. Pictures pasted on flat panels, thin cards and empty boxes show up there. A rebuild can only score above 6 if it also holds together as a space from there.

Judge only what you can see. Compare, in order:
1. Room: does the ceiling match (flat, tiled, or exposed trusses and ducts; height)? Floor and walls? Does the room feel the same size and depth, or does it end too soon?
2. Objects: what is in the photo but missing from the rebuild? What is in the rebuild but not the photo? Ignore people. Are positions and proportions right?
3. Materials and light: are colors and surface types right? Do things look lit, or flat and gray?
4. Artifacts: photo crops showing background around an object, people printed on objects, bare box sides, things floating.

Score the likeness out of 10: 0-3 unrecognizable, 4-6 same kind of place but clearly a rough model, 7-8 recognizably this store, 9-10 hard to tell apart. Most rebuilds deserve 3-6. Do not reward effort.

Reply with JSON only, no other text:
{"score": <number>, "differences": [{"severity": "major" or "minor", "what": "<what is wrong>", "where": "<where in the photo>", "fix": "<the change that would fix it>"}]}
List at most 8 differences, most important first."#;

/// Horizontal field of view that shows the whole photo in the viewport. The
/// photo is cropped back out of the middle afterwards; matching the horizontal
/// angle alone cuts off the top and bottom of a photo narrower than the viewport.
pub fn fit_hfov(photo_hfov_deg: f32, photo_aspect: f32, view_aspect: f32) -> f32 {
    if view_aspect <= photo_aspect {
        return photo_hfov_deg;
    }
    let half = (photo_hfov_deg.to_radians() * 0.5).tan() * view_aspect / photo_aspect;
    2.0 * half.atan().to_degrees()
}

fn crop_to_aspect(img: &RgbImage, aspect: f32) -> RgbImage {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (cw, ch) = if w / h > aspect { (h * aspect, h) } else { (w, w / aspect) };
    let (cw, ch) = ((cw as u32).max(1), (ch as u32).max(1));
    image::imageops::crop_imm(img, (img.width() - cw) / 2, (img.height() - ch) / 2, cw, ch).to_image()
}

fn resize(img: &RgbImage, w: u32, h: u32) -> RgbImage {
    image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle)
}

fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

fn region_name(col: u32, row: u32) -> String {
    let v = ["top", "upper middle", "lower middle", "bottom"][row.min(ROWS - 1) as usize];
    let h = ["far left", "left", "center left", "center right", "right", "far right"][col.min(COLS - 1) as usize];
    format!("{v}, {h}")
}

fn luma(p: &image::Rgb<u8>) -> f32 {
    0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
}

/// Mean edge strength of a cell: how much detail it holds.
fn cell_detail(img: &RgbImage, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
    let (mut sum, mut n) = (0.0, 0u32);
    for y in y0..y1.saturating_sub(1) {
        for x in x0..x1.saturating_sub(1) {
            let c = luma(img.get_pixel(x, y));
            sum += (c - luma(img.get_pixel(x + 1, y))).abs() + (c - luma(img.get_pixel(x, y + 1))).abs();
            n += 1;
        }
    }
    sum / n.max(1) as f32
}

/// Puts photo and render side by side with a blend and a difference map under
/// them, and measures how alike they are per region.
pub fn compare(photo: &RgbImage, render: &RgbImage) -> (RgbImage, Value) {
    let aspect = photo.width() as f32 / photo.height().max(1) as f32;
    let (w, h) = {
        let h = ((PANE_W as f32 / aspect) as u32).min(PANE_MAX_H).max(COLS);
        (((h as f32 * aspect) as u32).clamp(COLS, PANE_W), h)
    };
    let p = resize(photo, w, h);
    let r = resize(&crop_to_aspect(render, aspect), w, h);

    let mut cells = Vec::new();
    for row in 0..ROWS {
        for col in 0..COLS {
            let (x0, x1) = (col * w / COLS, (col + 1) * w / COLS);
            let (y0, y1) = (row * h / ROWS, (row + 1) * h / ROWS);
            let (mut diff, mut n) = (0.0f32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (a, b) = (p.get_pixel(x, y), r.get_pixel(x, y));
                    diff += (0..3).map(|c| (a[c] as f32 - b[c] as f32).abs()).sum::<f32>() / 3.0;
                    n += 1;
                }
            }
            let diff = diff / n.max(1) as f32;
            let (dp, dr) = (cell_detail(&p, x0, y0, x1, y1), cell_detail(&r, x0, y0, x1, y1));
            cells.push((col, row, diff, dp, dr));
        }
    }

    let mut out = RgbImage::new(w * 2, h * 2);
    let heat_of = |x: u32, y: u32| {
        let c = cells[((y * ROWS / h).min(ROWS - 1) * COLS + (x * COLS / w).min(COLS - 1)) as usize];
        (c.2 / 80.0).clamp(0.0, 1.0)
    };
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (p.get_pixel(x, y).0, r.get_pixel(x, y).0);
            out.put_pixel(x, y, image::Rgb(a));
            out.put_pixel(x + w, y, image::Rgb(b));
            out.put_pixel(x, y + h, image::Rgb([0, 1, 2].map(|c| ((a[c] as u16 + b[c] as u16) / 2) as u8)));
            let t = heat_of(x, y);
            let g = luma(p.get_pixel(x, y));
            out.put_pixel(x + w, y + h, image::Rgb([g + (255.0 - g) * t, g * (1.0 - t), g * (1.0 - t)].map(|v| v as u8)));
        }
    }

    let color_match = 1.0 - cells.iter().map(|c| c.2).sum::<f32>() / cells.len() as f32 / 128.0;
    let detail_match = cells.iter().map(|c| if c.3.max(c.4) < 0.5 { 1.0 } else { c.3.min(c.4) / c.3.max(c.4) }).sum::<f32>() / cells.len() as f32;
    let mut worst = cells.clone();
    worst.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let regions: Vec<Value> = worst
        .iter()
        .take(6)
        .map(|c| {
            let detail = if c.4 < c.3 * 0.5 { "the rebuild lacks detail here" } else if c.3 < c.4 * 0.5 { "the rebuild has detail the photo lacks" } else { "detail is similar" };
            json!({ "region": region_name(c.0, c.1), "color_difference": r2(c.2), "note": detail })
        })
        .collect();
    let metrics = json!({
        "color_match": r2(color_match.clamp(0.0, 1.0)),
        "detail_match": r2(detail_match),
        "worst_regions": regions,
        "image": "top left: photo; top right: rebuild from the photo camera; bottom left: blend (doubled edges are misplaced things); bottom right: difference map (red differs most)",
    });
    (out, metrics)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Difference {
    pub major: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Critique {
    pub score: f32,
    pub differences: Vec<Difference>,
}

impl Critique {
    pub fn passes(&self) -> bool {
        self.score >= PASS_SCORE && !self.differences.iter().any(|d| d.major)
    }

    fn list(&self) -> String {
        self.differences.iter().map(|d| format!("- [{}] {}", if d.major { "major" } else { "minor" }, d.text)).collect::<Vec<_>>().join("\n")
    }

    /// The differences, for telling the user what is still wrong.
    pub fn outstanding(&self) -> String {
        self.list()
    }

    /// What the builder is told when sent back.
    pub fn report(&self) -> String {
        format!(
            "[Automatic review] An independent reviewer compared your rebuild with the photo and scored it {:.1}/10 (it needs {PASS_SCORE:.0} with no major difference).\n{}\n\
             Fix the major differences first, revising your guesses where they were wrong (room size, sizes, positions, materials), and use create_asset, add_structure and set_object_material for what is missing. \
             Check with compare_to_photo. If something can't be fixed with these tools, say so plainly in your summary rather than claiming it matches.",
            self.score,
            self.list()
        )
    }
}

/// Reads a reviewer reply, tolerating text or a code fence around the JSON.
pub fn parse_critique(reply: &str) -> Option<Critique> {
    let (a, b) = (reply.find('{')?, reply.rfind('}')?);
    let v: Value = serde_json::from_str(reply.get(a..=b)?).ok()?;
    let score = v["score"].as_f64()? as f32;
    if !(0.0..=10.0).contains(&score) {
        return None;
    }
    let differences = v["differences"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let what = d["what"].as_str()?;
            let mut text = what.to_string();
            if let Some(w) = d["where"].as_str().filter(|s| !s.is_empty()) {
                text.push_str(&format!(" ({w})"));
            }
            if let Some(f) = d["fix"].as_str().filter(|s| !s.is_empty()) {
                text.push_str(&format!(". Fix: {f}"));
            }
            Some(Difference { major: d["severity"].as_str() == Some("major"), text })
        })
        .collect();
    Some(Critique { score, differences })
}

/// Asks a fresh LLM call, with no history and no tools, to judge a comparison image.
pub fn review(provider: &dyn Provider, comparison: &Arc<Vec<u8>>, mime: &str, side: Option<&(Arc<Vec<u8>>, String)>) -> Result<Critique> {
    let mut content = vec![
        Block::Text("Review this rebuild. Reply with the JSON only.".into()),
        Block::Image { data: comparison.clone(), mime: mime.to_string() },
    ];
    if let Some((data, m)) = side {
        content.push(Block::Text("The same rebuild from a nearby viewpoint:".into()));
        content.push(Block::Image { data: data.clone(), mime: m.clone() });
    }
    let msg = Message::user(content);
    let reply = provider.complete(REVIEWER_SYSTEM, &[msg], &[]).context("reviewer call")?;
    let text = reply.text();
    parse_critique(&text).ok_or_else(|| anyhow!("the reviewer's reply was not valid JSON: {}", text.chars().take(200).collect::<String>()))
}

/// Tracks whether the rebuild of the latest photo still needs reviewing.
/// What the depth check flagged, for the builder to look at again.
pub fn depth_note(summary: &str) -> Option<String> {
    let v: Value = serde_json::from_str(summary).ok()?;
    let flags: Vec<&str> = v["disagreements"].as_array()?.iter().filter_map(|f| f.as_str()).collect();
    (!flags.is_empty()).then(|| {
        format!(
            "[Depth check] The depth model disagrees about these. It built nothing and is often wrong, so don't copy its numbers; look at the photo again and keep your layout if you still believe it:\n- {}",
            flags.join("\n- ")
        )
    })
}

/// A viewpoint beside the photo camera, for catching what only looks right from it.
pub fn side_view(comparison_summary: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(comparison_summary.lines().take_while(|l| *l != "}").chain(["}"]).collect::<Vec<_>>().join("\n").as_str()).ok()?;
    let eye: Vec<f64> = v["camera"]["eye"].as_array()?.iter().filter_map(|x| x.as_f64()).collect();
    let target = v["camera"]["target"].clone();
    (eye.len() == 3).then(|| json!({ "eye": [eye[0] + 3.0, eye[1] + 0.5, eye[2] - 2.0], "target": target }))
}

#[derive(Default)]
pub struct PhotoCheck {
    image: Option<String>,
    dirty: bool,
    pub reviews: usize,
}

impl PhotoCheck {
    pub fn perceived(&mut self, image_id: &str) {
        self.image = Some(image_id.to_string());
    }

    pub fn scene_changed(&mut self) {
        self.dirty = true;
    }

    /// The photo to review now, if the scene changed since the last review and
    /// rounds remain. Starts that round.
    pub fn due(&mut self) -> Option<String> {
        let image = self.image.clone().filter(|_| self.dirty && self.reviews < MAX_REVIEWS)?;
        self.dirty = false;
        self.reviews += 1;
        Some(image)
    }

    pub fn can_send_back(&self) -> bool {
        self.reviews < MAX_REVIEWS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{Role, ToolDef};
    use std::sync::Mutex;

    #[test]
    fn a_wide_viewport_widens_the_view_to_keep_the_photo_whole() {
        // Square photo, 65 degrees: its vertical angle is also 65.
        let h = fit_hfov(65.0, 1.0, 2.0);
        let v = 2.0 * ((h.to_radians() * 0.5).tan() / 2.0).atan().to_degrees();
        assert!((v - 65.0).abs() < 1e-3, "vertical {v}");
        assert_eq!(fit_hfov(65.0, 2.0, 1.5), 65.0, "a narrower viewport already shows the whole width");
    }

    #[test]
    fn identical_images_match_and_different_ones_do_not() {
        let a = RgbImage::from_fn(320, 240, |x, y| image::Rgb([if (x / 4 + y / 4) % 2 == 0 { 230 } else { 30 }, (y % 255) as u8, 90]));
        let (img, m) = compare(&a, &a);
        assert_eq!(m["color_match"], json!(1.0));
        assert_eq!(m["detail_match"], json!(1.0));
        assert_eq!(img.height(), img.width() * 240 / 320);
        let flat = RgbImage::from_pixel(320, 240, image::Rgb([0, 0, 0]));
        let (_, m) = compare(&a, &flat);
        assert!(m["color_match"].as_f64().unwrap() < 0.8, "{m}");
        assert!(m["detail_match"].as_f64().unwrap() < 0.1, "{m}");
        assert_eq!(m["worst_regions"][0]["note"], json!("the rebuild lacks detail here"));
    }

    #[test]
    fn the_worst_region_is_named_where_the_difference_is() {
        let a = RgbImage::from_pixel(600, 400, image::Rgb([120, 120, 120]));
        let mut b = a.clone();
        for y in 0..100 {
            for x in 500..600 {
                b.put_pixel(x, y, image::Rgb([255, 0, 0]));
            }
        }
        let (_, m) = compare(&a, &b);
        assert_eq!(m["worst_regions"][0]["region"], json!("top, far right"));
    }

    #[test]
    fn a_wider_render_is_compared_by_its_middle() {
        let photo = RgbImage::from_pixel(100, 100, image::Rgb([200, 0, 0]));
        // Wide render: red in the middle square, black at the sides.
        let render = RgbImage::from_fn(200, 100, |x, _| if (50..150).contains(&x) { image::Rgb([200, 0, 0]) } else { image::Rgb([0, 0, 0]) });
        let (_, m) = compare(&photo, &render);
        assert!(m["color_match"].as_f64().unwrap() > 0.97, "{m}");
    }

    #[test]
    fn critiques_parse_with_surrounding_text_and_reject_nonsense() {
        let c = parse_critique("Here you go:\n```json\n{\"score\": 5.5, \"differences\": [{\"severity\": \"major\", \"what\": \"no trusses\", \"where\": \"top\", \"fix\": \"add beams\"}]}\n```").unwrap();
        assert_eq!(c.score, 5.5);
        assert_eq!(c.differences, vec![Difference { major: true, text: "no trusses (top). Fix: add beams".into() }]);
        assert!(parse_critique("looks great!").is_none());
        assert!(parse_critique("{\"differences\": []}").is_none(), "a reply with no score must not pass");
        assert!(parse_critique("{\"score\": 42}").is_none());
    }

    #[test]
    fn a_high_score_with_a_major_difference_still_fails() {
        let major = Critique { score: 9.0, differences: vec![Difference { major: true, text: "x".into() }] };
        assert!(!major.passes());
        assert!(!Critique { score: 6.9, differences: vec![] }.passes());
        assert!(Critique { score: 7.0, differences: vec![Difference { major: false, text: "x".into() }] }.passes());
    }

    #[test]
    fn reviews_run_only_after_changes_and_stop_at_the_limit() {
        let mut p = PhotoCheck::default();
        p.scene_changed();
        assert_eq!(p.due(), None, "no photo, nothing to review against");
        p.perceived("img1");
        assert_eq!(p.due().as_deref(), Some("img1"));
        assert_eq!(p.due(), None, "nothing changed since");
        for _ in 1..MAX_REVIEWS {
            p.scene_changed();
            assert!(p.due().is_some());
        }
        p.scene_changed();
        assert_eq!(p.due(), None, "out of rounds");
        assert!(!p.can_send_back());
    }

    /// Records what it was asked, and answers with a fixed reply.
    struct Fake {
        reply: String,
        seen: Mutex<Vec<(String, usize, usize, bool)>>,
    }

    impl Provider for Fake {
        fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
            let has_image = messages.iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::Image { .. }));
            self.seen.lock().unwrap().push((system.to_string(), messages.len(), tools.len(), has_image));
            Ok(Message { role: Role::Assistant, content: vec![Block::Text(self.reply.clone())], raw: None, usage: None })
        }
    }

    #[test]
    fn the_reviewer_sees_only_the_image_with_no_history_or_tools() {
        let fake = Fake { reply: "{\"score\": 4, \"differences\": []}".into(), seen: Mutex::default() };
        let c = review(&fake, &Arc::new(vec![1, 2, 3]), "image/png", None).unwrap();
        assert_eq!(c.score, 4.0);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, REVIEWER_SYSTEM);
        assert_eq!((seen[0].1, seen[0].2, seen[0].3), (1, 0, true));
    }

    #[test]
    fn depth_disagreements_become_a_note_and_agreement_says_nothing() {
        let some = json!({ "disagreements": ["shelf: 4.0 m in the layout, depth suggests about 9.0 m"] }).to_string();
        assert!(depth_note(&some).unwrap().contains("shelf: 4.0"));
        assert!(depth_note(&json!({ "disagreements": [] }).to_string()).is_none());
        assert!(depth_note("garbage").is_none());
    }

    #[test]
    fn the_side_view_stands_beside_the_photo_camera() {
        let text = serde_json::to_string_pretty(&json!({ "camera": { "eye": [0.0, 1.6, 0.0], "target": [0.0, 1.0, -10.0] }, "color_match": 0.5 })).unwrap();
        let v = side_view(&format!("{text}\nimage")).unwrap();
        assert_eq!(v["eye"], json!([3.0, 2.1, -2.0]));
        assert_eq!(v["target"], json!([0.0, 1.0, -10.0]));
        assert!(side_view("not json").is_none());
    }

    #[test]
    fn an_unreadable_review_is_an_error_not_a_pass() {
        let fake = Fake { reply: "Looks perfect to me.".into(), seen: Mutex::default() };
        assert!(review(&fake, &Arc::new(vec![1]), "image/png", None).is_err());
    }
}
