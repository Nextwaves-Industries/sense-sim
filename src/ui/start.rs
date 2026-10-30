//! Start screen: shown at launch (and from File → New scene). The user names
//! and creates an empty scene, opens a saved layout, or starts from one of
//! the sample scenes.

use bevy_egui::egui::{self, Align2, Margin, RichText, Sense, vec2};

use crate::library::Library;
use crate::samples::{self, SAMPLES};
use crate::theme::{self, ACCENT, icon};

pub enum Choice {
    Empty,
    Sample(usize),
    Open,
    Cancel,
}

const CARD_W: f32 = 236.0;
const THUMB_H: f32 = 124.0;

/// Draws the dialog; returns what the user picked, if anything.
pub fn show(ctx: &egui::Context, library: &mut Library, name: &mut String, cancelable: bool) -> Option<Choice> {
    let mut choice = None;
    let width = (CARD_W * 3.0 + 12.0 * 2.0 + 48.0 + 14.0).min(ctx.content_rect().width() - 40.0);
    let modal = egui::Modal::new(egui::Id::new("start_modal"))
        .backdrop_color(egui::Color32::from_black_alpha(190))
        .frame(egui::Frame::new().fill(theme::PANEL).stroke(egui::Stroke::new(1.0, theme::STROKE)).corner_radius(12).inner_margin(Margin::same(24)))
        .show(ctx, |ui| {
            ui.set_width(width - 48.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Create a scene").text_style(egui::TextStyle::Heading));
                if cancelable {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, icon::X, "Back to the current scene (Esc)").clicked() {
                            choice = Some(Choice::Cancel);
                        }
                    });
                }
            });
            ui.label(RichText::new("Name a new scene, or start from a sample and change it.").color(theme::WEAK));
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                let field = egui::TextEdit::singleline(name).hint_text("Scene name").desired_width(260.0).margin(Margin::symmetric(8, 6));
                let r = ui.add(field);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let create = egui::Button::new(RichText::new(theme::label(icon::PLUS, "Create empty scene")).color(theme::BG).strong())
                    .fill(ACCENT)
                    .min_size(vec2(0.0, 30.0));
                if ui.add(create).clicked() || enter {
                    choice = Some(Choice::Empty);
                }
                if ui.add(egui::Button::new(theme::label(icon::FOLDER_OPEN, "Open layout…")).min_size(vec2(0.0, 30.0))).clicked() {
                    choice = Some(Choice::Open);
                }
            });

            ui.add_space(10.0);
            theme::divider(ui);
            ui.label(RichText::new("Sample scenes").strong());
            // Scrolls when the window is too short for all the cards.
            let max_h = (ctx.content_rect().height() - 300.0).max(240.0);
            egui::ScrollArea::vertical().max_height(max_h).min_scrolled_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
                    for (i, s) in SAMPLES.iter().enumerate() {
                        if card(ui, library, i, s) {
                            choice = Some(Choice::Sample(i));
                        }
                    }
                });
            });
        });
    if cancelable && modal.should_close() && choice.is_none() {
        choice = Some(Choice::Cancel);
    }
    choice
}

/// The sample's rendered preview as a texture (decoded once, kept in egui memory).
fn preview(ctx: &egui::Context, index: usize, s: &samples::Sample) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("sample_preview", index));
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return t;
    }
    let tex = image::load_from_memory(s.preview).ok().map(|img| {
        let img = img.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        ctx.load_texture(format!("sample_preview_{index}"), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR)
    });
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

/// "9 anchors · 3 tags" for an RTLS sample, else "120 objects".
fn contents(index: usize) -> String {
    let devices = samples::device_counts(index);
    if devices.is_empty() {
        return format!("{} objects", samples::object_count(index));
    }
    let mut parts: Vec<(&str, usize)> = Vec::new();
    for (kind, n) in devices {
        let label = match kind.as_str() {
            "uwb_anchor" => "anchors",
            "rfid_reader" => "readers",
            "ble_beacon" | "ble_node" => "beacons",
            k if k.ends_with("tag") => "tags",
            _ => "devices",
        };
        match parts.iter_mut().find(|(l, _)| *l == label) {
            Some((_, c)) => *c += n,
            None => parts.push((label, n)),
        }
    }
    parts.iter().map(|(l, n)| format!("{n} {l}")).collect::<Vec<_>>().join(" · ")
}

/// One sample card: rendered preview (or hero thumbnail / icon), title, blurb,
/// what is in it.
fn card(ui: &mut egui::Ui, library: &mut Library, index: usize, s: &samples::Sample) -> bool {
    let (on_disk, total) = samples::available(s, library);
    let shot = preview(ui.ctx(), index, s);
    let hero = if shot.is_none() { library.entries.iter().position(|e| e.folder == s.hero) } else { None };
    if let Some(i) = hero {
        library.want_thumb(i);
    }
    let thumb = hero.and_then(|i| library.entries[i].thumb.clone());

    let (rect, r) = ui.allocate_exact_size(vec2(CARD_W, THUMB_H + 122.0), Sense::click());
    let hovered = r.hovered();
    let p = ui.painter().clone();
    p.rect_filled(rect, 10.0, if hovered { theme::HOVER } else { theme::RAISED });
    if hovered {
        p.rect_stroke(rect, 10.0, egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
    }
    let img = egui::Rect::from_min_size(rect.min + vec2(8.0, 8.0), vec2(CARD_W - 16.0, THUMB_H));
    p.rect_filled(img, 6.0, theme::BG);
    if let Some(t) = &shot {
        // Cover the frame, cropping the picture's longer side.
        let [w, h] = t.size();
        let (fw, fh) = (img.width() / w as f32, img.height() / h as f32);
        let (u, v) = if fw > fh { (1.0, fh / fw) } else { (fw / fh, 1.0) };
        let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), vec2(u, v));
        egui::Image::from_texture(t).uv(uv).corner_radius(6).paint_at(ui, img);
    } else if let Some(t) = thumb {
        // Fit the square thumbnail into the frame, centered.
        let side = img.height();
        let r = egui::Rect::from_center_size(img.center(), vec2(side, side));
        p.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    } else {
        p.text(img.center(), Align2::CENTER_CENTER, s.icon, egui::FontId::proportional(34.0), theme::WEAK);
    }
    // Icon badge on a dark chip so it reads over the picture.
    let badge = egui::Rect::from_min_size(img.left_top() + vec2(6.0, 6.0), vec2(24.0, 24.0));
    p.rect_filled(badge, 6.0, egui::Color32::from_black_alpha(160));
    p.text(badge.center(), Align2::CENTER_CENTER, s.icon, egui::FontId::proportional(14.0), ACCENT);

    let text_left = rect.left() + 12.0;
    let mut y = img.bottom() + 10.0;
    p.text(egui::pos2(text_left, y), Align2::LEFT_TOP, s.title, egui::FontId::proportional(14.5), theme::TEXT);
    y += 22.0;
    // At most three lines; the rest is elided (the footer sits below).
    let mut job = egui::text::LayoutJob::simple(s.blurb.to_string(), egui::FontId::proportional(11.5), theme::WEAK, CARD_W - 24.0);
    job.wrap.max_rows = 3;
    let blurb = p.layout_job(job);
    p.galley(egui::pos2(text_left, y), blurb, theme::WEAK);
    let (txt, color) = if on_disk == total {
        (format!("{} {}", icon::CHECK_CIRCLE, contents(index)), theme::SUCCESS)
    } else {
        (format!("{} {on_disk} of {total} on disk", icon::WARNING_CIRCLE), theme::DANGER)
    };
    p.text(egui::pos2(text_left, rect.bottom() - 10.0), Align2::LEFT_BOTTOM, txt, egui::FontId::proportional(11.5), color);

    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    let r = if on_disk < total { r.on_hover_text("Assets that are not on disk are downloaded when the scene loads.") } else { r };
    r.clicked()
}
