//! Tuning: a slider for each variable ticked as optimizable, and the antenna solved again each time
//! one is let go, so what a size does can be seen by moving it.

use antenna_core::report::param_text;
use bevy_egui::egui::{self, RichText, vec2};

use super::{results, solve, weak};
use crate::antenna::Studio;
use crate::theme;

pub fn page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Tuning");
    weak(ui, "Move a variable and let go: the antenna is solved again and the plot below follows. It uses the variables ticked Optimizable in the Variables table, between their Min. and Max. A variable that was a formula becomes the number you set.");
    solve::sync_opt_vars(st);
    let vars = st.project.vars().unwrap_or_default();
    let chosen: Vec<(String, f64, f64)> = st.ui.opt_vars.iter().filter(|v| v.3).map(|v| (v.0.clone(), v.1.min(v.2), v.1.max(v.2))).collect();
    if chosen.is_empty() {
        super::verdict(ui, false, "Nothing is ticked yet. Tick Optimizable beside a variable in the table below.");
    }
    let mut moved: Vec<(String, f64)> = Vec::new();
    let mut released = false;
    // One drag is one undo step, not one for every frame of it.
    let mut began = false;
    egui::Grid::new("tuning_rows").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        for (name, lo, hi) in &chosen {
            let Some(mut v) = vars.get(name).copied() else { continue };
            ui.label(RichText::new(name).monospace());
            let n = name.clone();
            let r = ui.add(egui::Slider::new(&mut v, *lo..=*hi).custom_formatter(move |x, _| param_text(&n, x)));
            if r.changed() {
                moved.push((name.clone(), v));
            }
            began |= r.drag_started() || (r.changed() && !r.dragged());
            released |= r.drag_stopped() || (r.changed() && !r.dragged());
            ui.end_row();
        }
    });
    if began {
        super::draw::remember(st);
    }
    if !moved.is_empty() {
        st.project = st.project.with_values(&moved);
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut st.ui.tune_auto, "Solve when I let go");
        if ui.button("Solve now").clicked() {
            st.ui.tune_dirty = true;
        }
    });
    if released && st.ui.tune_auto {
        st.ui.tune_dirty = true;
    }
    // One at a time: a slider let go during a run waits for it.
    if st.ui.tune_dirty && !st.busy() && st.ui.geometry.is_ok() {
        st.ui.tune_dirty = false;
        st.run();
    }
    if st.ui.tune_dirty || st.busy() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    if st.latest().is_some() {
        results::sparams(ui, st);
    } else {
        weak(ui, "The plot appears after the first solve.");
    }
}
