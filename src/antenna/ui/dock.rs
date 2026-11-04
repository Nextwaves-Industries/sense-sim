//! The strip under the canvas: every variable with its range, what the model and the solver have to
//! say, and what's running. Side by side when there's room for them, on tabs when there isn't.

use std::time::{Duration, Instant};

use antenna_core::project::Project;
use antenna_core::report::param_text;
use bevy_egui::egui::{self, Align, Layout, Margin, RichText, Sense, vec2};

use super::expr_field::{VarTable, expr_field};
use super::model3d::Selection;
use super::{solve, weak};
use crate::antenna::{Status, Studio};
use crate::theme::{self, icon};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Warning,
    Note,
}

struct Message {
    level: Level,
    text: String,
    /// The step it's about, so a click can show it.
    feature: Option<u32>,
}

/// One thing that ran, or is running.
pub struct JobRow {
    name: String,
    started: Instant,
    ended: Option<Duration>,
    failed: bool,
}

pub struct DockUi {
    search: String,
    /// Only the variables that change this part.
    used_by: Option<String>,
    corners: bool,
    derived: bool,
    show: [bool; 3],
    jobs: Vec<JobRow>,
    was_busy: bool,
    tab: usize,
    /// The row whose name is being typed over.
    renaming: Option<(String, String)>,
    rename_error: Option<String>,
}

impl Default for DockUi {
    fn default() -> Self {
        Self { search: String::new(), used_by: None, corners: false, derived: false, show: [true; 3], jobs: Vec::new(), was_busy: false, tab: 0, renaming: None, rename_error: None }
    }
}

const NAME_W: f32 = 74.0;
const VALUE_W: f32 = 84.0;
const OPT_W: f32 = 68.0;
const RANGE_W: f32 = 84.0;
const TRASH_W: f32 = 22.0;
const LINE_H: f32 = 24.0;

fn cell(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(vec2(w, LINE_H), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(w);
        ui.set_min_height(LINE_H);
        add(ui);
    });
}

fn expr_w(ui: &egui::Ui) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    (ui.available_width() - NAME_W - VALUE_W - OPT_W - 2.0 * RANGE_W - TRASH_W - 6.0 * gap).max(90.0)
}

fn header(ui: &mut egui::Ui) {
    let title = |ui: &mut egui::Ui, t: &str| {
        ui.label(RichText::new(t).size(12.0).color(theme::WEAK));
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        cell(ui, NAME_W, |ui| title(ui, "Name"));
        let w = expr_w(ui);
        cell(ui, w, |ui| title(ui, "Expression"));
        cell(ui, VALUE_W, |ui| title(ui, "Value"));
        cell(ui, OPT_W, |ui| title(ui, "Optimizable"));
        cell(ui, RANGE_W, |ui| title(ui, "Min."));
        cell(ui, RANGE_W, |ui| title(ui, "Max."));
    });
}

/// One row. The optimiser's list is kept up to date by the caller.
fn var_row(ui: &mut egui::Ui, st: &mut Studio, name: &str, table: &VarTable, stripe: bool) {
    let Some(i) = st.project.params.iter().position(|p| p.name == name) else { return };
    let value = table.value(name);
    let at = ui.cursor().min;
    let framed = egui::Frame::new().fill(if stripe { ui.visuals().faint_bg_color } else { egui::Color32::TRANSPARENT }).inner_margin(Margin::ZERO).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let locked = st.project.params[i].name == "f0" || st.project.params[i].name == "lambda0";
            cell(ui, NAME_W, |ui| {
                if st.ui.dock.renaming.as_ref().is_some_and(|r| r.0 == name) {
                    let mut text = st.ui.dock.renaming.as_ref().map_or(String::new(), |r| r.1.clone());
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(NAME_W - 4.0));
                    r.request_focus();
                    let done = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let gone = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if let Some(row) = st.ui.dock.renaming.as_mut() {
                        row.1 = text.clone();
                    }
                    if gone {
                        st.ui.dock.renaming = None;
                        st.ui.dock.rename_error = None;
                    } else if done {
                        let mut renamed = st.project.clone();
                        match renamed.rename_variable(name, text.trim()) {
                            Ok(()) => {
                                super::draw::remember(st);
                                st.project = renamed;
                                st.ui.variable_renamed(name, text.trim());
                                st.ui.dock.renaming = None;
                                st.ui.dock.rename_error = None;
                            }
                            Err(e) => st.ui.dock.rename_error = Some(format!("{e:#}")),
                        }
                    }
                } else {
                    let note = st.project.params[i].note.clone();
                    let r = ui.add(egui::Label::new(RichText::new(name).monospace()).truncate().sense(Sense::click()));
                    let r = if note.is_empty() { r } else { r.on_hover_text(note) };
                    if r.double_clicked() && !locked {
                        st.ui.dock.renaming = Some((name.to_string(), name.to_string()));
                    }
                    r.context_menu(|ui| {
                        if !locked && ui.button("Rename").clicked() {
                            st.ui.dock.renaming = Some((name.to_string(), name.to_string()));
                            ui.close();
                        }
                        if !locked && !st.project.params[i].derived && ui.button(theme::label(icon::TRASH, "Delete")).clicked() {
                            super::draw::remember(st);
                            st.project.params.remove(i);
                            ui.close();
                        }
                    });
                }
            });
            // Deleted or renamed a moment ago.
            if st.project.params.get(i).is_none_or(|p| p.name != name) {
                return;
            }
            let w = expr_w(ui);
            let field = {
                let p = &mut st.project.params[i];
                let field = expr_field(ui, ("dock_var", name), &mut p.expr, table, Some(name), None, |t| {
                    egui::TextEdit::multiline(t).desired_rows(1).desired_width(w).min_size(vec2(w, LINE_H - 2.0)).vertical_align(Align::Center).return_key(None)
                });
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    field.surrender_focus();
                }
                if field.changed() {
                    p.expr.retain(|c| c != '\n' && c != '\r');
                }
                field
            };
            if field.has_focus() {
                st.ui.dim_focus = Some(name.to_string());
            }
            cell(ui, VALUE_W, |ui| {
                ui.add(egui::Label::new(RichText::new(value.map_or("–".into(), |v| param_text(name, v))).color(theme::WEAK)).truncate());
            });
            let derived = st.project.params[i].derived;
            let row = st.ui.opt_vars.iter().position(|v| v.0 == name);
            match row {
                Some(k) if !derived => {
                    cell(ui, OPT_W, |ui| {
                        ui.add_space(22.0);
                        ui.checkbox(&mut st.ui.opt_vars[k].3, "").on_hover_text("The optimizer and Tuning may change this one");
                    });
                    let on = st.ui.opt_vars[k].3;
                    let speed = value.unwrap_or(0.0).abs() * 0.005;
                    let (mut lo, mut hi) = (st.ui.opt_vars[k].1, st.ui.opt_vars[k].2);
                    // Only something that will be varied has a range worth a field.
                    let range = |ui: &mut egui::Ui, v: &mut f64| {
                        if on {
                            ui.add(solve::var_drag(name, v, speed));
                        } else {
                            ui.add(egui::Label::new(RichText::new(param_text(name, *v)).size(12.0).color(theme::FAINT)).truncate());
                        }
                    };
                    cell(ui, RANGE_W, |ui| range(ui, &mut lo));
                    cell(ui, RANGE_W, |ui| range(ui, &mut hi));
                    if (lo, hi) != (st.ui.opt_vars[k].1, st.ui.opt_vars[k].2) {
                        st.ui.opt_vars[k].1 = lo;
                        st.ui.opt_vars[k].2 = hi;
                        // Saved with the project, so the range is still there next time.
                        st.project.params[i].range = Some((lo.min(hi), lo.max(hi)));
                    }
                }
                _ => {
                    cell(ui, OPT_W + RANGE_W * 2.0 + 8.0, |_| {});
                }
            }
        });
    });
    let rect = egui::Rect::from_min_size(at, framed.response.rect.size());
    if ui.rect_contains_pointer(rect) {
        st.ui.dim_focus = Some(name.to_string());
    }
}

fn variables(ui: &mut egui::Ui, st: &mut Studio) {
    let table = VarTable::of(&st.project);
    solve::sync_opt_vars(st);
    let corners = antenna_core::dims::corner_rows(&st.project);
    let parts: Vec<String> = st.ui.drivers.keys().cloned().collect();
    if st.ui.dock.used_by.as_ref().is_some_and(|p| !parts.contains(p)) && !parts.is_empty() {
        st.ui.dock.used_by = None;
    }
    let drivers = st.ui.dock.used_by.as_ref().and_then(|p| st.ui.drivers.get(p)).cloned();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if theme::icon_button(ui, icon::PLUS, "Add a variable").clicked() {
            st.ui.add_var = Some(super::params::AddVar { salt: String::new(), unit: "", name: super::params::free_var_name(st), expr: "0".to_string(), note: "Your own variable".to_string(), error: None, focused: false, asked: None });
        }
        ui.label(RichText::new("Used by:").size(12.0).color(theme::WEAK));
        let shown = st.ui.dock.used_by.clone().unwrap_or_else(|| "All".to_string());
        egui::ComboBox::from_id_salt("dock_used_by").selected_text(shown).width(104.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.ui.dock.used_by, None, "All");
            for p in &parts {
                ui.selectable_value(&mut st.ui.dock.used_by, Some(p.clone()), p);
            }
        });
        ui.add(egui::TextEdit::singleline(&mut st.ui.dock.search).hint_text("Search").desired_width(90.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.menu_button(RichText::new(icon::LIST_BULLETS), |ui| {
                ui.checkbox(&mut st.ui.dock.corners, format!("Corner positions ({})", corners.len()));
                ui.checkbox(&mut st.ui.dock.derived, "Helper values");
                ui.separator();
                if ui.button("Reset to the template").clicked() {
                    let f0 = st.project.params.iter().find(|p| p.name == "f0").map(|p| p.expr.clone());
                    if let Ok(p) = Project::from_template(&st.project.template.clone(), f0.as_deref()) {
                        super::draw::remember(st);
                        // A variable of your own stays: what you drew may be written in it.
                        let own: Vec<_> = st.project.params.iter().filter(|r| !p.params.iter().any(|t| t.name == r.name)).cloned().collect();
                        st.project.params = p.params;
                        st.project.params.extend(own);
                        let _ = st.project.reset_band();
                    }
                    ui.close();
                }
                if ui.button("Fit the sweep to f0").on_hover_text("Sets the sweep band and far-field frequency around the design frequency").clicked() {
                    let _ = st.project.reset_band();
                    ui.close();
                }
            });
        });
    });
    if let Some(e) = st.ui.dock.rename_error.clone() {
        ui.label(RichText::new(e).size(12.0).color(theme::DANGER));
    }
    header(ui);
    let needle = st.ui.dock.search.to_lowercase();
    let names: Vec<String> = st
        .project
        .params
        .iter()
        .filter(|p| st.ui.dock.derived || !p.derived)
        .filter(|p| st.ui.dock.corners || !corners.contains(&p.name))
        .filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle) || p.expr.to_lowercase().contains(&needle))
        .filter(|p| drivers.as_ref().is_none_or(|d| d.contains(&p.name)))
        .map(|p| p.name.clone())
        .collect();
    let before = st.project.params.clone();
    egui::ScrollArea::vertical().id_salt("dock_vars").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (row, name) in names.iter().enumerate() {
            var_row(ui, st, name, &table, row % 2 == 0);
        }
        if names.is_empty() {
            weak(ui, if needle.is_empty() && drivers.is_none() { "No variables yet. Right-click a number under Parameters to make one." } else { "None of them match." });
        }
    });
    // A stray edit moves everything that uses the variable, so it has to be possible to take back.
    // One undo step for each row typed in. Renaming and deleting have already made theirs.
    let same_rows = before.len() == st.project.params.len() && before.iter().zip(&st.project.params).all(|(a, b)| a.name == b.name);
    if st.project.params != before && same_rows {
        if !st.ui.draw.editing_vars {
            let now = std::mem::replace(&mut st.project.params, before);
            super::draw::remember(st);
            st.project.params = now;
            st.ui.draw.editing_vars = true;
        }
    } else if !ui.ctx().egui_wants_keyboard_input() && !super::expr_field::scrubbing(ui.ctx()) {
        st.ui.draw.editing_vars = false;
    }
}

fn gather_messages(st: &Studio) -> Vec<Message> {
    let mut out = Vec::new();
    let mut add = |level, text: String, feature| out.push(Message { level, text, feature });
    if let Err(e) = st.project.vars() {
        add(Level::Error, format!("{e:#}"), None);
    }
    for (id, why) in &st.ui.built.errors {
        let name = st.project.model.feature(*id).map_or("A step", |f| f.name.as_str());
        add(Level::Error, format!("{name}: {why}"), Some(*id));
    }
    if let Status::Failed(e) = st.status()
        && e != "Cancelled"
    {
        add(Level::Error, format!("The run failed: {}", e.chars().take(400).collect::<String>()), None);
    }
    if let Err(e) = &st.ui.geometry
        && !e.is_empty()
        && st.ui.built.errors.is_empty()
        && st.project.vars().is_ok()
    {
        // Having no feed yet isn't wrong, it's only not finished.
        let unfinished = e.contains("no port") || e.contains("no parts");
        add(if unfinished { Level::Note } else { Level::Error }, if e.contains("no port") { "To solve it, it needs a feed: Feed on the ribbon.".to_string() } else if e.contains("no parts") { "There are no parts yet.".to_string() } else { e.clone() }, None);
    }
    if let Some(p) = &st.ui.setup_problem {
        add(Level::Warning, p.clone(), None);
    }
    for (a, b) in &st.ui.touching {
        add(Level::Warning, format!("{a} and {b} touch but aren't joined. FDTD sees them as one piece of metal, but the method of moments needs them combined for current to cross: use Unite."), None);
    }
    if let Some(plan) = &st.ui.plan {
        for w in &plan.warnings {
            add(Level::Warning, w.clone(), None);
        }
    }
    if let Some(n) = &st.ui.m3.note {
        add(Level::Note, n.clone(), None);
    }
    if let Some((m, good)) = &st.ui.message {
        add(if *good { Level::Note } else { Level::Error }, m.clone(), None);
    }
    out
}

fn messages(ui: &mut egui::Ui, st: &mut Studio) {
    let list = gather_messages(st);
    let count = |l: Level| list.iter().filter(|m| m.level == l).count();
    let levels = [(Level::Error, "Errors", icon::WARNING_CIRCLE, theme::DANGER), (Level::Warning, "Warnings", icon::WARNING, theme::WARNING), (Level::Note, "Messages", icon::INFO, theme::ONLINE)];
    let wide = ui.available_width() > 290.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for (k, (level, word, glyph, colour)) in levels.iter().enumerate() {
            let on = st.ui.dock.show[k];
            // Three of these have to fit however narrow the pane is, so the words go first.
            let label = if wide { format!("{glyph} {} {word}", count(*level)) } else { format!("{glyph} {}", count(*level)) };
            let text = RichText::new(label).size(12.0).color(if on { *colour } else { theme::FAINT });
            if ui.add(egui::Button::new(text).fill(if on { theme::RAISED } else { theme::PANEL }).min_size(vec2(0.0, 22.0))).on_hover_text(format!("{} {word}. Click to show or hide them.", count(*level))).clicked() {
                st.ui.dock.show[k] = !on;
            }
        }
    });
    ui.add_space(3.0);
    let mut go = None;
    egui::ScrollArea::vertical().id_salt("dock_messages").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        for m in &list {
            let k = levels.iter().position(|l| l.0 == m.level).unwrap_or(2);
            if !st.ui.dock.show[k] {
                continue;
            }
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(levels[k].2).size(13.0).color(levels[k].3));
                let label = egui::Label::new(RichText::new(&m.text).size(12.0).color(theme::TEXT)).wrap();
                let r = if m.feature.is_some() { ui.add(label.sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand) } else { ui.add(label) };
                if r.clicked() {
                    go = m.feature;
                }
            });
        }
    });
    if let Some(id) = go {
        st.ui.m3.sel = Selection { feature: Some(id), step: true, ..Default::default() };
        st.ui.tree.pick = Default::default();
    }
}

/// Notices runs starting and stopping.
pub fn follow_jobs(st: &mut Studio) {
    let status = st.status();
    let busy = matches!(status, Status::Busy(..));
    if busy && !st.ui.dock.was_busy {
        let name = st.project.name.clone();
        st.ui.dock.jobs.push(JobRow { name, started: Instant::now(), ended: None, failed: false });
        let extra = st.ui.dock.jobs.len().saturating_sub(30);
        st.ui.dock.jobs.drain(..extra);
    }
    if !busy
        && st.ui.dock.was_busy
        && let Some(j) = st.ui.dock.jobs.last_mut()
    {
        j.ended = Some(j.started.elapsed());
        j.failed = matches!(status, Status::Failed(_));
    }
    st.ui.dock.was_busy = busy;
}

fn jobs(ui: &mut egui::Ui, st: &mut Studio) {
    ui.horizontal(|ui| {
        let title = |ui: &mut egui::Ui, w: f32, t: &str| {
            cell(ui, w, |ui| {
                ui.label(RichText::new(t).size(12.0).color(theme::WEAK));
            });
        };
        let w = (ui.available_width() - 150.0).max(60.0);
        title(ui, w, "Name");
        title(ui, 70.0, "Progress");
        title(ui, 64.0, "Elapsed");
    });
    let status = st.status();
    let (message, progress) = match &status {
        Status::Busy(m, p) => (m.clone(), *p),
        _ => (String::new(), -1.0),
    };
    let mut cancel = false;
    let total = st.ui.dock.jobs.len();
    egui::ScrollArea::vertical().id_salt("dock_jobs").auto_shrink([false, false]).show(ui, |ui| {
        if total == 0 {
            weak(ui, "Nothing has run yet.");
        }
        for (k, j) in st.ui.dock.jobs.iter().enumerate().rev() {
            let running = j.ended.is_none() && k + 1 == total;
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 158.0).max(60.0);
                cell(ui, w, |ui| {
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    let r = ui.add(egui::Label::new(RichText::new(&j.name).size(12.5)).truncate());
                    if running && !message.is_empty() {
                        r.on_hover_text(message.as_str());
                    }
                });
                cell(ui, 70.0, |ui| {
                    if running {
                        ui.add(egui::ProgressBar::new(progress.max(0.0)).desired_width(64.0).desired_height(6.0).fill(theme::ACCENT));
                    } else if j.failed {
                        ui.label(RichText::new("Stopped").size(12.0).color(theme::WARNING));
                    } else {
                        ui.label(RichText::new("Done").size(12.0).color(theme::SUCCESS));
                    }
                });
                cell(ui, 44.0, |ui| {
                    let e = j.ended.unwrap_or_else(|| j.started.elapsed());
                    ui.label(RichText::new(super::duration(e.as_secs_f64())).size(12.0).color(theme::WEAK));
                });
                if running && theme::icon_button(ui, icon::STOP, "Stop it").clicked() {
                    cancel = true;
                }
            });
        }
    });
    if cancel {
        st.cancel();
    }
    if matches!(status, Status::Busy(..)) {
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

fn pane(ui: &mut egui::Ui, w: f32, h: f32, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(vec2(w, h), Layout::top_down(Align::Min), |ui| {
        ui.set_width(w);
        ui.set_height(h);
        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
        egui::Frame::new().inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
            ui.set_width(w - 16.0);
            ui.set_height(h - 10.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            if !title.is_empty() {
                ui.label(RichText::new(title).font(theme::bold(12.5)).color(theme::TEXT));
            }
            add(ui);
        });
    });
}

/// The whole strip.
pub fn show(ui: &mut egui::Ui, st: &mut Studio, w: f32, h: f32) {
    if w >= 860.0 {
        let side = (w * 0.2).clamp(190.0, 300.0);
        let vars_w = w - 2.0 * side - 2.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            pane(ui, vars_w, h, "Variables", |ui| variables(ui, st));
            divider(ui, h);
            pane(ui, side, h, "Messages", |ui| messages(ui, st));
            divider(ui, h);
            pane(ui, side, h, "Jobs", |ui| jobs(ui, st));
        });
        return;
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.allocate_ui_with_layout(vec2(w, 26.0), Layout::left_to_right(Align::Center), |ui| {
            for (k, t) in ["Variables", "Messages", "Jobs"].iter().enumerate() {
                if theme::ribbon_tab(ui, t, st.ui.dock.tab == k).clicked() {
                    st.ui.dock.tab = k;
                }
            }
        });
        pane(ui, w, h - 26.0, "", |ui| match st.ui.dock.tab {
            0 => variables(ui, st),
            1 => messages(ui, st),
            _ => jobs(ui, st),
        });
    });
}

fn divider(ui: &mut egui::Ui, h: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(1.0, h), Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::STROKE);
}
