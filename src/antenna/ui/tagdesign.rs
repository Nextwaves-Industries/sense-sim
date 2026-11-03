//! The page for designing an RFID tag: what it has to do and what it's in, then the assistant designs
//! it, with `antenna_tag_try`. Everything tried comes back here as options, each saying where it came
//! from. Searching the known shapes stays on the page for working without a model, and says what it is.

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
    /// A request for the assistant, on its way to it. See `hand_to_assistant`.
    pub to_assistant: Option<String>,
    /// The assistant is in the middle of something, as of the last frame.
    pub assistant_busy: bool,
    /// Why the assistant can't start, such as having no key.
    pub assistant_blocked: Option<String>,
}

impl Default for TagUi {
    fn default() -> Self {
        Self { chip: "ucode9".into(), band: 0, preset: 1, max_x_mm: 95.0, max_y_mm: 25.0, min_um: 200.0, effort: Effort::Normal, evolve: false, models: HashMap::new(), tune: Default::default(), to_assistant: None, assistant_busy: false, assistant_blocked: None }
    }
}

impl TagUi {
    /// Shows what a search is really for, whoever started it. What the page has no entry for stays as
    /// it was, and the page says what the search was for instead.
    pub fn follow(&mut self, g: &TagGoal) {
        if let Some(c) = CHIPS.iter().find(|c| c.z == Some(g.chip_z)) {
            self.chip = c.id.to_string();
        }
        if let Some(k) = BANDS.iter().position(|b| b.1 * 1e6 == g.f_lo && b.2 * 1e6 == g.f_hi) {
            self.band = k;
        }
        if let Some(k) = PRESETS.iter().position(|p| (p.stack)() == g.stack) {
            self.preset = k;
        }
        if let (Some(x), Some(y)) = (g.max_x, g.max_y) {
            self.max_x_mm = x * 1e3;
            self.max_y_mm = y * 1e3;
        }
        self.min_um = g.min_feature * 1e6;
    }
}

fn goal_of(u: &TagUi) -> Option<TagGoal> {
    let chip = antenna_core::tag::chip(&u.chip)?;
    let z = chip.z?;
    let (_, lo, hi) = BANDS[u.band];
    Some(TagGoal { f_lo: lo * 1e6, f_hi: hi * 1e6, chip_z: z, read_dbm: chip.read_dbm.unwrap_or(-22.0), eirp_dbm: 36.0, max_x: Some(u.max_x_mm * 1e-3), max_y: Some(u.max_y_mm * 1e-3), min_feature: u.min_um * 1e-6, stack: (PRESETS[u.preset].stack)(), metal: "Aluminium".into() })
}

/// What the page asks the assistant for: everything on the form, in words a person can read in the
/// chat, with the ids its tools take. The rest of how to go about it is in its instructions.
fn request_of(u: &TagUi) -> Option<String> {
    let chip = antenna_core::tag::chip(&u.chip)?;
    let (band, lo, hi) = BANDS[u.band];
    let preset = &PRESETS[u.preset];
    Some(format!(
        "Design a new RFID tag antenna, a structure of your own rather than a known shape.\nChip: {} (id {}).\nBand: {band} ({:.3} to {:.3} GHz).\nIt's in: {} (preset {}): {}.\nIt has to fit in {:.1} x {:.1} mm.\nThinnest trace and gap: {:.0} µm.",
        chip.name,
        chip.id,
        lo / 1e3,
        hi / 1e3,
        preset.label,
        preset.id,
        (preset.stack)().describe(),
        u.max_x_mm,
        u.max_y_mm,
        u.min_um,
    ))
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
                    theme::tag_chip(ui, o.origin.label());
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
    weak(ui, "The AI assistant designs a new antenna for this chip, band and surroundings. It draws structures of its own, simulates each one, works out what limits it and changes the structure. Simulated, not measured.");
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
    });
    let goal = goal_of(&st.ui.tag);
    let can = goal.is_some() && !busy;
    let thinking = st.ui.tag.assistant_busy;
    // The run button takes the whole width, so what else there is goes under it.
    if super::run_button(ui, &theme::label(icon::SPARKLE, "Design with AI"), can && !thinking) {
        st.ui.tag.to_assistant = request_of(&st.ui.tag);
    }
    if thinking {
        weak(ui, "The assistant is working. Follow it in its panel: what it tries appears under Options.");
    } else if let Some(why) = &st.ui.tag.assistant_blocked {
        verdict(ui, false, why);
    }
    super::tagtune::section(ui, st, goal.as_ref());
    egui::CollapsingHeader::new(RichText::new("Without AI").color(theme::TEXT)).id_salt("tag_without_ai").show(ui, |ui| {
        weak(ui, format!("Resizes {} known shapes against the requirements above. It doesn't invent anything: use it to compare, or with no model set up.", FAMILIES.len()));
        theme::field_row(ui, "Search", |ui| {
            let u = &mut st.ui.tag;
            let shown = EFFORTS.iter().find(|e| e.0 == u.effort).map_or("", |e| e.1);
            egui::ComboBox::from_id_salt("tag_effort").selected_text(shown).width(180.0).show_ui(ui, |ui| {
                for (e, text) in EFFORTS {
                    ui.selectable_value(&mut u.effort, e, text);
                }
            });
        });
        ui.checkbox(&mut st.ui.tag.evolve, "Also evolve a free-form shape, cell by cell").on_hover_text("Random changes kept when they read further. No assumed structure. Takes as long again.");
        if ui.add_enabled(can, egui::Button::new(format!("Search the {} known shapes", FAMILIES.len()))).clicked()
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
    });
    let Some(run) = st.tag_run() else { return };
    // Anything the form can't show, such as a band it has no entry for.
    if let Some(g) = run.goal.as_ref().filter(|g| goal.as_ref() != Some(*g)) {
        let size = g.max_x.zip(g.max_y).map_or("no size limit".to_string(), |(x, y)| format!("{:.1} x {:.1} mm", x * 1e3, y * 1e3));
        weak(ui, format!("These options are for {:.0}-{:.0} MHz, {size}, {}.", g.f_lo / 1e6, g.f_hi / 1e6, g.stack.describe()));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The assistant is told what's on the form, with the ids its tools need.
    #[test]
    fn the_request_says_everything_on_the_form() {
        let u = TagUi::default();
        let r = request_of(&u).unwrap();
        for part in ["new RFID tag", "(id ucode9)", "0.902 to 0.928 GHz", "(preset paper_label)", "95.0 x 25.0 mm", "200 µm"] {
            assert!(r.contains(part), "{part} isn't in: {r}");
        }
    }

    /// The page once showed 95 x 25 mm while the assistant's search ran at 50 x 30.
    #[test]
    fn the_form_shows_what_a_search_is_really_for() {
        let mut u = TagUi::default();
        let mut g = goal_of(&u).unwrap();
        g.max_x = Some(0.05);
        g.max_y = Some(0.03);
        g.min_feature = 150e-6;
        g.f_lo = 865e6;
        g.f_hi = 868e6;
        g.stack = (PRESETS[3].stack)();
        u.follow(&g);
        assert_eq!((u.max_x_mm, u.max_y_mm), (50.0, 30.0));
        assert!((u.min_um - 150.0).abs() < 1e-9);
        assert_eq!(BANDS[u.band].0, "EU, 865-868 MHz");
        assert_eq!(u.preset, 3);
        assert_eq!(goal_of(&u).unwrap(), g, "so the page and the search agree, and it doesn't need the note saying they don't");

        // A band it has no entry for stays where it was, for the note to say.
        let mut odd = g.clone();
        odd.f_lo = 840e6;
        odd.f_hi = 845e6;
        u.follow(&odd);
        assert_eq!(BANDS[u.band].0, "EU, 865-868 MHz");
        assert_ne!(goal_of(&u).unwrap(), odd);
    }
}
