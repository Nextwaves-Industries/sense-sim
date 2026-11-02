//! The text field for a design-variable expression: variables, numbers, units and
//! functions in their own colours, an unknown name marked where it is, and a list of
//! what fits at the caret as you type. Up and Down move in the list, Enter or Tab take
//! the choice, Esc closes it, and Ctrl+Space opens it without typing. What each piece
//! is and what fits are worked out by [`antenna_core::expr`]; this only shows them.

use std::fmt::Debug;
use std::hash::Hash;

use antenna_core::expr::{self, Completions, TokenKind};
use antenna_core::project::{Project, VarHint};
use bevy_egui::egui::{self, Color32, FontId, Key, Modifiers, Order, Response, Sense, Stroke, TextEdit, TextFormat, vec2};

use crate::theme;

/// Rows of the list before it scrolls.
const LIST_HEIGHT: f32 = 236.0;
const LIST_MIN_WIDTH: f32 = 280.0;

/// The names expressions can use, read once per page rather than per field.
pub struct VarTable {
    hints: Vec<VarHint>,
    pairs: Vec<(String, String)>,
}

impl VarTable {
    pub fn of(p: &Project) -> Self {
        Self { hints: p.var_hints(), pairs: p.params.iter().map(|r| (r.name.clone(), r.expr.clone())).collect() }
    }

    fn has(&self, name: &str) -> bool {
        self.hints.iter().any(|h| h.name == name)
    }

    /// What a variable comes to, which it can do with another row half typed.
    pub fn value(&self, name: &str) -> Option<f64> {
        self.hints.iter().find(|h| h.name == name).and_then(|h| h.value)
    }

    /// What the row `own` can use: not itself, nor anything that leads back to it.
    fn usable(&self, own: Option<&str>) -> Vec<&str> {
        let loops = own.map(|n| expr::dependents(&self.pairs, n)).unwrap_or_default();
        self.hints.iter().map(|h| h.name.as_str()).filter(|n| Some(*n) != own && !loops.contains(*n)).collect()
    }
}

/// The list's state between frames.
#[derive(Clone, Copy, Default)]
struct List {
    open: bool,
    selected: usize,
    /// The pointer is on it: a press there takes focus from the field, and mustn't
    /// close the list before the click lands.
    hovered: bool,
}

fn color(kind: TokenKind) -> Color32 {
    match kind {
        TokenKind::Number => theme::TEXT,
        TokenKind::Variable => theme::ACCENT,
        TokenKind::Constant | TokenKind::Function => theme::PURPLE,
        TokenKind::Unit | TokenKind::Operator => theme::WEAK,
        TokenKind::Unknown => theme::DANGER,
    }
}

/// The expression in colour. `typing` is the caret when the field has focus: a word
/// still being typed isn't an error yet, and neither is a formula that isn't finished.
fn colored(src: &str, font: &FontId, table: &VarTable, own: Option<&str>, typing: Option<usize>) -> egui::text::LayoutJob {
    let plain = |c: Color32| TextFormat { font_id: font.clone(), color: c, ..Default::default() };
    let broken = typing.is_none() && !src.trim().is_empty() && expr::names(src).is_err();
    let mut job = egui::text::LayoutJob::default();
    let mut at = 0;
    for t in expr::tokens(src, |n| table.has(n) && Some(n) != own) {
        job.append(&src[at..t.start], 0.0, plain(theme::TEXT));
        let being_typed = typing.is_some_and(|c| t.start <= c && c <= t.end);
        let format = if t.kind == TokenKind::Unknown && !being_typed {
            TextFormat { underline: Stroke::new(1.0, theme::DANGER), ..plain(theme::DANGER) }
        } else if t.kind == TokenKind::Unknown {
            plain(theme::TEXT)
        } else if broken {
            plain(theme::DANGER)
        } else {
            plain(color(t.kind))
        };
        job.append(&src[t.start..t.end], 0.0, format);
        at = t.end;
    }
    job.append(&src[at..], 0.0, plain(theme::TEXT));
    job
}

fn byte_of(text: &str, char_index: impl Into<usize>) -> usize {
    text.char_indices().nth(char_index.into()).map_or(text.len(), |x| x.0)
}

fn caret(ctx: &egui::Context, id: egui::Id, text: &str) -> Option<usize> {
    let range = TextEdit::load_state(ctx, id)?.cursor.char_range()?;
    Some(byte_of(text, range.primary.index))
}

/// Puts the choice in the text and the caret after it.
fn accept(ctx: &egui::Context, id: egui::Id, text: &mut String, set: &Completions, item: usize) {
    let (new, at) = set.apply(text, item);
    let chars = new[..at].chars().count();
    *text = new;
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(chars))));
    state.store(ctx, id);
}

/// The label with the part already typed in bold.
fn label_job(label: &str, prefix: &str, base: Color32) -> egui::text::LayoutJob {
    let size = 13.5;
    let normal = TextFormat { font_id: FontId::proportional(size), color: base, ..Default::default() };
    let strong = TextFormat { font_id: theme::bold(size), color: base, ..Default::default() };
    let mut job = egui::text::LayoutJob::default();
    // Case folding can change a string's length, and then there's nothing to mark.
    let from = (!prefix.is_empty() && label.len() == label.to_lowercase().len()).then(|| label.to_lowercase().find(&prefix.to_lowercase())).flatten();
    match from {
        Some(a) if label.is_char_boundary(a) && label.is_char_boundary(a + prefix.len()) => {
            job.append(&label[..a], 0.0, normal.clone());
            job.append(&label[a..a + prefix.len()], 0.0, strong);
            job.append(&label[a + prefix.len()..], 0.0, normal);
        }
        _ => job.append(label, 0.0, normal),
    }
    job
}

/// The list under the field. Gives the item clicked, and whether the pointer is on it.
fn show_list(ctx: &egui::Context, id: egui::Id, field: egui::Rect, set: &Completions, table: &VarTable, selected: usize, reveal: bool) -> (Option<usize>, bool) {
    let mut clicked = None;
    let width = field.width().max(LIST_MIN_WIDTH);
    let area = egui::Area::new(id).order(Order::Foreground).fixed_pos(field.left_bottom() + vec2(0.0, 2.0)).constrain(true).show(ctx, |ui| {
        theme::floating_frame().inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            ui.set_width(width);
            egui::ScrollArea::vertical().max_height(LIST_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                for (k, item) in set.items.iter().enumerate() {
                    let hint = table.hints.iter().find(|h| h.name == item.label && item.kind == TokenKind::Variable);
                    let note = hint.map_or("", |h| h.note.as_str());
                    let height = if note.is_empty() { 26.0 } else { 40.0 };
                    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
                    if row.clicked() {
                        clicked = Some(k);
                    }
                    if k == selected && reveal {
                        row.scroll_to_me(None);
                    }
                    let painter = ui.painter_at(rect);
                    if k == selected {
                        painter.rect_filled(rect, 4.0, theme::ACCENT_SOFT);
                    } else if row.hovered() {
                        painter.rect_filled(rect, 4.0, theme::HOVER);
                    }
                    let inner = rect.shrink2(vec2(8.0, 4.0));
                    let galley = ui.fonts_mut(|f| f.layout_job(label_job(&item.label, &set.prefix, color(item.kind))));
                    painter.galley(inner.left_top(), galley, theme::TEXT);
                    let detail = match item.kind {
                        TokenKind::Variable => hint.and_then(|h| h.value).map_or(String::new(), |v| antenna_core::report::param_text(&item.label, v)),
                        TokenKind::Constant => "constant".to_string(),
                        TokenKind::Function => "function".to_string(),
                        TokenKind::Unit => "unit".to_string(),
                        _ => String::new(),
                    };
                    painter.text(inner.right_top(), egui::Align2::RIGHT_TOP, detail, FontId::proportional(12.5), theme::WEAK);
                    if !note.is_empty() {
                        painter.text(inner.left_top() + vec2(0.0, 18.0), egui::Align2::LEFT_TOP, note, FontId::proportional(12.0), theme::WEAK);
                    }
                }
            });
        });
    });
    (clicked, area.response.contains_pointer())
}

/// An expression field. `own` is the variable this expression defines, if it does.
/// `edit` makes the field itself, so a caller sets its size and whether it wraps.
pub fn expr_field(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, own: Option<&str>, edit: impl for<'t> FnOnce(&'t mut String) -> TextEdit<'t>) -> Response {
    let id = ui.make_persistent_id(salt);
    let list_id = id.with("list");
    let ctx = ui.ctx().clone();
    let mut list: List = ctx.data(|d| d.get_temp(list_id)).unwrap_or_default();
    let focused = ctx.memory(|m| m.has_focus(id));
    let mut accepted = false;
    let mut reveal = false;

    // Before the field sees them: these keys belong to the list while it's open.
    if focused {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space)) {
            list.open = true;
            list.selected = 0;
        }
        let set = if list.open { caret(&ctx, id, text).and_then(|c| expr::complete(text, c, &table.usable(own))) } else { None };
        if let Some(set) = set {
            let n = set.items.len();
            let mut take = false;
            ctx.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    list.selected = (list.selected + 1) % n;
                    reveal = true;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    list.selected = (list.selected + n - 1) % n;
                    reveal = true;
                }
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    list.open = false;
                }
                take = i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab);
            });
            if take {
                accept(&ctx, id, text, &set, list.selected.min(n - 1));
                list.open = false;
                accepted = true;
            }
        }
    }

    let typing = focused.then(|| caret(&ctx, id, text)).flatten();
    let font = egui::FontSelection::default().resolve(ui.style());
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = colored(buf.as_str(), &font, table, own, typing);
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let output = edit(text).id(id).layouter(&mut layouter).show(ui);
    let cursor = output.cursor_range;
    let mut response = output.response.response;
    if response.changed() {
        list.open = response.has_focus();
        list.selected = 0;
    }
    if accepted {
        response.mark_changed();
    }
    if !response.has_focus() && !list.hovered {
        list.open = false;
    }

    list.hovered = false;
    if list.open {
        let at = cursor.map(|r| byte_of(text, r.primary.index)).or_else(|| caret(&ctx, id, text));
        match at.and_then(|c| expr::complete(text, c, &table.usable(own))) {
            Some(set) => {
                list.selected = list.selected.min(set.items.len() - 1);
                let (clicked, hovered) = show_list(&ctx, list_id, response.rect, &set, table, list.selected, reveal);
                list.hovered = hovered;
                if let Some(k) = clicked {
                    accept(&ctx, id, text, &set, k);
                    list.open = false;
                    list.hovered = false;
                    response.mark_changed();
                    ctx.memory_mut(|m| m.request_focus(id));
                }
            }
            // The caret has moved somewhere nothing fits.
            None => list.open = false,
        }
    }
    // Takes effect next frame, when these keys would otherwise move the focus away.
    if list.open && response.has_focus() {
        let keys = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        ctx.memory_mut(|m| m.set_focus_lock_filter(id, keys));
    }
    ctx.data_mut(|d| d.insert_temp(list_id, list));
    response
}

/// An expression field on one line.
pub fn expr_line(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, width: f32) -> Response {
    expr_field(ui, salt, text, table, None, |t| TextEdit::singleline(t).desired_width(width))
}
