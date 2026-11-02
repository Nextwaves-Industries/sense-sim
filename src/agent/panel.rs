//! The assistant's chat panel and its settings page (shown in the app's
//! Settings window under "AI assistant").

use bevy_egui::egui::{self, Align, Color32, Layout, Margin, RichText, vec2};

use super::catalog::Catalog;
use super::config::{AgentConfig, ProviderKind};
use super::tools::ToolBridge;
use super::{Agent, ChatItem, images};
use crate::library::Library;
use crate::theme::{self, icon};

const CANDIDATE: f32 = 84.0;

const SUGGESTIONS: [&str; 3] = [
    "Place a row of 4 pallet racks 3 m apart",
    "Build a small loading dock with pallets and boxes",
    "Add safety barriers around the scene",
];

fn texture(ctx: &egui::Context, cache: &mut std::collections::HashMap<String, egui::TextureHandle>, id: &str) -> Option<egui::TextureHandle> {
    if let Some(t) = cache.get(id) {
        return Some(t.clone());
    }
    let (data, _) = images::get(id)?;
    let img = image::load_from_memory(&data).ok()?.thumbnail(640, 640).to_rgba8();
    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    let t = ctx.load_texture(format!("chat:{id}"), ci, egui::TextureOptions::LINEAR);
    cache.insert(id.to_string(), t.clone());
    Some(t)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}

/// Draws the chat panel. Returns true when the user asks for its settings.
pub fn show(root: &mut egui::Ui, ctx: &egui::Context, agent: &mut Agent, library: &mut Library, bridge: &ToolBridge) -> bool {
    if !agent.open {
        return false;
    }
    let mut open_settings = false;
    egui::Panel::right("assistant")
        .default_size(320.0)
        .size_range(280.0..=520.0)
        .resizable(true)
        .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::same(10)))
        .show(root, |ui| {
            theme::panel_header(ui, "Assistant", |ui| {
                if theme::icon_button(ui, icon::X, "Hide the assistant").clicked() {
                    agent.open = false;
                }
                if theme::icon_button(ui, icon::GEAR_SIX, "Assistant settings").clicked() {
                    open_settings = true;
                }
                if theme::icon_button(ui, icon::NOTE_PENCIL, "New chat").clicked() {
                    agent.new_chat();
                }
            });
            egui::Panel::bottom("assistant_composer")
                .frame(egui::Frame::new().inner_margin(Margin { left: 0, right: 0, top: 8, bottom: 0 }))
                .show_inside(ui, |ui| composer(ui, ctx, agent, bridge, &mut open_settings));
            if !agent.plan.is_empty() {
                plan(ui, agent);
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                transcript(ui, ctx, agent, library);
            });
        });
    open_settings
}

fn plan(ui: &mut egui::Ui, agent: &Agent) {
    let done = agent.plan.iter().filter(|(_, s)| s == "done").count();
    let total = agent.plan.len();
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            theme::caption(ui, "Plan");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{done} / {total}")).small().color(theme::WEAK));
            });
        });
        // Thin progress bar under the caption.
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, 2.0, theme::BG);
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height())), 2.0, theme::SUCCESS);
        ui.add_space(2.0);
        for (step, status) in &agent.plan {
            let (ic, color) = match status.as_str() {
                "done" => (icon::CHECK, theme::SUCCESS),
                "in_progress" => (icon::CIRCLE_NOTCH, theme::WARNING),
                "failed" => (icon::WARNING_CIRCLE, theme::DANGER),
                _ => ("·", theme::WEAK),
            };
            ui.horizontal(|ui| {
                // Fixed-width marker column so every step lines up.
                ui.add_sized(vec2(16.0, 18.0), egui::Label::new(RichText::new(ic).color(color)));
                ui.add(egui::Label::new(RichText::new(step).color(if status == "done" { theme::WEAK } else { theme::TEXT })).wrap());
            });
        }
    });
    ui.add_space(6.0);
}

fn transcript(ui: &mut egui::Ui, ctx: &egui::Context, agent: &mut Agent, library: &mut Library) {
    if agent.transcript.is_empty() {
        empty(ui, agent);
        return;
    }
    let running = agent.running;
    let Agent { transcript, questions, textures, .. } = agent;
    let width = ui.available_width();
    let mut i = 0;
    while i < transcript.len() {
        // A reply starts after each user message: space between turns and a
        // small "Assistant" label above the reply.
        if i > 0 && matches!(transcript[i - 1], ChatItem::User { .. }) && !matches!(transcript[i], ChatItem::User { .. }) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(RichText::new(icon::SPARKLE).size(12.0).color(theme::ACCENT));
                ui.label(RichText::new("Assistant").small().color(theme::WEAK));
            });
            ui.add_space(2.0);
        }
        // Consecutive tool calls render as one compact group.
        if matches!(transcript[i], ChatItem::Tool { .. }) {
            let start = i;
            while i < transcript.len() && matches!(transcript[i], ChatItem::Tool { .. }) {
                i += 1;
            }
            let last = i == transcript.len();
            tool_group(ui, &transcript[start..i], start, running && last);
            ui.add_space(10.0);
            continue;
        }
        let item = &transcript[i];
        i += 1;
        match item {
            ChatItem::User { text, images: imgs } => {
                if i > 1 {
                    ui.add_space(8.0);
                }
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    // Speech-bubble corner: square-ish towards the sender side.
                    let r = egui::CornerRadius { nw: 12, ne: 12, sw: 12, se: 4 };
                    egui::Frame::new().fill(theme::ACCENT.gamma_multiply(0.16)).stroke(egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.28))).corner_radius(r).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.set_max_width(width * 0.85);
                        ui.vertical(|ui| {
                            if !imgs.is_empty() {
                                ui.horizontal_wrapped(|ui| {
                                    for id in imgs {
                                        if let Some(t) = texture(ctx, textures, id) {
                                            ui.add(egui::Image::new(&t).max_width(160.0).max_height(120.0).corner_radius(6)).on_hover_text(id);
                                        }
                                    }
                                });
                            }
                            ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
                        });
                    });
                });
            }
            ChatItem::Assistant(t) => {
                super::markdown::show(ui, t);
            }
            ChatItem::Tool { .. } => {}
            ChatItem::Image(id) => {
                if let Some(t) = texture(ctx, textures, id) {
                    ui.add(egui::Image::new(&t).max_width(width).max_height(240.0).corner_radius(8)).on_hover_text(id);
                }
            }
            ChatItem::Question(i) => {
                if let Some(q) = questions.get_mut(*i) {
                    question(ui, q, library);
                }
            }
            ChatItem::Info(t) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.label(RichText::new(icon::INFO).small().color(theme::WEAK));
                    ui.add(egui::Label::new(RichText::new(t).small().color(theme::WEAK)).wrap());
                });
            }
            ChatItem::Error(t) => {
                egui::Frame::new()
                    .fill(theme::DANGER.gamma_multiply(0.12))
                    .stroke(egui::Stroke::new(1.0, theme::DANGER.gamma_multiply(0.35)))
                    .corner_radius(8)
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(icon::WARNING_CIRCLE).color(theme::DANGER));
                            ui.add(egui::Label::new(RichText::new(t).color(theme::TEXT)).wrap());
                        });
                    });
            }
        }
        ui.add_space(10.0);
    }
}

/// First-run view: what the assistant does and a few prompts to start from.
fn empty(ui: &mut egui::Ui, agent: &mut Agent) {
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(52.0, 52.0), egui::Sense::hover());
        ui.painter().circle_filled(r.center(), 26.0, theme::ACCENT.gamma_multiply(0.14));
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, icon::SPARKLE, egui::FontId::proportional(26.0), theme::ACCENT);
        ui.add_space(10.0);
        ui.label(RichText::new("Build scenes with AI").size(16.0).strong().color(theme::TEXT));
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new("Describe a layout, or attach a photo of a factory or warehouse: it is rebuilt from SimReady assets, measured and checked against the photo.")
                    .color(theme::WEAK),
            )
            .wrap(),
        );
    });
    ui.add_space(18.0);
    theme::caption(ui, "Try");
    ui.add_space(2.0);
    for s in SUGGESTIONS {
        let w = ui.available_width();
        let galley = ui.painter().layout(s.to_string(), egui::FontId::proportional(13.5), theme::TEXT, w - 44.0);
        let (rect, r) = ui.allocate_exact_size(vec2(w, galley.size().y + 18.0), egui::Sense::click());
        let p = ui.painter();
        p.rect_filled(rect, 8.0, if r.hovered() { theme::HOVER } else { theme::RAISED });
        p.text(rect.left_center() + vec2(14.0, 0.0), egui::Align2::CENTER_CENTER, icon::ARROW_UP, egui::FontId::proportional(13.0), theme::ACCENT);
        p.galley(egui::pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT);
        if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            agent.input = s.to_string();
        }
        ui.add_space(6.0);
    }
}

/// A block of consecutive tool calls: a "Used N tools" header (open while the
/// turn is running, collapsed afterwards) over compact one-line rows; each row
/// expands to its raw input/output on click.
fn tool_group(ui: &mut egui::Ui, items: &[ChatItem], first: usize, live: bool) {
    let id = egui::Id::new(("tool_group", first));
    let user_open: Option<bool> = ui.data(|d| d.get_temp(id));
    let open = user_open.unwrap_or(live);
    let failed = items.iter().filter(|c| matches!(c, ChatItem::Tool { is_error: true, output, .. } if !output.is_empty())).count();
    egui::Frame::new().fill(theme::RAISED).stroke(egui::Stroke::new(1.0, theme::STROKE)).corner_radius(8).inner_margin(Margin::same(6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        let header = format!("Used {} tool{}", items.len(), if items.len() == 1 { "" } else { "s" });
        let r = row(ui, |ui| {
            ui.label(RichText::new(if open { icon::CARET_DOWN } else { icon::CARET_RIGHT }).small().color(theme::WEAK));
            if live {
                ui.add(egui::Spinner::new().size(11.0).color(theme::ACCENT));
            } else {
                ui.label(RichText::new(icon::WRENCH).small().color(theme::WEAK));
            }
            ui.label(RichText::new(header).small().color(theme::TEXT));
            if failed > 0 {
                ui.label(RichText::new(format!("· {failed} failed")).small().color(theme::DANGER));
            }
        });
        if r.clicked() {
            ui.data_mut(|d| d.insert_temp(id, !open));
        }
        if !open {
            return;
        }
        ui.add_space(2.0);
        for (k, item) in items.iter().enumerate() {
            let ChatItem::Tool { name, input, output, is_error } = item else { continue };
            let rid = egui::Id::new(("tool_row", first + k));
            let expanded: bool = ui.data(|d| d.get_temp(rid)).unwrap_or(false);
            let detail = summarize(name, input, output, *is_error);
            let r = row(ui, |ui| {
                ui.add_space(14.0);
                if output.is_empty() {
                    ui.add(egui::Spinner::new().size(11.0).color(theme::WEAK));
                } else if *is_error {
                    ui.label(RichText::new(icon::WARNING_CIRCLE).small().color(theme::DANGER));
                } else {
                    ui.label(RichText::new(icon::CHECK).small().color(theme::SUCCESS));
                }
                ui.label(RichText::new(name.replace('_', " ")).small().color(theme::TEXT));
                if !detail.is_empty() {
                    ui.add(egui::Label::new(RichText::new(format!("· {}", clip(&detail, 60))).small().color(theme::WEAK)).truncate());
                }
            });
            if r.clicked() {
                ui.data_mut(|d| d.insert_temp(rid, !expanded));
            }
            if expanded {
                egui::Frame::new().fill(theme::BG).corner_radius(6).inner_margin(Margin::same(6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().id_salt(("tool_detail", first + k)).max_height(180.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(clip(input, 1500)).monospace().small().color(theme::WEAK)).wrap());
                        if !output.is_empty() {
                            theme::divider(ui);
                            ui.add(egui::Label::new(RichText::new(clip(output, 3000)).monospace().small().color(theme::WEAK)).wrap());
                        }
                    });
                });
            }
        }
    });
}

/// A full-width, 22 px clickable row with a hover highlight.
fn row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, theme::HOVER);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    child.spacing_mut().item_spacing.x = 6.0;
    add(&mut child);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Human summary of a tool call; never raw JSON. Empty when there's nothing
/// short and meaningful to say.
fn summarize(name: &str, input: &str, output: &str, is_error: bool) -> String {
    let inp: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
    let out: Option<serde_json::Value> = serde_json::from_str(output).ok();
    let first_line = || {
        let l = output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        if l.starts_with('{') || l.starts_with('[') || l.starts_with('"') { String::new() } else { l.to_string() }
    };
    if output.is_empty() {
        return match name {
            "search_assets" => inp["query"].as_str().unwrap_or("").to_string(),
            "place_objects" => format!("{} objects", inp["objects"].as_array().map(|a| a.len()).unwrap_or(0)),
            _ => String::new(),
        };
    }
    if is_error && out.is_none() {
        return first_line();
    }
    let n = |v: &serde_json::Value| v.as_array().map(|a| a.len()).unwrap_or(0);
    match (name, &out) {
        ("search_assets", Some(o)) => format!("{} · {} results", inp["query"].as_str().unwrap_or(""), n(&o["results"])),
        ("search_assets", None) => format!("{} · no results", inp["query"].as_str().unwrap_or("")),
        ("place_objects", Some(o)) => {
            let errs = n(&o["errors"]);
            if errs > 0 { format!("{} placed, {errs} failed", n(&o["placed"])) } else { format!("{} placed", n(&o["placed"])) }
        }
        ("verify_scene", Some(o)) => match o["issue_count"].as_u64().unwrap_or(0) {
            0 => "no issues".into(),
            k => format!("{k} issue{}", if k == 1 { "" } else { "s" }),
        },
        ("get_scene", Some(o)) => format!("{} objects", o["count"].as_u64().unwrap_or(0)),
        ("delete_objects", Some(o)) => format!("{} deleted", n(&o["deleted"])),
        ("download_assets", Some(o)) => format!("{} downloaded", n(&o["downloaded"])),
        ("update_object", Some(o)) => o["name"].as_str().unwrap_or("").to_string(),
        ("get_asset_info", Some(o)) => {
            let s = &o["size_m"];
            let folder = o["asset"].as_str().and_then(|a| a.split('/').rev().nth(2)).unwrap_or("");
            match (s[0].as_f64(), s[1].as_f64(), s[2].as_f64()) {
                (Some(x), Some(y), Some(z)) => format!("{folder} · {x:.2} × {y:.2} × {z:.2} m"),
                _ => folder.to_string(),
            }
        }
        ("ask_user", _) => inp["question"].as_str().map(|q| clip(q, 50)).unwrap_or_default(),
        ("update_plan", _) => format!("{} steps", n(&inp["steps"])),
        ("screenshot", _) => "captured".into(),
        ("perceive_photo", Some(o)) => format!("{} detections", o["detections"].as_u64().unwrap_or(0)),
        ("optimize_layout", Some(o)) => format!("IoU {:.2} → {:.2}", o["mean_iou_before"].as_f64().unwrap_or(0.0), o["mean_iou_after"].as_f64().unwrap_or(0.0)),
        ("rtls_auto_deploy", Some(o)) => {
            let cm = |v: &serde_json::Value| v.as_f64().map(|m| format!("{:.0} cm", m * 100.0)).unwrap_or_default();
            let met = if o["targets_met"].as_bool().unwrap_or(false) { "target met" } else { "target NOT met" };
            format!("{} · CEP95 {} · {met}", o["placed"].as_str().unwrap_or(""), cm(&o["test"]["cep95_m"]))
        }
        ("rtls_run", Some(o)) => {
            let cm = |k: &str| o[k].as_f64().map(|m| format!("{:.0} cm", m * 100.0)).unwrap_or_else(|| "-".into());
            format!("RMSE {} · CEP95 {}", cm("rmse_m"), cm("cep95_m"))
        }
        ("rtls_coverage", Some(o)) => match o["covered_fraction"].as_f64() {
            Some(c) => format!("{:.0}% covered · p95 {:.0} cm", c * 100.0, o["p95_predicted_error_m"].as_f64().unwrap_or(0.0) * 100.0),
            None => o["summary"].as_str().unwrap_or("").to_string(),
        },
        ("rtls_optimize_anchors", Some(o)) => o["note"].as_str().unwrap_or("").to_string(),
        ("rtls_list_devices", Some(o)) => format!("{} devices, {} zones", n(&o["devices"]), n(&o["zones"])),
        ("rtls_openems", Some(o)) => format!("OpenEMS {}", o["phase"].as_str().unwrap_or("")),
        (n_, Some(o)) if n_.starts_with("rtls_") => ["id", "summary", "note", "state"]
            .iter()
            .find_map(|k| o[*k].as_str().map(str::to_string))
            .unwrap_or_default(),
        _ if out.is_none() => first_line(),
        _ => String::new(),
    }
}

fn question(ui: &mut egui::Ui, q: &mut super::Question, library: &mut Library) {
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::ONLINE))
        .corner_radius(10)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(icon::CHAT_CIRCLE_DOTS).color(theme::ONLINE));
                ui.add(egui::Label::new(RichText::new(&q.text).color(theme::TEXT)).wrap());
            });
            if let Some(a) = &q.answer {
                ui.label(RichText::new(format!("{}  {a}", icon::CHECK)).color(theme::WEAK));
                return;
            }
            let mut answer = None;
            if !q.candidates.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for &idx in &q.candidates {
                        library.want_thumb(idx);
                        let Some(e) = library.entries.get(idx) else { continue };
                        ui.vertical(|ui| {
                            ui.set_width(CANDIDATE);
                            let size = vec2(CANDIDATE, CANDIDATE);
                            let btn = match &e.thumb {
                                Some(t) => egui::Button::image(egui::Image::new(t).fit_to_exact_size(size)),
                                None => egui::Button::new("no preview").min_size(size),
                            };
                            if ui.add(btn).on_hover_text(&e.name).clicked() {
                                answer = Some(e.usd.as_ref().map(|u| format!("{} ({})", e.name, u.display())).unwrap_or_else(|| e.name.clone()));
                            }
                            ui.add(egui::Label::new(RichText::new(&e.name).small().color(theme::WEAK)).truncate());
                        });
                    }
                });
            }
            if !q.options.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for o in &q.options {
                        let chip = egui::Button::new(RichText::new(o).color(theme::TEXT))
                            .fill(theme::HOVER)
                            .stroke(egui::Stroke::new(1.0, theme::ONLINE.gamma_multiply(0.45)))
                            .corner_radius(14)
                            .min_size(vec2(0.0, 28.0));
                        if ui.add(chip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            answer = Some(o.clone());
                        }
                    }
                });
            }
            if q.allow_text || (q.options.is_empty() && q.candidates.is_empty()) {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut q.input).hint_text("Type an answer").desired_width((ui.available_width() - 48.0).max(60.0)));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (theme::icon_button(ui, icon::ARROW_UP, "Answer").clicked() || enter) && !q.input.trim().is_empty() {
                        answer = Some(q.input.trim().to_string());
                    }
                });
            }
            if let Some(a) = answer {
                q.answer(a);
            }
        });
}

fn composer(ui: &mut egui::Ui, ctx: &egui::Context, agent: &mut Agent, bridge: &ToolBridge, open_settings: &mut bool) {
    if let Some(b) = agent.blocker() {
        egui::Frame::new().fill(theme::DANGER.gamma_multiply(0.14)).corner_radius(8).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(icon::WARNING_CIRCLE).color(theme::DANGER));
                ui.add(egui::Label::new(RichText::new(&b).color(theme::TEXT)).wrap());
                if ui.link("Open settings").clicked() {
                    *open_settings = true;
                }
            });
        });
        ui.add_space(6.0);
    }
    if agent.running {
        let waiting = agent.questions.iter().any(|q| q.answer.is_none() && q.reply.is_some());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add(egui::Spinner::new().size(12.0).color(if waiting { theme::ONLINE } else { theme::ACCENT }));
            ui.label(RichText::new(if waiting { "Waiting for your answer" } else { "Working…" }).small().color(theme::WEAK));
        });
        ui.add_space(4.0);
    }
    // Outline the box while the text field has focus (known from last frame).
    let input_id = egui::Id::new("assistant_input");
    let focused = ctx.memory(|m| m.has_focus(input_id));
    let stroke = if focused { egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.7)) } else { egui::Stroke::new(1.0, theme::STROKE) };
    egui::Frame::new().fill(theme::PANEL).stroke(stroke).corner_radius(6).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if !agent.attachments.is_empty() {
            let mut remove = None;
            ui.horizontal(|ui| {
                for (k, id) in agent.attachments.iter().enumerate() {
                    if let Some(t) = texture(ctx, &mut agent.textures, id) {
                        ui.add(egui::Image::new(&t).max_height(54.0).corner_radius(6));
                    }
                    if theme::icon_button(ui, icon::X, "Remove").clicked() {
                        remove = Some(k);
                    }
                }
            });
            if let Some(k) = remove {
                agent.attachments.remove(k);
            }
        }
        // Enter sends, Shift+Enter is a new line (Ctrl+Enter still sends). Not while an
        // input method is composing (Vietnamese Telex/VNI, CJK): there Enter commits.
        let focused = ui.memory(|m| m.has_focus(input_id));
        let composing = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) if !text.is_empty())));
        let send_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter))
            || (focused && !composing && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)));
        ui.add(
            egui::TextEdit::multiline(&mut agent.input)
                .id(input_id)
                .frame(egui::Frame::NONE)
                .hint_text(if agent.attachments.is_empty() { "Ask the assistant…" } else { "What should I do with this photo? (optional)" })
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if theme::icon_button(ui, icon::PAPERCLIP, "Attach a reference photo (or drop one on the window)").clicked()
                && let Some(p) = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "webp", "bmp"]).pick_file()
            {
                agent.attach_file(&p);
            }
            let p = agent.config.provider();
            let chip = format!("{} · {}", p.kind.label(), p.effective_model());
            if ui
                .add(egui::Button::new(RichText::new(clip(&chip, 34)).small().color(theme::WEAK)).fill(theme::RAISED).corner_radius(10))
                .on_hover_text("Change provider or model")
                .clicked()
            {
                *open_settings = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if agent.running {
                    // The status line sits above the box (see `composer`), so the
                    // model chip keeps its room here.
                    if round_button(ui, icon::STOP, theme::DANGER).on_hover_text("Stop").clicked() {
                        agent.stop();
                    }
                } else {
                    let can = (!agent.input.trim().is_empty() || !agent.attachments.is_empty()) && agent.blocker().is_none();
                    // Disabled: a quiet grey circle rather than a dimmed accent one.
                    let r = if can {
                        round_button(ui, icon::ARROW_UP, theme::ACCENT)
                    } else {
                        ui.add(egui::Button::new(RichText::new(icon::ARROW_UP).size(15.0).color(theme::FAINT)).fill(theme::HOVER).corner_radius(14).min_size(vec2(28.0, 28.0)).sense(egui::Sense::hover()))
                    };
                    if (r.on_hover_text("Send (Enter) · Shift+Enter for a new line").clicked() || send_key) && can {
                        agent.send(bridge);
                    }
                }
            });
        });
    });
}

fn round_button(ui: &mut egui::Ui, ic: &str, fill: Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(ic).size(15.0).color(theme::ON_ACCENT)).fill(fill).corner_radius(14).min_size(vec2(28.0, 28.0)))
}

// ---------------------------------------------------------------------------
// Settings page

/// The "AI assistant" page of the app's Settings window.
pub fn settings_ui(ui: &mut egui::Ui, agent: &mut Agent, library: &mut Library, catalog: &mut Catalog) {
    let mut changed = false;

    theme::caption(ui, "Model");
    let mut active = agent.config.active;
    theme::setting_row(ui, "Provider", "Cloud API or a local OpenAI-compatible server", |ui| {
        egui::ComboBox::from_id_salt("provider").selected_text(active.label()).width(200.0).show_ui(ui, |ui| {
            for k in ProviderKind::ALL {
                ui.selectable_value(&mut active, k, k.label());
            }
        });
    });
    if active != agent.config.active {
        agent.config.active = active;
        agent.test_status = None;
        changed = true;
        if !agent.models.contains_key(&active) {
            agent.refresh_models();
        }
    }
    let kind = agent.config.active;
    {
        let models = agent.models.get(&kind).cloned().unwrap_or_default();
        let mut refresh = false;
        let mut model = agent.config.provider().effective_model();
        let status = agent.models_status.clone();
        theme::setting_row(ui, "Model", &status, |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("model").selected_text(clip(&model, 32)).width(220.0).height(360.0).show_ui(ui, |ui| {
                    if models.is_empty() {
                        ui.label(RichText::new("Click refresh to load the list").color(theme::WEAK));
                    }
                    for m in &models {
                        ui.selectable_value(&mut model, m.clone(), m);
                    }
                });
                if theme::icon_button(ui, icon::ARROWS_CLOCKWISE, "Load the provider's model list").clicked() {
                    refresh = true;
                }
            });
        });
        if model != agent.config.provider().effective_model() {
            agent.config.provider_mut().model = model;
            changed = true;
        }
        let p = agent.config.provider_mut();
        theme::setting_row(ui, "Custom model id", "Use any id the provider accepts", |ui| {
            changed |= ui.add(egui::TextEdit::singleline(&mut p.model).hint_text(p.kind.default_model()).desired_width(240.0)).lost_focus();
        });
        if refresh {
            agent.refresh_models();
        }
    }
    {
        let p = agent.config.provider_mut();
        theme::setting_row(ui, "Endpoint", "Base URL of the API", |ui| {
            changed |= ui.add(egui::TextEdit::singleline(&mut p.base_url).desired_width(240.0)).lost_focus();
        });
        if p.kind.needs_key() {
            let from_env = p.key_from_env();
            theme::setting_row(ui, "API key", if from_env { "Read from the environment" } else { "Stored in agent.json" }, |ui| {
                if from_env {
                    ui.label(RichText::new(p.kind.env_var()).color(theme::SUCCESS));
                } else {
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut p.api_key).password(true).hint_text(p.kind.env_var()).desired_width(240.0))
                        .lost_focus();
                }
            });
        }
        if p.kind == ProviderKind::Anthropic {
            theme::setting_row(ui, "Refusal fallback", "Let the API retry a declined request on another model", |ui| {
                changed |= theme::toggle_switch(ui, &mut p.anthropic_fallbacks).changed();
            });
        }
    }
    {
        let cfg = &mut agent.config;
        theme::setting_row(ui, "Effort", "How hard the model thinks (Anthropic)", |ui| {
            egui::ComboBox::from_id_salt("effort").selected_text(cfg.effort.clone()).show_ui(ui, |ui| {
                for e in ["low", "medium", "high", "xhigh", "max"] {
                    changed |= ui.selectable_value(&mut cfg.effort, e.to_string(), e).changed();
                }
            });
        });
        theme::setting_row(ui, "Max steps", "Tool calls per request before it stops", |ui| {
            changed |= ui.add(egui::DragValue::new(&mut cfg.max_steps).range(5..=300)).lost_focus();
        });
    }
    ui.horizontal(|ui| {
        if theme::primary_button(ui, "Test connection").clicked() {
            agent.test_connection();
        }
        match &agent.test_status {
            Some(Ok(t)) => {
                ui.add(egui::Label::new(RichText::new(t).color(theme::SUCCESS)).wrap());
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(e).color(theme::DANGER)).wrap());
            }
            None => {}
        }
    });

    theme::divider(ui);
    theme::caption(ui, "Photo measurement");
    {
        let cfg = &mut agent.config;
        theme::setting_row(ui, "Photo field of view", "Used when the photo has no EXIF focal length", |ui| {
            changed |= ui.add(egui::Slider::new(&mut cfg.photo_hfov_deg, 30.0..=120.0).suffix("°")).drag_stopped();
        });
    }
    if let Some((label, done, total)) = super::vision::runtime::progress().snapshot() {
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        ui.add(egui::ProgressBar::new(frac).text(format!("{label}  {:.0} / {:.0} MB", done as f64 / 1e6, total as f64 / 1e6)));
    }

    theme::divider(ui);
    theme::caption(ui, "Assets");
    let known = library.entries.iter().filter(|e| e.usd.as_ref().is_some_and(|u| catalog.get(u).is_some())).count();
    let local = library.entries.iter().filter(|e| e.usd.is_some()).count();
    theme::setting_row(ui, "Asset sizes", &format!("{known} of {local} local assets measured"), |ui| {
        if catalog.indexing > 0 {
            ui.spinner();
            ui.label(format!("{} left", catalog.indexing));
        } else if known < local && ui.button("Measure all").clicked() {
            catalog.index_all(library);
        }
    });

    theme::divider(ui);
    theme::caption(ui, "Knowledge base");
    {
        let ix = super::knowledge::index();
        let summary: Vec<String> = ix.domains().into_iter().map(|(d, docs, _)| format!("{d} {docs}")).collect();
        ui.add(egui::Label::new(RichText::new(format!("{} documents, {} passages: {}", ix.docs.len(), ix.chunk_count(), summary.join(", "))).color(theme::WEAK)).wrap());
        let cfg = &mut agent.config;
        theme::setting_row(ui, "Attach automatically", "Add clearly relevant passages to your messages", |ui| {
            changed |= theme::toggle_switch(ui, &mut cfg.knowledge_auto).changed();
        });
        let skipped = super::knowledge::skipped();
        if !skipped.is_empty() {
            let lines: Vec<String> = skipped.iter().map(|(f, why)| format!("{f}: {why}")).collect();
            ui.add(egui::Label::new(RichText::new(format!("Skipped {} of your files", skipped.len())).color(theme::WARNING)).wrap()).on_hover_text(lines.join("\n"));
        }
        ui.horizontal(|ui| {
            let dir = super::knowledge::user_dir();
            if ui.button(theme::label(icon::ARROW_SQUARE_OUT, "Add your documents")).on_hover_text(format!("{}\n.md or .txt, one sub-folder per topic", dir.display())).clicked() {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            }
            if ui.button(theme::label(icon::ARROWS_CLOCKWISE, "Reload documents")).clicked() {
                super::knowledge::reload();
            }
        });
        let id = ui.make_persistent_id("knowledge_try");
        let mut query = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
        if ui.add(egui::TextEdit::singleline(&mut query).hint_text("Try a search, e.g. concrete wall attenuation").desired_width(f32::INFINITY)).changed() {
            ui.data_mut(|d| d.insert_temp(id, query.clone()));
        }
        for h in ix.search(&query, &[], 5, 2) {
            let (d, c) = ix.chunk(&h);
            ui.add(egui::Label::new(RichText::new(format!("{} ({}, {:.0}% of the query)", c.heading, d.domain, h.coverage * 100.0)).small()).wrap()).on_hover_text(&c.text);
        }
    }

    theme::divider(ui);
    theme::caption(ui, "MCP");
    ui.label(RichText::new(&agent.mcp_server_status).color(theme::WEAK));
    let status = agent.mcp.status.lock().unwrap().clone();
    if status.is_empty() {
        ui.label(RichText::new("No external MCP servers. Add \"mcp_servers\" to agent.json.").color(theme::WEAK));
    }
    for (name, s) in status {
        ui.label(format!("{name}: {s}"));
    }
    ui.horizontal(|ui| {
        if ui.button(theme::label(icon::ARROW_SQUARE_OUT, "Open agent.json")).on_hover_text(AgentConfig::path().display().to_string()).clicked() {
            let p = AgentConfig::path();
            if !p.exists() {
                let _ = agent.config.save();
            }
            let _ = std::process::Command::new("explorer").arg(&p).spawn();
        }
        if ui.button(theme::label(icon::ARROWS_CLOCKWISE, "Reload & reconnect")).clicked() {
            agent.config = AgentConfig::load();
            agent.mcp.connect_all(&agent.config.mcp_servers);
        }
    });

    if changed {
        let _ = agent.config.save();
    }
}
