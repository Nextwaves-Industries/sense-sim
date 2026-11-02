//! Bringing a drawing in: pick a file, see what the file holds, say what its layers
//! are and how it stands, then start a new antenna from it or add it to this one.

use antenna_core::cad::{self, CadDrawing, CadFeed, CadPlane, LayerPlacement, LayerRole, P2, Placement, RawDrawing, Resolve};
use antenna_core::geometry::triangulate;
use antenna_core::material;
use bevy_egui::egui::{self, Color32, Mesh, Pos2, RichText, Sense, Shape as EShape, Stroke, pos2, vec2};

use super::{charts, group, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

const UNITS: [(&str, f64); 6] = [("mm", 1e-3), ("cm", 1e-2), ("m", 1.0), ("inch", 0.0254), ("mil", 25.4e-6), ("µm", 1e-6)];

/// An open import.
pub struct Loaded {
    pub path: String,
    pub raw: RawDrawing,
    pub cad: CadDrawing,
    pub unit: f64,
    pub chord_mm: f64,
    pub plane: CadPlane,
    pub center: bool,
    pub f0: String,
    pub places: Vec<LayerPlacement>,
    pub use_feed: Vec<bool>,
    /// Filled metal per layer, for the picture.
    preview: Vec<Vec<[P2; 3]>>,
}

#[derive(Default)]
pub struct ImportUi {
    pub open: Option<Loaded>,
    pub error: Option<String>,
}

fn fill_of(c: &CadDrawing) -> Vec<Vec<[P2; 3]>> {
    c.layers
        .iter()
        .map(|l| {
            let mut out = Vec::new();
            for island in &l.islands {
                let pts = island.keyholed();
                out.extend(triangulate(&pts).into_iter().map(|t| [pts[t[0]], pts[t[1]], pts[t[2]]]));
            }
            out
        })
        .collect()
}

fn resolve(l: &mut Loaded) -> Result<(), String> {
    let cad = l.raw.resolve(Resolve { scale_to_m: l.unit, chord_tol: l.chord_mm * 1e-3 }).map_err(|e| format!("{e:#}"))?;
    let keep_places = l.places.len() == cad.layers.len();
    if !keep_places {
        l.places = cad::default_placements(&cad, "PEC");
    }
    if l.use_feed.len() != cad.feeds.len() {
        l.use_feed = vec![true; cad.feeds.len()];
    }
    l.preview = fill_of(&cad);
    l.cad = cad;
    Ok(())
}

/// Opens a file (or a stack of Gerber files) into the import panel.
pub fn load(st: &mut Studio, paths: &[std::path::PathBuf]) {
    let Some(first) = paths.first() else { return };
    let mut raw: Option<RawDrawing> = None;
    for path in paths {
        let name = path.display().to_string();
        let read = std::fs::read(path).map_err(anyhow::Error::from).and_then(|b| cad::read(&name, &b));
        match read {
            Ok(r) => match raw.as_mut() {
                Some(all) => all.merge(r),
                None => raw = Some(r),
            },
            Err(e) => {
                st.ui.import.error = Some(format!("{}: {e:#}", path.file_name().map_or(name.clone(), |n| n.to_string_lossy().to_string())));
                return;
            }
        }
    }
    let Some(raw) = raw else { return };
    let unit = raw.units_to_m.filter(|_| raw.format != "Gerber").unwrap_or(1e-3);
    let f0 = st.project.params.iter().find(|p| p.name == "f0").map_or("915MHz".to_string(), |p| p.expr.clone());
    let placeholder = CadDrawing { format: raw.format.clone(), layers: Vec::new(), feeds: Vec::new(), feeds_guessed: false, warnings: Vec::new() };
    let mut l = Loaded { path: first.display().to_string(), raw, cad: placeholder, unit, chord_mm: 0.05, plane: CadPlane::Flat, center: true, f0, places: Vec::new(), use_feed: Vec::new(), preview: Vec::new() };
    match resolve(&mut l) {
        Ok(()) => {
            st.ui.import.open = Some(l);
            st.ui.import.error = None;
        }
        Err(e) => st.ui.import.error = Some(e),
    }
}

/// The drawing as it reads, one colour per layer, with its feeds numbered.
fn picture(ui: &mut egui::Ui, l: &Loaded) {
    let Some((lo, hi)) = l.cad.bounds() else {
        weak(ui, "Nothing on the metal layers to show.");
        return;
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 190.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let (w, h) = ((hi[0] - lo[0]).max(1e-9), (hi[1] - lo[1]).max(1e-9));
    let s = ((rect.width() - 16.0) as f64 / w).min((rect.height() - 16.0) as f64 / h);
    let c = rect.center();
    let at = |q: P2| -> Pos2 { pos2(c.x + ((q[0] - (lo[0] + hi[0]) / 2.0) * s) as f32, c.y - ((q[1] - (lo[1] + hi[1]) / 2.0) * s) as f32) };
    for (k, layer) in l.cad.layers.iter().enumerate() {
        if !l.places.get(k).is_some_and(|x| x.include) || layer.role == LayerRole::Feed {
            continue;
        }
        let base = charts::PALETTE[k % charts::PALETTE.len()];
        let fill = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 120);
        let mut mesh = Mesh::default();
        for t in l.preview.get(k).map_or(&[][..], Vec::as_slice) {
            let n = mesh.vertices.len() as u32;
            for q in t {
                mesh.colored_vertex(at(*q), fill);
            }
            mesh.add_triangle(n, n + 1, n + 2);
        }
        p.add(EShape::mesh(mesh));
        for island in &layer.islands {
            let mut rings = vec![&island.outer];
            rings.extend(island.holes.iter());
            for ring in rings {
                p.add(EShape::closed_line(ring.iter().map(|q| at(*q)).collect(), Stroke::new(1.0, base)));
            }
        }
    }
    let mut number = 0;
    for (k, f) in l.cad.feeds.iter().enumerate() {
        if !l.use_feed.get(k).copied().unwrap_or(false) {
            continue;
        }
        number += 1;
        let m = match f {
            CadFeed::Gap { a, b, .. } => {
                p.line_segment([at(*a), at(*b)], Stroke::new(2.5, theme::DANGER));
                at([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0])
            }
            CadFeed::Probe { at: q } => at(*q),
        };
        p.circle_filled(m, 6.5, theme::DANGER);
        p.text(m, egui::Align2::CENTER_CENTER, number.to_string(), egui::FontId::proportional(9.5), Color32::WHITE);
    }
}

fn apply(st: &mut Studio, new_antenna: bool) {
    let Some(l) = st.ui.import.open.as_ref() else { return };
    let feeds: Vec<CadFeed> = l.cad.feeds.iter().zip(&l.use_feed).filter(|x| *x.1).map(|x| x.0.clone()).collect();
    let how = Placement { plane: l.plane, center: l.center, layers: l.places.clone(), feeds };
    let done = if new_antenna {
        cad::new_project(&l.cad, &l.path, &l.f0, &how).map(|(p, a)| (Some(p), a))
    } else {
        let mut p = st.project.clone();
        cad::add_to_project(&mut p, &l.cad, &l.path, &how).map(|a| (Some(p), a))
    };
    match done {
        Ok((Some(p), added)) => {
            super::draw::remember(st);
            st.project = p;
            // Frames the new model, wherever the drawing puts it.
            st.ui.draw.frame = None;
            st.ui.draw.selected = None;
            st.ui.import.open = None;
            st.ui.import.error = None;
            st.ui.message = Some((format!("Added {} parts and {} ports from the drawing.{}", added.parts, added.ports, if added.ports == 0 { " Add a port below before solving." } else { "" }), added.ports > 0));
        }
        Ok((None, _)) => {}
        Err(e) => st.ui.import.error = Some(format!("{e:#}")),
    }
}

/// The import panel on the Parts page.
pub fn panel(ui: &mut egui::Ui, st: &mut Studio) {
    if st.ui.import.open.is_none() {
        group(ui, |ui| {
            theme::caption(ui, "Import a drawing");
            weak(ui, "A DXF, or Gerber copper layers. Several Gerber files make one stack. A layer named FEED or PORT marks feeds: a line across a gap, or a small circle for a probe.");
            if ui.button(theme::label(icon::FILE_ARROW_UP, "Choose a file…")).clicked()
                && let Some(paths) = rfd::FileDialog::new().add_filter("Drawing", &["dxf", "gbr", "ger", "gtl", "gbl", "art", "pho"]).add_filter("All files", &["*"]).pick_files()
            {
                load(st, &paths);
            }
            if let Some(e) = st.ui.import.error.clone() {
                verdict(ui, false, &e);
            }
        });
        return;
    }
    let mut close = false;
    let mut again = false;
    let mut act = None;
    let metal_names: Vec<String> = material::library().into_iter().filter(|m| m.is_conductor()).map(|m| m.name).collect();
    let table = super::expr_field::VarTable::of(&st.project);
    let substrate = st.project.substrate.clone();
    let error = st.ui.import.error.clone();
    let Some(l) = st.ui.import.open.as_mut() else { return };
    group(ui, |ui| {
        ui.horizontal(|ui| {
            let name = std::path::Path::new(&l.path).file_name().map_or(l.path.clone(), |n| n.to_string_lossy().to_string());
            theme::caption(ui, &format!("{} · {}", l.cad.format, name));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::icon_button(ui, icon::TRASH, "Leave this drawing out").clicked() {
                    close = true;
                }
            });
        });
        egui::Grid::new("cad_opts").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
            if l.cad.format != "Gerber" {
                ui.label("Drawing unit");
                let now = UNITS.iter().find(|u| (u.1 - l.unit).abs() < l.unit * 1e-9).map_or("custom", |u| u.0);
                egui::ComboBox::from_id_salt("cad_unit").selected_text(now).show_ui(ui, |ui| {
                    for (name, v) in UNITS {
                        if ui.selectable_value(&mut l.unit, v, name).changed() {
                            again = true;
                        }
                    }
                });
                ui.end_row();
            }
            ui.label("Stands");
            egui::ComboBox::from_id_salt("cad_plane").selected_text(l.plane.label()).show_ui(ui, |ui| {
                for p in CadPlane::ALL {
                    ui.selectable_value(&mut l.plane, p, p.label());
                }
            });
            ui.end_row();
            ui.label("Curves within");
            if ui.add(egui::DragValue::new(&mut l.chord_mm).range(0.001..=5.0).speed(0.005).suffix(" mm").max_decimals(3)).on_hover_text("Fewer points mean fewer unknowns; closer follows the drawing better").drag_stopped() {
                again = true;
            }
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut l.center, "Move the metal's centre to the origin");
            ui.end_row();
        });
        picture(ui, l);
        if let Some((lo, hi)) = l.cad.bounds() {
            weak(ui, format!("{:.2} × {:.2} mm. Check this against the drawing: a wrong unit shows here first.", (hi[0] - lo[0]) * 1e3, (hi[1] - lo[1]) * 1e3));
        }
        theme::caption(ui, "Layers");
        egui::Grid::new("cad_layers").num_columns(4).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
            for h in ["Layer", "It is", "Height", "Material"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for k in 0..l.cad.layers.len() {
                let layer = l.cad.layers[k].clone();
                let color = charts::PALETTE[k % charts::PALETTE.len()];
                let mut role = layer.role;
                ui.horizontal(|ui| {
                    if layer.role == LayerRole::Metal || layer.role == LayerRole::Board {
                        ui.checkbox(&mut l.places[k].include, "");
                    }
                    ui.label(RichText::new("●").color(color));
                    ui.label(RichText::new(&layer.name).size(12.5)).on_hover_text(format!("{} outlines, {:.2} mm², {} open paths", layer.islands.len(), layer.area() * 1e6, layer.open.len()));
                });
                egui::ComboBox::from_id_salt(("cad_role", k)).selected_text(role.label()).width(92.0).show_ui(ui, |ui| {
                    for r in LayerRole::ALL {
                        ui.selectable_value(&mut role, r, r.label());
                    }
                });
                if role != layer.role {
                    l.raw.layers[k].role = role;
                    l.places[k].include = role == LayerRole::Metal || role == LayerRole::Board;
                    again = true;
                }
                match role {
                    LayerRole::Metal => {
                        super::expr_field::expr_line(ui, ("cad_height", k), &mut l.places[k].height, &table, 66.0);
                        egui::ComboBox::from_id_salt(("cad_mat", k)).selected_text(l.places[k].material.clone()).width(84.0).show_ui(ui, |ui| {
                            for m in &metal_names {
                                ui.selectable_value(&mut l.places[k].material, m.clone(), m);
                            }
                        });
                    }
                    LayerRole::Board => {
                        ui.label(RichText::new("metal to metal").size(11.5).color(theme::WEAK));
                        ui.label(RichText::new(substrate.clone()).size(11.5).color(theme::WEAK));
                    }
                    _ => {
                        ui.label("");
                        ui.label("");
                    }
                }
                ui.end_row();
            }
        });
        weak(ui, "A height is an expression, so a stack can follow a variable: 0, h, 1.6mm. Change Stands above to put the drawing on its edge.");
        theme::caption(ui, "Feeds");
        if l.cad.feeds.is_empty() {
            weak(ui, "None found. Draw a line across the gap on a layer named FEED, or add a lumped port by its coordinates after importing.");
        } else {
            if l.cad.feeds_guessed {
                weak(ui, "These are the narrowest gaps in the metal; the file doesn't mark any. Each ticked one becomes a port.");
            }
            for (k, f) in l.cad.feeds.iter().enumerate() {
                if let Some(on) = l.use_feed.get_mut(k) {
                    ui.checkbox(on, RichText::new(f.describe()).size(12.5));
                }
            }
        }
        for w in &l.cad.warnings {
            verdict(ui, false, w);
        }
        if !l.raw.skipped.is_empty() {
            let list: Vec<String> = l.raw.skipped.iter().map(|(k, n)| format!("{n} {}", k.to_lowercase())).collect();
            weak(ui, format!("Left out, not outlines: {}.", list.join(", ")));
        }
        if let Some(e) = &error {
            verdict(ui, false, e);
        }
        ui.horizontal(|ui| {
            ui.label("Design frequency");
            ui.add(egui::TextEdit::singleline(&mut l.f0).desired_width(72.0));
            if ui.button(theme::label(icon::PLUS, "New antenna")).on_hover_text("Replaces the current design").clicked() {
                act = Some(true);
            }
            if ui.button("Add to this model").clicked() {
                act = Some(false);
            }
        });
    });
    if close {
        st.ui.import.open = None;
        st.ui.import.error = None;
        return;
    }
    if again && let Some(l) = st.ui.import.open.as_mut() {
        let done = resolve(l);
        st.ui.import.error = done.err();
    }
    if let Some(new_antenna) = act {
        apply(st, new_antenna);
    }
}
