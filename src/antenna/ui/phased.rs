//! The array and signal-processing pages: Phased array, Direction finding, Signal chain and
//! Radar. All of it is in the project file, and none of it needs a solve to start; with a solved
//! multi-port run the first two read the real array, coupling and all.

use std::sync::Arc;

use antenna_core::farfield::FarField;
use antenna_core::geometry::{Geometry, tessellate};
use antenna_core::phased::aperture::{Element, Layout, Sparse, angle_between};
use antenna_core::phased::beam::{self, Beam, BeamPlan, Delay, MaskRegion, MultiBeam};
use antenna_core::phased::chain::{self, Adc, Bf, Lo, Stage, Waveform};
use antenna_core::phased::doa::{self, Method, Scan, Source, Spectrum};
use antenna_core::phased::pattern::ElementKind;
use antenna_core::phased::radar::Target;
use antenna_core::phased::solved::SolvedArray;
use antenna_core::phased::{JammerDesign, PhasedDesign};
use antenna_core::C64;
use bevy_egui::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, TextureHandle, pos2, vec2};

use super::charts::{self, ChartOptions, Series};
use super::{Page, ghz_field, group, run_button, tiles, verdict, view3d, weak};
use crate::antenna::Studio;
use crate::antenna::phased::{BeamView, DfView, Lazy, SolvedView, beam_view, butler_beams, design_f0, df_view, marker_model, solved_array, solved_element, solved_view, switched_errors};
use crate::theme::{self, icon};

type BeamKey = (f64, Layout, ElementKind, BeamPlan, f64, usize);
type SolvedKey = (usize, usize, BeamPlan, bool);
type DfKey = (antenna_core::phased::DfDesign, f64, usize);

/// A cached picture and what it was drawn from.
type Picture = Option<(usize, TextureHandle)>;

/// Editing state and cached results of these pages. What matters is in the project.
#[derive(Default)]
pub struct PhasedUi {
    beam: Lazy<BeamKey, Result<BeamView, String>>,
    solved: Lazy<SolvedKey, SolvedView>,
    df: Lazy<DfKey, DfView>,
    /// Weights that realize the most gain, rather than textbook phases.
    pub matched: bool,
    /// The latest run as an array, and what it was read for: result, far-field index, variables.
    array: Option<(usize, usize, Vec<antenna_core::project::Param>, Option<Arc<SolvedArray>>)>,
    markers: Option<(Layout, u64, Arc<(Geometry, Vec<view3d::Tessellated>)>)>,
    map_picture: Picture,
    df_picture: Picture,
    radar_picture: Picture,
    pub shape_phi: f64,
    pub shape_phase_only: bool,
    pub shape_top: (f64, f64),
    pub shape_top_ripple: f64,
    pub shape_guard: f64,
    pub shape_sll: f64,
    pub switched: doa::Switched,
    switched_view: Lazy<(doa::Switched, antenna_core::phased::DfDesign, f64), Result<(f64, f64), String>>,
    /// Direction finders for a fix: x and y in metres, bearing and its error in degrees.
    pub stations: Vec<[f64; 4]>,
}

fn array_of(st: &mut Studio) -> Option<Arc<SolvedArray>> {
    let r = st.latest()?;
    let key = (Arc::as_ptr(&r) as usize, st.ui.far_k);
    if let Some((a, b, params, cached)) = &st.ui.phased.array
        && (*a, *b) == key
        && *params == st.project.params
    {
        return cached.clone();
    }
    let made = solved_array(st);
    st.ui.phased.array = Some((key.0, key.1, st.project.params.clone(), made.clone()));
    made
}

fn num(ui: &mut egui::Ui, v: &mut f64, range: std::ops::RangeInclusive<f64>, speed: f64, suffix: &str) -> egui::Response {
    ui.add(egui::DragValue::new(v).range(range).speed(speed).suffix(suffix.to_string()))
}

fn int(ui: &mut egui::Ui, v: &mut usize, range: std::ops::RangeInclusive<usize>) -> egui::Response {
    ui.add(egui::DragValue::new(v).range(range))
}

fn head(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong().size(13.0));
}

fn evidence_note(ui: &mut egui::Ui, simulated: bool) {
    let label = if simulated { proof::Evidence::Simulated.label() } else { proof::Evidence::Estimate.label() };
    let why = if simulated { "from a solved model, with its coupling. Not measured on hardware." } else { "from an array factor and these element models, with no coupling. Not measured on hardware." };
    weak(ui, format!("{label}: {why}"));
}

/// The same, for a page with no array model behind it.
fn estimate_note(ui: &mut egui::Ui, why: &str) {
    weak(ui, format!("{}: {why} Not measured on hardware.", proof::Evidence::Estimate.label()));
}

fn layout_name(l: &Layout) -> &'static str {
    match l {
        Layout::Line { .. } => "Line",
        Layout::Planar { triangular: false, .. } => "Planar, rectangular",
        Layout::Planar { triangular: true, .. } => "Planar, triangular",
        Layout::Ring { .. } => "Ring",
        Layout::Rings { .. } => "Concentric rings",
        Layout::Cylinder { .. } => "Cylinder",
        Layout::Panels { .. } => "Multi-face panels",
        Layout::SparseLine { kind: Sparse::Nested { .. }, .. } => "Sparse line, nested",
        Layout::SparseLine { kind: Sparse::Coprime { .. }, .. } => "Sparse line, coprime",
        Layout::SparseLine { kind: Sparse::MinimumRedundancy { .. }, .. } => "Sparse line, minimum redundancy",
        Layout::Custom { .. } => "Given positions",
    }
}

/// Picks a layout and sets its numbers. Spacings and radii are in wavelengths.
fn layout_ui(ui: &mut egui::Ui, id: &str, l: &mut Layout) {
    let options = [
        Layout::Line { n: 8, spacing: 0.5 },
        Layout::Planar { nx: 8, ny: 8, dx: 0.5, dy: 0.5, triangular: false },
        Layout::Planar { nx: 8, ny: 8, dx: 0.5, dy: 0.433, triangular: true },
        Layout::Ring { n: 8, radius: 0.5, radial: false },
        Layout::Rings { rings: vec![(4, 0.25), (8, 0.5)], radial: false },
        Layout::Cylinder { around: 16, rows: 4, radius: 1.5, dz: 0.5 },
        Layout::Panels { faces: 4, nx: 8, ny: 8, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 },
        Layout::SparseLine { kind: Sparse::Nested { n1: 3, n2: 3 }, spacing: 0.5 },
        Layout::SparseLine { kind: Sparse::Coprime { m: 3, n: 5 }, spacing: 0.5 },
        Layout::SparseLine { kind: Sparse::MinimumRedundancy { n: 5 }, spacing: 0.5 },
    ];
    egui::Grid::new((id, "layout")).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Layout");
        egui::ComboBox::from_id_salt((id, "layout_kind")).selected_text(layout_name(l)).width(220.0).show_ui(ui, |ui| {
            for option in &options {
                let on = layout_name(l) == layout_name(option);
                if ui.selectable_label(on, layout_name(option)).clicked() && !on {
                    *l = option.clone();
                }
            }
        });
        ui.end_row();
        match l {
            Layout::Line { n, spacing } => {
                ui.label("Elements");
                int(ui, n, 1..=1024);
                ui.end_row();
                ui.label("Spacing");
                num(ui, spacing, 0.05..=8.0, 0.005, " λ");
                ui.end_row();
            }
            Layout::Planar { nx, ny, dx, dy, .. } => {
                ui.label("Elements x × y");
                ui.horizontal(|ui| {
                    int(ui, nx, 1..=128);
                    ui.label("×");
                    int(ui, ny, 1..=128);
                });
                ui.end_row();
                ui.label("Spacing x, y");
                ui.horizontal(|ui| {
                    num(ui, dx, 0.05..=8.0, 0.005, " λ");
                    num(ui, dy, 0.05..=8.0, 0.005, " λ");
                });
                ui.end_row();
            }
            Layout::Ring { n, radius, radial } => {
                ui.label("Elements");
                int(ui, n, 3..=512);
                ui.end_row();
                ui.label("Radius");
                num(ui, radius, 0.02..=32.0, 0.005, " λ");
                ui.end_row();
                ui.label("Facing");
                ui.checkbox(radial, "outwards").on_hover_text("Off: all face up, as monopoles on a plate do.");
                ui.end_row();
            }
            Layout::Rings { rings, radial } => {
                ui.label("Rings");
                ui.vertical(|ui| {
                    let mut remove = None;
                    for (k, (n, r)) in rings.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            int(ui, n, 1..=256);
                            ui.label("at");
                            num(ui, r, 0.0..=32.0, 0.005, " λ");
                            if ui.small_button(icon::X).clicked() {
                                remove = Some(k);
                            }
                        });
                    }
                    if let Some(k) = remove.filter(|_| rings.len() > 1) {
                        rings.remove(k);
                    }
                    if ui.small_button(theme::label(icon::PLUS, "Ring")).clicked() {
                        let r = rings.last().map_or(0.5, |x| x.1 + 0.25);
                        rings.push((8, r));
                    }
                });
                ui.end_row();
                ui.label("Facing");
                ui.checkbox(radial, "outwards");
                ui.end_row();
            }
            Layout::Cylinder { around, rows, radius, dz } => {
                ui.label("Around × rows");
                ui.horizontal(|ui| {
                    int(ui, around, 3..=256);
                    ui.label("×");
                    int(ui, rows, 1..=64);
                });
                ui.end_row();
                ui.label("Radius, row step");
                ui.horizontal(|ui| {
                    num(ui, radius, 0.1..=32.0, 0.01, " λ");
                    num(ui, dz, 0.05..=4.0, 0.005, " λ");
                });
                ui.end_row();
            }
            Layout::Panels { faces, nx, ny, dx, dy, radius, tilt_deg } => {
                ui.label("Faces");
                int(ui, faces, 2..=12);
                ui.end_row();
                ui.label("Each x × y");
                ui.horizontal(|ui| {
                    int(ui, nx, 1..=64);
                    ui.label("×");
                    int(ui, ny, 1..=64);
                });
                ui.end_row();
                ui.label("Spacing x, y");
                ui.horizontal(|ui| {
                    num(ui, dx, 0.05..=4.0, 0.005, " λ");
                    num(ui, dy, 0.05..=4.0, 0.005, " λ");
                });
                ui.end_row();
                ui.label("From centre, tilt");
                ui.horizontal(|ui| {
                    num(ui, radius, 0.1..=32.0, 0.01, " λ");
                    num(ui, tilt_deg, -60.0..=60.0, 0.5, "°");
                });
                ui.end_row();
            }
            Layout::SparseLine { kind, spacing } => {
                match kind {
                    Sparse::Nested { n1, n2 } => {
                        ui.label("Inner, outer");
                        ui.horizontal(|ui| {
                            int(ui, n1, 1..=32);
                            int(ui, n2, 1..=32);
                        });
                        ui.end_row();
                    }
                    Sparse::Coprime { m, n } => {
                        ui.label("Spacings m, n");
                        ui.horizontal(|ui| {
                            int(ui, m, 2..=32);
                            int(ui, n, 2..=32);
                        });
                        ui.end_row();
                    }
                    Sparse::MinimumRedundancy { n } => {
                        ui.label("Elements");
                        int(ui, n, 3..=8);
                        ui.end_row();
                    }
                }
                ui.label("Base spacing");
                num(ui, spacing, 0.05..=4.0, 0.005, " λ");
                ui.end_row();
            }
            Layout::Custom { positions } => {
                ui.label("Elements");
                ui.label(format!("{} at given places", positions.len()));
                ui.end_row();
            }
        }
    });
}

fn element_ui(ui: &mut egui::Ui, e: &mut ElementKind, has_result: bool, has_array: bool) {
    egui::ComboBox::from_id_salt("phased_element").selected_text(e.label()).width(220.0).show_ui(ui, |ui| {
        for option in [ElementKind::Cosine { q: 1.5, back_db: -20.0 }, ElementKind::Isotropic, ElementKind::Sector3gpp] {
            if ui.selectable_label(std::mem::discriminant(e) == std::mem::discriminant(&option), option.label()).clicked() {
                *e = option;
            }
        }
        let plain = ElementKind::Solved { embedded: false };
        if ui.add_enabled(has_result, egui::Button::selectable(*e == plain, plain.label())).on_disabled_hover_text("Run a simulation first").clicked() {
            *e = plain;
        }
        let embedded = ElementKind::Solved { embedded: true };
        if ui.add_enabled(has_array, egui::Button::selectable(*e == embedded, embedded.label())).on_hover_text("The middle port of a solved array, moved to the origin: its coupling comes along, so a much larger array of the same lattice can be sized from a small solve.").on_disabled_hover_text("Solve a model with a port per element first").clicked() {
            *e = embedded;
        }
    });
    if let ElementKind::Cosine { q, back_db } = e {
        num(ui, q, 0.0..=20.0, 0.05, " q");
        num(ui, back_db, -60.0..=0.0, 0.5, " dB back");
    }
}

fn beams_ui(ui: &mut egui::Ui, plan: &mut BeamPlan) {
    egui::Grid::new("phased_beams").num_columns(5).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
        ui.label(RichText::new("Shown").size(11.5).color(theme::WEAK));
        ui.label(RichText::new("θ").size(11.5).color(theme::WEAK));
        ui.label(RichText::new("φ").size(11.5).color(theme::WEAK));
        ui.label(RichText::new("Share").size(11.5).color(theme::WEAK));
        ui.label("");
        ui.end_row();
        let mut remove = None;
        let many = plan.beams.len() > 1;
        for k in 0..plan.beams.len() {
            ui.radio_value(&mut plan.active, k, format!("{}", k + 1));
            let b = &mut plan.beams[k];
            num(ui, &mut b.theta, 0.0..=180.0, 0.25, "°");
            num(ui, &mut b.phi, -360.0..=360.0, 0.5, "°");
            num(ui, &mut b.share, 0.0..=10.0, 0.02, "");
            if many && ui.small_button(icon::X).clicked() {
                remove = Some(k);
            }
            ui.end_row();
        }
        if let Some(k) = remove {
            plan.beams.remove(k);
            plan.active = plan.active.min(plan.beams.len() - 1);
        }
    });
    ui.horizontal(|ui| {
        if ui.button(theme::label(icon::PLUS, "Beam")).clicked() {
            let last = plan.beams.last().copied().unwrap_or_default();
            plan.beams.push(Beam { theta: (last.theta + 20.0).min(70.0), phi: last.phi, share: 1.0 });
        }
        egui::ComboBox::from_id_salt("phased_multibeam").selected_text(plan.multibeam.label()).show_ui(ui, |ui| {
            for m in MultiBeam::ALL {
                ui.selectable_value(&mut plan.multibeam, m, m.label());
            }
        });
    });
    ui.add_space(2.0);
    weak(ui, "Nulls: directions the weights leave dark, at the least change to the beam.");
    let mut remove = None;
    for (k, n) in plan.nulls.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("Null {}", k + 1));
            num(ui, &mut n.0, 0.0..=180.0, 0.25, "° θ");
            num(ui, &mut n.1, -360.0..=360.0, 0.5, "° φ");
            if ui.small_button(icon::X).clicked() {
                remove = Some(k);
            }
        });
    }
    if let Some(k) = remove {
        plan.nulls.remove(k);
    }
    if ui.button(theme::label(icon::PLUS, "Null")).clicked() {
        plan.nulls.push((45.0, 180.0));
    }
}

/// With `solved` the settings that take more than one frequency or a layout's panels stay out:
/// a solved array is one frequency, and its ports have no subarrays.
fn feed_ui(ui: &mut egui::Ui, plan: &mut BeamPlan, flat: bool, solved: bool) {
    let hw = &mut plan.hw;
    egui::Grid::new("phased_feed").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        ui.label("Taper x");
        ui.horizontal(|ui| super::analysis::taper_ui(ui, "phased_tap_x", &mut plan.taper_x));
        ui.end_row();
        ui.label("Taper y");
        ui.horizontal(|ui| super::analysis::taper_ui(ui, "phased_tap_y", &mut plan.taper_y));
        ui.end_row();
        if !solved {
            ui.label("Steering");
            egui::ComboBox::from_id_salt("phased_delay").selected_text(hw.delay.label()).show_ui(ui, |ui| {
                for d in Delay::ALL {
                    ui.selectable_value(&mut hw.delay, d, d.label());
                }
            });
            ui.end_row();
            // A delay per subarray means there are subarrays.
            if hw.delay == Delay::Subarray && hw.subarray.is_none() {
                hw.subarray = Some((4, 4));
            }
        }
        ui.label("Phase shifters");
        ui.horizontal(|ui| {
            let mut on = hw.phase_bits.is_some();
            if ui.checkbox(&mut on, "finite bits").changed() {
                hw.phase_bits = on.then_some(6);
            }
            if let Some(b) = hw.phase_bits.as_mut() {
                ui.add(egui::DragValue::new(b).range(1..=12).suffix(" bits"));
            }
        });
        ui.end_row();
        ui.label("Attenuators");
        ui.horizontal(|ui| {
            let mut on = hw.atten_step_db.is_some();
            if ui.checkbox(&mut on, "steps").changed() {
                hw.atten_step_db = on.then_some(0.5);
            }
            if let Some(s) = hw.atten_step_db.as_mut() {
                num(ui, s, 0.05..=6.0, 0.05, " dB");
            }
        });
        ui.end_row();
        if !solved {
            ui.label("Subarrays");
            ui.horizontal(|ui| {
                let mut on = hw.subarray.is_some();
                if ui.checkbox(&mut on, "one converter per").on_hover_text("Elements within a subarray share an analog phase-shifter network, and the digital part steers between them.").changed() {
                    hw.subarray = on.then_some((4, 4));
                    if !on && hw.delay == Delay::Subarray {
                        hw.delay = Delay::PhaseOnly;
                    }
                }
                if let Some((x, y)) = hw.subarray.as_mut() {
                    int(ui, x, 1..=64);
                    ui.label("×");
                    int(ui, y, 1..=64);
                }
            });
            ui.end_row();
        }
        if hw.subarray.is_some() && !solved {
            ui.label("Analog part aims");
            ui.horizontal(|ui| {
                let mut on = hw.analog_aim.is_some();
                if ui.checkbox(&mut on, "elsewhere").on_hover_text("Off: at the beam. On: digital beams move around this aim, and quantisation lobes grow as they leave it.").changed() {
                    hw.analog_aim = on.then_some((0.0, 0.0));
                }
                if let Some((t, p)) = hw.analog_aim.as_mut() {
                    num(ui, t, 0.0..=90.0, 0.5, "° θ");
                    num(ui, p, -360.0..=360.0, 0.5, "° φ");
                }
            });
            ui.end_row();
        }
        ui.label("Random errors");
        ui.horizontal(|ui| {
            num(ui, &mut hw.amp_err_db, 0.0..=6.0, 0.05, " dB");
            num(ui, &mut hw.phase_err_deg, 0.0..=45.0, 0.5, "°");
        });
        ui.end_row();
        if !flat && !solved {
            ui.label("Lit within");
            num(ui, &mut plan.cone_deg, 10.0..=180.0, 1.0, "° of the beam").on_hover_text("On an array whose elements face different ways, those turned further than this from the beam stay dark.");
            ui.end_row();
        }
    });
}

/// Where each element goes on a flat picture of the array, and which way that reads.
fn flatten(elements: &[Element]) -> (Vec<(f32, f32)>, &'static str) {
    let z = elements.iter().map(|e| e.pos[2]).fold((f64::MAX, f64::MIN), |a, b| (a.0.min(b), a.1.max(b)));
    let normals_differ = elements.windows(2).any(|w| (0..3).any(|k| (w[0].normal[k] - w[1].normal[k]).abs() > 1e-6));
    if normals_differ && z.1 - z.0 > 1e-9 {
        // Round the outside, unrolled.
        let radius = elements.iter().map(|e| (e.pos[0].powi(2) + e.pos[1].powi(2)).sqrt()).fold(0.0, f64::max).max(1e-6);
        (elements.iter().map(|e| ((e.pos[1].atan2(e.pos[0]) * radius) as f32, e.pos[2] as f32)).collect(), "unrolled round the outside")
    } else {
        (elements.iter().map(|e| (e.pos[0] as f32, e.pos[1] as f32)).collect(), "from above")
    }
}

/// The elements, coloured by phase and sized by amplitude. A click takes one out or puts it back.
fn layout_plot(ui: &mut egui::Ui, elements: &[Element], weights: &[C64], failed: &mut Vec<usize>) {
    let (points, reads) = flatten(elements);
    let h = 210.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let inner = egui::Rect::from_min_max(rect.min + vec2(16.0, 28.0), rect.max - vec2(16.0, 26.0));
    let (x0, x1, y0, y1) = points.iter().fold((f32::MAX, f32::MIN, f32::MAX, f32::MIN), |a, b| (a.0.min(b.0), a.1.max(b.0), a.2.min(b.1), a.3.max(b.1)));
    let span = (x1 - x0).max(y1 - y0).max(1e-6);
    let scale = (inner.width() / span).min(inner.height() / span);
    let mid = pos2((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let to = |q: (f32, f32)| inner.center() + vec2((q.0 - mid.x) * scale, -(q.1 - mid.y) * scale);
    let top = weights.iter().map(|w| w.norm()).fold(0.0, f64::max).max(1e-12);
    let pitch = (scale * span / (points.len() as f32).sqrt().max(1.0)).clamp(3.0, 16.0);
    let mut hovered = None;
    let mouse = resp.hover_pos();
    for (i, q) in points.iter().enumerate() {
        let at = to(*q);
        let w = weights.get(i).copied().unwrap_or_default();
        let amp = (w.norm() / top) as f32;
        let down = failed.contains(&i);
        let r = (pitch * 0.42).max(1.5);
        if down {
            let c = theme::DANGER;
            p.line_segment([at - vec2(r, r), at + vec2(r, r)], Stroke::new(1.5, c));
            p.line_segment([at - vec2(r, -r), at + vec2(r, -r)], Stroke::new(1.5, c));
        } else if amp <= 0.0 {
            p.circle_stroke(at, r, Stroke::new(1.0, theme::STROKE_STRONG));
        } else {
            let hue = (w.arg() / std::f64::consts::TAU).rem_euclid(1.0) as f32;
            p.circle_filled(at, (r * amp.sqrt()).max(1.5), egui::ecolor::Hsva::new(hue, 0.8, 0.8, 1.0));
        }
        if let Some(m) = mouse
            && (m - at).length() < r.max(5.0)
        {
            hovered = Some(i);
        }
    }
    p.text(rect.left_top() + vec2(8.0, 5.0), Align2::LEFT_TOP, format!("{} elements, {reads}. Colour is phase, size is amplitude.", elements.len()), FontId::proportional(11.0), theme::WEAK);
    if let Some(i) = hovered {
        let w = weights.get(i).copied().unwrap_or_default();
        p.text(rect.left_bottom() + vec2(8.0, -5.0), Align2::LEFT_BOTTOM, format!("Element {}: {:.1} dB, {:.1}°, panel {}. Click to fail or restore.", i + 1, 20.0 * w.norm().max(1e-9).log10(), w.arg().to_degrees(), elements[i].panel + 1), FontId::proportional(11.0), theme::TEXT);
        if resp.clicked() {
            match failed.iter().position(|f| *f == i) {
                Some(k) => {
                    failed.remove(k);
                }
                None => failed.push(i),
            }
        }
    }
}

/// A dB map as a picture, redrawn only when the data behind it changes.
#[allow(clippy::too_many_arguments)]
fn heat_map(ui: &mut egui::Ui, slot: &mut Picture, key: usize, name: &str, cols: usize, rows: usize, size: egui::Vec2, value: impl Fn(usize, usize) -> f64) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if cols == 0 || rows == 0 {
        return rect;
    }
    if slot.as_ref().is_none_or(|s| s.0 != key) {
        let mut rgba = Vec::with_capacity(cols * rows * 4);
        for r in (0..rows).rev() {
            for c in 0..cols {
                let v = value(c, r);
                if v.is_finite() {
                    let col = charts::heat(v as f32);
                    rgba.extend([col.r(), col.g(), col.b(), 255]);
                } else {
                    rgba.extend([0, 0, 0, 0]);
                }
            }
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([cols, rows], &rgba);
        *slot = Some((key, ui.ctx().load_texture(name, image, egui::TextureOptions::LINEAR)));
    }
    if let Some((_, t)) = slot.as_ref() {
        ui.painter().image(t.id(), rect, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    rect
}

fn fold_cut(cut: &[(f64, f64)], peak: f64) -> Vec<(f64, f64)> {
    cut.iter().map(|(t, v)| (*t, v.max(peak - 60.0))).collect()
}

/// What the Phased array page shows on the canvas.
pub fn canvas_far(st: &mut Studio) -> Option<FarField> {
    if st.ui.page != Page::Phased {
        return None;
    }
    if st.project.phased.use_ports {
        return st.ui.phased.solved.latest().map(|v| v.far.clone());
    }
    st.ui.phased.beam.latest().and_then(|v| v.as_ref().as_ref().ok().map(|v| v.far.clone()))
}

/// On the Phased array page with a layout rather than a model, small plates show the elements.
pub fn canvas_model(st: &mut Studio) -> Option<Arc<(Geometry, Vec<view3d::Tessellated>)>> {
    if st.ui.page != Page::Phased || st.project.phased.use_ports || antenna_core::phased::beam::too_big(&st.project.phased.layout) {
        return None;
    }
    let (layout, f0) = (st.project.phased.layout.clone(), design_f0(&st.project));
    if let Some((l, f, m)) = &st.ui.phased.markers
        && *l == layout
        && *f == f0.to_bits()
    {
        return Some(m.clone());
    }
    let g = marker_model(&layout.elements(f0), f0);
    let tess = g.solids.iter().map(|s| tessellate(&s.shape)).collect();
    let made = Arc::new((g, tess));
    st.ui.phased.markers = Some((layout, f0.to_bits(), made.clone()));
    Some(made)
}

fn scan_series(rows: &[antenna_core::phased::solved::ScanRow], pick: impl Fn(&antenna_core::phased::solved::ScanRow) -> f64) -> Vec<(f64, f64)> {
    rows.iter().map(|r| (r.theta, pick(r))).collect()
}

fn solved_tiles(ui: &mut egui::Ui, v: &SolvedView) {
    let good_match = v.row.tarc_db < -10.0;
    let worst = format!("worst port, {}", v.names.get(v.row.worst_port).map_or("?", String::as_str));
    tiles(
        ui,
        &[
            (format!("{:.2} dBi", v.row.realized_dbi), "realized gain", theme::SUCCESS),
            (format!("{:.2} dBi", v.row.limit_dbi), "most any weights could", theme::TEXT),
            (format!("{:.1}°", v.row.pointing_error_deg), "pointing error", theme::TEXT),
            (format!("{:.1} dB", v.row.tarc_db), "total active reflection", if good_match { theme::TEXT } else { theme::WARNING }),
            (format!("{:.1} dB", v.row.worst_active_db), &worst, theme::TEXT),
            (format!("{:.2} dB", v.row.mismatch_db), "lost to mismatch", theme::TEXT),
        ],
    );
}

fn solved_page(ui: &mut egui::Ui, st: &mut Studio, a: Arc<SolvedArray>) {
    let plan = st.project.phased.plan.clone();
    let key: SolvedKey = (Arc::as_ptr(&a) as usize, st.ui.far_k, plan.clone(), st.ui.phased.matched);
    let matched = st.ui.phased.matched;
    let job_array = a.clone();
    let job_plan = plan.clone();
    let (view, stale) = st.ui.phased.solved.get(&key, move || solved_view(&job_array, &job_plan, matched));
    if stale {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }
    let Some(v) = view else {
        ui.spinner();
        return;
    };
    let shown = plan.active.min(plan.beams.len().saturating_sub(1));
    ui.horizontal(|ui| {
        ui.checkbox(&mut st.ui.phased.matched, "Conjugate-matched weights").on_hover_text("Each port driven by the conjugate of its own solved field: no weights can realize more gain this way. Off: textbook phases from the port positions.");
        if ui.button(theme::label(icon::LIGHTNING, "Drive the ports with these")).on_hover_text("Sets the project's port drive, so the Far field and other result pages show this beam.").clicked() {
            st.project.drive = v.weights.iter().map(|w| antenna_core::project::Drive { amp: w.norm(), phase: w.arg().to_degrees() }).collect();
            st.ui.use_drive = true;
            st.ui.message = Some(("The ports now have this beam's drive: the Far field page shows it.".into(), true));
        }
    });
    if (v.balance - 1.0).abs() > 0.1 {
        verdict(ui, false, &format!("Radiated over accepted power is {:.2} for these weights. Loss in the model accounts for a drop; well above 1, the far-field grid may be too coarse for this array's size.", v.balance));
    }
    if v.blind.is_empty() {
        verdict(ui, true, "No scan blindness in the two planes shown, out to 70°.");
    } else {
        let list: Vec<String> = v.blind.iter().map(|b| format!("{:.0}° ({:.0} dB down)", b.0, b.1)).collect();
        verdict(ui, false, &format!("Gain falls away with almost all the power coming back at {}. That's scan blindness.", list.join(", ")));
    }
    let peak = v.row.realized_dbi.max(v.row.limit_dbi);
    charts::line_chart(
        ui,
        ChartOptions { title: "Pattern cuts, realized gain", unit: " dBi", x_unit: "θ°", height: 180.0, y_range: Some((peak - 40.0, peak + 1.0)), ..Default::default() },
        &[Series { name: &format!("φ = {:.0}°", v.cut_phis[0]), color: charts::ORANGE, points: &fold_cut(&v.cuts[0], peak) }, Series { name: &format!("φ = {:.0}°", v.cut_phis[1]), color: charts::BLUE, points: &fold_cut(&v.cuts[1], peak) }],
    );
    let (a1, b1) = (scan_series(&v.scan_a, |r| r.realized_dbi), scan_series(&v.scan_b, |r| r.realized_dbi));
    let (a2, b2) = (scan_series(&v.scan_a, |r| r.limit_dbi), scan_series(&v.scan_b, |r| r.limit_dbi));
    charts::line_chart(
        ui,
        ChartOptions { title: "Scanned, textbook phases", unit: " dBi", x_unit: "θ°", height: 170.0, marker: plan.beams.get(shown).map(|b| b.theta), ..Default::default() },
        &[Series { name: &format!("φ {:.0}°", v.cut_phis[0]), color: charts::ORANGE, points: &a1 }, Series { name: &format!("φ {:.0}°", v.cut_phis[1]), color: charts::BLUE, points: &b1 }, Series { name: "limit", color: charts::GREY, points: &a2 }, Series { name: "limit", color: charts::GREY, points: &b2 }],
    );
    let (ta, tb) = (scan_series(&v.scan_a, |r| r.tarc_db), scan_series(&v.scan_b, |r| r.tarc_db));
    charts::line_chart(ui, ChartOptions { title: "Total active reflection while scanning", unit: " dB", x_unit: "θ°", height: 140.0, limit: Some(-10.0), marker: plan.beams.get(shown).map(|b| b.theta), ..Default::default() }, &[Series { name: &format!("φ {:.0}°", v.cut_phis[0]), color: charts::ORANGE, points: &ta }, Series { name: &format!("φ {:.0}°", v.cut_phis[1]), color: charts::BLUE, points: &tb }]);
    let bars: Vec<(f64, f64)> = v.active_db.iter().enumerate().map(|(i, d)| ((i + 1) as f64, *d)).collect();
    charts::line_chart(ui, ChartOptions { title: "Active reflection at each port, this beam", unit: " dB", x_unit: "port", height: 110.0, limit: Some(-10.0), ..Default::default() }, &[Series { name: "Port", color: charts::GREEN, points: &bars }]);
    weak(ui, format!("Port {} in the middle has {:.2} dBi embedded realized gain at broadside. To size a much larger array of this lattice, choose a layout with the element set to its embedded pattern.", v.names.get(v.centre_port).map_or("?", String::as_str), v.centre_gain_dbi));
    if ui.button(theme::label(icon::EXPORT, "Save the beam table")).on_hover_text("Each port's amplitude, phase and shifter code for this beam").clicked()
        && let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("beam_table.csv").save_file()
    {
        let b = plan.beams.get(shown).copied().unwrap_or_default();
        let csv = beam::solved_csv(&v.pos, (b.theta, b.phi), &v.weights, v.f, plan.hw.phase_bits);
        st.ui.message = Some(match std::fs::write(&path, csv) {
            Ok(()) => (format!("Wrote {}", path.display()), true),
            Err(e) => (format!("Could not write it: {e}"), false),
        });
    }
    let elements: Vec<Element> = v.pos.iter().map(|p| Element { pos: *p, normal: [0.0, 0.0, 1.0], x_axis: [1.0, 0.0, 0.0], panel: 0, ix: 0, iy: 0, nx: 1, ny: 1 }).collect();
    let mut failed = st.project.phased.plan.hw.failed.clone();
    layout_plot(ui, &elements, &v.weights, &mut failed);
    if failed != st.project.phased.plan.hw.failed {
        st.project.phased.plan.hw.failed = failed;
    }
    if stale {
        weak(ui, "Updating…");
    }
    evidence_note(ui, true);
}

fn beam_tiles(ui: &mut egui::Ui, v: &BeamView) {
    let r = &v.reports[v.shown.min(v.reports.len().saturating_sub(1))];
    let band = v.band_hz.map_or("beyond ±20 %".to_string(), |b| format!("{:.1} %", 100.0 * b / v.f0));
    tiles(
        ui,
        &[
            (format!("{:.2} dBi", r.realized_dbi), "realized gain", theme::SUCCESS),
            (format!("{:.1} dBm", v.eirp_dbm), "EIRP", theme::SUCCESS),
            (format!("{:.2} dBi", r.directivity_dbi), "directivity", theme::TEXT),
            (if r.hpbw_a >= 359.0 { "–".into() } else { format!("{:.1}°", r.hpbw_a) }, "beam width, along scan", theme::TEXT),
            (if r.hpbw_b >= 359.0 { "–".into() } else { format!("{:.1}°", r.hpbw_b) }, "beam width, across", theme::TEXT),
            (r.sll_db.map_or("–".into(), |s| format!("{s:.1} dB")), "side lobes", theme::TEXT),
            (format!("{:.2}°", r.pointing_error_deg), "pointing error", if r.pointing_error_deg > r.hpbw_a.min(r.hpbw_b) / 4.0 { theme::WARNING } else { theme::TEXT }),
            (format!("{:.2} dB", r.taper_db), "taper loss", theme::TEXT),
            (band, "band at −3 dB", theme::TEXT),
        ],
    );
}

fn beam_results(ui: &mut egui::Ui, st: &mut Studio, d: &mut PhasedDesign, v: &BeamView, stale: bool) {
    let r = &v.reports[v.shown.min(v.reports.len().saturating_sub(1))];
    if !r.grating.is_empty() {
        let list: Vec<String> = r.grating.iter().map(|g| format!("θ {:.0}°, φ {:.0}° ({:.1} dB)", g.0, g.1, g.2)).collect();
        verdict(ui, false, &format!("Grating lobes as strong as the beam at {}. Closer elements, less scan, or the element's pattern would have to deal with them.", list.join("; ")));
    }
    if d.plan.multibeam == MultiBeam::Digital && d.plan.beams.len() > 1 {
        if v.load.peak_voltage > 1.05 {
            verdict(ui, false, &format!("When these beams' signals line up an element sees {:.1} times its single-beam voltage, {:.1} dB of peak an amplifier has to back off for.", v.load.peak_voltage, 20.0 * v.load.peak_voltage.log10()));
        }
    }
    if v.load.power_use < 0.999 {
        weak(ui, format!("The array uses {:.0} % of the power its amplifiers could give: that's a taper or a beam sum's cost.", v.load.power_use * 100.0));
    }
    if r.estimated {
        verdict(ui, false, "So many elements that the integral ran on a grid coarser than the beam asks for: gain and side lobes are good to a few tenths of a dB.");
    }
    if v.reports.len() > 1 {
        head(ui, "Each beam");
        egui::Grid::new("phased_each_beam").num_columns(6).spacing(vec2(12.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["Beam", "Asked", "Peak", "Realized at asked", "Width", "Side lobes"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for (k, x) in v.reports.iter().enumerate() {
                ui.label(format!("{}", k + 1));
                ui.label(format!("{:.0}°, {:.0}°", x.theta, x.phi));
                ui.label(format!("{:.1}°, {:.1}°", x.peak_theta, x.peak_phi));
                ui.label(format!("{:.1} dBi", x.at_request_dbi));
                ui.label(format!("{:.1}° × {:.1}°", x.hpbw_a, x.hpbw_b));
                ui.label(x.sll_db.map_or("–".to_string(), |s| format!("{s:.1} dB")));
                ui.end_row();
            }
        });
    }
    let peak = r.directivity_dbi;
    charts::line_chart(
        ui,
        ChartOptions { title: "Pattern cuts, directivity", unit: " dBi", x_unit: "θ°", height: 180.0, y_range: Some((peak - 50.0, peak + 1.0)), marker: Some(r.theta), ..Default::default() },
        &[Series { name: &format!("φ = {:.0}°", v.cut_phis[0]), color: charts::ORANGE, points: &fold_cut(&v.cuts[0], peak) }, Series { name: &format!("φ = {:.0}°", v.cut_phis[1]), color: charts::BLUE, points: &fold_cut(&v.cuts[1], peak) }],
    );
    // The map: a beam off the page's edge wouldn't show on a slice.
    let key = v as *const BeamView as usize;
    let (rows, cols) = (v.map.len(), v.map.first().map_or(0, Vec::len));
    let floor = -40.0;
    head(ui, if v.map_is_uv { "Upper half-space, in direction cosines" } else { "All round, azimuth across and elevation up" });
    let size = if v.map_is_uv { vec2(ui.available_width().min(300.0), ui.available_width().min(300.0)) } else { vec2(ui.available_width(), ui.available_width() * 0.5) };
    let rect = heat_map(ui, &mut st.ui.phased.map_picture, key, "phased_map", cols, rows, size, |c, row| {
        let x = v.map[row][c];
        if x.is_finite() { ((x - floor) / -floor).clamp(0.0, 1.0) } else { f64::NAN }
    });
    let painter = ui.painter_at(rect);
    for (i, b) in d.plan.beams.iter().enumerate() {
        let at = if v.map_is_uv {
            let (u, vv) = crate::antenna::phased::uv_of(b.theta, b.phi);
            rect.center() + vec2(u as f32 * rect.width() / 2.0, -vv as f32 * rect.height() / 2.0)
        } else {
            pos2(rect.left() + (b.phi.rem_euclid(360.0) / 360.0) as f32 * rect.width(), rect.bottom() - ((90.0 - b.theta + 90.0) / 180.0) as f32 * rect.height())
        };
        painter.circle_stroke(at, 7.0, Stroke::new(1.5, Color32::WHITE));
        painter.text(at + vec2(9.0, -9.0), Align2::LEFT_BOTTOM, format!("{}", i + 1), FontId::proportional(11.0), Color32::WHITE);
    }
    charts::heat_legend(ui, "−40 dB", "peak");
    if d.plan.beams.get(v.shown).is_some_and(|b| b.theta > 0.5) {
        let points: Vec<(f64, f64)> = v.band.iter().map(|p| (p.f / 1e9, p.rel_db)).collect();
        charts::line_chart(ui, ChartOptions { title: "Gain towards the beam against frequency, wavefront alone", unit: " dB", x_unit: "GHz", height: 130.0, limit: Some(-3.0), y_range: Some((-20.0, 1.0)), marker: Some(v.f0 / 1e9), ..Default::default() }, &[Series { name: d.plan.hw.delay.label(), color: charts::PURPLE, points: &points }]);
    }
    layout_plot(ui, &v.elements, &v.weights, &mut d.plan.hw.failed);
    ui.horizontal(|ui| {
        if ui.button(theme::label(icon::EXPORT, "Save the beam table")).on_hover_text("For each beam and element: amplitude, phase, delay-line setting, what's left for the shifter and its code").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("beam_table.csv").save_file()
        {
            let csv = beam::weights_csv(&v.model, &d.plan, v.f0, d.layout.is_flat());
            st.ui.message = Some(match std::fs::write(&path, csv) {
                Ok(()) => (format!("Wrote {}", path.display()), true),
                Err(e) => (format!("Could not write it: {e}"), false),
            });
        }
        if ui.button(theme::label(icon::SQUARES_FOUR, "Fill a codebook")).on_hover_text("Beams on a hexagonal grid over a 60° cone, crossing at 3 dB").clicked() {
            let list = beam::codebook(&v.model, v.f0, 60.0, -3.0);
            if list.len() <= 256 {
                d.plan.beams = list.into_iter().map(|(theta, phi)| Beam { theta, phi, share: 1.0 }).collect();
                d.plan.active = 0;
            } else {
                st.ui.message = Some((format!("That would be {} beams, more than the page can show. The beam table can hold them: ask the assistant for a sparser cone or a smaller array.", list.len()), false));
            }
        }
        if let Layout::Line { n, spacing } = d.layout
            && n.is_power_of_two()
            && n > 1
            && ui.button(theme::label(icon::SHARE_NETWORK, "Butler beams")).on_hover_text("The line's orthogonal beams, as a Butler matrix makes them. Set the mode to one network for them all at once.").clicked()
        {
            d.plan.beams = butler_beams(n, spacing);
            d.plan.active = 0;
        }
    });
    if stale {
        weak(ui, "Updating…");
    }
    evidence_note(ui, false);
}

fn shaped_ui(ui: &mut egui::Ui, st: &mut Studio) {
    egui::CollapsingHeader::new("Shape a beam to a mask").id_salt("phased_shape").show(ui, |ui| {
        weak(ui, "A flat-topped sector with a side-lobe ceiling, in the plane at φ, by alternating projection.");
        let u = &mut st.ui.phased;
        if u.shape_top == (0.0, 0.0) {
            u.shape_top = (-20.0, 20.0);
            u.shape_top_ripple = 1.0;
            u.shape_guard = 8.0;
            u.shape_sll = -25.0;
        }
        egui::Grid::new("phased_shape_grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Plane φ");
            num(ui, &mut u.shape_phi, -360.0..=360.0, 0.5, "°");
            ui.end_row();
            ui.label("Top from, to");
            ui.horizontal(|ui| {
                num(ui, &mut u.shape_top.0, -85.0..=85.0, 0.5, "°");
                num(ui, &mut u.shape_top.1, -85.0..=85.0, 0.5, "°");
            });
            ui.end_row();
            ui.label("Ripple allowed");
            num(ui, &mut u.shape_top_ripple, 0.1..=6.0, 0.05, " dB");
            ui.end_row();
            ui.label("Slope width");
            num(ui, &mut u.shape_guard, 1.0..=40.0, 0.25, "°");
            ui.end_row();
            ui.label("Side-lobe ceiling");
            num(ui, &mut u.shape_sll, -60.0..=-3.0, 0.5, " dB");
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut u.shape_phase_only, "Phase only");
            ui.end_row();
        });
        let busy = st.busy();
        if ui.add_enabled(!busy, egui::Button::new("Fit the weights")).clicked() {
            let u = &st.ui.phased;
            let (lo, hi) = (u.shape_top.0.min(u.shape_top.1), u.shape_top.0.max(u.shape_top.1));
            let mask = vec![
                MaskRegion { theta_lo: lo, theta_hi: hi, lo_db: Some(-u.shape_top_ripple), hi_db: 0.0 },
                MaskRegion { theta_lo: -90.0, theta_hi: lo - u.shape_guard, lo_db: None, hi_db: u.shape_sll },
                MaskRegion { theta_lo: hi + u.shape_guard, theta_hi: 90.0, lo_db: None, hi_db: u.shape_sll },
            ];
            let (phi, phase_only) = (u.shape_phi, u.shape_phase_only);
            st.run_shaped(phi, mask, phase_only);
        }
        if let Some(s) = st.phased_jobs().shaped {
            let good = s.worst_db < 1.0;
            verdict(ui, good, &format!("{} iterations; the worst miss of the mask is {:.2} dB, and the taper costs {:.2} dB.", s.iterations, s.worst_db, s.taper_db));
            charts::line_chart(ui, ChartOptions { title: "Shaped pattern", unit: " dB", x_unit: "θ°", height: 150.0, y_range: Some((-50.0, 1.0)), ..Default::default() }, &[Series { name: "Pattern", color: charts::PURPLE, points: &s.cut }]);
        }
    });
}

/// A project that asks for its solved ports, and has none to read yet.
fn unsolved_array(ui: &mut egui::Ui, st: &mut Studio, use_layout: impl FnOnce(&mut PhasedDesign)) {
    if st.busy() {
        weak(ui, "This array is being solved, every port of it. Its beam and its coupling will show here when that's done.");
        return;
    }
    let ports = st.project.geometry().map_or(0, |g| g.ports.len());
    if ports < 2 {
        verdict(ui, false, &format!("This model has {ports} port, and a solved array needs one per element. An array template has that, or a layout needs no model."));
    } else if st.latest().is_some() {
        verdict(ui, false, "The model has changed since its last solve, so its fields and its port positions no longer agree. Run it again to read the array.");
    } else {
        weak(ui, format!("This model has {ports} ports, one per element. One run solves them all, and then scanning needs no other."));
    }
    if ports >= 2 && run_button(ui, "Solve the array", true) {
        st.run();
    }
    if ui.button("Use a layout instead").clicked() {
        use_layout(&mut st.project.phased);
    }
}

/// The Phased array page.
pub fn phased_page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Phased array");
    weak(ui, "Any layout, several beams at once, nulls, finite-bit hardware, subarrays and time delay. Start from a layout here, or solve a model with a port per element to bring in its real coupling.");
    let array = array_of(st);
    let has_result = st.latest().is_some();
    let mut d = st.project.phased.clone();
    let project_f0 = st.project.f0();
    if d.use_ports && array.is_none() {
        unsolved_array(ui, st, |d| d.use_ports = false);
        return;
    }
    // What's being tuned, up here where a drag further down can still see it.
    if d.use_ports {
        if let Some(v) = st.ui.phased.solved.latest() {
            solved_tiles(ui, &v);
        }
    } else if let Some(v) = st.ui.phased.beam.latest()
        && let Ok(v) = v.as_ref()
    {
        beam_tiles(ui, v);
    }
    group(ui, |ui| {
        head(ui, "Array");
        if let Some(a) = &array {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut d.use_ports, true, format!("The solved model, {} ports", a.ports()));
                ui.selectable_value(&mut d.use_ports, false, "A layout");
            });
        } else {
            weak(ui, "A solved model with a port per element brings in its real coupling.");
            if ui.small_button(theme::label(icon::SQUARES_FOUR, "Array templates")).clicked() {
                st.ui.gallery_cat = Some(antenna_core::templates::Category::Array);
                st.ui.page = Page::Gallery;
            }
        }
        if !d.use_ports {
            ui.horizontal(|ui| {
                ui.label("Frequency");
                let mut f = if d.f0 > 0.0 { d.f0 } else { project_f0 };
                if ghz_field(ui, &mut f).changed() {
                    d.f0 = f;
                }
            });
            layout_ui(ui, "phased", &mut d.layout);
            ui.horizontal(|ui| {
                ui.label("Element");
                element_ui(ui, &mut d.element, has_result, array.is_some());
            });
            ui.horizontal(|ui| {
                ui.label("Each element at full drive");
                num(ui, &mut d.tx_dbm, -30.0..=60.0, 0.25, " dBm");
            });
        }
    });
    group(ui, |ui| {
        head(ui, "Beams");
        beams_ui(ui, &mut d.plan);
    });
    group(ui, |ui| {
        head(ui, "Beamformer");
        feed_ui(ui, &mut d.plan, d.layout.is_flat(), d.use_ports && array.is_some());
    });
    if d != st.project.phased {
        st.project.phased = d.clone();
    }
    if d.use_ports
        && let Some(a) = array
    {
        solved_page(ui, st, a);
        return;
    }
    let f0 = design_f0(&st.project);
    let latest = st.latest();
    let key: BeamKey = (f0, d.layout.clone(), d.element, d.plan.clone(), d.tx_dbm, latest.as_ref().map_or(0, |r| Arc::as_ptr(r) as usize));
    let (job, job_array, far_k) = (d.clone(), array.clone(), st.ui.far_k);
    let (view, stale) = st.ui.phased.beam.get(&key, move || {
        let element = if matches!(job.element, ElementKind::Solved { .. }) { solved_element(job_array.as_deref(), latest.as_deref(), far_k) } else { None };
        beam_view(&job, f0, element.as_ref())
    });
    if stale {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }
    match view.as_ref().map(|v| v.as_ref()) {
        Some(Ok(v)) => beam_results(ui, st, &mut d, v, stale),
        Some(Err(e)) => verdict(ui, false, e),
        None => {
            ui.spinner();
        }
    }
    if d != st.project.phased {
        st.project.phased = d;
    }
    shaped_ui(ui, st);
}

fn method_ui(ui: &mut egui::Ui, m: &mut Method, unavailable: &[(Method, &'static str)]) {
    egui::ComboBox::from_id_salt("df_method").selected_text(m.label()).show_ui(ui, |ui| {
        for option in Method::ALL {
            let why = unavailable.iter().find(|u| u.0 == option).map(|u| u.1);
            let r = ui.add_enabled(why.is_none(), egui::Button::selectable(*m == option, option.label()));
            if let Some(why) = why {
                r.on_disabled_hover_text(format!("It {why}."));
            } else if r.clicked() {
                *m = option;
            }
        }
    });
}

fn scan_ui(ui: &mut egui::Ui, scan: &mut Scan, step: &mut f64) {
    #[derive(Clone, Copy, PartialEq)]
    enum K {
        Plane,
        Azimuth,
        Hemisphere,
    }
    let mut k = match scan {
        Scan::Plane { .. } => K::Plane,
        Scan::Azimuth { .. } => K::Azimuth,
        Scan::Hemisphere { .. } => K::Hemisphere,
    };
    if theme::segmented(ui, &mut k, &[(K::Plane, "A plane"), (K::Azimuth, "All round"), (K::Hemisphere, "Half-space")]) {
        *scan = match k {
            K::Plane => Scan::Plane { phi: 0.0 },
            K::Azimuth => Scan::Azimuth { theta: 90.0 },
            K::Hemisphere => Scan::Hemisphere { theta_max: 80.0 },
        };
    }
    ui.horizontal(|ui| match scan {
        Scan::Plane { phi } => {
            num(ui, phi, -360.0..=360.0, 1.0, "° φ");
            num(ui, step, 0.1..=5.0, 0.05, "° step");
        }
        Scan::Azimuth { theta } => {
            num(ui, theta, 1.0..=179.0, 1.0, "° θ");
            num(ui, step, 0.1..=5.0, 0.05, "° step");
        }
        Scan::Hemisphere { theta_max } => {
            num(ui, theta_max, 10.0..=90.0, 1.0, "° out");
            num(ui, step, 2.0..=10.0, 0.25, "° step");
        }
    });
}

fn sources_ui(ui: &mut egui::Ui, sources: &mut Vec<Source>, scan: Scan) {
    egui::Grid::new("df_sources").num_columns(5).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
        let plane = matches!(scan, Scan::Plane { .. });
        for h in [if plane { "Angle" } else { "θ" }, if plane { "" } else { "φ" }, "Power", "Echo of", ""] {
            ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
        }
        ui.end_row();
        let mut remove = None;
        let count = sources.len();
        for k in 0..count {
            let earlier = k;
            let s = &mut sources[k];
            match scan {
                Scan::Plane { phi } => {
                    // Off to one side or the other of the plane's broadside, as one signed angle.
                    let mut angle = doa::signed_plane_angle((s.theta, s.phi), phi);
                    if num(ui, &mut angle, -90.0..=90.0, 0.25, "°").changed() {
                        s.theta = angle.abs();
                        s.phi = if angle < 0.0 { phi + 180.0 } else { phi };
                    }
                    ui.label("");
                }
                Scan::Azimuth { theta } => {
                    s.theta = theta;
                    ui.label(format!("{theta:.0}°"));
                    num(ui, &mut s.phi, -360.0..=360.0, 0.5, "°");
                }
                Scan::Hemisphere { .. } => {
                    num(ui, &mut s.theta, 0.0..=90.0, 0.25, "°");
                    num(ui, &mut s.phi, -360.0..=360.0, 0.5, "°");
                }
            }
            num(ui, &mut s.power_db, -40.0..=40.0, 0.25, " dB");
            let mut echo = s.coherent_with.is_some();
            ui.add_enabled_ui(earlier > 0, |ui| {
                if ui.checkbox(&mut echo, if earlier > 0 { "1" } else { "–" }).on_hover_text("The same waveform as source 1 from another direction: a reflection.").changed() {
                    s.coherent_with = echo.then_some(0);
                }
            });
            if count > 1 && ui.small_button(icon::X).clicked() {
                remove = Some(k);
            }
            ui.end_row();
        }
        if let Some(k) = remove {
            sources.remove(k);
            // Echoes all copy the first source, so they go with it.
            if k == 0 {
                for s in sources.iter_mut() {
                    s.coherent_with = None;
                }
            }
        }
    });
    if ui.button(theme::label(icon::PLUS, "Source")).clicked() {
        let last = sources.last().copied().unwrap_or_default();
        sources.push(Source { theta: (last.theta + 15.0).min(85.0), phi: last.phi, ..Default::default() });
    }
}

/// A chart with log y, as error against SNR wants: `(name, colour, points)`.
fn log_chart(ui: &mut egui::Ui, title: &str, x_unit: &str, y_unit: &str, curves: &[(&str, Color32, Vec<(f64, f64)>)]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 210.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let f = FontId::proportional(10.5);
    p.text(rect.left_top() + vec2(8.0, 5.0), Align2::LEFT_TOP, title, FontId::proportional(12.0), theme::TEXT);
    let finite: Vec<(f64, f64)> = curves.iter().flat_map(|c| c.2.iter().copied()).filter(|v| v.0.is_finite() && v.1.is_finite() && v.1 > 0.0).collect();
    if finite.len() < 2 {
        p.text(rect.center(), Align2::CENTER_CENTER, "No data yet", f, theme::WEAK);
        return;
    }
    let (x0, x1) = finite.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(v.0), a.1.max(v.0)));
    let (y0, y1) = finite.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(v.1.log10()), a.1.max(v.1.log10())));
    let (y0, y1) = (y0.floor(), y1.ceil().max(y0.floor() + 1.0));
    let plot = egui::Rect::from_min_max(rect.min + vec2(58.0, 24.0), rect.max - vec2(10.0, 20.0));
    let to = |x: f64, y: f64| pos2(plot.left() + ((x - x0) / (x1 - x0).max(1e-9)) as f32 * plot.width(), plot.bottom() - ((y - y0) / (y1 - y0)) as f32 * plot.height());
    let grid = Stroke::new(1.0, theme::STROKE);
    let mut d = y0;
    while d <= y1 {
        let at = to(x0, d);
        p.hline(plot.left()..=plot.right(), at.y, grid);
        let text = if d >= 0.0 { format!("{}", 10f64.powf(d) as i64) } else { format!("{}", 10f64.powf(d)) };
        p.text(pos2(plot.left() - 4.0, at.y), Align2::RIGHT_CENTER, format!("{text}{y_unit}"), f.clone(), theme::WEAK);
        d += 1.0;
    }
    let mut x = x0;
    let dx = ((x1 - x0) / 8.0).max(1.0).ceil();
    while x <= x1 + 1e-9 {
        let at = to(x, y0);
        p.vline(at.x, plot.top()..=plot.bottom(), grid);
        p.text(pos2(at.x, plot.bottom() + 3.0), Align2::CENTER_TOP, format!("{x:.0}"), f.clone(), theme::WEAK);
        x += dx;
    }
    p.text(pos2(plot.right(), rect.bottom() - 2.0), Align2::RIGHT_BOTTOM, x_unit, f.clone(), theme::WEAK);
    let clip = p.with_clip_rect(plot);
    let mut lx = rect.right() - 8.0;
    for (name, col, pts) in curves.iter().rev() {
        let line: Vec<egui::Pos2> = pts.iter().filter(|v| v.1 > 0.0 && v.1.is_finite()).map(|v| to(v.0, v.1.log10())).collect();
        if line.len() >= 2 {
            clip.add(egui::Shape::line(line.clone(), Stroke::new(1.6, *col)));
        }
        for q in line {
            clip.circle_filled(q, 2.5, *col);
        }
        let g = p.layout_no_wrap(name.to_string(), f.clone(), *col);
        lx -= g.size().x;
        p.galley(pos2(lx, rect.top() + 6.0), g, *col);
        lx -= 10.0;
    }
}

fn spectrum_view(ui: &mut egui::Ui, st: &mut Studio, s: &Spectrum, v: &DfView, found: &[(f64, f64)]) {
    let floor = -50.0;
    let db = |x: f64| (10.0 * x.max(1e-12).log10()).max(floor);
    match v.scan {
        Scan::Plane { .. } | Scan::Azimuth { .. } => {
            let along_theta = matches!(v.scan, Scan::Plane { .. });
            let points: Vec<(f64, f64)> = if along_theta { s.theta.iter().zip(&s.p).map(|(t, row)| (*t, db(row[0]))).collect() } else { s.phi.iter().zip(&s.p[0]).map(|(t, x)| (*t, db(*x))).collect() };
            let plane = if let Scan::Plane { phi } = v.scan { phi } else { 0.0 };
            let truth: Vec<f64> = v.truth.iter().map(|x| if along_theta { doa::signed_plane_angle((x.theta, x.phi), plane) } else { x.phi.rem_euclid(360.0) }).collect();
            let marks: Vec<Vec<(f64, f64)>> = truth.iter().map(|x| vec![(*x, floor), (*x, 0.0)]).collect();
            let mut series = vec![Series { name: "Spectrum", color: charts::BLUE, points: &points }];
            for m in &marks {
                series.push(Series { name: "", color: charts::GREEN, points: m });
            }
            charts::line_chart(ui, ChartOptions { title: "Spatial spectrum; green marks the true sources", unit: " dB", x_unit: if along_theta { "angle°" } else { "φ°" }, height: 200.0, y_range: Some((floor, 2.0)), ..Default::default() }, &series);
        }
        Scan::Hemisphere { .. } => {
            head(ui, "Spatial spectrum: φ across, θ up");
            let (rows, cols) = (s.p.len(), s.phi.len());
            let key = s as *const Spectrum as usize;
            let rect = heat_map(ui, &mut st.ui.phased.df_picture, key, "df_map", cols, rows, vec2(ui.available_width(), ui.available_width() * 0.42), |c, r| ((db(s.p[r][c]) - floor) / -floor).clamp(0.0, 1.0));
            let painter = ui.painter_at(rect);
            let top = s.theta.last().copied().unwrap_or(90.0).max(1.0);
            let place = |theta: f64, phi: f64| pos2(rect.left() + (phi.rem_euclid(360.0) / 360.0) as f32 * rect.width(), rect.bottom() - (theta / top) as f32 * rect.height());
            for x in &v.truth {
                painter.circle_stroke(place(x.theta, x.phi), 6.0, Stroke::new(1.5, Color32::WHITE));
            }
            for d in found {
                let at = place(d.0, d.1);
                painter.line_segment([at - vec2(4.0, 0.0), at + vec2(4.0, 0.0)], Stroke::new(1.5, Color32::BLACK));
                painter.line_segment([at - vec2(0.0, 4.0), at + vec2(0.0, 4.0)], Stroke::new(1.5, Color32::BLACK));
            }
            weak(ui, "White rings are the truth, black crosses what was found.");
            charts::heat_legend(ui, "−50 dB", "peak");
        }
    }
}

fn df_tiles(ui: &mut egui::Ui, v: &DfView) {
    let Ok(r) = &v.result else { return };
    let truth: Vec<(f64, f64)> = v.truth.iter().map(|s| (s.theta, s.phi)).collect();
    // A source that was missed is the worst error there is, not one to leave out.
    let pairs = doa::pair_up(&truth, &r.dirs);
    let missed = pairs.iter().filter(|e| e.is_none()).count();
    let worst = pairs.iter().flatten().copied().fold(f64::MIN, f64::max);
    let bound = if v.crb.is_empty() { f64::NAN } else { v.crb.iter().map(|x| x * x).sum::<f64>().sqrt() / (v.crb.len() as f64).sqrt() };
    let error = if missed > 0 {
        (format!("{missed} missed"), theme::WARNING)
    } else if worst >= 0.0 {
        (format!("{worst:.3}°"), theme::SUCCESS)
    } else {
        ("–".to_string(), theme::TEXT)
    };
    tiles(
        ui,
        &[
            (error.0, "worst error, this run", error.1),
            (if bound.is_finite() { format!("{bound:.3}°") } else { "–".to_string() }, "the bound (rms)", theme::TEXT),
            (format!("{:.1}°", v.rayleigh), "beam's limit", theme::TEXT),
            (format!("{}", r.counted.mdl), "sources by MDL", theme::TEXT),
            (format!("{}", r.counted.aic), "by AIC", theme::TEXT),
            (format!("{}", v.channels), "channels", theme::TEXT),
        ],
    );
}

fn df_results_ui(ui: &mut egui::Ui, st: &mut Studio, v: &DfView, simulated: bool) {
    let truth: Vec<(f64, f64)> = v.truth.iter().map(|s| (s.theta, s.phi)).collect();
    match &v.result {
        Ok(r) => {
            if v.ambiguity.level > 0.9 {
                verdict(ui, false, &format!("This array can't tell θ {:.0}°, φ {:.0}° from θ {:.0}°, φ {:.0}°: their responses match to {:.0} %. Closer elements, or a different scan, would settle it.", v.ambiguity.a.0, v.ambiguity.a.1, v.ambiguity.b.0, v.ambiguity.b.1, 100.0 * v.ambiguity.level));
            }
            for note in &r.notes {
                verdict(ui, false, note);
            }
            if r.method.single_source() && v.truth.len() > 1 {
                verdict(ui, false, &format!("{} reports one direction: it reads the strongest wave and assumes it's alone, so the others bias it.", r.method.label()));
            }
            if let Some(s) = &r.spectrum {
                spectrum_view(ui, st, s, v, &r.dirs);
            }
            if !r.dirs.is_empty() {
                head(ui, "Found");
                egui::Grid::new("df_found").num_columns(4).spacing(vec2(14.0, 3.0)).striped(true).show(ui, |ui| {
                    for h in ["θ", "φ", "Nearest true source", "Off by"] {
                        ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
                    }
                    ui.end_row();
                    for d in &r.dirs {
                        ui.label(format!("{:.2}°", d.0));
                        ui.label(format!("{:.2}°", d.1));
                        let nearest = truth.iter().enumerate().map(|(k, t)| (k, angle_between(*t, *d))).min_by(|a, b| a.1.total_cmp(&b.1));
                        match nearest {
                            Some((k, e)) => {
                                ui.label(format!("{}", k + 1));
                                ui.label(format!("{e:.3}°"));
                            }
                            None => {
                                ui.label("–");
                                ui.label("–");
                            }
                        }
                        ui.end_row();
                    }
                });
            }
            let eigen: Vec<(f64, f64)> = r.eigen_db.iter().enumerate().map(|(i, x)| ((i + 1) as f64, *x)).collect();
            if eigen.len() > 1 {
                charts::line_chart(ui, ChartOptions { title: "Covariance eigenvalues: a step down from the sources to the noise", unit: " dB", x_unit: "index", height: 120.0, ..Default::default() }, &[Series { name: "Eigenvalue", color: charts::TEAL, points: &eigen }]);
            }
        }
        Err(e) => verdict(ui, false, e),
    }
    evidence_note(ui, simulated);
}

/// The Direction finding page.
pub fn df_page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Direction finding");
    weak(ui, "Where the sources are, from what each channel receives: MUSIC, Root-MUSIC, ESPRIT, Capon, sparse coarrays, monopulse, phase interferometers, Watson-Watt and pseudo-Doppler, each read against the Cramér-Rao bound.");
    let array = array_of(st);
    let f0 = design_f0(&st.project);
    let mut df = st.project.phased.df.clone();
    if df.use_ports && array.is_none() {
        unsolved_array(ui, st, |d| d.df.use_ports = false);
        return;
    }
    let solved_key = array.as_ref().filter(|_| df.use_ports).map_or(0, |a| Arc::as_ptr(a) as usize);
    let key: DfKey = (df.clone(), f0, solved_key);
    let (job_df, job_array) = (df.clone(), array.clone());
    let (view, stale) = st.ui.phased.df.get(&key, move || df_view(&job_df, f0, job_array.as_ref()));
    if stale {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }
    let unavailable: Vec<(Method, &'static str)> = view.as_ref().map_or(Vec::new(), |v| v.unavailable.clone());
    if let Some(v) = &view {
        df_tiles(ui, v);
    }
    group(ui, |ui| {
        head(ui, "Array");
        if let Some(a) = &array {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut df.use_ports, true, format!("The solved model, {} ports", a.ports()));
                ui.selectable_value(&mut df.use_ports, false, "A layout");
            });
            if df.use_ports {
                ui.checkbox(&mut df.calibrated, "Calibrated from the solve").on_hover_text("On: the estimator's table is the solved array itself, as a table measured on a range would be. Off: it believes the textbook phases of the port positions, which shows what coupling and the mount cost.");
            }
        } else {
            weak(ui, "Solve an array template with a port per element to read its real coupling here.");
        }
        if !df.use_ports {
            layout_ui(ui, "df", &mut df.layout);
        }
    });
    group(ui, |ui| {
        head(ui, "What's out there");
        scan_ui(ui, &mut df.scan, &mut df.step);
        sources_ui(ui, &mut df.sources, df.scan);
        egui::Grid::new("df_conditions").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("SNR, each channel");
            num(ui, &mut df.snr_db, -30.0..=60.0, 0.25, " dB");
            ui.end_row();
            ui.label("Snapshots");
            int(ui, &mut df.snapshots, 1..=100_000);
            ui.end_row();
            ui.label("Channel errors");
            ui.horizontal(|ui| {
                num(ui, &mut df.impair.gain_db, 0.0..=6.0, 0.05, " dB");
                num(ui, &mut df.impair.phase_deg, 0.0..=45.0, 0.5, "°");
            });
            ui.end_row();
            ui.label("Converter");
            ui.horizontal(|ui| {
                let mut on = df.impair.adc_bits.is_some();
                if ui.checkbox(&mut on, "finite bits").changed() {
                    df.impair.adc_bits = on.then_some(4);
                }
                if let Some(b) = df.impair.adc_bits.as_mut() {
                    ui.add(egui::DragValue::new(b).range(1..=16).suffix(" bits"));
                    num(ui, &mut df.impair.backoff_db, 0.0..=30.0, 0.25, " dB back");
                }
            });
            ui.end_row();
        });
    });
    group(ui, |ui| {
        head(ui, "How it's read");
        egui::Grid::new("df_method_grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Method");
            method_ui(ui, &mut df.method, &unavailable);
            ui.end_row();
            ui.label("Sources");
            ui.horizontal(|ui| {
                let mut count = df.count.is_none();
                if ui.checkbox(&mut count, "count them").changed() {
                    df.count = if count { None } else { Some(df.sources.len().max(1)) };
                }
                if let Some(c) = df.count.as_mut() {
                    int(ui, c, 1..=64);
                }
            });
            ui.end_row();
            ui.label("For echoes");
            ui.horizontal(|ui| {
                ui.checkbox(&mut df.forward_backward, "forward-backward");
                let mut smooth = df.smooth_len > 0;
                if ui.checkbox(&mut smooth, "smoothing").changed() {
                    df.smooth_len = if smooth { (view.as_ref().map_or(8, |v| v.channels) * 2 / 3).max(2) } else { 0 };
                }
                if df.smooth_len > 0 {
                    int(ui, &mut df.smooth_len, 2..=256);
                }
            });
            ui.end_row();
        });
    });
    if df != st.project.phased.df {
        st.project.phased.df = df.clone();
    }
    match view.as_ref() {
        Some(v) => df_results_ui(ui, st, v, df.use_ports),
        None => {
            ui.spinner();
        }
    }
    if stale {
        weak(ui, "Updating…");
    }
    group(ui, |ui| {
        head(ui, "Accuracy against SNR");
        let mut compare = df.compare.clone();
        ui.horizontal_wrapped(|ui| {
            for m in Method::ALL {
                let why = unavailable.iter().find(|u| u.0 == m);
                let mut on = compare.contains(&m);
                if ui.add_enabled(why.is_none(), egui::Checkbox::new(&mut on, m.label())).changed() {
                    if on {
                        compare.push(m);
                    } else {
                        compare.retain(|x| *x != m);
                    }
                }
            }
        });
        let mut d2 = df.clone();
        d2.compare = compare;
        ui.horizontal(|ui| {
            num(ui, &mut d2.snr_from, -40.0..=60.0, 1.0, " dB from");
            num(ui, &mut d2.snr_to, -40.0..=80.0, 1.0, " dB to");
            num(ui, &mut d2.snr_step, 1.0..=20.0, 0.5, " dB step");
            int(ui, &mut d2.trials, 10..=5000);
            ui.label("trials");
        });
        if d2 != st.project.phased.df {
            st.project.phased.df = d2;
        }
        let busy = st.busy();
        if run_button(ui, "Run the study", !busy) {
            st.run_df_study();
        }
        if let Some(curves) = st.phased_jobs().study {
            let palette = charts::PALETTE;
            let mut lines: Vec<(&str, Color32, Vec<(f64, f64)>)> = curves.iter().enumerate().map(|(i, c)| (c.method.label(), palette[i % palette.len()], c.points.iter().map(|p| (p.snr_db, p.rmse_deg)).collect())).collect();
            if let Some(first) = curves.first() {
                lines.push(("Bound", Color32::BLACK, first.points.iter().map(|p| (p.snr_db, p.crb_deg)).collect()));
            }
            log_chart(ui, "RMS error against SNR, in resolved trials", "SNR dB", "°", &lines);
            let resolved: Vec<(String, Color32, Vec<(f64, f64)>)> = curves.iter().enumerate().map(|(i, c)| (c.method.label().to_string(), palette[i % palette.len()], c.points.iter().map(|p| (p.snr_db, 100.0 * p.resolved)).collect())).collect();
            let series: Vec<Series> = resolved.iter().map(|r| Series { name: &r.0, color: r.1, points: &r.2 }).collect();
            charts::line_chart(ui, ChartOptions { title: "Trials with every source resolved", unit: " %", x_unit: "SNR dB", height: 130.0, y_range: Some((0.0, 105.0)), ..Default::default() }, &series);
        }
    });
    switched_ui(ui, st, f0);
    fix_ui(ui, st);
}

/// One receiver switched round the array, as a BLE locator does: what its frequency offset costs.
fn switched_ui(ui: &mut egui::Ui, st: &mut Studio, f0: f64) {
    egui::CollapsingHeader::new("One receiver, switched round the array").id_salt("df_switched").show(ui, |ui| {
        weak(ui, "As a BLE angle-of-arrival locator: one receiver takes each element in turn. A frequency offset between transmitter and receiver then reads as angle, unless reference samples measure it first.");
        let sw = &mut st.ui.phased.switched;
        egui::Grid::new("df_switched_grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Slot, offset");
            ui.horizontal(|ui| {
                let mut us = sw.slot_s * 1e6;
                if num(ui, &mut us, 0.01..=1000.0, 0.05, " µs").changed() {
                    sw.slot_s = us * 1e-6;
                }
                let mut khz = sw.cfo_hz / 1e3;
                if num(ui, &mut khz, -500.0..=500.0, 1.0, " kHz").changed() {
                    sw.cfo_hz = khz * 1e3;
                }
            });
            ui.end_row();
            ui.label("Reference, rounds");
            ui.horizontal(|ui| {
                int(ui, &mut sw.reference, 2..=256);
                int(ui, &mut sw.rounds, 1..=256);
            });
            ui.end_row();
        });
        let (sw, df) = (st.ui.phased.switched, st.project.phased.df.clone());
        let key = (sw, df.clone(), f0);
        let (view, stale) = st.ui.phased.switched_view.get(&key, move || switched_errors(&df, f0, &sw, 100));
        if stale {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }
        match view.as_deref() {
            Some(Ok((with, without))) => tiles(ui, &[(format!("{with:.2}°"), "rms, offset taken out", theme::SUCCESS), (format!("{without:.2}°"), "rms, offset left in", theme::WARNING)]),
            Some(Err(e)) => verdict(ui, false, e),
            None => {
                ui.spinner();
            }
        }
    });
}

/// Where bearings from several stations cross.
fn fix_ui(ui: &mut egui::Ui, st: &mut Studio) {
    egui::CollapsingHeader::new("A fix from several stations").id_salt("df_fix").show(ui, |ui| {
        weak(ui, "Where bearings from separate direction finders cross, weighted by how good each is. The error grows with range, and with how shallow the crossing is.");
        let stations = &mut st.ui.phased.stations;
        if stations.is_empty() {
            *stations = vec![[0.0, 0.0, 51.3, 2.0], [1000.0, 0.0, 106.7, 2.0], [400.0, 800.0, 286.7, 2.0]];
        }
        egui::Grid::new("df_fix_grid").num_columns(5).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            for h in ["x", "y", "Bearing φ", "Error", ""] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            let mut remove = None;
            let many = stations.len() > 2;
            for (k, s) in stations.iter_mut().enumerate() {
                num(ui, &mut s[0], -1e7..=1e7, 1.0, " m");
                num(ui, &mut s[1], -1e7..=1e7, 1.0, " m");
                num(ui, &mut s[2], -360.0..=360.0, 0.1, "°");
                num(ui, &mut s[3], 0.01..=45.0, 0.05, "°");
                if many && ui.small_button(icon::X).clicked() {
                    remove = Some(k);
                }
                ui.end_row();
            }
            if let Some(k) = remove {
                stations.remove(k);
            }
        });
        if ui.button(theme::label(icon::PLUS, "Station")).clicked() {
            stations.push([0.0, 500.0, 0.0, 2.0]);
        }
        let lines: Vec<(antenna_core::geometry::P3, (f64, f64), f64)> = stations.iter().map(|s| ([s[0], s[1], 0.0], (90.0, s[2]), s[3])).collect();
        match doa::intersect_bearings(&lines) {
            Ok((at, radius)) => tiles(ui, &[(format!("{:.1} m", at[0]), "x", theme::SUCCESS), (format!("{:.1} m", at[1]), "y", theme::SUCCESS), (format!("{radius:.1} m"), "error radius, 1σ", theme::TEXT)]),
            Err(e) => verdict(ui, false, &e.to_string()),
        }
    });
}

fn stages_ui(ui: &mut egui::Ui, stages: &mut Vec<Stage>) {
    egui::Grid::new("chain_stages").num_columns(5).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        for h in ["Stage", "Gain", "NF", "IIP3", ""] {
            ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
        }
        ui.end_row();
        let mut remove = None;
        for (k, s) in stages.iter_mut().enumerate() {
            ui.add_sized(vec2(96.0, 22.0), egui::TextEdit::singleline(&mut s.name));
            num(ui, &mut s.gain_db, -60.0..=80.0, 0.1, " dB");
            num(ui, &mut s.nf_db, 0.0..=40.0, 0.05, " dB");
            ui.horizontal(|ui| {
                let mut on = s.iip3_dbm.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    s.iip3_dbm = on.then_some(0.0);
                }
                if let Some(v) = s.iip3_dbm.as_mut() {
                    num(ui, v, -60.0..=60.0, 0.25, " dBm");
                }
            });
            if ui.small_button(icon::X).clicked() {
                remove = Some(k);
            }
            ui.end_row();
        }
        if let Some(k) = remove {
            stages.remove(k);
        }
    });
    if ui.button(theme::label(icon::PLUS, "Stage")).clicked() {
        stages.push(Stage::new("Amp", 15.0, 3.0, Some(10.0)));
    }
}

fn adc_ui(ui: &mut egui::Ui, adc: &mut Adc) {
    ui.add(egui::DragValue::new(&mut adc.bits).range(1..=20).suffix(" bits"));
    let mut fs = adc.fs_hz / 1e6;
    if num(ui, &mut fs, 0.1..=100_000.0, 1.0, " MHz").changed() {
        adc.fs_hz = fs * 1e6;
    }
    num(ui, &mut adc.full_scale_dbm, -40.0..=30.0, 0.25, " dBm FS");
}

fn waveform_ui(ui: &mut egui::Ui, id: &str, w: &mut Waveform, bw: f64) {
    let options = [Waveform::Qpsk { symbol_rate: bw / 2.0, rolloff: 0.35 }, Waveform::Qam16 { symbol_rate: bw / 2.0, rolloff: 0.35 }, Waveform::Ofdm { subcarriers: 256, bw_hz: bw }, Waveform::Chirp { bw_hz: bw, period_s: 100e-6 }, Waveform::Tone { offset_hz: 0.13 * bw }, Waveform::Noise { bw_hz: bw }];
    egui::ComboBox::from_id_salt(id).selected_text(w.label()).show_ui(ui, |ui| {
        for option in options {
            if ui.selectable_label(w.label() == option.label(), option.label()).clicked() {
                *w = option;
            }
        }
    });
}

fn block_diagram(ui: &mut egui::Ui, stages: &[Stage], adc: &Adc, decim: usize, bf: Bf, channels: usize) {
    let mut names: Vec<String> = stages.iter().map(|s| s.name.clone()).collect();
    names.push(format!("{}-bit ADC", adc.bits));
    names.push(format!("Filter ↓{decim}"));
    names.push(bf.label().to_string());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    let n = names.len().max(1) as f32;
    let w = rect.width() / n;
    let y = rect.center().y;
    for (i, name) in names.iter().enumerate() {
        let c = pos2(rect.left() + (i as f32 + 0.5) * w, y);
        let digital = i + 2 >= names.len();
        let b = egui::Rect::from_center_size(c, vec2(w - 10.0, 30.0));
        p.rect(b, 3.0, if digital { theme::ACCENT_SOFT } else { theme::PANEL }, Stroke::new(1.0, theme::STROKE_STRONG), egui::StrokeKind::Inside);
        let g = p.layout(name.clone(), FontId::proportional(10.5), theme::TEXT, w - 12.0);
        p.galley(c - g.size() / 2.0, g, theme::TEXT);
        if i + 1 < names.len() {
            p.line_segment([pos2(b.right(), y), pos2(b.right() + 10.0, y)], Stroke::new(1.2, theme::TEXT));
        }
    }
    if channels > 1 {
        p.text(rect.left_top() + vec2(4.0, 1.0), Align2::LEFT_TOP, format!("× {channels}"), FontId::proportional(10.0), theme::WEAK);
    }
}

fn constellation(ui: &mut egui::Ui, points: &[(f64, f64)]) {
    let size = ui.available_width().min(240.0);
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::RAISED);
    p.hline(rect.left()..=rect.right(), rect.center().y, Stroke::new(1.0, theme::STROKE));
    p.vline(rect.center().x, rect.top()..=rect.bottom(), Stroke::new(1.0, theme::STROKE));
    let top = points.iter().map(|q| q.0.abs().max(q.1.abs())).fold(0.0f64, f64::max).clamp(0.2, 2.0) * 1.15;
    for q in points {
        let at = rect.center() + vec2((q.0 / top) as f32 * size / 2.0, -(q.1 / top) as f32 * size / 2.0);
        if rect.contains(at) {
            p.circle_filled(at, 1.4, charts::BLUE);
        }
    }
    p.text(rect.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, "Constellation", FontId::proportional(11.0), theme::TEXT);
}

/// The Signal chain page.
pub fn chain_page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Signal chain");
    weak(ui, "From the antenna to the digital beam, by formula at once and sample by sample on demand, with compression, oscillator phase noise, I/Q imbalance, the converter, a decimating filter and an adaptive beamformer.");
    let mut d = st.project.phased.chain.clone();
    group(ui, |ui| {
        head(ui, "Receiver");
        block_diagram(ui, &d.sim.chain.stages, &d.sim.chain.adc, d.sim.decim, d.sim.bf, d.channels);
        stages_ui(ui, &mut d.sim.chain.stages);
        ui.add_space(2.0);
        egui::Grid::new("chain_adc").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Converter");
            ui.horizontal(|ui| adc_ui(ui, &mut d.sim.chain.adc));
            ui.end_row();
            ui.label("Clock");
            ui.horizontal(|ui| {
                let mut ps = d.sim.chain.adc.jitter_s * 1e12;
                if num(ui, &mut ps, 0.0..=1000.0, 0.05, " ps jitter").changed() {
                    d.sim.chain.adc.jitter_s = ps * 1e-12;
                }
                let mut f = d.sim.chain.adc.f_in_hz / 1e6;
                if num(ui, &mut f, 0.0..=100_000.0, 1.0, " MHz in").changed() {
                    d.sim.chain.adc.f_in_hz = f * 1e6;
                }
            });
            ui.end_row();
            ui.label("Signal band");
            ui.horizontal(|ui| {
                let mut bw = d.sim.chain.bw_hz / 1e6;
                if num(ui, &mut bw, 0.01..=10_000.0, 0.5, " MHz").changed() {
                    d.sim.chain.bw_hz = bw * 1e6;
                }
                ui.label("decimate");
                int(ui, &mut d.sim.decim, 1..=64);
            });
            ui.end_row();
        });
    });
    let budget = chain::budget(&d.sim.chain);
    tiles(
        ui,
        &[
            (format!("{:.2} dB", budget.nf_system_db), "noise figure", theme::SUCCESS),
            (format!("{:.1} dBm", budget.noise_floor_dbm), "noise floor", theme::TEXT),
            (format!("{:.1} dBm", budget.clip_input_dbm), "clips at", theme::TEXT),
            (format!("{:.1} dB", budget.dynamic_range_db), "dynamic range", theme::TEXT),
            (budget.sfdr_db.map_or("–".to_string(), |v| format!("{v:.1} dB")), "spur-free range", theme::TEXT),
            (format!("{:.1} dB", budget.sqnr_db), "converter's own SNR", theme::TEXT),
        ],
    );
    if budget.thermal_over_quant_db < 6.0 {
        verdict(ui, false, &format!("Thermal noise is only {:.1} dB above the converter's own at its input, so the converter is setting the noise: {:.1} dB lost. More gain ahead of it, or more bits, would help.", budget.thermal_over_quant_db, budget.nf_system_db - budget.nf_analog_db));
    } else if budget.thermal_over_quant_db > 30.0 {
        verdict(ui, false, &format!("Thermal noise sits {:.0} dB above the converter's own, so bits are being spent on noise. The chain could have less gain, and take larger signals before it clips.", budget.thermal_over_quant_db));
    } else {
        verdict(ui, true, &format!("Thermal noise sits {:.0} dB above the converter's own, so the analog chain sets the noise and little range is wasted.", budget.thermal_over_quant_db));
    }
    egui::CollapsingHeader::new("Cascade, stage by stage").id_salt("chain_cascade").show(ui, |ui| {
        egui::Grid::new("chain_budget").num_columns(5).spacing(vec2(12.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["After", "Gain", "NF", "IIP3", "Noise out"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for r in &budget.rows {
                ui.label(&r.name);
                ui.label(format!("{:.1} dB", r.gain_db));
                ui.label(format!("{:.2} dB", r.nf_db));
                ui.label(r.iip3_dbm.map_or("–".to_string(), |v| format!("{v:.1} dBm")));
                ui.label(format!("{:.1} dBm", r.noise_dbm));
                ui.end_row();
            }
        });
    });
    group(ui, |ui| {
        head(ui, "What's received");
        let bw = d.sim.chain.bw_hz;
        egui::Grid::new("chain_scene").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Channels, half-wave line");
            int(ui, &mut d.channels, 1..=256);
            ui.end_row();
            ui.label("Signal");
            ui.horizontal(|ui| {
                waveform_ui(ui, "chain_wave", &mut d.sim.waveform, bw);
                num(ui, &mut d.sim.signal_dbm, -150.0..=10.0, 0.5, " dBm");
                num(ui, &mut d.signal_deg, -90.0..=90.0, 0.5, "°");
            });
            ui.end_row();
            ui.label("Jammer");
            ui.horizontal(|ui| {
                let mut on = d.jammer.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    d.jammer = on.then_some(JammerDesign { angle_deg: 40.0, dbm: -40.0, waveform: Waveform::Tone { offset_hz: 0.13 * bw } });
                }
                if let Some(j) = d.jammer.as_mut() {
                    waveform_ui(ui, "chain_jam", &mut j.waveform, bw);
                    num(ui, &mut j.dbm, -150.0..=10.0, 0.5, " dBm");
                    num(ui, &mut j.angle_deg, -90.0..=90.0, 0.5, "°");
                }
            });
            ui.end_row();
            ui.label("Beamformer");
            egui::ComboBox::from_id_salt("chain_bf").selected_text(d.sim.bf.label()).show_ui(ui, |ui| {
                for b in Bf::ALL {
                    ui.selectable_value(&mut d.sim.bf, b, b.label());
                }
            });
            ui.end_row();
            ui.label("Oscillator");
            ui.horizontal(|ui| {
                let mut on = d.sim.lo.pn_dbc_hz.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    d.sim.lo = Lo { pn_dbc_hz: on.then_some(-90.0), ..d.sim.lo };
                }
                if let Some(l) = d.sim.lo.pn_dbc_hz.as_mut() {
                    num(ui, l, -170.0..=-30.0, 0.5, " dBc/Hz");
                    let mut k = d.sim.lo.pn_offset_hz / 1e3;
                    if num(ui, &mut k, 0.01..=10_000.0, 1.0, " kHz").changed() {
                        d.sim.lo.pn_offset_hz = k * 1e3;
                    }
                    let mut lp = d.sim.lo.loop_hz / 1e3;
                    if num(ui, &mut lp, 0.0..=100_000.0, 1.0, " kHz loop").changed() {
                        d.sim.lo.loop_hz = lp * 1e3;
                    }
                    ui.checkbox(&mut d.sim.lo.shared, "shared");
                }
            });
            ui.end_row();
            ui.label("I/Q imbalance");
            ui.horizontal(|ui| {
                num(ui, &mut d.sim.iq_gain_db, 0.0..=6.0, 0.01, " dB");
                num(ui, &mut d.sim.iq_phase_deg, 0.0..=30.0, 0.05, "°");
                weak(ui, format!("{:.0} dB image", chain::image_rejection_db(d.sim.iq_gain_db, d.sim.iq_phase_deg).min(99.0)));
            });
            ui.end_row();
            ui.label("Channel mismatch");
            ui.horizontal(|ui| {
                num(ui, &mut d.sim.channel_gain_db, 0.0..=6.0, 0.05, " dB");
                num(ui, &mut d.sim.channel_phase_deg, 0.0..=90.0, 0.5, "°");
                let mut on = d.sim.calibrate_snr_db.is_some();
                if ui.checkbox(&mut on, "calibrated").changed() {
                    d.sim.calibrate_snr_db = on.then_some(35.0);
                }
                if let Some(s) = d.sim.calibrate_snr_db.as_mut() {
                    num(ui, s, 0.0..=80.0, 0.5, " dB pilot");
                }
            });
            ui.end_row();
            ui.label("Gain control");
            ui.horizontal(|ui| {
                let mut on = d.sim.agc_backoff_db.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    d.sim.agc_backoff_db = on.then_some(12.0);
                }
                if let Some(b) = d.sim.agc_backoff_db.as_mut() {
                    num(ui, b, 0.0..=60.0, 0.25, " dB below full scale");
                }
            });
            ui.end_row();
        });
    });
    if d != st.project.phased.chain {
        st.project.phased.chain = d.clone();
    }
    let busy = st.busy();
    if run_button(ui, "Run the chain, sample by sample", !busy) {
        st.run_chain();
    }
    let jobs = st.phased_jobs();
    if let Some(r) = &jobs.chain {
        tiles(
            ui,
            &[
                (format!("{:.1} dB", r.output_sinr_db), "out of the beam", theme::SUCCESS),
                (format!("{:.1} dB", r.element_sinr_db), "one channel", theme::TEXT),
                (format!("{:.1} dB", r.array_gain_db), "array gain", theme::TEXT),
                (r.evm_percent.map_or("–".to_string(), |v| format!("{v:.1} %")), "EVM", theme::TEXT),
                (format!("{:.1} dBFS", r.adc_rms_dbfs), "converter level", theme::TEXT),
                (format!("{:.2} %", r.clipped_percent), "clipped", if r.clipped_percent > 0.1 { theme::WARNING } else { theme::TEXT }),
            ],
        );
        let ideal = 10.0 * (r.channels.max(1) as f64).log10();
        if r.channels > 1 && r.array_gain_db < ideal - 1.0 {
            weak(ui, format!("A perfect array in white noise would gain {ideal:.1} dB. What's common to the channels, or not corrected between them, is what takes the rest."));
        }
        head(ui, "Signal over noise along the chain");
        let rows: Vec<(f64, f64)> = r.taps.iter().enumerate().map(|(i, t)| (i as f64, t.sinr_db)).collect();
        charts::line_chart(ui, ChartOptions { title: "Each point read through the same filter", unit: " dB", x_unit: "stage", height: 130.0, ..Default::default() }, &[Series { name: "Signal over noise", color: charts::GREEN, points: &rows }]);
        egui::Grid::new("chain_taps").num_columns(3).spacing(vec2(12.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["At", "S/N", "Power"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for t in &r.taps {
                ui.label(&t.name);
                ui.label(format!("{:.1} dB", t.sinr_db));
                ui.label(format!("{:.1} dBm", t.power_dbm));
                ui.end_row();
            }
        });
        charts::line_chart(ui, ChartOptions { title: "At the converter", unit: " dBFS", x_unit: "MHz", height: 150.0, ..Default::default() }, &[Series { name: "One channel", color: charts::BLUE, points: &r.spectrum_adc }]);
        charts::line_chart(ui, ChartOptions { title: "After the beamformer", unit: " dB", x_unit: "MHz", height: 150.0, y_range: Some((-90.0, 2.0)), ..Default::default() }, &[Series { name: "Beam", color: charts::PURPLE, points: &r.spectrum_out }]);
        if !r.constellation.is_empty() {
            constellation(ui, &r.constellation);
        }
    }
    if let Some(tx) = &jobs.tx {
        head(ui, "The same signal through a power amplifier");
        let (evm, dpd): (Vec<(f64, f64)>, Vec<(f64, f64)>) = (tx.iter().map(|p| (p.backoff_db, p.evm_percent.max(0.01))).collect(), tx.iter().map(|p| (p.backoff_db, p.dpd_evm_percent.max(0.01))).collect());
        log_chart(ui, "EVM against back-off from saturation", "back-off dB", " %", &[("Plain", charts::ORANGE, evm), ("With predistortion", charts::GREEN, dpd)]);
        let (a, b): (Vec<(f64, f64)>, Vec<(f64, f64)>) = (tx.iter().map(|p| (p.backoff_db, p.acpr_db.max(-90.0))).collect(), tx.iter().map(|p| (p.backoff_db, p.dpd_acpr_db.max(-90.0))).collect());
        charts::line_chart(ui, ChartOptions { title: "Next channel over the wanted one", unit: " dB", x_unit: "back-off dB", height: 140.0, y_range: Some((-92.0, -10.0)), ..Default::default() }, &[Series { name: "Plain", color: charts::ORANGE, points: &a }, Series { name: "With predistortion", color: charts::GREEN, points: &b }]);
        if let Some(first) = tx.first() {
            weak(ui, format!("This signal peaks {:.1} dB above its average. A memoryless Rapp amplifier, 30 dBm at saturation. The curves stop at 0.01 % and −90 dB because this model has nothing to limit them: real amplifiers' memory and a real feedback path's noise stop predistortion well before that.", first.papr_db));
        }
    }
    estimate_note(ui, "from behavioural models of each stage, with no measured parts.");
}

/// The Radar page.
pub fn radar_page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Radar");
    weak(ui, "An FMCW MIMO radar from raw beat signal: range and Doppler FFTs, CFAR, the time-division correction, and angle from the virtual array.");
    let mut d = st.project.phased.radar.clone();
    group(ui, |ui| {
        head(ui, "Waveform and array");
        let r = &mut d.radar;
        egui::Grid::new("radar_setup").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Carrier");
            ghz_field(ui, &mut r.f0);
            ui.end_row();
            ui.label("Sweep");
            ui.horizontal(|ui| {
                let mut b = r.bandwidth / 1e6;
                if num(ui, &mut b, 1.0..=10_000.0, 5.0, " MHz").changed() {
                    r.bandwidth = b * 1e6;
                }
                let mut c = r.chirp_s * 1e6;
                if num(ui, &mut c, 1.0..=10_000.0, 0.5, " µs").changed() {
                    r.chirp_s = c * 1e-6;
                }
            });
            ui.end_row();
            ui.label("Repeats every");
            let mut p = r.repeat_s * 1e6;
            if num(ui, &mut p, 1.0..=10_000.0, 0.5, " µs").changed() {
                r.repeat_s = p * 1e-6;
            }
            ui.end_row();
            ui.label("Samples, chirps");
            ui.horizontal(|ui| {
                int(ui, &mut r.samples, 16..=4096);
                int(ui, &mut r.loops, 4..=1024);
            });
            ui.end_row();
            ui.label("Transmit, receive");
            ui.horizontal(|ui| {
                int(ui, &mut d.n_tx, 1..=16);
                int(ui, &mut d.n_rx, 1..=32);
            });
            ui.end_row();
            ui.label("Power, gains");
            ui.horizontal(|ui| {
                num(ui, &mut r.tx_dbm, -30.0..=50.0, 0.25, " dBm");
                num(ui, &mut r.tx_gain_dbi, -10.0..=50.0, 0.25, " dBi tx");
                num(ui, &mut r.rx_gain_dbi, -10.0..=50.0, 0.25, " dBi rx");
            });
            ui.end_row();
            ui.label("Noise figure");
            num(ui, &mut r.nf_db, 0.0..=40.0, 0.25, " dB");
            ui.end_row();
        });
    });
    group(ui, |ui| {
        head(ui, "Targets");
        egui::Grid::new("radar_targets").num_columns(6).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
            for h in ["Range", "Speed", "θ", "φ", "RCS", ""] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            let mut remove = None;
            for (k, t) in d.targets.iter_mut().enumerate() {
                num(ui, &mut t.range_m, 0.1..=100_000.0, 0.25, " m");
                num(ui, &mut t.speed_mps, -1000.0..=1000.0, 0.1, " m/s");
                num(ui, &mut t.theta, 0.0..=90.0, 0.25, "°");
                num(ui, &mut t.phi, -360.0..=360.0, 1.0, "°");
                num(ui, &mut t.rcs_dbsm, -60.0..=60.0, 0.5, " dBsm");
                if ui.small_button(icon::X).clicked() {
                    remove = Some(k);
                }
                ui.end_row();
            }
            if let Some(k) = remove {
                d.targets.remove(k);
            }
        });
        if ui.button(theme::label(icon::PLUS, "Target")).clicked() {
            d.targets.push(Target::default());
        }
    });
    if d != st.project.phased.radar {
        st.project.phased.radar = d.clone();
    }
    let radar = d.resolved();
    let limits = radar.limits();
    tiles(
        ui,
        &[
            (format!("{:.2} m", limits.range_resolution_m), "range resolution", theme::TEXT),
            (format!("{:.0} m", limits.max_range_m), "range limit", theme::TEXT),
            (format!("{:.2} m/s", limits.speed_resolution_mps), "speed resolution", theme::TEXT),
            (format!("±{:.1} m/s", limits.max_speed_mps), "speed limit", theme::TEXT),
            (format!("{:.1}°", limits.angle_resolution_deg), "angle resolution", theme::TEXT),
            (format!("{}", limits.virtual_elements), "virtual elements", theme::TEXT),
        ],
    );
    for t in &d.targets {
        if t.range_m > limits.max_range_m {
            verdict(ui, false, &format!("A target at {:.0} m is past the {:.0} m range limit: it will alias.", t.range_m, limits.max_range_m));
        }
        // Past the FFT's limit several transmitters can still tell a fold, from their phases.
        let reach = if d.n_tx > 1 && d.n_rx > 1 { d.n_tx as f64 * limits.max_speed_mps } else { limits.max_speed_mps };
        if t.speed_mps.abs() > reach {
            verdict(ui, false, &format!("A target at {:.1} m/s is past what this waveform can tell, ±{reach:.1} m/s: its speed folds, and with several transmitters its angle goes wrong too.", t.speed_mps));
        }
    }
    let busy = st.busy();
    if run_button(ui, "Simulate and process", !busy) {
        st.run_radar();
    }
    if let Some(out) = st.phased_jobs().radar {
        head(ui, "Range against speed");
        let (rows, cols) = (out.map.len(), out.map.first().map_or(0, Vec::len));
        let top = out.map.iter().flatten().copied().fold(f32::MIN, f32::max) as f64;
        let floor = top - 60.0;
        let key = Arc::as_ptr(&out) as usize;
        let rect = heat_map(ui, &mut st.ui.phased.radar_picture, key, "radar_map", cols, rows, vec2(ui.available_width(), 220.0), |c, r| ((out.map[r][c] as f64 - floor) / 60.0).clamp(0.0, 1.0));
        let p = ui.painter_at(rect);
        let (r_max, v_max) = (out.range_axis.last().copied().unwrap_or(1.0).max(1e-6), out.speed_axis.last().copied().unwrap_or(1.0).abs().max(1e-6));
        for det in &out.detections {
            let folded = (det.speed_mps + limits.max_speed_mps).rem_euclid(2.0 * limits.max_speed_mps) - limits.max_speed_mps;
            let at = pos2(rect.left() + (det.range_m / r_max) as f32 * rect.width(), rect.center().y - (folded / v_max) as f32 * rect.height() / 2.0);
            p.circle_stroke(at, 6.0, Stroke::new(1.5, Color32::WHITE));
        }
        p.text(rect.left_bottom() + vec2(4.0, -3.0), Align2::LEFT_BOTTOM, format!("0 to {r_max:.0} m across, ±{v_max:.1} m/s up. Rings are detections."), FontId::proportional(10.5), Color32::WHITE);
        charts::heat_legend(ui, "−60 dB", "peak");
        head(ui, "Detections");
        egui::Grid::new("radar_found").num_columns(5).spacing(vec2(12.0, 3.0)).striped(true).show(ui, |ui| {
            for h in ["Range", "Speed", "Angle", "S/N", "Target"] {
                ui.label(RichText::new(h).size(11.5).color(theme::WEAK));
            }
            ui.end_row();
            for det in &out.detections {
                ui.label(format!("{:.2} m", det.range_m));
                ui.label(format!("{:.2} m/s", det.speed_mps));
                ui.label(if det.has_angle { format!("{:.1}°", doa::signed_plane_angle((det.theta, det.phi), 0.0)) } else { "–".to_string() });
                ui.label(format!("{:.1} dB", det.snr_db));
                let found = d.targets.iter().position(|t| (t.range_m - det.range_m).abs() < 2.0 * limits.range_resolution_m && (t.speed_mps - det.speed_mps).abs() < 2.0 * limits.speed_resolution_mps);
                ui.label(found.map_or("false alarm".to_string(), |k| format!("{}", k + 1)));
                ui.end_row();
            }
        });
        let missed: Vec<String> = d.targets.iter().enumerate().filter(|(_, t)| !out.detections.iter().any(|x| (t.range_m - x.range_m).abs() < 2.0 * limits.range_resolution_m)).map(|(k, t)| format!("{} ({:.0} dB by the radar equation)", k + 1, radar.integrated_snr_db(t))).collect();
        if !missed.is_empty() {
            verdict(ui, false, &format!("Not detected: target {}.", missed.join(", ")));
        }
    }
    estimate_note(ui, "point targets in free space by the radar equation, with the antenna gains as entered: no clutter or multipath.");
}
