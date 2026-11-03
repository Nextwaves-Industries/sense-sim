//! Tuning the open tag by the two things that decide it: the loop, whose length and width match the
//! chip, and the long line outside it, which puts the tag on its frequency. The assistant works the
//! same tune through `antenna_tag_tune`.

use antenna_core::tag_design::TagGoal;
use antenna_core::tag_tune::{self, Knobs, Reading, Tune};
use bevy_egui::egui::{self, RichText};

use super::{group, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

/// What this part of the page is set to.
pub struct TuneUi {
    mhz: f64,
    /// The band the frequency was last set from, so changing band moves it and nothing else does.
    band_seen: (f64, f64),
    /// For a drawing, which has whatever variables its designer named.
    loop_a: String,
    loop_b: String,
    arm: String,
}

impl Default for TuneUi {
    fn default() -> Self {
        Self { mhz: 902.0, band_seen: (902e6, 928e6), loop_a: String::new(), loop_b: String::new(), arm: String::new() }
    }
}

fn picked_knobs(u: &TuneUi) -> Option<Knobs> {
    if u.loop_a.is_empty() || u.arm.is_empty() {
        return None;
    }
    let mut loop_vars = vec![u.loop_a.clone()];
    if !u.loop_b.is_empty() && u.loop_b != u.loop_a {
        loop_vars.push(u.loop_b.clone());
    }
    Some(Knobs { loop_vars, arm_var: u.arm.clone() })
}

fn pick(ui: &mut egui::Ui, salt: &str, value: &mut String, names: &[String], optional: bool) {
    let shown = if value.is_empty() { "Choose" } else { value.as_str() };
    egui::ComboBox::from_id_salt(salt).selected_text(shown.to_string()).width(90.0).show_ui(ui, |ui| {
        if optional {
            ui.selectable_value(value, String::new(), "None");
        }
        for n in names {
            ui.selectable_value(value, n.clone(), n);
        }
    });
}

fn ohms(z: (f64, f64)) -> String {
    format!("{:.1} {} j{:.1} Ω", z.0, if z.1 < 0.0 { "-" } else { "+" }, z.1.abs())
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).color(theme::TEXT));
}

/// The two metrics, in the order a designer sets them.
fn show_reading(ui: &mut egui::Ui, r: &Reading) {
    heading(ui, "1. Loop: matches the chip");
    let dims: Vec<String> = r.loop_dims.iter().map(|(n, v)| format!("{n} {:.2} mm", v * 1e3)).collect();
    ui.label(dims.join(", "));
    ui.label(format!("Antenna {}, and the chip needs {}", ohms(r.z), ohms(r.needed)));
    let mhz = r.f0 / 1e6;
    verdict(ui, r.matched(), &format!("{:.0} % of the collected power reaches the chip at {mhz:.1} MHz", r.transfer * 100.0));
    ui.add_space(4.0);
    heading(ui, "2. Outer line: sets the frequency");
    match r.arm_wire {
        Some(w) => ui.label(format!("One arm is {:.1} mm along its metal, {:.2} × λ/4 ({:.1} mm). {} is {:.2} mm", w * 1e3, w / r.quarter_wave, r.quarter_wave * 1e3, r.arm_var.0, r.arm_var.1 * 1e3)),
        None => ui.label(format!("{} is {:.2} mm. λ/4 is {:.1} mm", r.arm_var.0, r.arm_var.1 * 1e3, r.quarter_wave * 1e3)),
    };
    let peak = match r.peak_hz {
        Some(f) => format!("Peaks at {:.1} MHz against a target of {mhz:.1} MHz, at {:.0} % of the power", f / 1e6, r.peak_transfer * 100.0),
        None => format!("Peaks more than a fifth away from {mhz:.1} MHz"),
    };
    verdict(ui, r.on_frequency(), &peak);
    ui.add_space(2.0);
    weak(ui, format!("{:.1} x {:.1} mm, {:.1} dBi, {:.0} % radiated. Read range limit {:.1} m at {mhz:.1} MHz: forward link, aligned, on this model alone.", r.size_x * 1e3, r.size_y * 1e3, r.gain_dbi, r.efficiency * 100.0, r.range_m));
}

fn show_tune(ui: &mut egui::Ui, st: &mut Studio, t: &Tune, busy: bool) {
    show_reading(ui, &t.after);
    let before = &t.before;
    weak(ui, format!("Before: {} at {:.1} MHz, {:.0} % of the power to the chip.", ohms(before.z), t.f0 / 1e6, before.transfer * 100.0));
    for n in &t.notes {
        weak(ui, n.clone());
    }
    if !t.changes.is_empty() {
        ui.add_space(4.0);
        heading(ui, "What moved");
        for (name, from, to) in &t.changes {
            if tag_tune::is_count(name) {
                weak(ui, format!("{name}: {from:.0} to {to:.0}"));
            } else {
                weak(ui, format!("{name}: {:.2} to {:.2} mm", from * 1e3, to * 1e3));
            }
        }
    }
    if !t.worth.is_empty() {
        ui.add_space(4.0);
        heading(ui, "If you adjust it by hand, each extra millimetre gives");
        egui::Grid::new("tag_tune_worth").num_columns(4).spacing([14.0, 3.0]).show(ui, |ui| {
            for head in ["", "Resistance", "Reactance", "Peak"] {
                ui.label(RichText::new(head).size(12.0).color(theme::WEAK));
            }
            ui.end_row();
            for w in &t.worth {
                ui.label(if w.is_line { format!("{} (line)", w.var) } else { format!("{} (loop)", w.var) });
                ui.label(format!("{:+.1} Ω", w.r_per_mm));
                ui.label(format!("{:+.1} Ω", w.x_per_mm));
                ui.label(w.mhz_per_mm.map_or_else(|| "n/a".to_string(), |m| format!("{m:+.1} MHz")));
                ui.end_row();
            }
        });
        weak(ui, "These move together: changing one means checking the other. Simulated, not measured.");
    }
    if ui.add_enabled(!busy, egui::Button::new("Use the tuned design")).on_hover_text("Make this the current design, to run, sweep or draw on").clicked() {
        st.open_tag_option(t.project.clone());
        st.ui.message = Some(("Tuned design opened, in its surroundings. Simulated, not measured.".to_string(), true));
    }
}

/// The controls and what the last tune found. `goal` is what the rest of the page is set to.
pub fn section(ui: &mut egui::Ui, st: &mut Studio, goal: Option<&TagGoal>) {
    let busy = st.busy();
    let own = tag_tune::knobs_for(&st.project);
    let names: Vec<String> = st.project.params.iter().filter(|r| !r.derived && r.name != "f0" && r.name != "Z0").map(|r| r.name.clone()).collect();
    if let Some(g) = goal
        && st.ui.tag.tune.band_seen != (g.f_lo, g.f_hi)
    {
        st.ui.tag.tune.band_seen = (g.f_lo, g.f_hi);
        st.ui.tag.tune.mhz = (g.f_lo + g.f_hi) / 2e6;
    }
    ui.add_space(6.0);
    theme::caption(ui, "Tune the open design");
    group(ui, |ui| {
        weak(ui, "Two things decide a tag. The loop's length and width match the antenna to the chip's impedance, and the long line outside it sets the frequency.");
        let u = &mut st.ui.tag.tune;
        theme::field_row(ui, "Frequency", |ui| {
            ui.add(egui::DragValue::new(&mut u.mhz).suffix(" MHz").speed(0.5).range(100.0..=6000.0));
            weak(ui, format!("λ/4 is {:.1} mm", antenna_core::C0 / (u.mhz * 1e6) / 4.0 * 1e3));
        });
        if own.is_none() {
            weak(ui, "This design has no loop and line that the app recognises. Say which variables they are, or start from a tag below.");
            theme::field_row(ui, "Loop", |ui| {
                pick(ui, "tag_tune_loop_a", &mut u.loop_a, &names, false);
                pick(ui, "tag_tune_loop_b", &mut u.loop_b, &names, true);
            });
            theme::field_row(ui, "Line", |ui| {
                pick(ui, "tag_tune_arm", &mut u.arm, &names, false);
            });
        }
        let knobs = own.clone().or_else(|| picked_knobs(u));
        let target = u.mhz * 1e6;
        let can = goal.is_some() && knobs.is_some() && !busy;
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(can, egui::Button::new(theme::label(icon::SPARKLE, "Tune loop and line"))).on_hover_text("Moves them until the antenna is the chip's conjugate at this frequency, in the surroundings and footprint set above").clicked()
                && let (Some(g), Some(k)) = (goal, knobs.clone())
            {
                st.run_tag_tune(g.clone(), target, k, false);
            }
            if ui.add_enabled(can, egui::Button::new("Only read the two numbers")).on_hover_text("Reads the design as it stands, and changes nothing").clicked()
                && let (Some(g), Some(k)) = (goal, knobs.clone())
            {
                st.run_tag_tune(g.clone(), target, k, true);
            }
            if own.is_none() {
                if ui.add_enabled(!busy, egui::Button::new("Start from a T-match meander")).clicked() {
                    st.start_tag_from("rfid_tag", target);
                }
                if ui.add_enabled(!busy, egui::Button::new("Start from a coupled loop")).clicked() {
                    st.start_tag_from("tag_loop", target);
                }
            }
        });
        let Some(run) = st.tag_run() else { return };
        if let Some(t) = &run.tune {
            ui.add_space(4.0);
            show_tune(ui, st, t, busy);
        } else if let Some(r) = &run.reading {
            ui.add_space(4.0);
            show_reading(ui, r);
        }
    });
}
