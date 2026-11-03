//! The RFID gate test page: the excitation states, the aisle, the tags, and what
//! comes of them. Changing a state or the power re-judges at once; the field only
//! has to be solved again when the model or the aisle moves.

use antenna_core::geometry::Geometry;
use antenna_core::project::{Drive, DriveState};
use antenna_core::recipes::gate::{self, GateReport, GateSpec, HeatMap, ORIENTATIONS, Preset};
use bevy_egui::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, pos2, vec2};
use proof::RobustReport;

use super::{group, tiles, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

/// Page state: the spec being edited and the last judgement.
#[derive(Default)]
pub struct GateUi {
    pub spec: Option<GateSpec>,
    spec_for: String,
    cached: Option<Cached>,
    show_amps: bool,
    pub search_states: usize,
}

struct Cached {
    field: usize,
    spec: GateSpec,
    states: Vec<DriveState>,
    report: GateReport,
    read: RobustReport,
    stray: RobustReport,
}

impl GateUi {
    /// Sets the spec for this project, so opening the page doesn't replace it with the default.
    pub fn keep_spec(&mut self, spec: GateSpec, project: &antenna_core::project::Project) {
        self.spec = Some(spec);
        self.spec_for = spec_key(project);
    }
}

fn spec_key(p: &antenna_core::project::Project) -> String {
    format!("{} {}", p.template, p.name)
}

const AXIS: [&str; 3] = ["x", "y", "z"];

/// The two poles and the neutral middle of the margin scale: a read's threshold sits
/// on the neutral gray, so what reads and what doesn't are opposite hues.
const READS: Color32 = Color32::from_rgb(0x2a, 0x78, 0xd6);
const FAILS: Color32 = Color32::from_rgb(0xe3, 0x49, 0x48);
const NEUTRAL: Color32 = Color32::from_rgb(0xf0, 0xef, 0xec);
const SCALE_DB: f64 = 15.0;

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn margin_color(db: f64) -> Color32 {
    let t = (db / SCALE_DB).clamp(-1.0, 1.0) as f32;
    if t >= 0.0 { lerp(NEUTRAL, READS, t) } else { lerp(NEUTRAL, FAILS, -t) }
}

fn share(v: f64) -> String {
    format!("{:.1} %", v * 100.0)
}

/// Margins on a grid, at true proportions, with a read-out under the pointer.
fn heat_map(ui: &mut egui::Ui, title: &str, map: &HeatMap) {
    let (nu, nv) = (map.u.len(), map.v.len());
    if nu == 0 || nv == 0 || map.db.len() != nu * nv {
        return;
    }
    let span = |a: &[f64]| if a.len() > 1 { a[a.len() - 1] - a[0] } else { 0.0 };
    let (su, sv) = (span(&map.u).max(1e-6), span(&map.v).max(1e-6));
    ui.label(RichText::new(title).size(12.5).color(theme::TEXT));
    let left = 34.0;
    let width = (ui.available_width() - left).min(320.0);
    let scale = (width / su as f32).min(220.0 / sv as f32);
    let (w, h) = (su as f32 * scale, sv as f32 * scale);
    // Room around the plot for the labels at its corners.
    let (top, right) = (7.0, 14.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(left + w + right, top + h + 30.0), Sense::hover());
    let p = ui.painter_at(rect);
    let plot = egui::Rect::from_min_size(rect.min + vec2(left, top), vec2(w, h));
    let (cw, ch) = (w / nu as f32, h / nv as f32);
    for j in 0..nv {
        for i in 0..nu {
            let v = map.db[i + nu * j];
            if v.is_finite() {
                let c = egui::Rect::from_min_size(pos2(plot.left() + i as f32 * cw, plot.bottom() - (j + 1) as f32 * ch), vec2(cw, ch));
                p.rect_filled(c.shrink(0.5), 1.0, margin_color(v));
            }
        }
    }
    p.rect_stroke(plot, 2.0, Stroke::new(1.0, Color32::from_black_alpha(26)), egui::StrokeKind::Outside);
    let small = FontId::proportional(10.5);
    for a in [map.u[0], map.u[nu - 1]] {
        let x = plot.left() + ((a - map.u[0]) / su) as f32 * w;
        p.text(pos2(x, plot.bottom() + 3.0), Align2::CENTER_TOP, format!("{a:.1}"), small.clone(), theme::WEAK);
    }
    p.text(pos2(plot.center().x, plot.bottom() + 15.0), Align2::CENTER_TOP, format!("{} (m)", AXIS[map.axes.0]), small.clone(), theme::WEAK);
    for a in [map.v[0], map.v[nv - 1]] {
        let y = plot.bottom() - ((a - map.v[0]) / sv) as f32 * h;
        p.text(pos2(plot.left() - 4.0, y), Align2::RIGHT_CENTER, format!("{a:.1}"), small.clone(), theme::WEAK);
    }
    p.text(pos2(rect.left(), plot.center().y), Align2::LEFT_CENTER, AXIS[map.axes.1], small, theme::WEAK);
    if let Some(at) = resp.hover_pos()
        && plot.contains(at)
    {
        let i = (((at.x - plot.left()) / cw) as usize).min(nu - 1);
        let j = (((plot.bottom() - at.y) / ch) as usize).min(nv - 1);
        let v = map.db[i + nu * j];
        let (un, vn) = (AXIS[map.axes.0], AXIS[map.axes.1]);
        let line = if v.is_finite() { format!("{un} {:.2} m, {vn} {:.2} m\n{v:+.1} dB: {}", map.u[i], map.v[j], if v >= 0.0 { "reads" } else { "doesn't read" }) } else { format!("{un} {:.2} m, {vn} {:.2} m\nToo close to the metal to say", map.u[i], map.v[j]) };
        resp.clone().on_hover_ui_at_pointer(|ui| {
            ui.label(line);
        });
    }
}

fn scale_legend(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(320.0), 30.0), Sense::hover());
    let p = ui.painter_at(rect);
    let bar = egui::Rect::from_min_size(rect.min, vec2(rect.width(), 9.0));
    let n = 60;
    for k in 0..n {
        let t = k as f32 / (n - 1) as f32;
        let x = bar.left() + bar.width() * k as f32 / n as f32;
        p.rect_filled(egui::Rect::from_min_size(pos2(x, bar.top()), vec2(bar.width() / n as f32 + 0.5, bar.height())), 0.0, margin_color((t as f64 * 2.0 - 1.0) * SCALE_DB));
    }
    let f = FontId::proportional(10.5);
    p.text(pos2(bar.left(), bar.bottom() + 3.0), Align2::LEFT_TOP, format!("−{SCALE_DB:.0} dB, no read"), f.clone(), theme::WEAK);
    p.text(pos2(bar.center().x, bar.bottom() + 3.0), Align2::CENTER_TOP, "0", f.clone(), theme::WEAK);
    p.text(pos2(bar.right(), bar.bottom() + 3.0), Align2::RIGHT_TOP, format!("reads, +{SCALE_DB:.0} dB"), f, theme::WEAK);
}

/// A bar per tag orientation, worst first.
fn orientation_bars(ui: &mut egui::Ui, by: &[(String, f64)], goal: f64) {
    ui.label(RichText::new("Tags read walking through, by the way the tag points").size(12.5).color(theme::TEXT));
    let total = ui.available_width().min(380.0);
    let (label_w, value_w, row) = (38.0, 52.0, 15.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(total, row * by.len() as f32 + 2.0), Sense::hover());
    let p = ui.painter_at(rect);
    let track = total - label_w - value_w;
    let f = FontId::proportional(11.0);
    for (k, (name, v)) in by.iter().enumerate() {
        let y = rect.top() + row * k as f32;
        p.text(pos2(rect.left(), y + row / 2.0), Align2::LEFT_CENTER, name, f.clone(), theme::WEAK);
        let x0 = rect.left() + label_w;
        let bar = egui::Rect::from_min_size(pos2(x0, y + 3.0), vec2((track * *v as f32).max(1.0), row - 6.0));
        p.rect_filled(bar, egui::CornerRadius { nw: 0, sw: 0, ne: 3, se: 3 }, READS);
        p.text(pos2(rect.right(), y + row / 2.0), Align2::RIGHT_CENTER, share(*v), f.clone(), theme::TEXT);
    }
    // The goal, a thin rule through every bar.
    let gx = rect.left() + label_w + track * goal as f32;
    p.line_segment([pos2(gx, rect.top()), pos2(gx, rect.bottom())], Stroke::new(1.0, theme::TEXT));
    if let Some(at) = resp.hover_pos() {
        let k = (((at.y - rect.top()) / row) as usize).min(by.len() - 1);
        let line = format!("Tag along {}: {} of walks read. Goal {}.", by[k].0, share(by[k].1), share(goal));
        resp.clone().on_hover_ui_at_pointer(|ui| {
            ui.label(line);
        });
    }
}

fn number(ui: &mut egui::Ui, label: &str, v: &mut f64, range: std::ops::RangeInclusive<f64>, speed: f64, suffix: &str) {
    ui.label(label);
    ui.add(egui::DragValue::new(v).range(range).speed(speed).suffix(suffix.to_string()).max_decimals(2));
    ui.end_row();
}

fn states_table(ui: &mut egui::Ui, st: &mut Studio, ports: usize) {
    group(ui, |ui| {
        ui.horizontal(|ui| {
            theme::caption(ui, "Excitation states");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut st.ui.gate.show_amps, "Amplitudes");
            });
        });
        weak(ui, "The phase at each port, as a network or a switch would set it. Several states mean the reader cycles through them, each scaled to the same total power.");
        for s in &mut st.project.states {
            while s.drive.len() < ports {
                s.drive.push(Drive { amp: 1.0, phase: 0.0 });
            }
            s.drive.truncate(ports);
        }
        let names: Vec<String> = st.ui.geometry.as_ref().map_or(Vec::new(), |g| g.ports.iter().map(|p| p.name.clone()).collect());
        let mut drop_row = None;
        let show_amps = st.ui.gate.show_amps;
        egui::ScrollArea::horizontal().id_salt("gate_states").show(ui, |ui| {
            egui::Grid::new("gate_states_grid").num_columns(ports + 2).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                ui.label("");
                for n in names.iter().take(ports) {
                    ui.label(RichText::new(n).size(11.5).color(theme::WEAK));
                }
                ui.label("");
                ui.end_row();
                let many = st.project.states.len() > 1;
                for (r, s) in st.project.states.iter_mut().enumerate() {
                    ui.add_sized(vec2(112.0, 22.0), egui::TextEdit::singleline(&mut s.name));
                    for d in &mut s.drive {
                        ui.vertical(|ui| {
                            ui.add(egui::DragValue::new(&mut d.phase).range(-180.0..=180.0).speed(1.0).suffix("°").max_decimals(0));
                            if show_amps {
                                ui.add(egui::DragValue::new(&mut d.amp).range(0.0..=10.0).speed(0.01).prefix("× ").max_decimals(2));
                            }
                        });
                    }
                    if many && theme::icon_button(ui, icon::TRASH, "Remove this state").clicked() {
                        drop_row = Some(r);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(r) = drop_row {
            st.project.states.remove(r);
        }
        ui.horizontal(|ui| {
            if ui.button(theme::label(icon::PLUS, "Add a state")).clicked() {
                let next = st.project.states.last().cloned().map_or_else(|| gate::preset_states(Preset::InPhase, ports).remove(0), |mut s| {
                    s.name = format!("State {}", st.project.states.len() + 1);
                    s
                });
                st.project.states.push(next);
            }
            egui::ComboBox::from_id_salt("gate_preset").selected_text("Replace with…").width(170.0).show_ui(ui, |ui| {
                let turn = (360 / ports.max(1)) as i32;
                let mut list = vec![Preset::InPhase, Preset::Progressive(90), Preset::Alternating, Preset::OneAtATime, Preset::Sweep];
                if turn != 90 && ports > 1 {
                    list.insert(1, Preset::Progressive(turn));
                }
                for preset in list {
                    if ui.selectable_label(false, preset.label()).clicked() {
                        st.project.states = gate::preset_states(preset, ports);
                    }
                }
            });
        });
    });
}

fn aisle_group(ui: &mut egui::Ui, spec: &mut GateSpec, reset: &mut bool) {
    group(ui, |ui| {
        ui.horizontal(|ui| {
            theme::caption(ui, "The aisle");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset").on_hover_text("Back to the aisle this model suggests").clicked() {
                    *reset = true;
                }
            });
        });
        let a = &mut spec.aisle;
        egui::Grid::new("gate_aisle").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            ui.label("Across, walking along");
            ui.horizontal(|ui| {
                let mut across = a.across;
                egui::ComboBox::from_id_salt("gate_across").selected_text(AXIS[across]).width(44.0).show_ui(ui, |ui| {
                    for k in 0..3 {
                        ui.selectable_value(&mut across, k, AXIS[k]);
                    }
                });
                let mut walk = a.walk;
                egui::ComboBox::from_id_salt("gate_walk").selected_text(AXIS[walk]).width(44.0).show_ui(ui, |ui| {
                    for k in 0..3 {
                        ui.selectable_value(&mut walk, k, AXIS[k]);
                    }
                });
                if across != walk {
                    a.across = across;
                    a.walk = walk;
                }
            });
            ui.end_row();
            number(ui, "Panel front at", &mut a.start, -50.0..=50.0, 0.01, " m");
            number(ui, "Aisle width", &mut a.width, 0.1..=10.0, 0.01, " m");
            number(ui, "Read zone length", &mut a.length, 0.1..=10.0, 0.01, " m");
            number(ui, "Tag height, lowest", &mut a.low, -50.0..=50.0, 0.01, " m");
            number(ui, "Tag height, highest", &mut a.high, -50.0..=50.0, 0.01, " m");
            number(ui, "Displays start beyond", &mut a.stray_from, 0.1..=20.0, 0.05, " m");
            number(ui, "Displays run for", &mut a.stray_depth, 0.1..=20.0, 0.05, " m");
            number(ui, "Sample every", &mut spec.spacing, 0.02..=1.0, 0.005, " m");
        });
    });
}

fn link_group(ui: &mut egui::Ui, spec: &mut GateSpec) {
    group(ui, |ui| {
        theme::caption(ui, "Reader, tags and goals");
        egui::Grid::new("gate_link").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            number(ui, "Reader power", &mut spec.tx_dbm, 0.0..=36.0, 0.1, " dBm");
            number(ui, "Cable and splitter loss", &mut spec.feed_loss_db, 0.0..=20.0, 0.1, " dB");
            number(ui, "Reader sensitivity", &mut spec.reader_sensitivity_dbm, -110.0..=-30.0, 0.5, " dBm");
            number(ui, "Tag chip sensitivity", &mut spec.tag_sensitivity_dbm, -35.0..=-5.0, 0.1, " dBm");
            number(ui, "Tag gain", &mut spec.tag_gain_dbi, -20.0..=6.0, 0.1, " dBi");
            number(ui, "Loss from the product", &mut spec.product_loss_db.0, 0.0..=30.0, 0.1, " dB");
            number(ui, "… give or take", &mut spec.product_loss_db.1, 0.0..=15.0, 0.1, " dB");
            number(ui, "Walking speed", &mut spec.walk_speed, 0.1..=5.0, 0.05, " m/s");
            let mut ms = spec.dwell_s * 1e3;
            number(ui, "Time in each state", &mut ms, 1.0..=2000.0, 1.0, " ms");
            spec.dwell_s = ms / 1e3;
            let mut read = spec.min_read * 100.0;
            number(ui, "Read at least", &mut read, 0.0..=100.0, 0.1, " %");
            spec.min_read = read / 100.0;
            let mut stray = spec.max_stray * 100.0;
            number(ui, "Stray at most", &mut stray, 0.0..=100.0, 0.1, " %");
            spec.max_stray = stray / 100.0;
            number(ui, "EIRP limit", &mut spec.eirp_limit_dbm, 0.0..=60.0, 0.5, " dBm");
        });
    });
}

fn state_scores(ui: &mut egui::Ui, r: &GateReport, limit: f64) {
    egui::Grid::new("gate_scores").num_columns(5).spacing(vec2(12.0, 3.0)).striped(true).show(ui, |ui| {
        for h in ["State", "Reads alone", "Stray", "Reflected", "EIRP"] {
            ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
        }
        ui.end_row();
        for s in &r.states {
            ui.label(RichText::new(&s.name).size(12.5));
            ui.label(share(s.read));
            ui.label(share(s.stray));
            ui.label(format!("{:.1} dB", s.tarc_db));
            match s.eirp_dbm {
                Some(e) if e > limit => {
                    ui.label(RichText::new(format!("{} {e:.1} dBm, over", icon::WARNING)).color(theme::WARNING));
                }
                Some(e) => {
                    ui.label(format!("{e:.1} dBm"));
                }
                None => {
                    ui.label("–");
                }
            }
            ui.end_row();
        }
    });
}

pub fn page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "RFID gate test");
    weak(ui, "An aisle sits in the near field of its own antennas, so a pattern can't say what reads there. This samples the solved field where tags walk, for every way a tag can point, and adds the ports up under each state.");
    if let Some(found) = st.take_found_states() {
        st.project.states = found;
    }
    let geometry = match &st.ui.geometry {
        Ok(g) => g.clone(),
        Err(e) => {
            verdict(ui, false, e);
            return;
        }
    };
    if st.project.reconfig.is_on() {
        verdict(ui, false, "This model has switches or varactors on some of its ports, and the gate test drives every port. Remove them on the Reconfigurable page to test it as a gate.");
        return;
    }
    let ports = geometry.ports.len();
    if ports == 0 {
        verdict(ui, false, "The model has no ports. Add them under Parts and ports.");
        return;
    }
    let key = spec_key(&st.project);
    if st.ui.gate.spec.is_none() || st.ui.gate.spec_for != key {
        st.ui.gate.spec = Some(GateSpec::for_project(&st.project, &geometry));
        st.ui.gate.spec_for = key;
    }
    if st.project.states.is_empty() {
        st.project.states = gate::default_states(ports);
    }
    if st.ui.gate.search_states == 0 {
        st.ui.gate.search_states = 2;
    }
    let mut spec = st.ui.gate.spec.unwrap_or_else(|| GateSpec::for_project(&st.project, &geometry));
    spec.f = st.project.f0();

    states_table(ui, st, ports);
    // The answer comes before the form that shapes it: it's what the page is for.
    solve_and_results(ui, st, &geometry, ports, &mut spec);
    let mut reset = false;
    aisle_group(ui, &mut spec, &mut reset);
    if reset {
        spec = GateSpec { aisle: GateSpec::for_project(&st.project, &geometry).aisle, spacing: 0.1, ..spec };
    }
    link_group(ui, &mut spec);
    st.ui.gate.spec = Some(spec);
}

fn solve_and_results(ui: &mut egui::Ui, st: &mut Studio, geometry: &Geometry, ports: usize, spec_out: &mut GateSpec) {
    let spec = *spec_out;
    let run = st.gate_run();
    let fresh = run.as_ref().is_some_and(|r| r.applies_to(&st.project, &spec));
    let fit = geometry.solver_fit();
    let fdtd = format!("FDTD, one run per port ({ports} runs): minutes, more for a big aisle.");
    let about = match st.project.setup.solver {
        antenna_core::project::SolverKind::Fdtd | antenna_core::project::SolverKind::Fit => fdtd,
        antenna_core::project::SolverKind::Auto if !(fit.wire_mom || fit.surface_mom) => format!("It has dielectrics. {fdtd}"),
        antenna_core::project::SolverKind::Auto => "Metal only, so the method of moments if it stays small enough at this frequency: seconds. Otherwise FDTD: minutes.".to_string(),
        s => s.label().to_string(),
    };
    weak(ui, about);
    let label = if fresh { "Solve the gate again" } else { "Solve the gate" };
    if super::run_button(ui, &theme::label(icon::PLAY, label), !st.busy()) {
        st.run_gate(spec);
    }
    let Some(run) = run else { return };
    if !fresh {
        verdict(ui, false, "The model or the aisle changed since the last solve. Solve the gate again for results.");
        return;
    }
    // Judge again only when something changed.
    let id = std::sync::Arc::as_ptr(&run.field) as usize;
    let same = st.ui.gate.cached.as_ref().is_some_and(|c| c.field == id && c.spec == spec && c.states == st.project.states);
    if !same {
        let report = gate::evaluate(&run.field, &spec, &st.project.states);
        let (read, stray) = gate::predict(&report, &spec, 300);
        st.ui.gate.cached = Some(Cached { field: id, spec, states: st.project.states.clone(), report, read, stray });
    }
    let Some(c) = st.ui.gate.cached.as_ref() else { return };
    let r = &c.report;
    theme::caption(ui, &format!("At {:.3} GHz, {:.1} dBm", r.f / 1e9, spec.tx_dbm));
    let read_ok = r.read >= spec.min_read;
    let stray_ok = r.stray <= spec.max_stray;
    let worst = r.by_orientation.first().cloned().unwrap_or_default();
    let loud = r.states.iter().filter_map(|s| s.eirp_dbm).fold(f64::NEG_INFINITY, f64::max);
    let ok_or = |good: bool| if good { theme::SUCCESS } else { theme::WARNING };
    tiles(
        ui,
        &[
            (share(r.read), "read walking through", ok_or(read_ok)),
            (share(r.stray), "on display, read", ok_or(stray_ok)),
            (format!("{} {}", share(worst.1), worst.0), "worst way to point", theme::TEXT),
            (if loud.is_finite() { format!("{loud:.1} dBm") } else { "–".into() }, "EIRP, loudest state", ok_or(!loud.is_finite() || loud <= spec.eirp_limit_dbm)),
            (r.power_for_goal.map_or("not reached".into(), |p| format!("{:.1} dBm", p.0)), "power for the goal", theme::TEXT),
            (if r.reverse_limited { "Reader".to_string() } else { "Tag chip".to_string() }, "what limits", theme::TEXT),
        ],
    );
    let mut use_power = None;
    if let Some((tx, read, stray)) = r.legal {
        verdict(ui, false, &format!("{loud:.1} dBm EIRP is over the {:.0} dBm limit. Keeping to it means {tx:.1} dBm at the reader, where {} read and {} on display read.", spec.eirp_limit_dbm, share(read), share(stray)));
        if ui.button(format!("Use {tx:.1} dBm")).clicked() {
            use_power = Some((tx * 10.0).floor() / 10.0);
        }
    } else if read_ok && stray_ok {
        verdict(ui, true, "Meets the read and stray goals, within the EIRP limit.");
    } else {
        let mut why = Vec::new();
        if !read_ok {
            why.push(format!("reads {} against a goal of {}", share(r.read), share(spec.min_read)));
        }
        if !stray_ok {
            why.push(format!("reads {} on display against at most {}", share(r.stray), share(spec.max_stray)));
        }
        verdict(ui, false, &format!("Misses: {}.", why.join("; ")));
    }
    // The evidence label stays on the number.
    let driver = c.read.main_driver().map_or(String::new(), |d| format!(" {} moves it most.", d.factor));
    weak(ui, format!("{}: {}–{} read, {}–{} on display, across tag and product spread.{driver} Not measured yet.", c.read.prediction.evidence.label(), share(c.read.prediction.low), share(c.read.prediction.high), share(c.stray.prediction.low), share(c.stray.prediction.high)));
    state_scores(ui, r, spec.eirp_limit_dbm);
    if r.states.len() > 1 {
        weak(ui, format!("Cycling {} states at {:.0} ms each, a tag counts as read if one state covers it for a whole trip round them: {:.2} m at this walking speed.", r.states.len(), spec.dwell_s * 1e3, r.states.len() as f64 * spec.dwell_s * spec.walk_speed));
    }
    orientation_bars(ui, &r.by_orientation, spec.min_read);
    heat_map(ui, "Each lane through the gate, at its worst tag direction", &r.lane_map);
    heat_map(ui, "From above, at mid height, at the worst tag direction", &r.plane_map);
    scale_legend(ui);
    weak(ui, "Blank cells are too close to the metal to say.");
    let unknown = run.field.samples.excluded;
    if unknown > 0 {
        weak(ui, format!("{unknown} samples were left out for that. {} {} samples in all, {} s to solve.", run.field.solver, run.field.samples.points.len(), run.field.seconds.round()));
    }
    for w in &r.warnings {
        verdict(ui, false, w);
    }
    let mut show = false;
    group(ui, |ui| {
        theme::caption(ui, "Find better phases");
        weak(ui, "Searches the phases on this solved field, at the power each candidate may legally use. The states it finds replace the ones above.");
        ui.horizontal(|ui| {
            ui.label("States");
            ui.add(egui::DragValue::new(&mut st.ui.gate.search_states).range(1..=6));
            if ui.add_enabled(!st.busy(), egui::Button::new(theme::label(icon::SPARKLE, "Search"))).clicked() {
                let n = st.ui.gate.search_states;
                st.search_gate_phases(spec, n, 800);
            }
            if ui.button("See the first state on the far field page").clicked() {
                show = true;
            }
        });
    });
    egui::CollapsingHeader::new("What this assumes").id_salt("gate_assumes").show(ui, |ui| {
        for a in gate::assumptions(&run.field, &spec) {
            weak(ui, format!("• {a}"));
        }
        weak(ui, format!("• Tag directions: {}.", ORIENTATIONS.map(|o| o.0).join(", ")));
    });
    if let Some(tx) = use_power {
        spec_out.tx_dbm = tx;
    }
    if show && let Some(first) = st.project.states.first().cloned() {
        st.project.drive = first.drive;
        st.ui.use_drive = true;
        st.ui.page = super::Page::FarField;
    }
}
