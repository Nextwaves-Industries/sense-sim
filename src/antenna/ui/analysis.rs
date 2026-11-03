//! Analysis pages that need no new solve (array, matching, MIMO) and the
//! outputs: project files, Touchstone, far-field tables, report, CAD files, and
//! handing the pattern to the positioning planner.

use antenna_core::array::{self, Lattice, Taper};
use antenna_core::farfield::{Pol, Quantity};
use antenna_core::matching::{self, Kind, Network as MatchNet, Series as ESeries};
use antenna_core::project::{Project, SimResult};
use antenna_core::recipes::gate;
use antenna_core::report;
use antenna_core::{C0, C64, sparam};
use bevy_egui::egui::{self, vec2};

use super::charts::{self, ChartOptions, Series};
use super::{Page, ghz_field, group, tiles, verdict, weak};
use crate::antenna::Studio;
use crate::project_file;
use crate::rtls::antenna::AntennaLibrary;
use crate::rtls::planning::{AppliedAntenna, FdtdAntenna, Tech};
use crate::theme::{self, icon};

/// The report for a result, written from the project that made it: the one being edited
/// may have moved on since, or be showing an earlier run.
fn report_page(st: &Studio, r: &std::sync::Arc<SimResult>) -> String {
    let history = st.history();
    let source = history.iter().rev().find(|(_, x)| std::sync::Arc::ptr_eq(x, r)).map(|(p, _)| p);
    let mut project = source.cloned().unwrap_or_else(|| st.project.clone());
    // A rename since the run is still this design.
    if project.template == st.project.template {
        project.name = st.project.name.clone();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let link = &st.ui.link;
    // A study's results are only about this result if this result came out of it.
    let mesh_study = st.converge().filter(|c| c.steps.iter().any(|s| std::sync::Arc::ptr_eq(&s.result, r))).map_or_else(Vec::new, |c| {
        c.steps.iter().map(|s| report::MeshStep { density: s.density, size: s.size, seconds: s.seconds, f_res: s.f_res, peak_dbi: s.peak_dbi, delta_s: s.delta_s, gain_change_db: s.gain_change_db }).collect()
    });
    let gate = st.gate_run().and_then(|run| {
        let spec = st.ui.gate.spec.unwrap_or(run.spec);
        if !run.applies_to(&project, &spec) {
            return None;
        }
        let report = gate::evaluate(&run.field, &spec, &project.states);
        let (read, stray) = gate::predict(&report, &spec, 300);
        Some(report::GateSection { read_range: (read.prediction.low, read.prediction.high), stray_range: (stray.prediction.low, stray.prediction.high), evidence: read.prediction.evidence.label().to_string(), assumptions: gate::assumptions(&run.field, &spec), report, spec })
    });
    let extras = report::Extras {
        stamp: format!("Exported {} from {} {}", report::utc_stamp(now), crate::APP_NAME, env!("CARGO_PKG_VERSION")),
        link: Some(report::LinkInputs { tx_dbm: link.tx_dbm, rx_gain_dbi: link.rx_gain, sensitivity_dbm: link.sens_dbm, margin_db: link.margin_db }),
        regions: st.ui.rfid_limits.clone(),
        mesh_study,
        gate,
    };
    report::html_with(&project, r, &extras)
}

fn need_result(ui: &mut egui::Ui, st: &mut Studio) -> bool {
    if st.latest().is_some() {
        return false;
    }
    theme::empty_state(ui, icon::CHART_LINE, "No results yet", "Run a simulation on the Solver setup page.");
    if ui.button("Go to the solver setup").clicked() {
        st.ui.page = Page::Setup;
    }
    true
}

pub(crate) fn taper_ui(ui: &mut egui::Ui, id: &str, t: &mut Taper) {
    let label = |t: &Taper| match t {
        Taper::Uniform => "Uniform".to_string(),
        Taper::Binomial => "Binomial".to_string(),
        Taper::DolphChebyshev { .. } => "Dolph-Chebyshev".to_string(),
        Taper::Taylor { .. } => "Taylor".to_string(),
        Taper::Cosine { .. } => "Cosine".to_string(),
        Taper::Hamming => "Hamming".to_string(),
        Taper::Blackman => "Blackman".to_string(),
    };
    egui::ComboBox::from_id_salt(id).selected_text(label(t)).show_ui(ui, |ui| {
        for opt in [Taper::Uniform, Taper::Binomial, Taper::DolphChebyshev { sll_db: -30.0 }, Taper::Taylor { sll_db: -30.0, nbar: 5 }, Taper::Cosine { power: 1.0 }, Taper::Hamming, Taper::Blackman] {
            let on = std::mem::discriminant(t) == std::mem::discriminant(&opt);
            if ui.selectable_label(on, label(&opt)).clicked() && !on {
                *t = opt;
            }
        }
    });
    match t {
        Taper::DolphChebyshev { sll_db } | Taper::Taylor { sll_db, .. } => {
            ui.add(egui::DragValue::new(sll_db).range(-80.0..=-13.0).suffix(" dB"));
        }
        Taper::Cosine { power } => {
            ui.add(egui::DragValue::new(power).range(0.1..=4.0).speed(0.05).prefix("power "));
        }
        _ => {}
    }
}

pub fn array(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    let Some(element) = r.far.first().and_then(|p| p.get(st.ui.far_k.min(r.far_f.len().saturating_sub(1)))).cloned() else {
        weak(ui, "This run has no far field to build the array from.");
        return;
    };
    theme::caption(ui, "Array synthesis");
    weak(ui, "Multiplies the simulated element pattern by the array factor, as CST's farfield array and HFSS's array setup do. Coupling between elements is not included: for that, simulate a small array with several ports.");
    let lambda = C0 / element.f;
    let cfg = &mut st.ui.array;
    let spacing = &mut st.ui.array_spacing_lambda;
    egui::Grid::new("array_cfg").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Elements x × y");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut cfg.nx).range(1..=256));
            ui.label("×");
            ui.add(egui::DragValue::new(&mut cfg.ny).range(1..=256));
        });
        ui.end_row();
        ui.label("Spacing x, y");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut spacing.0).range(0.05..=5.0).speed(0.005).suffix(" λ"));
            ui.add(egui::DragValue::new(&mut spacing.1).range(0.05..=5.0).speed(0.005).suffix(" λ"));
        });
        ui.end_row();
        ui.label("Lattice");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut cfg.lattice, Lattice::Rectangular, "Rectangular");
            ui.selectable_value(&mut cfg.lattice, Lattice::Triangular, "Triangular");
        });
        ui.end_row();
        ui.label("Taper x");
        ui.horizontal(|ui| taper_ui(ui, "tap_x", &mut cfg.taper_x));
        ui.end_row();
        ui.label("Taper y");
        ui.horizontal(|ui| taper_ui(ui, "tap_y", &mut cfg.taper_y));
        ui.end_row();
        ui.label("Steer θ, φ");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut cfg.steer_theta).range(0.0..=90.0).suffix("°"));
            ui.add(egui::DragValue::new(&mut cfg.steer_phi).range(-180.0..=360.0).suffix("°"));
        });
        ui.end_row();
        ui.label("Phase shifters");
        ui.horizontal(|ui| {
            let mut on = cfg.phase_bits.is_some();
            if ui.checkbox(&mut on, "quantised").changed() {
                cfg.phase_bits = on.then_some(4);
            }
            if let Some(b) = cfg.phase_bits.as_mut() {
                ui.add(egui::DragValue::new(b).range(1..=10).suffix(" bits"));
            }
        });
        ui.end_row();
        ui.label("Random errors");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut cfg.amp_err_db).range(0.0..=6.0).speed(0.05).suffix(" dB"));
            ui.add(egui::DragValue::new(&mut cfg.phase_err_deg).range(0.0..=45.0).speed(0.5).suffix("°"));
        });
        ui.end_row();
    });
    cfg.dx = spacing.0 * lambda;
    cfg.dy = spacing.1 * lambda;
    let view = super::array_far(&mut st.ui, &r, &element);
    let (s, g) = (view.summary, view.gain);
    tiles(
        ui,
        &[
            (format!("{:.2} dBi", g.realized_dbi), "realized gain", theme::SUCCESS),
            (format!("{:.2} dBi", g.gain_dbi), "gain", theme::TEXT),
            (format!("{:.2} dBi", g.directivity_dbi), "directivity", theme::TEXT),
            (format!("{:.2} dBi", s.directivity_dbi), "array factor alone", theme::TEXT),
            (format!("{:.2} dB", s.taper_loss_db), "taper loss", theme::TEXT),
            (if s.hpbw_x_deg >= 359.0 { "–".to_string() } else { format!("{:.1}°", s.hpbw_x_deg) }, "beam width x", theme::TEXT),
            (if s.hpbw_y_deg >= 359.0 { "–".to_string() } else { format!("{:.1}°", s.hpbw_y_deg) }, "beam width y", theme::TEXT),
            (if s.sll_db.is_finite() { format!("{:.1} dB", s.sll_db) } else { "–".to_string() }, "side lobes", theme::TEXT),
        ],
    );
    match s.grating_lobe {
        Some((t, p)) => verdict(ui, false, &format!("A grating lobe enters visible space near θ {t:.0}°, φ {p:.0}°. Closer elements or less scan would remove it (no grating lobe up to {:.0}° scan at this spacing).", s.max_scan_no_grating_deg)),
        None => verdict(ui, true, &format!("No grating lobes. This spacing scans to {:.0}° before one appears.", s.max_scan_no_grating_deg)),
    }
    if g.close_spacing {
        verdict(ui, false, "Elements closer than λ/2 couple strongly, and this page doesn't model that. Power is still conserved, so gain can't pass directivity, but real impedance and gain will differ: simulate a few elements with several ports.");
    }
    if g.estimated {
        verdict(ui, false, "Too many elements for a full power integral: directivity and gain are estimated from the main lobes, good to about 0.3 dB.");
    }
    let peak = g.directivity_dbi;
    let (cut_a, cut_b) = (&view.cuts[0], &view.cuts[1]);
    charts::line_chart(
        ui,
        ChartOptions { title: "Pattern cuts", unit: " dBi", x_unit: "θ°", height: 190.0, y_range: Some((peak - 50.0, peak + 1.0)), ..Default::default() },
        &[Series { name: "φ = 0°", color: charts::ORANGE, points: cut_a }, Series { name: "φ = 90°", color: charts::BLUE, points: cut_b }],
    );
    // Weights.
    if st.ui.array.nx > 1 {
        let wx: Vec<(f64, f64)> = array::taper_weights(st.ui.array.taper_x, st.ui.array.nx).iter().enumerate().map(|(i, v)| ((i + 1) as f64, *v)).collect();
        charts::line_chart(ui, ChartOptions { title: "Amplitude across x", x_unit: "element", height: 90.0, y_range: Some((0.0, 1.05)), ..Default::default() }, &[Series { name: "Weight", color: charts::GREEN, points: &wx }]);
    }
}

fn kind_text(e: &matching::Element) -> String {
    let nh = e.value * 1e9;
    let pf = e.value * 1e12;
    match e.kind {
        Kind::SeriesL => format!("Series L {nh:.2} nH"),
        Kind::SeriesC => format!("Series C {pf:.2} pF"),
        Kind::ShuntL => format!("Shunt L {nh:.2} nH"),
        Kind::ShuntC => format!("Shunt C {pf:.2} pF"),
        Kind::Line { z0, deg, .. } => format!("Line {z0:.0} Ω, {deg:.1}°"),
        Kind::Stub { z0, deg, shorted, .. } => format!("{} stub {z0:.0} Ω, {deg:.1}°", if shorted { "Shorted" } else { "Open" }),
    }
}

/// A one-line schematic of the network, source on the left.
fn schematic(ui: &mut egui::Ui, net: &MatchNet) {
    let n = net.elements.len().max(1);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let y = rect.top() + 18.0;
    let (x0, x1) = (rect.left() + 34.0, rect.right() - 34.0);
    let stroke = egui::Stroke::new(1.4, theme::TEXT);
    p.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
    p.text(egui::pos2(rect.left() + 6.0, y), egui::Align2::LEFT_CENTER, "Src", egui::FontId::proportional(11.0), theme::WEAK);
    p.text(egui::pos2(rect.right() - 6.0, y), egui::Align2::RIGHT_CENTER, "Ant", egui::FontId::proportional(11.0), theme::WEAK);
    for (i, e) in net.elements.iter().enumerate() {
        let x = x0 + (x1 - x0) * (i as f32 + 0.5) / n as f32;
        let (series, glyph) = match e.kind {
            Kind::SeriesL => (true, "L"),
            Kind::SeriesC => (true, "C"),
            Kind::ShuntL => (false, "L"),
            Kind::ShuntC => (false, "C"),
            Kind::Line { .. } => (true, "TL"),
            Kind::Stub { .. } => (false, "ST"),
        };
        if series {
            let r = egui::Rect::from_center_size(egui::pos2(x, y), vec2(26.0, 14.0));
            p.rect(r, 2.0, theme::ACCENT_SOFT, stroke, egui::StrokeKind::Inside);
            p.text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(10.5), theme::TEXT);
        } else {
            p.circle_filled(egui::pos2(x, y), 2.5, theme::TEXT);
            p.line_segment([egui::pos2(x, y), egui::pos2(x, y + 24.0)], stroke);
            let r = egui::Rect::from_center_size(egui::pos2(x, y + 14.0), vec2(14.0, 18.0));
            p.rect(r, 2.0, theme::ACCENT_SOFT, stroke, egui::StrokeKind::Inside);
            p.text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(9.5), theme::TEXT);
            p.line_segment([egui::pos2(x - 6.0, y + 25.0), egui::pos2(x + 6.0, y + 25.0)], stroke);
        }
        p.text(egui::pos2(x, rect.bottom() - 7.0), egui::Align2::CENTER_CENTER, kind_text(e), egui::FontId::proportional(10.5), theme::WEAK);
    }
}

pub fn matching_page(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    let net = &r.network;
    if net.ports() == 0 {
        return;
    }
    theme::caption(ui, "Matching network");
    weak(ui, "Lumped or line networks that bring the simulated port 1 impedance to the source. Computed from the result, so it's instant.");
    let z0 = st.ui.match_z0;
    if st.ui.match_f <= 0.0 || st.ui.match_f < net.f[0] || st.ui.match_f > net.f[net.f.len() - 1] {
        st.ui.match_f = st.project.f0().clamp(net.f[0], net.f[net.f.len() - 1]);
    }
    let f = st.ui.match_f;
    let zl_all = net.zin(0);
    let k = net.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map_or(0, |x| x.0);
    let zl = zl_all[k];
    egui::Grid::new("match_ctl").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Match to");
        ui.add(egui::DragValue::new(&mut st.ui.match_z0).range(1.0..=1000.0).suffix(" Ω"));
        ui.end_row();
        ui.label("Match at");
        ghz_field(ui, &mut st.ui.match_f);
        ui.end_row();
        ui.label("Component Q");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut st.ui.match_q.0).range(0.0..=1000.0).prefix("L ").custom_formatter(|v, _| if v == 0.0 { "ideal".into() } else { format!("{v:.0}") }));
            ui.add(egui::DragValue::new(&mut st.ui.match_q.1).range(0.0..=1000.0).prefix("C ").custom_formatter(|v, _| if v == 0.0 { "ideal".into() } else { format!("{v:.0}") }));
        });
        ui.end_row();
        ui.label("Band width");
        ui.checkbox(&mut st.ui.match_band, "Refine over the −10 dB band or ±2 %");
        ui.end_row();
    });
    tiles(ui, &[(format!("{:.1} {:+.1}j Ω", zl.re, zl.im), "antenna impedance", theme::TEXT), (format!("{:.1} dB", antenna_core::db20(((zl - z0) / (zl + z0)).norm())), "S11 unmatched", theme::TEXT)]);

    let (ql, qc) = (st.ui.match_q.0, st.ui.match_q.1);
    let with_q = |mut n: MatchNet| {
        for e in &mut n.elements {
            e.q = match e.kind {
                Kind::SeriesL | Kind::ShuntL if ql > 0.0 => Some(ql),
                Kind::SeriesC | Kind::ShuntC if qc > 0.0 => Some(qc),
                _ => None,
            };
        }
        n
    };
    let mut cands: Vec<MatchNet> = Vec::new();
    if st.ui.match_band {
        let span = net.bandwidths(0, -10.0).iter().find(|b| b.0 <= f && f <= b.1).copied().unwrap_or((f * 0.98, f * 1.02));
        let qs = (Some(ql).filter(|q| *q > 0.0), Some(qc).filter(|q| *q > 0.0));
        cands.extend(matching::best_for_band(&net.f, &zl_all, z0, span.0, span.1, qs.0, qs.1).into_iter().map(|x| x.0));
    }
    cands.extend(matching::synth_l(zl, z0, f));
    let q_design = ((zl.re / z0).max(z0 / zl.re.max(1e-3)) - 1.0).max(0.0).sqrt() + 2.0;
    cands.extend(matching::synth_pi(zl, z0, f, q_design));
    cands.extend(matching::synth_t(zl, z0, f, q_design));
    cands.extend(matching::synth_stub(zl, z0, f));
    cands.extend(matching::synth_quarter_wave(zl, z0, f));
    if cands.is_empty() {
        weak(ui, "No network found for this load.");
        return;
    }
    let cands: Vec<MatchNet> = cands.into_iter().map(with_q).collect();
    st.ui.match_pick = st.ui.match_pick.min(cands.len() - 1);
    egui::ComboBox::from_id_salt("match_pick").selected_text(cands[st.ui.match_pick].name.clone()).width(ui.available_width()).show_ui(ui, |ui| {
        for (i, c) in cands.iter().enumerate() {
            let worst = matching::reflection(c, zl, z0, f).norm();
            let text = format!("{} · |Γ| {:.3}", c.name, worst);
            ui.selectable_value(&mut st.ui.match_pick, i, text);
        }
    });
    let pick = cands[st.ui.match_pick].clone();
    schematic(ui, &pick);
    // Real parts: snap to a standard series and show both curves.
    ui.horizontal(|ui| {
        ui.label("Snap to");
        for (s, name) in [(ESeries::E6, "E6"), (ESeries::E12, "E12"), (ESeries::E24, "E24"), (ESeries::E96, "E96")] {
            ui.selectable_value(&mut st.ui.match_series, s, name);
        }
    });
    let snapped = matching::snapped(&pick, st.ui.match_series);
    let ghz: Vec<f64> = net.f.iter().map(|x| x / 1e9).collect();
    // An ideal network reaches -300 dB at one frequency; that would flatten the rest.
    let db = |g: &[C64]| -> Vec<(f64, f64)> { ghz.iter().copied().zip(g.iter().map(|v| antenna_core::db20(v.norm()).max(-60.0))).collect() };
    let bare: Vec<C64> = zl_all.iter().map(|z| (*z - z0) / (*z + z0)).collect();
    let before = db(&bare);
    let after = db(&matching::sweep(&pick, &net.f, &zl_all, z0));
    let real = db(&matching::sweep(&snapped, &net.f, &zl_all, z0));
    charts::line_chart(
        ui,
        ChartOptions { title: "S11 with the network", unit: " dB", x_unit: "GHz", height: 210.0, limit: Some(-10.0), marker: Some(f / 1e9), ..Default::default() },
        &[Series { name: "Antenna alone", color: charts::GREY, points: &before }, Series { name: "Exact values", color: charts::BLUE, points: &after }, Series { name: "Standard parts", color: charts::ORANGE, points: &real }],
    );
    // Both from the parts that get built, whose mismatch is part of what the antenna receives.
    let (s11_parts, power_parts) = matching::as_built(&snapped, zl, z0, f);
    let list: Vec<String> = snapped.elements.iter().map(kind_text).collect();
    tiles(ui, &[(format!("{s11_parts:.2} dB"), "S11 with parts", if s11_parts <= -10.0 { theme::SUCCESS } else { theme::WARNING }), (format!("{power_parts:.2} dB"), "power to antenna", theme::TEXT)]);
    weak(ui, format!("Standard parts: {}. They carry only the Q set above: no series inductance, self-resonance or pad parasitics, which matter most near a part's self-resonance.", list.join(", ")));
    // The physical limit for this resonance.
    let q = net.q_factor(0)[k];
    if q.is_finite() && q > 0.0 {
        let limit = matching::bode_fano_bandwidth(q, f, 10f64.powf(-10.0 / 20.0));
        weak(ui, format!("Bode-Fano: with Q {q:.1} at this frequency no network can give more than {:.1} MHz at −10 dB.", limit / 1e6));
    }
}

pub fn mimo(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    let net = &r.network;
    let n = net.ports();
    theme::caption(ui, "MIMO and coupling");
    if n < 2 {
        weak(ui, "This model has one port. Try the two-port patch pair or the turnstile, or add a port on the Parts page.");
        return;
    }
    let ghz: Vec<f64> = net.f.iter().map(|f| f / 1e9).collect();
    let mut curves: Vec<(String, Vec<(f64, f64)>)> = Vec::new();
    for i in 0..n {
        for j in 0..i {
            curves.push((format!("S{}{}", i + 1, j + 1), ghz.iter().copied().zip(net.sij_db(i, j)).collect()));
        }
    }
    let series: Vec<Series> = curves.iter().enumerate().map(|(k, c)| Series { name: &c.0, color: charts::PALETTE[k % charts::PALETTE.len()], points: &c.1 }).collect();
    charts::line_chart(ui, ChartOptions { title: "Coupling between ports", unit: " dB", x_unit: "GHz", height: 170.0, limit: Some(-15.0), ..Default::default() }, &series);
    if r.solver.contains("FDTD") {
        let stop = st.project.setup.end_db.max(50.0);
        weak(ui, format!("FDTD stops at −{stop:.0} dB of energy, and what is cut off limits small signals: coupling near −{:.0} dB is good to about 10 %. For weaker coupling raise Stop at energy on the solver setup page.", stop - 20.0));
    }
    let ecc_s: Vec<(f64, f64)> = ghz.iter().copied().zip(net.ecc_from_s(0, 1)).collect();
    charts::line_chart(ui, ChartOptions { title: "Envelope correlation from S (ports 1, 2; lossless assumption)", x_unit: "GHz", height: 130.0, limit: Some(0.5), y_range: Some((0.0, 1.0)), ..Default::default() }, &[Series { name: "ECC", color: charts::PURPLE, points: &ecc_s }]);
    let k = st.ui.far_k.min(r.far_f.len().saturating_sub(1));
    if let (Some(a), Some(b)) = (r.far.first().and_then(|p| p.get(k)), r.far.get(1).and_then(|p| p.get(k))) {
        let e = sparam::ecc_from_far(a, b);
        let good = e < 0.5;
        verdict(ui, good, &format!("ECC from the far fields at {:.3} GHz: {e:.3}. Diversity gain {:.2} dB. {}", a.f / 1e9, sparam::diversity_gain_db(e), if good { "Below the usual 0.5 limit." } else { "Above the usual 0.5 limit." }));
    }
    super::results::drive_table(ui, st, n);
    let w = st.project.weights(n);
    let tarc: Vec<(f64, f64)> = ghz.iter().copied().zip(net.tarc(&w).into_iter().map(|v| antenna_core::db20(v))).collect();
    charts::line_chart(ui, ChartOptions { title: "TARC at these weights", unit: " dB", x_unit: "GHz", height: 130.0, ..Default::default() }, &[Series { name: "TARC", color: charts::TEAL, points: &tarc }]);
    let active: Vec<Vec<(f64, f64)>> = (0..n).map(|p| ghz.iter().copied().zip(net.active_reflection(p, &w).iter().map(|v| antenna_core::db20(v.norm()))).collect()).collect();
    let names: Vec<String> = (0..n).map(|p| format!("Port {}", p + 1)).collect();
    let series: Vec<Series> = active.iter().enumerate().map(|(k, c)| Series { name: &names[k], color: charts::PALETTE[k % charts::PALETTE.len()], points: c }).collect();
    charts::line_chart(ui, ChartOptions { title: "Active reflection at these weights", unit: " dB", x_unit: "GHz", height: 150.0, limit: Some(-10.0), ..Default::default() }, &series);
}

pub fn rcs(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Radar cross section");
    weak(ui, "How strongly the metal model scatters a plane wave, from the method of moments with the ports shorted. Needs a model of metal only; an imported STL works.");
    let fit = st.ui.model.as_ref().map_or(Default::default(), |g| g.solver_fit());
    if !(fit.wire_mom || fit.surface_mom) {
        verdict(ui, false, "This model has dielectrics or mixes wires with sheets, which the method of moments here can't solve.");
        return;
    }
    if st.ui.rcs_f <= 0.0 {
        st.ui.rcs_f = st.project.f0();
    }
    egui::Grid::new("rcs_ctl").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Frequency");
        ghz_field(ui, &mut st.ui.rcs_f);
        ui.end_row();
        ui.label("Kind");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut st.ui.rcs_mono, true, "Monostatic");
            ui.selectable_value(&mut st.ui.rcs_mono, false, "Bistatic");
        });
        ui.end_row();
        ui.label("Polarisation");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut st.ui.rcs_pol_theta, true, "Eθ");
            ui.selectable_value(&mut st.ui.rcs_pol_theta, false, "Eφ");
        });
        ui.end_row();
        ui.label("In the plane φ");
        ui.add(egui::DragValue::new(&mut st.ui.rcs_phi).range(0.0..=360.0).suffix("°"));
        ui.end_row();
        if !st.ui.rcs_mono {
            ui.label("Wave from θ");
            ui.add(egui::DragValue::new(&mut st.ui.rcs_inc).range(0.0..=180.0).suffix("°"));
            ui.end_row();
        }
        ui.label("Angle step");
        ui.add(egui::DragValue::new(&mut st.ui.rcs_step).range(0.5..=30.0).speed(0.5).suffix("°"));
        ui.end_row();
    });
    if super::run_button(ui, &theme::label(icon::PLAY, "Compute the RCS"), !st.busy()) {
        let u = &st.ui;
        let (f, phi, pol, mono, inc, step) = (u.rcs_f, u.rcs_phi, u.rcs_pol_theta, u.rcs_mono, u.rcs_inc, u.rcs_step);
        st.run_rcs(f, phi, pol, mono, inc, step);
    }
    let Some(r) = st.rcs() else { return };
    let peak = r.curve.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let at = r.curve.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map_or(0.0, |p| p.0);
    tiles(ui, &[(format!("{peak:.1} dBsm"), "largest", theme::TEXT), (format!("{at:.0}°"), "at θ", theme::TEXT), (format!("{} · {}", r.unknowns, super::duration(r.seconds)), "unknowns, time", theme::TEXT)]);
    let title = format!("{} RCS at {:.3} GHz, φ = {:.0}°", if r.monostatic { "Monostatic" } else { "Bistatic" }, r.f / 1e9, r.phi);
    charts::line_chart(ui, ChartOptions { title: &title, unit: " dBsm", x_unit: "θ°", height: 220.0, ..Default::default() }, &[Series { name: "RCS", color: charts::BLUE, points: &r.curve }]);
}

fn save_text(file: String, filter: (&str, &str), text: &[u8]) -> Result<Option<std::path::PathBuf>, String> {
    let Some(path) = rfd::FileDialog::new().add_filter(filter.0, &[filter.1]).set_file_name(file).save_file() else { return Ok(None) };
    std::fs::write(&path, text).map_err(|e| format!("{e}"))?;
    Ok(Some(path))
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn tech_for(f: f64) -> Tech {
    if f > 3e9 {
        Tech::Uwb
    } else if f > 1.5e9 {
        Tech::Ble
    } else {
        Tech::Rfid
    }
}

/// The pattern in the form the positioning planner reads: gain (radiation loss
/// included) on a regular grid, mismatch separate.
pub(crate) fn planner_antenna(r: &SimResult, label: String, axis_forward: bool, f0: f64) -> Option<FdtdAntenna> {
    let k = r.far_index(f0)?;
    let ff = r.far.first()?.get(k)?;
    let grid = ff.grid_db(Quantity::Gain, Pol::Total);
    let net = &r.network;
    let i = net.f.iter().enumerate().min_by(|a, b| (a.1 - ff.f).abs().total_cmp(&(b.1 - ff.f).abs()))?.0;
    let g = net.s[i][0][0];
    let mismatch = 10.0 * (1.0 - g.norm_sqr()).max(1e-6).log10();
    Some(FdtdAntenna {
        tech: tech_for(ff.f),
        label,
        source: r.dir.clone().into(),
        theta_step: ff.theta_step() as f32,
        phi_step: ff.phi_step() as f32,
        d: grid.iter().map(|row| row.iter().map(|v| *v as f32).collect()).collect(),
        mismatch_db: mismatch as f32,
        axis_forward,
        bias_m: None,
    })
}

/// Writes the project to its file, or asks for one: the first time, on request, or when the
/// antenna showing is no longer the one in that file.
pub fn save_project(st: &mut Studio, pick: bool) {
    let known = st.ui.file.as_ref().filter(|f| !pick && f.is_for(&st.project)).map(|f| project_file::antenna_save_path(&f.path));
    let file_name = format!("{}.{}", slug(&st.project.name), project_file::EXTENSION);
    let Some(path) = known.or_else(|| rfd::FileDialog::new().add_filter("Sense Sim project", &[project_file::EXTENSION]).set_file_name(file_name).save_file().map(|p| project_file::save_path(&p))) else { return };
    match write_project(&st.project, &path) {
        Ok(()) => {
            st.ui.message = Some((format!("Saved {}", path.display()), true));
            st.ui.file = Some(super::ProjectFile { path, saved: st.project.clone() });
        }
        Err(e) => st.ui.message = Some((format!("Could not save: {e:#}"), false)),
    }
}

fn write_project(project: &Project, path: &std::path::Path) -> anyhow::Result<()> {
    project_file::write(path, &project_file::antenna_doc(serde_json::to_value(project)?))
}

/// An antenna project from a `.sesim`, or from the `.antenna.json` earlier versions saved.
fn read_project(path: &std::path::Path) -> anyhow::Result<Project> {
    use anyhow::Context;
    serde_json::from_value(project_file::antenna_in(project_file::read(path)?)).context("this is not a Sense Sim antenna project")
}

pub fn open_project(st: &mut Studio) {
    let Some(path) = rfd::FileDialog::new().add_filter("Sense Sim project", &[project_file::EXTENSION]).add_filter("Older antenna project", &["json"]).pick_file() else { return };
    open_project_at(st, path);
}

pub fn open_project_at(st: &mut Studio, path: std::path::PathBuf) {
    match read_project(&path) {
        Ok(project) => {
            st.ui.message = Some((format!("Opened {}", path.display()), true));
            st.ui.file = Some(super::ProjectFile { path, saved: project.clone() });
            st.project = project;
        }
        Err(e) => st.ui.message = Some((format!("{e:#}"), false)),
    }
}

pub fn export(ui: &mut egui::Ui, st: &mut Studio, library: &mut AntennaLibrary, applied: &mut AppliedAntenna) {
    let name = slug(&st.project.name);
    let msg = |st: &mut Studio, r: Result<Option<std::path::PathBuf>, String>| match r {
        Ok(Some(p)) => st.ui.message = Some((format!("Saved {}", p.display()), true)),
        Ok(None) => {}
        Err(e) => st.ui.message = Some((format!("Could not save: {e}"), false)),
    };

    theme::caption(ui, "Project");
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut st.project.name).desired_width(220.0));
    });
    ui.horizontal(|ui| {
        if ui.button(theme::label(icon::FLOPPY_DISK, "Save project")).clicked() {
            save_project(st, false);
        }
        if ui.button(theme::label(icon::FOLDER_OPEN, "Open project")).clicked() {
            open_project(st);
        }
    });

    theme::caption(ui, "Geometry for fabrication");
    if let Ok(g) = st.ui.geometry.clone() {
        ui.horizontal_wrapped(|ui| {
            if ui.button("STL (metal, mm)").clicked() {
                let r = save_text(format!("{name}.stl"), ("STL", "stl"), &g.to_stl());
                msg(st, r);
            }
            let layers = g.flat_layers();
            if !layers.is_empty() {
                if ui.button("DXF (flat metal, mm)").clicked() {
                    let r = save_text(format!("{name}.dxf"), ("DXF", "dxf"), g.to_dxf().as_bytes());
                    msg(st, r);
                }
                for (axis, at, _) in &layers {
                    let word = ["x", "y", "z"][*axis];
                    if ui.button(format!("Gerber, {word} = {:.2} mm", at * 1e3)).clicked() {
                        let r = save_text(format!("{name}_{word}{:.0}um.gbr", at * 1e6), ("Gerber", "gbr"), g.to_gerber(*axis, *at).as_bytes());
                        msg(st, r);
                    }
                }
            }
        });
    }

    let Some(r) = st.latest() else {
        weak(ui, "Result files appear here after a run.");
        return;
    };
    theme::caption(ui, "Results");
    ui.horizontal_wrapped(|ui| {
        let n = r.network.ports();
        if ui.button(format!("Touchstone (.s{n}p)")).clicked() {
            let ext = format!("s{n}p");
            let res = save_text(format!("{name}.{ext}"), ("Touchstone", &ext), r.network.to_touchstone().as_bytes());
            msg(st, res);
        }
        if ui.button("Report (HTML)").on_hover_text("One page to read or print: the antenna with its drawings, parts and feeds, its match and bandwidth, its patterns, how the numbers were made, and the data").clicked() {
            let res = save_text(format!("{name}_report.html"), ("HTML", "html"), report_page(st, &r).as_bytes());
            msg(st, res);
        }
    });
    if let Some(ff) = r.far.first().and_then(|p| p.first()) {
        ui.horizontal_wrapped(|ui| {
            for (label, ext, text) in [("Far field CSV", "csv", ff.to_csv()), ("FEKO .ffe", "ffe", ff.to_ffe()), ("HFSS/CST .ffd", "ffd", ff.to_ffd())] {
                if ui.button(label).clicked() {
                    let res = save_text(format!("{name}.{ext}"), (label, ext), text.as_bytes());
                    msg(st, res);
                }
            }
        });
        weak(ui, format!("Far-field files hold port 1 at {:.3} GHz, r·E in volts for 1 W incident.", ff.f / 1e9));
    }

    theme::caption(ui, "Use it in the scene");
    let axis_forward = st.ui.geometry.as_ref().is_ok_and(|g| g.axis_forward);
    let f0 = st.project.f0();
    let ant_name = if st.ui.lib_name.is_empty() { format!("{} {:.2} GHz", st.project.name, f0 / 1e9) } else { st.ui.lib_name.clone() };
    if let Some(a) = planner_antenna(&r, ant_name.clone(), axis_forward, f0) {
        group(ui, |ui| {
            weak(ui, format!("The positioning planner can use this pattern instead of an ideal antenna: {} ({:.2} dB mismatch).", if axis_forward { "aimed where a device faces" } else { "standing upright" }, a.mismatch_db));
            ui.horizontal(|ui| {
                if st.ui.lib_name.is_empty() {
                    st.ui.lib_name = ant_name.clone();
                }
                ui.add(egui::TextEdit::singleline(&mut st.ui.lib_name).desired_width(200.0));
                if ui.button("Save to the antenna library").on_hover_text("Pick it per anchor or tag in Inspector → Antenna").clicked() {
                    match library.save(a.clone()) {
                        Ok(()) => st.ui.message = Some((format!("Saved \"{}\". Pick it in Inspector → Antenna.", a.label), true)),
                        Err(e) => st.ui.message = Some((format!("Could not save: {e}"), false)),
                    }
                }
            });
            ui.horizontal(|ui| {
                let on = applied.0.as_ref().is_some_and(|x| x.label == a.label && x.source == a.source);
                let mut v = on;
                if theme::toggle_switch(ui, &mut v).changed() {
                    applied.0 = v.then(|| a.clone());
                }
                ui.label(format!("Use on all {} infrastructure", a.tech.label()));
            });
        });
    }
}

#[cfg(test)]
mod project_file_tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sense-sim-antenna-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_antenna_comes_back_whole_from_its_project_file() {
        let d = folder("round");
        let mut p = Project::from_template("tag_loop", None).unwrap();
        p.params[0].expr = "868MHz".into();
        p.name = "Dán thử".into();
        let path = d.join("tag.sesim");
        write_project(&p, &path).unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(b"PK"), "a container, like every other project");
        assert!(project_file::holds_antenna(&path));
        // Reading JSON can move a number by its last digit (1052249999.9999999 Hz comes back as
        // 1052250000), as it always did for these projects, so it's held to the same as an older file.
        let as_older_file = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(read_project(&path).unwrap(), as_older_file);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_older_antenna_file_still_opens_and_saving_leaves_it_as_it_was() {
        let d = folder("old");
        let p = Project::from_template("dipole", None).unwrap();
        let old = d.join("Dipole.antenna.json");
        let text = p.to_json().unwrap();
        std::fs::write(&old, &text).unwrap();
        assert_eq!(read_project(&old).unwrap(), p);
        let new = project_file::antenna_save_path(&old);
        write_project(&p, &new).unwrap();
        assert_eq!(new, d.join("Dipole.sesim"));
        assert_eq!(read_project(&new).unwrap(), p);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), text);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_scene_is_not_taken_for_an_antenna() {
        let d = folder("scene");
        let path = d.join("warehouse.sesim");
        project_file::write(&path, &serde_json::json!({ "objects": [], "rtls": { "devices": [] } })).unwrap();
        assert!(!project_file::holds_antenna(&path));
        assert!(read_project(&path).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
