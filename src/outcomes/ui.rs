//! The Predictions card: everything predicted so far, what it rests on, and the
//! recipes that add to it or check it.

use bevy::prelude::*;
use bevy_egui::egui::{self, RichText};
use proof::{Evidence, Verdict};
use serde_json::{Value, json};

use super::Predictions;
use super::hardware::{JobState, Jobs};
use crate::theme::{self, icon};

#[derive(Resource)]
pub struct OutcomesUi {
    /// Tools to run with the whole world in hand, next frame.
    pub pending: Vec<(String, Value)>,
    pub output: String,
    pub output_error: bool,
    pub measurement: String,
    pub measuring: Option<String>,
    pub contents: String,
    pub radios: Vec<(String, bool)>,
    pub isolation_db: f32,
    pub template: String,
    pub f0: String,
    pub shelf_optimize: bool,
}

impl Default for OutcomesUi {
    fn default() -> Self {
        let radios = antenna_core::recipes::coexist::Radio::presets().into_iter().enumerate().map(|(k, r)| (r.name, k == 0 || k == 4)).collect();
        Self { pending: Vec::new(), output: String::new(), output_error: false, measurement: String::new(), measuring: None, contents: "water_based".into(), radios, isolation_db: 20.0, template: "strip_dipole".into(), f0: "6.4896GHz".into(), shelf_optimize: false }
    }
}

/// Runs what the card asked for, where it can reach the whole scene.
pub fn run_pending(world: &mut World) {
    let pending = std::mem::take(&mut world.resource_mut::<OutcomesUi>().pending);
    for (name, input) in pending {
        let Some(out) = super::tools::start(world, &name, &input) else { continue };
        let mut ui = world.resource_mut::<OutcomesUi>();
        ui.output_error = out.is_error;
        ui.output = out.content.iter().filter_map(|b| if let crate::agent::llm::Block::Text(t) = b { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
    }
}

fn evidence_color(e: Evidence) -> egui::Color32 {
    match e {
        Evidence::Estimate => theme::WARNING,
        Evidence::Simulated => theme::ACCENT,
        Evidence::Calibrated | Evidence::Measured => theme::SUCCESS,
    }
}

fn verdict_color(v: Verdict) -> egui::Color32 {
    match v {
        Verdict::Likely => theme::SUCCESS,
        Verdict::Uncertain | Verdict::TooFewRuns | Verdict::WorthTesting => theme::WARNING,
        Verdict::MeetsOnce => theme::ACCENT,
        Verdict::Unlikely => theme::DANGER,
        Verdict::NoTarget => theme::WEAK,
    }
}

fn pick_file(title: &str, name: &str, ext: &[&str]) -> Option<String> {
    rfd::FileDialog::new().set_title(title).add_filter(name, ext).pick_file().map(|p| p.to_string_lossy().to_string())
}

/// The card's body.
pub fn show(ui: &mut egui::Ui, state: &mut OutcomesUi, ledger: &mut Predictions, jobs: &Jobs) {
    ui.label(RichText::new("Every number here says what it rests on. Measure one, and it gets scored.").small().color(theme::WEAK));
    ui.label(RichText::new(proof::NOT_VALIDATED).small().color(theme::WARNING));
    if !ledger.ledger.claims.is_empty() || !ledger.ledger.removed.is_empty() {
        let color = if ledger.ledger.hit_rate().is_some() { theme::TEXT } else { theme::WARNING };
        ui.label(RichText::new(ledger.ledger.score_line()).color(color));
    }
    if !ledger.ledger.removed.is_empty() {
        ui.label(RichText::new(format!("{} taken off the list, still on record and in the score.", ledger.ledger.removed.len())).small().color(theme::WEAK));
    }
    ui.add_space(4.0);

    if ledger.ledger.claims.is_empty() {
        theme::empty_state(ui, icon::TARGET, "No predictions yet", "Run a recipe below, or ask the assistant to.");
    }
    let mut record: Option<(String, f64)> = None;
    let mut remove: Option<String> = None;
    for c in ledger.ledger.claims.iter().rev() {
        egui::Frame::new().fill(theme::RAISED).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&c.subject).strong());
            });
            ui.label(format!("{}: {}", c.prediction.metric, c.range_text()));
            ui.horizontal_wrapped(|ui| {
                theme::pill(ui, c.prediction.evidence.label(), evidence_color(c.prediction.evidence));
                if c.verdict != Verdict::NoTarget {
                    theme::pill(ui, &c.verdict_text(), verdict_color(c.verdict));
                }
            });
            if !c.drivers.is_empty() {
                ui.label(RichText::new(format!("Most uncertain because of: {}", c.drivers.join(", "))).small().color(theme::WEAK));
            }
            for k in &c.checks {
                // One solve has no range to land in: just say how far off it was.
                if c.prediction.is_point() {
                    ui.label(RichText::new(format!("Measured {:.3} {}: off by {:.3}", k.measured, c.prediction.unit, k.error)).small());
                    continue;
                }
                let word = if k.within_range { "inside the range" } else { "outside the range" };
                ui.label(RichText::new(format!("Measured {:.3} {}: {word}", k.measured, c.prediction.unit)).small().color(if k.within_range { theme::SUCCESS } else { theme::DANGER }));
            }
            egui::CollapsingHeader::new(RichText::new("What this assumes").small()).id_salt(("assumes", &c.id)).show(ui, |ui| {
                for a in &c.assumptions {
                    ui.label(RichText::new(format!("• {a}")).small());
                }
            });
            ui.horizontal(|ui| {
                if state.measuring.as_deref() == Some(c.id.as_str()) {
                    ui.add(egui::TextEdit::singleline(&mut state.measurement).desired_width(70.0).hint_text(c.prediction.unit.as_str()));
                    if ui.button("Record").clicked()
                        && let Some(v) = state.measurement.trim().parse::<f64>().ok().filter(|v| v.is_finite())
                    {
                        record = Some((c.id.clone(), v));
                    }
                } else if ui.small_button("Add a measurement").clicked() {
                    state.measuring = Some(c.id.clone());
                    state.measurement.clear();
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(c.id.clone());
                }
            });
        });
        ui.add_space(2.0);
    }
    if let Some((id, v)) = record {
        if let Some(c) = ledger.ledger.get_mut(&id) {
            c.check_from(v, "Entered by hand", "Predictions card");
        }
        ledger.save();
        state.measuring = None;
    }
    if let Some(id) = remove {
        ledger.ledger.remove(&id);
        ledger.save();
    }

    theme::section(ui, "This site", true, |ui| {
        let go = |ui: &mut egui::Ui, text: &str, tip: &str| ui.add_sized([ui.available_width(), 24.0], egui::Button::new(text)).on_hover_text(tip).clicked();
        if go(ui, "Plan a survey walk", "Find the fewest spots that pin down this site's materials") {
            state.pending.push(("site_survey_plan".into(), json!({})));
        }
        if go(ui, "Fit a measured walk…", "Readings you took on site with a real tag (device,x,y,z,kind,value). What passes its checks replaces the built-in material costs for this scene and band") {
            if let Some(path) = pick_file("Survey walk", "Readings", &["csv", "jsonl", "txt"]) {
                state.pending.push(("site_survey_fit".into(), json!({ "path": path, "origin": "field", "apply": true })));
            }
        }
        if go(ui, "Fit a practice walk…", "A made-up walk from site_survey_practice, to try the loop. It can move numbers and never counts as calibration") {
            if let Some(path) = pick_file("Practice walk", "Readings", &["csv", "jsonl", "txt"]) {
                state.pending.push(("site_survey_fit".into(), json!({ "path": path, "origin": "practice", "apply": true })));
            }
        }
        if go(ui, "Stress test the system", "Full and empty racks, crowds, worse materials, a dead anchor. Takes a while") {
            state.pending.push(("site_stress".into(), json!({})));
        }
        if go(ui, "Record design positions", "The baseline a drift check compares against") {
            state.pending.push(("site_drift_baseline".into(), json!({})));
        }
        if go(ui, "Drift drill", "Runs the drift check on 30 s of simulation, to try it out. The simulator can't show a real site is healthy: give the tool a file of live ranges for that") {
            state.pending.push(("site_drift_check".into(), json!({})));
        }
        if go(ui, "Compare undecided materials", "How far coverage could fall, and which material choice matters most") {
            state.pending.push(("site_material_ranges".into(), json!({})));
        }
        if go(ui, "Emergency radio coverage", "Firefighters' radios per floor across undecided glazing and walls. For a building not built yet") {
            state.pending.push(("site_erces".into(), json!({})));
        }
        if go(ui, "Compare technologies", "Deploy each one here and cost it over five years. Placeholder prices until you give real ones. Takes a while") {
            state.pending.push(("site_compare_tech".into(), json!({})));
        }
        if go(ui, "Plan an acceptance test", "How many points a handover takes to show 50 cm, and where. Ask the assistant for another target") {
            state.pending.push(("site_acceptance_plan".into(), json!({ "target_cm": 50 })));
        }
        if go(ui, "Preview the acceptance test", "Run that plan in the simulator. A forecast, never a pass") {
            state.pending.push(("site_acceptance_preview".into(), json!({})));
        }
        if go(ui, "Score acceptance results…", "Load real results (id,x,y,z or id,error_m) against the plan in force") {
            if let Some(path) = pick_file("Acceptance results", "Results", &["csv", "jsonl", "txt"]) {
                let report = std::path::Path::new(&path).with_extension("html").to_string_lossy().to_string();
                state.pending.push(("site_acceptance_score".into(), json!({ "source": format!("Chosen by hand in the app: {path}"), "path": path, "report_path": report })));
            }
        }
        if go(ui, "Fit anchors from their ranges…", "Load ranges between anchors (a,b,range_m) and see where they really are") {
            if let Some(path) = pick_file("Ranges between anchors", "Ranges", &["csv", "jsonl", "txt"]) {
                state.pending.push(("site_anchor_fit".into(), json!({ "path": path })));
            }
        }
        if go(ui, "Tag battery against update rate", "The slowest rate that holds the accuracy target and the life target") {
            state.pending.push(("site_battery".into(), json!({})));
        }
        if go(ui, "Shelf labels", "Labels reached and how long a store-wide update takes. Needs esl-gateway devices") {
            state.pending.push(("site_esl".into(), json!({})));
        }
        if go(ui, "Robot route radio", "Gaps and handoffs along a device's path") {
            state.pending.push(("site_route_coverage".into(), json!({})));
        }
        if go(ui, "Forklift alerts at the real corners", "Does the alert fire in time to stop, and how often for someone safely to the side?") {
            state.pending.push(("site_proximity".into(), json!({})));
        }
    });

    theme::section(ui, "Hardware", true, |ui| {
        theme::caption(ui, "RFID tag on a product");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("tag_contents").selected_text(antenna_core::recipes::tagging::contents(&state.contents).map_or("", |c| c.label)).width(ui.available_width() - 130.0).show_ui(ui, |ui| {
                for c in &antenna_core::recipes::tagging::CONTENTS {
                    ui.selectable_value(&mut state.contents, c.id.to_string(), c.label);
                }
            });
            if ui.button("Rank").clicked() {
                state.pending.push(("design_tag".into(), json!({ "contents": state.contents })));
            }
            if ui.button("Tunnel").on_hover_text("These contents through an RFID tunnel: tags read, and the fastest belt").clicked() {
                state.pending.push(("design_tunnel".into(), json!({ "contents": state.contents })));
            }
        });
        theme::caption(ui, "Radios in one device");
        ui.horizontal_wrapped(|ui| {
            for (name, on) in state.radios.iter_mut() {
                ui.checkbox(on, name.as_str());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Isolation");
            ui.add(egui::DragValue::new(&mut state.isolation_db).range(0.0..=80.0).suffix(" dB"));
            if ui.button("Check").clicked() {
                let radios: Vec<&String> = state.radios.iter().filter(|r| r.1).map(|r| &r.0).collect();
                state.pending.push(("design_coexist".into(), json!({ "radios": radios, "isolation_db": state.isolation_db })));
            }
        });
        theme::caption(ui, "Antenna (metal, solved here)");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut state.template).desired_width(110.0));
            ui.add(egui::TextEdit::singleline(&mut state.f0).desired_width(80.0));
            if ui.button("UWB bias").on_hover_text("Range shift per direction from this antenna's shape").clicked() {
                state.pending.push(("design_uwb_bias".into(), json!({ "template": state.template, "f0": state.f0 })));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Shelf antenna").on_hover_text("An RFID patch on a metal shelf: read rate and stray reads").clicked() {
                state.pending.push(("design_shelf".into(), json!({ "optimize": state.shelf_optimize })));
            }
            ui.checkbox(&mut state.shelf_optimize, "search for the best");
            if ui.button("AoA array").on_hover_text("Angle error of a 4×4 array, and a calibration table").clicked() {
                state.pending.push(("design_aoa".into(), json!({})));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Medical device").on_hover_text("Which ward radios to test a wireless medical device against first").clicked() {
                state.pending.push(("design_medical_coexist".into(), json!({})));
            }
            if ui.button("SAR pre-check").on_hover_text("Real FDTD with tissue beside the antenna above. A design check, not a compliance result. Minutes to tens of minutes").clicked() {
                state.pending.push(("design_sar".into(), json!({ "template": state.template, "f0": state.f0 })));
            }
        });
    });

    if !jobs.list.is_empty() {
        theme::section(ui, "Jobs", true, |ui| {
            for j in jobs.list.iter().rev().take(6) {
                ui.horizontal(|ui| {
                    match &j.state {
                        JobState::Running => {
                            ui.add(egui::Spinner::new().size(12.0).color(theme::ACCENT));
                        }
                        JobState::Done(_) => {
                            ui.label(RichText::new(icon::CHECK).color(theme::SUCCESS));
                        }
                        JobState::Failed(_) => {
                            ui.label(RichText::new(icon::WARNING).color(theme::DANGER));
                        }
                    }
                    ui.label(&j.title);
                    let secs = j.seconds.unwrap_or_else(|| j.started.elapsed().as_secs_f64());
                    ui.label(RichText::new(format!("{secs:.0} s")).small().color(theme::WEAK));
                    match &j.state {
                        JobState::Done(v) => {
                            if ui.small_button("Show").clicked() {
                                state.output = serde_json::to_string_pretty(v).unwrap_or_default();
                                state.output_error = false;
                            }
                        }
                        JobState::Failed(e) => {
                            if ui.small_button("Why").clicked() {
                                state.output = e.clone();
                                state.output_error = true;
                            }
                        }
                        JobState::Running => {}
                    }
                });
            }
        });
    }

    if !state.output.is_empty() {
        theme::section(ui, "Latest result", true, |ui| {
            let color = if state.output_error { theme::DANGER } else { theme::TEXT };
            egui::ScrollArea::vertical().id_salt("outcomes_output").max_height(260.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&state.output).small().monospace().color(color)).wrap());
            });
        });
    }
}
