//! Asset library tab: search, filters, grid / list of assets with status
//! badges, click to place (online assets download first).

use std::path::PathBuf;
use std::sync::Arc;

use bevy_egui::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, StrokeKind, vec2};

use super::{Filter, UiState};
use crate::library::{DownloadState, Library, LibraryEntry};
use crate::prototype::{ProtoState, PrototypeCache};
use crate::rtls::DevicePlace;
use crate::rtls::device::{DeviceKind, Profile, Profiles};
use crate::scene::PlaceMode;
use crate::theme::{self, ACCENT, ONLINE, icon};

const MIN_CARD: f32 = 76.0;
const LABEL_H: f32 = 32.0;
const ROW_H: f32 = 48.0;
const GAP: f32 = 8.0;

fn elided(ui: &egui::Ui, text: &str, width: f32, rows: usize, size: f32, color: Color32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(FontId::proportional(size), color));
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: rows, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// Search field with a magnifier and a clear button.
pub fn search_box(ui: &mut egui::Ui, text: &mut String, hint: &str) {
    egui::Frame::new().fill(theme::PANEL).stroke(Stroke::new(1.0, Color32::from_rgb(214, 218, 224))).corner_radius(5).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(theme::WEAK));
            // Right-to-left so the field takes exactly the space left; asking for more
            // would make the panel grow a little every frame.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !text.is_empty()
                    && ui.add(egui::Button::new(RichText::new(icon::X).color(theme::WEAK)).frame_when_inactive(false).small()).on_hover_text("Clear").clicked()
                {
                    text.clear();
                }
                // Fixed id: the clear button appearing in front must not change the
                // field's auto id, or it loses keyboard focus after the first key.
                ui.add(egui::TextEdit::singleline(text).id_salt(hint).hint_text(hint).frame(egui::Frame::NONE).desired_width(f32::INFINITY));
            });
        });
    });
}

struct Status<'a> {
    online: bool,
    dl: Option<DownloadState>,
    proto: Option<&'a ProtoState>,
    active: bool,
}

impl Status<'_> {
    fn subtitle(&self) -> (String, Color32) {
        match (&self.dl, self.proto) {
            (Some(DownloadState::Active { done, total }), _) => {
                let pct = if *total > 0 { *done as f32 / *total as f32 * 100.0 } else { 0.0 };
                (format!("Downloading {pct:.0}% · {:.1} MB", *total as f64 / 1e6), ONLINE)
            }
            (Some(DownloadState::Queued), _) => ("Queued".into(), theme::WEAK),
            _ if self.online => ("Online · click to download".into(), ONLINE),
            (_, Some(ProtoState::Loading)) => ("Loading…".into(), theme::WARNING),
            (_, Some(ProtoState::Ready(p))) => (format!("{} tris · {} moving parts", p.triangles, p.joints.len()), theme::WEAK),
            (_, Some(ProtoState::Failed(_))) => ("Failed to load · click to retry".into(), theme::DANGER),
            _ => ("On disk".into(), theme::WEAK),
        }
    }

    fn tooltip(&self) -> String {
        match (&self.dl, self.proto) {
            (Some(_), _) => "Downloading — it will be placed when done".into(),
            _ if self.online => "Online — click to download and place".into(),
            (_, Some(ProtoState::Loading)) => "Loading…".into(),
            (_, Some(ProtoState::Ready(p))) => {
                format!("{} tris, {} parts, {} moving parts, loaded in {:.0} ms", p.triangles, p.parts.len(), p.joints.len(), p.load_ms)
            }
            (_, Some(ProtoState::Failed(err))) => format!("Failed: {err}"),
            _ => "Click to place".into(),
        }
    }

    /// Small status glyph in the corner of a card / end of a row.
    fn paint_badge(&self, painter: &egui::Painter, c: egui::Pos2) {
        let (glyph, color) = match (&self.dl, self.proto) {
            (Some(DownloadState::Queued), _) => (icon::CLOUD_ARROW_DOWN, theme::WEAK),
            (Some(_), _) => return,
            _ if self.online => (icon::CLOUD, ONLINE),
            (_, Some(ProtoState::Ready(_))) => (icon::CHECK_CIRCLE, theme::SUCCESS),
            (_, Some(ProtoState::Loading)) => (icon::CIRCLE_NOTCH, theme::WARNING),
            (_, Some(ProtoState::Failed(_))) => (icon::WARNING_CIRCLE, theme::DANGER),
            _ => return,
        };
        painter.circle(c, 10.0, theme::PANEL, Stroke::new(1.0, theme::STROKE));
        painter.text(c, Align2::CENTER_CENTER, glyph, FontId::proportional(13.0), color);
    }

    fn progress(&self) -> Option<f32> {
        match self.dl {
            Some(DownloadState::Active { done, total }) => Some(if total > 0 { (done as f32 / total as f32).min(1.0) } else { 0.0 }),
            _ => None,
        }
    }
}

fn paint_thumb(ui: &egui::Ui, rect: egui::Rect, e: &LibraryEntry, s: &Status, hovered: bool) {
    let p = ui.painter();
    p.rect_filled(rect, 4.0, theme::RAISED);
    match &e.thumb {
        Some(t) => egui::Image::new(t).corner_radius(4).paint_at(ui, rect),
        None => {
            p.text(rect.center(), Align2::CENTER_CENTER, icon::CUBE, FontId::proportional(rect.height() * 0.32), theme::FAINT);
        }
    }
    if s.online && e.thumb.is_some() {
        p.rect_filled(rect, 4.0, Color32::from_white_alpha(if hovered { 50 } else { 90 }));
    }
    if let Some(frac) = s.progress() {
        p.rect_filled(rect, 4.0, Color32::from_white_alpha(170));
        let track = egui::Rect::from_min_size(rect.left_bottom() + vec2(6.0, -10.0), vec2(rect.width() - 12.0, 4.0));
        p.rect_filled(track, 2.0, theme::STROKE);
        p.rect_filled(egui::Rect::from_min_size(track.min, vec2(track.width() * frac, 4.0)), 2.0, ONLINE);
        if rect.height() > 60.0 {
            p.text(rect.center(), Align2::CENTER_CENTER, format!("{:.0}%", frac * 100.0), FontId::proportional(14.0), theme::TEXT);
        }
    } else if matches!(s.dl, Some(DownloadState::Queued)) && rect.height() > 60.0 {
        p.text(rect.center(), Align2::CENTER_CENTER, "Queued", FontId::proportional(12.0), theme::WEAK);
    }
    if s.active {
        p.rect_stroke(rect, 4.0, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    } else if hovered {
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::STROKE_STRONG), StrokeKind::Inside);
    }
}

#[derive(Default)]
struct Clicks {
    retry: Option<PathBuf>,
    download: Option<String>,
    download_only: Option<String>,
    want: Vec<usize>,
}

pub fn show(
    ui: &mut egui::Ui,
    library: &mut Library,
    place: &mut PlaceMode,
    cache: &mut PrototypeCache,
    state: &mut UiState,
    (profiles, device_place): (&Profiles, &mut DevicePlace),
) {
    // Panel width before anything is laid out: a row that overflows widens the
    // Ui, and sizing the grid from that would push the panel wider every frame.
    let panel_w = ui.available_width();
    search_box(ui, &mut library.search, "Search assets…");
    ui.add_space(2.0);

    let local = library.entries.iter().filter(|e| e.usd.is_some()).count();
    let online = library.entries.len() - local;
    let labels = [format!("All {}", library.entries.len()), format!("Local {local}"), format!("Online {online}")];
    let opts = [(Filter::All, labels[0].as_str()), (Filter::Local, labels[1].as_str()), (Filter::Online, labels[2].as_str())];
    theme::segmented_fill(ui, &mut state.filter, &opts);
    library.show_remote = state.filter != Filter::Local;

    let terms: Vec<String> = library.search.to_lowercase().split_whitespace().map(str::to_string).collect();
    let filter = state.filter;
    let movable_only = state.movable_only;
    let visible: Vec<usize> = (0..library.entries.len())
        .filter(|&i| {
            let e = &library.entries[i];
            let kind_ok = match filter {
                Filter::All => true,
                Filter::Local => e.usd.is_some(),
                Filter::Online => e.usd.is_none(),
            };
            kind_ok && (!movable_only || e.movable) && terms.iter().all(|t| e.name.to_lowercase().contains(t))
        })
        .collect();
    let remote_visible: Vec<String> = visible
        .iter()
        .map(|&i| &library.entries[i])
        .filter(|e| e.usd.is_none() && library.download_state(&e.folder).is_none())
        .map(|e| e.folder.clone())
        .collect();

    // Buttons first (right to left), then the counts truncate into what's left.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let tip = if state.movable_only {
                "Showing only assets with moving parts (doors, lids, drawers…). Click to show all."
            } else {
                "Show only on-disk assets with moving parts (doors, lids, drawers…)"
            };
            ui.spacing_mut().item_spacing.x = 0.0;
            let (ic, view_tip) = if state.list_view { (icon::SQUARES_FOUR, "Grid view") } else { (icon::LIST_BULLETS, "List view") };
            if theme::icon_button(ui, ic, view_tip).clicked() {
                state.list_view = !state.list_view;
            }
            if theme::icon_button(ui, icon::ARROWS_CLOCKWISE, "Rescan the folder and refresh the online catalog (also rescans every 15 s)").clicked() {
                library.refresh_remote();
                library.rescan();
            }
            if theme::icon_button_colored(ui, icon::DOOR_OPEN, tip, state.movable_only.then_some(ACCENT), state.movable_only).clicked() {
                state.movable_only = !state.movable_only;
            }
            ui.spacing_mut().item_spacing.x = 8.0;
            if !terms.is_empty()
                && !remote_visible.is_empty()
                && ui
                    .add(
                        egui::Button::new(RichText::new(theme::label(icon::CLOUD_ARROW_DOWN, &format!("Get {}", remote_visible.len()))).small().color(ONLINE))
                            .small(),
                    )
                    .on_hover_text(format!("Download all {} online results", remote_visible.len()))
                    .clicked()
            {
                for f in &remote_visible {
                    library.start_download(f, false);
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let mut text = format!("{} results", visible.len());
                if !library.remote_status.is_empty() {
                    text += &format!(" · {}", library.remote_status);
                }
                ui.add(egui::Label::new(RichText::new(text).small().color(theme::WEAK)).truncate());
            });
        });
    });
    ui.add_space(2.0);

    // Positioning devices (RFID readers and tags, UWB anchors and tags, BLE)
    // aren't USD assets: they come from the device profiles and are placed by
    // the positioning system. Listed here so a search finds them too.
    let devices = matching_devices(profiles, &terms);
    if !devices.is_empty() {
        device_section(ui, &devices, place, device_place);
    }

    if visible.is_empty() {
        if !devices.is_empty() {
            return;
        }
        if library.entries.is_empty() {
            theme::empty_state(ui, icon::FOLDER_OPEN, "No assets yet", "Choose your SimReady dataset folder, or wait for the online catalog to load.");
            ui.vertical_centered(|ui| {
                if theme::primary_button(ui, &theme::label(icon::FOLDER_OPEN, "Choose folder")).clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    library.set_root(p.to_string_lossy().replace('\\', "/"));
                }
            });
        } else {
            theme::empty_state(ui, icon::MAGNIFYING_GLASS, "No matches", "Try a different search or filter.");
        }
        return;
    }

    let mut clicks = Clicks::default();
    // Leave room for the scroll bar: rows wider than the panel make it grow every frame.
    let width = (panel_w.min(ui.available_width()) - ui.spacing().scroll.allocated_width() - 1.0).floor();
    if state.list_view {
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, ROW_H, visible.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for &i in &visible[range] {
                let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                entry(ui, library, place, cache, i, rect, resp, true, &mut clicks);
            }
        });
    } else {
        let cols = (((width + GAP) / (MIN_CARD + GAP)).floor() as usize).max(1);
        let card = ((width - GAP * (cols as f32 - 1.0)) / cols as f32).floor();
        let rows = visible.len().div_ceil(cols);
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, card + LABEL_H, rows, |ui, range| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in range {
                ui.horizontal(|ui| {
                    for &i in visible.iter().skip(row * cols).take(cols) {
                        let (rect, resp) = ui.allocate_exact_size(vec2(card, card + LABEL_H), Sense::click());
                        entry(ui, library, place, cache, i, rect, resp, false, &mut clicks);
                    }
                });
            }
        });
    }

    for i in clicks.want {
        library.want_thumb(i);
    }
    if let Some(p) = clicks.retry {
        cache.retry(&p);
    }
    if let Some(f) = clicks.download {
        library.start_download(&f, true);
    }
    if let Some(f) = clicks.download_only {
        library.start_download(&f, false);
    }
}

#[allow(clippy::too_many_arguments)]
fn entry(
    ui: &mut egui::Ui,
    library: &Library,
    place: &mut PlaceMode,
    cache: &PrototypeCache,
    i: usize,
    rect: egui::Rect,
    resp: egui::Response,
    list: bool,
    clicks: &mut Clicks,
) {
    let e = &library.entries[i];
    let s = Status {
        online: e.usd.is_none(),
        dl: library.download_state(&e.folder),
        proto: e.usd.as_ref().and_then(|u| cache.states.get(u)),
        active: e.usd.is_some() && place.asset.as_ref() == e.usd.as_ref(),
    };
    if e.thumb.is_none() && ui.is_rect_visible(rect) {
        clicks.want.push(i);
    }
    let hovered = resp.hovered();
    let name_color = if s.online { ONLINE } else { theme::TEXT };
    if list {
        if hovered || s.active {
            ui.painter().rect_filled(rect, 4.0, if s.active { theme::ACCENT_SOFT } else { theme::RAISED });
        }
        let thumb = egui::Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(ROW_H - 8.0, ROW_H - 8.0));
        paint_thumb(ui, thumb, e, &s, false);
        let x = thumb.right() + 10.0;
        let text_w = rect.right() - x - 30.0;
        let name = elided(ui, &e.name, text_w, 1, 13.0, name_color);
        let (sub, sub_c) = s.subtitle();
        let sub = elided(ui, &sub, text_w, 1, 11.0, sub_c);
        ui.painter().galley(egui::pos2(x, rect.top() + 7.0), name, name_color);
        ui.painter().galley(egui::pos2(x, rect.top() + 26.0), sub, sub_c);
        s.paint_badge(ui.painter(), egui::pos2(rect.right() - 16.0, rect.center().y));
    } else {
        let thumb = egui::Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));
        paint_thumb(ui, thumb, e, &s, hovered);
        s.paint_badge(ui.painter(), thumb.right_top() + vec2(-14.0, 14.0));
        if hovered && s.dl.is_none() {
            let (glyph, text) = if s.online {
                (icon::DOWNLOAD_SIMPLE, "Get")
            } else if s.active {
                (icon::X, "Cancel")
            } else {
                (icon::PLUS, "Place")
            };
            let pill = egui::Rect::from_center_size(thumb.center_bottom() + vec2(0.0, -16.0), vec2(72.0, 22.0));
            ui.painter().rect_filled(pill, 11.0, theme::INK);
            ui.painter().text(pill.center(), Align2::CENTER_CENTER, theme::label(glyph, text), FontId::proportional(12.0), Color32::WHITE);
        }
        let name = elided(ui, &e.name, rect.width() - 2.0, 2, 11.5, name_color);
        ui.painter().galley(egui::pos2(rect.left() + 1.0, thumb.bottom() + 4.0), name, name_color);
    }

    let location = e.usd.as_ref().map(|u| u.display().to_string()).unwrap_or_else(|| format!("{}/{}", crate::remote::REPO, e.folder));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_ui(|ui| {
        ui.label(RichText::new(&e.name).strong());
        ui.label(RichText::new(&location).small().color(theme::WEAK));
        ui.label(s.tooltip());
    });
    if resp.clicked() {
        match &e.usd {
            None => clicks.download = Some(e.folder.clone()),
            Some(_) if s.active => place.asset = None,
            Some(usd) => {
                if matches!(s.proto, Some(ProtoState::Failed(_))) {
                    clicks.retry = Some(usd.clone());
                }
                place.asset = Some(usd.clone());
            }
        }
    }
    resp.context_menu(|ui| {
        ui.set_min_width(210.0);
        ui.label(RichText::new(&e.name).strong());
        ui.separator();
        match &e.usd {
            Some(usd) => {
                if theme::menu_item(ui, icon::PLUS, "Place in scene").clicked() {
                    place.asset = Some(usd.clone());
                    ui.close();
                }
                if theme::menu_item(ui, icon::STACK, "Place several").clicked() {
                    place.asset = Some(usd.clone());
                    place.sticky = true;
                    ui.close();
                }
                ui.separator();
                if theme::menu_item(ui, icon::FOLDER_OPEN, "Open file location").clicked() {
                    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", usd.display())).spawn();
                    ui.close();
                }
                if theme::menu_item(ui, icon::COPY, "Copy USD path").clicked() {
                    ui.ctx().copy_text(usd.display().to_string());
                    ui.close();
                }
            }
            None => {
                if s.dl.is_none() && theme::menu_item(ui, icon::PLUS, "Download and place").clicked() {
                    clicks.download = Some(e.folder.clone());
                    ui.close();
                }
                if s.dl.is_none() && theme::menu_item(ui, icon::DOWNLOAD_SIMPLE, "Download only").clicked() {
                    clicks.download_only = Some(e.folder.clone());
                    ui.close();
                }
                if theme::menu_item(ui, icon::ARROW_SQUARE_OUT, "Open on Hugging Face").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://huggingface.co/datasets/{}/tree/main/{}", crate::remote::REPO, e.folder)));
                    ui.close();
                }
            }
        }
        if theme::menu_item(ui, icon::LINK, "Copy name").clicked() {
            ui.ctx().copy_text(e.folder.clone());
            ui.close();
        }
    });
}

/// Words a device of this kind is found by, besides its name, vendor and id.
fn kind_words(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::UwbAnchor => "uwb anchor anchors archor positioning rtls",
        DeviceKind::UwbTag => "uwb tag tags badge positioning rtls",
        DeviceKind::BleNode => "ble bluetooth beacon gateway anchor locator positioning rtls",
        DeviceKind::BleTag => "ble bluetooth tag tags beacon positioning rtls",
        DeviceKind::RfidReader => "rfid uhf reader readers antenna portal positioning rtls",
        DeviceKind::RfidTag => "rfid uhf tag tags label inlay epc positioning rtls",
        DeviceKind::WifiAp => "wifi wi-fi access point ap router ftm rtt positioning rtls",
        DeviceKind::WifiClient => "wifi wi-fi client phone handheld tablet positioning rtls",
    }
}

/// Device profiles matching every search term. Nothing when the search is empty.
fn matching_devices<'a>(profiles: &'a Profiles, terms: &[String]) -> Vec<&'a Profile> {
    if terms.is_empty() {
        return Vec::new();
    }
    profiles
        .0
        .iter()
        .filter(|p| {
            let hay = format!("{} {} {} {} {}", p.name, p.vendor, p.id, p.kind.label(), kind_words(p.kind)).to_lowercase();
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .collect()
}

/// "Positioning devices" list: click one, then click in the scene to place it.
fn device_section(ui: &mut egui::Ui, devices: &[&Profile], place: &mut PlaceMode, device_place: &mut DevicePlace) {
    ui.label(RichText::new(format!("Positioning devices · {}", devices.len())).small().strong().color(theme::WEAK));
    egui::ScrollArea::vertical().id_salt("library_devices").max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let w = ui.available_width();
        for p in devices {
            let active = device_place.profile.as_deref() == Some(p.id.as_str());
            let c = p.kind.color().to_srgba();
            let dot = theme::ink(Color32::from_rgb((c.red * 255.0) as u8, (c.green * 255.0) as u8, (c.blue * 255.0) as u8));
            let resp = theme::color_row(ui, w, dot, &format!("{}  ·  {}", p.name, p.kind.label()), active)
                .on_hover_text(format!("{} · {}\nClick, then click in the scene to place it (Shift keeps placing, Esc stops)", p.vendor, p.kind.label()));
            if resp.clicked() {
                device_place.profile = if active { None } else { Some(p.id.clone()) };
                device_place.waypoints_for = None;
                place.asset = None;
            }
        }
    });
    ui.separator();
}
