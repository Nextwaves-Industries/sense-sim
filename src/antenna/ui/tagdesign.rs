//! The page for designing an RFID tag: what it has to do and what it's in, the search, and the options
//! it comes back with. The assistant works the same search through `antenna_tag_design`.

use std::collections::HashMap;

use antenna_core::geometry::Geometry;
use antenna_core::stack::PRESETS;
use antenna_core::tag::CHIPS;
use antenna_core::tag_design::{Effort, FAMILIES, TagGoal, TagOption};
use bevy_egui::egui::{self, RichText, vec2};

use super::{group, verdict, view3d, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

const BANDS: [(&str, f64, f64); 4] = [("US, 902-928 MHz", 902.0, 928.0), ("EU, 865-868 MHz", 865.0, 868.0), ("Global, 860-960 MHz", 860.0, 960.0), ("China, 920-925 MHz", 920.0, 925.0)];

const EFFORTS: [(Effort, &str); 4] = [(Effort::Quick, "Quick, a first look"), (Effort::Normal, "Normal"), (Effort::Deep, "Deep"), (Effort::Exhaustive, "Exhaustive")];

/// What the page is set to.
pub struct TagUi {
    chip: String,
    band: usize,
    preset: usize,
    max_x_mm: f64,
    max_y_mm: f64,
    min_um: f64,
    effort: Effort,
    evolve: bool,
    /// Each option's model, for its picture, by what it scored.
    models: HashMap<String, Geometry>,
    pub tune: super::tagtune::TuneUi,
}

impl Default for TagUi {
    fn default() -> Self {
        Self { chip: "ucode9".into(), band: 0, preset: 1, max_x_mm: 95.0, max_y_mm: 25.0, min_um: 200.0, effort: Effort::Normal, evolve: false, models: HashMap::new(), tune: Default::default() }
    }
}

fn goal_of(u: &TagUi) -> Option<TagGoal> {
    let chip = antenna_core::tag::chip(&u.chip)?;
    let z = chip.z?;
    let (_, lo, hi) = BANDS[u.band];
    Some(TagGoal { f_lo: lo * 1e6, f_hi: hi * 1e6, chip_z: z, read_dbm: chip.read_dbm.unwrap_or(-22.0), eirp_dbm: 36.0, max_x: Some(u.max_x_mm * 1e-3), max_y: Some(u.max_y_mm * 1e-3), min_feature: u.min_um * 1e-6, stack: (PRESETS[u.preset].stack)(), metal: "Aluminium".into() })
}

fn option_card(ui: &mut egui::Ui, st: &mut Studio, o: &TagOption, w: f32) {
    group(ui, |ui| {
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(130.0, 96.0), egui::Sense::hover());
            let key = format!("{}-{:.9}", o.title, o.score.center_range_m);
            let model = st.ui.tag.models.entry(key).or_insert_with(|| o.project.geometry().unwrap_or_default());
            view3d::thumbnail(ui, rect, model);
            ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0, theme::STROKE), egui::StrokeKind::Inside);
            ui.vertical(|ui| {
                ui.set_max_width((w - 165.0).max(120.0));
                ui.label(RichText::new(&o.title).size(13.0).color(theme::TEXT));
                weak(ui, o.idea.clone());
                ui.horizontal_wrapped(|ui| {
                    for s in &o.stands_out {
                        theme::tag_chip(ui, s);
                    }
                });
                let s = &o.score;
                ui.label(format!("{:.1} m at the band's worst, {:.2} of the power to the chip, {:.1} dBi, {:.0} % radiated", s.worst_range_m, s.worst_transfer, s.gain_dbi, s.efficiency * 100.0));
                ui.label(format!("{:.1} x {:.1} mm", s.size_x * 1e3, s.size_y * 1e3));
                if let Some(b) = o.band {
                    weak(ui, format!("Above 70 % of its best from {:.0} to {:.0} MHz", b.0 / 1e6, b.1 / 1e6));
                }
                if let Some(worst) = o.if_changed.iter().min_by(|a, b| a.1.total_cmp(&b.1)) {
                    weak(ui, format!("Worst nearby surroundings: {} ({:.1} m)", worst.0.to_lowercase(), worst.1));
                }
                for w in &o.warnings {
                    verdict(ui, false, w);
                }
                if ui.add_enabled(!st.busy(), egui::Button::new("Open")).on_hover_text("Make this the current design, to run, sweep or draw on").clicked() {
                    let (title, project) = (o.title.clone(), o.project.clone());
                    st.open_tag_option(project);
                    st.ui.message = Some((format!("{title} opened, in its surroundings. Simulated, not measured."), true));
                }
            });
        });
    });
}

pub fn page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Design an RFID tag");
    weak(ui, format!("Searches {} different structures against the chip, band and what the tag will really be in, judged by read range. Simulated, not measured.", FAMILIES.len()));
    let busy = st.busy();
    let w = ui.available_width();
    group(ui, |ui| {
        let u = &mut st.ui.tag;
        theme::field_row(ui, "Chip", |ui| {
            let shown = antenna_core::tag::chip(&u.chip).map_or("", |c| c.name);
            egui::ComboBox::from_id_salt("tag_chip").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                for c in CHIPS.iter().filter(|c| c.z.is_some()) {
                    ui.selectable_value(&mut u.chip, c.id.to_string(), c.name);
                }
            });
        });
        theme::field_row(ui, "Band", |ui| {
            egui::ComboBox::from_id_salt("tag_band").selected_text(BANDS[u.band].0).width(220.0).show_ui(ui, |ui| {
                for (k, b) in BANDS.iter().enumerate() {
                    ui.selectable_value(&mut u.band, k, b.0);
                }
            });
        });
        theme::field_row(ui, "It's in", |ui| {
            egui::ComboBox::from_id_salt("tag_stack").selected_text(PRESETS[u.preset].label).width(220.0).show_ui(ui, |ui| {
                for (k, p) in PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut u.preset, k, p.label).on_hover_text(p.about);
                }
            });
        });
        weak(ui, (PRESETS[u.preset].stack)().describe());
        theme::field_row(ui, "Fits in", |ui| {
            ui.add(egui::DragValue::new(&mut u.max_x_mm).suffix(" mm").range(10.0..=400.0));
            ui.label("by");
            ui.add(egui::DragValue::new(&mut u.max_y_mm).suffix(" mm").range(5.0..=400.0));
        });
        theme::field_row(ui, "Min trace", |ui| {
            ui.add(egui::DragValue::new(&mut u.min_um).suffix(" µm").range(20.0..=3000.0));
        });
        theme::field_row(ui, "Search", |ui| {
            let shown = EFFORTS.iter().find(|e| e.0 == u.effort).map_or("", |e| e.1);
            egui::ComboBox::from_id_salt("tag_effort").selected_text(shown).width(180.0).show_ui(ui, |ui| {
                for (e, text) in EFFORTS {
                    ui.selectable_value(&mut u.effort, e, text);
                }
            });
        });
        ui.checkbox(&mut u.evolve, "Also evolve a free-form shape, cell by cell").on_hover_text("No assumed structure. Takes as long again.");
    });
    let goal = goal_of(&st.ui.tag);
    let can = goal.is_some() && !busy;
    // The run button takes the whole width, so the retune goes under it.
    if super::run_button(ui, &theme::label(icon::SPARKLE, "Find designs"), can)
        && let Some(g) = goal.clone()
    {
        let (effort, evolve) = (st.ui.tag.effort, st.ui.tag.evolve);
        st.ui.tag.models.clear();
        st.run_tag_design(g, Vec::new(), Vec::new(), effort, evolve.then_some((14, 6, 480)));
    }
    if ui.add_enabled(can, egui::Button::new("Retune the open design for this")).on_hover_text("What these surroundings do to the design that's open, and what small changes get back").clicked()
        && let Some(g) = goal.clone()
    {
        st.run_tag_retune(g, 0.15, 120);
    }
    super::tagtune::section(ui, st, goal.as_ref());
    let Some(run) = st.tag_run() else { return };
    if let Some(r) = &run.retune {
        group(ui, |ui| {
            ui.label(RichText::new("Retune").size(13.0).color(theme::TEXT));
            ui.label(&r.note);
            for c in &r.changes {
                weak(ui, format!("{}: {:.2} to {:.2} mm", c.0, c.1 * 1e3, c.2 * 1e3));
            }
            if ui.add_enabled(!busy, egui::Button::new("Use the retuned design")).clicked() {
                st.open_tag_option(r.project.clone());
            }
        });
    }
    if !run.options.is_empty() {
        ui.add_space(6.0);
        theme::caption(ui, "Options");
        for o in &run.options {
            option_card(ui, st, o, w);
            ui.add_space(4.0);
        }
    }
}
