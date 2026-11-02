//! Look and feel: light engineering-workbench theme (white cards floating on a
//! pale grey canvas), Lato + Phosphor icon fonts, and the shared widgets every
//! screen is built from: project trees, ribbon buttons, field rows, pills, tag
//! chips and the modal shell.

use std::sync::Arc;

use bevy_egui::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout, Margin, Pos2, Rect, Response, RichText, Sense, Shadow, Stroke,
    StrokeKind, TextStyle, Ui, pos2, vec2,
};

/// Pale grey behind wells and placeholders.
pub const BG: Color32 = Color32::from_rgb(244, 245, 246);
pub const PANEL: Color32 = Color32::WHITE;
/// Quiet fill for cards and wells inside a white panel.
pub const RAISED: Color32 = Color32::from_rgb(246, 247, 249);
pub const HOVER: Color32 = Color32::from_rgb(238, 240, 243);
pub const STROKE: Color32 = Color32::from_rgb(224, 227, 231);
pub const STROKE_STRONG: Color32 = Color32::from_rgb(192, 197, 205);
/// The grey section bands of long lists.
pub const BAND: Color32 = Color32::from_rgb(233, 234, 236);
pub const TEXT: Color32 = Color32::from_rgb(38, 41, 46);
pub const WEAK: Color32 = Color32::from_rgb(108, 114, 124);
/// Tree guide lines, disabled icons.
pub const FAINT: Color32 = Color32::from_rgb(170, 175, 183);
/// The near-black of confirm buttons.
pub const INK: Color32 = Color32::from_rgb(28, 30, 34);

pub const ACCENT: Color32 = Color32::from_rgb(45, 110, 240);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(235, 242, 254);
pub const ON_ACCENT: Color32 = Color32::WHITE;
pub const ONLINE: Color32 = Color32::from_rgb(28, 105, 212);
pub const SUCCESS: Color32 = Color32::from_rgb(32, 155, 80);
pub const WARNING: Color32 = Color32::from_rgb(209, 120, 8);
pub const DANGER: Color32 = Color32::from_rgb(222, 56, 52);
pub const PURPLE: Color32 = Color32::from_rgb(131, 56, 196);
pub const AXIS: [Color32; 3] = [Color32::from_rgb(222, 62, 62), Color32::from_rgb(48, 160, 72), Color32::from_rgb(52, 110, 230)];

pub const STATUS_BAR: Color32 = Color32::from_rgb(38, 40, 44);
pub const STATUS_TEXT: Color32 = Color32::from_rgb(218, 221, 226);
pub const STATUS_WEAK: Color32 = Color32::from_rgb(150, 155, 163);
pub const BACKDROP: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 92);

/// The 3D view's clear colour, as sRGB floats: how the lit ground renders towards
/// the horizon, so the floor melts into the background without a visible edge.
pub const CANVAS_SRGB: [f32; 3] = [242.0 / 255.0, 243.0 / 255.0, 244.0 / 255.0];

pub const SEMIBOLD: &str = "semibold";

/// Phosphor (regular) codepoints. The font is a fallback of every family,
/// so these work inside any text: `format!("{} Save", icon::FLOPPY_DISK)`.
#[allow(dead_code)]
pub mod icon {
    pub const MAGNIFYING_GLASS: &str = "\u{e30c}";
    pub const X: &str = "\u{e4f6}";
    pub const FOLDER_OPEN: &str = "\u{e256}";
    pub const FOLDER_PLUS: &str = "\u{e258}";
    pub const FLOPPY_DISK: &str = "\u{e248}";
    pub const FILE_ARROW_UP: &str = "\u{e61e}";
    pub const FILE_TEXT: &str = "\u{e23a}";
    pub const FILE_DASHED: &str = "\u{e704}";
    pub const EXPORT: &str = "\u{eaf0}";
    pub const TRASH: &str = "\u{e4a6}";
    pub const ARROW_COUNTER_CLOCKWISE: &str = "\u{e038}";
    pub const ARROW_CLOCKWISE: &str = "\u{e036}";
    pub const COPY: &str = "\u{e1ca}";
    pub const CROSSHAIR: &str = "\u{e1d6}";
    pub const CUBE: &str = "\u{e1da}";
    pub const CUBE_FOCUS: &str = "\u{ed0a}";
    pub const CUBE_TRANSPARENT: &str = "\u{ec7c}";
    pub const STACK: &str = "\u{e466}";
    pub const DOWNLOAD_SIMPLE: &str = "\u{e20c}";
    pub const CLOUD_ARROW_DOWN: &str = "\u{e1ac}";
    pub const CLOUD: &str = "\u{e1aa}";
    pub const CHECK_CIRCLE: &str = "\u{e184}";
    pub const WARNING_CIRCLE: &str = "\u{e4e2}";
    pub const WARNING: &str = "\u{e4e0}";
    pub const GEAR_SIX: &str = "\u{e272}";
    pub const SPARKLE: &str = "\u{e6a2}";
    pub const STOP: &str = "\u{e46c}";
    pub const PAPERCLIP: &str = "\u{e39a}";
    pub const IMAGE: &str = "\u{e2ca}";
    pub const PLUS: &str = "\u{e3d4}";
    pub const PLUS_CIRCLE: &str = "\u{e3d6}";
    pub const MINUS: &str = "\u{e32a}";
    pub const ARROWS_CLOCKWISE: &str = "\u{e094}";
    pub const ARROW_LINE_DOWN: &str = "\u{e05c}";
    pub const ARROWS_OUT_CARDINAL: &str = "\u{e0a4}";
    pub const FRAME_CORNERS: &str = "\u{e626}";
    pub const GRID_FOUR: &str = "\u{e296}";
    pub const BOUNDING_BOX: &str = "\u{e6ce}";
    pub const BROADCAST: &str = "\u{e0f2}";
    pub const SUN: &str = "\u{e472}";
    pub const LIST_BULLETS: &str = "\u{e2f2}";
    pub const LIST_CHECKS: &str = "\u{eadc}";
    pub const SQUARES_FOUR: &str = "\u{e464}";
    pub const SLIDERS_HORIZONTAL: &str = "\u{e434}";
    pub const SLIDERS: &str = "\u{e432}";
    pub const KEYBOARD: &str = "\u{e2d8}";
    pub const CARET_DOWN: &str = "\u{e136}";
    pub const CARET_RIGHT: &str = "\u{e13a}";
    pub const CARET_UP: &str = "\u{e13c}";
    pub const CARET_LEFT: &str = "\u{e138}";
    pub const ROBOT: &str = "\u{e762}";
    pub const WRENCH: &str = "\u{e5d4}";
    pub const CURSOR: &str = "\u{e1dc}";
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
    pub const PLAY_CIRCLE: &str = "\u{e3d2}";
    pub const PAUSE: &str = "\u{e39e}";
    pub const DROP: &str = "\u{e210}";
    pub const GEAR: &str = "\u{e270}";
    pub const PLUG: &str = "\u{e946}";
    pub const HOUSE: &str = "\u{e2c2}";
    pub const QUESTION: &str = "\u{e3e8}";
    pub const EYE: &str = "\u{e220}";
    pub const EYE_SLASH: &str = "\u{e224}";
    pub const LOCK: &str = "\u{e2fa}";
    pub const LOCK_OPEN: &str = "\u{e306}";
    pub const LIGHTNING: &str = "\u{e2de}";
    pub const CHART_LINE: &str = "\u{e154}";
    pub const CHART_LINE_UP: &str = "\u{e156}";
    pub const MAP_TRIFOLD: &str = "\u{e31a}";
    pub const WAVE_SINE: &str = "\u{ea9a}";
    pub const FLOW_ARROW: &str = "\u{e6ec}";
    pub const FADERS: &str = "\u{e228}";
    pub const PACKAGE: &str = "\u{e390}";
    pub const PULSE: &str = "\u{e000}";
    pub const TARGET: &str = "\u{e47c}";
    pub const ROCKET_LAUNCH: &str = "\u{e3fe}";
    pub const MAGNET: &str = "\u{e680}";
    pub const SHARE_NETWORK: &str = "\u{e408}";
}

/// Installs the fonts. Call once, then `apply_style` on a later frame
/// (the style names a font family that only exists once the fonts are loaded).
pub fn apply_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("Lato".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Lato-Regular.ttf"))));
    fonts.font_data.insert("LatoBold".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Lato-Bold.ttf"))));
    fonts.font_data.insert(
        "Phosphor".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Phosphor.ttf")).tweak(egui::FontTweak {
            scale: 1.1,
            y_offset_factor: 0.08,
            ..Default::default()
        })),
    );
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    // Phosphor has Latin letters for ligatures, so the text font must come first
    // (Lato has no glyphs in the private-use range the icons use).
    prop.insert(0, "Lato".into());
    prop.insert(1, "Phosphor".into());
    fonts.families.entry(FontFamily::Monospace).or_default().push("Phosphor".into());
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let semi: Vec<String> = fallback.into_iter().map(|f| if f == "Lato" { "LatoBold".to_string() } else { f }).collect();
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);
    ctx.set_fonts(fonts);
}

/// Bold variant of the UI font at a given size.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Light theme, spacing and text styles.
pub fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.style_mut_of(egui::Theme::Light, |style| {
        let semibold = FontFamily::Name(SEMIBOLD.into());
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, FontId::new(16.0, semibold)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(10.0, 4.0);
        s.interact_size = vec2(32.0, 26.0);
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(12);
        s.icon_spacing = 6.0;
        s.slider_rail_height = 4.0;
        s.combo_width = 140.0;
        s.scroll = egui::style::ScrollStyle::thin();
        s.scroll.fade.strength = 0.0;

        let v = &mut style.visuals;
        v.dark_mode = false;
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, STROKE);
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(6);
        v.window_shadow = Shadow { offset: [0, 6], blur: 22, spread: 0, color: Color32::from_black_alpha(40) };
        v.popup_shadow = Shadow { offset: [0, 3], blur: 12, spread: 0, color: Color32::from_black_alpha(36) };
        v.faint_bg_color = Color32::from_rgb(248, 249, 250);
        v.extreme_bg_color = PANEL;
        v.text_edit_bg_color = Some(PANEL);
        v.code_bg_color = RAISED;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = WARNING;
        v.error_fg_color = DANGER;
        v.weak_text_color = Some(WEAK);
        v.selection.bg_fill = Color32::from_rgb(214, 228, 253);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.indent_has_left_vline = false;
        v.collapsing_header_frame = false;

        let r = CornerRadius::same(5);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, STROKE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;
        // Buttons are outlined, as are text fields and combo boxes; `bg_fill` is
        // what sliders use for their rail and check boxes for their box.
        w.inactive.bg_fill = Color32::from_rgb(226, 229, 234);
        w.inactive.weak_bg_fill = PANEL;
        w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(210, 214, 220));
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = Color32::from_rgb(214, 218, 225);
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, STROKE_STRONG);
        w.hovered.fg_stroke = Stroke::new(1.0, INK);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = Color32::from_rgb(205, 210, 218);
        w.active.weak_bg_fill = Color32::from_rgb(228, 232, 238);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0, INK);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;
        w.open.bg_fill = HOVER;
        w.open.weak_bg_fill = HOVER;
        w.open.bg_stroke = Stroke::new(1.0, STROKE_STRONG);
        w.open.fg_stroke = Stroke::new(1.0, TEXT);
        w.open.corner_radius = r;
    });
}

/// Soft shadow under floating cards.
fn card_shadow() -> Shadow {
    Shadow { offset: [0, 1], blur: 8, spread: 0, color: Color32::from_black_alpha(26) }
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

/// Menu content that isn't an entry (a title, captions, sliders), lined up with
/// the entries: an inactive button hides its frame but keeps the frame's margin.
pub fn menu_section(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let w = &ui.visuals().widgets.inactive;
    let left = (ui.spacing().button_padding.x + w.expansion - w.bg_stroke.width) as i8;
    egui::Frame::new().inner_margin(Margin { left, ..Margin::ZERO }).show(ui, add);
}

/// Menu content that greys out while `enabled` is false, at the menu's full width.
pub fn menu_enabled(ui: &mut Ui, enabled: bool, add: impl FnOnce(&mut Ui)) {
    let width = ui.min_rect().width();
    ui.add_enabled_ui(enabled, |ui| {
        ui.set_min_width(width);
        add(ui)
    });
}

/// Menu row with small buttons on the right instead of a single action. Add them
/// with `menu_row_button`, rightmost first.
pub fn menu_row(ui: &mut Ui, icon: &str, text: &str, buttons: impl FnOnce(&mut Ui)) {
    menu_section(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(label(icon, text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                buttons(ui)
            });
        });
    });
}

/// Icon button for a `menu_row`, no taller than the entries around it.
pub fn menu_row_button(ui: &mut Ui, icon: &str, tip: &str) -> Response {
    ui.spacing_mut().button_padding = vec2(4.0, 2.0);
    ui.add(egui::Button::new(RichText::new(icon).size(16.0)).frame_when_inactive(false).min_size(vec2(26.0, 26.0))).on_hover_text(tip)
}

/// Filled call-to-action button.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(text).color(ON_ACCENT)).fill(ACCENT).stroke(Stroke::NONE).min_size(vec2(0.0, 30.0)))
}

/// Large call-to-action; grey and inert while `enabled` is false.
pub fn cta_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let (fill, sense) = if enabled { (ACCENT, Sense::click()) } else { (Color32::from_rgb(198, 201, 206), Sense::hover()) };
    let b = egui::Button::new(RichText::new(text).size(14.5).color(ON_ACCENT)).fill(fill).stroke(Stroke::NONE).min_size(vec2(136.0, 36.0)).sense(sense);
    let r = ui.add(b);
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// Bordered square button with an X, top right of cards and dialogs.
pub fn close_button(ui: &mut Ui, tip: &str) -> Response {
    square_button(ui, icon::X, tip, PANEL, TEXT, Stroke::new(1.0, Color32::from_rgb(214, 218, 224)))
}

/// Near-black square button with a check mark: "done".
pub fn confirm_button(ui: &mut Ui, tip: &str) -> Response {
    square_button(ui, icon::CHECK, tip, INK, Color32::WHITE, Stroke::NONE)
}

fn square_button(ui: &mut Ui, glyph: &str, tip: &str, fill: Color32, color: Color32, stroke: Stroke) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let p = ui.painter();
    let fill = if r.hovered() && fill == PANEL { HOVER } else { fill };
    p.rect(rect, 5.0, fill, stroke, StrokeKind::Inside);
    p.text(rect.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(15.0), color);
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// Pill-shaped switch: outlined while off, accent while on.
pub fn toggle_switch(ui: &mut Ui, on: &mut bool) -> Response {
    let size = vec2(34.0, 18.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let bg = lerp_color(PANEL, ACCENT, t);
        let border = lerp_color(if response.hovered() { STROKE_STRONG } else { Color32::from_rgb(208, 212, 218) }, ACCENT, t);
        let r = rect.height() / 2.0;
        ui.painter().rect(rect, r, bg, Stroke::new(1.0, border), StrokeKind::Inside);
        let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
        let knob = lerp_color(Color32::from_rgb(214, 217, 222), Color32::WHITE, t);
        ui.painter().circle(pos2(x, rect.center().y), r - 2.5, knob, Stroke::new(1.0, lerp_color(Color32::from_rgb(190, 195, 202), Color32::WHITE, t)));
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Segmented control. Returns true if the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new().fill(HOVER).corner_radius(6).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // Inside a right-aligned row the first widget lands rightmost.
            let flip = ui.layout().prefer_right_to_left();
            let order: Vec<&(T, &str)> = if flip { options.iter().rev().collect() } else { options.iter().collect() };
            for (v, text) in order {
                let sel = *value == *v;
                let text = if sel { RichText::new(*text).color(TEXT) } else { RichText::new(*text).color(WEAK) };
                let b = egui::Button::new(text).fill(if sel { PANEL } else { Color32::TRANSPARENT }).frame_when_inactive(sel).corner_radius(4);
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
    egui::Frame::new().fill(HOVER).corner_radius(6).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.spacing_mut().button_padding.x = 4.0;
            let n = options.len().max(1) as f32;
            let w = ((ui.available_width() - 2.0 * (n - 1.0)) / n).floor().max(1.0);
            let h = ui.spacing().interact_size.y;
            let flip = ui.layout().prefer_right_to_left();
            let order: Vec<&(T, &str)> = if flip { options.iter().rev().collect() } else { options.iter().collect() };
            for (v, label) in order {
                let sel = *value == *v;
                let text = RichText::new(*label).color(if sel { TEXT } else { WEAK });
                let b = egui::Button::new(text).fill(if sel { PANEL } else { Color32::TRANSPARENT }).frame_when_inactive(sel).corner_radius(4).truncate();
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
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(title);
            if !desc.is_empty() {
                ui.label(RichText::new(desc).small().color(WEAK));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
    ui.add_space(8.0);
}

pub const FIELD_LABEL_W: f32 = 70.0;

/// Property row of a card: label on the left, value or control on the right.
pub fn field_row(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        let w = FIELD_LABEL_W.min(ui.available_width() * 0.42);
        ui.allocate_ui_with_layout(vec2(w, 26.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_width(w);
            ui.add(egui::Label::new(title).truncate());
        });
        add(ui);
    });
}

/// Small uppercase caption above a group.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).font(bold(11.5)).color(WEAK));
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

/// Title row of a floating detail card: title, then actions on the right.
pub fn card_title(ui: &mut Ui, title: &str, actions: impl FnOnce(&mut Ui)) {
    egui::Frame::new().inner_margin(Margin { left: 14, right: 10, top: 9, bottom: 9 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.add(egui::Label::new(RichText::new(title).font(bold(16.0)).color(TEXT)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                actions(ui)
            });
        });
    });
    hairline(ui);
}

/// Static coloured pill, e.g. a status badge.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)))
        .response
}

/// Solid pill with small bold capitals ("SPECIAL").
pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let font = bold(10.0);
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), font, Color32::WHITE);
    let (rect, r) = ui.allocate_exact_size(galley.size() + vec2(12.0, 5.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, rect.height() / 2.0, color);
    p.galley(rect.min + vec2(6.0, 2.5), galley, Color32::WHITE);
    r
}

/// Grey luggage-tag chip for keywords, as seen on the detail page of a list item.
pub fn tag_chip(ui: &mut Ui, text: &str) -> Response {
    let font = FontId::proportional(11.0);
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), font, TEXT);
    let size = galley.size() + vec2(24.0, 10.0);
    let (rect, r) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    let (l, t, rr, b, mid) = (rect.left(), rect.top(), rect.right(), rect.bottom(), rect.center().y);
    let fill = Color32::from_rgb(229, 230, 233);
    p.add(egui::Shape::convex_polygon(vec![pos2(l + 8.0, t), pos2(rr, t), pos2(rr, b), pos2(l + 8.0, b), pos2(l, mid)], fill, Stroke::NONE));
    p.circle_filled(pos2(l + 8.0, mid), 2.0, PANEL);
    p.galley(pos2(l + 16.0, t + 5.0), galley, TEXT);
    r
}

/// Rounded floating container for cards over the viewport.
pub fn floating_frame() -> egui::Frame {
    egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, Color32::from_rgb(232, 234, 237))).corner_radius(4).inner_margin(Margin::same(4)).shadow(card_shadow())
}

/// Floating card of a fixed width. A row that overflows is clipped instead of
/// growing the card, so a long label can't push it wider every frame.
/// `max_height`: how tall it may get. An `Area` only offers its content the size
/// it had last frame, so a scroll area inside would never grow without this.
pub fn fixed_card<R>(ui: &mut Ui, width: f32, max_height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let bg = ui.painter().add(egui::Shape::Noop);
    let min = ui.next_widget_position();
    let inner = ui.scope(|ui| {
        ui.set_width(width);
        ui.set_max_height(max_height);
        ui.set_clip_rect(Rect::from_min_size(min, vec2(width, f32::INFINITY)).intersect(ui.clip_rect()));
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        add(ui)
    });
    let rect = Rect::from_min_size(min, vec2(width, inner.response.rect.height()));
    ui.painter().set(bg, floating_frame().inner_margin(Margin::ZERO).paint(rect));
    inner.inner
}

/// A quiet group inside a card.
pub fn card_frame() -> egui::Frame {
    egui::Frame::new().fill(RAISED).corner_radius(5).inner_margin(Margin::same(10))
}

/// The dialog frame: square-ish white sheet, no inner margin.
pub fn modal_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, Color32::from_rgb(214, 217, 222)))
        .corner_radius(3)
        .inner_margin(Margin::ZERO)
        .shadow(Shadow { offset: [0, 10], blur: 34, spread: 0, color: Color32::from_black_alpha(60) })
}

/// Title strip of a dialog; returns true when its close button was clicked.
pub fn modal_title(ui: &mut Ui, width: f32, title: &str, closable: bool, tip: &str) -> bool {
    let mut close = false;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
    ui.painter().text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, title, bold(16.0), TEXT);
    if closable {
        let b = Rect::from_center_size(rect.right_center() - vec2(30.0, 0.0), vec2(30.0, 30.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(b));
        close = close_button(&mut child, tip).clicked();
    }
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, STROKE));
    close
}

/// Entry of a dialog's left-hand category list.
pub fn nav_item(ui: &mut Ui, glyph: &str, text: &str, selected: bool, width: f32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(width, 34.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 0.0, ACCENT_SOFT);
    } else if r.hovered() {
        p.rect_filled(rect, 0.0, RAISED);
    }
    let col = if selected { ACCENT } else { TEXT };
    let mut x = rect.left() + 14.0;
    if !glyph.is_empty() {
        p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, glyph, FontId::proportional(15.0), if selected { ACCENT } else { WEAK });
        x += 24.0;
    }
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(14.0), col);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Grey section band of a long list.
pub fn band(ui: &mut Ui, text: &str, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, BAND);
    ui.painter().text(rect.left_center() + vec2(9.0, 0.0), Align2::LEFT_CENTER, text.to_uppercase(), bold(12.0), TEXT);
}

/// Selectable list row with a coloured dot; a long name is cut with an ellipsis.
pub fn color_row(ui: &mut Ui, width: f32, color: Color32, text: &str, selected: bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 3.0, ACCENT_SOFT);
    } else if r.hovered() {
        p.rect_filled(rect, 3.0, RAISED);
    }
    p.circle_filled(rect.left_center() + vec2(9.0, 0.0), 4.0, color);
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), FontId::proportional(14.0), TEXT);
    job.wrap = egui::text::TextWrapping { max_width: (width - 28.0).max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = p.layout_job(job);
    p.galley(pos2(rect.left() + 22.0, rect.center().y - g.size().y / 2.0), g, TEXT);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Thin full-width line (no spacing).
pub fn hairline(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, STROKE));
}

/// Centered icon + title + explanation for empty lists.
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, body: &str) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon).size(30.0).color(FAINT));
        ui.add_space(4.0);
        ui.label(RichText::new(title).strong());
        ui.add(egui::Label::new(RichText::new(body).small().color(WEAK)).wrap());
    });
    ui.add_space(12.0);
}

/// Collapsible property section with a strong header.
pub fn section<R>(ui: &mut Ui, title: &str, default_open: bool, add: impl FnOnce(&mut Ui) -> R) {
    egui::CollapsingHeader::new(RichText::new(title).font(bold(13.5))).default_open(default_open).show(ui, |ui| {
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

// ---------------------------------------------------------------------------
// Project trees

/// Progress of a tree node.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dot {
    /// Nothing drawn.
    #[default]
    None,
    /// Green disc with a check: set up.
    Done,
    /// Red ring: needs something from you.
    Todo,
    /// Blue ring: optional, nothing required.
    Optional,
    /// Amber ring: working.
    Busy,
}

/// Paints a status marker centred on `c`.
pub fn paint_dot(p: &egui::Painter, c: Pos2, dot: Dot) {
    let r = 5.5;
    match dot {
        Dot::None => {}
        Dot::Done => {
            p.circle_filled(c, r + 0.5, SUCCESS);
            let s = Stroke::new(1.5, Color32::WHITE);
            p.line_segment([c + vec2(-2.8, 0.2), c + vec2(-0.8, 2.2)], s);
            p.line_segment([c + vec2(-0.8, 2.2), c + vec2(2.9, -2.0)], s);
        }
        Dot::Todo => {
            p.circle_stroke(c, r - 0.3, Stroke::new(2.4, DANGER));
        }
        Dot::Optional => {
            p.circle_stroke(c, r - 0.3, Stroke::new(2.4, Color32::from_rgb(30, 110, 170)));
        }
        Dot::Busy => {
            p.circle_stroke(c, r - 0.3, Stroke::new(2.4, WARNING));
        }
    }
}

/// Card section header: chevron, coloured icon, CAPITAL TITLE, then optional
/// right-aligned controls. Flips `open` when the row is clicked.
pub fn section_header(ui: &mut Ui, id: &str, open: &mut bool, glyph: &str, glyph_color: Color32, title: &str, trailing: impl FnOnce(&mut Ui)) -> Response {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 38.0), Sense::hover());
    // Trailing controls take clicks first, the rest of the row toggles.
    let row = ui.interact(rect, ui.id().with(id), Sense::click());
    if row.hovered() {
        ui.painter().rect_filled(rect, 0.0, RAISED);
    }
    let p = ui.painter();
    let chevron = if *open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    p.text(rect.left_center() + vec2(18.0, 0.0), Align2::CENTER_CENTER, chevron, FontId::proportional(13.0), TEXT);
    let mut x = rect.left() + 32.0;
    if !glyph.is_empty() {
        p.text(pos2(x + 8.0, rect.center().y), Align2::CENTER_CENTER, glyph, FontId::proportional(16.0), glyph_color);
        x += 24.0;
    }
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, title.to_uppercase(), bold(13.5), TEXT);
    let controls = Rect::from_min_max(rect.center_top(), rect.right_bottom() - vec2(8.0, 0.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(Layout::right_to_left(Align::Center)));
    child.spacing_mut().item_spacing.x = 2.0;
    trailing(&mut child);
    if row.clicked() {
        *open = !*open;
    }
    row
}

/// The small round ⊕ of a tree row or section header.
pub fn add_button(ui: &mut Ui, tip: &str) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    let col = if r.hovered() { TEXT } else { WEAK };
    let c = rect.center();
    ui.painter().circle(c, 7.5, if r.hovered() { HOVER } else { PANEL }, Stroke::new(1.0, Color32::from_rgb(198, 202, 208)), );
    ui.painter().hline((c.x - 3.5)..=(c.x + 3.5), c.y, Stroke::new(1.2, col));
    ui.painter().vline(c.x, (c.y - 3.5)..=(c.y + 3.5), Stroke::new(1.2, col));
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// One row of a project tree.
pub struct TreeRow<'a> {
    /// 0 = root level.
    pub depth: u8,
    pub text: &'a str,
    pub glyph: &'a str,
    pub dot: Dot,
    pub selected: bool,
    /// Expand box in front of the row: `Some(open)` when it has children.
    pub expander: Option<bool>,
    /// Draw the tree guide into this row.
    pub guide: bool,
    /// Last child at its depth: the guide stops halfway.
    pub last: bool,
    /// Text after the name, in grey.
    pub note: &'a str,
    /// Grey text: the thing is hidden or off.
    pub dim: bool,
}

/// What was clicked on a tree row.
#[derive(Default, Clone, Copy)]
pub struct TreeClick {
    pub row: bool,
    pub expander: bool,
}

pub const TREE_ROW_H: f32 = 30.0;

/// Draws a row across `width` and leaves the area to the right free for the
/// caller's own controls. Returns the row's response and rect.
pub fn tree_row(ui: &mut Ui, width: f32, row: &TreeRow) -> (TreeClick, Response) {
    let (rect, r) = ui.allocate_exact_size(vec2(width, TREE_ROW_H), Sense::click());
    let mut click = TreeClick::default();
    let p = ui.painter().clone();
    if row.selected {
        p.rect_filled(rect, 0.0, ACCENT_SOFT);
    } else if r.hovered() {
        p.rect_filled(rect, 0.0, RAISED);
    }
    let cy = rect.center().y;
    let indent = 18.0f32;
    let base = rect.left() + 18.0 + indent * row.depth as f32;
    if row.guide && row.depth > 0 {
        let gx = base - indent + 8.0;
        let s = Stroke::new(1.0, Color32::from_rgb(204, 208, 214));
        let bottom = if row.last { cy } else { rect.bottom() };
        p.vline(gx, rect.top()..=bottom, s);
        p.hline(gx..=(base - 2.0), cy, s);
    }
    let mut x = base;
    if let Some(open) = row.expander {
        let b = Rect::from_center_size(pos2(x, cy), vec2(11.0, 11.0));
        let hit = ui.interact(b.expand(4.0), r.id.with("exp"), Sense::click());
        p.rect(b, 1.5, PANEL, Stroke::new(1.0, if hit.hovered() { WEAK } else { Color32::from_rgb(180, 185, 192) }), StrokeKind::Inside);
        p.hline((b.left() + 2.5)..=(b.right() - 2.5), cy, Stroke::new(1.0, TEXT));
        if !open {
            p.vline(b.center().x, (b.top() + 2.5)..=(b.bottom() - 2.5), Stroke::new(1.0, TEXT));
        }
        click.expander = hit.clicked();
        x += 14.0;
    }
    if row.dot != Dot::None {
        paint_dot(&p, pos2(x + 7.0, cy), row.dot);
        x += 21.0;
    }
    if !row.glyph.is_empty() {
        p.text(pos2(x + 7.0, cy), Align2::CENTER_CENTER, row.glyph, FontId::proportional(15.0), if row.dim { FAINT } else { WEAK });
        x += 21.0;
    }
    let note = (!row.note.is_empty()).then(|| p.layout_no_wrap(row.note.to_string(), FontId::proportional(12.0), WEAK));
    let note_w = note.as_ref().map_or(0.0, |g| g.size().x + 8.0);
    let max = (rect.right() - x - 6.0 - note_w).max(10.0);
    let color = if row.dim { FAINT } else { TEXT };
    let mut job = egui::text::LayoutJob::simple_singleline(row.text.to_string(), FontId::proportional(14.0), color);
    job.wrap = egui::text::TextWrapping { max_width: max, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = p.layout_job(job);
    let tw = g.size().x;
    p.galley(pos2(x, cy - g.size().y / 2.0), g, color);
    if let Some(g) = note {
        p.galley(pos2(x + tw + 8.0, cy - g.size().y / 2.0), g, WEAK);
    }
    click.row = r.clicked() && !click.expander;
    (click, r)
}

// ---------------------------------------------------------------------------
// Ribbon

/// Icon over a small label, as in the tool ribbon.
pub fn ribbon_button(ui: &mut Ui, glyph: &str, text: &str, enabled: bool, selected: bool, tip: &str) -> Response {
    let font = FontId::proportional(11.5);
    let width = ui.painter().layout_no_wrap(text.to_string(), font.clone(), TEXT).size().x + 14.0;
    let (rect, r) = ui.allocate_exact_size(vec2(width.max(46.0), 50.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if selected {
        p.rect(rect, 5.0, ACCENT_SOFT, Stroke::new(1.0, Color32::from_rgb(190, 212, 250)), StrokeKind::Inside);
    } else if enabled && r.hovered() {
        p.rect_filled(rect, 5.0, HOVER);
    }
    let (ic, tx) = if enabled { (ACCENT, TEXT) } else { (FAINT, FAINT) };
    p.text(rect.center_top() + vec2(0.0, 18.0), Align2::CENTER_CENTER, glyph, FontId::proportional(22.0), ic);
    p.text(rect.center_bottom() - vec2(0.0, 9.0), Align2::CENTER_CENTER, text, font, tx);
    let r = if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// A tab of the ribbon: its name, underlined in blue while it shows.
pub fn ribbon_tab(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let font = if selected { bold(13.0) } else { FontId::proportional(13.0) };
    let color = if selected { ACCENT } else { TEXT };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let (rect, r) = ui.allocate_exact_size(vec2(galley.size().x + 24.0, ui.available_height()), Sense::click());
    let p = ui.painter();
    if !selected && r.hovered() {
        p.rect_filled(rect.shrink2(vec2(0.0, 2.0)), 4.0, HOVER);
    }
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
    if selected {
        p.hline(rect.shrink2(vec2(6.0, 0.0)).x_range(), rect.bottom() - 1.0, Stroke::new(2.0, ACCENT));
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A workbench in the ribbon's tab row: blue while it is the one in front, and
/// underlined while its tools show. `dot` flags something going on inside it.
pub fn workbench_tab(ui: &mut Ui, glyph: &str, text: &str, current: bool, showing: bool, dot: Option<Color32>) -> Response {
    let font = if current { bold(13.0) } else { FontId::proportional(13.0) };
    let color = if current { ACCENT } else { TEXT };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let (rect, r) = ui.allocate_exact_size(vec2(galley.size().x + 44.0, ui.available_height()), Sense::click());
    let p = ui.painter();
    if current && !showing {
        p.rect_filled(rect.shrink2(vec2(0.0, 2.0)), 4.0, ACCENT_SOFT);
    } else if !current && r.hovered() {
        p.rect_filled(rect.shrink2(vec2(0.0, 2.0)), 4.0, HOVER);
    }
    p.text(rect.left_center() + vec2(19.0, 0.0), Align2::CENTER_CENTER, glyph, FontId::proportional(15.0), if current { ACCENT } else { WEAK });
    let y = rect.center().y - galley.size().y / 2.0;
    p.galley(pos2(rect.left() + 31.0, y), galley, color);
    if let Some(c) = dot {
        p.circle_filled(rect.left_center() + vec2(26.0, -6.0), 3.5, c);
    }
    if showing {
        p.hline(rect.shrink2(vec2(6.0, 0.0)).x_range(), rect.bottom() - 1.0, Stroke::new(2.0, ACCENT));
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Grey capital label in front of a group of ribbon buttons.
pub fn ribbon_caption(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text.to_uppercase()).size(11.5).color(WEAK));
    ui.add_space(2.0);
}

// ---------------------------------------------------------------------------
// Small painter helpers

/// Soft white chip behind a label drawn over the 3D view.
pub fn viewport_chip(p: &egui::Painter, rect: Rect) {
    p.rect(rect, 3.0, Color32::from_white_alpha(235), Stroke::new(1.0, Color32::from_rgb(214, 217, 222)), StrokeKind::Inside);
}

/// A device or chart colour dark enough to read on white.
pub fn ink(c: Color32) -> Color32 {
    let lum = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    let k = if lum > 120.0 { 120.0 / lum } else { 1.0 };
    Color32::from_rgb((c.r() as f32 * k) as u8, (c.g() as f32 * k) as u8, (c.b() as f32 * k) as u8)
}

/// Rounded outline whose colour runs from `a` (left) to `b` (right).
pub fn gradient_outline(p: &egui::Painter, rect: Rect, radius: f32, a: Color32, b: Color32) {
    let steps = 14;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let clip = Rect::from_min_max(pos2(egui::lerp(rect.left()..=rect.right(), t0), rect.top()), pos2(egui::lerp(rect.left()..=rect.right(), t1), rect.bottom())).expand2(vec2(0.0, 1.0));
        p.with_clip_rect(clip).rect_stroke(rect, radius, Stroke::new(1.0, lerp_color(a, b, (t0 + t1) / 2.0)), StrokeKind::Inside);
    }
}

/// The little `‖` grip beside a card that resizes it sideways. `sign` is +1
/// when dragging right widens the card, −1 when dragging left does.
pub fn width_grip(ctx: &egui::Context, id: &str, center: Pos2, width: &mut f32, range: std::ops::RangeInclusive<f32>, sign: f32) {
    egui::Area::new(egui::Id::new(("width_grip", id))).order(egui::Order::Middle).pivot(Align2::CENTER_CENTER).fixed_pos(center).show(ctx, |ui| {
        let (rect, r) = ui.allocate_exact_size(vec2(9.0, 34.0), Sense::drag());
        let col = if r.hovered() || r.dragged() { WEAK } else { Color32::from_rgb(196, 200, 206) };
        for dx in [-1.5, 1.5] {
            ui.painter().vline(rect.center().x + dx, (rect.center().y - 7.0)..=(rect.center().y + 7.0), Stroke::new(1.0, col));
        }
        if r.dragged() {
            *width = (*width + sign * r.drag_delta().x).clamp(*range.start(), *range.end());
        }
        r.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    });
}
