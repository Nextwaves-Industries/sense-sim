//! The Parameters panel: everything about what's picked in the tree, in a grid that can be typed in.
//! Any dimension can be turned into a variable from its right-click menu.

use antenna_core::geometry::P3;
use antenna_core::material::{self, MatKind};
use antenna_core::modeler::{BodyKind, Kind};
use antenna_core::project::SolverKind;
use antenna_core::report::param_text;
use bevy_egui::egui::{self, Key, Response, RichText, vec2};

use super::expr_field::{NewName, VarTable, expr_line, new_name_waiting, put_back, take_new_names, without_asking};
use super::model3d::{self, Unit};
use super::project_tree::Pick;
use super::{ghz_field, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

/// The Add variable dialog: a name for what's in a field, which the field then says instead.
pub struct AddVar {
    /// Which field asked.
    pub salt: String,
    /// The unit a plain number means in that field: `mm`, `deg`, or nothing.
    pub unit: &'static str,
    pub name: String,
    pub expr: String,
    pub note: String,
    pub error: Option<String>,
    pub focused: bool,
    /// A name typed into a field, being asked about: the name stays as typed, and the field keeps
    /// what it says if this goes ahead and gets back what it said if not.
    pub asked: Option<NewName>,
}

/// Opens the dialog for the next name typed into a field that no variable has.
pub fn ask_new_names(ctx: &egui::Context, st: &mut Studio) {
    st.ui.new_names.extend(take_new_names(ctx));
    if st.ui.add_var.is_some() {
        return;
    }
    let hints = st.project.var_hints();
    while let Some(n) = st.ui.new_names.pop_front() {
        // Typed into two fields, say, and answered in the first.
        if hints.iter().any(|h| h.name == n.name) {
            continue;
        }
        st.ui.add_var = Some(AddVar { salt: String::new(), unit: n.unit, name: n.name.clone(), expr: value_from(&n, st), note: String::new(), error: None, focused: false, asked: Some(n) });
        return;
    }
}

/// What the field held, if that makes a value: typing `a1` over `4.5mm` usually means that's what a1 is.
/// In `a1+10mm` it says nothing about a1, and neither does a 0, which is what an empty field starts as.
fn value_from(n: &NewName, st: &Studio) -> String {
    let was = n.was.trim();
    let vars = st.project.vars().unwrap_or_default();
    if n.alone && antenna_core::expr::eval(was, &vars).is_ok_and(|v| v != 0.0) { was.to_string() } else { String::new() }
}

/// A dialog is open or about to be, over a name a field has just asked about.
pub fn asking(ctx: &egui::Context, st: &Studio) -> bool {
    new_name_waiting(ctx) || !st.ui.new_names.is_empty() || st.ui.add_var.as_ref().is_some_and(|d| d.asked.is_some())
}

/// The first of a1, a2, … that's free.
pub fn free_var_name(st: &Studio) -> String {
    let taken = st.project.var_hints();
    (1..).map(|k| format!("a{k}")).find(|n| !taken.iter().any(|h| &h.name == n)).unwrap_or_else(|| "a".to_string())
}

/// The field's own right-click menu: name what's in it, or use a variable that's already there.
pub fn field_menu(resp: &Response, st: &mut Studio, salt: &str, label: &str, text: &str, unit: &'static str) {
    resp.context_menu(|ui| {
        if ui.button(theme::label(icon::PLUS, "Add variable…")).on_hover_text("Name what's in this field. The field then says the name, and changing the variable changes it.").clicked() {
            st.ui.add_var = Some(AddVar { salt: salt.to_string(), unit, name: free_var_name(st), expr: if text.trim().is_empty() { "0".to_string() } else { text.trim().to_string() }, note: label.to_string(), error: None, focused: false, asked: None });
            ui.close();
        }
        let hints = st.project.var_hints();
        ui.menu_button(theme::label(icon::SLIDERS_HORIZONTAL, "Use variable"), |ui| {
            egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                for h in &hints {
                    let value = h.value.map_or(String::new(), |v| param_text(&h.name, v));
                    if ui.button(format!("{}   {value}", h.name)).on_hover_text(h.note.as_str()).clicked() {
                        st.ui.named = Some((salt.to_string(), h.name.clone()));
                        ui.close();
                    }
                }
            });
        });
    });
}

/// What the dialog gave this field, once.
pub fn take_named(st: &mut Studio, salt: &str) -> Option<String> {
    if st.ui.named.as_ref().is_some_and(|n| n.0 == salt) {
        st.ui.named.take().map(|n| n.1)
    } else {
        None
    }
}

/// The dialog, if it's open.
pub fn add_var_dialog(ctx: &egui::Context, st: &mut Studio) {
    let Some(mut d) = st.ui.add_var.take() else { return };
    let vars = st.project.vars().unwrap_or_default();
    let (mut ok, mut cancel) = (false, false);
    let table = VarTable::of(&st.project);
    let asked = d.asked.is_some();
    const WIDTH: f32 = 440.0;
    let modal = egui::Modal::new(egui::Id::new("studio_add_variable")).backdrop_color(theme::BACKDROP).frame(theme::modal_frame()).show(ctx, |ui| {
        ui.set_width(WIDTH);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        cancel |= theme::modal_title(ui, WIDTH, if asked { "New variable" } else { "Add variable" }, true, "Cancel (Esc)");
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
            ui.set_width(WIDTH - 32.0);
            ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
            if asked {
                ui.add(egui::Label::new(format!("{} isn't a variable yet. Give it a value and the field will use it.", d.name)).wrap());
                ui.add_space(2.0);
            }
            without_asking(ctx, || {
                egui::Grid::new("add_variable_grid").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
                    ui.label(RichText::new("Name").color(theme::WEAK));
                    let name = if asked { ui.label(RichText::new(&d.name).font(theme::bold(14.0))) } else { ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(300.0).min_size(vec2(300.0, 0.0))) };
                    ui.end_row();
                    ui.label(RichText::new("Value").color(theme::WEAK));
                    let field = expr_line(ui, "add_variable_expr", &mut d.expr, &table, 300.0).on_hover_text("A number or an expression over other variables, like 4.5mm or L/2");
                    ui.end_row();
                    ui.label("");
                    let value = antenna_core::expr::eval(&with_unit(&d.expr, d.unit), &vars).map_or_else(|_| "Type a number or an expression".to_string(), |v| format!("= {}", param_text(&d.name, v)));
                    ui.label(RichText::new(value).small().color(theme::WEAK));
                    ui.end_row();
                    if !d.focused {
                        if asked { field.request_focus() } else { name.request_focus() };
                        d.focused = true;
                    }
                    ok |= (name.lost_focus() || field.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
                });
            });
            if let Some(e) = &d.error {
                verdict(ui, false, e);
            }
        });
        let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 0.0), egui::Sense::hover());
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, theme::STROKE));
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.set_width(WIDTH - 32.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ok |= theme::primary_button(ui, if asked { "Create" } else { "OK" }).clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
    });
    cancel |= modal.should_close();
    if cancel {
        // Turned down: the field says what it said, and its other new names go with this one.
        if let Some(n) = d.asked {
            put_back(ctx, n.field, n.was);
            st.ui.new_names.retain(|other| other.field != n.field);
        }
        return;
    }
    if ok && asked && d.expr.trim().is_empty() {
        d.error = Some(format!("{} needs a value, like 4.5mm.", d.name));
    } else if ok {
        let expr = with_unit(&d.expr, d.unit);
        let before = st.project.clone();
        match antenna_core::sketch::add_variable(&mut st.project, &d.name, &expr, &d.note) {
            Ok(()) if asked => {
                // Typing it had already marked an undo step if it's in the project, which then doesn't
                // work out on its own. One undo takes back the name and the variable together.
                if before.scatterer().is_ok() {
                    let now = std::mem::replace(&mut st.project, before);
                    super::draw::remember(st);
                    st.project = now;
                }
                return;
            }
            Ok(()) => {
                let now = std::mem::replace(&mut st.project, before);
                super::draw::remember(st);
                st.project = now;
                st.ui.named = Some((d.salt, d.name.trim().to_string()));
                // The field taking the name, next frame, is part of this.
                model3d::one_undo_step(st);
                return;
            }
            Err(e) => d.error = Some(format!("{e:#}")),
        }
    }
    st.ui.add_var = Some(d);
}

/// A plain number in a variable is taken as it stands, but in a length field it meant millimetres
/// and in an angle field degrees, so it keeps that meaning.
fn with_unit(expr: &str, unit: &str) -> String {
    let t = expr.trim();
    if !unit.is_empty() && t.parse::<f64>().is_ok() { format!("{t}{unit}") } else { t.to_string() }
}

/// A row that only says something.
pub fn read_only(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(label);
    ui.add(egui::Label::new(RichText::new(value).color(theme::WEAK)).wrap());
    ui.end_row();
}

fn mm(v: f64) -> String {
    format!("{} mm", (v * 1e7).round() / 1e4)
}

/// Where something is and how big, from its bounds, the way the grid says it for anything else.
pub fn size_rows(ui: &mut egui::Ui, lo: P3, hi: P3) {
    read_only(ui, "P0", &format!("{}; {}; {}", mm(lo[0]), mm(lo[1]), mm(lo[2])));
    for (k, name) in ["XDim", "YDim", "ZDim"].iter().enumerate() {
        read_only(ui, name, &mm(hi[k] - lo[k]));
    }
}

/// What a body comes to, under what made it.
fn body_rows(ui: &mut egui::Ui, st: &Studio, id: u32) {
    let Some(b) = st.ui.built.bodies.iter().find(|b| b.id == id) else { return };
    if b.mesh.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(RichText::new("Size").strong()).id_salt("params_size").default_open(true).show(ui, |ui| {
        grid(ui, "params_size_grid", |ui| {
            let (lo, hi) = b.mesh.bounds();
            size_rows(ui, lo, hi);
            match b.kind {
                BodyKind::Solid => read_only(ui, "Volume", &format!("{:.4} cm³", b.mesh.volume() * 1e6)),
                BodyKind::Sheet => read_only(ui, "Area", &format!("{:.3} cm²", b.mesh.area() * 1e4)),
                BodyKind::Wire => read_only(ui, "Length", &format!("{:.2} mm", b.wires.iter().map(|(p, _)| p.windows(2).map(|w| antenna_core::geometry::norm(antenna_core::geometry::sub(w[1], w[0]))).sum::<f64>()).sum::<f64>() * 1e3)),
            }
        });
    });
}

/// The property grid every panel here uses: names on the left, values on the right.
pub fn grid(ui: &mut egui::Ui, salt: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(salt).num_columns(2).min_col_width(78.0).spacing(vec2(8.0, 3.0)).striped(true).show(ui, add);
}

fn sketch_used_by(kind: &Kind) -> Option<u32> {
    match kind {
        Kind::Extrude { sketch, .. } | Kind::Sheet { sketch, .. } | Kind::Revolve { sketch, .. } | Kind::Sweep { sketch, .. } => Some(*sketch),
        Kind::Wire { path: antenna_core::modeler::PathRef::Sketch { sketch }, .. } => Some(*sketch),
        _ => None,
    }
}

/// One step of the model, with the numbers of the sketch it came from on the same page, so nothing
/// has to be gone looking for.
fn feature(ui: &mut egui::Ui, st: &mut Studio, id: u32) {
    let Some(kind) = st.project.model.feature(id).map(|f| f.kind.clone()) else { return };
    model3d::feature_panel(ui, st, id);
    let sketch = if matches!(kind, Kind::Sketch { .. }) { Some(id) } else { sketch_used_by(&kind) };
    if let Some(s) = sketch {
        model3d::sketch_dims(ui, st, s);
    }
    let body = st.ui.m3.sel.body.or_else(|| st.ui.built.bodies.iter().find(|b| b.id == id || b.last == id).map(|b| b.id));
    if let Some(b) = body {
        body_rows(ui, st, b);
    }
}

/// A part from a template or the flat tools.
fn part(ui: &mut egui::Ui, st: &mut Studio, name: &str) {
    if st.ui.draw.selected.is_some() {
        super::draw::properties(ui, st);
    }
    let Some(row) = st.ui.all_parts.iter().find(|p| p.name == name).cloned() else {
        weak(ui, "It isn't in the model any more.");
        return;
    };
    let from_template = st.ui.draw.selected.is_none();
    theme::caption(ui, "Geometry");
    grid(ui, "params_part", |ui| {
        if from_template {
            read_only(ui, "Name", &row.name);
            read_only(ui, "Material", &row.material);
        }
        read_only(ui, "Kind", row.word);
        size_rows(ui, row.lo, row.hi);
        if row.word == "Sheet" {
            read_only(ui, "Thickness", "0: a surface");
        }
    });
    // A template's formulas aren't text anyone can edit, but the variables that drive them are.
    let driving: Vec<String> = st.ui.drivers.get(name).cloned().unwrap_or_default();
    if !driving.is_empty() {
        theme::caption(ui, "Variables that change it");
        let table = VarTable::of(&st.project);
        let mut focus = None;
        grid(ui, "params_part_vars", |ui| {
            for v in &driving {
                let Some(i) = st.project.params.iter().position(|p| &p.name == v) else { continue };
                let label = ui.label(RichText::new(v).monospace()).on_hover_text(st.project.params[i].note.as_str());
                let w = ui.available_width().max(90.0);
                let field = expr_line(ui, ("part_var", v), &mut st.project.params[i].expr, &table, w);
                if label.hovered() || field.hovered() || field.has_focus() {
                    focus = Some(v.clone());
                }
                ui.end_row();
            }
        });
        if focus.is_some() {
            st.ui.dim_focus = focus;
        }
    }
    if from_template {
        weak(ui, "This part's sizes come from the template's formulas. To edit each size directly, or give it a thickness, make it a feature.");
        if ui.button(theme::label(icon::TREE_STRUCTURE, "Make them editable")).on_hover_text("Turns the template's parts into features with their own dimensions. They stop following the template's formulas, and each size can be tied to a variable again.").clicked() {
            model3d::run_cmd(st, model3d::Cmd::BringIn);
        }
    }
}

/// A port that isn't a step of the model.
fn port(ui: &mut egui::Ui, st: &mut Studio, name: &str) {
    let table = VarTable::of(&st.project);
    if let Some(i) = st.project.extra_ports.iter().position(|p| p.name == name) {
        theme::caption(ui, "Port");
        let mut p = st.project.extra_ports[i].clone();
        let before = p.clone();
        grid(ui, "params_port", |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(150.0));
            ui.end_row();
            for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                ui.label(format!("From {axis}"));
                expr_line(ui, ("port_a", k), &mut p.a[k], &table, 150.0);
                ui.end_row();
            }
            for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                ui.label(format!("To {axis}"));
                expr_line(ui, ("port_b", k), &mut p.b[k], &table, 150.0);
                ui.end_row();
            }
            ui.label("Width");
            expr_line(ui, "port_w", &mut p.width, &table, 150.0);
            ui.end_row();
            ui.label("Impedance");
            expr_line(ui, "port_z", &mut p.z0, &table, 150.0);
            ui.end_row();
        });
        if p != before {
            if p.name != before.name {
                st.ui.tree.pick = Pick::Port(p.name.clone());
            }
            st.project.extra_ports[i] = p;
        }
        if ui.button(theme::label(icon::TRASH, "Remove this port")).clicked() {
            super::draw::remember(st);
            st.project.extra_ports.remove(i);
            st.ui.tree.pick = Pick::Nothing;
        }
        return;
    }
    let Some(p) = st.ui.model.as_ref().and_then(|g| g.ports.iter().find(|p| p.name == name)).cloned() else {
        weak(ui, "It isn't in the model any more.");
        return;
    };
    theme::caption(ui, "Port");
    grid(ui, "params_port_read", |ui| {
        read_only(ui, "Name", &p.name);
        read_only(ui, "From", &format!("{}; {}; {}", mm(p.a[0]), mm(p.a[1]), mm(p.a[2])));
        read_only(ui, "To", &format!("{}; {}; {}", mm(p.b[0]), mm(p.b[1]), mm(p.b[2])));
        read_only(ui, "Impedance", &format!("{:.1} Ω", p.z0));
    });
    weak(ui, "A template's port follows its variables, and its impedance is the Z0 variable.");
}

/// A coordinate system's place, or the global one's lack of any.
fn coord(ui: &mut egui::Ui, st: &mut Studio, id: Option<u32>) {
    theme::caption(ui, "Coordinate system");
    let table = VarTable::of(&st.project);
    let vars = st.project.vars().unwrap_or_default();
    let Some(id) = id else {
        grid(ui, "params_coord_global", |ui| {
            read_only(ui, "Name", "Global system");
            read_only(ui, "Origin", "0; 0; 0");
        });
        work_plane(ui, st);
        weak(ui, "New shapes are drawn in the system in bold in the tree. Add another to draw on something tilted or set apart.");
        return;
    };
    let Some(mut c) = st.project.model.coord(id).cloned() else { return };
    let before = c.clone();
    grid(ui, "params_coord", |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(150.0));
        ui.end_row();
        model3d::point3(ui, st, &format!("coord{id}_o"), "Origin", &mut c.origin, Unit::Length, &table, &vars);
        for (k, axis) in ["x", "y", "z"].iter().enumerate() {
            model3d::field(ui, st, &format!("coord{id}_t{k}"), &format!("Turn about {axis}"), &mut c.turn[k], Unit::Angle, &table, &vars);
        }
    });
    if c != before {
        model3d::begin_edit(st);
        if let Some(slot) = st.project.model.coords.iter_mut().find(|x| x.id == id) {
            *slot = c;
        }
    }
    work_plane(ui, st);
    let drawn: Vec<String> = st.project.model.drawn_in(id).iter().map(|f| f.name.clone()).collect();
    weak(ui, if drawn.is_empty() { "Nothing is drawn in it yet.".to_string() } else { format!("Drawn in it: {}.", drawn.join(", ")) });
}

fn work_plane(ui: &mut egui::Ui, st: &mut Studio) {
    ui.horizontal(|ui| {
        ui.label("Work plane");
        theme::segmented(ui, &mut st.ui.m3.work_axis, &[(2, "XY"), (1, "XZ"), (0, "YZ")]);
    });
}

fn boundary(ui: &mut egui::Ui, st: &Studio) {
    theme::caption(ui, "Boundary conditions");
    let (method, what) = if st.ui.chosen.is_time_domain() {
        let ground = st.ui.plan.as_ref().is_some_and(|p| p.ground);
        ("A cell mesh with an absorbing boundary", if ground { "Absorbing layers on five sides, and a perfect conductor along the ground plane at the bottom." } else { "Absorbing layers on all six sides, standing in for open space." })
    } else {
        ("The method of moments", "Open space all round: radiation leaves and doesn't come back, so there's no box to set up. Metal is what's meshed.")
    };
    grid(ui, "params_boundary", |ui| {
        read_only(ui, "Solver", st.ui.chosen.label());
        read_only(ui, "Method", method);
    });
    weak(ui, what);
    weak(ui, "These follow from the solver and aren't set by hand. Choose the solver under Settings.");
}

fn settings(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Frequency and solver");
    let mut s = st.project.setup.clone();
    let before = s.clone();
    grid(ui, "params_settings", |ui| {
        ui.label("Sweep from");
        ghz_field(ui, &mut s.f_lo);
        ui.end_row();
        ui.label("Sweep to");
        ghz_field(ui, &mut s.f_hi);
        ui.end_row();
        ui.label("Points");
        ui.add(egui::DragValue::new(&mut s.points).range(11..=20001));
        ui.end_row();
        ui.label("Solver");
        egui::ComboBox::from_id_salt("params_solver").selected_text(s.solver.label()).width(170.0).show_ui(ui, |ui| {
            for k in [SolverKind::Auto, SolverKind::MomSurface, SolverKind::MomWire, SolverKind::Fit, SolverKind::Fdtd] {
                ui.selectable_value(&mut s.solver, k, k.label());
            }
        });
        ui.end_row();
    });
    if s != before {
        st.project.setup = s;
    }
    if let Some(p) = &st.ui.setup_problem {
        verdict(ui, false, p);
    }
    if ui.button(theme::label(icon::PLAY_CIRCLE, "All of the solver's settings")).clicked() {
        st.ui.page = super::Page::Setup;
    }
}

fn material_panel(ui: &mut egui::Ui, st: &mut Studio, name: &str) {
    let f = st.project.f0();
    if let Some(i) = st.project.materials.iter().position(|m| m.name == name) {
        theme::caption(ui, "Your material");
        let mut m = st.project.materials[i].clone();
        let before = m.clone();
        // The name is its id, so it only changes when it's finished: on the way to "Paper 2" it
        // would be "Paper", which is taken.
        let mut text = match &st.ui.tree.material_name {
            Some((of, t)) if of == name => t.clone(),
            _ => m.name.clone(),
        };
        let mut finished = false;
        grid(ui, "params_user_material", |ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0));
            if r.changed() {
                st.ui.tree.material_name = Some((name.to_string(), text.clone()));
            }
            finished = r.lost_focus();
            ui.end_row();
            ui.label("Kind");
            let mut metal = matches!(m.kind, MatKind::Metal { .. });
            if theme::segmented(ui, &mut metal, &[(false, "Dielectric"), (true, "Metal")]) {
                m.kind = if metal { MatKind::Metal { sigma: 1e7 } } else { MatKind::Dielectric { eps_r: 2.0, tan_d: 0.001, f_tan: 10e9, mu_r: 1.0 } };
            }
            ui.end_row();
            match &mut m.kind {
                MatKind::Metal { sigma } => {
                    ui.label("Conductivity");
                    let mut ms = *sigma / 1e6;
                    if ui.add(egui::DragValue::new(&mut ms).range(0.001..=100.0).speed(0.05).suffix(" MS/m")).changed() {
                        *sigma = ms * 1e6;
                    }
                    ui.end_row();
                }
                MatKind::Dielectric { eps_r, tan_d, .. } => {
                    ui.label("εr");
                    ui.add(egui::DragValue::new(eps_r).range(1.0..=1000.0).speed(0.01));
                    ui.end_row();
                    ui.label("tan δ");
                    ui.add(egui::DragValue::new(tan_d).range(0.0..=10.0).speed(0.0005).max_decimals(5));
                    ui.end_row();
                }
                MatKind::Pec => {}
            }
        });
        if m != before {
            model3d::begin_edit(st);
            st.project.materials[i] = m;
        }
        if finished {
            let new = text.trim().to_string();
            st.ui.tree.material_name = None;
            if new != name {
                let taken = new.is_empty() || material::built_in().iter().any(|b| b.name.eq_ignore_ascii_case(&new)) || st.project.materials.iter().enumerate().any(|(k, o)| k != i && o.name.eq_ignore_ascii_case(&new));
                if taken {
                    st.ui.m3.note = Some(format!("'{new}' can't be its name: that one's empty or taken, and a material's name is how a part refers to it."));
                } else {
                    super::draw::remember(st);
                    st.project.materials[i].name = new.clone();
                    st.project.rename_material(name, &new);
                    st.ui.tree.pick = Pick::Material(new);
                    return;
                }
            }
        }
        use_buttons(ui, st, name, st.project.materials[i].is_conductor());
        if ui.button(theme::label(icon::TRASH, "Remove it")).clicked() {
            let uses = st.project.uses_of_material(name);
            if uses.is_empty() {
                super::draw::remember(st);
                st.project.materials.remove(i);
                st.ui.tree.pick = Pick::Nothing;
            } else {
                st.ui.m3.note = Some(format!("{name} is still in use: {}. Give those another material first.", uses.join(", ")));
            }
        }
        return;
    }
    let Some(m) = material::by_name(name) else {
        weak(ui, "It isn't in the library any more.");
        return;
    };
    theme::caption(ui, "Material");
    grid(ui, "params_material", |ui| {
        read_only(ui, "Name", &m.name);
        match m.kind {
            MatKind::Pec => {
                read_only(ui, "Kind", "Perfect conductor");
                read_only(ui, "Loss", "None");
            }
            MatKind::Metal { sigma } => {
                read_only(ui, "Kind", "Metal");
                read_only(ui, "Conductivity", &format!("{:.2} MS/m", sigma / 1e6));
                read_only(ui, "Skin depth", &format!("{:.2} µm at the design frequency", m.skin_depth(f) * 1e6));
            }
            MatKind::Dielectric { eps_r, tan_d, .. } => {
                read_only(ui, "Kind", "Dielectric");
                read_only(ui, "εr", &format!("{eps_r:.2}"));
                read_only(ui, "tan δ", &format!("{tan_d}"));
            }
        }
    });
    use_buttons(ui, st, name, m.is_conductor());
}

fn use_buttons(ui: &mut egui::Ui, st: &mut Studio, name: &str, conductor: bool) {
    if conductor {
        if st.project.metal == name {
            weak(ui, "This is the antenna's metal.");
        } else if ui.button("Use as the antenna's metal").clicked() {
            super::draw::remember(st);
            st.project.metal = name.to_string();
        }
    } else if st.project.substrate == name {
        weak(ui, "This is the antenna's substrate.");
    } else if ui.button("Use as the antenna's substrate").clicked() {
        super::draw::remember(st);
        st.project.substrate = name.to_string();
    }
}

/// With nothing picked: what the project is, and where to start.
fn overview(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Project");
    grid(ui, "params_overview", |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut st.project.name).desired_width(150.0));
        ui.end_row();
        read_only(ui, "Design frequency", &format!("{:.4} GHz", st.project.f0() / 1e9));
        read_only(ui, "Metal", &st.project.metal.clone());
        if st.project.substrate != "Air" {
            read_only(ui, "Substrate", &st.project.substrate.clone());
        }
        if let Some((lo, hi)) = overall(st) {
            read_only(ui, "Overall size", &format!("{:.1} × {:.1} × {:.1} mm", (hi[0] - lo[0]) * 1e3, (hi[1] - lo[1]) * 1e3, (hi[2] - lo[2]) * 1e3));
        }
    });
    if let Some(t) = antenna_core::templates::by_id(&st.project.template) {
        weak(ui, t.blurb);
    }
    weak(ui, "Pick anything in the tree above to see and change its numbers. Right-click a number to turn it into a variable.");
}

fn overall(st: &Studio) -> Option<(P3, P3)> {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    let mut any = false;
    for p in st.ui.parts.iter().filter(|p| !p.mesh.is_empty()) {
        let (a, b) = p.mesh.bounds();
        for k in 0..3 {
            lo[k] = lo[k].min(a[k]);
            hi[k] = hi[k].max(b[k]);
        }
        any = true;
    }
    any.then_some((lo, hi))
}

/// The whole panel.
pub fn panel(ui: &mut egui::Ui, st: &mut Studio) {
    ui.spacing_mut().item_spacing = vec2(8.0, 5.0);
    if let Some(id) = st.ui.m3.active {
        model3d::sketch_panel(ui, st, id);
        model3d::done_editing(ui, st);
        return;
    }
    model3d::forget_stale_selection(st);
    if st.ui.m3.sel.features().len() > 1 {
        model3d::selection_panel(ui, st);
        return;
    }
    if let Some(id) = st.ui.m3.sel.feature {
        feature(ui, st, id);
        model3d::inspect_panel(ui, st);
        model3d::done_editing(ui, st);
        return;
    }
    match st.ui.tree.pick.clone() {
        Pick::Nothing => overview(ui, st),
        Pick::Part(name) => part(ui, st, &name),
        Pick::Port(name) => port(ui, st, &name),
        Pick::Coord(c) => coord(ui, st, c),
        Pick::Boundary => boundary(ui, st),
        Pick::Meshing => model3d::mesh_panel(ui, st),
        Pick::Material(name) => material_panel(ui, st, &name),
        Pick::Settings => settings(ui, st),
    }
    model3d::inspect_panel(ui, st);
    model3d::done_editing(ui, st);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turning a field's number into a variable mustn't change what it comes to.
    #[test]
    fn a_plain_number_keeps_the_meaning_it_had_in_its_field() {
        assert_eq!(with_unit("17", "mm"), "17mm");
        assert_eq!(with_unit(" 30 ", "deg"), "30deg");
        assert_eq!(with_unit("6", ""), "6", "a count stays a count");
        assert_eq!(with_unit("1", ""), "1", "so does a direction");
        assert_eq!(with_unit("L/2", "mm"), "L/2", "a formula says for itself");
        assert_eq!(with_unit("17mm", "mm"), "17mm");
    }
}

#[cfg(test)]
mod new_name_tests {
    use super::*;
    use antenna_core::project::Project;

    fn studio() -> Studio {
        let mut st = Studio::default();
        st.project = Project::from_template("custom", Some("1GHz")).unwrap();
        st
    }

    fn name(name: &str, was: &str, alone: bool) -> NewName {
        NewName { name: name.into(), unit: "mm", was: was.into(), alone, field: egui::Id::new("field") }
    }

    #[test]
    fn the_dialog_opens_with_what_the_name_was_typed_over() {
        let st = studio();
        assert_eq!(value_from(&name("a1", "4.5mm", true), &st), "4.5mm");
        assert_eq!(value_from(&name("a1", "4.5mm", false), &st), "", "a1+10mm says nothing about a1 alone");
        assert_eq!(value_from(&name("a1", "0", true), &st), "", "what an empty field starts as");
        assert_eq!(value_from(&name("a1", "b+1", true), &st), "", "doesn't work out either");
    }

    #[test]
    fn names_are_asked_about_one_at_a_time_and_only_if_still_missing() {
        let ctx = egui::Context::default();
        let mut st = studio();
        st.ui.new_names.extend([name("a1", "", false), name("f0", "", false), name("a2", "", false)]);
        ask_new_names(&ctx, &mut st);
        assert_eq!(st.ui.add_var.as_ref().map(|d| d.name.as_str()), Some("a1"));
        assert!(asking(&ctx, &st));
        ask_new_names(&ctx, &mut st);
        assert_eq!(st.ui.add_var.as_ref().map(|d| d.name.as_str()), Some("a1"), "the open one stays");
        st.ui.add_var = None;
        ask_new_names(&ctx, &mut st);
        assert_eq!(st.ui.add_var.as_ref().map(|d| d.name.as_str()), Some("a2"), "f0 exists, so it's skipped");
        st.ui.add_var = None;
        ask_new_names(&ctx, &mut st);
        assert!(st.ui.add_var.is_none());
        assert!(!asking(&ctx, &st));
    }

    #[test]
    fn a_plain_number_means_what_it_meant_in_the_field() {
        assert_eq!(with_unit("4.5", "mm"), "4.5mm");
        assert_eq!(with_unit("4.5", ""), "4.5");
        assert_eq!(with_unit("L/2", "mm"), "L/2");
    }
}
