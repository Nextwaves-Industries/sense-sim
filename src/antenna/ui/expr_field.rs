//! The text field for a design-variable expression: variables, numbers, units and
//! functions in their own colours, an unknown name marked where it is, and a list of
//! what fits at the caret as you type. Up and Down move in the list, Enter or Tab take
//! the choice, Esc closes it, and Ctrl+Space opens it without typing. What each piece
//! is and what fits are worked out by [`antenna_core::expr`]; this only shows them.
//! A field holding a plain number can also be dragged, or scrolled while it's being typed in.

use std::fmt::Debug;
use std::hash::Hash;

use antenna_core::expr::{self, Completions, PlainNumber, TokenKind};
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

/// A name typed into a field that no variable has yet, for the Studio to ask about. A field can't do
/// that itself: it only has its own text, and the variable goes in the project.
#[derive(Clone, Debug, PartialEq)]
pub struct NewName {
    pub name: String,
    /// What a plain number means in that field: `mm`, `deg`, or nothing.
    pub unit: &'static str,
    /// What the field said before it was typed in.
    pub was: String,
    /// The name is all the field says: it was typed over what was there, which is then likely its value.
    pub alone: bool,
    pub field: egui::Id,
}

fn new_names_id() -> egui::Id {
    egui::Id::new("expr_field_new_names")
}

fn no_asking_id() -> egui::Id {
    egui::Id::new("expr_field_no_asking")
}

fn put_back_id(field: egui::Id) -> egui::Id {
    field.with("put_back")
}

/// The names this expression uses that no variable has. An expression that doesn't parse yet has
/// none to speak of, and neither does a variable's own name in its own row.
pub fn missing_names(text: &str, table: &VarTable, own: Option<&str>) -> Vec<String> {
    let Ok(names) = expr::names(text) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if Some(n.as_str()) != own && !table.has(&n) && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// What fields have asked about since this was last called.
pub fn take_new_names(ctx: &egui::Context) -> Vec<NewName> {
    ctx.data_mut(|d| d.remove_temp::<Vec<NewName>>(new_names_id())).unwrap_or_default()
}

/// A field has asked about a name this frame, and nobody has picked that up yet.
pub fn new_name_waiting(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Vec<NewName>>(new_names_id())).is_some_and(|q| !q.is_empty())
}

/// The field goes back to what it said: the name it was asked about was turned down.
pub fn put_back(ctx: &egui::Context, field: egui::Id, was: String) {
    ctx.data_mut(|d| d.insert_temp(put_back_id(field), was));
}

/// Fields drawn inside `f` don't ask: it's the dialog that's asking, and it says what's wrong itself.
pub fn without_asking<R>(ctx: &egui::Context, f: impl FnOnce() -> R) -> R {
    ctx.data_mut(|d| d.insert_temp(no_asking_id(), true));
    let out = f();
    ctx.data_mut(|d| d.remove_temp::<bool>(no_asking_id()));
    out
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

/// A drag on a field holding a plain number, from where it began.
#[derive(Clone)]
struct Scrub {
    number: PlainNumber,
    /// As it was typed, to go back to exactly if the drag comes back to where it started.
    start: String,
    /// Steps moved so far: added up, so pressing Shift partway doesn't make the value jump.
    moved: f64,
    decimals: usize,
}

impl Scrub {
    /// The text for how far it's moved, at `step` a step.
    fn written(&self, step: f64) -> String {
        if self.moved.abs() < 1e-9 { self.start.clone() } else { self.number.text(self.number.value + self.moved * step, self.decimals) }
    }
}

/// Shift for a tenth of the step, Ctrl for ten of them.
fn step_scale(ctx: &egui::Context) -> f64 {
    ctx.input(|i| if i.modifiers.shift { 0.1 } else if i.modifiers.ctrl { 10.0 } else { 1.0 })
}

fn scrub_frame() -> egui::Id {
    egui::Id::new("expr_field_scrub_frame")
}

/// A field is being dragged, and not for the first frame. The pages count one edit per field by
/// whether the keyboard is in one, and a drag never puts it there; this keeps a drag to one undo.
/// The first frame is left out so that a drag right after typing is an undo step of its own.
pub fn scrubbing(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(scrub_frame())) == Some(ctx.cumulative_frame_nr())
}

/// The drag on a field that isn't being typed in: changes `text`, or on a plain click, puts the caret
/// where it landed. Gives what the field's response has to take on.
fn scrub(ui: &egui::Ui, id: egui::Id, rect: egui::Rect, text: &mut String, unit: Option<&str>, caret_at: impl Fn(egui::Pos2) -> egui::text::CCursor) -> (Response, bool) {
    let ctx = ui.ctx().clone();
    let state_id = id.with("scrub_state");
    // Not focusable: Tab should go on stopping only at the field itself.
    let over = ui.interact(rect, id.with("scrub"), Sense::CLICK | Sense::DRAG);
    if over.hovered() || over.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let mut changed = false;
    if over.drag_started() {
        // Whatever was being typed in ends here, as a click elsewhere would end it.
        ctx.memory_mut(|m| {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        });
        if let Some(number) = PlainNumber::read(text, unit) {
            let decimals = number.decimals_for(number.drag_step());
            ctx.data_mut(|d| d.insert_temp(state_id, Scrub { number, start: text.clone(), moved: 0.0, decimals }));
        }
    } else if over.dragged()
        && let Some(mut s) = ctx.data(|d| d.get_temp::<Scrub>(state_id))
    {
        let d = over.drag_delta();
        let scale = step_scale(&ctx);
        let step = s.number.drag_step();
        // Right and up for more, as on the app's other number fields.
        s.moved += (d.x - d.y) as f64 * scale;
        s.decimals = s.decimals.max(s.number.decimals_for(step * scale));
        let now = s.written(step);
        changed = now != *text;
        *text = now;
        // Read first: asking the context anything while it's locked for writing never returns.
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| {
            d.insert_temp(state_id, s);
            d.insert_temp(scrub_frame(), frame);
        });
    }
    let held_still = over.drag_stopped() && ctx.data(|d| d.get_temp::<Scrub>(state_id)).is_some_and(|s| s.moved == 0.0);
    if over.drag_stopped() {
        ctx.data_mut(|d| d.remove::<Scrub>(state_id));
    }
    if over.clicked() || held_still {
        let mut state = TextEdit::load_state(&ctx, id).unwrap_or_default();
        let at = over.interact_pointer_pos().map_or_else(|| egui::text::CCursor::new(text.chars().count()), caret_at);
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(at)));
        state.store(&ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }
    (over, changed)
}

/// The wheel over the field being typed in. Only that one: a value that changed because the panel
/// was being scrolled and a field happened to pass under the pointer is a design silently altered.
fn wheel(ui: &egui::Ui, id: egui::Id, number: &PlainNumber, text: &mut String) -> bool {
    let ctx = ui.ctx();
    let notches: f32 = ctx.input_mut(|i| {
        // Every frame the pointer's here, as what's left of a scroll carries on for a few.
        i.smooth_scroll_delta = egui::Vec2::ZERO;
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, .. } => Some(delta.x + delta.y),
                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } => Some((delta.x + delta.y) / 50.0),
                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Page, delta, .. } => Some((delta.x + delta.y) * 10.0),
                _ => None,
            })
            .sum()
    });
    if notches == 0.0 {
        return false;
    }
    // Held from the first notch until the field's typed in or left, not read again from the text:
    // so ten up and ten down come back to where they began even if the number passed 10 on the way
    // or went through a bare zero, and a touchpad's fractions of a notch add up.
    let state_id = wheel_state(id);
    // Unless the text isn't what it left there: then something else has changed it, and that counts.
    let held = ctx.data(|d| d.get_temp::<Scrub>(state_id)).filter(|s| s.written(s.number.wheel_step()) == *text);
    let mut s = held.unwrap_or_else(|| Scrub { number: number.clone(), start: text.clone(), moved: 0.0, decimals: 0 });
    let scale = if s.number.whole { step_scale(ctx).max(1.0) } else { step_scale(ctx) };
    let step = s.number.wheel_step();
    s.moved += notches as f64 * scale;
    s.decimals = s.decimals.max(s.number.decimals_for(step * scale));
    let now = s.written(step);
    let changed = now != *text;
    *text = now;
    ctx.data_mut(|d| d.insert_temp(state_id, s));
    changed
}

fn wheel_state(id: egui::Id) -> egui::Id {
    id.with("wheel_state")
}

/// An expression field. `own` is the variable this expression defines, if it does. `unit` is what
/// it measures, if it knows; see [`PlainNumber::read`]. `edit` makes the field itself, so a caller
/// sets its size and whether it wraps. While it holds a plain number, a drag on it changes the
/// number, a click starts typing, and once typing the wheel over it changes the number.
pub fn expr_field(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, own: Option<&str>, unit: Option<&str>, edit: impl for<'t> FnOnce(&'t mut String) -> TextEdit<'t>) -> Response {
    let id = ui.make_persistent_id(salt);
    let list_id = id.with("list");
    let ctx = ui.ctx().clone();
    let mut list: List = ctx.data(|d| d.get_temp(list_id)).unwrap_or_default();
    let focused = ctx.memory(|m| m.has_focus(id));
    let mut accepted = false;
    let mut reveal = false;
    if let Some(was) = ctx.data_mut(|d| d.remove_temp::<String>(put_back_id(id))) {
        *text = was;
        accepted = true;
    }

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
    let (galley, galley_pos) = (output.galley.clone(), output.galley_pos);
    let mut response = output.response.response;
    if response.changed() {
        list.open = response.has_focus();
        list.selected = 0;
    }
    if accepted {
        response.mark_changed();
    }
    // What the wheel's been going by no longer applies.
    if response.changed() || response.lost_focus() {
        ctx.data_mut(|d| d.remove::<Scrub>(wheel_state(id)));
    }
    // A variable written as a bare 0 has nothing to say what it measures: 1 might be a metre.
    let number = PlainNumber::read(text, unit).filter(|n| own.is_none() || n.value != 0.0 || !n.suffix.is_empty());
    if response.has_focus() {
        if let Some(number) = number.filter(|_| !list.open && ui.rect_contains_pointer(response.rect))
            && wheel(ui, id, &number, text)
        {
            response.mark_changed();
        }
    } else if number.is_some() && ui.is_enabled() {
        let caret_at = |at: egui::Pos2| galley.cursor_from_pos(at - galley_pos);
        let (over, changed) = scrub(ui, id, response.rect, text, unit, caret_at);
        // Hover text and the like belong to the field, which is underneath.
        response = response.union(over);
        if changed {
            response.mark_changed();
        }
    }
    if response.gained_focus() {
        ctx.data_mut(|d| d.insert_temp(id.with("was"), text.clone()));
    }
    // Leaving the field is when a name stops being a word still being typed. A press on the list takes
    // the focus too, on its way to finishing the word, and Esc is for leaving things as they are.
    // `lost_focus` stays true for two frames, and the key that did it is only there in the first, so
    // it's decided once, in the first, and not again until the field has had the focus back.
    let left = id.with("left");
    let decided = ctx.data(|d| d.get_temp::<bool>(left)).unwrap_or(false);
    if response.has_focus() && decided {
        ctx.data_mut(|d| d.remove_temp::<bool>(left));
    }
    let leaving = response.lost_focus() && !decided;
    if leaving {
        ctx.data_mut(|d| d.insert_temp(left, true));
    }
    if leaving && !list.hovered && !ctx.input(|i| i.key_pressed(Key::Escape)) && ctx.data(|d| d.get_temp::<bool>(no_asking_id())).is_none() {
        let missing = missing_names(text, table, own);
        if !missing.is_empty() {
            let was: String = ctx.data(|d| d.get_temp(id.with("was"))).unwrap_or_default();
            let unit = match unit {
                Some("mm") => "mm",
                Some("deg") => "deg",
                _ => "",
            };
            let typed = text.trim().to_string();
            ctx.data_mut(|d| {
                let queue = d.get_temp_mut_or_default::<Vec<NewName>>(new_names_id());
                queue.extend(missing.into_iter().map(|name| NewName { alone: name == typed, name, unit, was: was.clone(), field: id }));
            });
            ctx.request_repaint();
        }
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

fn line(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, width: f32, unit: Option<&str>) -> Response {
    // A grid hands a cell only as much room as its column had last frame, so without a floor a column
    // that starts narrow stays narrow, and `a1+10mm` wraps a word to a line.
    expr_field(ui, salt, text, table, None, unit, |t| TextEdit::singleline(t).desired_width(width).min_size(egui::vec2(width, 0.0)))
}

/// An expression field on one line, for a count or anything else with no unit of its own.
pub fn expr_line(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, width: f32) -> Response {
    line(ui, salt, text, table, width, None)
}

/// An expression field on one line, for a length.
pub fn length_line(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, width: f32) -> Response {
    line(ui, salt, text, table, width, Some("mm"))
}

/// An expression field on one line, for an angle that takes a unit.
pub fn angle_line(ui: &mut egui::Ui, salt: impl Hash + Debug, text: &mut String, table: &VarTable, width: f32) -> Response {
    line(ui, salt, text, table, width, Some("deg"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_egui::egui::{Event, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect, TouchPhase};

    /// The field on its own, played real pointer events a frame at a time.
    struct Bench {
        ctx: egui::Context,
        table: VarTable,
        text: String,
        rect: Rect,
        time: f64,
        focused: bool,
        changed: bool,
        going: bool,
        scroll_left: egui::Vec2,
        modifiers: egui::Modifiers,
    }

    impl Bench {
        fn new(text: &str) -> Self {
            let project = Project::from_template("custom", Some("1GHz")).unwrap();
            let mut b = Self { ctx: egui::Context::default(), table: VarTable::of(&project), text: text.into(), rect: Rect::NOTHING, time: 0.0, focused: false, changed: false, going: false, scroll_left: egui::Vec2::ZERO, modifiers: egui::Modifiers::NONE };
            // A widget can only be hit once it's been laid out.
            b.frame(vec![]);
            b.frame(vec![]);
            b
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.time += 0.05;
            let mut all = vec![Event::ModifiersChanged(self.modifiers)];
            all.extend(events);
            let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), time: Some(self.time), predicted_dt: 0.05, events: all, ..Default::default() };
            let ctx = self.ctx.clone();
            let mut out = ctx.run_ui(input, |ui| {
                let r = length_line(ui, "field", &mut self.text, &self.table, 120.0);
                self.rect = r.rect;
                self.focused = r.has_focus();
                self.changed |= r.changed();
                self.going = scrubbing(ui.ctx());
                self.scroll_left = ui.input(|i| i.smooth_scroll_delta);
            });
            // Nothing draws this frame, and epaint panics if its texture updates are dropped unapplied.
            out.textures_delta.clear();
        }

        fn middle(&self) -> Pos2 {
            self.rect.center()
        }

        fn button(&mut self, at: Pos2, pressed: bool) {
            self.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: self.modifiers }]);
        }

        fn drag(&mut self, steps: &[f32]) {
            let at = self.middle();
            self.frame(vec![Event::PointerMoved(at)]);
            self.button(at, true);
            for dx in steps {
                self.frame(vec![Event::PointerMoved(at + vec2(*dx, 0.0))]);
            }
            self.button(at + vec2(*steps.last().unwrap(), 0.0), false);
            self.frame(vec![]);
        }

        fn click(&mut self) {
            let at = self.middle();
            self.frame(vec![Event::PointerMoved(at)]);
            self.button(at, true);
            self.button(at, false);
            self.frame(vec![]);
        }

        fn wheel(&mut self, notches: f32) {
            let at = self.middle();
            self.frame(vec![Event::PointerMoved(at)]);
            self.frame(vec![Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, notches), phase: TouchPhase::Move, modifiers: self.modifiers }]);
        }
    }

    #[test]
    fn dragging_a_number_changes_it_and_keeps_its_unit() {
        let mut b = Bench::new("70mm");
        // Past the distance that makes it a drag, then three points at a time, at 0.1 a point.
        b.drag(&[10.0, 13.0, 16.0]);
        assert_eq!(b.text, "70.6mm");
        assert!(b.changed);
        assert!(!b.focused, "a drag doesn't start typing");
    }

    #[test]
    fn dragging_left_takes_away_and_coming_back_restores_what_was_typed() {
        let mut b = Bench::new("1.50mm");
        b.drag(&[10.0, 5.0]);
        assert_eq!(b.text, "1.45mm");
        // Back to where it began, the text is as it was typed, trailing zero and all.
        let mut b = Bench::new("1.50mm");
        b.drag(&[10.0, 15.0, 10.0]);
        assert_eq!(b.text, "1.50mm");
    }

    #[test]
    fn a_bare_zero_in_a_length_field_becomes_millimetres() {
        let mut b = Bench::new("0");
        b.drag(&[10.0, 13.0]);
        assert_eq!(b.text, "0.03mm", "not 0.03, which would be 30 mm");
    }

    #[test]
    fn shift_is_finer_and_ctrl_is_coarser() {
        let mut b = Bench::new("70mm");
        b.modifiers = egui::Modifiers::SHIFT;
        b.drag(&[10.0, 13.0]);
        assert_eq!(b.text, "70.03mm");
        let mut b = Bench::new("70mm");
        b.modifiers = egui::Modifiers::CTRL;
        b.drag(&[10.0, 13.0]);
        assert_eq!(b.text, "73mm");
    }

    #[test]
    fn a_click_starts_typing_and_changes_nothing() {
        let mut b = Bench::new("70mm");
        b.click();
        assert!(b.focused);
        assert_eq!(b.text, "70mm");
        assert!(!b.changed);
    }

    #[test]
    fn once_typing_a_drag_selects_text_as_it_always_did() {
        let mut b = Bench::new("70mm");
        b.click();
        b.drag(&[10.0, 16.0]);
        assert_eq!(b.text, "70mm");
        assert!(b.focused);
    }

    #[test]
    fn a_formula_has_no_number_to_drag() {
        for formula in ["f0/2", "2*3mm"] {
            let mut b = Bench::new(formula);
            b.drag(&[10.0, 30.0]);
            assert_eq!(b.text, formula);
            assert!(b.focused, "it selects text, as before");
        }
    }

    #[test]
    fn a_drag_after_its_first_frame_counts_as_one_edit() {
        let mut b = Bench::new("70mm");
        let at = b.middle();
        b.frame(vec![Event::PointerMoved(at)]);
        b.button(at, true);
        b.frame(vec![Event::PointerMoved(at + vec2(10.0, 0.0))]);
        assert!(!b.going, "the first frame leaves room for a new undo step");
        b.frame(vec![Event::PointerMoved(at + vec2(13.0, 0.0))]);
        assert!(b.going);
        b.frame(vec![Event::PointerMoved(at + vec2(16.0, 0.0))]);
        assert!(b.going);
        b.button(at + vec2(16.0, 0.0), false);
        assert!(!b.going);
    }

    #[test]
    fn the_wheel_changes_only_the_field_being_typed_in() {
        let mut b = Bench::new("70mm");
        b.wheel(1.0);
        assert_eq!(b.text, "70mm", "scrolling a panel past a field must not change it");
        b.click();
        b.wheel(1.0);
        assert_eq!(b.text, "71mm");
        assert_eq!(b.scroll_left, egui::Vec2::ZERO, "and the panel behind it stays put");
        b.wheel(-2.0);
        assert_eq!(b.text, "69mm");
    }

    #[test]
    fn the_wheel_comes_back_to_where_it_began_across_a_power_of_ten() {
        let mut b = Bench::new("9.5mm");
        b.click();
        for _ in 0..10 {
            b.wheel(1.0);
        }
        assert_eq!(b.text, "10.5mm");
        for _ in 0..10 {
            b.wheel(-1.0);
        }
        assert_eq!(b.text, "9.5mm");
    }

    #[test]
    fn the_wheel_keeps_what_a_bare_number_meant_through_zero() {
        // 0.01 here is ten millimetres. Passing through 0 mustn't turn it into 0.01mm.
        let mut b = Bench::new("0.01");
        b.click();
        for _ in 0..10 {
            b.wheel(-1.0);
        }
        assert_eq!(b.text, "0");
        for _ in 0..10 {
            b.wheel(1.0);
        }
        assert_eq!(b.text, "0.01");
    }

    #[test]
    fn a_touchpads_fractions_of_a_notch_add_up() {
        let mut b = Bench::new("70mm");
        b.click();
        for _ in 0..3 {
            let at = b.middle();
            b.frame(vec![Event::PointerMoved(at)]);
            b.frame(vec![Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, 20.0), phase: TouchPhase::Move, modifiers: b.modifiers }]);
        }
        assert_eq!(b.text, "71mm", "1.2 notches");
    }

    #[test]
    fn a_value_changed_from_elsewhere_starts_the_wheel_afresh() {
        let mut b = Bench::new("70mm");
        b.click();
        b.wheel(1.0);
        assert_eq!(b.text, "71mm");
        // An undo, or a tool, with the field still in use.
        b.text = "5mm".into();
        b.wheel(1.0);
        assert_eq!(b.text, "5.1mm", "from what's there now, by a step to suit it");
    }

    #[test]
    fn leaving_the_field_starts_the_wheel_afresh() {
        let mut b = Bench::new("70mm");
        b.click();
        b.wheel(1.0);
        let away = egui::pos2(300.0, 250.0);
        b.frame(vec![Event::PointerMoved(away)]);
        b.button(away, true);
        b.button(away, false);
        b.frame(vec![]);
        assert!(!b.focused);
        b.text = "71.5mm".into();
        b.click();
        b.wheel(-1.0);
        assert_eq!(b.text, "70.5mm");
    }

    fn away(b: &mut Bench) {
        let at = egui::pos2(300.0, 250.0);
        b.frame(vec![Event::PointerMoved(at)]);
        b.button(at, true);
        b.button(at, false);
        b.frame(vec![]);
    }

    #[test]
    fn a_name_nobody_has_is_asked_about_on_leaving_the_field() {
        let mut b = Bench::new("70mm");
        b.click();
        b.text = "a1+10mm".into();
        b.frame(vec![]);
        assert!(take_new_names(&b.ctx).is_empty(), "not while it's being typed");
        away(&mut b);
        let asked = take_new_names(&b.ctx);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!((asked[0].name.as_str(), asked[0].unit, asked[0].was.as_str()), ("a1", "mm", "70mm"));
        assert_eq!(b.text, "a1+10mm", "and the field keeps what was typed");
        assert!(take_new_names(&b.ctx).is_empty(), "once");
        // Back in and out again is a new decision.
        b.click();
        b.frame(vec![]);
        away(&mut b);
        assert_eq!(take_new_names(&b.ctx).len(), 1);
    }

    #[test]
    fn enter_asks_too_and_each_new_name_is_asked_about_once() {
        let mut b = Bench::new("1mm");
        b.click();
        b.text = "w1*w1+w2".into();
        b.frame(vec![]);
        let enter = |pressed| Event::Key { key: Key::Enter, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        b.frame(vec![enter(true), enter(false)]);
        b.frame(vec![]);
        let names: Vec<String> = take_new_names(&b.ctx).into_iter().map(|n| n.name).collect();
        assert_eq!(names, ["w1", "w2"]);
    }

    #[test]
    fn esc_asks_nothing() {
        let mut b = Bench::new("1mm");
        b.click();
        b.text = "a1".into();
        b.frame(vec![]);
        let esc = |pressed| Event::Key { key: Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        b.frame(vec![esc(true), esc(false)]);
        b.frame(vec![]);
        assert!(!b.focused);
        assert!(take_new_names(&b.ctx).is_empty());
    }

    #[test]
    fn what_can_already_be_worked_out_is_not_asked_about() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        p.params.push(antenna_core::project::Param::new("L", "70mm", ""));
        let table = VarTable::of(&p);
        for fine in ["L", "L/2+1mm", "c0/f0", "2*pi", "sqrt(L)", "er", "5mm", "", "a1+", "(a1"] {
            assert!(missing_names(fine, &table, None).is_empty(), "{fine}");
        }
        assert_eq!(missing_names("a1+L", &table, None), ["a1"]);
        assert_eq!(missing_names("a1+a2+a1", &table, None), ["a1", "a2"]);
        assert!(missing_names("x+1", &table, Some("x")).is_empty(), "its own row says what's wrong with that");
    }

    #[test]
    fn picking_from_the_list_is_not_leaving_the_field() {
        let mut b = Bench::new("");
        // The list writes in the app's own fonts.
        crate::theme::apply_fonts(&b.ctx);
        b.frame(vec![]);
        b.click();
        // Typed so the list opens: only `f0` starts this way.
        b.frame(vec![Event::Text("f".into())]);
        b.frame(vec![]);
        assert_eq!(b.text, "f");
        let item = b.rect.left_bottom() + vec2(30.0, 20.0);
        b.frame(vec![Event::PointerMoved(item)]);
        b.frame(vec![]);
        b.button(item, true);
        b.button(item, false);
        b.frame(vec![]);
        assert_eq!(b.text, "f0");
        assert!(take_new_names(&b.ctx).is_empty());
    }

    #[test]
    fn turning_a_name_down_puts_back_what_was_there() {
        let mut b = Bench::new("70mm");
        b.click();
        b.text = "a1".into();
        b.frame(vec![]);
        away(&mut b);
        let asked = take_new_names(&b.ctx);
        b.changed = false;
        put_back(&b.ctx, asked[0].field, asked[0].was.clone());
        b.frame(vec![]);
        assert_eq!(b.text, "70mm");
        assert!(b.changed, "whatever goes by the field's changes hears about it");
        assert!(take_new_names(&b.ctx).is_empty());
    }

    #[test]
    fn a_field_inside_the_dialog_does_not_ask() {
        let project = Project::from_template("custom", Some("1GHz")).unwrap();
        let table = VarTable::of(&project);
        let ctx = egui::Context::default();
        let mut text = "1mm".to_string();
        let mut frame = |events: Vec<Event>, t: f64, text: &mut String| {
            let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), time: Some(t), events, ..Default::default() };
            let mut rect = Rect::NOTHING;
            let mut out = ctx.run_ui(input, |ui| {
                let c = ui.ctx().clone();
                without_asking(&c, || rect = length_line(ui, "in dialog", text, &table, 120.0).rect);
            });
            out.textures_delta.clear();
            rect
        };
        frame(vec![], 0.0, &mut text);
        let at = frame(vec![], 0.05, &mut text).center();
        for (k, pressed) in [true, false].into_iter().enumerate() {
            frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], 0.1 + k as f64 * 0.05, &mut text);
        }
        frame(vec![], 0.2, &mut text);
        text = "a1".into();
        let off = egui::pos2(300.0, 250.0);
        for (k, pressed) in [true, false].into_iter().enumerate() {
            frame(vec![Event::PointerMoved(off), Event::PointerButton { pos: off, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], 0.25 + k as f64 * 0.05, &mut text);
        }
        frame(vec![], 0.4, &mut text);
        assert!(take_new_names(&ctx).is_empty());
    }

    #[test]
    fn a_variable_written_as_a_bare_zero_cannot_be_dragged() {
        let project = Project::from_template("custom", Some("1GHz")).unwrap();
        let table = VarTable::of(&project);
        for (start, moves) in [("0", false), ("0mm", true), ("50", true)] {
            let ctx = egui::Context::default();
            let mut text = start.to_string();
            let mut rect = Rect::NOTHING;
            let mut frame = |events: Vec<Event>, t: f64, text: &mut String| {
                let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), time: Some(t), events, ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    rect = expr_field(ui, "v", text, &table, Some("mine"), None, |t| TextEdit::singleline(t).desired_width(120.0)).rect;
                });
                out.textures_delta.clear();
                rect
            };
            frame(vec![], 0.0, &mut text);
            let at = frame(vec![], 0.05, &mut text).center();
            frame(vec![Event::PointerMoved(at)], 0.1, &mut text);
            frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }], 0.15, &mut text);
            for (k, dx) in [10.0, 30.0, 50.0].into_iter().enumerate() {
                frame(vec![Event::PointerMoved(at + vec2(dx, 0.0))], 0.2 + k as f64 * 0.05, &mut text);
            }
            frame(vec![Event::PointerButton { pos: at + vec2(50.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }], 0.4, &mut text);
            assert_eq!(text != start, moves, "{start} became {text}");
        }
    }

    #[test]
    fn tab_stops_only_at_the_fields_themselves() {
        let project = Project::from_template("custom", Some("1GHz")).unwrap();
        let table = VarTable::of(&project);
        let ctx = egui::Context::default();
        let (mut first, mut second) = ("1mm".to_string(), "2mm".to_string());
        let mut frame = |events: Vec<Event>, t: f64| -> (Rect, egui::Id) {
            let (mut rect, mut id) = (Rect::NOTHING, egui::Id::NULL);
            let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), time: Some(t), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                rect = length_line(ui, "one", &mut first, &table, 120.0).rect;
                id = length_line(ui, "two", &mut second, &table, 120.0).id;
            });
            out.textures_delta.clear();
            (rect, id)
        };
        frame(vec![], 0.0);
        let (rect, two) = frame(vec![], 0.05);
        let at = rect.center();
        frame(vec![Event::PointerMoved(at)], 0.1);
        frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }], 0.15);
        frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }], 0.2);
        frame(vec![], 0.25);
        let tab = |pressed| Event::Key { key: Key::Tab, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        frame(vec![tab(true), tab(false)], 0.3);
        frame(vec![], 0.35);
        assert_eq!(ctx.memory(|m| m.focused()), Some(two));
    }
}
