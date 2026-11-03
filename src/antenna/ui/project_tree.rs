//! The Project panel: everything the antenna is made of, in one tree. Objects with the steps that
//! made them, coordinate systems, ports, the mesh, results and materials. Picking a row shows its
//! parameters in the panel under it.

use std::collections::HashSet;

use antenna_core::material;
use antenna_core::modeler::Kind;
use bevy_egui::egui::{self, Align2, FontId, Response, Sense, Stroke, StrokeKind, pos2, vec2};

use super::model3d::{self, Cmd, Glyph, Picked, Selection, glyph_of, small_glyph};
use super::Page;
use crate::antenna::Studio;
use crate::theme::{self, icon};

const ROW_H: f32 = 23.0;
const INDENT: f32 = 15.0;

/// What's picked in the tree, when it isn't a step of the model. Those are in `m3.sel`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Pick {
    #[default]
    Nothing,
    /// A part from a template or the flat tools, by name.
    Part(String),
    Port(String),
    /// A coordinate system. `None` is the global one.
    Coord(Option<u32>),
    Boundary,
    Meshing,
    Material(String),
    Settings,
}

#[derive(Default)]
pub struct TreeUi {
    pub pick: Pick,
    /// A user material's name while it's being typed: which material, and what's typed so far.
    pub material_name: Option<(String, String)>,
    /// Rows the other way from how they start.
    flipped: HashSet<String>,
}

impl TreeUi {
    fn is_open(&self, key: &str, by_default: bool) -> bool {
        self.flipped.contains(key) != by_default
    }

    fn flip(&mut self, key: &str) {
        if !self.flipped.remove(key) {
            self.flipped.insert(key.to_string());
        }
    }
}

#[derive(Clone, Copy)]
enum Mark<'a> {
    None,
    Icon(&'a str),
    Model(Glyph),
}

struct Row<'a> {
    depth: u8,
    text: &'a str,
    mark: Mark<'a>,
    /// Grey text at the right: what it's made of, how many.
    note: &'a str,
    selected: bool,
    /// Has children, and whether they show.
    open: Option<bool>,
    dim: bool,
    danger: bool,
    strong: bool,
    /// An eye at the row's end, and whether it's seen.
    eye: Option<bool>,
}

impl Default for Row<'_> {
    fn default() -> Self {
        Row { depth: 0, text: "", mark: Mark::None, note: "", selected: false, open: None, dim: false, danger: false, strong: false, eye: None }
    }
}

#[derive(Default)]
struct Hit {
    click: bool,
    double: bool,
    expander: bool,
    eye: bool,
    /// Ctrl or Shift was down.
    adding: bool,
}

fn row(ui: &mut egui::Ui, w: f32, r: &Row) -> (Hit, Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
    let p = ui.painter().clone();
    if r.selected {
        p.rect_filled(rect, 3.0, theme::ACCENT_SOFT);
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, theme::HOVER);
    }
    let cy = rect.center().y;
    let mut x = rect.left() + 8.0 + INDENT * f32::from(r.depth);
    let mut hit = Hit { adding: ui.input(|i| i.modifiers.ctrl || i.modifiers.shift), ..Default::default() };
    if let Some(open) = r.open {
        let b = egui::Rect::from_center_size(pos2(x + 5.0, cy), vec2(11.0, 11.0));
        let e = ui.interact(b.expand(3.0), resp.id.with("exp"), Sense::click());
        p.rect(b, 1.5, theme::PANEL, Stroke::new(1.0, if e.hovered() { theme::WEAK } else { theme::STROKE_STRONG }), StrokeKind::Inside);
        p.hline((b.left() + 2.5)..=(b.right() - 2.5), cy, Stroke::new(1.0, theme::TEXT));
        if !open {
            p.vline(b.center().x, (b.top() + 2.5)..=(b.bottom() - 2.5), Stroke::new(1.0, theme::TEXT));
        }
        hit.expander = e.clicked();
    }
    x += 15.0;
    match r.mark {
        Mark::None => {}
        Mark::Icon(g) => {
            p.text(pos2(x + 7.0, cy), Align2::CENTER_CENTER, g, FontId::proportional(14.0), if r.dim { theme::FAINT } else { theme::WEAK });
            x += 19.0;
        }
        Mark::Model(g) => {
            small_glyph(ui, pos2(x + 7.0, cy), 17.0, g, !r.dim);
            x += 19.0;
        }
    }
    let mut right = rect.right() - 6.0;
    if let Some(seen) = r.eye {
        let e = egui::Rect::from_center_size(pos2(right - 8.0, cy), vec2(18.0, 18.0));
        let hit_eye = ui.interact(e, resp.id.with("eye"), Sense::click()).on_hover_text(if seen { "Hide it" } else { "Show it" });
        let colour = if !seen { theme::FAINT } else if hit_eye.hovered() { theme::TEXT } else { theme::WEAK };
        p.text(e.center(), Align2::CENTER_CENTER, if seen { icon::EYE } else { icon::EYE_SLASH }, FontId::proportional(14.0), colour);
        hit.eye = hit_eye.clicked();
        right -= 20.0;
    }
    if !r.note.is_empty() {
        let g = p.layout_no_wrap(r.note.to_string(), FontId::proportional(11.5), theme::WEAK);
        let wide = g.size().x.min((right - x) * 0.5);
        let mut clip = p.clone();
        clip.set_clip_rect(egui::Rect::from_min_max(pos2(right - wide, rect.top()), pos2(right, rect.bottom())));
        clip.galley(pos2(right - wide, cy - g.size().y / 2.0), g, theme::WEAK);
        right -= wide + 6.0;
    }
    let colour = if r.danger { theme::DANGER } else if r.dim { theme::FAINT } else { theme::TEXT };
    let font = if r.strong { theme::bold(13.0) } else { FontId::proportional(13.0) };
    let mut job = egui::text::LayoutJob::simple_singleline(r.text.to_string(), font, colour);
    job.wrap = egui::text::TextWrapping { max_width: (right - x).max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = p.layout_job(job);
    p.galley(pos2(x, cy - g.size().y / 2.0), g, colour);
    hit.click = resp.clicked() && !hit.expander && !hit.eye;
    hit.double = resp.double_clicked();
    (hit, resp)
}

/// A heading of the tree. Its text picks it if it has a page of its own in Parameters.
fn section(ui: &mut egui::Ui, st: &mut Studio, w: f32, key: &str, text: &str, count: usize, by_default: bool) -> bool {
    let open = st.ui.tree.is_open(key, by_default);
    let note = if count > 0 { count.to_string() } else { String::new() };
    let (hit, _) = row(ui, w, &Row { text, open: Some(open), note: &note, strong: true, ..Default::default() });
    if hit.click || hit.expander {
        st.ui.tree.flip(key);
    }
    open
}

fn pick_other(st: &mut Studio, pick: Pick) {
    st.ui.m3.sel = Selection::default();
    st.ui.draw.selected = if let Pick::Part(name) = &pick { st.project.extra.iter().position(|e| &e.name == name) } else { None };
    st.ui.tree.pick = pick;
}

/// A click on one of the model's objects or steps. With Ctrl or Shift it adds to what's selected.
fn pick_model(st: &mut Studio, feature: u32, body: Option<u32>, step: bool, adding: bool) {
    st.ui.tree.pick = Pick::Nothing;
    if adding {
        model3d::toggle_pick(st, Picked { feature, body });
        st.ui.m3.sel.step = step;
    } else {
        st.ui.m3.sel = Selection { feature: Some(feature), body, step, ..Default::default() };
    }
}

/// The menu on a model row.
fn model_menu(resp: &Response, st: &mut Studio, feature: u32, body: Option<u32>, is_sketch: bool, index: usize, step: bool) {
    resp.context_menu(|ui| {
        let mut ask = None;
        if is_sketch && ui.button(theme::label(icon::NOTE_PENCIL, "Edit this sketch")).clicked() {
            model3d::open_sketch(st, feature);
            ui.close();
        }
        // Acts on this row, as what this row is: the step or the whole object. Several selected
        // stay selected if this is one of them.
        let select = |st: &mut Studio| {
            let among = !st.ui.m3.sel.more.is_empty() && (st.ui.m3.sel.has_feature(feature) || body.is_some_and(|b| st.ui.m3.sel.bodies().contains(&b)));
            if among {
                st.ui.m3.sel.step = step;
            } else {
                st.ui.m3.sel = Selection { feature: Some(feature), body, step, ..Default::default() };
            }
        };
        for (text, glyph, c) in [("Copy", icon::COPY, Cmd::Copy), ("Cut", icon::X, Cmd::ClipCut), ("Delete", icon::TRASH, Cmd::Delete)] {
            if ui.button(theme::label(glyph, text)).clicked() {
                select(st);
                ask = Some(c);
                ui.close();
            }
        }
        ui.separator();
        let off = st.project.model.feature(feature).is_some_and(|f| f.off);
        if ui.button(if off { "Switch it back on" } else { "Switch it off, leaving it out of the solve" }).clicked() {
            super::draw::remember(st);
            if let Some(f) = st.project.model.feature_mut(feature) {
                f.off = !f.off;
            }
            ui.close();
        }
        if ui.button(theme::label(icon::ARROW_COUNTER_CLOCKWISE, "Roll back to just before this")).clicked() {
            super::draw::remember(st);
            st.project.model.rollback = Some(index);
            ui.close();
        }
        if let Some(c) = ask {
            model3d::run_cmd(st, c);
        }
    });
}

fn objects(ui: &mut egui::Ui, st: &mut Studio, w: f32) {
    let objects = st.project.model.objects();
    let copies: Vec<(u32, u32, String, String)> = st.ui.built.bodies.iter().filter(|b| b.copy).map(|b| (b.id, b.last, b.name.clone(), b.material.clone())).collect();
    let loose = st.project.model.loose_steps();
    let parts = st.ui.all_parts.clone();
    let open = section(ui, st, w, "objects", "Objects", objects.len() + copies.len() + parts.len(), true);
    if !open {
        return;
    }
    if objects.is_empty() && copies.is_empty() && parts.is_empty() && loose.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new("Nothing yet. Pick a shape on the ribbon, or a template under Design.").size(12.0).color(theme::WEAK)).wrap());
    }
    let errors = st.ui.built.errors.clone();
    let rollback = st.project.model.rollback;
    let index_of = |st: &Studio, id: u32| st.project.model.features.iter().position(|f| f.id == id).unwrap_or(0);
    if let Some(r) = rollback {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(format!("Rolled back to step {r}.")).size(12.0).color(theme::WARNING));
            if ui.small_button("Roll forward").clicked() {
                super::draw::remember(st);
                st.project.model.rollback = None;
            }
        });
    }
    let metal = st.project.metal.clone();
    for o in &objects {
        let body = st.ui.built.bodies.iter().find(|b| b.id == o.id).cloned();
        let key = format!("object {}", o.id);
        // Showing the step that's selected, so a new fillet can be seen and deleted on its own.
        let step_picked = |st: &Studio, s: u32| st.ui.m3.sel.has_feature(s) && (st.ui.m3.sel.step || st.ui.m3.sel.body.is_none());
        let inside = o.steps.iter().any(|s| *s != o.id && step_picked(st, *s));
        let expanded = st.ui.tree.is_open(&key, false) || inside;
        let failed = errors.iter().find(|e| o.steps.contains(&e.0)).map(|e| e.1.clone());
        let selected = !st.ui.m3.sel.step && (st.ui.m3.sel.has_feature(o.id) || st.ui.m3.sel.bodies().contains(&o.id));
        let hidden = st.ui.m3.hidden.contains(&o.id);
        let material = body.as_ref().map_or(String::new(), |b| if b.material.is_empty() { metal.clone() } else { b.material.clone() });
        let glyph = st.project.model.feature(o.id).map_or(Glyph::Box_, |f| glyph_of(&f.kind));
        let (hit, resp) = row(ui, w, &Row { depth: 1, text: &o.name, mark: Mark::Model(glyph), note: &material, selected, open: Some(expanded), dim: hidden, danger: failed.is_some(), eye: body.is_some().then_some(!hidden), ..Default::default() });
        let resp = match &failed {
            Some(e) => resp.on_hover_text(e.as_str()),
            None => resp,
        };
        let at = index_of(st, o.id);
        model_menu(&resp, st, o.id, body.as_ref().map(|b| b.id), false, at, false);
        if hit.expander {
            st.ui.tree.flip(&key);
        }
        if hit.eye {
            st.ui.m3.toggle_hidden(o.id);
            st.ui.hidden_changed = true;
        }
        if hit.click {
            pick_model(st, o.id, body.as_ref().map(|b| b.id), false, hit.adding);
        }
        if !expanded {
            continue;
        }
        for s in &o.steps {
            let Some(f) = st.project.model.feature(*s).cloned() else { continue };
            let error = errors.iter().find(|e| e.0 == *s).map(|e| e.1.clone());
            let selected = step_picked(st, *s);
            let beyond = rollback.is_some_and(|r| index_of(st, *s) >= r);
            let is_sketch = matches!(f.kind, Kind::Sketch { .. });
            let (hit, resp) = row(ui, w, &Row { depth: 2, text: &f.name, mark: Mark::Model(glyph_of(&f.kind)), selected, dim: f.off || beyond, danger: error.is_some(), ..Default::default() });
            let resp = match &error {
                Some(e) => resp.on_hover_text(e.as_str()),
                None => resp,
            };
            let at = index_of(st, *s);
            model_menu(&resp, st, *s, Some(o.id), is_sketch, at, true);
            if hit.click {
                pick_model(st, *s, Some(o.id), true, hit.adding);
            }
            if hit.double && is_sketch {
                model3d::open_sketch(st, *s);
            }
        }
    }
    for (id, last, name, material) in &copies {
        let selected = st.ui.m3.sel.bodies().contains(id);
        let hidden = st.ui.m3.hidden.contains(id);
        let note = if material.is_empty() { metal.clone() } else { material.clone() };
        let (hit, _) = row(ui, w, &Row { depth: 1, text: name, mark: Mark::Model(Glyph::Pattern), note: &note, selected, dim: hidden, eye: Some(!hidden), ..Default::default() });
        if hit.eye {
            st.ui.m3.toggle_hidden(*id);
            st.ui.hidden_changed = true;
        }
        if hit.click {
            pick_model(st, *last, Some(*id), false, hit.adding);
        }
    }
    // Parts that come from a template or the flat tools, which have no steps.
    for part in &parts {
        let selected = st.ui.tree.pick == Pick::Part(part.name.clone());
        let mark = match part.word {
            "Wire" => Glyph::Wire,
            "Cylinder" => Glyph::Cylinder,
            "Sphere" => Glyph::Sphere,
            "Box" => Glyph::Box_,
            _ => Glyph::Sheet,
        };
        let unseen = st.ui.m3.hidden_parts.contains(&part.name);
        let (hit, resp) = row(ui, w, &Row { depth: 1, text: &part.name, mark: Mark::Model(mark), note: &part.material, selected, dim: part.hidden || unseen, eye: (!part.hidden).then_some(!unseen), ..Default::default() });
        if hit.eye {
            // Only out of sight, as for a body. Leaving it out of the solve is a separate choice.
            if !st.ui.m3.hidden_parts.remove(&part.name) {
                st.ui.m3.hidden_parts.insert(part.name.clone());
            }
            st.ui.hidden_changed = true;
        }
        resp.context_menu(|ui| {
            let off = st.project.hidden.contains(&part.name);
            if ui.button(if off { "Switch it back on" } else { "Switch it off, leaving it out of the solve" }).clicked() {
                super::draw::remember(st);
                if off {
                    st.project.hidden.retain(|h| *h != part.name);
                } else {
                    st.project.hidden.push(part.name.clone());
                }
                ui.close();
            }
        });
        if hit.click {
            pick_other(st, Pick::Part(part.name.clone()));
        }
    }
    // Sketches nothing's made from yet.
    for s in &loose {
        let Some(f) = st.project.model.feature(*s).cloned() else { continue };
        let is_sketch = matches!(f.kind, Kind::Sketch { .. });
        let error = errors.iter().find(|e| e.0 == *s).map(|e| e.1.clone());
        let (hit, resp) = row(ui, w, &Row { depth: 1, text: &f.name, mark: Mark::Model(glyph_of(&f.kind)), selected: st.ui.m3.sel.has_feature(*s), dim: f.off, danger: error.is_some(), ..Default::default() });
        let resp = match &error {
            Some(e) => resp.on_hover_text(e.as_str()),
            None => resp,
        };
        let at = index_of(st, *s);
        model_menu(&resp, st, *s, None, is_sketch, at, true);
        if hit.click {
            pick_model(st, *s, None, true, hit.adding);
        }
        if hit.double && is_sketch {
            model3d::open_sketch(st, *s);
        }
    }
}

fn coordinates(ui: &mut egui::Ui, st: &mut Studio, w: f32) {
    let list: Vec<(u32, String)> = st.project.model.coords.iter().map(|c| (c.id, c.name.clone())).collect();
    if !section(ui, st, w, "coordinates", "Coordinates", 0, true) {
        return;
    }
    let active = st.ui.m3.coord;
    let (hit, _) = row(ui, w, &Row { depth: 1, text: "Global system", mark: Mark::Icon(icon::CROSSHAIR), strong: active.is_none(), selected: st.ui.tree.pick == Pick::Coord(None), ..Default::default() });
    if hit.click {
        st.ui.m3.coord = None;
        pick_other(st, Pick::Coord(None));
    }
    for (id, name) in &list {
        let (hit, resp) = row(ui, w, &Row { depth: 1, text: name, mark: Mark::Icon(icon::CROSSHAIR), strong: active == Some(*id), selected: st.ui.tree.pick == Pick::Coord(Some(*id)), ..Default::default() });
        if hit.click {
            st.ui.m3.coord = Some(*id);
            pick_other(st, Pick::Coord(Some(*id)));
        }
        resp.context_menu(|ui| {
            if ui.button(theme::label(icon::TRASH, "Delete")).clicked() {
                super::draw::remember(st);
                let moved = st.project.model.drawn_in(*id).len();
                st.project.model.remove_coord(*id);
                if moved > 0 {
                    st.ui.m3.note = Some(format!("{moved} things drawn in {name} are now in the global system, so they've moved unless it was at the origin. Undo brings it back."));
                }
                if st.ui.m3.coord == Some(*id) {
                    st.ui.m3.coord = None;
                }
                if st.ui.tree.pick == Pick::Coord(Some(*id)) {
                    st.ui.tree.pick = Pick::Coord(None);
                }
                ui.close();
            }
        });
    }
}

fn ports(ui: &mut egui::Ui, st: &mut Studio, w: f32) {
    // Every feed step, built or not: one switched off or failing still needs a row to be dealt with.
    let feeds: Vec<(u32, String, bool)> = st.project.model.features.iter().filter(|f| matches!(f.kind, Kind::Feed { .. } | Kind::Probe { .. })).map(|f| (f.id, f.name.clone(), f.off)).collect();
    let others: Vec<String> = st.ui.model.as_ref().map_or(Vec::new(), |g| g.ports.iter().map(|p| p.name.clone()).filter(|n| !feeds.iter().any(|f| f.1 == *n)).collect());
    if !section(ui, st, w, "ports", "Ports", feeds.len() + others.len(), true) {
        return;
    }
    let errors = st.ui.built.errors.clone();
    for (id, name, off) in &feeds {
        let error = errors.iter().find(|e| e.0 == *id).map(|e| e.1.clone());
        let selected = st.ui.m3.sel.has_feature(*id);
        let (hit, resp) = row(ui, w, &Row { depth: 1, text: name, mark: Mark::Icon(icon::PLUG), selected, dim: *off, danger: error.is_some(), ..Default::default() });
        let resp = match &error {
            Some(e) => resp.on_hover_text(e.as_str()),
            None => resp,
        };
        let at = st.project.model.features.iter().position(|f| f.id == *id).unwrap_or(0);
        model_menu(&resp, st, *id, None, false, at, true);
        if hit.click {
            pick_model(st, *id, None, true, false);
        }
    }
    for name in &others {
        let (hit, _) = row(ui, w, &Row { depth: 1, text: name, mark: Mark::Icon(icon::PLUG), selected: st.ui.tree.pick == Pick::Port(name.clone()), ..Default::default() });
        if hit.click {
            pick_other(st, Pick::Port(name.clone()));
        }
    }
}

fn results(ui: &mut egui::Ui, st: &mut Studio, w: f32) {
    // Shut until there's something in it.
    let ready = st.latest().is_some();
    if !section(ui, st, w, "results", "Data visualizations", 0, ready) {
        return;
    }
    for (page, text, glyph) in [(Page::SParams, "S-parameters", icon::CHART_LINE), (Page::Smith, "Smith chart", icon::TARGET), (Page::FarField, "Far field", icon::BROADCAST), (Page::Fields, "Fields and currents", icon::WAVE_SINE)] {
        let (hit, resp) = row(ui, w, &Row { depth: 1, text, mark: Mark::Icon(glyph), selected: st.ui.page == page, dim: !ready, ..Default::default() });
        if !ready {
            resp.on_hover_text("Run a simulation first");
        }
        if hit.click {
            st.ui.page = page;
        }
    }
}

fn materials(ui: &mut egui::Ui, st: &mut Studio, w: f32) {
    if !section(ui, st, w, "materials", "Materials", 0, false) {
        return;
    }
    let (metal, substrate) = (st.project.metal.clone(), st.project.substrate.clone());
    let user: Vec<String> = st.project.materials.iter().map(|m| m.name.clone()).collect();
    let built_in: Vec<String> = material::built_in().into_iter().map(|m| m.name).collect();
    let open = {
        let open = st.ui.tree.is_open("built-in", true);
        let (hit, _) = row(ui, w, &Row { depth: 1, text: "Built-in", open: Some(open), ..Default::default() });
        if hit.click || hit.expander {
            st.ui.tree.flip("built-in");
        }
        open
    };
    let mark = |ui: &mut egui::Ui, st: &mut Studio, name: &String| {
        let note = if *name == metal { "metal" } else if *name == substrate { "substrate" } else { "" };
        let (hit, _) = row(ui, w, &Row { depth: 2, text: name, note, selected: st.ui.tree.pick == Pick::Material(name.clone()), strong: !note.is_empty(), ..Default::default() });
        if hit.click {
            pick_other(st, Pick::Material(name.clone()));
        }
    };
    if open {
        for name in &built_in {
            mark(ui, st, name);
        }
    }
    let (hit, _) = row(ui, w, &Row { depth: 1, text: "User defined", open: (!user.is_empty()).then_some(true), ..Default::default() });
    let plus = egui::Rect::from_center_size(ui.min_rect().right_bottom() - vec2(16.0, ROW_H / 2.0), vec2(18.0, 18.0));
    let add = ui.interact(plus, ui.id().with("add_material"), Sense::click()).on_hover_text("Add a material of your own");
    ui.painter().text(plus.center(), Align2::CENTER_CENTER, icon::PLUS_CIRCLE, FontId::proportional(14.0), if add.hovered() { theme::ACCENT } else { theme::WEAK });
    if add.clicked() {
        super::draw::remember(st);
        let name = (1..).map(|k| format!("My material {k}")).find(|n| material::by_name(n).is_none()).unwrap_or_default();
        st.project.materials.push(material::Material::dielectric(&name, 2.0, 0.001));
        pick_other(st, Pick::Material(name));
    }
    let _ = hit;
    for name in &user {
        mark(ui, st, name);
    }
}

/// The whole tree.
pub fn show(ui: &mut egui::Ui, st: &mut Studio, w: f32) {
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    objects(ui, st, w);
    coordinates(ui, st, w);
    ports(ui, st, w);
    for (text, pick, glyph) in [("Boundary conditions", Pick::Boundary, icon::BOUNDING_BOX), ("Meshing", Pick::Meshing, icon::GRID_FOUR)] {
        let (hit, _) = row(ui, w, &Row { text, mark: Mark::Icon(glyph), selected: st.ui.tree.pick == pick, strong: true, ..Default::default() });
        if hit.click {
            pick_other(st, pick);
        }
    }
    results(ui, st, w);
    materials(ui, st, w);
    let (hit, _) = row(ui, w, &Row { text: "Settings", mark: Mark::Icon(icon::GEAR_SIX), selected: st.ui.tree.pick == Pick::Settings, strong: true, ..Default::default() });
    if hit.click {
        pick_other(st, Pick::Settings);
    }
}
