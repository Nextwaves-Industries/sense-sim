//! Look and feel: dark theme, Geist + Phosphor icon fonts, and small shared
//! widgets (icon buttons, toggle switches, segmented controls, setting rows).

use std::sync::Arc;

use bevy_egui::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout, Margin, Response, RichText, Sense, Shadow, Stroke, TextStyle,
    Ui, vec2,
};

pub const BG: Color32 = Color32::from_rgb(17, 19, 23);
pub const PANEL: Color32 = Color32::from_rgb(25, 27, 32);
pub const RAISED: Color32 = Color32::from_rgb(34, 37, 43);
pub const HOVER: Color32 = Color32::from_rgb(43, 47, 55);
pub const STROKE: Color32 = Color32::from_rgb(44, 48, 56);
pub const TEXT: Color32 = Color32::from_rgb(228, 230, 235);
pub const WEAK: Color32 = Color32::from_rgb(139, 145, 155);

pub const ACCENT: Color32 = Color32::from_rgb(255, 176, 32);
pub const ONLINE: Color32 = Color32::from_rgb(110, 170, 255);
pub const SUCCESS: Color32 = Color32::from_rgb(110, 210, 120);
pub const DANGER: Color32 = Color32::from_rgb(238, 98, 88);
pub const AXIS: [Color32; 3] = [Color32::from_rgb(232, 86, 86), Color32::from_rgb(110, 200, 90), Color32::from_rgb(80, 140, 240)];

pub const SEMIBOLD: &str = "semibold";

/// Phosphor (regular) codepoints. The font is a fallback of every family,
/// so these work inside any text: `format!("{} Save", icon::FLOPPY_DISK)`.
#[allow(dead_code)]
pub mod icon {
    pub const MAGNIFYING_GLASS: &str = "\u{e30c}";
    pub const X: &str = "\u{e4f6}";
    pub const FOLDER_OPEN: &str = "\u{e256}";
    pub const FLOPPY_DISK: &str = "\u{e248}";
    pub const FILE_ARROW_UP: &str = "\u{e61e}";
    pub const EXPORT: &str = "\u{eaf0}";
    pub const TRASH: &str = "\u{e4a6}";
    pub const ARROW_COUNTER_CLOCKWISE: &str = "\u{e038}";
    pub const ARROW_CLOCKWISE: &str = "\u{e036}";
    pub const COPY: &str = "\u{e1ca}";
    pub const CROSSHAIR: &str = "\u{e1d6}";
    pub const CUBE: &str = "\u{e1da}";
    pub const CUBE_FOCUS: &str = "\u{ed0a}";
    pub const STACK: &str = "\u{e466}";
    pub const DOWNLOAD_SIMPLE: &str = "\u{e20c}";
    pub const CLOUD_ARROW_DOWN: &str = "\u{e1ac}";
    pub const CLOUD: &str = "\u{e1aa}";
    pub const CHECK_CIRCLE: &str = "\u{e184}";
    pub const WARNING_CIRCLE: &str = "\u{e4e2}";
    pub const GEAR_SIX: &str = "\u{e272}";
    pub const SPARKLE: &str = "\u{e6a2}";
    pub const STOP: &str = "\u{e46c}";
    pub const PAPERCLIP: &str = "\u{e39a}";
    pub const IMAGE: &str = "\u{e2ca}";
    pub const PLUS: &str = "\u{e3d4}";
    pub const ARROWS_CLOCKWISE: &str = "\u{e094}";
    pub const ARROW_LINE_DOWN: &str = "\u{e05c}";
    pub const FRAME_CORNERS: &str = "\u{e626}";
    pub const GRID_FOUR: &str = "\u{e296}";
    pub const BOUNDING_BOX: &str = "\u{e6ce}";
    pub const BROADCAST: &str = "\u{e0f2}";
    pub const SUN: &str = "\u{e472}";
    pub const LIST_BULLETS: &str = "\u{e2f2}";
    pub const SQUARES_FOUR: &str = "\u{e464}";
    pub const SLIDERS_HORIZONTAL: &str = "\u{e434}";
    pub const KEYBOARD: &str = "\u{e2d8}";
    pub const CARET_DOWN: &str = "\u{e136}";
    pub const CARET_RIGHT: &str = "\u{e13a}";
    pub const ROBOT: &str = "\u{e762}";
    pub const WRENCH: &str = "\u{e5d4}";
    pub const CURSOR_CLICK: &str = "\u{e7c8}";
    pub const ARROW_UP: &str = "\u{e08e}";
    pub const CIRCLE_NOTCH: &str = "\u{eb44}";
    pub const HARD_DRIVES: &str = "\u{e2a0}";
    pub const PLUGS: &str = "\u{eb56}";
    pub const INFO: &str = "\u{e2ce}";
    pub const CHAT_CIRCLE_DOTS: &str = "\u{e16c}";
    pub const LINK: &str = "\u{e2e2}";
    pub const ARROW_SQUARE_OUT: &str = "\u{e5de}";
    pub const TREE_STRUCTURE: &str = "\u{e67c}";
    pub const DOOR_OPEN: &str = "\u{e7e6}";
    pub const SIGN_OUT: &str = "\u{e42a}";
    pub const NOTE_PENCIL: &str = "\u{e34c}";
    pub const GLOBE: &str = "\u{e288}";
    pub const GAUGE: &str = "\u{e628}";
    pub const CHECK: &str = "\u{e182}";
    pub const SELECTION_SLASH: &str = "\u{e69e}";
    pub const ARROW_U_UP_LEFT: &str = "\u{e08a}";
    pub const ARROW_U_UP_RIGHT: &str = "\u{e08c}";
    pub const WALL: &str = "\u{e688}";
    pub const PLAY: &str = "\u{e3d0}";
    pub const PAUSE: &str = "\u{e39e}";
    pub const DROP: &str = "\u{e210}";
    pub const GEAR: &str = "\u{e270}";
    pub const PLUG: &str = "\u{e946}";
}

/// Installs the fonts. Call once, then `apply_style` on a later frame
/// (the style names a font family that only exists once the fonts are loaded).
pub fn apply_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("Geist".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Geist-Regular.ttf"))));
    fonts.font_data.insert("GeistSemiBold".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Geist-SemiBold.ttf"))));
    fonts.font_data.insert(
        "Phosphor".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Phosphor.ttf")).tweak(egui::FontTweak {
            scale: 1.1,
            y_offset_factor: 0.08,
            ..Default::default()
        })),
    );
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    // Geist has no glyphs in the private-use range the icons use (Inter does), and
    // Phosphor has Latin letters for ligatures, so the text font must come first.
    prop.insert(0, "Geist".into());
    prop.insert(1, "Phosphor".into());
    fonts.families.entry(FontFamily::Monospace).or_default().push("Phosphor".into());
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let semi: Vec<String> = fallback.into_iter().map(|f| if f == "Geist" { "GeistSemiBold".to_string() } else { f }).collect();
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);
    ctx.set_fonts(fonts);
}

/// Dark theme, spacing and text styles.
pub fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let semibold = FontFamily::Name(SEMIBOLD.into());
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Heading, FontId::new(15.0, semibold)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(8.0, 4.0);
        s.interact_size = vec2(32.0, 24.0);
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(12);
        s.icon_spacing = 6.0;
        s.slider_rail_height = 4.0;
        s.combo_width = 140.0;
        s.scroll = egui::style::ScrollStyle::thin();

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, STROKE);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(120) };
        v.popup_shadow = Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(110) };
        v.faint_bg_color = Color32::from_rgb(30, 33, 38);
        v.extreme_bg_color = BG;
        v.text_edit_bg_color = Some(BG);
        v.code_bg_color = BG;
        v.hyperlink_color = ONLINE;
        v.warn_fg_color = ACCENT;
        v.error_fg_color = DANGER;
        v.selection.bg_fill = Color32::from_rgb(78, 60, 22);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = true;
        v.indent_has_left_vline = false;
        v.collapsing_header_frame = false;

        let r = CornerRadius::same(6);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, STROKE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = RAISED;
        w.inactive.weak_bg_fill = RAISED;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(62, 67, 77));
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = Color32::from_rgb(52, 57, 67);
        w.active.weak_bg_fill = Color32::from_rgb(52, 57, 67);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;
        w.open.bg_fill = HOVER;
        w.open.weak_bg_fill = HOVER;
        w.open.bg_stroke = Stroke::new(1.0, STROKE);
        w.open.fg_stroke = Stroke::new(1.0, TEXT);
        w.open.corner_radius = r;
    });
}

/// Square, frameless-until-hovered icon button.
pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> Response {
    icon_button_colored(ui, icon, tip, None, false)
}

pub fn icon_button_colored(ui: &mut Ui, icon: &str, tip: &str, color: Option<Color32>, selected: bool) -> Response {
    let mut text = RichText::new(icon).size(16.0);
    if let Some(c) = color {
        text = text.color(c);
    }
    let r = ui.add(egui::Button::new(text).frame_when_inactive(selected).selected(selected).min_size(vec2(28.0, 28.0)));
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// Icon button that flips a bool and shows it as selected while on.
pub fn icon_toggle(ui: &mut Ui, value: &mut bool, icon: &str, tip: &str) -> Response {
    let r = icon_button_colored(ui, icon, tip, None, *value);
    if r.clicked() {
        *value = !*value;
    }
    r
}

/// `icon  text` label for buttons and menu items.
pub fn label(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}

/// Width for menu entries: fill the menu, but never stretch a popup or area
/// (where the available width is the whole window) beyond a menu-like size.
fn menu_width(ui: &Ui) -> f32 {
    ui.available_width().min(ui.min_rect().width().max(200.0)).min(280.0)
}

/// Full-width, left-aligned menu entry with an icon.
pub fn menu_item(ui: &mut Ui, icon: &str, text: &str) -> Response {
    ui.add(egui::Button::new(label(icon, text)).frame_when_inactive(false).min_size(vec2(menu_width(ui), 26.0)))
}

pub fn menu_item_danger(ui: &mut Ui, icon: &str, text: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(label(icon, text)).color(DANGER)).frame_when_inactive(false).min_size(vec2(menu_width(ui), 26.0)))
}

/// Filled call-to-action button.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::from_rgb(28, 22, 8))).fill(ACCENT).min_size(vec2(0.0, 28.0)))
}

/// iOS-style switch.
pub fn toggle_switch(ui: &mut Ui, on: &mut bool) -> Response {
    let size = vec2(34.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let off_bg = if response.hovered() { HOVER } else { Color32::from_rgb(52, 56, 64) };
        let bg = lerp_color(off_bg, ACCENT, t);
        let r = rect.height() / 2.0;
        ui.painter().rect_filled(rect, r, bg);
        let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), r - 3.0, Color32::WHITE);
    }
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Segmented control. Returns true if the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new().fill(BG).corner_radius(7).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (v, text) in options {
                let sel = *value == *v;
                let text = if sel { RichText::new(*text).color(Color32::WHITE) } else { RichText::new(*text).color(WEAK) };
                let b = egui::Button::new(text).fill(if sel { HOVER } else { Color32::TRANSPARENT }).frame_when_inactive(sel).corner_radius(5);
                if ui.add(b).clicked() && !sel {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// Segmented control that spans the full width with equal segments; labels
/// truncate instead of pushing the parent (and the panel) wider.
pub fn segmented_fill<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new().fill(BG).corner_radius(7).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let n = options.len().max(1) as f32;
            let w = ((ui.available_width() - 2.0 * (n - 1.0)) / n).floor().max(1.0);
            let h = ui.spacing().interact_size.y;
            for (v, label) in options {
                let sel = *value == *v;
                let text = RichText::new(*label).color(if sel { Color32::WHITE } else { WEAK });
                let b = egui::Button::new(text).fill(if sel { HOVER } else { Color32::TRANSPARENT }).frame_when_inactive(sel).corner_radius(5).truncate();
                let r = ui.add_sized([w, h], b).on_hover_text(*label);
                if r.clicked() && !sel {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// Settings row: label and description on the left, control on the right.
pub fn setting_row(ui: &mut Ui, title: &str, desc: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let w = (ui.available_width() * 0.5).max(160.0);
        ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(w);
            ui.label(title);
            if !desc.is_empty() {
                ui.label(RichText::new(desc).small().color(WEAK));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
    ui.add_space(8.0);
}

/// Small uppercase caption above a group.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).small().color(WEAK).strong());
}

/// Panel title row with optional right-aligned actions.
pub fn panel_header(ui: &mut Ui, title: &str, actions: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        ui.label(RichText::new(title).text_style(TextStyle::Heading));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            actions(ui)
        });
    });
}

/// Static coloured pill, e.g. a status badge.
#[allow(dead_code)]
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)))
        .response
}

/// Rounded floating container for overlays on the viewport.
pub fn floating_frame() -> egui::Frame {
    egui::Frame::new().fill(PANEL.gamma_multiply(0.96)).stroke(Stroke::new(1.0, STROKE)).corner_radius(10).inner_margin(Margin::same(4)).shadow(Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(110),
    })
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::new().fill(RAISED).corner_radius(8).inner_margin(Margin::same(10))
}

/// Centered icon + title + explanation for empty lists.
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, body: &str) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon).size(30.0).color(WEAK));
        ui.add_space(4.0);
        ui.label(RichText::new(title).strong());
        ui.add(egui::Label::new(RichText::new(body).small().color(WEAK)).wrap());
    });
    ui.add_space(12.0);
}

/// Collapsible property section with a strong header.
pub fn section<R>(ui: &mut Ui, title: &str, default_open: bool, add: impl FnOnce(&mut Ui) -> R) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(default_open).show(ui, |ui| {
        ui.add_space(2.0);
        add(ui);
        ui.add_space(4.0);
    });
}

/// Thin horizontal divider spanning the panel.
pub fn divider(ui: &mut Ui) {
    ui.add_space(2.0);
    let rect = ui.available_rect_before_wrap();
    let y = rect.top();
    ui.painter().hline(rect.x_range(), y, Stroke::new(1.0, STROKE));
    ui.add_space(6.0);
}
