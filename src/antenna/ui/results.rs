//! Result pages: S-parameters, Smith chart, far field, fields, link budget.

use antenna_core::farfield::{Cut, Pol, Quantity};
use antenna_core::{C64, sparam};
use bevy_egui::egui::{self, vec2};

use super::charts::{self, ChartOptions, Series};
use super::{group, tiles, verdict, weak};
use crate::antenna::{Studio, run};
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SpView {
    #[default]
    Db,
    Phase,
    Vswr,
    Impedance,
    Admittance,
    GroupDelay,
    Q,
    Tdr,
}

impl SpView {
    const ALL: [(SpView, &'static str); 8] = [(SpView::Db, "dB"), (SpView::Phase, "Phase"), (SpView::Vswr, "VSWR"), (SpView::Impedance, "Z"), (SpView::Admittance, "Y"), (SpView::GroupDelay, "Delay"), (SpView::Q, "Q"), (SpView::Tdr, "TDR")];
}

/// What the run itself said about its numbers.
fn run_warnings(ui: &mut egui::Ui, r: &antenna_core::project::SimResult) {
    for w in &r.warnings {
        verdict(ui, false, w);
    }
}

/// Nothing to show yet: a pointer back to the run.
fn need_result(ui: &mut egui::Ui, st: &mut Studio) -> bool {
    if st.latest().is_some() {
        return false;
    }
    theme::empty_state(ui, theme::icon::CHART_LINE, "No results yet", "Run a simulation on the Solver setup page.");
    if ui.button("Go to the solver setup").clicked() {
        st.ui.page = super::Page::Setup;
    }
    true
}

pub fn sparams(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    let net = &r.network;
    let n = net.ports();
    if n == 0 {
        return;
    }
    let ghz: Vec<f64> = net.f.iter().map(|f| f / 1e9).collect();
    theme::caption(ui, "Match");
    run_warnings(ui, &r);
    // Verdict at the design frequency.
    let f0 = st.project.f0();
    let k0 = net.f.iter().enumerate().min_by(|a, b| (a.1 - f0).abs().total_cmp(&(b.1 - f0).abs())).map_or(0, |x| x.0);
    let s11 = net.sij_db(0, 0);
    let z = net.zin(0);
    let good = s11[k0] <= st.ui.sp_limit;
    let res = net.resonances(0);
    let hint = res
        .iter()
        .min_by(|a, b| (a.0 - f0).abs().total_cmp(&(b.0 - f0).abs()))
        .map(|(fr, _)| {
            let off = (fr - f0) / f0 * 100.0;
            if off.abs() > 1.0 { format!(" The nearest resonance is {:.1} % {} f0.", off.abs(), if off > 0.0 { "above" } else { "below" }) } else { String::new() }
        })
        .unwrap_or_default();
    verdict(ui, good, &format!("{} at {:.3} GHz: S11 {:.1} dB.{hint}", if good { "Matched" } else { "Not matched" }, f0 / 1e9, s11[k0]));
    let bw = net.bandwidths(0, st.ui.sp_limit);
    let bw_at_f0 = bw.iter().find(|b| b.0 <= f0 && f0 <= b.1).or(bw.first());
    // A band that reaches the end of the sweep is only as wide as the sweep lets it show.
    let cut_off = |band: &(f64, f64)| net.f.first().is_some_and(|f| band.0 <= *f) || net.f.last().is_some_and(|f| band.1 >= *f);
    let mut items = vec![
        (format!("{:.0} {:+.0}j Ω", z[k0].re, z[k0].im), "Zin at f0", theme::TEXT),
        (format!("{:.2}", net.vswr(0)[k0]), "VSWR at f0", theme::TEXT),
        (format!("{:.2} dB", net.mismatch_loss_db(0)[k0]), "mismatch loss", theme::TEXT),
    ];
    if let Some(&(fr, db)) = res.iter().min_by(|a, b| a.1.total_cmp(&b.1)) {
        items.push((format!("{:.3} GHz", fr / 1e9), "best match at", theme::TEXT));
        items.push((format!("{db:.1} dB"), "best match", theme::TEXT));
    }
    if let Some(band @ (a, b)) = bw_at_f0 {
        items.push((format!("{}{:.0} MHz", if cut_off(band) { "≥ " } else { "" }, (b - a) / 1e6), &"bandwidth", theme::TEXT));
    }
    let q = net.q_factor(0);
    items.push((if q[k0].is_finite() { format!("{:.1}", q[k0]) } else { "–".into() }, "Q at f0", theme::TEXT));
    tiles(ui, &items);
    if r.end_db > 0.0 {
        let left = 10f64.powf(-r.end_db / 20.0);
        let error_db = |level_db: f64| 20.0 * (1.0 + left / 10f64.powf(level_db / 20.0)).log10();
        weak(ui, format!("FDTD stopped at −{:.0} dB of energy, cutting off about {:.1} % of the amplitude: S11 near −20 dB is good to about {:.1} dB, and near −30 dB to about {:.1} dB. Lower the stop level on the solver setup page for more.", r.end_db, left * 100.0, error_db(-20.0), error_db(-30.0)));
    }

    let mut view = st.ui.sp_view;
    theme::segmented_fill(ui, &mut view, &SpView::ALL);
    st.ui.sp_view = view;
    let palette = charts::PALETTE;
    match view {
        SpView::Db => {
            let mut curves: Vec<(String, Vec<(f64, f64)>)> = Vec::new();
            for i in 0..n {
                for j in 0..n {
                    if i >= j {
                        curves.push((format!("S{}{}", i + 1, j + 1), ghz.iter().copied().zip(net.sij_db(i, j)).collect()));
                    }
                }
            }
            let series: Vec<Series> = curves.iter().enumerate().map(|(k, c)| Series { name: &c.0, color: palette[k % palette.len()], points: &c.1 }).collect();
            charts::line_chart(ui, ChartOptions { title: "S-parameters", unit: " dB", x_unit: "GHz", height: 240.0, limit: Some(st.ui.sp_limit), marker: Some(f0 / 1e9), ..Default::default() }, &series);
            ui.horizontal(|ui| {
                ui.label("Match limit");
                ui.add(egui::DragValue::new(&mut st.ui.sp_limit).range(-40.0..=-1.0).suffix(" dB"));
            });
            if !bw.is_empty() {
                let list: Vec<String> = bw.iter().map(|(a, b)| format!("{:.3}–{:.3} GHz ({:.1} %)", a / 1e9, b / 1e9, (b - a) / ((a + b) / 2.0) * 100.0)).collect();
                weak(ui, format!("Bands below {:.0} dB: {}.", st.ui.sp_limit, list.join(", ")));
                if bw.iter().any(cut_off) {
                    weak(ui, "The match continues past the end of the sweep, so the real bandwidth is wider. Widen the sweep to measure it.");
                }
            } else {
                weak(ui, format!("No band below {:.0} dB in this sweep.", st.ui.sp_limit));
            }
        }
        SpView::Phase => {
            let mut curves: Vec<(String, Vec<(f64, f64)>)> = Vec::new();
            for i in 0..n {
                for j in 0..n {
                    if i >= j {
                        curves.push((format!("∠S{}{}", i + 1, j + 1), ghz.iter().copied().zip(net.sij_phase_deg(i, j)).collect()));
                    }
                }
            }
            let series: Vec<Series> = curves.iter().enumerate().map(|(k, c)| Series { name: &c.0, color: palette[k % palette.len()], points: &c.1 }).collect();
            charts::line_chart(ui, ChartOptions { title: "Phase, unwrapped", unit: "°", x_unit: "GHz", height: 240.0, ..Default::default() }, &series);
        }
        SpView::Vswr => {
            let curves: Vec<(String, Vec<(f64, f64)>)> = (0..n).map(|p| (format!("Port {}", p + 1), ghz.iter().copied().zip(net.vswr(p).into_iter().map(|v| v.min(20.0))).collect())).collect();
            let series: Vec<Series> = curves.iter().enumerate().map(|(k, c)| Series { name: &c.0, color: palette[k % palette.len()], points: &c.1 }).collect();
            charts::line_chart(ui, ChartOptions { title: "VSWR", x_unit: "GHz", height: 240.0, limit: Some(2.0), y_range: Some((1.0, 10.0)), marker: Some(f0 / 1e9), ..Default::default() }, &series);
        }
        SpView::Impedance | SpView::Admittance => {
            let (re, im, unit, title): (Vec<(f64, f64)>, Vec<(f64, f64)>, &str, &str) = if view == SpView::Impedance {
                (ghz.iter().copied().zip(z.iter().map(|c| c.re)).collect(), ghz.iter().copied().zip(z.iter().map(|c| c.im)).collect(), " Ω", "Input impedance, port 1")
            } else {
                let y: Vec<C64> = z.iter().map(|c| C64::new(1.0, 0.0) / *c * 1000.0).collect();
                (ghz.iter().copied().zip(y.iter().map(|c| c.re)).collect(), ghz.iter().copied().zip(y.iter().map(|c| c.im)).collect(), " mS", "Input admittance, port 1")
            };
            // A sharp antiresonance would flatten everything else.
            let lim = re.iter().map(|v| v.1.abs()).fold(0.0, f64::max).min(4000.0).max(1e-3);
            let clamp = |v: &[(f64, f64)]| -> Vec<(f64, f64)> { v.iter().map(|p| (p.0, p.1.clamp(-lim, lim))).collect() };
            let (re, im) = (clamp(&re), clamp(&im));
            charts::line_chart(ui, ChartOptions { title, unit, x_unit: "GHz", height: 240.0, marker: Some(f0 / 1e9), ..Default::default() }, &[Series { name: "Real", color: charts::GREEN, points: &re }, Series { name: "Imaginary", color: charts::ORANGE, points: &im }]);
        }
        SpView::GroupDelay => {
            let (i, j) = st.ui.sp_pair;
            let (i, j) = (i.min(n - 1), j.min(n - 1));
            let d: Vec<(f64, f64)> = ghz.iter().copied().zip(net.group_delay(i, j).into_iter().map(|v| v * 1e9)).collect();
            let name = format!("S{}{}", i + 1, j + 1);
            charts::line_chart(ui, ChartOptions { title: "Group delay", unit: " ns", x_unit: "GHz", height: 240.0, ..Default::default() }, &[Series { name: &name, color: charts::BLUE, points: &d }]);
            if n > 1 {
                ui.horizontal(|ui| {
                    ui.label("Between ports");
                    let mut a = i + 1;
                    let mut b = j + 1;
                    ui.add(egui::DragValue::new(&mut a).range(1..=n));
                    ui.add(egui::DragValue::new(&mut b).range(1..=n));
                    st.ui.sp_pair = (a - 1, b - 1);
                });
            }
        }
        SpView::Q => {
            let d: Vec<(f64, f64)> = ghz.iter().copied().zip(q.iter().map(|v| v.min(1000.0))).collect();
            charts::line_chart(ui, ChartOptions { title: "Quality factor (Yaghjian-Best)", x_unit: "GHz", height: 240.0, marker: Some(f0 / 1e9), ..Default::default() }, &[Series { name: "Q", color: charts::PURPLE, points: &d }]);
            if let Ok(g) = &st.ui.geometry {
                let (lo, hi) = g.bounds();
                let a = antenna_core::geometry::norm(antenna_core::geometry::sub(hi, lo)) / 2.0;
                let ka = 2.0 * std::f64::consts::PI * f0 / antenna_core::C0 * a;
                weak(ui, format!("Enclosing sphere: ka = {ka:.2}. Chu's limit for a linearly polarised antenna of that size: Q ≥ {:.1}. A Q far above it means the design has room for more bandwidth.", sparam::chu_q_limit(ka)));
            }
        }
        SpView::Tdr => {
            let (t, ohm) = net.tdr(0, 1024);
            let d: Vec<(f64, f64)> = t.iter().map(|x| x * 1e9).zip(ohm.iter().map(|v| v.clamp(0.0, 1000.0))).collect();
            charts::line_chart(ui, ChartOptions { title: "Time-domain reflectometry, port 1", unit: " Ω", x_unit: "ns", height: 240.0, ..Default::default() }, &[Series { name: "Impedance", color: charts::TEAL, points: &d }]);
            weak(ui, "From the swept S-parameters by inverse transform; its resolution follows the sweep's bandwidth.");
        }
    }
}

pub fn smith(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    let net = &r.network;
    let n = net.ports();
    if n == 0 {
        return;
    }
    theme::caption(ui, "Smith chart");
    let ghz: Vec<f64> = net.f.iter().map(|f| f / 1e9).collect();
    let curves: Vec<(String, Vec<C64>)> = (0..n).map(|p| (format!("S{0}{0}", p + 1), net.s.iter().map(|m| m[p][p]).collect())).collect();
    let refs: Vec<(&str, egui::Color32, &[C64])> = curves.iter().enumerate().map(|(k, c)| (c.0.as_str(), charts::PALETTE[k % charts::PALETTE.len()], c.1.as_slice())).collect();
    let z0 = net.z0.first().copied().unwrap_or(50.0);
    let size = ui.available_width().min(460.0);
    charts::smith_chart(ui, &refs, &ghz, z0, size);
    weak(ui, format!("Normalised to {z0:.0} Ω. Filled dot: start of the sweep ({:.3} GHz); ring: its end ({:.3} GHz). Hover the curve to read the impedance.", ghz[0], ghz[ghz.len() - 1]));
    let f0 = st.project.f0();
    let k = net.f.iter().enumerate().min_by(|a, b| (a.1 - f0).abs().total_cmp(&(b.1 - f0).abs())).map_or(0, |x| x.0);
    let g = net.s[k][0][0];
    let zv = net.zin(0)[k];
    tiles(ui, &[(format!("{:.0} {:+.0}j Ω", zv.re, zv.im), "Z at f0", theme::TEXT), (format!("{:.3}", g.norm()), "|Γ| at f0", theme::TEXT), (format!("{:.0}°", g.arg().to_degrees()), "∠Γ at f0", theme::TEXT)]);
    if ui.button("Design a matching network").clicked() {
        st.ui.page = super::Page::Matching;
    }
}

pub fn far_field(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    if r.far_f.is_empty() || r.far.is_empty() {
        weak(ui, "This run has no far field. Add a frequency on the Solver setup page.");
        return;
    }
    theme::caption(ui, "Far field");
    run_warnings(ui, &r);
    let n_ports = r.far.len();
    st.ui.far_k = st.ui.far_k.min(r.far_f.len() - 1);
    st.ui.far_port = st.ui.far_port.min(n_ports - 1);
    egui::Grid::new("ff_controls").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Frequency");
        egui::ComboBox::from_id_salt("ff_f").selected_text(format!("{:.3} GHz", r.far_f[st.ui.far_k] / 1e9)).show_ui(ui, |ui| {
            for (k, f) in r.far_f.iter().enumerate() {
                ui.selectable_value(&mut st.ui.far_k, k, format!("{:.3} GHz", f / 1e9));
            }
        });
        ui.end_row();
        ui.label("Show");
        egui::ComboBox::from_id_salt("ff_q").selected_text(st.ui.quantity.label()).show_ui(ui, |ui| {
            for q in Quantity::ALL {
                ui.selectable_value(&mut st.ui.quantity, q, q.label());
            }
        });
        ui.end_row();
        ui.label("Component");
        egui::ComboBox::from_id_salt("ff_pol").selected_text(st.ui.pol.label()).show_ui(ui, |ui| {
            for p in Pol::ALL {
                ui.selectable_value(&mut st.ui.pol, p, p.label());
            }
        });
        ui.end_row();
        ui.label("Range");
        ui.add(egui::Slider::new(&mut st.ui.range_db, 10.0..=60.0).suffix(" dB"));
        ui.end_row();
        if n_ports > 1 {
            ui.label("Driven");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut st.ui.use_drive, false, "One port");
                ui.selectable_value(&mut st.ui.use_drive, true, "All, weighted");
                if !st.ui.use_drive {
                    let mut p = st.ui.far_port + 1;
                    ui.add(egui::DragValue::new(&mut p).range(1..=n_ports));
                    st.ui.far_port = p - 1;
                }
            });
            ui.end_row();
        }
    });
    if n_ports > 1 && st.ui.use_drive {
        drive_table(ui, st, n_ports);
    }
    let k = st.ui.far_k;
    let ff = if st.ui.use_drive && n_ports > 1 { r.combined(k, &st.project.weights(n_ports)) } else { r.far[st.ui.far_port].get(k).cloned() };
    let Some(ff) = ff else { return };
    let m = ff.metrics();
    let items = vec![
        (format!("{:.2} dBi", m.peak_realized_dbi), "realized gain", theme::SUCCESS),
        (format!("{:.2} dBi", m.peak_gain_dbi), "gain", theme::TEXT),
        (format!("{:.2} dBi", m.peak_directivity_dbi), "directivity", theme::TEXT),
        (format!("{:.1} %", m.rad_efficiency * 100.0), if r.lossless { "power balance" } else { "radiation eff." }, theme::TEXT),
        (format!("{:.1} %", m.total_efficiency * 100.0), if r.lossless { "total eff. (mismatch)" } else { "total eff." }, theme::TEXT),
        // On the z axis φ means nothing.
        (if m.peak_theta < 0.5 || m.peak_theta > 179.5 { format!("{:.0}°", m.peak_theta) } else { format!("{:.0}°, {:.0}°", m.peak_theta, m.peak_phi) }, "peak θ, φ", theme::TEXT),
        (m.hpbw_e.map_or("–".into(), |v| format!("{v:.1}°")), "beam width, main", theme::TEXT),
        (m.hpbw_h.map_or("–".into(), |v| format!("{v:.1}°")), "beam width, cross", theme::TEXT),
        (m.front_to_back_db.map_or("–".into(), |v| format!("{v:.1} dB")), "front/back", theme::TEXT),
        (m.side_lobe_db.map_or("–".into(), |v| format!("{v:.1} dB")), "side lobes", theme::TEXT),
        (if m.axial_ratio_db > 40.0 { "linear".to_string() } else { format!("{:.1} dB", m.axial_ratio_db) }, "axial ratio", theme::TEXT),
        (if m.axial_ratio_db > 20.0 { "linear".to_string() } else if m.rhcp_at_peak { "RHCP".into() } else { "LHCP".into() }, "polarisation", theme::TEXT),
    ];
    tiles(ui, &items);
    if n_ports > 1 && !st.ui.use_drive {
        weak(ui, "Power coupled into the other ports, which absorb it, counts against radiation efficiency here.");
    }
    if r.lossless {
        weak(ui, format!("Metal: {}. This model has no loss, so gain is just directivity, and the power balance only checks the far-field integral: it says nothing about the real antenna's efficiency.", r.metal));
    } else {
        weak(ui, format!("Metal: {}.", r.metal));
    }
    let single_drive = n_ports > 1 && !st.ui.use_drive;
    let off = m.rad_efficiency - 1.0;
    if m.rad_efficiency.is_nan() {
        verdict(ui, false, "The port returns more power than it receives at this frequency, which means a negative resistance: gain and efficiency have no meaning here. Check the mesh and the feed.");
    } else if off > antenna_core::checks::BALANCE_LIMIT || (r.lossless && !single_drive && off < -antenna_core::checks::BALANCE_LIMIT) {
        let fix = if r.solver.contains("FDTD") { "Refine the mesh or lower the stop level." } else { "Refine the mesh or the grid step." };
        verdict(ui, false, &format!("Radiated power is {:.1} % of what the port accepts: the far field isn't converged. {fix}", m.rad_efficiency * 100.0));
    }
    let (q, pol) = (st.ui.quantity, st.ui.pol);
    let peak = ff.grid_db(q, pol).iter().flatten().copied().fold(f64::MIN, f64::max);
    let cross = match pol {
        Pol::Theta => Pol::Phi,
        Pol::Phi => Pol::Theta,
        Pol::Rhcp => Pol::Lhcp,
        Pol::Lhcp => Pol::Rhcp,
        Pol::Co => Pol::Cross,
        Pol::Cross => Pol::Co,
        Pol::Total => Pol::Total,
    };
    let plane = |c: Cut| {
        let main = ff.cut(c, q, pol);
        let other = (cross != pol).then(|| ff.cut(c, q, cross));
        (main, other)
    };
    let cuts = [("φ = 0° plane", Cut::Phi(0.0)), ("φ = 90° plane", Cut::Phi(90.0)), ("θ = 90° cone", Cut::Theta(90.0))];
    let size = ((ui.available_width() - 16.0) / 2.0).clamp(120.0, 230.0);
    for row in cuts.chunks(2) {
        ui.horizontal_top(|ui| {
        for (name, c) in row {
            let c = *c;
            let (main, other) = plane(c);
            let mut curves: Vec<(&str, egui::Color32, &[(f64, f64)])> = vec![(pol.label(), charts::ORANGE, main.as_slice())];
            if let Some(o) = &other {
                curves.push((cross.label(), charts::BLUE, o.as_slice()));
            }
            charts::polar_chart(ui, name, &curves, peak, size);
        }
        });
    }
    weak(ui, format!("Outer ring {peak:.1} dBi, then every 10 dB. θ = 0° is the +z axis."));
    let (main, other) = plane(Cut::Phi(m.peak_phi));
    let mut series = vec![Series { name: pol.label(), color: charts::ORANGE, points: &main }];
    if let Some(o) = &other {
        series.push(Series { name: cross.label(), color: charts::BLUE, points: o });
    }
    charts::line_chart(ui, ChartOptions { title: "Through the beam", unit: " dBi", x_unit: "θ°", height: 170.0, y_range: Some((peak - st.ui.range_db, peak + 1.0)), ..Default::default() }, &series);
    if r.far_f.len() > 1 {
        let gain: Vec<(f64, f64)> = r.far[0].iter().map(|f| (f.f / 1e9, f.metrics().peak_realized_dbi)).collect();
        let eff: Vec<(f64, f64)> = r.far[0].iter().map(|f| (f.f / 1e9, f.total_efficiency() * 100.0)).collect();
        charts::line_chart(ui, ChartOptions { title: "Peak realized gain against frequency", unit: " dBi", x_unit: "GHz", height: 130.0, ..Default::default() }, &[Series { name: "Gain", color: charts::ORANGE, points: &gain }]);
        charts::line_chart(ui, ChartOptions { title: "Total efficiency against frequency", unit: " %", x_unit: "GHz", height: 130.0, ..Default::default() }, &[Series { name: "Efficiency", color: charts::GREEN, points: &eff }]);
    }
}

/// Amplitude and phase per port (HFSS "Edit Sources").
pub fn drive_table(ui: &mut egui::Ui, st: &mut Studio, n: usize) {
    while st.project.drive.len() < n {
        st.project.drive.push(antenna_core::project::Drive { amp: 1.0, phase: 0.0 });
    }
    group(ui, |ui| {
        theme::caption(ui, "Port excitation");
        egui::Grid::new("drive").num_columns(3).spacing(vec2(10.0, 3.0)).show(ui, |ui| {
            for (i, d) in st.project.drive.iter_mut().take(n).enumerate() {
                ui.label(format!("Port {}", i + 1));
                ui.add(egui::DragValue::new(&mut d.amp).range(0.0..=10.0).speed(0.01).prefix("× "));
                ui.add(egui::DragValue::new(&mut d.phase).range(-360.0..=360.0).speed(1.0).suffix("°"));
                ui.end_row();
            }
        });
        weak(ui, "Post-processing only, no new run. Two crossed dipoles at 0° and 90° give circular polarisation.");
    });
}

pub fn fields(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    theme::caption(ui, "Fields and currents");
    if !r.planes.is_empty() {
        egui::Grid::new("field_ctl").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Plane");
            st.ui.plane_k = st.ui.plane_k.min(r.planes.len() - 1);
            egui::ComboBox::from_id_salt("plane").selected_text(run::plane_title(&r.planes[st.ui.plane_k].name)).show_ui(ui, |ui| {
                for (k, p) in r.planes.iter().enumerate() {
                    ui.selectable_value(&mut st.ui.plane_k, k, run::plane_title(&p.name));
                }
            });
            ui.end_row();
        });
        ui.checkbox(&mut st.ui.animate, "Animate over one RF period");
        charts::heat_legend(ui, "−40 dB", "0 dB (99th percentile)");
        let p = &r.planes[st.ui.plane_k];
        weak(ui, format!("{} × {} samples at the design frequency, from the first port's run. On the canvas, in its real place around the model.", p.nu, p.nv));
    }
    if r.currents.is_some() {
        ui.horizontal(|ui| {
            if ui.selectable_label(st.ui.view == super::ViewMode::Currents, "Show the currents").clicked() {
                st.ui.view = super::ViewMode::Currents;
            }
        });
        charts::heat_legend(ui, "−30 dB", "0 dB (98th percentile)");
        weak(ui, "Current magnitude on every wire segment or triangle at the first far-field frequency, for port 1 driven.");
    }
    if r.planes.is_empty() && r.currents.is_none() {
        weak(ui, "This run saved no fields. Turn on field planes in the solver setup and run again.");
    }
}

/// Link budget inputs (Friis, free space).
pub struct LinkBudget {
    pub tx_dbm: f64,
    pub rx_gain: f64,
    pub sens_dbm: f64,
    pub margin_db: f64,
}

impl Default for LinkBudget {
    fn default() -> Self {
        Self { tx_dbm: 10.0, rx_gain: 2.0, sens_dbm: -90.0, margin_db: 10.0 }
    }
}

pub fn link(ui: &mut egui::Ui, st: &mut Studio) {
    if need_result(ui, st) {
        return;
    }
    let Some(r) = st.latest() else { return };
    theme::caption(ui, "Link budget");
    let Some(m) = r.metrics(0, st.ui.far_k.min(r.far_f.len().saturating_sub(1))) else {
        weak(ui, "This run has no far field.");
        return;
    };
    weak(ui, format!("This antenna transmits: {:.2} dBi realized gain at {:.3} GHz (mismatch and loss included). Free space, Friis.", m.peak_realized_dbi, m.f / 1e9));
    let l = &mut st.ui.link;
    egui::Grid::new("link").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Transmit power");
        ui.add(egui::DragValue::new(&mut l.tx_dbm).range(-40.0..=60.0).suffix(" dBm"));
        ui.end_row();
        ui.label("Receive antenna gain");
        ui.add(egui::DragValue::new(&mut l.rx_gain).range(-20.0..=40.0).suffix(" dBi"));
        ui.end_row();
        ui.label("Receiver sensitivity");
        ui.add(egui::DragValue::new(&mut l.sens_dbm).range(-150.0..=0.0).suffix(" dBm"));
        ui.end_row();
        ui.label("Fade margin");
        ui.add(egui::DragValue::new(&mut l.margin_db).range(0.0..=60.0).suffix(" dB"));
        ui.end_row();
    });
    let range = sparam::friis_range_m(l.tx_dbm, m.peak_realized_dbi, l.rx_gain, m.f, l.sens_dbm + l.margin_db);
    let eirp = l.tx_dbm + m.peak_realized_dbi;
    tiles(ui, &[(format!("{eirp:.1} dBm"), "EIRP", theme::TEXT), (if range >= 1000.0 { format!("{:.1} km", range / 1000.0) } else { format!("{range:.0} m") }, "range", theme::SUCCESS), (format!("{:.1} dB", l.tx_dbm + m.peak_realized_dbi + l.rx_gain - l.sens_dbm), "link budget", theme::TEXT)]);
    let (tx, rx, s, f) = (l.tx_dbm, l.rx_gain, l.sens_dbm, m.f);
    let curve: Vec<(f64, f64)> = (0..=60).map(|k| {
        let d = 10f64.powf(-1.0 + k as f64 / 60.0 * 5.0);
        (d.log10(), sparam::friis_received_dbm(tx, m.peak_realized_dbi, rx, f, d))
    }).collect();
    charts::line_chart(ui, ChartOptions { title: "Received power", unit: " dBm", x_unit: "log₁₀ distance (m)", height: 170.0, limit: Some(s), ..Default::default() }, &[Series { name: "Rx", color: charts::BLUE, points: &curve }]);
}
