//! Design pages: the template gallery, the variable table, parts and ports,
//! materials.

use antenna_core::geometry::{Shape, read_stl};
use antenna_core::material::{self, MatKind};
use antenna_core::project::{ExtraPort, ExtraPrim, ExtraSolid, Param, Project};
use antenna_core::report::param_text;
use antenna_core::templates::{self, Category};
use bevy_egui::egui::{self, RichText, vec2};

use super::expr_field::{VarTable, expr_field, expr_line, length_line};
use super::{group, view3d, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

pub fn gallery(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "New antenna");
    weak(ui, "Each one starts from textbook dimensions for your frequency, trimmed where full-wave runs showed the formula is off. All of it stays editable in the variable table.");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut st.ui.gallery_search).hint_text("Search").desired_width(150.0));
        let label = st.ui.gallery_cat.map_or("All kinds", Category::label);
        egui::ComboBox::from_id_salt("gallery_cat").selected_text(label).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.ui.gallery_cat, None, "All kinds");
            for c in Category::ALL {
                ui.selectable_value(&mut st.ui.gallery_cat, Some(c), c.label());
            }
        });
    });
    let needle = st.ui.gallery_search.to_lowercase();
    let cat = st.ui.gallery_cat;
    let list: Vec<&'static templates::Template> = templates::all()
        .iter()
        .filter(|t| cat.is_none_or(|c| c == t.category))
        .filter(|t| needle.is_empty() || t.title.to_lowercase().contains(&needle) || t.blurb.to_lowercase().contains(&needle))
        .collect();
    let width = ui.available_width();
    let cols = ((width / 140.0).floor() as usize).max(2);
    let w = (width - 8.0 * (cols as f32 - 1.0)) / cols as f32;
    let f0_now = st.project.params.iter().find(|p| p.name == "f0").map(|p| p.expr.clone());
    for row in list.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for t in row {
                let geom = st.ui.thumbs.entry(t.id).or_insert_with(|| Project::from_template(t.id, None).and_then(|p| p.geometry()).unwrap_or_default());
                let (rect, resp) = ui.allocate_exact_size(vec2(w, w * 0.72 + 34.0), egui::Sense::click());
                let on = st.project.template == t.id;
                let pic = egui::Rect::from_min_size(rect.min, vec2(w, w * 0.72));
                if geom.solids.is_empty() {
                    ui.painter().rect_filled(pic, 4.0, theme::RAISED);
                    ui.painter().text(pic.center(), egui::Align2::CENTER_CENTER, icon::PLUS, egui::FontId::proportional(22.0), theme::WEAK);
                } else {
                    view3d::thumbnail(ui, pic, geom);
                }
                let stroke = if on { egui::Stroke::new(2.0, theme::ACCENT) } else if resp.hovered() { egui::Stroke::new(1.0, theme::STROKE_STRONG) } else { egui::Stroke::new(1.0, theme::STROKE) };
                ui.painter().rect_stroke(pic, 4.0, stroke, egui::StrokeKind::Inside);
                let g = ui.painter().layout(t.title.to_string(), egui::FontId::proportional(13.0), theme::TEXT, w);
                ui.painter().galley(pic.left_bottom() + vec2(0.0, 3.0), g, theme::TEXT);
                let resp = resp.on_hover_text(t.blurb).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    // Keep the design frequency across templates.
                    let f0 = f0_now.as_deref();
                    match Project::from_template(t.id, f0) {
                        Ok(p) => {
                            st.project = p;
                            st.ui.file = None;
                            // A blank model has nothing to tune yet: straight to drawing it.
                            st.ui.page = if t.id == "custom" { super::Page::Draw } else { super::Page::Variables };
                            st.ui.message = None;
                            st.ui.cam = Default::default();
                            st.ui.opt_vars.clear();
                            st.ui.tol.clear();
                            st.ui.goals.clear();
                            st.ui.sweep_var.clear();
                        }
                        Err(e) => st.ui.message = Some((format!("{e:#}"), false)),
                    }
                }
            }
        });
    }
}

// The variable table's fixed columns. The expression takes whatever the card has
// left, so widening the card widens the field.
const VAR_NAME_W: f32 = 62.0;
const VAR_VALUE_W: f32 = 88.0;
const VAR_TRASH_W: f32 = 28.0;
const VAR_LINE_H: f32 = 28.0;
const VAR_PAD: i8 = 4;

fn var_expr_w(ui: &egui::Ui) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    (ui.available_width() - VAR_VALUE_W - VAR_TRASH_W - 2.0 * gap).max(80.0)
}

fn var_cell(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(vec2(w, VAR_LINE_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(w);
        ui.set_min_height(VAR_LINE_H);
        add(ui);
    });
}

fn var_header(ui: &mut egui::Ui) {
    let title = |ui: &mut egui::Ui, text: &str| {
        ui.label(RichText::new(text).size(12.0).color(theme::WEAK));
    };
    egui::Frame::new().inner_margin(egui::Margin::symmetric(VAR_PAD, 0)).show(ui, |ui| {
        ui.horizontal(|ui| {
            var_cell(ui, VAR_NAME_W, |ui| title(ui, "Name"));
            let w = var_expr_w(ui);
            var_cell(ui, w, |ui| title(ui, "Value or expression"));
            var_cell(ui, VAR_VALUE_W, |ui| title(ui, "Evaluates to"));
        });
    });
}

/// One variable. A long formula wraps inside its field rather than scrolling out
/// of sight, and the meaning sits under it, where it can't squeeze the field.
/// One row: whether to remove it, and whether it's being pointed at or typed in.
fn param_row(ui: &mut egui::Ui, row: usize, p: &mut Param, value: Option<f64>, table: &VarTable) -> (bool, bool) {
    let mut remove = false;
    let mut typing = false;
    let stripe = if row % 2 == 0 { ui.visuals().faint_bg_color } else { egui::Color32::TRANSPARENT };
    let framed = egui::Frame::new().fill(stripe).corner_radius(3).inner_margin(egui::Margin::same(VAR_PAD)).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            var_cell(ui, VAR_NAME_W, |ui| {
                ui.add(egui::Label::new(RichText::new(&p.name).monospace()).truncate());
            });
            let w = var_expr_w(ui);
            ui.vertical(|ui| {
                ui.set_width(w);
                ui.spacing_mut().item_spacing.y = 2.0;
                let field = expr_field(ui, ("var", &p.name), &mut p.expr, table, Some(&p.name), None, |t| {
                    egui::TextEdit::multiline(t).desired_rows(1).desired_width(w).min_size(vec2(0.0, VAR_LINE_H)).vertical_align(egui::Align::Center).return_key(None)
                });
                // Still one line of text: Enter finishes, and a pasted line break goes.
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    field.surrender_focus();
                }
                if field.changed() {
                    p.expr.retain(|c| c != '\n' && c != '\r');
                }
                typing = field.has_focus();
                ui.add(egui::Label::new(RichText::new(&p.note).size(12.0).color(theme::WEAK)).wrap());
            });
            var_cell(ui, VAR_VALUE_W, |ui| {
                ui.add(egui::Label::new(RichText::new(value.map_or("–".into(), |v| param_text(&p.name, v))).color(theme::WEAK)).truncate());
            });
            var_cell(ui, VAR_TRASH_W, |ui| {
                // The theme's padding makes this wider than the column, and a row that
                // overflows widens every row below it.
                ui.spacing_mut().button_padding.x = 4.0;
                if !p.derived && p.name != "f0" && p.name != "lambda0" && theme::icon_button(ui, icon::TRASH, "Remove this variable").clicked() {
                    remove = true;
                }
            });
        });
    });
    (remove, typing || ui.rect_contains_pointer(framed.response.rect))
}

/// The rows of the variable table, without the helper values. The Draw page shows them too. When
/// a drawing has named every corner, those fold away under the sizes.
pub(super) fn variable_rows(ui: &mut egui::Ui, st: &mut Studio, table: &VarTable) {
    let corners = antenna_core::dims::corner_rows(&st.project);
    let before = st.project.params.clone();
    let mut remove = None;
    let mut focus = None;
    let mut rows_where = |ui: &mut egui::Ui, st: &mut Studio, folded: bool| {
        let rows = st.project.params.iter_mut().enumerate().filter(|(_, p)| !p.derived && corners.contains(&p.name) == folded);
        for (row, (i, p)) in rows.enumerate() {
            let v = table.value(&p.name);
            let (gone, pointed) = param_row(ui, row, p, v, table);
            if gone {
                remove = Some(i);
            }
            if pointed {
                focus = Some(p.name.clone());
            }
        }
    };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        var_header(ui);
        rows_where(ui, st, false);
    });
    if !corners.is_empty() {
        let title = RichText::new(format!("Corner positions ({})", corners.len())).color(theme::WEAK);
        egui::CollapsingHeader::new(title).id_salt("variable_corners").show(ui, |ui| {
            weak(ui, "Where each corner of the drawing sits. Point at a row to see it on the model.");
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                rows_where(ui, st, true);
            });
        });
    }
    if let Some(i) = remove {
        st.project.params.remove(i);
    }
    if focus.is_some() {
        st.ui.dim_focus = focus;
    }
    // A stray drag on a variable moves everything that uses it, so it has to be possible to take
    // back. One undo step for each row typed in or dragged, as on the other fields.
    if st.project.params != before {
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

pub fn variables(ui: &mut egui::Ui, st: &mut Studio) {
    let tmpl = templates::by_id(&st.project.template);
    ui.horizontal(|ui| {
        theme::caption(ui, tmpl.map_or("Model", |t| t.title));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Change antenna").clicked() {
                st.ui.page = super::Page::Gallery;
            }
        });
    });
    if let Some(t) = tmpl {
        weak(ui, t.blurb);
    }
    weak(ui, "Expressions may use the other variables, er and tand of the substrate, c0, pi, and units such as 12mm or 2.45GHz.");
    let show_derived = st.project.params.iter().any(|p| p.derived);
    let table = VarTable::of(&st.project);
    variable_rows(ui, st, &table);
    // Under the rows, so a half-typed word doesn't move the field it's being typed in. A
    // bad table stops the model too, with the same words, and once is enough.
    let table_error = st.project.vars().err().map(|e| format!("{e:#}"));
    if let Some(e) = &table_error {
        super::verdict(ui, false, e);
    }
    if let Err(e) = &st.ui.geometry
        && !e.is_empty()
        && Some(e) != table_error.as_ref()
    {
        super::verdict(ui, false, e);
    }
    ui.horizontal(|ui| {
        if ui.button(theme::label(icon::PLUS, "Add a variable")).clicked() {
            let mut k = 1;
            while st.project.params.iter().any(|p| p.name == format!("var{k}")) {
                k += 1;
            }
            st.project.params.push(Param::new(&format!("var{k}"), "0", "Your own variable"));
        }
        if ui.button("Reset to the template").clicked() {
            let f0 = st.project.params.iter().find(|p| p.name == "f0").map(|p| p.expr.clone());
            if let Ok(p) = Project::from_template(&st.project.template.clone(), f0.as_deref()) {
                // A variable of your own stays: what you drew may be written in it.
                let own: Vec<Param> = st.project.params.iter().filter(|r| !p.params.iter().any(|t| t.name == r.name)).cloned().collect();
                st.project.params = p.params;
                st.project.params.extend(own);
                let _ = st.project.reset_band();
            }
        }
        if ui.button("Fit the sweep to f0").on_hover_text("Sets the sweep band and far-field frequency around the design frequency").clicked() {
            let _ = st.project.reset_band();
        }
    });
    if show_derived {
        egui::CollapsingHeader::new(RichText::new("Derived values").color(theme::WEAK)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (row, p) in st.project.params.iter_mut().filter(|p| p.derived).enumerate() {
                let v = table.value(&p.name);
                let _ = param_row(ui, row, p, v, &table);
            }
        });
    }
    if let Ok(g) = &st.ui.geometry {
        let (lo, hi) = g.bounds();
        let lambda = antenna_core::C0 / st.project.f0();
        let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        weak(ui, format!("Overall size {:.1} × {:.1} × {:.1} mm, {:.2} λ across.", size[0] * 1e3, size[1] * 1e3, size[2] * 1e3, size.iter().copied().fold(0.0, f64::max) / lambda));
    }
}

#[derive(Clone)]
pub struct NewPart {
    pub kind: usize,
    pub name: String,
    pub material: String,
    pub a: [String; 3],
    pub b: [String; 3],
    pub r: String,
    pub stl_scale: f64,
}

impl Default for NewPart {
    fn default() -> Self {
        let z = || String::from("0");
        Self { kind: 0, name: "part".into(), material: "PEC".into(), a: [z(), z(), z()], b: ["10mm".into(), "10mm".into(), z()], r: "1mm".into(), stl_scale: 0.001 }
    }
}

#[derive(Clone)]
pub struct NewPort {
    pub a: [String; 3],
    pub b: [String; 3],
    pub width: String,
    pub z0: String,
}

impl Default for NewPort {
    fn default() -> Self {
        let z = || String::from("0");
        Self { a: [z(), z(), z()], b: [z(), z(), "1mm".into()], width: z(), z0: "50".into() }
    }
}

fn xyz(ui: &mut egui::Ui, whose: &str, label: &str, v: &mut [String; 3], table: &VarTable) {
    ui.label(label);
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 16.0) / 3.0;
        for (k, s) in v.iter_mut().enumerate() {
            length_line(ui, (whose, label, k), s, table, w.max(40.0));
        }
    });
    ui.end_row();
}

pub(super) fn shape_word(s: &Shape) -> &'static str {
    match s {
        Shape::Box { min, max } => {
            if (0..3).any(|k| max[k] - min[k] <= 0.0) {
                "Sheet"
            } else {
                "Box"
            }
        }
        Shape::Cylinder { .. } => "Cylinder",
        Shape::Sphere { .. } => "Sphere",
        Shape::Polygon { .. } => "Outline",
        Shape::Wire { .. } => "Wire",
        Shape::Tris { .. } => "Mesh",
    }
}

pub fn parts(ui: &mut egui::Ui, st: &mut Studio) {
    let table = VarTable::of(&st.project);
    theme::caption(ui, "Parts");
    // Everything the template and the user made, with on/off.
    let full = {
        let mut p = st.project.clone();
        p.hidden.clear();
        p.scatterer().ok()
    };
    let mut toggle = None;
    if let Some(g) = &full {
        egui::Grid::new("studio_parts").num_columns(3).spacing(vec2(10.0, 3.0)).striped(true).show(ui, |ui| {
            for s in &g.solids {
                let hidden = st.project.hidden.contains(&s.name);
                let mut on = !hidden;
                if ui.checkbox(&mut on, &s.name).changed() {
                    toggle = Some(s.name.clone());
                }
                ui.label(RichText::new(shape_word(&s.shape)).size(12.0).color(theme::WEAK));
                ui.label(RichText::new(&s.material).size(12.0).color(theme::WEAK));
                ui.end_row();
            }
        });
    }
    if let Some(name) = toggle {
        if let Some(i) = st.project.hidden.iter().position(|h| *h == name) {
            st.project.hidden.remove(i);
        } else {
            st.project.hidden.push(name);
        }
    }
    let mut drop_part = None;
    for (i, e) in st.project.extra.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Added: {}", e.name)).size(12.5));
            if theme::icon_button(ui, icon::TRASH, "Remove this part").clicked() {
                drop_part = Some(i);
            }
        });
    }
    if let Some(i) = drop_part {
        st.project.extra.remove(i);
    }

    super::import::panel(ui, st);
    group(ui, |ui| {
        theme::caption(ui, "Add a part");
        const KINDS: [&str; 5] = ["Box or sheet", "Cylinder", "Sphere", "Wire", "Import STL"];
        let np = &mut st.ui.new_part;
        egui::Grid::new("studio_newpart").num_columns(2).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
            ui.label("Kind");
            egui::ComboBox::from_id_salt("np_kind").selected_text(KINDS[np.kind]).show_ui(ui, |ui| {
                for (k, n) in KINDS.iter().enumerate() {
                    ui.selectable_value(&mut np.kind, k, *n);
                }
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut np.name).desired_width(150.0));
            ui.end_row();
            ui.label("Material");
            egui::ComboBox::from_id_salt("np_mat").selected_text(np.material.clone()).show_ui(ui, |ui| {
                for m in material::library() {
                    ui.selectable_value(&mut np.material, m.name.clone(), m.name.clone());
                }
            });
            ui.end_row();
            match np.kind {
                0 => {
                    xyz(ui, "part", "From x y z", &mut np.a, &table);
                    xyz(ui, "part", "To x y z", &mut np.b, &table);
                }
                1 | 3 => {
                    xyz(ui, "part", "Start x y z", &mut np.a, &table);
                    xyz(ui, "part", "End x y z", &mut np.b, &table);
                    ui.label("Radius");
                    length_line(ui, "part radius", &mut np.r, &table, 100.0);
                    ui.end_row();
                }
                2 => {
                    xyz(ui, "part", "Centre x y z", &mut np.a, &table);
                    ui.label("Radius");
                    length_line(ui, "part radius", &mut np.r, &table, 100.0);
                    ui.end_row();
                }
                _ => {
                    ui.label("File unit");
                    egui::ComboBox::from_id_salt("np_scale").selected_text(if np.stl_scale == 0.001 { "mm" } else if np.stl_scale == 0.01 { "cm" } else if np.stl_scale == 1.0 { "m" } else { "inch" }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut np.stl_scale, 0.001, "mm");
                        ui.selectable_value(&mut np.stl_scale, 0.01, "cm");
                        ui.selectable_value(&mut np.stl_scale, 1.0, "m");
                        ui.selectable_value(&mut np.stl_scale, 0.0254, "inch");
                    });
                    ui.end_row();
                }
            }
        });
        weak(ui, "Coordinates are expressions, so a part follows the variables: L/2, 0.25*lambda0, 12mm.");
        let label = if np.kind == 4 { "Choose an STL file…" } else { "Add the part" };
        if ui.button(theme::label(icon::PLUS, label)).clicked() {
            let np = st.ui.new_part.clone();
            let prim = match np.kind {
                0 => Some(ExtraPrim::Box { min: np.a.clone(), max: np.b.clone() }),
                1 => Some(ExtraPrim::Cylinder { a: np.a.clone(), b: np.b.clone(), r: np.r.clone() }),
                2 => Some(ExtraPrim::Sphere { c: np.a.clone(), r: np.r.clone() }),
                3 => Some(ExtraPrim::Wire { pts: vec![np.a.clone(), np.b.clone()], r: np.r.clone() }),
                _ => rfd::FileDialog::new().add_filter("STL model", &["stl"]).pick_file().and_then(|path| {
                    let read = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| read_stl(&b, np.stl_scale));
                    match read {
                        Ok(tris) => Some(ExtraPrim::Imported { file: path.display().to_string(), tris, solid: true }),
                        Err(e) => {
                            st.ui.message = Some((format!("Could not read the file: {e:#}"), false));
                            None
                        }
                    }
                }),
            };
            if let Some(prim) = prim {
                let mut name = np.name.trim().to_string();
                if name.is_empty() {
                    name = "part".into();
                }
                let taken = |n: &str, p: &Project| p.extra.iter().any(|e| e.name == n);
                let mut k = 2;
                let base = name.clone();
                while taken(&name, &st.project) {
                    name = format!("{base} {k}");
                    k += 1;
                }
                st.project.extra.push(ExtraSolid { name, material: np.material, prim });
            }
        }
    });

    theme::caption(ui, "Ports");
    let mut z0_edit: Vec<(String, f64)> = Vec::new();
    if let Ok(g) = &st.ui.geometry {
        for (i, p) in g.ports.iter().enumerate() {
            let axis = ["x", "y", "z"][p.axis()];
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} · {}", i + 1, p.name)).strong());
                ui.label(RichText::new(format!("{:.1} Ω, along {axis}, gap {:.2} mm", p.z0, antenna_core::geometry::norm(antenna_core::geometry::sub(p.b, p.a)) * 1e3)).size(12.0).color(theme::WEAK));
            });
        }
        z0_edit.clear();
    }
    let mut drop_port = None;
    for (i, p) in st.project.extra_ports.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Added: {}", p.name)).size(12.5));
            if theme::icon_button(ui, icon::TRASH, "Remove this port").clicked() {
                drop_port = Some(i);
            }
        });
    }
    if let Some(i) = drop_port {
        st.project.extra_ports.remove(i);
    }
    weak(ui, "A template's port impedance is the Z0 variable. Add a lumped port below for more ports; each one gives a row and a column of the S-matrix.");
    group(ui, |ui| {
        theme::caption(ui, "Add a lumped port");
        let np = &mut st.ui.new_port;
        egui::Grid::new("studio_newport").num_columns(2).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
            xyz(ui, "port", "From x y z", &mut np.a, &table);
            xyz(ui, "port", "To x y z", &mut np.b, &table);
            ui.label("Width");
            length_line(ui, "port width", &mut np.width, &table, 100.0);
            ui.end_row();
            ui.label("Impedance");
            expr_line(ui, "port z0", &mut np.z0, &table, 100.0);
            ui.end_row();
        });
        weak(ui, "The two ends must differ along exactly one axis and touch the conductors they feed.");
        if ui.button(theme::label(icon::PLUS, "Add the port")).clicked() {
            let np = st.ui.new_port.clone();
            let n = st.project.extra_ports.len() + 1;
            st.project.extra_ports.push(ExtraPort { name: format!("added {n}"), a: np.a, b: np.b, width: np.width, z0: np.z0 });
        }
    });
}

pub fn materials(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "This antenna");
    if !st.project.stack.is_empty() {
        super::weak(ui, format!("In layers: {}. Change them from Design an RFID tag, or ask the assistant.", st.project.stack.describe()));
        if ui.button("Take it out of its layers").clicked() {
            st.project.stack = Default::default();
        }
    }
    let lib = material::library();
    let tmpl = templates::by_id(&st.project.template);
    let has_substrate = tmpl.is_some_and(|t| t.substrate != "Air") || st.project.substrate != "Air";
    egui::Grid::new("studio_mats").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Metal");
        egui::ComboBox::from_id_salt("mat_metal").selected_text(st.project.metal.clone()).width(190.0).show_ui(ui, |ui| {
            for m in lib.iter().filter(|m| m.is_conductor()) {
                ui.selectable_value(&mut st.project.metal, m.name.clone(), &m.name);
            }
        });
        ui.end_row();
        if has_substrate {
            ui.label("Substrate");
            egui::ComboBox::from_id_salt("mat_sub").selected_text(st.project.substrate.clone()).width(190.0).show_ui(ui, |ui| {
                for m in lib.iter().filter(|m| !m.is_conductor()) {
                    ui.selectable_value(&mut st.project.substrate, m.name.clone(), &m.name);
                }
            });
            ui.end_row();
        }
    });
    if has_substrate {
        weak(ui, "Changing the substrate re-evaluates every dimension that depends on er, so the antenna stays tuned to the design frequency as far as the formulas go.");
    }
    let f = st.project.f0();
    if let Some(m) = material::by_name(&st.project.metal) {
        match m.kind {
            MatKind::Pec => weak(ui, "PEC has no loss: radiation efficiency stays 100 % apart from the dielectric."),
            _ => weak(ui, format!("Skin depth at the design frequency: {:.2} µm. FDTD uses it on flat sheets (35 µm thick) only; solids and wires stay lossless there. MoM uses it everywhere.", m.skin_depth(f) * 1e6)),
        }
    }
    theme::caption(ui, "Library");
    egui::Grid::new("studio_matlib").num_columns(3).spacing(vec2(14.0, 3.0)).striped(true).show(ui, |ui| {
        ui.label(RichText::new("Name").size(12.0).color(theme::WEAK));
        ui.label(RichText::new("Constant").size(12.0).color(theme::WEAK));
        ui.label(RichText::new("Loss").size(12.0).color(theme::WEAK));
        ui.end_row();
        for m in &lib {
            ui.label(&m.name);
            match m.kind {
                MatKind::Pec => {
                    ui.label("perfect");
                    ui.label("none");
                }
                MatKind::Metal { sigma } => {
                    ui.label(format!("σ {:.1} MS/m", sigma / 1e6));
                    ui.label(format!("{:.1} µm skin", m.skin_depth(f) * 1e6));
                }
                MatKind::Dielectric { eps_r, tan_d, .. } => {
                    ui.label(format!("εr {eps_r:.2}"));
                    if m.name == "Water" {
                        // Its loss follows a Debye curve, so the library's number is only one point on it.
                        ui.label(format!("tan δ {:.3} here", m.dielectric_kappa(f) / (2.0 * std::f64::consts::PI * f * antenna_core::EPS0 * eps_r)));
                    } else {
                        ui.label(format!("tan δ {tan_d}"));
                    }
                }
            }
            ui.end_row();
        }
    });
}
