//! The Simulations panel: the studies, each one's set-up, its runs and how to read the results.

use bevy_egui::egui::{self, Align, Color32, Layout, RichText, Sense, vec2};
use sim_core::catalog::{self, Needs};
use sim_core::jobs::{Kind, State};
use sim_core::materials;
use sim_core::results::{ResultSet, StepKind};
use sim_core::study::{BcKind, Condition, Study};

use super::{SimState, dialog, view};
use crate::design::{Slot, Workspace, bridge};
use crate::theme::{self, ACCENT, icon};

/// Applies the faces chosen in the design, once the user says Done.
pub fn finish_pick(state: &mut SimState, slot: &mut Slot) {
    if !std::mem::take(&mut slot.prompt_done) {
        return;
    }
    slot.prompt = None;
    let Some((study, condition)) = state.picking.take() else { return };
    let Some(host) = &slot.host else { return };
    let picked = bridge::selected_faces(&host.app.highlighted());
    let Some(s) = state.studies.get_mut(study) else { return };
    if picked.is_empty() {
        state.message = Some("No faces were selected. Click a face in the view, then Done.".into());
        return;
    }
    // A study is on one body, and maybe a second it touches: the picks decide the first if it wasn't
    // chosen yet.
    let body = s.body.clone().unwrap_or_else(|| picked[0].0.clone());
    let mut faces = Vec::new();
    let mut others = 0;
    for (on, f) in &picked {
        if *on == body {
            faces.push(f.clone());
        } else if s.second_body.as_deref() == Some(on.as_str()) {
            faces.push(sim_core::study::FaceRef { second: true, ..f.clone() });
        } else {
            others += 1;
        }
    }
    if others > 0 {
        state.message = Some(format!("{others} selected faces were on another body than {body}, and were left out."));
    }
    s.body = Some(body);
    if let Some(c) = s.conditions.get_mut(condition) {
        c.faces = faces;
    }
}

fn number(ui: &mut egui::Ui, label: &str, value: &mut f64, unit: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let speed = (value.abs() * 0.01).max(1e-3);
        ui.add(egui::DragValue::new(value).speed(speed).max_decimals(6));
        ui.label(RichText::new(unit).color(theme::WEAK));
    });
}

fn condition_ui(ui: &mut egui::Ui, kind: &mut BcKind) {
    match kind {
        BcKind::Fixed | BcKind::Wall => {}
        BcKind::Force { force } => {
            ui.horizontal(|ui| {
                ui.label("Fx");
                ui.add(egui::DragValue::new(&mut force[0]).speed(1.0));
                ui.label("Fy");
                ui.add(egui::DragValue::new(&mut force[1]).speed(1.0));
                ui.label("Fz");
                ui.add(egui::DragValue::new(&mut force[2]).speed(1.0));
                ui.label(RichText::new("N").color(theme::WEAK));
            });
        }
        BcKind::Pressure { pressure } => number(ui, "Pressure", pressure, "Pa"),
        BcKind::Temperature { value } => number(ui, "Temperature", value, "°C"),
        BcKind::HeatFlux { flux } => number(ui, "Heat flux", flux, "W/m²"),
        BcKind::Convection { coefficient, ambient } => {
            number(ui, "Coefficient", coefficient, "W/m²K");
            number(ui, "Ambient", ambient, "°C");
        }
        BcKind::VelocityInlet { speed } => number(ui, "Speed", speed, "m/s"),
        BcKind::PressureOutlet { pressure } => number(ui, "Gauge pressure", pressure, "Pa"),
        BcKind::Potential { value } => number(ui, "Potential", value, "V"),
        BcKind::Displacement { displacement } => {
            number(ui, "Move x", &mut displacement[0], "m");
            number(ui, "Move y", &mut displacement[1], "m");
            number(ui, "Move z", &mut displacement[2], "m");
        }
        BcKind::Contact { friction } => {
            number(ui, "Friction", friction, "");
            ui.label(RichText::new("Pick the touching faces on both bodies.").color(theme::WEAK).size(11.5));
        }
    }
}

fn axis_ui(ui: &mut egui::Ui, axis: &mut u32) {
    ui.horizontal(|ui| {
        ui.label("About the axis through its middle:");
        for (i, name) in ["X", "Y", "Z"].iter().enumerate() {
            ui.selectable_value(axis, i as u32, *name);
        }
    });
}

fn settings_ui(ui: &mut egui::Ui, s: &mut Study) {
    use catalog::AnalysisId as A;
    let t = &mut s.settings;
    ui.horizontal(|ui| {
        ui.label("Element size");
        ui.add(egui::Slider::new(&mut t.element_size, 0.01..=0.2).logarithmic(true).custom_formatter(|v, _| format!("{:.1}% of size", v * 100.0)));
    });
    match s.analysis {
        A::Frequency => {
            ui.horizontal(|ui| {
                ui.label("Modes");
                ui.add(egui::DragValue::new(&mut t.modes).range(1..=50));
            });
        }
        A::Dynamic => {
            number(ui, "Time step", &mut t.dt, "s");
            number(ui, "Duration", &mut t.duration, "s");
        }
        A::Harmonic => {
            ui.horizontal(|ui| {
                ui.label("Modes");
                ui.add(egui::DragValue::new(&mut t.modes).range(1..=50));
            });
            number(ui, "From", &mut t.f_min, "Hz");
            number(ui, "To", &mut t.f_max, "Hz");
            ui.horizontal(|ui| {
                ui.label("Points per interval");
                ui.add(egui::DragValue::new(&mut t.points).range(2..=200));
            });
            number(ui, "Damping ratio", &mut t.damping, "");
        }
        A::HeatTransfer | A::Thermomechanical => {
            ui.checkbox(&mut t.transient, "Over time");
            if t.transient {
                number(ui, "Time step", &mut t.dt, "s");
                number(ui, "Duration", &mut t.duration, "s");
            }
            number(ui, "Starting temperature", &mut t.initial_temperature, "°C");
            if s.analysis == A::Thermomechanical {
                number(ui, "Stress-free temperature", &mut t.reference_temperature, "°C");
            }
        }
        A::NonlinearMechanical => {
            ui.horizontal(|ui| {
                ui.label("Load increment");
                ui.add(egui::Slider::new(&mut t.load_increment, 0.01..=1.0));
            });
        }
        A::MultiPurpose => {
            number(ui, "Free stream, with no inlet picked", &mut t.free_stream, "m/s");
            ui.checkbox(&mut t.compressible, "Density changes with the flow");
            number(ui, "Rotating zone around the body", &mut t.rotor_rpm, "rpm");
            if t.rotor_rpm != 0.0 {
                axis_ui(ui, &mut t.axis);
            }
        }
        A::ConjugateHeatTransfer => {
            number(ui, "Air speed", &mut t.free_stream, "m/s");
            number(ui, "Air temperature", &mut t.initial_temperature, "°C");
        }
        A::Electromagnetics => {
            number(ui, "Coil current", &mut t.coil_ampere_turns, "ampere-turns");
            if t.coil_ampere_turns != 0.0 {
                axis_ui(ui, &mut t.axis);
                ui.label(RichText::new("The body carries this current round the axis, in air. Conditions aren't used.").color(theme::WEAK).size(11.5));
            } else {
                ui.label(RichText::new("At zero: the electric field between faces with potentials.").color(theme::WEAK).size(11.5));
            }
        }
        A::IncompressibleLbm | A::ConjugateHeatTransferVoxel | A::PedestrianWindComfort => {
            number(ui, "Wind speed", &mut t.free_stream, "m/s");
            ui.horizontal(|ui| {
                ui.label("Cells across the body");
                ui.add(egui::DragValue::new(&mut t.resolution).range(8..=512));
            });
            if s.analysis == A::PedestrianWindComfort {
                ui.horizontal(|ui| {
                    ui.label("Wind directions");
                    ui.add(egui::DragValue::new(&mut t.directions).range(1..=36));
                });
            }
            if s.analysis == A::ConjugateHeatTransferVoxel {
                number(ui, "Air temperature", &mut t.initial_temperature, "°C");
            }
        }
        A::Multiphase => {
            number(ui, "Duration", &mut t.duration, "s");
            ui.horizontal(|ui| {
                ui.label("Starting water height");
                ui.add(egui::Slider::new(&mut t.fill_fraction, 0.0..=1.0));
            });
        }
        A::Incompressible | A::Compressible | A::ConvectiveHeatTransfer => {
            number(ui, "Free stream, with no inlet picked", &mut t.free_stream, "m/s");
        }
        _ => {}
    }
}

/// Starts a study's run on a thread of its own, and says if it can't, in the user's words. The geometry
/// is taken now, as it is.
pub fn start_run(state: &mut SimState, slot: &mut Slot, index: usize) -> Result<u64, String> {
    let study = state.studies.get(index).cloned().ok_or("That study no longer exists.")?;
    let needs = catalog::analysis(study.analysis).needs;
    let body = study.body.clone().unwrap_or_default();
    let label = study.name.clone();
    let name = study.name.clone();
    let meshed = matches!(needs, Needs::NativeFea | Needs::Elmer);
    // What the solvers get: a surface with its faces, and for a design body, STEP as well.
    if study.second_body.is_some() && study.scene_object().is_some() {
        return Err("Contact needs both bodies from the design: a mesh from the scene has no CAD faces to touch cleanly.".into());
    }
    let second = match &study.second_body {
        Some(b) if meshed => {
            let host = crate::design::ensure_host(slot)?;
            Some(Box::new(bridge::fea_geometry(&host.app.session, b).map_err(|e| format!("{b}: {e:#}"))?))
        }
        _ => None,
    };
    let (part, step) = if let Some(object) = study.scene_object() {
        let Some(scene) = state.scene.bodies.get(object) else {
            return Err(state.scene.errors.get(object).cloned().unwrap_or_else(|| format!("{object} is still being read from the scene: try again in a moment.")));
        };
        if meshed && let Some(problem) = scene.regions.solid_problem() {
            return Err(problem);
        }
        (scene.regions.partition.clone(), Vec::new())
    } else {
        let host = crate::design::ensure_host(slot)?;
        let session = &host.app.session;
        if meshed {
            let g = bridge::fea_geometry(session, &body).map_err(|e| format!("{e:#}"))?;
            (g.partition, g.step)
        } else {
            (bridge::partition(session, &body, solidcraft_engine::io::MeshQuality::Fine).map_err(|e| format!("{e:#}"))?, Vec::new())
        }
    };
    let id = match needs {
        Needs::NativeFea => {
            let geometry = sim_core::ccx::study::FeaGeometry { partition: part, step, second };
            state.jobs.submit(&label, &name, Kind::Cpu, move |ctx| sim_core::ccx::study::run_study(&study, &geometry, ctx))
        }
        Needs::Elmer => {
            let geometry = sim_core::ccx::study::FeaGeometry { partition: part, step, second: None };
            state.jobs.submit(&label, &name, Kind::Cpu, move |ctx| sim_core::elmer_study::run_study(&study, &geometry, ctx))
        }
        Needs::Foam => state.jobs.submit(&label, &name, Kind::Cpu, move |ctx| sim_core::foam::study::run_study(&study, &part, ctx)),
        Needs::Gpu => state.jobs.submit(&label, &name, Kind::Gpu, move |ctx| sim_core::lbm::study::run_study(&study, &part, ctx)),
    };
    state.log_job = Some(id);
    Ok(id)
}

/// Colours a study's body by condition in the 3D view, and says what each one covers, so that a wrong
/// face can be seen before anything is solved. Refuses a pick that has gone stale.
pub fn preview_faces(state: &mut SimState, slot: &mut Slot, index: usize) -> Result<Vec<sim_core::check::Coverage>, String> {
    let study = state.studies.get(index).cloned().ok_or("That study no longer exists.")?;
    let body = study.body.clone().ok_or("Choose a body first.")?;
    let part = super::tools::partition_of(state, slot, &body)?;
    let second = match &study.second_body {
        Some(b) => Some(super::tools::partition_of(state, slot, b)?),
        None => None,
    };
    let (set, rows) = sim_core::check::preview(&study, &part, second.as_ref()).map_err(|e| format!("{e:#}"))?;
    let mut v = view::View::new(0, std::sync::Arc::new(set));
    v.hides = study.scene_object().map(str::to_string);
    v.legend = Some(rows.clone());
    state.view = Some(v);
    state.active = Some(index);
    Ok(rows)
}

/// An area in the unit that reads best: `format_value`'s prefixes mean something else on a square metre.
fn area_text(a: f64) -> String {
    if a >= 0.1 {
        format!("{a:.3} m²")
    } else if a >= 1e-4 {
        format!("{:.2} cm²", a * 1e4)
    } else {
        format!("{:.2} mm²", a * 1e6)
    }
}

/// [`start_run`], with any refusal shown in the panel.
pub fn run(state: &mut SimState, slot: &mut Slot, index: usize) {
    if let Err(e) = start_run(state, slot, index) {
        state.message = Some(e);
    }
}

/// Opens a finished run's results, with the study they belong to brought up beside them.
pub fn show_result(state: &mut SimState, job: u64, study: &str, set: std::sync::Arc<ResultSet>) {
    let at = state.studies.iter().position(|s| s.name == study);
    if at.is_some() {
        state.active = at;
    }
    let mut v = view::View::new(job, set);
    // A scene object's result is drawn where the object stands.
    v.hides = at.and_then(|i| state.studies[i].scene_object()).map(str::to_string);
    state.view = Some(v);
}

/// A setup for the solvers an analysis needs, after asking.
fn install_ui(ui: &mut egui::Ui, state: &mut SimState, needs: Needs) -> bool {
    match needs {
        Needs::NativeFea if !state.installed.native_fea => {
            ui.colored_label(ACCENT, "This needs Gmsh and CalculiX, which aren't installed.");
            ui.label(RichText::new("About 100 MB, from their own sites, GPL-licensed, run as separate programs.").color(theme::WEAK).size(11.5));
            if ui.button("Install them").clicked() {
                state.jobs.submit("Installing Gmsh and CalculiX", "", Kind::Install, |ctx| {
                    sim_core::provision::install_native(&ctx.cancel, |s| ctx.set(s, 0.5))?;
                    Ok(ResultSet::default())
                });
            }
            false
        }
        Needs::Foam | Needs::Elmer if !state.installed.wsl_available => {
            ui.colored_label(theme::DANGER, "This needs Windows Subsystem for Linux, which isn't available on this PC.");
            false
        }
        Needs::Foam | Needs::Elmer if !state.installed.solvers_distro => {
            ui.colored_label(ACCENT, "This needs OpenFOAM and Elmer, in a private Linux environment.");
            ui.label(RichText::new("About 4 GB. Your own Linux distros aren't touched.").color(theme::WEAK).size(11.5));
            if ui.button("Set it up").clicked() {
                state.jobs.submit("Setting up the solver environment", "", Kind::Install, |ctx| {
                    sim_core::provision::install_wsl(&ctx.cancel, |l| {
                        ctx.set("Installing", 0.5);
                        ctx.log(l);
                    })?;
                    Ok(ResultSet::default())
                });
            }
            false
        }
        _ => true,
    }
}

fn study_ui(ui: &mut egui::Ui, state: &mut SimState, slot: &mut Slot, workspace: &mut Workspace, index: usize) {
    let analysis = catalog::analysis(state.studies[index].analysis);
    ui.horizontal(|ui| {
        ui.label(RichText::new(analysis.label).color(ACCENT));
        if analysis.readiness == catalog::Readiness::Experimental {
            ui.label(RichText::new("beta").color(theme::WEAK).size(11.0));
        }
    });

    // Geometry.
    theme::section(ui, "Geometry", true, |ui| {
        let bodies = slot.host.as_ref().map_or_else(Vec::new, |h| bridge::bodies(&h.app.session));
        let objects = state.scene.names.clone();
        if bodies.is_empty() && objects.is_empty() {
            ui.label(RichText::new("Model a part in the design workspace, or place an object in the scene.").color(theme::WEAK));
        } else {
            let s = &mut state.studies[index];
            let before = s.body.clone();
            let shown = s.body.as_deref().map_or_else(|| "Choose a body".to_string(), |b| b.strip_prefix(sim_core::study::SCENE_PREFIX).map_or_else(|| b.to_string(), |o| format!("{o} (scene)")));
            egui::ComboBox::from_id_salt("body").selected_text(shown).show_ui(ui, |ui| {
                for b in &bodies {
                    if ui.selectable_label(s.body.as_deref() == Some(b), b).clicked() {
                        s.body = Some(b.clone());
                    }
                }
                if !objects.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("In the scene").color(theme::WEAK).size(11.0));
                }
                for o in &objects {
                    let id = format!("{}{o}", sim_core::study::SCENE_PREFIX);
                    if ui.selectable_label(s.body.as_deref() == Some(&id), o).clicked() {
                        s.body = Some(id);
                    }
                }
            });
            // Faces are numbered per body: picks on another body mean nothing.
            if s.body != before {
                for c in &mut s.conditions {
                    c.faces.clear();
                }
            }
        }
        if ui.button(theme::label(icon::CUBE, "Open design")).clicked() {
            *workspace = Workspace::Design;
        }
        // Nonlinear can take a second body, to press on.
        if state.studies[index].analysis == catalog::AnalysisId::NonlinearMechanical && !bodies.is_empty() {
            let s = &mut state.studies[index];
            let before = s.second_body.clone();
            ui.horizontal(|ui| {
                ui.label("Touching");
                egui::ComboBox::from_id_salt("second_body").selected_text(s.second_body.clone().unwrap_or_else(|| "Nothing".into())).show_ui(ui, |ui| {
                    if ui.selectable_label(s.second_body.is_none(), "Nothing").clicked() {
                        s.second_body = None;
                    }
                    for b in bodies.iter().filter(|b| s.body.as_deref() != Some(b.as_str())) {
                        if ui.selectable_label(s.second_body.as_deref() == Some(b), b).clicked() {
                            s.second_body = Some(b.clone());
                        }
                    }
                });
            });
            // Faces picked on the second body mean nothing on a different one.
            if s.second_body != before {
                for c in &mut s.conditions {
                    c.faces.retain(|f| !f.second);
                }
            }
        }
        // What a scene object can and can't be used for, once it's been read.
        if let Some(object) = state.studies[index].scene_object() {
            if let Some(error) = state.scene.errors.get(object) {
                ui.colored_label(theme::DANGER, error);
            } else if let Some(body) = state.scene.bodies.get(object) {
                let r = &body.regions;
                ui.label(RichText::new(format!("{} faces, found from the mesh's creases. {} triangles.", r.partition.faces, r.partition.triangles.len())).color(theme::WEAK).size(11.5));
                if matches!(analysis.needs, Needs::NativeFea | Needs::Elmer)
                    && let Some(problem) = r.solid_problem()
                {
                    ui.colored_label(theme::DANGER, problem);
                }
            }
        }
    });

    // Material.
    theme::section(ui, "Material", true, |ui| {
        let s = &mut state.studies[index];
        if s.uses_solid() {
            egui::ComboBox::from_id_salt("solid").selected_text(s.material.name.clone()).show_ui(ui, |ui| {
                for m in materials::solids() {
                    if ui.selectable_label(s.material.name == m.name, &m.name).clicked() {
                        s.material = m;
                        s.material_confirmed = true;
                    }
                }
            });
            let m = &s.material;
            let numbers = if s.is_fea() { format!("E {:.0} GPa, ν {}, {:.0} kg/m³", m.young / 1e9, m.poisson, m.density) } else { format!("Conducts {:.1} W/mK, {:.0} kg/m³", m.conductivity, m.density) };
            ui.label(RichText::new(numbers).color(theme::WEAK).size(11.5));
            // A scene object doesn't say what it's made of, and the first in the list isn't an answer.
            if s.scene_object().is_some() {
                ui.checkbox(&mut s.material_confirmed, "This is what it's made of");
            }
        }
        if s.uses_fluid() {
            egui::ComboBox::from_id_salt("fluid").selected_text(s.fluid.name.clone()).show_ui(ui, |ui| {
                for m in materials::fluids() {
                    if ui.selectable_label(s.fluid.name == m.name, &m.name).clicked() {
                        s.fluid = m;
                    }
                }
            });
        }
        if !s.uses_solid() && !s.uses_fluid() {
            ui.label(RichText::new("The part stands in air. Only its shape and its potentials or coil count.").color(theme::WEAK).size(11.5));
        }
    });

    // Conditions.
    theme::section(ui, "Conditions", true, |ui| {
        let offered = state.studies[index].offered_conditions();
        ui.menu_button(theme::label(icon::PLUS, "Add"), |ui| {
            for kind in offered {
                if ui.button(kind.label()).clicked() {
                    state.studies[index].conditions.push(Condition { kind, faces: Vec::new() });
                    ui.close();
                }
            }
        });
        let mut remove = None;
        let count = state.studies[index].conditions.len();
        for c in 0..count {
            ui.add_space(3.0);
            egui::Frame::new().fill(theme::HOVER.gamma_multiply(0.5)).corner_radius(4.0).inner_margin(6).show(ui, |ui| {
                let condition = &mut state.studies[index].conditions[c];
                ui.horizontal(|ui| {
                    ui.label(RichText::new(condition.kind.label()).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button(icon::TRASH).clicked() {
                            remove = Some(c);
                        }
                    });
                });
                condition_ui(ui, &mut condition.kind);
                let faces = condition.faces.len();
                let text = if faces == 0 { "Pick faces".to_string() } else { format!("{faces} faces: pick again") };
                let on_scene = state.studies[index].scene_object().is_some();
                if state.scene_pick == Some((index, c)) {
                    ui.colored_label(ACCENT, "Click faces in the view. Right-drag turns it.");
                    if theme::primary_button(ui, "Done").clicked() {
                        state.scene_pick = None;
                    }
                } else if ui.button(theme::label(icon::CURSOR_CLICK, &text)).clicked() {
                    if on_scene {
                        state.scene_pick = Some((index, c));
                    } else {
                        state.picking = Some((index, c));
                        slot.prompt = Some(format!("Click the faces for {} (Ctrl for several).", state.studies[index].conditions[c].kind.label().to_lowercase()));
                        *workspace = Workspace::Design;
                    }
                }
            });
        }
        if let Some(c) = remove {
            state.studies[index].conditions.remove(c);
            state.scene_pick = None;
        }
        let can_check = state.studies[index].body.is_some() && state.studies[index].conditions.iter().any(|c| !c.faces.is_empty());
        ui.add_space(3.0);
        if ui.add_enabled(can_check, egui::Button::new(theme::label(icon::MAGNIFYING_GLASS, "Check faces"))).on_hover_text("Colour the body by condition and show each one's area, before running").clicked() {
            match preview_faces(state, slot, index) {
                Ok(_) => *workspace = Workspace::Scene,
                Err(e) => state.message = Some(e),
            }
        }
    });

    theme::section(ui, "Settings", false, |ui| settings_ui(ui, &mut state.studies[index]));

    ui.add_space(6.0);
    let needs = analysis.needs;
    if install_ui(ui, state, needs) {
        let problems = state.studies[index].problems();
        for p in &problems {
            ui.label(RichText::new(p).color(theme::WEAK).size(11.5));
        }
        if ui.add_enabled(problems.is_empty(), egui::Button::new(theme::label(icon::PLAY, "Run")).min_size(vec2(90.0, 28.0))).clicked() {
            run(state, slot, index);
        }
    }
}

fn jobs_ui(ui: &mut egui::Ui, state: &mut SimState) {
    let jobs = state.jobs.list();
    if jobs.is_empty() {
        return;
    }
    theme::section(ui, "Runs", true, |ui| {
        let mut any_install_done = false;
        for job in jobs.iter().rev().take(8) {
            ui.horizontal(|ui| {
                match &job.state {
                    State::Queued => {
                        ui.label(RichText::new("waiting").color(theme::WEAK));
                    }
                    State::Running { stage, progress } => {
                        ui.add(egui::ProgressBar::new(*progress).desired_width(70.0));
                        ui.label(RichText::new(stage).color(theme::WEAK).size(11.5));
                        if ui.small_button(icon::STOP).on_hover_text("Stop").clicked() {
                            state.jobs.cancel(job.id);
                        }
                    }
                    State::Done => {
                        ui.colored_label(theme::SUCCESS, icon::CHECK);
                        if job.name.starts_with("Install") || job.name.starts_with("Setting up") {
                            any_install_done = true;
                        }
                        if job.has_result && !job.study.is_empty() && ui.small_button("View").clicked() {
                            if let Some(set) = state.jobs.result(job.id) {
                                show_result(state, job.id, &job.study, set);
                            }
                        }
                    }
                    State::Failed { .. } => {
                        ui.colored_label(theme::DANGER, icon::WARNING_CIRCLE);
                    }
                    State::Cancelled => {
                        ui.label(RichText::new("stopped").color(theme::WEAK));
                    }
                    State::Interrupted => {
                        ui.label(RichText::new("interrupted").color(theme::WEAK));
                    }
                }
                ui.label(&job.name);
                if ui.small_button("log").clicked() {
                    state.log_job = if state.log_job == Some(job.id) { None } else { Some(job.id) };
                }
            });
            if let State::Failed { error } = &job.state {
                ui.label(RichText::new(error).color(theme::DANGER).size(11.5));
            }
        }
        if any_install_done && !state.installed.native_fea {
            state.installed = super::installed_now();
        }
        if let Some(id) = state.log_job {
            egui::ScrollArea::vertical().id_salt("job_log").max_height(120.0).stick_to_bottom(true).show(ui, |ui| {
                for l in state.jobs.log(id, 200) {
                    ui.label(RichText::new(l).monospace().size(10.5).color(theme::WEAK));
                }
            });
        }
    });
}

fn format_value(v: f32, unit: &str) -> String {
    let a = v.abs();
    // A plain number that small is a residual, and three decimals would make it 0.000.
    if unit.is_empty() && a > 0.0 && a < 1e-2 {
        return format!("{v:.2e}");
    }
    let (scaled, prefix) = if unit.is_empty() || a == 0.0 {
        (v, "")
    } else if a >= 1e9 {
        (v / 1e9, "G")
    } else if a >= 1e6 {
        (v / 1e6, "M")
    } else if a >= 1e3 {
        (v / 1e3, "k")
    } else if a < 1e-6 {
        (v * 1e9, "n")
    } else if a < 1e-3 {
        (v * 1e6, "µ")
    } else if a < 1.0 {
        (v * 1e3, "m")
    } else {
        (v, "")
    };
    format!("{scaled:.3} {prefix}{unit}")
}

/// A solver's history as a small chart, with its last value.
fn plot_ui(ui: &mut egui::Ui, plot: &sim_core::results::Plot) {
    let points: Vec<(f64, f64)> = plot.points.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()).collect();
    if plot.name == "heat balance" && points.len() == 2 {
        ui.label(format!("Heat going in {:.3} {unit}, carried out by the air {:.3} {unit}", points[0].1, points[1].1, unit = plot.unit));
        return;
    }
    let Some(last) = points.last().copied() else { return };
    ui.label(RichText::new(format!("{}: {} at the end", plot.name, format_value(last.1 as f32, &plot.unit))).size(11.5));
    if points.len() < 2 {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(290.0), 64.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::HOVER.gamma_multiply(0.5));
    let (x0, x1) = points.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (y0, y1) = points.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let pad = ((y1 - y0) * 0.08).max(f64::MIN_POSITIVE);
    let (y0, y1) = (y0 - pad, y1 + pad);
    let at = |p: &(f64, f64)| egui::pos2(rect.left() + ((p.0 - x0) / (x1 - x0).max(f64::MIN_POSITIVE)) as f32 * rect.width(), rect.bottom() - ((p.1 - y0) / (y1 - y0)) as f32 * rect.height());
    painter.add(egui::Shape::line(points.iter().map(at).collect(), egui::Stroke::new(1.5, ACCENT)));
    let font = egui::FontId::proportional(10.0);
    painter.text(rect.left_top() + vec2(3.0, 1.0), egui::Align2::LEFT_TOP, format_value(y1 as f32, &plot.unit), font.clone(), theme::WEAK);
    painter.text(rect.left_bottom() + vec2(3.0, -1.0), egui::Align2::LEFT_BOTTOM, format_value(y0 as f32, &plot.unit), font, theme::WEAK);
}

/// Ways to look inside a volume: a cut, an isosurface, streamlines.
fn looks_ui(ui: &mut egui::Ui, v: &mut view::View) {
    if v.set.mesh.tets.is_empty() {
        return;
    }
    let mut looks = v.looks.clone();
    let mut cut_on = looks.cut.is_some();
    ui.checkbox(&mut cut_on, "Cut plane");
    if cut_on {
        let (mut axis, mut at) = looks.cut.unwrap_or((1, 0.5));
        ui.horizontal(|ui| {
            for (i, name) in ["X", "Y", "Z"].iter().enumerate() {
                ui.selectable_value(&mut axis, i, *name);
            }
            ui.add(egui::Slider::new(&mut at, 0.0..=1.0).show_value(false));
        });
        looks.cut = Some((axis, at));
    } else {
        looks.cut = None;
    }
    let (lo, hi) = v.range;
    let mut iso_on = looks.iso.is_some();
    ui.checkbox(&mut iso_on, "Isosurface of this field");
    if iso_on {
        let mut level = looks.iso.unwrap_or(0.5);
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut level, 0.0..=1.0).show_value(false));
            ui.label(RichText::new(format!("{:.4}", lo + (hi - lo) * level)).size(11.5));
        });
        looks.iso = Some(level);
    } else {
        looks.iso = None;
    }
    if v.velocity().is_some() {
        let mut lines_on = looks.lines.is_some();
        ui.checkbox(&mut lines_on, "Streamlines");
        if lines_on {
            let mut count = looks.lines.unwrap_or(40);
            ui.add(egui::Slider::new(&mut count, 4..=200).text("lines"));
            looks.lines = Some(count);
            if let Some(top) = v.velocity_peak() {
                ui.label(RichText::new(format!("Coloured by speed, 0 to {}", format_value(top, "m/s"))).color(theme::WEAK).size(11.0));
            }
        } else {
            looks.lines = None;
        }
    }
    if v.looks_pending() {
        ui.label(RichText::new("Working it out...").color(theme::WEAK).size(11.0));
    }
    v.looks = looks;
}

/// What the colours of a study's face check mean.
fn faces_ui(ui: &mut egui::Ui, rows: &[sim_core::check::Coverage], range: (f32, f32)) -> bool {
    let mut close = false;
    theme::section(ui, "Faces", true, |ui| {
        for r in rows.iter().filter(|r| r.faces > 0) {
            let t = if range.1 > range.0 { (r.value - range.0) / (range.1 - range.0) } else { 0.0 };
            let [red, green, blue] = view::colour(t);
            ui.horizontal(|ui| {
                let (swatch, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(swatch, 2.0, Color32::from_rgb(red, green, blue));
                ui.label(format!("{}: {} face{}, {}", r.label, r.faces, if r.faces == 1 { "" } else { "s" }, area_text(r.area)));
            });
        }
        if ui.button("Close").clicked() {
            close = true;
        }
    });
    close
}

fn results_ui(ui: &mut egui::Ui, state: &mut SimState) {
    let Some(v) = state.view.as_mut() else { return };
    if let Some(rows) = &v.legend {
        if faces_ui(ui, rows, v.range) {
            state.view = None;
        }
        return;
    }
    let mut close = false;
    theme::section(ui, "Results", true, |ui| {
        let steps = v.set.steps.len();
        if steps > 1 {
            let kind = v.set.steps[v.step].kind;
            let mut step = v.step;
            ui.add(egui::Slider::new(&mut step, 0..=steps - 1).text(match kind {
                StepKind::Mode => "mode",
                StepKind::Frequency => "frequency",
                StepKind::Time => "step",
            }));
            if step != v.step {
                v.step = step;
                v.field = v.field.min(v.set.steps[step].fields.len().saturating_sub(1));
                v.dirty = true;
            }
        }
        let step = &v.set.steps[v.step];
        let value = match step.kind {
            StepKind::Mode | StepKind::Frequency => format!("{:.3} Hz", step.value),
            StepKind::Time => format!("t = {:.4} s", step.value),
        };
        ui.label(RichText::new(value).strong());
        let current = step.fields.get(v.field).map_or(String::new(), |f| f.name.clone());
        let names: Vec<String> = step.fields.iter().map(|f| f.name.clone()).collect();
        egui::ComboBox::from_id_salt("field").selected_text(&current).show_ui(ui, |ui| {
            for (i, n) in names.iter().enumerate() {
                if ui.selectable_label(i == v.field, n).clicked() {
                    v.field = i;
                    v.dirty = true;
                }
            }
        });
        // The legend.
        let unit = step.fields.get(v.field).map_or("", |f| f.unit.as_str()).to_string();
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(260.0), 14.0), Sense::hover());
        for i in 0..64 {
            let t = i as f32 / 63.0;
            let [r, g, b] = view::colour(t);
            let x0 = rect.left() + rect.width() * i as f32 / 64.0;
            ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x0 + rect.width() / 64.0 + 1.0, rect.bottom())), 0.0, Color32::from_rgb(r, g, b));
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(format_value(v.range.0, &unit)).size(11.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format_value(v.range.1, &unit)).size(11.5));
            });
        });
        if v.set.mesh.cut_start.is_some() && ui.checkbox(&mut v.show_walls, "Show walls").changed() {
            v.dirty = true;
        }
        if v.displacement().is_some() {
            ui.horizontal(|ui| {
                ui.label("Deformation");
                let max = (v.auto_scale * 5.0).max(1.0);
                if ui.add(egui::Slider::new(&mut v.deform, 0.0..=max).max_decimals(1)).changed() {
                    v.dirty = true;
                }
            });
            ui.label(RichText::new(if v.deform > 1.0 { format!("Exaggerated {:.0} times", v.deform) } else { "True scale".to_string() }).color(theme::WEAK).size(11.0));
            if ui.checkbox(&mut v.animate, "Animate").changed() {
                v.dirty = true;
            }
        }
        looks_ui(ui, v);
        for plot in v.set.plots.clone().iter().take(6) {
            plot_ui(ui, plot);
        }
        if ui.button("Close results").clicked() {
            close = true;
        }
    });
    if close {
        state.view = None;
    }
}

pub fn show(ctx: &egui::Context, state: &mut SimState, slot: &mut Slot, workspace: &mut Workspace) {
    let mut open = true;
    egui::Window::new("Simulations").open(&mut open).default_pos([ctx.content_rect().right() - 640.0, 176.0]).default_width(340.0).resizable(true).show(ctx, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ctx.content_rect().height() - 140.0).show(ui, |ui| {
            if ui.button(theme::label(icon::PLUS, "New simulation")).clicked() {
                state.dialog = Some(dialog::Dialog::default());
            }
            if let Some(m) = state.message.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ACCENT, m);
                    if ui.small_button(icon::X).clicked() {
                        state.message = None;
                    }
                });
            }
            ui.add_space(4.0);
            // A face check's colours mean nothing without their key, so it comes first, in plain sight.
            let checking_faces = state.view.as_ref().is_some_and(|v| v.legend.is_some());
            if checking_faces {
                results_ui(ui, state);
            }
            let names: Vec<String> = state.studies.iter().map(|s| s.name.clone()).collect();
            for (i, name) in names.iter().enumerate() {
                if ui.selectable_label(state.active == Some(i), name).clicked() {
                    state.active = Some(i);
                }
            }
            if let Some(i) = state.active.filter(|i| *i < state.studies.len()) {
                ui.separator();
                study_ui(ui, state, slot, workspace, i);
            }
            jobs_ui(ui, state);
            if !checking_faces {
                results_ui(ui, state);
            }
            ui.add_space(6.0);
            if ui.small_button("Licences").clicked() {
                state.notices_open = true;
            }
        });
    });
    if !open {
        state.panel_open = false;
        state.scene_pick = None;
    }
}
