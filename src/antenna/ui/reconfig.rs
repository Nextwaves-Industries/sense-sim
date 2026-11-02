//! The reconfigurable antenna page: the parts across the gaps, their states, what comes
//! of them, and the search for settings that meet a target. Changing a part or a state
//! re-judges at once; only moving metal needs another solve.

use antenna_core::geometry::Geometry;
use antenna_core::project::{ExtraPort, Project};
use antenna_core::reconfig::{self, Device, Element, Found, Model, Mount, PolGoal, Reconfig, Report, Setting, Target, TunePoint, TuneState};
use bevy_egui::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, pos2, vec2};

use super::charts::{self, ChartOptions, Series};
use super::{Page, duration, ghz_field, group, run_button, tiles, verdict, weak};
use crate::antenna::Studio;
use crate::antenna::reconfig::{ReconfigRun, states_from_found};
use crate::theme::{self, icon};

/// Page state.
pub struct ReconfigUi {
    /// The state whose parts are shown in detail, and marked on the canvas.
    pub shown: usize,
    pub found: Option<Vec<Found>>,
    cached: Option<Cached>,
    search_steps: usize,
    /// Part-to-part spread, percent.
    spread: f64,
    spread_out: Vec<(String, proof::RobustReport)>,
    tune_which: Option<usize>,
    add_port: String,
    add_part: String,
    new_gap: NewGap,
    all_steps: usize,
    tune_runs: usize,
    /// The part whose values are open.
    open_part: Option<usize>,
    /// The frequency to show a state's pattern at, once its result arrives.
    pub view_f: Option<f64>,
}

impl Default for ReconfigUi {
    fn default() -> Self {
        Self {
            shown: 0,
            found: None,
            cached: None,
            search_steps: 9,
            spread: 10.0,
            spread_out: Vec::new(),
            tune_which: None,
            add_port: String::new(),
            add_part: "RF switch, generic".into(),
            new_gap: NewGap::default(),
            all_steps: 3,
            tune_runs: 40,
            open_part: None,
            view_f: None,
        }
    }
}

#[derive(Clone)]
struct NewGap {
    a: [String; 3],
    b: [String; 3],
    width: String,
}

impl Default for NewGap {
    fn default() -> Self {
        let z = || String::from("0");
        Self { a: [z(), z(), z()], b: [z(), z(), "1mm".into()], width: z() }
    }
}

struct Cached {
    basis: usize,
    rc: Reconfig,
    tune_which: Option<usize>,
    report: Result<Report, String>,
    curve: Vec<TunePoint>,
}

const STATES_LISTED: usize = 64;
const ON: Color32 = Color32::from_rgb(0x1f, 0x9d, 0x55);
const BIAS: Color32 = Color32::from_rgb(0xd9, 0x82, 0x1a);

/// A part's colour on the canvas for a setting: on, off or biased.
pub fn mark_color(device: &Device, s: Setting) -> Color32 {
    match (device.model, device.normalized(s)) {
        (Model::Switch { .. }, Setting::On) => ON,
        (Model::Switch { .. }, _) => charts::GREY,
        (Model::Varactor { .. }, _) => BIAS,
        (Model::Fixed { .. }, _) => charts::BLUE,
    }
}

/// What the canvas marks: each part's port, colour and name, in the state on show.
pub fn canvas_marks(st: &Studio) -> Vec<(String, Color32, String)> {
    let rc = &st.project.reconfig;
    if !rc.is_on() {
        return Vec::new();
    }
    let k = if st.ui.page == Page::Reconfig { st.ui.reconfig.shown } else { rc.active };
    let state = rc.states.get(k.min(rc.states.len().saturating_sub(1)));
    rc.elements
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let s = state.and_then(|st| st.settings.get(i).copied()).unwrap_or_else(|| e.device.default_setting());
            (e.port.clone(), mark_color(&e.device, s), format!("{} {}", e.name, e.device.describe(s)))
        })
        .collect()
}

fn value(ui: &mut egui::Ui, label: &str, v: &mut f64, unit: f64, suffix: &str, speed: f64) {
    ui.label(RichText::new(label).size(12.5));
    let mut x = *v / unit;
    if ui.add(egui::DragValue::new(&mut x).speed(speed).range(0.0..=1e9).suffix(suffix.to_string()).max_decimals(3)).changed() {
        *v = x * unit;
    }
    ui.end_row();
}

/// The part's own numbers, and what the board adds.
fn part_values(ui: &mut egui::Ui, k: usize, e: &mut Element) {
    egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 0, top: 2, bottom: 4 }).show(ui, |ui| {
        let before = e.device.clone();
        egui::Grid::new(("rc_vals", k)).num_columns(2).spacing(vec2(12.0, 3.0)).show(ui, |ui| {
            match &mut e.device.model {
                Model::Switch { r_on, c_off, r_off, ls, bias_ma, v_forward, v_reverse } => {
                    value(ui, "Resistance, on", r_on, 1.0, " Ω", 0.05);
                    value(ui, "Capacitance, off", c_off, 1e-12, " pF", 0.005);
                    value(ui, "Parallel resistance, off (0: none)", r_off, 1e3, " kΩ", 0.5);
                    value(ui, "Series inductance", ls, 1e-9, " nH", 0.01);
                    value(ui, "Bias current, on", bias_ma, 1.0, " mA", 0.1);
                    value(ui, "Forward voltage, on", v_forward, 1.0, " V", 0.01);
                    value(ui, "Reverse bias, off", v_reverse, 1.0, " V", 0.1);
                }
                Model::Varactor { cj0, vj, m, cp, rs, ls, v_max } => {
                    value(ui, "Junction capacitance at 0 V", cj0, 1e-12, " pF", 0.01);
                    value(ui, "Junction potential", vj, 1.0, " V", 0.01);
                    value(ui, "Grading exponent", m, 1.0, "", 0.01);
                    value(ui, "Package capacitance", cp, 1e-12, " pF", 0.005);
                    value(ui, "Series resistance", rs, 1.0, " Ω", 0.02);
                    value(ui, "Series inductance", ls, 1e-9, " nH", 0.01);
                    value(ui, "Highest reverse bias", v_max, 1.0, " V", 0.5);
                }
                Model::Fixed { r, l, c } => {
                    value(ui, "Resistance", r, 1.0, " Ω", 0.05);
                    value(ui, "Inductance", l, 1e-9, " nH", 0.05);
                    value(ui, "Capacitance (0: none)", c, 1e-12, " pF", 0.01);
                }
            }
            value(ui, "Limit, RF volts (0: none)", &mut e.device.v_limit, 1.0, " V", 0.5);
            value(ui, "Limit, RF current (0: none)", &mut e.device.i_limit, 1e-3, " mA", 1.0);
            value(ui, "Limit, dissipation (0: none)", &mut e.device.p_limit, 1e-3, " mW", 1.0);
            ui.label(RichText::new("On the board").size(12.0).color(theme::WEAK));
            ui.end_row();
            value(ui, "DC block in series (0: none)", &mut e.mount.c_block, 1e-12, " pF", 0.5);
            value(ui, "Layout inductance in series", &mut e.mount.l_series, 1e-9, " nH", 0.01);
            value(ui, "Bias choke across it (0: none)", &mut e.mount.l_choke, 1e-9, " nH", 0.5);
            value(ui, "Choke's self-resonance (0: ideal)", &mut e.mount.choke_srf, 1e9, " GHz", 0.05);
            value(ui, "Choke's Q (0: ideal)", &mut e.mount.choke_q, 1.0, "", 1.0);
            value(ui, "Pads and gap across it", &mut e.mount.c_pad, 1e-12, " pF", 0.005);
        });
        if e.device.model != before.model && !e.device.source.ends_with("by hand.") {
            e.device.source = format!("{} with values set by hand.", e.device.part);
        }
        weak(ui, e.device.source.clone());
    });
}

/// A part number short enough for its row. The whole name is in the list.
fn short_name(part: &str) -> String {
    if part.chars().count() <= 16 {
        part.to_string()
    } else {
        format!("{}…", part.chars().take(15).collect::<String>())
    }
}

fn parts_group(ui: &mut egui::Ui, st: &mut Studio, geometry: Option<&Geometry>) {
    let ports: Vec<String> = geometry.map_or(Vec::new(), |g| g.ports.iter().map(|p| p.name.clone()).collect());
    let library = Device::library();
    group(ui, |ui| {
        theme::caption(ui, "Parts across the gaps");
        if !st.project.reconfig.elements.is_empty() {
            weak(ui, "Each one's name, the port it sits across, and what it is.");
        }
        let mut remove = None;
        egui::ScrollArea::horizontal().id_salt("rc_part_rows").show(ui, |ui| {
        for k in 0..st.project.reconfig.elements.len() {
            let e = &mut st.project.reconfig.elements[k];
            ui.horizontal(|ui| {
                ui.add_sized(vec2(66.0, 22.0), egui::TextEdit::singleline(&mut e.name));
                egui::ComboBox::from_id_salt(("rc_port", k)).selected_text(e.port.clone()).width(64.0).truncate().show_ui(ui, |ui| {
                    for p in &ports {
                        ui.selectable_value(&mut e.port, p.clone(), p);
                    }
                });
                let mut part = e.device.part.clone();
                egui::ComboBox::from_id_salt(("rc_kind", k)).selected_text(short_name(&part)).width(124.0).show_ui(ui, |ui| {
                    for d in &library {
                        ui.selectable_value(&mut part, d.part.clone(), &d.part);
                    }
                });
                if part != e.device.part
                    && let Some(d) = library.iter().find(|d| d.part == part)
                {
                    e.device = d.clone();
                }
                let open = st.ui.reconfig.open_part == Some(k);
                if theme::icon_button_colored(ui, icon::SLIDERS_HORIZONTAL, &format!("{}. Click for its values and mount.", e.device.summary()), None, open).clicked() {
                    st.ui.reconfig.open_part = if open { None } else { Some(k) };
                }
                if theme::icon_button(ui, icon::TRASH, "Remove this part").clicked() {
                    remove = Some(k);
                }
            });
            if st.ui.reconfig.open_part == Some(k) {
                weak(ui, e.device.summary());
                part_values(ui, k, e);
            }
        }
        });
        if let Some(k) = remove {
            st.project.reconfig.remove_element(k);
            st.ui.reconfig.found = None;
            st.ui.reconfig.open_part = None;
        }
        let none_yet = st.project.reconfig.elements.is_empty();
        egui::CollapsingHeader::new(RichText::new("Add a part").size(12.5)).id_salt("rc_add").default_open(none_yet).show(ui, |ui| add_part(ui, st, &ports, &library));
    });
}

fn add_part(ui: &mut egui::Ui, st: &mut Studio, ports: &[String], library: &[Device]) {
    let taken: Vec<String> = st.project.reconfig.elements.iter().map(|e| e.port.clone()).collect();
    let free: Vec<&String> = ports.iter().filter(|p| !taken.contains(p)).collect();
    // A port has to stay the feed.
    let spare: Vec<&String> = if free.len() > 1 { free.iter().skip(1).copied().collect() } else { Vec::new() };
    let r = &mut st.ui.reconfig;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("rc_addpart").selected_text(r.add_part.clone()).width(168.0).show_ui(ui, |ui| {
            for d in library {
                ui.selectable_value(&mut r.add_part, d.part.clone(), &d.part);
            }
        });
    });
    let device = library.iter().find(|d| d.part == r.add_part).cloned().unwrap_or_else(|| library[0].clone());
    let name_for = |rc: &Reconfig| (1..).map(|k| format!("D{k}")).find(|n| !rc.elements.iter().any(|e| e.name == *n)).unwrap_or_else(|| "D".to_string());
    ui.horizontal_wrapped(|ui| {
        if ui.button(theme::label(icon::NOTE_PENCIL, "Click across a gap")).on_hover_text("Opens the drawing page. Click one side of a gap, then the other, and the part lands there.").clicked() {
            st.ui.draw.next_part = Some(device.clone());
            st.ui.draw.tool = super::draw::Tool::Feed;
            st.ui.page = Page::Draw;
        }
        if !spare.is_empty() {
            ui.label(RichText::new("or on port").size(12.5).color(theme::WEAK));
            if !spare.iter().any(|p| **p == r.add_port) {
                r.add_port = spare[0].clone();
            }
            egui::ComboBox::from_id_salt("rc_addport").selected_text(r.add_port.clone()).width(90.0).show_ui(ui, |ui| {
                for p in &spare {
                    ui.selectable_value(&mut r.add_port, (*p).clone(), *p);
                }
            });
            if ui.button("Put it there").clicked() {
                let name = name_for(&st.project.reconfig);
                let port = r.add_port.clone();
                st.project.reconfig.add_element(Element { name, port, device: device.clone(), mount: Mount::default() });
            }
        }
    });
    egui::CollapsingHeader::new(RichText::new("Or give the gap's coordinates").size(12.5)).id_salt("rc_addcoords").show(ui, |ui| {
        let g = &mut st.ui.reconfig.new_gap;
        egui::Grid::new("rc_newgap").num_columns(2).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
            for (label, v) in [("From x y z", &mut g.a), ("To x y z", &mut g.b)] {
                ui.label(label);
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 16.0) / 3.0;
                    for s in v.iter_mut() {
                        ui.add(egui::TextEdit::singleline(s).desired_width(w.max(40.0)));
                    }
                });
                ui.end_row();
            }
            ui.label("Width");
            ui.add(egui::TextEdit::singleline(&mut g.width).desired_width(100.0));
            ui.end_row();
        });
        weak(ui, "Expressions, so it follows the variables. The ends differ along one axis and touch the metal either side. On unbroken metal, the part sits in a cut across it.");
        if ui.button(theme::label(icon::PLUS, "Add the part")).clicked() {
            let g = st.ui.reconfig.new_gap.clone();
            let name = name_for(&st.project.reconfig);
            let port = antenna_core::sketch::free_name(&st.project, &name);
            let mut p = st.project.clone();
            p.extra_ports.push(ExtraPort { name: port.clone(), a: g.a, b: g.b, width: g.width, z0: "50".into() });
            match p.geometry() {
                Ok(_) => {
                    p.reconfig.add_element(Element { name, port, device, mount: Mount::default() });
                    st.project = p;
                    st.ui.message = None;
                }
                Err(e) => st.ui.message = Some((format!("{e:#}"), false)),
            }
        }
    });
}

fn setting_cell(ui: &mut egui::Ui, device: &Device, s: &mut Setting) {
    match (device.model, device.normalized(*s)) {
        (Model::Varactor { v_max, .. }, now) => {
            let mut v = if let Setting::Volts(v) = now { v } else { 0.0 };
            if ui.add(egui::DragValue::new(&mut v).range(0.0..=v_max).speed(0.1).suffix(" V").max_decimals(1)).changed() {
                *s = Setting::Volts(v);
            }
        }
        (Model::Fixed { .. }, _) => {
            ui.label(RichText::new("fixed").size(11.5).color(theme::WEAK));
        }
        (_, now) => {
            let on = now == Setting::On;
            let text = RichText::new(if on { "On" } else { "Off" }).size(12.5).color(if on { ON } else { charts::GREY });
            if ui.add(egui::Button::new(text).min_size(vec2(30.0, 20.0))).on_hover_text("Click to switch").clicked() {
                *s = if on { Setting::Off } else { Setting::On };
            }
        }
    }
}

fn states_group(ui: &mut egui::Ui, st: &mut Studio) {
    group(ui, |ui| {
        theme::caption(ui, "States");
        weak(ui, "Each state is one way of setting the parts. The marked one is what an ordinary run, sweep or optimisation solves.");
        let elements = st.project.reconfig.elements.clone();
        let f0 = st.project.f0();
        let shown = &mut st.ui.reconfig.shown;
        let rc = &mut st.project.reconfig;
        for s in &mut rc.states {
            s.settings.resize_with(elements.len(), || Setting::Off);
        }
        let many = rc.states.len() > 1;
        let (mut drop_row, mut pick) = (None, None);
        let active = rc.active;
        egui::ScrollArea::horizontal().id_salt("rc_states").show(ui, |ui| {
            egui::Grid::new("rc_states_grid").num_columns(elements.len() + 2).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
                ui.label("");
                for e in &elements {
                    ui.label(RichText::new(&e.name).size(11.5).color(theme::WEAK));
                }
                ui.label(RichText::new("For").size(11.5).color(theme::WEAK));
                ui.end_row();
                for (r, s) in rc.states.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.radio(active == r, "").on_hover_text("Solve this state in ordinary runs").clicked() {
                            pick = Some(r);
                        }
                        if ui.add_sized(vec2(100.0, 22.0), egui::TextEdit::singleline(&mut s.name)).gained_focus() {
                            *shown = r;
                        }
                        if many && theme::icon_button(ui, icon::TRASH, "Remove this state").clicked() {
                            drop_row = Some(r);
                        }
                    });
                    for (k, e) in elements.iter().enumerate() {
                        setting_cell(ui, &e.device, &mut s.settings[k]);
                    }
                    ui.horizontal(|ui| match &mut s.f_op {
                        Some(f) => {
                            ghz_field(ui, f);
                            if theme::icon_button(ui, icon::X, "Back to wherever it matches best").clicked() {
                                s.f_op = None;
                            }
                        }
                        None => {
                            if ui.small_button("Best match").on_hover_text("Judged where it matches best. Click to give a frequency.").clicked() {
                                s.f_op = Some(f0);
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        });
        if let Some(r) = pick {
            rc.active = r;
            *shown = r;
        }
        if let Some(r) = drop_row {
            rc.states.remove(r);
            rc.active = rc.active.min(rc.states.len().saturating_sub(1));
        }
        let steps = &mut st.ui.reconfig.all_steps;
        let mut problem = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button(theme::label(icon::PLUS, "Add a state")).clicked() {
                let mut next = rc.states.last().cloned().unwrap_or_else(|| rc.blank_state("State"));
                next.name = format!("State {}", rc.states.len() + 1);
                rc.states.push(next);
            }
            if ui.button("Every combination").on_hover_text("Lists every combination of the switches, and of the bias steps set here for each varactor").clicked() {
                match rc.all_combinations(*steps, STATES_LISTED) {
                    Ok(list) => {
                        rc.states = list;
                        rc.active = 0;
                        *shown = 0;
                    }
                    Err(e) => problem = Some(e.to_string()),
                }
            }
            if elements.iter().any(|e| matches!(e.device.model, Model::Varactor { .. })) {
                ui.add(egui::DragValue::new(steps).range(2..=17).suffix(" bias steps"));
            }
        });
        if let Some(m) = problem {
            verdict(ui, false, &m);
        }
    });
}

fn scale_to(x: f64, lo: f64, hi: f64, left: f32, width: f32) -> f32 {
    left + (((x - lo) / (hi - lo).max(1e-30)) as f32).clamp(0.0, 1.0) * width
}

/// Which frequencies each state covers, under each other, with the targets as columns.
fn coverage_map(ui: &mut egui::Ui, r: &Report, targets: &[Target], lo: f64, hi: f64) {
    if r.states.is_empty() {
        return;
    }
    ui.label(RichText::new("What each state covers (under −10 dB)").size(12.5).color(theme::TEXT));
    let row = 15.0;
    let label_w = 112.0;
    let rows = r.states.len().min(24);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row * (rows as f32 + 1.0) + 14.0), Sense::hover());
    let p = ui.painter_at(rect);
    let left = rect.left() + label_w;
    let width = (rect.width() - label_w - 8.0).max(10.0);
    let small = FontId::proportional(10.5);
    for t in targets {
        let (a, b) = (scale_to(t.f_lo, lo, hi, left, width), scale_to(t.f_hi, lo, hi, left, width));
        let band = egui::Rect::from_min_max(pos2(a, rect.top()), pos2(b.max(a + 1.5), rect.top() + row * rows as f32));
        p.rect_filled(band, 0.0, theme::ACCENT.gamma_multiply(0.18));
    }
    for (k, s) in r.states.iter().take(rows).enumerate() {
        let y = rect.top() + row * k as f32;
        let name = p.layout_no_wrap(s.name.clone(), small.clone(), theme::TEXT);
        let clipped = p.with_clip_rect(egui::Rect::from_min_size(pos2(rect.left(), y), vec2(label_w - 6.0, row)));
        clipped.galley(pos2(rect.left(), y + (row - name.size().y) / 2.0), name, theme::TEXT);
        p.hline(left..=left + width, y + row / 2.0, Stroke::new(1.0, theme::STROKE));
        let color = charts::PALETTE[k % charts::PALETTE.len()];
        for b in &s.bands {
            let (a, c) = (scale_to(b.0, lo, hi, left, width), scale_to(b.1, lo, hi, left, width));
            p.rect_filled(egui::Rect::from_min_max(pos2(a, y + 3.0), pos2(c.max(a + 2.0), y + row - 3.0)), 2.0, color);
        }
    }
    let axis_y = rect.top() + row * rows as f32 + 2.0;
    p.text(pos2(left, axis_y), Align2::LEFT_TOP, format!("{:.3} GHz", lo / 1e9), small.clone(), theme::WEAK);
    p.text(pos2(left + width, axis_y), Align2::RIGHT_TOP, format!("{:.3} GHz", hi / 1e9), small, theme::WEAK);
    if let Some(at) = resp.hover_pos()
        && at.x >= left
    {
        let f = lo + ((at.x - left) / width) as f64 * (hi - lo);
        let k = (((at.y - rect.top()) / row) as usize).min(rows - 1);
        let s = &r.states[k];
        let bands: Vec<String> = s.bands.iter().map(|b| format!("{:.3}–{:.3} GHz", b.0 / 1e9, b.1 / 1e9)).collect();
        let line = format!("{}\n{:.3} GHz\n{}", s.name, f / 1e9, if bands.is_empty() { "Nothing under −10 dB".to_string() } else { bands.join(", ") });
        resp.clone().on_hover_ui_at_pointer(|ui| {
            ui.label(line);
        });
    }
}

fn state_table(ui: &mut egui::Ui, st: &mut Studio, r: &Report) {
    let busy = st.busy();
    let mut view = None;
    egui::ScrollArea::horizontal().id_salt("rc_scores").show(ui, |ui| {
        egui::Grid::new("rc_scores_grid").num_columns(9).spacing(vec2(10.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["State", "", "At", "Match", "Gain", "Beam θ, φ", "Polarisation", "Lost in parts", "Bias"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for (k, s) in r.states.iter().enumerate() {
                let color = charts::PALETTE[k % charts::PALETTE.len()];
                if ui.selectable_label(st.ui.reconfig.shown == k, RichText::new(&s.name).size(12.5).color(color)).on_hover_text("Show this state's parts, here and on the canvas").clicked() {
                    st.ui.reconfig.shown = k;
                }
                if ui.add_enabled(!busy, egui::Button::new("View").small()).on_hover_text("Show it on the far-field, S-parameter, current, array and link pages").clicked() {
                    view = Some((k, s.f_op));
                }
                ui.label(format!("{:.3} GHz", s.f_op / 1e9));
                let good = s.s_op_db <= -10.0;
                ui.label(RichText::new(format!("{:.1} dB", s.s_op_db)).color(if good { theme::TEXT } else { theme::WARNING }));
                match &s.metrics {
                    Some(m) => {
                        ui.label(format!("{:.1} dBi", m.peak_realized_dbi));
                        ui.label(format!("{:.0}°, {:.0}°", m.peak_theta, m.peak_phi));
                    }
                    None => {
                        ui.label("–");
                        ui.label("–");
                    }
                }
                ui.label(&s.polarisation);
                ui.label(format!("{:.1} %", s.part_loss * 100.0));
                ui.label(if s.dc_mw > 0.0 { format!("{:.1} mW", s.dc_mw) } else { "none".to_string() });
                ui.end_row();
            }
        });
    });
    if let Some((k, f)) = view {
        st.view_state(k);
        st.ui.reconfig.view_f = Some(f);
        st.ui.page = Page::FarField;
    }
}

fn part_table(ui: &mut egui::Ui, st: &mut Studio, r: &Report) {
    let k = st.ui.reconfig.shown.min(r.states.len().saturating_sub(1));
    let Some(s) = r.states.get(k) else { return };
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("What the parts go through in '{}'", s.name)).size(12.5).color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::DragValue::new(&mut st.project.reconfig.power_dbm).range(-30.0..=60.0).speed(0.5).suffix(" dBm into the feed").max_decimals(1));
        });
    });
    egui::ScrollArea::horizontal().id_salt("rc_parts").show(ui, |ui| {
        egui::Grid::new("rc_parts_grid").num_columns(7).spacing(vec2(10.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["Part", "Setting", "At the gap", "RF volts", "RF current", "Heat", "Reaches a limit at"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for p in &s.parts {
                ui.label(RichText::new(&p.name).size(12.5));
                ui.label(&p.setting);
                ui.label(impedance_text(p.z_load));
                ui.label(format!("{:.2} V", p.v_peak));
                ui.label(format!("{:.1} mA", p.i_peak * 1e3));
                ui.label(format!("{:.1} mW", p.p_device * 1e3));
                match p.max_power_dbm {
                    Some(m) => {
                        let warn = p.over || p.nonlinear;
                        let r = ui.label(RichText::new(format!("{m:.1} dBm")).color(if warn { theme::WARNING } else { theme::TEXT }));
                        if let Some(l) = &p.limit {
                            r.on_hover_text(l.clone());
                        }
                    }
                    None => {
                        ui.label("–");
                    }
                }
                ui.end_row();
            }
        });
    });
    weak(ui, format!("Peak values at {:.3} GHz. Hover over a limit to see what it is.", s.f_op / 1e9));
}

fn impedance_text(z: antenna_core::C64) -> String {
    let m = z.norm();
    if m >= 1e6 {
        "open".to_string()
    } else if m < 1e-3 {
        "short".to_string()
    } else {
        format!("{:.1} {} j{:.1} Ω", z.re, if z.im < 0.0 { "−" } else { "+" }, z.im.abs())
    }
}

fn tuning_charts(ui: &mut egui::Ui, st: &mut Studio, curve: &[TunePoint]) {
    let varactors: Vec<(usize, String)> = st.project.reconfig.elements.iter().enumerate().filter(|(_, e)| matches!(e.device.model, Model::Varactor { .. })).map(|(i, e)| (i, e.name.clone())).collect();
    if varactors.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Tuning with bias").size(12.5).color(theme::TEXT));
        if varactors.len() > 1 {
            let which = &mut st.ui.reconfig.tune_which;
            let text = which.and_then(|w| varactors.iter().find(|v| v.0 == w)).map_or("All together".to_string(), |v| v.1.clone());
            egui::ComboBox::from_id_salt("rc_tune").selected_text(text).width(120.0).show_ui(ui, |ui| {
                ui.selectable_value(which, None, "All together");
                for (i, n) in &varactors {
                    ui.selectable_value(which, Some(*i), n);
                }
            });
        }
    });
    let f: Vec<(f64, f64)> = curve.iter().filter_map(|c| c.f_res.map(|x| (c.volts, x / 1e9))).collect();
    let lo: Vec<(f64, f64)> = curve.iter().filter_map(|c| c.band.map(|b| (c.volts, b.0 / 1e9))).collect();
    let hi: Vec<(f64, f64)> = curve.iter().filter_map(|c| c.band.map(|b| (c.volts, b.1 / 1e9))).collect();
    charts::line_chart(ui, ChartOptions { title: "Resonance against reverse bias, and its −10 dB band", unit: " GHz", x_unit: "V", height: 170.0, ..Default::default() }, &[Series { name: "Resonance", color: charts::BLUE, points: &f }, Series { name: "Band edge", color: charts::GREY, points: &lo }, Series { name: "Band edge", color: charts::GREY, points: &hi }]);
    let e: Vec<(f64, f64)> = curve.iter().filter(|c| c.part_efficiency.is_finite()).map(|c| (c.volts, c.part_efficiency * 100.0)).collect();
    charts::line_chart(ui, ChartOptions { title: "Accepted power not lost in the parts, against bias", unit: " %", x_unit: "V", height: 120.0, y_range: Some((0.0, 100.0)), ..Default::default() }, &[Series { name: "Passes the parts", color: charts::GREEN, points: &e }]);
    if let (Some(a), Some(b)) = (f.iter().map(|v| v.1).reduce(f64::min), f.iter().map(|v| v.1).reduce(f64::max)) {
        weak(ui, format!("Tunes {a:.3}–{b:.3} GHz, a span of {:.0} %. Low bias gives the most capacitance, so the most current and the most loss.", 200.0 * (b - a) / (b + a)));
    }
}

fn targets_group(ui: &mut egui::Ui, st: &mut Studio, report: Option<&Report>, run: Option<&ReconfigRun>) {
    let f0 = st.project.f0();
    group(ui, |ui| {
        theme::caption(ui, "Targets");
        weak(ui, "What the antenna has to do in some state. Leave the beam off to judge gain and polarisation at the pattern's peak.");
        let mut remove = None;
        for (k, t) in st.project.reconfig.targets.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut t.name).desired_width(128.0));
                ghz_field(ui, &mut t.f_lo);
                ui.label("–");
                ghz_field(ui, &mut t.f_hi);
                if theme::icon_button(ui, icon::TRASH, "Remove this target").clicked() {
                    remove = Some(k);
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut t.limit_db).range(-60.0..=0.0).speed(0.5).prefix("match ").suffix(" dB").max_decimals(1));
                let mut gain = t.min_gain_dbi.is_some();
                if ui.checkbox(&mut gain, "gain").changed() {
                    t.min_gain_dbi = gain.then_some(3.0);
                }
                if let Some(g) = t.min_gain_dbi.as_mut() {
                    ui.add(egui::DragValue::new(g).range(-40.0..=40.0).speed(0.1).suffix(" dBi").max_decimals(1));
                }
            });
            ui.horizontal(|ui| {
                let mut beam = t.beam.is_some();
                if ui.checkbox(&mut beam, "beam").changed() {
                    t.beam = beam.then_some([0.0, 0.0]);
                }
                if let Some(b) = t.beam.as_mut() {
                    ui.add(egui::DragValue::new(&mut b[0]).range(0.0..=180.0).prefix("θ ").suffix("°").max_decimals(0));
                    ui.add(egui::DragValue::new(&mut b[1]).range(0.0..=360.0).prefix("φ ").suffix("°").max_decimals(0));
                }
                let word = t.pol.map_or("any polarisation", PolGoal::label);
                egui::ComboBox::from_id_salt(("rc_pol", k)).selected_text(word).width(110.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut t.pol, None, "any polarisation");
                    for p in PolGoal::ALL {
                        ui.selectable_value(&mut t.pol, Some(p), p.label());
                    }
                });
            });
            if let Some(list) = report.and_then(|r| r.met_by.get(k)) {
                let names: Vec<String> = list.1.iter().filter_map(|i| st.project.reconfig.states.get(*i)).map(|s| s.name.clone()).collect();
                if names.is_empty() {
                    ui.label(RichText::new(format!("{} No state meets this", icon::WARNING_CIRCLE)).size(12.0).color(theme::WARNING));
                } else {
                    ui.label(RichText::new(format!("{} Met by {}", icon::CHECK_CIRCLE, names.join(", "))).size(12.0).color(theme::SUCCESS));
                }
            }
        }
        if let Some(k) = remove {
            st.project.reconfig.targets.remove(k);
            st.ui.reconfig.found = None;
        }
        if ui.button(theme::label(icon::PLUS, "Add a target")).clicked() {
            let n = st.project.reconfig.targets.len() + 1;
            st.project.reconfig.targets.push(Target::band(&format!("Target {n}"), f0 * 0.99, f0 * 1.01));
            st.ui.reconfig.found = None;
        }
        let Some(run) = run else { return };
        if st.project.reconfig.targets.is_empty() {
            return;
        }
        theme::hairline(ui);
        theme::caption(ui, "Find the settings");
        weak(ui, "Tries the combinations on this solve, without a new one, and picks the best for each target.");
        ui.horizontal(|ui| {
            if st.project.reconfig.elements.iter().any(|e| matches!(e.device.model, Model::Varactor { .. })) {
                ui.add(egui::DragValue::new(&mut st.ui.reconfig.search_steps).range(2..=41).suffix(" bias steps"));
            }
            if ui.add_enabled(!st.busy(), egui::Button::new(theme::label(icon::SPARKLE, "Search"))).clicked() {
                let steps = st.ui.reconfig.search_steps;
                st.search_reconfig(steps);
            }
        });
        if let Some(found) = st.ui.reconfig.found.clone() {
            found_table(ui, st, &found);
        }
        spread_panel(ui, st, run, report);
    });
}

fn found_table(ui: &mut egui::Ui, st: &mut Studio, found: &[Found]) {
    let same_length = found.iter().all(|f| f.settings.len() == st.project.reconfig.elements.len());
    if !same_length {
        return;
    }
    egui::ScrollArea::horizontal().id_salt("rc_found").show(ui, |ui| {
        egui::Grid::new("rc_found_grid").num_columns(4).spacing(vec2(10.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["Target", "Settings", "Worst match", "Gain"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for f in found {
                let good = f.score.met;
                ui.label(RichText::new(&f.target).size(12.5).color(if good { theme::SUCCESS } else { theme::WARNING }));
                ui.label(RichText::new(st.project.reconfig.state_name(&f.settings)).size(12.5));
                ui.label(format!("{:.1} dB", f.score.worst_s_db));
                ui.label(f.score.gain_dbi.map_or("–".to_string(), |g| format!("{g:.1} dBi")));
                ui.end_row();
            }
        });
    });
    let tried = found.first().map_or(0, |f| f.tried);
    let how = if found.iter().all(|f| f.exhaustive) { format!("Every one of {tried} combinations tried.") } else { format!("Too many combinations to try them all: this is the best of {tried} searches, and may not be the very best.") };
    let missed: Vec<&str> = found.iter().filter(|f| !f.score.met).map(|f| f.target.as_str()).collect();
    if missed.is_empty() {
        verdict(ui, true, &format!("A state meets every target. {how}"));
    } else {
        verdict(ui, false, &format!("Nothing here meets {}, which is the closest shown. Part values won't fix that alone: the metal has to change, which the tuner below can try. {how}", missed.join(", ")));
    }
    if ui.button("Use these as the states").on_hover_text("Replaces the states above with one per target").clicked() {
        let states: Vec<TuneState> = states_from_found(found, &st.project.reconfig.targets);
        st.project.reconfig.states = states;
        st.project.reconfig.active = 0;
        st.ui.reconfig.shown = 0;
    }
}

fn spread_panel(ui: &mut egui::Ui, st: &mut Studio, run: &ReconfigRun, report: Option<&Report>) {
    theme::hairline(ui);
    theme::caption(ui, "When parts differ");
    weak(ui, "How often a build still meets each target, in the first state that does, when every part's values wander. Parts only: the metal stays as drawn.");
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut st.ui.reconfig.spread).range(0.5..=50.0).speed(0.5).prefix("± ").suffix(" %, 1σ").max_decimals(1));
        if ui.button("Check").clicked() {
            let rc = st.project.reconfig.clone();
            let spread = st.ui.reconfig.spread / 100.0;
            let mut out = Vec::new();
            if let (Ok(solved), Some(rep)) = (run.solved(&rc), report) {
                for (t, (_, met)) in rc.targets.iter().zip(&rep.met_by) {
                    let k = met.first().copied().unwrap_or(rc.active);
                    if let Some(state) = rc.states.get(k) {
                        out.push((format!("{} in '{}'", t.name, state.name), solved.robust(&rc, state, t, spread, 11)));
                    }
                }
            }
            st.ui.reconfig.spread_out = out;
        }
    });
    for (what, r) in &st.ui.reconfig.spread_out {
        let chance = r.pass_probability.map_or("–".to_string(), |p| format!("{:.0} %", p * 100.0));
        let driver = r.main_driver().map_or(String::new(), |d| format!(" {} moves it most.", d.factor));
        weak(ui, format!("{what}: {chance} of builds pass; worst match {:.1} to {:.1} dB. {}: assumed spread, not measured.{driver}", r.prediction.low, r.prediction.high, r.prediction.evidence.label()));
    }
}

/// Tuning the metal for all the targets at once, with the settings searched at every step.
fn tune_panel(ui: &mut egui::Ui, st: &mut Studio) {
    egui::CollapsingHeader::new(RichText::new("Tune the dimensions for every target").size(12.5)).id_salt("rc_tune_metal").show(ui, |ui| {
        weak(ui, "Each step is a full solve of all the ports, then a search of the settings. Pick few variables, and a fast mesh.");
        let vars = st.project.vars().unwrap_or_default();
        let names: Vec<String> = st.project.params.iter().filter(|p| !p.derived && p.name != "f0" && p.name != "Z0").map(|p| p.name.clone()).collect();
        st.ui.opt_vars.retain(|v| names.contains(&v.0));
        for n in &names {
            if !st.ui.opt_vars.iter().any(|v| &v.0 == n) {
                let c = vars.get(n).copied().unwrap_or(0.0);
                st.ui.opt_vars.push((n.clone(), c * 0.8, c * 1.2, false));
            }
        }
        egui::Grid::new("rc_optvars").num_columns(3).spacing(vec2(10.0, 3.0)).striped(true).show(ui, |ui| {
            for v in st.ui.opt_vars.iter_mut() {
                ui.checkbox(&mut v.3, &v.0);
                let c = vars.get(&v.0).copied().unwrap_or(0.0);
                ui.label(RichText::new(antenna_core::report::param_text(&v.0, c)).size(12.0).color(theme::WEAK));
                let sp = c.abs().max(1e-6) * 0.005;
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut v.1).speed(sp).custom_formatter(|x, _| antenna_core::report::param_text(&v.0, x)));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut v.2).speed(sp).custom_formatter(|x, _| antenna_core::report::param_text(&v.0, x)));
                });
                ui.end_row();
            }
        });
        let chosen: Vec<(String, f64, f64)> = st.ui.opt_vars.iter().filter(|v| v.3).map(|v| (v.0.clone(), v.1.min(v.2), v.1.max(v.2))).collect();
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut st.ui.reconfig.tune_runs).range(4..=2000).suffix(" solves"));
            let ok = !chosen.is_empty() && !st.project.reconfig.targets.is_empty() && !st.busy();
            if ui.add_enabled(ok, egui::Button::new(theme::label(icon::ROCKET_LAUNCH, "Tune"))).on_disabled_hover_text("Needs a target and a variable, and no job running").clicked() {
                let (method, runs, parallel) = (st.ui.method, st.ui.reconfig.tune_runs, st.ui.parallel);
                st.run_reconfig_optimize(chosen, method, runs, parallel);
            }
        });
        if let Some(o) = st.opt().filter(|o| !o.best_x.is_empty() && o.names.iter().all(|n| names.contains(n))) {
            let values: Vec<String> = o.names.iter().zip(&o.best_x).map(|(n, x)| format!("{n} = {}", antenna_core::report::param_text(n, *x))).collect();
            weak(ui, format!("Best so far, missing the targets by {:.1} dB after {} solves: {}.", o.best_value, o.evals, values.join(", ")));
            if ui.add_enabled(!st.busy(), egui::Button::new("Use these values, and solve")).clicked() {
                let pairs: Vec<(String, f64)> = o.names.iter().cloned().zip(o.best_x.iter().copied()).collect();
                st.project = st.project.with_values(&pairs);
                st.run_reconfig();
            }
        }
    });
}

fn exports(ui: &mut egui::Ui, st: &mut Studio, run: &ReconfigRun, report: &Report) {
    let rc = st.project.reconfig.clone();
    let name = slug(&st.project.name);
    ui.horizontal_wrapped(|ui| {
        if ui.button(theme::label(icon::EXPORT, "State table (CSV)")).on_hover_text("The bias truth table for whoever writes the firmware, and every state's figures").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name(format!("{name}_states.csv")).save_file()
        {
            st.ui.message = Some(match std::fs::write(&path, reconfig::truth_table_csv(&rc, report)) {
                Ok(()) => (format!("Wrote {}", path.display()), true),
                Err(e) => (format!("Could not write it: {e}"), false),
            });
        }
        if ui.button(theme::label(icon::EXPORT, "Touchstone per state")).on_hover_text("One S-parameter file per state, for a circuit simulator").clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            st.ui.message = Some(match write_touchstones(&dir, &name, &rc, run) {
                Ok(n) => (format!("Wrote {n} files to {}", dir.display()), true),
                Err(e) => (format!("Could not write them: {e:#}"), false),
            });
        }
        if ui.button(theme::label(icon::EXPORT, "Report (HTML)")).clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("HTML", &["html"]).set_file_name(format!("{name}_reconfigurable.html")).save_file()
        {
            let html = antenna_core::report::reconfig_html(&st.project, report, &reconfig::assumptions(&rc, &run.basis));
            st.ui.message = Some(match std::fs::write(&path, html) {
                Ok(()) => (format!("Wrote {}", path.display()), true),
                Err(e) => (format!("Could not write it: {e}"), false),
            });
        }
    });
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn write_touchstones(dir: &std::path::Path, name: &str, rc: &Reconfig, run: &ReconfigRun) -> anyhow::Result<usize> {
    let solved = run.solved(rc)?;
    for (k, s) in rc.states.iter().enumerate() {
        let net = solved.network(rc, &rc.settings_of(s))?;
        let file = format!("{name}_{}_{}.s{}p", k + 1, slug(&s.name), net.ports());
        std::fs::write(dir.join(file), net.to_touchstone())?;
    }
    Ok(rc.states.len())
}

fn solve_text(st: &Studio, geometry: &Geometry) -> String {
    let n = geometry.ports.len();
    if st.ui.chosen == antenna_core::project::SolverKind::Fdtd {
        let time = st.ui.plan.as_ref().map_or(String::new(), |p| format!(", about {}", duration(p.estimate_s(st.speed_mcs()) * n as f64)));
        format!("FDTD, one run per port: {n} runs{time}. That one solve answers for every state.")
    } else {
        format!("{}: one sweep with all {n} ports, usually seconds. That one solve answers for every state.", st.ui.chosen.label())
    }
}

fn starters(ui: &mut egui::Ui, st: &mut Studio) {
    group(ui, |ui| {
        theme::caption(ui, "Start from a design");
        let list: Vec<(&str, &str, &str)> = antenna_core::templates::all().iter().filter(|t| t.category == antenna_core::templates::Category::Reconfigurable).map(|t| (t.id, t.title, t.blurb)).collect();
        for (id, title, blurb) in list {
            if ui.button(title).on_hover_text(blurb).clicked()
                && let Ok(p) = Project::from_template(id, None)
            {
                st.project = p;
                st.ui.reconfig = ReconfigUi::default();
            }
            weak(ui, blurb);
        }
    });
}

pub fn page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Reconfigurable antenna");
    weak(ui, "Switches and varactors across gaps change what the metal does: the band, the beam or the polarisation. Each gap is a port, so one solve answers for every state, and changing a part or a state is instant.");
    if let Some(found) = st.take_reconfig_found() {
        st.ui.reconfig.found = Some(found);
    }
    let geometry = st.ui.geometry.clone();
    if let Err(e) = &geometry
        && !st.project.reconfig.is_on()
    {
        verdict(ui, false, e);
    }
    let g = geometry.as_ref().ok();
    if !st.project.reconfig.is_on() {
        weak(ui, "Put a part across a gap in this model, or start from one of these.");
        parts_group(ui, st, g);
        starters(ui, st);
        return;
    }
    parts_group(ui, st, g);
    let Some(g) = g else {
        if let Err(e) = &geometry {
            verdict(ui, false, e);
        }
        return;
    };
    let names: Vec<String> = g.ports.iter().map(|p| p.name.clone()).collect();
    if let Err(e) = st.project.reconfig.bind(&names) {
        verdict(ui, false, &format!("{e:#}"));
        return;
    }
    if st.project.reconfig.states.is_empty() {
        st.project.reconfig.states = st.project.reconfig.default_states();
    }
    st.ui.reconfig.shown = st.ui.reconfig.shown.min(st.project.reconfig.states.len().saturating_sub(1));
    states_group(ui, st);
    weak(ui, solve_text(st, g));
    let run = st.reconfig_run().filter(|r| r.applies_to(g, &st.project.setup));
    let stale = run.is_none() && st.reconfig_run().is_some();
    let label = if run.is_some() { format!("Solve the {} ports again", names.len()) } else { format!("Solve the {} ports", names.len()) };
    if run_button(ui, &theme::label(icon::PLAY, &label), !st.busy() && st.ui.setup_problem.is_none()) {
        st.ui.message = None;
        st.ui.reconfig.found = None;
        st.ui.reconfig.spread_out.clear();
        st.run_reconfig();
    }
    if let Some(p) = &st.ui.setup_problem {
        verdict(ui, false, p);
    }
    if stale {
        verdict(ui, false, "The metal or the solver setup changed since the last solve. Solve again for results.");
    }
    let Some(run) = run else {
        targets_group(ui, st, None, None);
        return;
    };
    let id = std::sync::Arc::as_ptr(&run.basis) as usize;
    let which = st.ui.reconfig.tune_which;
    let same = st.ui.reconfig.cached.as_ref().is_some_and(|c| c.basis == id && c.rc == st.project.reconfig && c.tune_which == which);
    if !same {
        let rc = st.project.reconfig.clone();
        let (report, curve) = match run.solved(&rc) {
            Ok(solved) => {
                let base = rc.active_state().cloned().unwrap_or_else(|| rc.blank_state("base"));
                let curve = if rc.elements.iter().any(|e| matches!(e.device.model, Model::Varactor { .. })) { solved.tuning(&rc, &base, which, 25) } else { Vec::new() };
                (solved.report(&rc).map_err(|e| format!("{e:#}")), curve)
            }
            Err(e) => (Err(format!("{e:#}")), Vec::new()),
        };
        st.ui.reconfig.cached = Some(Cached { basis: id, rc, tune_which: which, report, curve });
    }
    let Some(cached) = st.ui.reconfig.cached.take() else { return };
    results(ui, st, &run, &cached);
    st.ui.reconfig.cached = Some(cached);
}

fn results(ui: &mut egui::Ui, st: &mut Studio, run: &ReconfigRun, cached: &Cached) {
    let report = match &cached.report {
        Ok(r) => r,
        Err(e) => {
            verdict(ui, false, e);
            return;
        }
    };
    let (lo, hi) = (st.project.setup.f_lo, st.project.setup.f_hi);
    let span = report.tuning.map_or("–".to_string(), |t| format!("{:.3}–{:.3} GHz", t.0 / 1e9, t.1 / 1e9));
    let share = report.tuning.map_or(String::new(), |t| format!("{:.0} %", 200.0 * (t.1 - t.0) / (t.1 + t.0)));
    let covered: f64 = report.coverage.iter().map(|b| b.1 - b.0).sum();
    let worst_loss = report.states.iter().map(|s| s.part_loss).fold(0.0, f64::max);
    let most_bias = report.states.iter().map(|s| s.dc_mw).fold(0.0, f64::max);
    let mut pols: Vec<String> = report.states.iter().map(|s| s.polarisation.split(" (").next().unwrap_or("").to_string()).collect();
    pols.sort();
    pols.dedup();
    let met = report.met_by.iter().filter(|m| !m.1.is_empty()).count();
    let targets = report.met_by.len();
    let good_or = |ok: bool| if ok { theme::SUCCESS } else { theme::WARNING };
    tiles(
        ui,
        &[
            (span, &format!("resonances, {share} span"), theme::TEXT),
            (format!("{:.0} MHz", covered / 1e6), "covered under −10 dB", theme::TEXT),
            (format!("{:.0}°", report.beam_span_deg), "beam moves across", theme::TEXT),
            (pols.join(", "), "polarisations", theme::TEXT),
            (format!("{:.1} dB", 0.0 - antenna_core::db10((1.0 - worst_loss).max(1e-6))), "lost in parts, at worst", theme::TEXT),
            (if targets > 0 { format!("{met} of {targets}") } else { format!("{most_bias:.1} mW") }, if targets > 0 { "targets met" } else { "bias, at worst" }, if targets > 0 { good_or(met == targets) } else { theme::TEXT }),
        ],
    );
    weak(ui, format!("{}, from {}, with typical part values. Not measured.", proof::Evidence::Simulated.label(), run.basis.solver));
    for w in &report.warnings {
        verdict(ui, false, w);
    }
    state_table(ui, st, report);
    let curves: Vec<Vec<(f64, f64)>> = report.states.iter().map(|s| s.curve.iter().map(|c| (c.0 / 1e9, c.1)).collect()).collect();
    let env: Vec<(f64, f64)> = report.envelope.iter().map(|c| (c.0 / 1e9, c.1)).collect();
    let mut series: Vec<Series> = report.states.iter().zip(&curves).enumerate().map(|(k, (s, c))| Series { name: s.name.as_str(), color: charts::PALETTE[k % charts::PALETTE.len()], points: c }).collect();
    if report.states.len() > 8 {
        series.truncate(0);
        series.push(Series { name: "Best of all states", color: charts::BLUE, points: &env });
    }
    charts::line_chart(ui, ChartOptions { title: "Reflection at the feed", unit: " dB", x_unit: "GHz", height: 210.0, limit: Some(-10.0), y_range: Some((-40.0, 0.5)), ..Default::default() }, &series);
    coverage_map(ui, report, &st.project.reconfig.targets.clone(), lo, hi);
    part_table(ui, st, report);
    tuning_charts(ui, st, &cached.curve);
    targets_group(ui, st, Some(report), Some(run));
    tune_panel(ui, st);
    group(ui, |ui| {
        theme::caption(ui, "Hand it on");
        exports(ui, st, run, report);
    });
    egui::CollapsingHeader::new("What this assumes").id_salt("rc_assumes").show(ui, |ui| {
        for a in reconfig::assumptions(&st.project.reconfig, &run.basis) {
            weak(ui, format!("• {a}"));
        }
    });
}
