//! The assistant's chat panel and its settings page (shown in the app's
//! Settings window under "AI assistant").

use bevy_egui::egui::{self, Align, Color32, Layout, Margin, RichText, vec2};

use super::catalog::Catalog;
use super::config::{AgentConfig, ProviderKind, may_train_on_input};
use super::tools::ToolBridge;
use super::{Agent, ChatItem, images};
use crate::library::Library;
use crate::theme::{self, icon};

const CANDIDATE: f32 = 84.0;

/// Remembers, for the session, whether the plan card is open.
const PLAN_OPEN: &str = "assistant_plan_open";
/// The most of the room above the message box that an open plan's steps may take.
const PLAN_SHARE: f32 = 0.4;

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
                plan(ui, &agent.plan);
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                transcript(ui, ctx, agent, library);
            });
        });
    open_settings
}

/// The checklist, pinned above the chat. Closed it takes one line, the step in hand; open, the
/// steps scroll inside a cap. However long the plan is, the chat keeps most of the panel.
fn plan(ui: &mut egui::Ui, steps: &[(String, String)]) {
    let count = |status: &str| steps.iter().filter(|(_, s)| s == status).count();
    let (done, failed, total) = (count("done"), count("failed"), steps.len());
    let in_hand = steps.iter().find(|(_, s)| s == "in_progress").or_else(|| steps.iter().find(|(_, s)| s != "done" && s != "failed"));
    let id = egui::Id::new(PLAN_OPEN);
    let open: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
    // Measured before the card takes any of it: this is the room it shares with the chat.
    let cap = (ui.available_height() * PLAN_SHARE).max(80.0);
    group_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        let r = row(ui, |ui| {
            ui.label(RichText::new(if open { icon::CARET_DOWN } else { icon::CARET_RIGHT }).small().color(theme::WEAK));
            ui.label(RichText::new("Plan").small().color(theme::TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{done} / {total}")).small().color(theme::WEAK));
                if failed > 0 {
                    ui.label(RichText::new(format!("{failed} failed")).small().color(theme::DANGER));
                }
                if let (false, Some((step, _))) = (open, in_hand) {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(format!("· {step}")).small().color(theme::WEAK)).truncate());
                    });
                }
            });
        });
        if r.clicked() {
            ui.data_mut(|d| d.insert_temp(id, !open));
        }
        // Thin progress bar under the header.
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, 2.0, theme::BG);
        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height())), 2.0, theme::SUCCESS);
        if !open {
            return;
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("assistant_plan_steps").auto_shrink([false, true]).max_height(cap).show(ui, |ui| {
            for (step, status) in steps {
                let (ic, color) = match status.as_str() {
                    "done" => (icon::CHECK, theme::SUCCESS),
                    "in_progress" => (icon::CIRCLE_NOTCH, theme::WARNING),
                    "failed" => (icon::WARNING_CIRCLE, theme::DANGER),
                    _ => ("·", theme::WEAK),
                };
                ui.horizontal(|ui| {
                    // Fixed-width marker column so every step lines up.
                    ui.add_sized(vec2(16.0, 16.0), egui::Label::new(RichText::new(ic).small().color(color)));
                    ui.add(egui::Label::new(RichText::new(step).small().color(if status == "done" { theme::WEAK } else { theme::TEXT })).wrap());
                });
            }
        });
    });
    ui.add_space(6.0);
}

/// The quiet bordered box around a plan or a run of tool calls.
fn group_frame() -> egui::Frame {
    egui::Frame::new().fill(theme::RAISED).stroke(egui::Stroke::new(1.0, theme::STROKE)).corner_radius(8).inner_margin(Margin::same(6))
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
        // Consecutive tool calls, with the thinking between them, render as one compact group.
        if matches!(transcript[i], ChatItem::Tool { .. } | ChatItem::Thinking(_)) {
            let start = i;
            while i < transcript.len() && matches!(transcript[i], ChatItem::Tool { .. } | ChatItem::Thinking(_)) {
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
            ChatItem::Tool { .. } | ChatItem::Thinking(_) => {}
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
    // The app was closed on a chat: offer it back rather than losing the work.
    if let Some(chat) = agent.resumable.clone() {
        theme::card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::caption(ui, "Last chat");
            ui.add(egui::Label::new(RichText::new(&chat.title).color(theme::TEXT)).wrap());
            ui.add_space(4.0);
            if theme::primary_button(ui, "Pick up where it left off").clicked() {
                agent.resume();
            }
        });
    }
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

/// The header of a group of tool calls and thinking. It names both, since it's all that shows of
/// a closed group.
fn group_header(tools: usize, thoughts: usize, live: bool) -> String {
    let count = |n: usize, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
    match (tools, thoughts, live) {
        (0, _, true) => "Thinking…".to_string(),
        (0, _, false) => "Thought process".to_string(),
        (t, 0, _) => format!("Used {}", count(t, "tool")),
        (t, n, _) => format!("Used {} · {}", count(t, "tool"), count(n, "thought")),
    }
}

/// The start of a thought, for its one-line row.
fn thought_preview(text: &str) -> String {
    let line = text.lines().map(|l| l.trim().trim_matches(['*', '#', ' '])).find(|l| !l.is_empty()).unwrap_or("");
    clip(line, 80)
}

/// A thought in full. The one being written follows its end, in its own small window so it can't
/// push the chat around.
fn thought_text(ui: &mut egui::Ui, text: &str, key: usize, writing: bool) {
    egui::Frame::new().fill(theme::BG).corner_radius(6).inner_margin(Margin::same(6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt(("thought_text", key)).max_height(220.0).stick_to_bottom(writing).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text.trim()).small().color(theme::WEAK)).wrap());
        });
    });
}

/// A block of consecutive tool calls and the thinking between them: a "Used N
/// tools" header (open while the turn is running, collapsed afterwards) over
/// compact one-line rows; each row expands on click, a tool's to its raw
/// input/output and a thought's to all of it. The thought being written stays open.
fn tool_group(ui: &mut egui::Ui, items: &[ChatItem], first: usize, live: bool) {
    let id = egui::Id::new(("tool_group", first));
    let user_open: Option<bool> = ui.data(|d| d.get_temp(id));
    let open = user_open.unwrap_or(live);
    let failed = items.iter().filter(|c| matches!(c, ChatItem::Tool { is_error: true, output, .. } if !output.is_empty())).count();
    group_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        let tools = items.iter().filter(|c| matches!(c, ChatItem::Tool { .. })).count();
        let header = group_header(tools, items.len() - tools, live);
        let r = row(ui, |ui| {
            ui.label(RichText::new(if open { icon::CARET_DOWN } else { icon::CARET_RIGHT }).small().color(theme::WEAK));
            if live {
                ui.add(egui::Spinner::new().size(11.0).color(theme::ACCENT));
            } else {
                ui.label(RichText::new(if tools == 0 { icon::CHAT_CIRCLE_DOTS } else { icon::WRENCH }).small().color(theme::WEAK));
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
            if let ChatItem::Thinking(text) = item {
                let writing = live && k + 1 == items.len();
                // The header already says what it is.
                if tools == 0 {
                    thought_text(ui, text, first + k, writing);
                    continue;
                }
                let rid = egui::Id::new(("thought_row", first + k));
                let expanded: bool = ui.data(|d| d.get_temp(rid)).unwrap_or(writing);
                let r = row(ui, |ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new(icon::CHAT_CIRCLE_DOTS).small().color(if writing { theme::ACCENT } else { theme::WEAK }));
                    ui.label(RichText::new(if writing { "Thinking" } else { "Thought" }).small().color(theme::TEXT));
                    if !expanded {
                        ui.add(egui::Label::new(RichText::new(format!("· {}", thought_preview(text))).small().color(theme::WEAK)).truncate());
                    }
                });
                if r.clicked() {
                    ui.data_mut(|d| d.insert_temp(rid, !expanded));
                }
                if expanded {
                    thought_text(ui, text, first + k, writing);
                }
                continue;
            }
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
    // A selectable label takes the click for itself, and the row wouldn't get it.
    child.style_mut().interaction.selectable_labels = false;
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
            Some(c) => format!("{:.0}% covered · p95 {}", c * 100.0, o["p95_predicted_error_m"].as_f64().map_or("-".to_string(), |m| format!("{:.0} cm", m * 100.0))),
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
    // Amber means attention: the app is asking for permission, not the model for an opinion.
    let (accent, mark) = if q.approval { (theme::WARNING, icon::WARNING) } else { (theme::ONLINE, icon::CHAT_CIRCLE_DOTS) };
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, accent))
        .corner_radius(10)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(mark).color(accent));
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
    if let Some(line) = agent.usage_line() {
        ui.label(RichText::new(line).small().color(theme::WEAK)).on_hover_text(agent.usage_cached());
        ui.add_space(2.0);
    }
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
            let status = if agent.interrupting {
                "Stopping this step to send your message…"
            } else if waiting {
                "Waiting for your answer"
            } else {
                "Working…"
            };
            ui.label(RichText::new(status).small().color(theme::WEAK));
        });
        ui.add_space(4.0);
    }
    queue(ui, agent);
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
                .hint_text(if !agent.attachments.is_empty() {
                    "What should I do with this photo? (optional)"
                } else if agent.running {
                    "Queue a message for when this finishes…"
                } else {
                    "Ask the assistant…"
                })
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if theme::icon_button(ui, icon::PAPERCLIP, "Attach a reference photo (or drop one on the window)").clicked()
                && let Some(p) = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "webp", "bmp"]).pick_file()
            {
                agent.attach_file(&p);
            }
            // Blue means act, so it's plain until on. Takes effect from the next message.
            let (fill, ink) = if agent.plan_mode { (theme::ACCENT, theme::ON_ACCENT) } else { (theme::RAISED, theme::WEAK) };
            if ui
                .add(egui::Button::new(RichText::new(format!("{} Plan", icon::NOTE_PENCIL)).small().color(ink)).fill(fill).corner_radius(10))
                .on_hover_text("Plan first: the assistant looks and proposes, and changes nothing until you approve")
                .clicked()
            {
                agent.plan_mode = !agent.plan_mode;
            }
            // Which model answers is a setting, not part of the chat: see the gear in the header.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if agent.running {
                    // The status line sits above the box (see `composer`).
                    if round_button(ui, icon::STOP, theme::DANGER).on_hover_text("Stop. Queued messages go back in the box").clicked() {
                        agent.stop();
                    }
                }
                let can = (!agent.input.trim().is_empty() || !agent.attachments.is_empty()) && agent.blocker().is_none();
                // Disabled: a quiet grey circle rather than a dimmed accent one.
                let r = if can {
                    round_button(ui, icon::ARROW_UP, theme::ACCENT)
                } else {
                    ui.add(egui::Button::new(RichText::new(icon::ARROW_UP).size(15.0).color(theme::FAINT)).fill(theme::HOVER).corner_radius(14).min_size(vec2(28.0, 28.0)).sense(egui::Sense::hover()))
                };
                let tip = if agent.running { "Queue for when this finishes (Enter) · Shift+Enter for a new line" } else { "Send (Enter) · Shift+Enter for a new line" };
                if (r.on_hover_text(tip).clicked() || send_key) && can {
                    agent.send(bridge);
                }
            });
        });
    });
}

/// Messages waiting for the turn to end. Each can be dropped, or cut in with "Send now".
fn queue(ui: &mut egui::Ui, agent: &mut Agent) {
    let (mut send_now, mut remove) = (None, None);
    for (k, q) in agent.queued.iter().enumerate() {
        let words = q.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let shown = match (words.is_empty(), q.images.len()) {
            (false, _) => words,
            (true, 1) => "1 photo".to_string(),
            (true, n) => format!("{n} photos"),
        };
        egui::Frame::new().fill(theme::RAISED).corner_radius(6).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Inside a row: on its own, a right-to-left layout takes the whole height of the panel.
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if theme::icon_button(ui, icon::X, "Don't send this").clicked() {
                        remove = Some(k);
                    }
                    // Once it's on its way there is nothing left to press.
                    if !(agent.interrupting && k == 0)
                        && ui
                            .add(egui::Button::new(RichText::new(format!("{} Send now", icon::LIGHTNING)).small()).fill(theme::HOVER).corner_radius(10))
                            .on_hover_text("Stop the current step and send this next. What's already done is kept")
                            .clicked()
                    {
                        send_now = Some(k);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&shown).small().color(theme::TEXT)).truncate()).on_hover_text(&q.text);
                    });
                });
            });
        });
        ui.add_space(4.0);
    }
    if let Some(k) = remove {
        agent.queued.remove(k);
    }
    if let Some(k) = send_now {
        agent.send_now(k);
    }
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
        if may_train_on_input(&p.effective_model()) {
            ui.add(egui::Label::new(RichText::new("Contributor tier: the vendor may train its models on what you send, including photos and scene data.").color(theme::WARNING)).wrap());
        }
        if refresh {
            agent.refresh_models();
        }
        if !agent.usage.is_empty() {
            let by_model = agent.usage_by_model();
            theme::setting_row(ui, "Used in this chat", "Tokens by model", |ui| {
                ui.label(RichText::new(by_model).small().color(theme::WEAK));
            });
        }
    }
    {
        let p = agent.config.provider_mut();
        theme::setting_row(ui, "Endpoint", "Base URL of the API", |ui| {
            changed |= ui.add(egui::TextEdit::singleline(&mut p.base_url).desired_width(240.0)).lost_focus();
        });
        // A custom endpoint may be a cloud vendor that isn't listed, so it can take a key too.
        if p.kind.needs_key() || p.kind == ProviderKind::Custom {
            let from_env = p.key_from_env();
            let hint = if p.kind.needs_key() { p.kind.env_var() } else { "Only if the server needs one" };
            theme::setting_row(ui, "API key", if from_env { "Read from the environment" } else { "Stored in agent.json" }, |ui| {
                if from_env {
                    ui.label(RichText::new(p.kind.env_var()).color(theme::SUCCESS));
                } else {
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut p.api_key).password(true).hint_text(hint).desired_width(240.0))
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
    agent.measure_storage();
    let (agent_storage, confirm_clear) = (agent.storage, agent.confirm_clear);
    let (mut clear_pressed, mut clear_confirmed, mut clear_cancelled) = (false, false, false);
    {
        let cfg = &mut agent.config;
        theme::setting_row(ui, "Effort", "How hard the model thinks (Anthropic, Muse)", |ui| {
            egui::ComboBox::from_id_salt("effort").selected_text(cfg.effort.clone()).show_ui(ui, |ui| {
                for e in ["low", "medium", "high", "xhigh", "max"] {
                    changed |= ui.selectable_value(&mut cfg.effort, e.to_string(), e).changed();
                }
            });
        });
        theme::setting_row(ui, "Max steps", "Model calls per request before it stops", |ui| {
            changed |= ui.add(egui::DragValue::new(&mut cfg.max_steps).range(5..=300)).lost_focus();
        });
        theme::setting_row(ui, "Token budget", "Per request, charged tokens before it stops and asks. 0 for no limit", |ui| {
            changed |= ui.add(egui::DragValue::new(&mut cfg.budget_tokens).speed(10_000.0).range(0..=100_000_000)).lost_focus();
        });
        theme::setting_row(ui, "Saved chats", "Chats and the assistant's working files, on this computer", |ui| {
            match agent_storage {
                Some((bytes, chats)) if !confirm_clear => {
                    ui.label(RichText::new(format!("{chats} chats, {:.1} MB", bytes as f64 / 1e6)).color(theme::WEAK));
                    clear_pressed = ui.button("Clear").clicked();
                }
                Some((_, chats)) => {
                    ui.label(RichText::new(format!("Delete {chats} chats and their files?")).color(theme::WARNING));
                    clear_confirmed = ui.button("Delete").clicked();
                    clear_cancelled = ui.button("Keep").clicked();
                }
                None => {
                    ui.label(RichText::new("measuring…").color(theme::WEAK));
                }
            }
        });
        theme::setting_row(ui, "Share what's on screen", "Workspace, open card, selection and recent actions go with each message", |ui| {
            changed |= theme::toggle_switch(ui, &mut cfg.share_screen).changed();
        });
        theme::setting_row(ui, "Approvals", "What the assistant may do without asking", |ui| {
            egui::ComboBox::from_id_salt("approval").selected_text(cfg.approval.label()).width(280.0).show_ui(ui, |ui| {
                for m in super::gate::Mode::ALL {
                    changed |= ui.selectable_value(&mut cfg.approval, m, m.label()).changed();
                }
            });
        });
    }
    if clear_pressed {
        agent.confirm_clear = true;
    }
    if clear_cancelled {
        agent.confirm_clear = false;
    }
    if clear_confirmed {
        agent.clear_saved_chats();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The height the plan card takes in an 800 point tall window, with `n` long steps.
    fn plan_height(n: usize, open: bool) -> f32 {
        let steps: Vec<_> = (0..n).map(|i| (format!("Step {i}, long enough to wrap onto a second line in a narrow panel"), "pending".to_string())).collect();
        let ctx = egui::Context::default();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(PLAN_OPEN), open));
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(320.0, 800.0))), ..Default::default() };
        let mut height = 0.0;
        let mut output = ctx.run_ui(input, |ui| {
            let top = ui.cursor().top();
            plan(ui, &steps);
            height = ui.cursor().top() - top;
        });
        // Nothing draws this frame, and epaint panics if its texture updates are dropped unapplied.
        output.textures_delta.clear();
        height
    }

    #[test]
    fn a_long_plan_leaves_the_chat_its_room() {
        let closed = plan_height(30, false);
        assert_eq!(closed, plan_height(1, false), "closed, the plan's height doesn't depend on its length");
        assert!(closed < 60.0, "closed, the plan is one line, got {closed}");
        let open = plan_height(30, true);
        assert!(open <= 800.0 * PLAN_SHARE + 60.0, "open, the steps scroll inside the cap, got {open}");
    }

    #[test]
    fn a_group_with_no_tool_calls_doesnt_claim_any() {
        assert_eq!(group_header(0, 1, true), "Thinking…");
        assert_eq!(group_header(0, 2, false), "Thought process");
        assert_eq!(group_header(1, 0, true), "Used 1 tool");
        assert_eq!(group_header(3, 0, false), "Used 3 tools");
        assert_eq!(group_header(1, 1, false), "Used 1 tool · 1 thought");
        assert_eq!(group_header(3, 2, false), "Used 3 tools · 2 thoughts");
    }

    #[test]
    fn a_thoughts_row_starts_with_its_words() {
        assert_eq!(thought_preview("\n\n**Checking the loop**\n\nThe loop is short."), "Checking the loop");
        assert_eq!(thought_preview(""), "");
        assert!(thought_preview(&"long ".repeat(100)).ends_with('…'));
    }

    #[test]
    fn an_open_plan_takes_only_what_it_needs() {
        let (one, three) = (plan_height(1, true), plan_height(3, true));
        assert!(one < three, "the cap is a limit, not a fixed height: {one} and {three}");
    }
}
