//! The start dialog: shown at launch and from File → New. What to work on: an antenna,
//! which needs no scene, or a scene. Kinds on the left, every choice of that
//! kind in the middle, the highlighted one described on the right. Hovering previews; a
//! click selects, and the button (or a double click) goes ahead. It can always be
//! closed: a blank scene is there to fall back on.

use bevy_egui::egui::{self, Align, Align2, Layout, Margin, RichText, Sense, Stroke, UiBuilder, pos2, vec2};

use crate::library::Library;
use antenna_core::samples as antenna_samples;

use crate::samples::{self, SAMPLES};
use crate::theme::{self, ACCENT, icon};

pub enum Choice {
    Antenna,
    OpenAntenna,
    Empty,
    Sample(usize),
    /// A sample antenna project, by its place in `antenna_core::samples::all`.
    AntennaSample(usize),
    Open,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Antenna,
    Empty,
    Sample(usize),
    AntennaSample(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    All,
    Design,
    Antennas,
    Blank,
    Positioning,
    Facilities,
    Plant,
}

const KINDS: [(Kind, &str, &str); 7] = [
    (Kind::All, icon::SQUARES_FOUR, "Everything"),
    (Kind::Design, icon::TARGET, "Design"),
    (Kind::Antennas, icon::BROADCAST, "Antenna samples"),
    (Kind::Blank, icon::FILE_DASHED, "Blank scene"),
    (Kind::Positioning, icon::BROADCAST, "Positioning"),
    (Kind::Facilities, icon::CUBE, "Facilities"),
    (Kind::Plant, icon::DROP, "Plant systems"),
];

/// What the dialog remembers while it is open.
pub struct StartState {
    kind: Kind,
    selected: Option<Item>,
    /// Item under the pointer last frame: previewed on the right.
    hovered: Option<Item>,
}

impl Default for StartState {
    /// With nothing open yet, an antenna project is what is offered first.
    fn default() -> Self {
        Self { kind: Kind::default(), selected: Some(Item::Antenna), hovered: None }
    }
}

impl StartState {
    pub fn select_antenna(&mut self) {
        self.selected = Some(Item::Antenna);
    }

    pub fn select_antenna_sample(&mut self, index: usize) {
        self.selected = (index < antenna_samples::all().len()).then_some(Item::AntennaSample(index));
    }

    pub fn select_empty(&mut self) {
        self.selected = Some(Item::Empty);
    }

    pub fn select_sample(&mut self, index: usize) {
        self.selected = (index < SAMPLES.len()).then_some(Item::Sample(index));
    }
}

/// The list section a sample belongs to, by its place in `SAMPLES`.
fn kind_of(index: usize) -> Kind {
    match index {
        0 | 1 => Kind::Positioning,
        6..=8 => Kind::Plant,
        _ => Kind::Facilities,
    }
}

/// Keywords shown as chips on a sample's description.
fn tags(index: usize) -> &'static [&'static str] {
    match index {
        0 => &["Positioning", "UWB", "Warehouse", "Anchors", "Asset tracking"],
        1 => &["Positioning", "RFID", "Warehouse", "Smart shelves", "Portals"],
        2 => &["Facility", "Healthcare", "Interior layout"],
        3 => &["Facility", "Vehicles", "Workbenches", "Storage"],
        4 => &["Facility", "Office", "Interior layout"],
        5 => &["Facility", "Food service", "Interior layout"],
        6 => &["Plant", "Fluid flow", "Pump", "Tanks", "Valves"],
        7 => &["Plant", "Rotating machinery", "Belt drive", "Generator"],
        8 => &["Plant", "Material handling", "Conveyors"],
        _ => &[],
    }
}

/// Draws the dialog; returns what the user picked, if anything.
pub fn show(ctx: &egui::Context, st: &mut StartState, library: &mut Library, name: &mut String, antenna: bool) -> Option<Choice> {
    let mut choice = None;
    if !antenna && st.selected == Some(Item::Antenna) {
        st.selected = None;
    }
    let screen = ctx.content_rect();
    let size = vec2(1040.0f32.min(screen.width() - 48.0), 830.0f32.min(screen.height() - 40.0));
    let modal = egui::Modal::new(egui::Id::new("start_modal")).backdrop_color(theme::BACKDROP).frame(theme::modal_frame()).show(ctx, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

        if theme::modal_title(ui, size.x, "What do you want to work on?", true, "Close (Esc)") {
            choice = Some(Choice::Cancel);
        }

        // Fixed strips below the three columns.
        let (footer_h, cta_h) = (52.0, 58.0);
        let (body, _) = ui.allocate_exact_size(vec2(size.x, size.y - 56.0 - footer_h - cta_h), Sense::hover());
        let nav_w = 232.0;
        let list_w = 286.0;
        let nav = egui::Rect::from_min_size(body.min, vec2(nav_w, body.height()));
        let list = egui::Rect::from_min_size(body.min + vec2(nav_w, 0.0), vec2(list_w, body.height()));
        let detail = egui::Rect::from_min_max(pos2(list.right(), body.top()), body.max);
        let p = ui.painter().clone();
        p.vline(list.left(), body.y_range(), Stroke::new(1.0, theme::STROKE));
        p.vline(list.right(), body.y_range(), Stroke::new(1.0, theme::STROKE));

        // ---- Kinds
        let mut child = ui.new_child(UiBuilder::new().max_rect(nav));
        child.spacing_mut().item_spacing = vec2(0.0, 0.0);
        for (k, glyph, text) in KINDS {
            if theme::nav_item(&mut child, glyph, text, st.kind == k, nav_w - 1.0).clicked() {
                st.kind = k;
            }
        }

        // ---- Scenes of that kind
        let previous_hover = st.hovered.take();
        let mut child = ui.new_child(UiBuilder::new().max_rect(list.shrink2(vec2(1.0, 0.0))));
        child.set_clip_rect(list);
        egui::ScrollArea::vertical().id_salt("start_list").auto_shrink(false).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let w = list_w - 2.0 - ui.spacing().scroll.allocated_width();
            let w = w.max(100.0);
            let mut section = |ui: &mut egui::Ui, title: &str, kind: Kind, items: &[Item]| {
                if st.kind != Kind::All && st.kind != kind {
                    return;
                }
                theme::band(ui, title, w);
                for &item in items {
                    let (glyph, text, online) = match item {
                        Item::Antenna => (icon::TARGET, "Antenna", false),
                        Item::Empty => (icon::FILE_DASHED, "Empty scene", false),
                        Item::AntennaSample(i) => (icon::BROADCAST, antenna_samples::all()[i].title, false),
                        Item::Sample(i) => {
                            let (on_disk, total) = samples::available(&SAMPLES[i], library);
                            (SAMPLES[i].icon, SAMPLES[i].title, on_disk < total)
                        }
                    };
                    let (rect, r) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
                    let sel = st.selected == Some(item);
                    if sel {
                        ui.painter().rect_filled(rect, 0.0, theme::ACCENT_SOFT);
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, 0.0, theme::RAISED);
                    }
                    ui.painter().text(rect.left_center() + vec2(22.0, 0.0), Align2::CENTER_CENTER, glyph, egui::FontId::proportional(16.0), if sel { ACCENT } else { theme::WEAK });
                    ui.painter().text(rect.left_center() + vec2(40.0, 0.0), Align2::LEFT_CENTER, text, egui::FontId::proportional(14.0), theme::TEXT);
                    if online {
                        let at = egui::Rect::from_min_max(rect.center_top(), rect.right_bottom() - vec2(8.0, 0.0));
                        let mut c = ui.new_child(UiBuilder::new().max_rect(at).layout(Layout::right_to_left(Align::Center)));
                        theme::pill(&mut c, "Online", ACCENT).on_hover_text("Assets that are not on disk are downloaded when the scene loads.");
                    }
                    if r.hovered() {
                        st.hovered = Some(item);
                    }
                    if r.clicked() {
                        st.selected = Some(item);
                    }
                    if r.double_clicked() {
                        choice = Some(choice_of(item));
                    }
                    r.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            };
            if antenna {
                section(ui, "Design, no scene needed", Kind::Design, &[Item::Antenna]);
                for g in antenna_samples::Group::ALL {
                    let items: Vec<Item> = antenna_samples::all().iter().enumerate().filter(|(_, s)| s.group == g).map(|(i, _)| Item::AntennaSample(i)).collect();
                    section(ui, &format!("Antennas: {}", g.label()), Kind::Antennas, &items);
                }
            }
            section(ui, "Start a scene from nothing", Kind::Blank, &[Item::Empty]);
            let of = |k: Kind| -> Vec<Item> { (0..SAMPLES.len()).filter(|&i| kind_of(i) == k).map(Item::Sample).collect() };
            section(ui, "Positioning", Kind::Positioning, &of(Kind::Positioning));
            section(ui, "Facilities", Kind::Facilities, &of(Kind::Facilities));
            section(ui, "Plant systems", Kind::Plant, &of(Kind::Plant));
        });

        // ---- The highlighted scene
        let shown = st.hovered.or(previous_hover).or(st.selected);
        let mut child = ui.new_child(UiBuilder::new().max_rect(detail));
        child.set_clip_rect(detail);
        match shown {
            None => nothing_selected(&mut child, detail),
            Some(Item::Antenna) => {
                design_detail(&mut child, detail, icon::TARGET, "Antenna", "Design one antenna on its own: start from a template, change its variables, run a field solver, and read S-parameters and far fields. You can use it in a scene later.", &["Templates", "Field solvers", "S-parameters", "Far field", "Optimizer"], |ui| {
                    if ui.button(theme::label(icon::FOLDER_OPEN, "Open an antenna project…")).clicked() {
                        choice = Some(Choice::OpenAntenna);
                    }
                });
            }
            Some(Item::Empty) => empty_detail(&mut child, detail),
            Some(Item::Sample(i)) => sample_detail(&mut child, detail, library, i),
            Some(Item::AntennaSample(i)) => antenna_sample_detail(&mut child, detail, i),
        }

        // ---- "Have a layout?"
        let (footer, footer_r) = ui.allocate_exact_size(vec2(size.x, footer_h), Sense::click());
        p.hline(footer.x_range(), footer.top(), Stroke::new(1.0, theme::STROKE));
        if footer_r.hovered() {
            p.rect_filled(footer.shrink(1.0), 0.0, theme::RAISED);
        }
        p.text(footer.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, "Have a project?", theme::bold(15.5), theme::TEXT);
        p.text(footer.left_center() + vec2(142.0, 0.0), Align2::LEFT_CENTER, "Click here to open a scene or antenna project you saved earlier.", egui::FontId::proportional(14.0), theme::TEXT);
        if footer_r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            choice = Some(Choice::Open);
        }

        // ---- Name and the button
        let (cta, _) = ui.allocate_exact_size(vec2(size.x, cta_h), Sense::hover());
        p.hline(cta.x_range(), cta.top(), Stroke::new(1.0, theme::STROKE));
        let mut child = ui.new_child(UiBuilder::new().max_rect(cta.shrink2(vec2(14.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
        child.spacing_mut().item_spacing = vec2(10.0, 0.0);
        // Only a scene is named here: an antenna gets its name in its own workspace.
        let mut enter = false;
        if !matches!(st.selected, Some(Item::Antenna | Item::AntennaSample(_))) {
            child.label("Scene name");
            let hint = match st.selected {
                Some(Item::Sample(i)) => SAMPLES[i].title,
                _ => "Untitled scene",
            };
            let field = egui::TextEdit::singleline(name).hint_text(hint).desired_width(280.0).margin(Margin::symmetric(8, 7));
            let r = child.add(field);
            enter = r.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter));
        }
        child.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let can = st.selected.is_some();
            let text = match st.selected {
                Some(Item::Antenna) => "Design Antenna",
                Some(Item::AntennaSample(_)) => "Open Sample",
                Some(_) => "Create Scene",
                None => "Create",
            };
            if (theme::cta_button(ui, text, can).clicked() || enter) && can {
                choice = st.selected.map(choice_of);
            }
        });
    });
    if modal.should_close() && choice.is_none() {
        choice = Some(Choice::Cancel);
    }
    choice
}

fn choice_of(item: Item) -> Choice {
    match item {
        Item::Antenna => Choice::Antenna,
        Item::Empty => Choice::Empty,
        Item::Sample(i) => Choice::Sample(i),
        Item::AntennaSample(i) => Choice::AntennaSample(i),
    }
}

fn design_detail(ui: &mut egui::Ui, rect: egui::Rect, glyph: &str, title: &str, blurb: &str, chips: &[&str], below: impl FnOnce(&mut egui::Ui)) {
    let hero = egui::Rect::from_min_size(rect.min, vec2(rect.width(), (rect.width() * 0.46).min(rect.height() * 0.42)));
    let p = ui.painter();
    p.rect_filled(hero, 0.0, theme::BG);
    p.text(hero.center(), Align2::CENTER_CENTER, glyph, egui::FontId::proportional(56.0), theme::FAINT);
    let body = egui::Rect::from_min_max(pos2(rect.left() + 20.0, hero.bottom() + 16.0), rect.max - vec2(20.0, 8.0));
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
        ui.label(RichText::new(title).font(theme::bold(18.0)));
        ui.add(egui::Label::new(blurb).wrap());
        ui.horizontal_wrapped(|ui| {
            for c in chips {
                theme::tag_chip(ui, c);
            }
        });
        ui.add_space(4.0);
        below(ui);
    });
}

fn nothing_selected(ui: &mut egui::Ui, rect: egui::Rect) {
    let c = rect.center();
    let p = ui.painter();
    p.text(c - vec2(0.0, 26.0), Align2::CENTER_CENTER, "Nothing selected", egui::FontId::proportional(20.0), theme::WEAK);
    let hint = "Pick something from the list, or press Esc to start with a blank scene.";
    let g = p.layout(hint.to_string(), egui::FontId::proportional(12.0), theme::WEAK, 300.0);
    p.galley(pos2(c.x - g.size().x / 2.0, c.y + 2.0), g, theme::WEAK);
}

fn empty_detail(ui: &mut egui::Ui, rect: egui::Rect) {
    let hero = egui::Rect::from_min_size(rect.min, vec2(rect.width(), (rect.width() * 0.46).min(rect.height() * 0.42)));
    let p = ui.painter();
    p.rect_filled(hero, 0.0, theme::BG);
    p.text(hero.center(), Align2::CENTER_CENTER, icon::CUBE_TRANSPARENT, egui::FontId::proportional(56.0), theme::FAINT);
    let body = egui::Rect::from_min_max(pos2(rect.left() + 20.0, hero.bottom() + 16.0), rect.max - vec2(20.0, 8.0));
    let mut child = ui.new_child(UiBuilder::new().max_rect(body));
    child.spacing_mut().item_spacing = vec2(8.0, 8.0);
    child.label(RichText::new("Empty scene").font(theme::bold(18.0)));
    child.add(egui::Label::new("A bare floor and nothing else. Place assets from the library, draw walls, add positioning devices, or describe what you want to the assistant.").wrap());
}

/// The sample's model, what it's for, and how opening it goes.
fn antenna_sample_detail(ui: &mut egui::Ui, rect: egui::Rect, index: usize) {
    let s = &antenna_samples::all()[index];
    let img = egui::Rect::from_min_size(rect.min, vec2(rect.width(), (rect.width() * 0.48).min(rect.height() * 0.46)));
    #[cfg(feature = "antenna-studio")]
    crate::antenna::ui::samples::thumbnail(ui, img, index);
    #[cfg(not(feature = "antenna-studio"))]
    ui.painter().rect_filled(img, 0.0, theme::BG);
    ui.painter().hline(img.x_range(), img.bottom(), Stroke::new(1.0, theme::STROKE));
    let body = egui::Rect::from_min_max(pos2(rect.left() + 20.0, img.bottom() + 16.0), rect.max - vec2(20.0, 8.0));
    let mut child = ui.new_child(UiBuilder::new().max_rect(body));
    child.spacing_mut().item_spacing = vec2(6.0, 8.0);
    child.label(RichText::new(s.title).font(theme::bold(18.0)));
    child.add(egui::Label::new(s.about).wrap());
    #[cfg(feature = "antenna-studio")]
    child.add(egui::Label::new(RichText::new(format!("{} {}", icon::CHECK_CIRCLE, crate::antenna::ui::samples::opening_words(s))).color(theme::SUCCESS)).wrap());
    child.add_space(2.0);
    child.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for t in [s.group.label().to_string(), s.band_text(), "Simulated".to_string()] {
            theme::tag_chip(ui, &t);
        }
    });
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

/// "9 anchors · 3 tags" for an RTLS sample, a plant says so, else "120 objects".
fn contents(index: usize) -> String {
    // A plant's parts are generated when it loads, so there is nothing to count yet.
    if SAMPLES[index].plant.is_some() {
        return "Connected system, ready to run".to_string();
    }
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

/// Rendered preview (or hero thumbnail / icon), title, blurb, contents, chips.
fn sample_detail(ui: &mut egui::Ui, rect: egui::Rect, library: &mut Library, index: usize) {
    let s = &SAMPLES[index];
    let (on_disk, total) = samples::available(s, library);
    let shot = preview(ui.ctx(), index, s);
    let hero_entry = if shot.is_none() { library.entries.iter().position(|e| e.folder == s.hero) } else { None };
    if let Some(i) = hero_entry {
        library.want_thumb(i);
    }
    let thumb = hero_entry.and_then(|i| library.entries[i].thumb.clone());

    let img = egui::Rect::from_min_size(rect.min, vec2(rect.width(), (rect.width() * 0.48).min(rect.height() * 0.46)));
    let p = ui.painter().clone();
    p.rect_filled(img, 0.0, theme::BG);
    if let Some(t) = &shot {
        // Cover the frame, cropping the picture's longer side.
        let [w, h] = t.size();
        let (fw, fh) = (img.width() / w as f32, img.height() / h as f32);
        let (u, v) = if fw > fh { (1.0, fh / fw) } else { (fw / fh, 1.0) };
        let uv = egui::Rect::from_center_size(pos2(0.5, 0.5), vec2(u, v));
        egui::Image::from_texture(t).uv(uv).paint_at(ui, img);
    } else if let Some(t) = thumb {
        // Fit the square thumbnail into the frame, centered.
        let side = img.height();
        let r = egui::Rect::from_center_size(img.center(), vec2(side, side));
        p.image(t.id(), r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
    } else {
        p.text(img.center(), Align2::CENTER_CENTER, s.icon, egui::FontId::proportional(44.0), theme::FAINT);
    }
    p.hline(img.x_range(), img.bottom(), Stroke::new(1.0, theme::STROKE));

    let body = egui::Rect::from_min_max(pos2(rect.left() + 20.0, img.bottom() + 16.0), rect.max - vec2(20.0, 8.0));
    let mut child = ui.new_child(UiBuilder::new().max_rect(body));
    child.spacing_mut().item_spacing = vec2(6.0, 8.0);
    child.label(RichText::new(s.title).font(theme::bold(18.0)));
    child.add(egui::Label::new(s.blurb).wrap());
    let (txt, color) = if on_disk == total {
        (format!("{} {}", icon::CHECK_CIRCLE, contents(index)), theme::SUCCESS)
    } else {
        (format!("{} {} · {on_disk} of {total} assets on disk, the rest download when it loads", icon::CLOUD_ARROW_DOWN, contents(index)), theme::ONLINE)
    };
    child.add(egui::Label::new(RichText::new(txt).color(color)).wrap());
    child.add_space(2.0);
    child.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for t in tags(index) {
            theme::tag_chip(ui, t);
        }
    });
}
