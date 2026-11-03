//! Solver setup and the automation pages: sweep, optimiser, mesh convergence,
//! tolerance analysis.

use antenna_core::linspace;
use antenna_core::optimize::Method;
use antenna_core::project::{Goal, SolverKind};
use bevy_egui::egui::{self, RichText, vec2};

use super::charts::{self, ChartOptions, Series};
use super::{duration, ghz_field, group, run_button, tiles, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

fn solver_options(fit: antenna_core::geometry::SolverFit) -> Vec<SolverKind> {
    let mut v = vec![SolverKind::Auto];
    if fit.fdtd {
        v.push(SolverKind::Fdtd);
    }
    if fit.wire_mom {
        v.push(SolverKind::MomWire);
    }
    if fit.surface_mom {
        v.push(SolverKind::MomSurface);
    }
    v
}

/// A UHF RFID band the sweep can be set to in one click.
#[derive(Clone, Debug, PartialEq)]
pub struct RfidBand {
    pub label: String,
    /// Band edges, Hz.
    pub band: (f64, f64),
    /// What applies there, for the line under the menu.
    pub note: String,
}

/// The whole Gen2 range, then the active region, then every region whose band is confirmed.
pub fn rfid_bands(state: &crate::rtls::region::RegionState) -> Vec<RfidBand> {
    let hz = |mhz: f32| (mhz as f64 * 1e3).round() * 1e3;
    let mut bands = vec![RfidBand { label: "Worldwide · 860–960 MHz".into(), band: (860e6, 960e6), note: "The whole range Gen2 tags are built for. No single country allows all of it.".into() }];
    let active = state.active.as_ref();
    let others = state.regions.iter().filter(|r| active.is_none_or(|a| a.id != r.id));
    for r in active.into_iter().chain(others) {
        let Some([lo, hi]) = r.rfid.band_mhz else { continue };
        let mark = if active.is_some_and(|a| a.id == r.id) { " (active region)" } else { "" };
        bands.push(RfidBand { label: format!("{} · {lo}–{hi} MHz{mark}", r.name), band: (hz(lo), hz(hi)), note: format!("{}: {}, {}. {}", r.name, r.rfid.limit_text, r.rfid.status.label(), r.rfid.note) });
    }
    bands
}

/// Every region's UHF RFID rule as the report prints it, the active region first. A rule
/// nobody could confirm has no limit to apply, the same as everywhere else.
pub fn rfid_limits(state: &crate::rtls::region::RegionState) -> Vec<antenna_core::report::RegionLimit> {
    use crate::rtls::region::Status;
    let hz = |mhz: f32| (mhz as f64 * 1e3).round() * 1e3;
    // These are stored as f32, and 35.15 shouldn't print as 35.150001.
    let db = |v: f32| (v as f64 * 100.0).round() / 100.0;
    let active = state.active.as_ref();
    let others = state.regions.iter().filter(|r| active.is_none_or(|a| a.id != r.id));
    active
        .into_iter()
        .chain(others)
        .filter_map(|r| {
            let [lo, hi] = r.rfid.band_mhz?;
            let checked = if r.checked.is_empty() { String::new() } else { format!(", checked {}", r.checked) };
            Some(antenna_core::report::RegionLimit {
                name: r.name.clone(),
                band: (hz(lo), hz(hi)),
                limit_text: r.rfid.limit_text.clone(),
                eirp_dbm: r.rfid.max_eirp_dbm.filter(|_| r.rfid.status != Status::Unverified).map(db),
                conducted_dbm: r.conducted_limit_dbm().map(db),
                status: format!("{}{checked}", r.rfid.status.label()),
                source: r.rfid.source.clone(),
                active: active.is_some_and(|a| a.id == r.id),
            })
        })
        .collect()
}

fn rfid_band_menu(ui: &mut egui::Ui, st: &mut Studio) {
    let (f_lo, f_hi) = (st.project.setup.f_lo, st.project.setup.f_hi);
    let current = st.ui.rfid_bands.iter().find(|b| (b.band.0 - f_lo).abs() < 1e3 && (b.band.1 - f_hi).abs() < 1e3).cloned();
    let mut pick = None;
    egui::ComboBox::from_id_salt("rfid_band").selected_text(current.as_ref().map_or("Other", |b| b.label.as_str())).width(300.0).show_ui(ui, |ui| {
        for b in &st.ui.rfid_bands {
            if ui.selectable_label(current.as_ref() == Some(b), &b.label).clicked() {
                pick = Some(b.band);
            }
        }
    })
    .response
    .on_hover_text("Sets the sweep to the band, the far field to its centre, and sizes the antenna for that centre through f0.");
    if let Some((lo, hi)) = pick {
        st.project.fit_band(lo, hi);
        st.ui.message = Some((format!("Sweep set to {} to {} MHz, with the antenna sized for {} MHz. Run it for new results.", lo / 1e6, hi / 1e6, (lo + hi) / 2e6), true));
    }
}

pub fn setup(ui: &mut egui::Ui, st: &mut Studio) {
    let busy = st.busy();
    let Ok(geom) = st.ui.geometry.clone() else {
        if let Err(e) = &st.ui.geometry {
            verdict(ui, false, e);
        }
        return;
    };
    let fit = geom.solver_fit();
    let chosen = st.ui.chosen;
    theme::caption(ui, "Solver");
    egui::Grid::new("studio_setup").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Solver");
        egui::ComboBox::from_id_salt("solver").selected_text(st.project.setup.solver.label()).width(240.0).show_ui(ui, |ui| {
            for s in solver_options(fit) {
                ui.selectable_value(&mut st.project.setup.solver, s, s.label());
            }
        });
        ui.end_row();
        ui.label("RFID band");
        rfid_band_menu(ui, st);
        ui.end_row();
        ui.label("Sweep from");
        ghz_field(ui, &mut st.project.setup.f_lo);
        ui.end_row();
        ui.label("Sweep to");
        ghz_field(ui, &mut st.project.setup.f_hi);
        ui.end_row();
        ui.label("Output points");
        ui.add(egui::DragValue::new(&mut st.project.setup.points).range(11..=4001));
        ui.end_row();
    });
    if let Some(problem) = &st.ui.setup_problem {
        verdict(ui, false, problem);
    }
    let (f_lo, f_hi) = (st.project.setup.f_lo, st.project.setup.f_hi);
    if let Some(b) = st.ui.rfid_bands.iter().find(|b| (b.band.0 - f_lo).abs() < 1e3 && (b.band.1 - f_hi).abs() < 1e3) {
        weak(ui, b.note.clone());
    }
    weak(
        ui,
        match chosen {
            SolverKind::Fdtd => "FDTD handles anything, dielectrics included. One run gives the whole band at once; a high-Q antenna takes longer to ring down.",
            SolverKind::MomWire => "Method of moments on wire segments: exact free-space physics for metal wires, solved in seconds, one matrix per frequency.",
            SolverKind::MomSurface => "Method of moments on triangles (RWG): metal in free space, no mesh of the air around it. Cost grows with the third power of the unknowns.",
            SolverKind::Auto => "",
        },
    );
    if !fit.wire_mom && !fit.surface_mom {
        weak(ui, "This model has dielectrics or mixes wires with sheets, so only FDTD applies.");
    }

    theme::caption(ui, "Far field");
    let mut far = st.project.setup.far_freqs.clone();
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for (i, f) in far.iter_mut().enumerate() {
            ghz_field(ui, f);
            if theme::icon_button(ui, icon::X, "Remove").clicked() {
                remove = Some(i);
            }
        }
        if ui.small_button(theme::label(icon::PLUS, "Frequency")).clicked() {
            remove = Some(usize::MAX);
        }
    });
    match remove {
        Some(usize::MAX) => far.push(st.project.f0()),
        Some(i) => {
            far.remove(i);
        }
        None => {}
    }
    st.project.setup.far_freqs = far;
    ui.horizontal(|ui| {
        ui.label("Grid step");
        ui.add(egui::DragValue::new(&mut st.project.setup.far_step).range(1.0..=15.0).suffix("°").speed(0.5));
    });

    theme::caption(ui, "Mesh");
    match chosen {
        SolverKind::Fdtd => {
            ui.horizontal(|ui| {
                ui.label("Cells per λ");
                let mut v = st.project.setup.cells_per_lambda;
                theme::segmented(ui, &mut v, &[(12.0, "Fast 12"), (20.0, "Normal 20"), (30.0, "Fine 30"), (45.0, "Finest 45")]);
                st.project.setup.cells_per_lambda = v;
            });
            egui::Grid::new("studio_end").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                ui.label("Stop at energy").on_hover_text("The run ends when the energy in the model has decayed this far");
                ui.add(egui::DragValue::new(&mut st.project.setup.end_db).range(20.0..=80.0).prefix("−").suffix(" dB"));
                ui.end_row();
                ui.label("Step limit");
                ui.add(egui::DragValue::new(&mut st.project.setup.max_steps).range(1000..=100_000_000).speed(10000));
                ui.end_row();
                ui.label("Threads");
                ui.add(egui::DragValue::new(&mut st.project.setup.threads).range(0..=256).custom_formatter(|v, _| if v == 0.0 { "auto".into() } else { format!("{v:.0}") }));
                ui.end_row();
            });
            ui.checkbox(&mut st.project.setup.field_planes, "Save field planes for viewing");
            if let Some(plan) = &st.ui.plan {
                let n_ports = geom.ports.len();
                let speed = st.speed_mcs();
                let est = plan.estimate_s(speed) * n_ports as f64;
                tiles(
                    ui,
                    &[
                        (format!("{:.2} M", plan.cells() as f64 / 1e6), "cells", theme::TEXT),
                        (format!("{:.1} GB", plan.memory_bytes() as f64 / 1e9), "memory", theme::TEXT),
                        (format!("~{}", duration(est)), &format!("at {speed:.0} MC/s"), theme::TEXT),
                    ],
                );
                for w in &plan.warnings {
                    weak(ui, w.clone());
                }
            }
            ui.horizontal(|ui| {
                match crate::rtls::fdtd::exe() {
                    Some(_) => {
                        theme::badge(ui, &format!("openEMS {}", crate::rtls::fdtd::OPENEMS_VERSION), theme::SUCCESS);
                    }
                    None => {
                        theme::badge(ui, "openEMS not installed", theme::WARNING);
                        let idle = st.ui.install_status.is_none();
                        if ui.add_enabled(idle, egui::Button::new("Install (about 65 MB)")).on_hover_text("Downloads the official Windows build into %LOCALAPPDATA%\\SenseSim\\openems-fdtd").clicked() {
                            st.ui.want_install = true;
                        }
                    }
                };
            });
            if let Some(s) = st.ui.install_status.clone() {
                weak(ui, s);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
        }
        SolverKind::MomWire => {
            ui.horizontal(|ui| {
                ui.label("Segments per λ");
                ui.add(egui::DragValue::new(&mut st.project.setup.segments_per_lambda).range(6.0..=100.0));
                ui.label("Solved frequencies");
                ui.add(egui::DragValue::new(&mut st.project.setup.mom_samples).range(3..=401));
            });
        }
        _ => {
            ui.horizontal(|ui| {
                ui.label("Edges per λ");
                ui.add(egui::DragValue::new(&mut st.project.setup.edges_per_lambda).range(4.0..=40.0));
                ui.label("Solved frequencies");
                ui.add(egui::DragValue::new(&mut st.project.setup.mom_samples).range(3..=401));
            });
            weak(ui, "Doubling the edge density means about four times the unknowns and sixty times the solve time.");
        }
    }
    if let Some(s) = st.project.reconfig.active_state().filter(|_| st.project.reconfig.is_on()) {
        weak(ui, format!("This antenna has switches or varactors. A run solves its ports and shows state '{}'; the Reconfigurable page shows every state from one solve.", s.name));
    }
    let word = if geom.ports.len() > 1 { format!("Run {} ports", geom.ports.len()) } else { "Run the simulation".to_string() };
    if run_button(ui, &theme::label(icon::PLAY, &word), !busy && st.ui.setup_problem.is_none()) {
        st.ui.message = None;
        st.run();
    }
    let log = st.log_tail(8);
    if !log.is_empty() {
        egui::CollapsingHeader::new(RichText::new("Solver log").size(12.0).color(theme::WEAK)).show(ui, |ui| {
            for l in log {
                ui.label(RichText::new(l).size(11.5).monospace().color(theme::WEAK));
            }
        });
    }
    if let Some(r) = st.latest() {
        theme::caption(ui, "Last run");
        ui.label(RichText::new(format!("{} · {} · {}", r.solver, if r.solver.contains("FDTD") { format!("{:.2} M cells", r.size as f64 / 1e6) } else { format!("{} unknowns", r.size) }, duration(r.seconds))).size(12.5).color(theme::WEAK));
        for w in &r.warnings {
            verdict(ui, false, w);
        }
        ui.horizontal(|ui| {
            if ui.button("S-parameters").clicked() {
                st.ui.page = super::Page::SParams;
            }
            if ui.button("Far field").clicked() {
                st.ui.page = super::Page::FarField;
            }
        });
        let hist = st.history();
        if hist.len() > 1 {
            egui::CollapsingHeader::new(RichText::new(format!("Earlier runs ({})", hist.len())).color(theme::WEAK)).show(ui, |ui| {
                for (p, res) in hist.iter().rev() {
                    let text = format!("{} · {} · {}", p.name, res.solver.split(' ').next().unwrap_or(""), duration(res.seconds));
                    if ui.selectable_label(std::sync::Arc::ptr_eq(res, &r), text).clicked() {
                        st.show_result(res.clone());
                    }
                }
            });
        }
    }
}

/// Variables worth sweeping: not derived, not the design frequency's wavelength.
fn free_vars(st: &Studio) -> Vec<String> {
    st.project.params.iter().filter(|p| !p.derived && p.name != "lambda0" && p.name != "Z0").map(|p| p.name.clone()).collect()
}

fn var_combo(ui: &mut egui::Ui, id: &str, names: &[String], v: &mut String) -> bool {
    let before = v.clone();
    egui::ComboBox::from_id_salt(id).selected_text(v.clone()).show_ui(ui, |ui| {
        for n in names {
            ui.selectable_value(v, n.clone(), n);
        }
    });
    *v != before
}

pub fn sweep(ui: &mut egui::Ui, st: &mut Studio) {
    let busy = st.busy();
    theme::caption(ui, "Parameter sweep");
    weak(ui, "Runs the solver once per value and overlays the curves: the fastest way to see what a dimension does.");
    let names = free_vars(st);
    if st.ui.sweep_var.is_empty()
        && let Some(n) = names.iter().find(|n| *n != "f0").or(names.first())
    {
        st.ui.sweep_var = n.clone();
        st.ui.sweep_range = (0.0, 0.0);
    }
    let vars = st.project.vars().unwrap_or_default();
    let cur = vars.get(&st.ui.sweep_var).copied().unwrap_or(0.0);
    if st.ui.sweep_range == (0.0, 0.0) {
        st.ui.sweep_range = (cur * 0.9, cur * 1.1);
    }
    egui::Grid::new("studio_sweep").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Variable");
        if var_combo(ui, "sw_var", &names, &mut st.ui.sweep_var) {
            let c = vars.get(&st.ui.sweep_var).copied().unwrap_or(0.0);
            st.ui.sweep_range = (c * 0.9, c * 1.1);
        }
        ui.end_row();
        ui.label("From, to");
        ui.horizontal(|ui| {
            ui.add(var_drag(&st.ui.sweep_var, &mut st.ui.sweep_range.0, cur.abs() * 0.002));
            ui.add(var_drag(&st.ui.sweep_var, &mut st.ui.sweep_range.1, cur.abs() * 0.002));
        });
        ui.end_row();
        ui.label("Values");
        ui.add(egui::DragValue::new(&mut st.ui.sweep_n).range(2..=101));
        ui.end_row();
        ui.label("At once");
        ui.add(egui::DragValue::new(&mut st.ui.parallel).range(1..=32)).on_hover_text("Solver runs side by side, sharing this PC's cores");
        ui.end_row();
    });
    ui.checkbox(&mut st.ui.sweep_two, "Second variable (a grid of runs)");
    if st.ui.sweep_two {
        if st.ui.sweep_var2.is_empty()
            && let Some(n) = names.iter().find(|n| **n != st.ui.sweep_var && **n != "f0")
        {
            st.ui.sweep_var2 = n.clone();
            let c = vars.get(n).copied().unwrap_or(0.0);
            st.ui.sweep_range2 = (c * 0.9, c * 1.1);
        }
        let c2 = vars.get(&st.ui.sweep_var2).copied().unwrap_or(0.0);
        egui::Grid::new("studio_sweep2").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Variable");
            if var_combo(ui, "sw_var2", &names, &mut st.ui.sweep_var2) {
                let c = vars.get(&st.ui.sweep_var2).copied().unwrap_or(0.0);
                st.ui.sweep_range2 = (c * 0.9, c * 1.1);
            }
            ui.end_row();
            ui.label("From, to");
            ui.horizontal(|ui| {
                ui.add(var_drag(&st.ui.sweep_var2, &mut st.ui.sweep_range2.0, c2.abs() * 0.002));
                ui.add(var_drag(&st.ui.sweep_var2, &mut st.ui.sweep_range2.1, c2.abs() * 0.002));
            });
            ui.end_row();
            ui.label("Values");
            ui.add(egui::DragValue::new(&mut st.ui.sweep_n2).range(2..=21));
            ui.end_row();
        });
    }
    let total = st.ui.sweep_n * if st.ui.sweep_two { st.ui.sweep_n2 } else { 1 };
    if run_button(ui, &theme::label(icon::PLAY, &format!("Run {total} simulations")), !busy) {
        let a = (st.ui.sweep_var.clone(), linspace(st.ui.sweep_range.0, st.ui.sweep_range.1, st.ui.sweep_n));
        let b = st.ui.sweep_two.then(|| (st.ui.sweep_var2.clone(), linspace(st.ui.sweep_range2.0, st.ui.sweep_range2.1, st.ui.sweep_n2)));
        let parallel = st.ui.parallel;
        st.run_sweep(a, b, parallel);
    }
    let Some(sw) = st.sweep() else { return };
    theme::caption(ui, &format!("Results: {} of {}", sw.points.len(), sw.total));
    let curves: Vec<(String, Vec<(f64, f64)>)> = sw
        .points
        .iter()
        .map(|p| {
            let name = p.values.iter().map(|(n, v)| format!("{n} = {}", antenna_core::report::param_text(n, *v))).collect::<Vec<_>>().join(", ");
            let pts: Vec<(f64, f64)> = p.result.network.f.iter().map(|f| f / 1e9).zip(p.result.network.sij_db(0, 0)).collect();
            (name, pts)
        })
        .collect();
    let series: Vec<Series> = curves.iter().enumerate().map(|(i, c)| Series { name: &c.0, color: charts::PALETTE[i % charts::PALETTE.len()], points: &c.1 }).collect();
    charts::line_chart(ui, ChartOptions { title: "|S11|", unit: " dB", x_unit: "GHz", height: 230.0, limit: Some(-10.0), ..Default::default() }, &series);
    // The figure each value gives: resonance and match, gain if available.
    let trend: Vec<(f64, f64, f64, f64)> = sw
        .points
        .iter()
        .map(|p| {
            let db = p.result.network.sij_db(0, 0);
            let best = p.result.network.f.iter().zip(&db).min_by(|a, b| a.1.total_cmp(b.1));
            let gain = p.result.metrics(0, 0).map_or(f64::NAN, |m| m.peak_realized_dbi);
            (p.values[0].1, best.map_or(f64::NAN, |b| *b.0 / 1e9), best.map_or(f64::NAN, |b| *b.1), gain)
        })
        .collect();
    if sw.var2.is_none() && trend.len() >= 2 {
        let fr: Vec<(f64, f64)> = trend.iter().map(|t| (t.0 * scale_for(t.0), t.1)).collect();
        let gain: Vec<(f64, f64)> = trend.iter().map(|t| (t.0 * scale_for(t.0), t.3)).collect();
        charts::line_chart(ui, ChartOptions { title: &format!("Resonance against {}", sw.var), unit: " GHz", x_unit: unit_for(trend[0].0), height: 130.0, ..Default::default() }, &[Series { name: "Resonance", color: charts::BLUE, points: &fr }]);
        if gain.iter().any(|g| g.1.is_finite()) {
            charts::line_chart(ui, ChartOptions { title: &format!("Realized gain against {}", sw.var), unit: " dBi", x_unit: unit_for(trend[0].0), height: 130.0, ..Default::default() }, &[Series { name: "Peak", color: charts::ORANGE, points: &gain }]);
        }
    }
    egui::Grid::new("studio_sw_table").num_columns(4).striped(true).spacing(vec2(14.0, 2.0)).show(ui, |ui| {
        for t in ["Value", "Resonance", "Best match", "Gain"] {
            ui.label(RichText::new(t).size(12.0).color(theme::WEAK));
        }
        ui.end_row();
        for (p, t) in sw.points.iter().zip(&trend) {
            let name = p.values.iter().map(|(n, v)| antenna_core::report::param_text(n, *v)).collect::<Vec<_>>().join(", ");
            ui.label(name);
            ui.label(format!("{:.3} GHz", t.1));
            ui.label(format!("{:.1} dB", t.2));
            ui.label(if t.3.is_finite() { format!("{:.2} dBi", t.3) } else { "–".into() });
            ui.end_row();
        }
    });
}

fn scale_for(v: f64) -> f64 {
    if v.abs() < 1.0 { 1000.0 } else { 1.0 }
}
fn unit_for(v: f64) -> &'static str {
    if v.abs() < 1.0 { "m×10⁻³" } else { "" }
}

/// Goals a designer starts from for the current band.
fn default_goals(st: &Studio) -> Vec<(Goal, f64)> {
    let f0 = st.project.f0();
    let bw = (st.project.setup.f_hi - st.project.setup.f_lo) * 0.08;
    let mut v = vec![(Goal::MatchBelow { port: 0, f_lo: f0 - bw / 2.0, f_hi: f0 + bw / 2.0, db: -12.0 }, 1.0)];
    if st.project.setup.far_freqs.iter().any(|f| (*f - f0).abs() < 1.0) {
        v.push((Goal::EfficiencyAbove { f: f0, eff: 0.5 }, 0.2));
    }
    v
}

fn goal_editor(ui: &mut egui::Ui, g: &mut Goal, id: usize) {
    let f_field = |ui: &mut egui::Ui, f: &mut f64| {
        ghz_field(ui, f);
    };
    ui.push_id(id, |ui| match g {
        Goal::MatchBelow { port, f_lo, f_hi, db } | Goal::IsolationBelow { i: port, f_lo, f_hi, db, .. } => {
            ui.label("Port");
            let mut p = *port + 1;
            ui.add(egui::DragValue::new(&mut p).range(1..=16));
            *port = p - 1;
            f_field(ui, f_lo);
            f_field(ui, f_hi);
            ui.add(egui::DragValue::new(db).suffix(" dB").range(-60.0..=0.0));
        }
        Goal::ResonanceAt { f, tol, .. } => {
            f_field(ui, f);
            let mut t = *tol / 1e6;
            if ui.add(egui::DragValue::new(&mut t).suffix(" MHz").range(0.01..=1000.0)).changed() {
                *tol = t * 1e6;
            }
        }
        Goal::ChipPowerAbove { f_lo, f_hi, at_least, .. } => {
            f_field(ui, f_lo);
            f_field(ui, f_hi);
            ui.add(egui::DragValue::new(at_least).range(0.05..=1.0).speed(0.01));
        }
        Goal::GainAbove { f, dbi } => {
            f_field(ui, f);
            ui.add(egui::DragValue::new(dbi).suffix(" dBi").range(-20.0..=40.0));
        }
        Goal::EfficiencyAbove { f, eff } => {
            f_field(ui, f);
            let mut e = *eff * 100.0;
            if ui.add(egui::DragValue::new(&mut e).suffix(" %").range(1.0..=100.0)).changed() {
                *eff = e / 100.0;
            }
        }
        Goal::FrontToBackAbove { f, db } | Goal::AxialRatioBelow { f, db } | Goal::SideLobesBelow { f, db } => {
            f_field(ui, f);
            ui.add(egui::DragValue::new(db).suffix(" dB").range(-60.0..=60.0));
        }
    });
}

pub fn optimize(ui: &mut egui::Ui, st: &mut Studio) {
    let busy = st.busy();
    theme::caption(ui, "Optimizer");
    weak(ui, "Pick the variables it may change and what the antenna must achieve. Each step is a full simulation, so use a fast mesh and few variables.");
    // Variables list, built on first view, kept across edits.
    let vars = st.project.vars().unwrap_or_default();
    let names = free_vars(st);
    st.ui.opt_vars.retain(|v| names.contains(&v.0));
    for n in &names {
        if !st.ui.opt_vars.iter().any(|v| &v.0 == n) {
            let c = vars.get(n).copied().unwrap_or(0.0);
            st.ui.opt_vars.push((n.clone(), c * 0.8, c * 1.2, false));
        }
    }
    egui::Grid::new("studio_optvars").num_columns(4).spacing(vec2(10.0, 3.0)).striped(true).show(ui, |ui| {
        for t in ["Vary", "Now", "Min", "Max"] {
            ui.label(RichText::new(t).size(12.0).color(theme::WEAK));
        }
        ui.end_row();
        for v in st.ui.opt_vars.iter_mut() {
            ui.checkbox(&mut v.3, &v.0);
            let c = vars.get(&v.0).copied().unwrap_or(0.0);
            ui.label(RichText::new(antenna_core::report::param_text(&v.0, c)).color(theme::WEAK));
            let sp = c.abs() * 0.005;
            ui.add(var_drag(&v.0, &mut v.1, sp));
            ui.add(var_drag(&v.0, &mut v.2, sp));
            ui.end_row();
        }
    });
    if st.ui.goals.is_empty() {
        st.ui.goals = default_goals(st);
    }
    theme::caption(ui, "Goals");
    let f0 = st.project.f0();
    let bw = (st.project.setup.f_hi - st.project.setup.f_lo) * 0.08;
    let mut remove = None;
    let mut goals = std::mem::take(&mut st.ui.goals);
    for (i, (g, w)) in goals.iter_mut().enumerate() {
        group(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(g.label()).size(12.5)).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::icon_button(ui, icon::X, "Remove this goal").clicked() {
                        remove = Some(i);
                    }
                    ui.add(egui::DragValue::new(w).range(0.0..=100.0).speed(0.05).prefix("weight "));
                });
            });
            ui.horizontal_wrapped(|ui| goal_editor(ui, g, i));
        });
    }
    if let Some(i) = remove {
        goals.remove(i);
    }
    st.ui.goals = goals;
    egui::ComboBox::from_id_salt("add_goal").selected_text(theme::label(icon::PLUS, "Add a goal")).show_ui(ui, |ui| {
        let opts: Vec<(&str, Goal)> = vec![
            ("Match over a band", Goal::MatchBelow { port: 0, f_lo: f0 - bw / 2.0, f_hi: f0 + bw / 2.0, db: -10.0 }),
            ("Resonance at a frequency", Goal::ResonanceAt { port: 0, f: f0, tol: f0 * 0.002 }),
            ("Realized gain", Goal::GainAbove { f: f0, dbi: 5.0 }),
            ("Radiation efficiency", Goal::EfficiencyAbove { f: f0, eff: 0.7 }),
            ("Front to back", Goal::FrontToBackAbove { f: f0, db: 15.0 }),
            ("Axial ratio", Goal::AxialRatioBelow { f: f0, db: 3.0 }),
            ("Side lobes", Goal::SideLobesBelow { f: f0, db: -15.0 }),
            ("Port isolation", Goal::IsolationBelow { i: 1, j: 0, f_lo: f0 - bw / 2.0, f_hi: f0 + bw / 2.0, db: -20.0 }),
        ];
        for (name, goal) in opts {
            if ui.selectable_label(false, name).clicked() {
                st.ui.goals.push((goal, 1.0));
            }
        }
    });
    let method_label = |m: Method| match m {
        Method::NelderMead => "Nelder-Mead (local, few runs)",
        Method::ParticleSwarm => "Particle swarm (global)",
        Method::DifferentialEvolution => "Differential evolution (global)",
        Method::Surrogate => "Surrogate model (expensive solvers)",
    };
    egui::Grid::new("studio_opt").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Method");
        egui::ComboBox::from_id_salt("opt_method").selected_text(method_label(st.ui.method)).width(240.0).show_ui(ui, |ui| {
            for m in [Method::NelderMead, Method::Surrogate, Method::ParticleSwarm, Method::DifferentialEvolution] {
                ui.selectable_value(&mut st.ui.method, m, method_label(m));
            }
        });
        ui.end_row();
        ui.label("Most runs");
        ui.add(egui::DragValue::new(&mut st.ui.max_evals).range(4..=5000));
        ui.end_row();
        ui.label("At once");
        ui.add(egui::DragValue::new(&mut st.ui.parallel).range(1..=32));
        ui.end_row();
    });
    let chosen: Vec<(String, f64, f64)> = st.ui.opt_vars.iter().filter(|v| v.3).map(|v| (v.0.clone(), v.1.min(v.2), v.1.max(v.2))).collect();
    if chosen.is_empty() {
        weak(ui, "Tick at least one variable to vary.");
    }
    if run_button(ui, &theme::label(icon::ROCKET_LAUNCH, "Optimize"), !busy && !chosen.is_empty() && !st.ui.goals.is_empty()) {
        let goals = st.ui.goals.clone();
        let (m, n, par) = (st.ui.method, st.ui.max_evals, st.ui.parallel);
        st.run_optimize(chosen, goals, m, n, par);
    }
    let Some(o) = st.opt() else { return };
    theme::caption(ui, "Progress");
    let met = o.best_value < 1e-9;
    tiles(
        ui,
        &[
            (format!("{}", o.evals), "runs", theme::TEXT),
            (if o.best_value < 1e5 { format!("{:.2}", o.best_value) } else { "–".into() }, "penalty (0: all met)", if met { theme::SUCCESS } else { theme::WARNING }),
            (o.method.clone(), "method", theme::TEXT),
        ],
    );
    let hist: Vec<(f64, f64)> = o.history.iter().map(|h| (h.0 as f64, h.1)).collect();
    charts::line_chart(ui, ChartOptions { title: "Best penalty so far", x_unit: "run", height: 120.0, ..Default::default() }, &[Series { name: "Penalty", color: charts::ORANGE, points: &hist }]);
    if !o.best_x.is_empty() {
        egui::Grid::new("studio_optbest").num_columns(2).spacing(vec2(14.0, 2.0)).striped(true).show(ui, |ui| {
            for (n, v) in o.names.iter().zip(&o.best_x) {
                ui.label(n);
                ui.label(antenna_core::report::param_text(n, *v));
                ui.end_row();
            }
        });
        if ui.add_enabled(!busy, egui::Button::new(theme::label(icon::CHECK, "Use these values"))).on_hover_text("Writes them into the variable table").clicked() {
            let values: Vec<(String, f64)> = o.names.iter().cloned().zip(o.best_x.iter().copied()).collect();
            st.project = st.project.with_values(&values);
            if let Some(b) = o.best.clone() {
                st.show_result(b);
            }
            st.ui.message = Some(("The best values are in the variable table. Run again to get field planes at this design.".into(), true));
        }
    }
}

pub fn converge(ui: &mut egui::Ui, st: &mut Studio) {
    let busy = st.busy();
    theme::caption(ui, "Mesh convergence");
    weak(ui, "Starts at this project's mesh and repeats the simulation on finer and finer ones until every S entry and the peak gain stop moving, two passes running. The whole mesh gets finer rather than only where the fields need it, so it costs more than adaptive tetrahedra would.");
    egui::Grid::new("studio_conv").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Stop when the error left is below");
        ui.add(egui::DragValue::new(&mut st.ui.conv_limit).range(0.001..=0.3).speed(0.002).max_decimals(3)).on_hover_text("What the last step says is still left in S, assuming the error falls in step with the mesh: ΔS, the largest complex change of any S entry between two passes, over the density ratio minus one.");
        ui.end_row();
        ui.label("Most passes");
        ui.add(egui::DragValue::new(&mut st.ui.conv_steps).range(3..=10));
        ui.end_row();
    });
    if run_button(ui, &theme::label(icon::PLAY, "Run the study"), !busy) {
        let (l, n) = (st.ui.conv_limit, st.ui.conv_steps);
        st.run_converge(l, n);
    }
    let Some(c) = st.converge() else { return };
    if c.finished {
        if c.converged {
            verdict(ui, true, &format!("Converged at density {:.0}: for two passes running the error left in S is under {:.3} and the peak gain moved less than {} dB.", c.steps.last().map_or(0.0, |s| s.density), c.limit, antenna_core::checks::GAIN_LIMIT_DB));
        } else if let Some(n) = &c.note {
            verdict(ui, false, &format!("Not converged. {n}"));
        } else {
            verdict(ui, false, "Not converged within the passes allowed. Allow more, or relax the limit.");
        }
    }
    egui::Grid::new("studio_convt").num_columns(8).striped(true).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
        for t in ["Density", "Size", "Time", "Resonance", "Gain", "Gain moved", "ΔS", "Left"] {
            ui.label(RichText::new(t).size(12.0).color(theme::WEAK));
        }
        ui.end_row();
        for s in &c.steps {
            ui.label(format!("{:.0}", s.density));
            ui.label(if s.size > 100_000 { format!("{:.1} M", s.size as f64 / 1e6) } else { format!("{}", s.size) });
            ui.label(duration(s.seconds));
            ui.label(format!("{:.3} GHz", s.f_res / 1e9));
            ui.label(if s.peak_dbi.is_finite() { format!("{:.2} dBi", s.peak_dbi) } else { "–".into() });
            ui.label(if s.gain_change_db.is_finite() { format!("{:.2} dB", s.gain_change_db) } else { "–".into() });
            ui.label(if s.delta_s.is_finite() { format!("{:.3}", s.delta_s) } else { "–".into() });
            ui.label(if s.remaining.is_finite() { format!("{:.3}", s.remaining) } else { "–".into() });
            ui.end_row();
        }
    });
    let curves: Vec<(String, Vec<(f64, f64)>)> = c.steps.iter().map(|s| (format!("{:.0}", s.density), s.result.network.f.iter().map(|f| f / 1e9).zip(s.result.network.sij_db(0, 0)).collect())).collect();
    let series: Vec<Series> = curves.iter().enumerate().map(|(i, c)| Series { name: &c.0, color: charts::PALETTE[i % charts::PALETTE.len()], points: &c.1 }).collect();
    charts::line_chart(ui, ChartOptions { title: "|S11| at each density", unit: " dB", x_unit: "GHz", height: 210.0, limit: Some(-10.0), ..Default::default() }, &series);
    if let Some(last) = c.steps.last()
        && ui.add_enabled(!busy, egui::Button::new("Show the finest result")).clicked()
    {
        st.show_result(last.result.clone());
    }
}

pub fn yield_page(ui: &mut egui::Ui, st: &mut Studio) {
    let busy = st.busy();
    theme::caption(ui, "Tolerance and yield");
    weak(ui, "Monte-Carlo analysis: each sample perturbs the chosen dimensions by their manufacturing tolerance. Yield is the share of samples whose worst |S11| over the band still meets the limit.");
    let names = free_vars(st);
    st.ui.tol.retain(|v| names.contains(&v.0));
    for n in &names {
        if !st.ui.tol.iter().any(|v| &v.0 == n) {
            st.ui.tol.push((n.clone(), 0.02, false, 0.0));
        }
    }
    let values = st.project.vars().unwrap_or_default();
    egui::Grid::new("studio_tol").num_columns(3).spacing(vec2(12.0, 3.0)).striped(true).show(ui, |ui| {
        ui.label(RichText::new("Vary").size(12.0).color(theme::WEAK));
        ui.label(RichText::new("Relative (1σ)").size(12.0).color(theme::WEAK));
        ui.label(RichText::new("Plus absolute (1σ)").size(12.0).color(theme::WEAK)).on_hover_text("Etching and machining errors don't scale with the dimension");
        ui.end_row();
        for t in st.ui.tol.iter_mut() {
            ui.checkbox(&mut t.2, &t.0);
            let mut pct = t.1 * 100.0;
            if ui.add(egui::DragValue::new(&mut pct).range(0.0..=50.0).speed(0.05).suffix(" %")).changed() {
                t.1 = pct / 100.0;
            }
            let speed = values.get(&t.0).copied().unwrap_or(1.0).abs() * 0.002;
            ui.add(var_drag(&t.0, &mut t.3, speed).range(0.0..=f64::MAX));
            ui.end_row();
        }
    });
    weak(ui, "Each sample is meshed afresh, so a little of the spread is mesh noise rather than tolerance.");
    let f0 = st.project.f0();
    let bw = (st.project.setup.f_hi - st.project.setup.f_lo) * 0.04;
    let (f_lo, f_hi) = (f0 - bw / 2.0, f0 + bw / 2.0);
    egui::Grid::new("studio_yield").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Samples");
        ui.add(egui::DragValue::new(&mut st.ui.yield_n).range(5..=5000));
        ui.end_row();
        ui.label("Limit");
        ui.add(egui::DragValue::new(&mut st.ui.yield_limit).range(-40.0..=-1.0).suffix(" dB"));
        ui.end_row();
        ui.label("At once");
        ui.add(egui::DragValue::new(&mut st.ui.parallel).range(1..=32));
        ui.end_row();
    });
    weak(ui, format!("Band checked: {:.3} to {:.3} GHz.", f_lo / 1e9, f_hi / 1e9));
    let chosen: Vec<(String, f64, f64)> = st.ui.tol.iter().filter(|t| t.2).map(|t| (t.0.clone(), t.1, t.3)).collect();
    if chosen.is_empty() {
        weak(ui, "Tick at least one dimension.");
    }
    if run_button(ui, &theme::label(icon::PLAY, &format!("Run {} samples", st.ui.yield_n)), !busy && !chosen.is_empty()) {
        let (n, l, p) = (st.ui.yield_n, st.ui.yield_limit, st.ui.parallel);
        st.run_yield(chosen, n, f_lo, f_hi, l, p);
    }
    let Some(y) = st.yield_state() else { return };
    let Some(o) = y.outcome else {
        if y.done > 0 {
            weak(ui, format!("{} of {} samples done.", y.done, y.samples));
        }
        return;
    };
    let solved = o.samples - o.failed;
    let target = crate::antenna::YIELD_TARGET * 100.0;
    let mut text = format!("Yield {:.1} %: {} of {solved} samples meet {:.0} dB (95 % interval {:.0}–{:.0} %).", o.fraction * 100.0, o.passed, y.limit_db, o.ci95.0 * 100.0, o.ci95.1 * 100.0);
    if o.failed > 0 {
        text += &format!(" {} of {} samples would not solve, so this is not a verdict: {}", o.failed, o.samples, o.first_failure.as_deref().unwrap_or("no reason given"));
    } else if !o.meets_target {
        text += &format!(" The data only supports {:.0} %, below the {target:.0} % a pass needs{}.", o.ci95.0 * 100.0, if o.fraction * 100.0 >= target { ": more samples could clear it" } else { "" });
    }
    verdict(ui, o.meets_target, &text);
    tiles(ui, &[(format!("{:.1} dB", o.mean), "mean worst S11", theme::TEXT), (format!("{:.2} dB", o.std), "spread (1σ)", theme::TEXT)]);
    // Histogram of the worst S11, of the samples that solved.
    let solved_values: Vec<f64> = o.values.iter().copied().filter(|v| !v.is_nan()).collect();
    let (lo, hi) = solved_values.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
    let bins = 16;
    let mut counts = vec![0usize; bins];
    for v in &solved_values {
        let k = (((v - lo) / (hi - lo).max(1e-9)) * bins as f64) as usize;
        counts[k.min(bins - 1)] += 1;
    }
    let hist: Vec<(f64, f64)> = counts.iter().enumerate().flat_map(|(i, c)| {
        let a = lo + (hi - lo) * i as f64 / bins as f64;
        let b = lo + (hi - lo) * (i + 1) as f64 / bins as f64;
        [(a, *c as f64), (b, *c as f64)]
    }).collect();
    charts::line_chart(ui, ChartOptions { title: "Distribution of worst |S11|", x_unit: "dB", height: 120.0, ..Default::default() }, &[Series { name: "Samples", color: charts::BLUE, points: &hist }]);
    theme::caption(ui, "What matters most");
    let mut rank: Vec<(String, f64)> = y.names.iter().cloned().zip(o.sensitivities.iter().copied()).collect();
    rank.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (n, s) in rank {
        ui.horizontal(|ui| {
            ui.label(RichText::new(n).monospace());
            ui.add(egui::ProgressBar::new(s.clamp(0.0, 1.0) as f32).desired_height(6.0).desired_width(160.0).fill(theme::ACCENT));
            ui.label(RichText::new(format!("{s:.2}")).size(12.0).color(theme::WEAK));
        });
    }
}

/// A number field for a design variable: shows its unit and accepts `46mm` or `2.4GHz`.
fn var_drag<'a>(name: &'a str, v: &'a mut f64, speed: f64) -> egui::DragValue<'a> {
    egui::DragValue::new(v)
        .speed(speed)
        .custom_formatter(move |x, _| antenna_core::report::param_text(name, x))
        .custom_parser(|s| antenna_core::expr::eval(s, &Default::default()).ok())
}
