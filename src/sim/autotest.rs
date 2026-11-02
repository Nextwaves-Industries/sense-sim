//! `--sim-test OUT`: the whole path in the real app. A beam is modelled in SolidCraft; its end faces
//! are found by geometry and picked as the UI picks them; a Static study runs through Gmsh and
//! CalculiX; and the deflection must match beam theory. If labels went wrong anywhere, the answer
//! would too. Writes `OUT.json`, and pictures of the dialog and the results.

use bevy::app::AppExit;
use bevy::input::ButtonState;
use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{CursorMoved, PrimaryWindow, WindowEvent};
use serde_json::json;
use sim_core::catalog::AnalysisId;
use sim_core::jobs::State;
use sim_core::results::ResultSet;
use sim_core::study::{BcKind, Condition, FaceRef, Study};
use solidcraft_engine::io::MeshQuality;

use super::{SimState, dialog, panel, view};
use crate::design::{Slot, Workspace, bridge};

#[derive(Resource)]
pub struct SimTest {
    pub out: String,
    frame: u32,
    step: usize,
    at: u32,
    job: u64,
    checks: Vec<serde_json::Value>,
    pub all: bool,
    /// Where the plank's free end is, along x.
    plank_end: f64,
    /// The tools' turn to be tried, and to read what their run gave.
    tools_due: bool,
    tools_results: bool,
    saved: Vec<Study>,
    problems_empty: bool,
    body_fitted_peak: f64,
    /// Where in the scene the test clicks.
    pick_target: Vec3,
    /// The first of two solvers' answers on one case.
    first_answer: Option<std::sync::Arc<ResultSet>>,
}

/// Mean speed in the pipe, for a Reynolds number of about 66: laminar, developed by mid-length.
const PIPE_SPEED: f64 = 0.02;
/// Past a 100 mm cube, for a Reynolds number of about 100: steady, and resolved by both solvers.
const CUBE_SPEED: f64 = 0.015;

impl SimTest {
    pub fn new(out: String) -> Self {
        Self { out, frame: 0, step: 0, at: 0, job: 0, checks: Vec::new(), all: std::env::args().any(|a| a == "--sim-test-all"), plank_end: 0.0, tools_due: false, tools_results: false, saved: Vec::new(), problems_empty: false, body_fitted_peak: 0.0, pick_target: Vec3::ZERO, first_answer: None }
    }
    fn check(&mut self, name: &str, pass: bool, detail: impl Into<serde_json::Value>) {
        let detail = detail.into();
        println!("[{}] {name} {detail}", if pass { "pass" } else { "FAIL" });
        self.checks.push(json!({ "name": name, "pass": pass, "detail": detail }));
    }
}

/// The tagged face whose triangles all lie at `x`, and a point on it.
fn face_at_x(part: &sim_core::geometry::Partition, x: f64) -> Option<FaceRef> {
    for face in 0..part.faces {
        let tris: Vec<_> = (0..part.triangles.len()).filter(|i| part.tri_face[*i] == face).collect();
        if !tris.is_empty() && tris.iter().all(|i| part.triangle(*i).iter().all(|p| (p[0] - x).abs() < 1e-6)) {
            let [a, b, c] = part.triangle(tris[0]);
            return Some(FaceRef { face, point: [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0], second: false });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    mut test: ResMut<SimTest>,
    mut commands: Commands,
    mut state: ResMut<SimState>,
    mut workspace: ResMut<Workspace>,
    mut slot: NonSendMut<Slot>,
    mut exit: MessageWriter<AppExit>,
    mut pending: ResMut<crate::scene::PendingSpawns>,
    mut actions: ResMut<crate::scene::Actions>,
    looks: Query<(), With<view::LooksMarker>>,
    (status, mut window_events, mut mouse, mut orbit, camera, window_id, picks): (
        Res<crate::scene::Status>,
        MessageWriter<WindowEvent>,
        MessageWriter<MouseButtonInput>,
        Single<&mut crate::scene::OrbitCamera>,
        Single<(&Camera, &GlobalTransform), With<crate::scene::OrbitCamera>>,
        Single<Entity, With<PrimaryWindow>>,
        Query<(), With<super::scene_body::PickMarker>>,
    ),
) {
    let shown_looks = looks.iter().count();
    let shown_picks = picks.iter().count();
    let window_entity = *window_id;
    test.frame += 1;
    if test.frame == 5 {
        // A clean start: an earlier session's unsaved design would be brought back otherwise.
        let _ = std::fs::remove_file(crate::design::autosave_path());
        *workspace = Workspace::Design;
    }
    if slot.host.is_none() {
        if test.frame > 1500 {
            test.check("the design workspace starts", false, slot.failed.clone().unwrap_or_default());
            finish(&mut test, &mut slot, &mut exit);
        }
        return;
    }
    if test.frame < test.at {
        return;
    }
    let step = test.step;
    test.step += 1;
    test.at = test.frame + 20;
    match step {
        0 => {
            let h = slot.host.as_mut().unwrap();
            h.app.home.open = false;
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Section"}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 0], "p1": [1000, 50]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "50 mm", "body_name": "Beam"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a 1 m beam is modelled in SolidCraft", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            h.app.cam.set_view(solidcraft_engine::render::StandardView::Iso);
            h.app.fit_view();
        }
        1 => {
            *workspace = Workspace::Scene;
            state.panel_open = true;
            state.dialog = Some(dialog::Dialog { analysis: Some(AnalysisId::ConjugateHeatTransferVoxel), ..Default::default() });
        }
        2 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_dialog.png", test.out)));
        }
        3 => {
            state.dialog = None;
            let h = slot.host.as_ref().unwrap();
            let bodies = bridge::bodies(&h.app.session);
            test.check("the body reaches the study", bodies == ["Beam"], format!("{bodies:?}"));
            let part = bridge::partition(&h.app.session, "Beam", MeshQuality::Fine).unwrap();
            let closed = part.is_closed();
            test.check("the tagged surface is closed", closed, format!("{} faces, {} triangles", part.faces, part.triangles.len()));
            let (fixed, tip) = (face_at_x(&part, 0.0), face_at_x(&part, 1.0));
            test.check("both end faces are found", fixed.is_some() && tip.is_some(), "");
            let mut s = Study::new("Beam, static".into(), AnalysisId::Static);
            s.body = Some("Beam".into());
            s.settings.element_size = 0.012;
            s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![fixed.unwrap()] });
            s.conditions.push(Condition { kind: BcKind::Force { force: [0.0, 0.0, -1000.0] }, faces: vec![tip.unwrap()] });
            test.check("the study has what it needs", s.problems().is_empty(), format!("{:?}", s.problems()));
            state.studies.push(s);
            state.active = Some(0);
            // Before running: what the picks really land on. 50 by 50 mm at each end, and 4 sides left.
            let rows = panel::preview_faces(&mut state, &mut slot, 0);
            let right = rows.as_ref().is_ok_and(|r| {
                let near = |i: usize, area: f64, faces: usize| r.get(i).is_some_and(|c| (c.area - area).abs() < area * 0.001 && c.faces == faces);
                near(0, 0.0025, 1) && near(1, 0.0025, 1) && near(2, 0.2, 4)
            });
            test.check("checking faces measures what each condition covers", right, format!("{rows:?}"));
            test.step = 70;
        }
        70 => {
            let shown = state.view.as_ref().is_some_and(|v| v.legend.is_some() && v.range == (0.0, 2.0));
            test.check("checking faces colours the body by condition", shown, format!("{:?}", state.view.as_ref().map(|v| v.range)));
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_faces.png", test.out)));
        }
        71 => {
            // A pick the design has moved out from under: refused, not coloured or solved.
            let mut moved = state.studies[0].clone();
            moved.conditions[0].faces[0].point = [0.5, 0.025, 0.025];
            state.studies.push(moved);
            let refused = panel::preview_faces(&mut state, &mut slot, 1);
            test.check("checking faces refuses a pick that has gone stale", refused.as_ref().is_err_and(|e| e.contains("pick the face again")), format!("{refused:?}"));
            state.studies.pop();
            state.active = Some(0);
            state.view = None;
            panel::run(&mut state, &mut slot, 0);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
            test.step = 4;
        }
        // Wait for the solve.
        4 => {
            let info = state.jobs.list().into_iter().find(|j| j.id == test.job);
            match info.map(|j| j.state) {
                Some(State::Done) => {}
                Some(State::Failed { error }) => {
                    test.check("the solve finishes", false, error);
                    finish(&mut test, &mut slot, &mut exit);
                    return;
                }
                Some(_) => {
                    test.step = 4;
                    test.at = test.frame + 10;
                    if test.frame > 40000 {
                        test.check("the solve finishes", false, "timed out");
                        finish(&mut test, &mut slot, &mut exit);
                    }
                    return;
                }
                None => {
                    test.check("the solve starts", false, state.message.clone().unwrap_or_default());
                    finish(&mut test, &mut slot, &mut exit);
                    return;
                }
            }
            let set: std::sync::Arc<ResultSet> = state.jobs.result(test.job).expect("a finished job has a result");
            let last = set.steps.last().expect("a step");
            let disp = last.field("DISP").expect("displacement");
            let (mut sum, mut n) = (0.0f64, 0.0f64);
            for (i, p) in set.mesh.nodes.iter().enumerate() {
                if (p[0] - 1.0).abs() < 1e-6 {
                    sum += f64::from(disp.at(i)[2]);
                    n += 1.0;
                }
            }
            let tip = -sum / n;
            let err = (tip - 3.054e-3).abs() / 3.054e-3;
            test.check("the deflection matches beam theory within 1%", err < 0.01, format!("{:.4} mm against 3.054 mm, {:.2}% off", tip * 1000.0, err * 100.0));
            let (lo, hi) = set.mesh.nodes.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            test.check("results are in metres", (hi - lo - 1.0).abs() < 1e-6, format!("x spans {lo} to {hi}"));
            state.view = Some(view::View::new(test.job, set));
        }
        5 => {}
        6 => {
            let v = state.view.as_ref().unwrap();
            let (min, max) = v.range;
            test.check("the view shows a stress range", max > min && max > 1e6, format!("{min:.3e} to {max:.3e} Pa"));
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_results.png", test.out)));
        }
        7 => {
            // SENSESIM_TEST_USD: straight to the USD object and its face picking, for working on those.
            if std::env::var_os("SENSESIM_TEST_USD").is_some() {
                test.step = 24;
                return;
            }
            // SENSESIM_TEST_FROM: straight to a later step, for working on it.
            if let Some(from) = std::env::var("SENSESIM_TEST_FROM").ok().and_then(|v| v.parse().ok()) {
                test.step = from;
                return;
            }
            // On to the other solver families, on real bodies. Only with `--sim-test-all`.
            if !test.all {
                finish(&mut test, &mut slot, &mut exit);
                return;
            }
            state.view = None;
            let h = slot.host.as_mut().unwrap();
            // A pipe for the flow runs, well away from the beam.
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "YZ", "name": "Bore"}},
                {"command": "sketch.circle.center", "params": {"center": [-300, 0], "diameter": 50}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "1000 mm", "body_name": "Pipe"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a pipe is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let bodies = bridge::bodies(&h.app.session);
            test.check("both bodies exist", bodies.iter().any(|b| b == "Pipe") && bodies.iter().any(|b| b == "Beam"), format!("{bodies:?}"));
            let part = bridge::partition(&h.app.session, "Beam", MeshQuality::Fine).unwrap();
            // Electrostatics between the beam's ends.
            let mut s = Study::new("Beam, potential".into(), AnalysisId::Electromagnetics);
            s.body = Some("Beam".into());
            s.settings.element_size = 0.03;
            s.conditions.push(Condition { kind: BcKind::Potential { value: 0.0 }, faces: vec![face_at_x(&part, 0.0).unwrap()] });
            s.conditions.push(Condition { kind: BcKind::Potential { value: 100.0 }, faces: vec![face_at_x(&part, 1.0).unwrap()] });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        8 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 7) else { return };
            let v = set.steps[0].field("potential").or_else(|| set.steps[0].fields.first()).expect("a field");
            let (mut sum, mut n) = (0.0f64, 0.0f64);
            for (i, p) in set.mesh.nodes.iter().enumerate() {
                if (p[0] - 0.5).abs() < 0.03 {
                    sum += f64::from(v.at(i)[0]);
                    n += 1.0;
                }
            }
            let mid = sum / n;
            test.check("Elmer: the potential halfway along is half the voltage", (mid - 50.0).abs() < 1.0, format!("{mid:.2} V against 50 V"));
            // Laminar flow through the pipe, on OpenFOAM through snappyHexMesh.
            let h = slot.host.as_ref().unwrap();
            let part = bridge::partition(&h.app.session, "Pipe", MeshQuality::Fine).unwrap();
            let mut s = Study::new("Pipe, flow".into(), AnalysisId::Incompressible);
            s.body = Some("Pipe".into());
            s.settings.element_size = 0.008;
            s.conditions.push(Condition { kind: BcKind::VelocityInlet { speed: PIPE_SPEED }, faces: vec![face_at_x(&part, 0.0).unwrap()] });
            s.conditions.push(Condition { kind: BcKind::PressureOutlet { pressure: 0.0 }, faces: vec![face_at_x(&part, 1.0).unwrap()] });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        9 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 8) else { return };
            let p = set.steps[0].field("p").expect("pressure");
            // The pipe's axis, from its nodes.
            let (ylo, yhi, zlo, zhi) = set.mesh.nodes.iter().fold((f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY), |(a, b, c, d), n| (a.min(n[1]), b.max(n[1]), c.min(n[2]), d.max(n[2])));
            let pipe_axis = ((ylo + yhi) / 2.0, (zlo + zhi) / 2.0);
            // Mean kinematic pressure over the nodes near the axis at two stations, past the entry.
            let station = |x: f64| {
                let (mut sum, mut n) = (0.0f64, 0.0f64);
                for (i, c) in set.mesh.nodes.iter().enumerate() {
                    if (c[0] - x).abs() < 0.02 {
                        sum += f64::from(p.at(i)[0]);
                        n += 1.0;
                    }
                }
                sum / n.max(1.0)
            };
            // On the axis a developed laminar flow runs at twice its mean speed, all the way along.
            let u = set.steps[0].field("U").expect("velocity");
            let axis = |x: f64| {
                let mut best = 0.0f64;
                for (i, c) in set.mesh.nodes.iter().enumerate() {
                    if (c[0] - x).abs() < 0.02 {
                        let (dy, dz) = (c[1] - pipe_axis.0, c[2] - pipe_axis.1);
                        if (dy * dy + dz * dz).sqrt() < 0.008 {
                            let v = u.at(i);
                            best = best.max(f64::from(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
                        }
                    }
                }
                best
            };
            let profile: Vec<String> = [0.3, 0.5, 0.7, 0.9, 0.98].iter().map(|x| format!("{x}: {:.4}", axis(*x))).collect();
            let top = u.range().map_or(0.0, |r| f64::from(r.1));
            test.check("OpenFOAM: the axis runs at twice the mean speed, 0.04 m/s, mid-pipe", (axis(0.7) - 2.0 * PIPE_SPEED).abs() / (2.0 * PIPE_SPEED) < 0.15, format!("{} m/s along x; fastest anywhere {top:.4}", profile.join(", ")));
            let gradient = (station(0.5) - station(0.9)) / 0.4;
            let air = sim_core::materials::fluids().remove(0);
            let nu = air.viscosity / air.density;
            let exact = 32.0 * nu * PIPE_SPEED / (0.05f64 * 0.05);
            let err = (gradient - exact).abs() / exact;
            test.check("OpenFOAM: pressure drop matches Hagen-Poiseuille within 10%", err < 0.10, format!("{gradient:.5} against {exact:.5} m²/s² per m, {:.1}% off", err * 100.0));
            state.view = Some(view::View::new(test.job, set));
        }
        10 => {}
        11 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_flow.png", test.out)));
        }
        12 => {
            // Cooling on the GPU: the heat going in must come out with the air.
            state.view = None;
            let h = slot.host.as_ref().unwrap();
            let part = bridge::partition(&h.app.session, "Beam", MeshQuality::Fine).unwrap();
            let mut s = Study::new("Beam, cooling".into(), AnalysisId::ConjugateHeatTransferVoxel);
            s.body = Some("Beam".into());
            s.settings.resolution = 32;
            s.settings.free_stream = 2.0;
            s.conditions.push(Condition { kind: BcKind::HeatFlux { flux: 2000.0 }, faces: vec![face_at_x(&part, 0.0).unwrap()] });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        13 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 12) else { return };
            let balance = set.plots.iter().find(|p| p.name == "heat balance").expect("a heat balance");
            let (power, carried) = (balance.points[0].1, balance.points[1].1);
            test.check("GPU cooling: the heat balance closes within 25%", (carried - power).abs() / power < 0.25, format!("{power:.2} W in, {carried:.2} W out"));
            let t = set.steps[0].field("T").expect("temperature");
            let (lo, hi) = t.range().unwrap_or((0.0, 0.0));
            test.check("GPU cooling: the part is warmer than the air", hi > lo && lo > 19.0, format!("{lo:.1} to {hi:.1} °C"));
            state.view = Some(view::View::new(test.job, set));
            // The panel shows the study whose results these are.
            state.active = state.studies.iter().position(|s| s.name == "Beam, cooling");
        }
        14 => {}
        15 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_cooling.png", test.out)));
        }
        16 => {
            // Inside the volume: a cut and streamlines, worked out on their own thread.
            let v = state.view.as_mut().unwrap();
            v.looks = view::Looks { cut: Some((2, 0.3)), iso: None, lines: Some(30) };
        }
        17 => {
            if state.view.as_ref().unwrap().looks_pending() {
                test.step = 17;
                test.at = test.frame + 10;
                return;
            }
        }
        18 => {
            test.check("the view draws a cut plane and streamlines", shown_looks > 0, format!("{shown_looks} meshes inside the volume"));
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_looks.png", test.out)));
        }
        19 => {}
        20 => {
            // Wind over a tower, from two directions.
            state.view = None;
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Footprint"}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 500000], "p1": [20000, 520000]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "60000 mm", "body_name": "Tower"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a tower is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let mut s = Study::new("Tower, wind".into(), AnalysisId::PedestrianWindComfort);
            s.body = Some("Tower".into());
            s.settings.resolution = 16;
            s.settings.directions = 2;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        21 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 20) else { return };
            let worst = set.steps[0].field("Worst speed-up").expect("the worst speed-up");
            let (lo, hi) = worst.range().unwrap_or((0.0, 0.0));
            let finite = worst.data.iter().all(|v| v.is_finite());
            test.check("wind comfort: a map of speed-up comes out", finite && hi > 0.3 && hi < 5.0 && hi > lo, format!("{lo:.2} to {hi:.2} times the wind speed"));
            let drag = set.plots.iter().find(|p| p.name == "drag").and_then(|p| p.points.last()).map_or(0.0, |p| p.1);
            test.check("wind comfort: the wind pushes the tower downstream", drag > 0.0, format!("{drag:.0} N"));
            panel::show_result(&mut state, test.job, "Tower, wind", set);
        }
        22 => {}
        23 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_wind.png", test.out)));
        }
        24 => {
            // A USD object from the scene, simulated: a plank, wider than it is thick, so that the wrong
            // axis would give a very different answer.
            state.view = None;
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Plank"}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 900000], "p1": [1000, 900050]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "20 mm", "body_name": "Plank"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a plank is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let asset = bridge::bake(&h.app.session, "Plank").expect("a USD asset");
            pending.0.push(crate::scene::PendingSpawn { asset, transform: Transform::default(), name: Some("Plank".into()), joints: None, select: false, locked: false });
            let mut s = Study::new("Plank, static".into(), AnalysisId::Static);
            s.body = Some(format!("{}Plank", sim_core::study::SCENE_PREFIX));
            s.settings.element_size = 0.008;
            state.studies.push(s);
        }
        25 => {
            // Until it has loaded and been read.
            let Some(body) = state.scene.bodies.get("Plank") else {
                test.step = 25;
                test.at = test.frame + 10;
                if test.frame > 40000 {
                    test.check("the plank reaches the scene", false, format!("{:?}", state.scene.errors));
                    finish(&mut test, &mut slot, &mut exit);
                }
                return;
            };
            let part = body.regions.partition.clone();
            let problem = body.regions.solid_problem();
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for p in &part.positions {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
            let right = (size[0] - 1.0).abs() < 1e-4 && (size[1] - 0.05).abs() < 1e-4 && (size[2] - 0.02).abs() < 1e-4;
            test.check("a scene object reads as a closed solid, the right way up, with its faces", right && problem.is_none() && part.faces == 6, format!("{size:?}, {} faces, {problem:?}", part.faces));
            let last = state.studies.len() - 1;
            let (a, b) = (face_at_x(&part, lo[0]), face_at_x(&part, hi[0]));
            test.check("its end faces are found", a.is_some() && b.is_some(), "");
            test.plank_end = hi[0];
            let s = &mut state.studies[last];
            s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![a.unwrap()] });
            s.conditions.push(Condition { kind: BcKind::Force { force: [0.0, 0.0, -100.0] }, faces: vec![b.unwrap()] });
            let asked = s.problems();
            test.check("a scene object's study asks for its material to be confirmed", asked.iter().any(|p| p.contains("Confirm the material")), format!("{asked:?}"));
            state.studies[last].material_confirmed = true;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        26 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 25) else { return };
            let disp = set.steps.last().unwrap().field("DISP").expect("displacement");
            let (mut sum, mut n) = (0.0f64, 0.0f64);
            for (i, p) in set.mesh.nodes.iter().enumerate() {
                if (p[0] - test.plank_end).abs() < 1e-5 {
                    sum += f64::from(disp.at(i)[2]);
                    n += 1.0;
                }
            }
            let young = state.studies.last().unwrap().material.young;
            let exact = 100.0 / (3.0 * young * (0.05 * 0.02f64.powi(3) / 12.0));
            let tip = -sum / n.max(1.0);
            let err = (tip - exact).abs() / exact;
            test.check("a USD object bends as beam theory says, about the right axis, within 2%", err < 0.02, format!("{:.3} mm against {:.3} mm, {:.2}% off", tip * 1e3, exact * 1e3, err * 100.0));
            panel::show_result(&mut state, test.job, "Plank, static", set);
        }
        27 => {
            test.check("a scene object's result takes the object's place", state.view.as_ref().and_then(|v| v.hides.as_deref()) == Some("Plank"), "");
        }
        28 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_usd.png", test.out)));
            // On to picking its faces by hand, then back.
            test.step = 60;
        }
        60 => {
            // Clicks on a USD object's face, through the same input the mouse uses, at a place on screen
            // worked out by the camera: it looks at the plank's far end.
            state.view = None;
            let mut s = Study::new("Plank, picked".into(), AnalysisId::Static);
            s.body = Some(format!("{}Plank", sim_core::study::SCENE_PREFIX));
            s.conditions.push(Condition { kind: BcKind::Fixed, faces: Vec::new() });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            state.active = Some(last);
            state.scene_pick = Some((last, 0));
            let part = &state.scene.bodies["Plank"].regions.partition;
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for p in &part.positions {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            // Solver (x, y, z) is scene (x, z, -y). The middle of the end face at the largest x.
            let end = Vec3::new(hi[0] as f32, ((lo[2] + hi[2]) / 2.0) as f32, -((lo[1] + hi[1]) / 2.0) as f32);
            test.pick_target = end;
            orbit.focus = end;
            orbit.radius = 0.5;
            orbit.yaw = std::f32::consts::FRAC_PI_2;
            orbit.pitch = 0.2;
        }
        61 => {
            let (cam, gt) = *camera;
            let Ok(at) = cam.world_to_viewport(gt, test.pick_target) else {
                test.check("the plank's end is on screen", false, "");
                finish(&mut test, &mut slot, &mut exit);
                return;
            };
            commands.insert_resource(super::scene_body::TestPointer(at));
            // And for the UI. With no move to go on, it reads a press as at the window's corner, on the
            // ribbon, and keeps the click for itself.
            window_events.write(WindowEvent::CursorMoved(CursorMoved { window: window_entity, position: at, delta: None }));
        }
        62 | 66 => {
            mouse.write(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Pressed, window: window_entity });
            test.at = test.frame + 3;
        }
        63 | 67 => {
            mouse.write(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Released, window: window_entity });
            test.at = test.frame + 10;
        }
        64 => {
            let last = state.studies.len() - 1;
            let picked = state.studies[last].conditions[0].faces.clone();
            let part = state.scene.bodies["Plank"].regions.partition.clone();
            let hi = part.positions.iter().fold(f64::NEG_INFINITY, |a, p| a.max(p[0]));
            let end_face = face_at_x(&part, hi).map(|f| f.face);
            let right = picked.len() == 1 && Some(picked[0].face) == end_face;
            test.check("a click on a USD object picks the face under it", right, format!("{:?} against face {end_face:?}", picked.iter().map(|f| f.face).collect::<Vec<_>>()));
            let study = state.studies[last].clone();
            let on_it = sim_core::ccx::study::check_faces(&study, &part, 1e-3);
            test.check("the point it stored is on that face, so the run's own check accepts it", on_it.is_ok(), format!("{on_it:?}"));
            test.check("the picked face is lit up", shown_picks > 0, format!("{shown_picks} highlight meshes"));
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_pick.png", test.out)));
        }
        65 => {}
        68 => {
            let last = state.studies.len() - 1;
            let left = state.studies[last].conditions[0].faces.len();
            test.check("a second click takes the face back off", left == 0, format!("{left} faces"));
            state.scene_pick = None;
            commands.remove_resource::<super::scene_body::TestPointer>();
            // Back to a view from the side, for the pictures that follow.
            orbit.yaw = 0.6;
            orbit.pitch = 0.5;
            state.studies.pop();
            state.active = None;
            // The CAD kernel could take the whole app down: the scene is saved on the way into Design.
            let _ = std::fs::remove_file(crate::design::scene_safety_path());
            *workspace = Workspace::Design;
            test.step = 72;
        }
        72 => {
            let text = std::fs::read_to_string(crate::design::scene_safety_path()).unwrap_or_default();
            test.check("opening Design saves the scene first", text.contains("Plank"), format!("{} bytes", text.len()));
            test.check("and doesn't announce it", !status.0.contains("scene-before-design"), status.0.clone());
            *workspace = Workspace::Scene;
            if std::env::var_os("SENSESIM_TEST_USD").is_some() {
                finish(&mut test, &mut slot, &mut exit);
                return;
            }
            test.step = 29;
        }
        29 => {
            // The assistant's tools, on a part of its own: see `tools_step`.
            state.view = None;
            test.tools_due = true;
        }
        30 => {
            if test.tools_due {
                test.step = 30;
                test.at = test.frame + 5;
                return;
            }
            if test.job == 0 {
                finish(&mut test, &mut slot, &mut exit);
                return;
            }
        }
        31 => {
            let Some(_) = wait(&mut test, &state, &mut slot, &mut exit, 30) else { return };
            test.tools_results = true;
        }
        32 => {
            if test.tools_results {
                test.step = 32;
                test.at = test.frame + 5;
                return;
            }
            // Saving: the studies go in the layout, and the design beside it.
            let path = std::env::temp_dir().join("sense-sim-layout-test.json");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(super::persist::design_path(&path));
            test.saved = state.studies.clone();
            actions.0.push(crate::scene::Action::SaveLayout(path));
        }
        33 => {
            let path = std::env::temp_dir().join("sense-sim-layout-test.json");
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let (studies, skipped) = super::persist::parse_section(&v["simulation"]);
            let same = studies == test.saved;
            test.check("the studies are saved with the layout", same && skipped == 0, format!("{} studies, {skipped} skipped", studies.len()));
            let design = super::persist::design_path(&path);
            let bodies = std::fs::read(&design).ok().and_then(|b| solidcraft_engine::io::read_design(&b).ok()).map(|doc| bridge::bodies(&solidcraft_engine::Session::opened_at(doc, None, None)));
            let has = bodies.as_ref().is_some_and(|b| b.iter().any(|n| n == "Beam") && b.iter().any(|n| n == "Plank"));
            test.check("the design is saved beside it, and reads back", has, format!("{bodies:?}"));
            actions.0.push(crate::scene::Action::ClearScene);
        }
        34 => {
            test.check("a cleared scene has no studies", state.studies.is_empty(), format!("{}", state.studies.len()));
            actions.0.push(crate::scene::Action::LoadLayout(std::env::temp_dir().join("sense-sim-layout-test.json")));
        }
        35 => {
            let same = state.studies == test.saved;
            test.check("loading the layout brings the studies back", same, format!("{} studies", state.studies.len()));
            let _ = std::fs::remove_file(crate::design::autosave_path());
        }
        36 => {
            // A coil, on a ring from the design, against the closed form for a thick solenoid.
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Winding"}},
                {"command": "sketch.circle.center", "params": {"center": [0, -2000000], "diameter": 120}},
                {"command": "sketch.circle.center", "params": {"center": [0, -2000000], "diameter": 100}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "100 mm", "body_name": "Coil"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a coil's ring is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let mut s = Study::new("Coil".into(), AnalysisId::Electromagnetics);
            s.body = Some("Coil".into());
            s.settings.coil_ampere_turns = 1000.0;
            s.settings.axis = 2;
            s.settings.element_size = 0.02;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        37 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 36) else { return };
            // Radii 50 and 60 mm, 100 mm long, 1000 A round it: B = mu0 J r1 F(alpha, beta).
            let (r1, r2, l) = (0.05f64, 0.06f64, 0.1f64);
            let j = 1000.0 / ((r2 - r1) * l);
            let (alpha, beta) = (r2 / r1, l / (2.0 * r1));
            let f = beta * ((alpha + (alpha * alpha + beta * beta).sqrt()) / (1.0 + (1.0 + beta * beta).sqrt())).ln();
            let exact = sim_core::elmer::MU_0 * j * r1 * f;
            // The middle, from the ring's own nodes: its surface is drawn.
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for t in &set.mesh.surface[..set.mesh.cut_start.unwrap_or(set.mesh.surface.len())] {
                for n in t {
                    let p = set.mesh.nodes[*n as usize];
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            let centre: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
            let out = super::tools::probe_value(&set, 0, "magnetic flux density", centre);
            let got = out.as_ref().ok().map_or(f64::NAN, |v| v[2].abs());
            let err = (got - exact).abs() / exact;
            test.check("a coil's field at its centre matches the closed form within 5%", err < 0.05, format!("{:.4} mT against {:.4} mT, {:.2}% off {:?}", got * 1e3, exact * 1e3, err * 100.0, out.err()));
            panel::show_result(&mut state, test.job, "Coil", set);
        }
        38 => {}
        39 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_coil.png", test.out)));
        }
        40 => {
            // Mach 2 at a block: a bow shock, so the nose sees the pitot pressure behind a normal shock.
            state.view = None;
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Block"}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 4000000], "p1": [100, 4000100]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "100 mm", "body_name": "Block"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a block is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let mut s = Study::new("Block, Mach 2".into(), AnalysisId::Compressible);
            s.body = Some("Block".into());
            s.settings.free_stream = 2.0 * (1.4f64 * 8314.46 / 28.96 * 293.15).sqrt();
            s.settings.element_size = 0.05;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        41 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 40) else { return };
            // Rayleigh's pitot formula, at Mach 2 and gamma 1.4: 5.640 times the free stream's pressure.
            let (m, g) = (2.0f64, 1.4f64);
            let ratio = ((g + 1.0) * (g + 1.0) * m * m / (4.0 * g * m * m - 2.0 * (g - 1.0))).powf(g / (g - 1.0)) * (1.0 - g + 2.0 * g * m * m) / (g + 1.0);
            let top = set.steps[0].field("p").and_then(|p| p.range()).map_or(0.0, |r| f64::from(r.1));
            let err = (top / 101325.0 - ratio).abs() / ratio;
            test.check("Compressible: the nose sees the pitot pressure behind a bow shock, within 10%", err < 0.10, format!("{:.3} against {ratio:.3} times free stream, {:.1}% off", top / 101325.0, err * 100.0));
            panel::show_result(&mut state, test.job, "Block, Mach 2", set);
        }
        42 => {}
        43 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_mach.png", test.out)));
        }
        44 => {
            // A rotor: a cylinder spinning about its axis, in a zone from the panel's settings.
            state.view = None;
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Rotor"}},
                {"command": "sketch.circle.center", "params": {"center": [0, 6000], "diameter": 100}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "400 mm", "body_name": "Rotor"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a rotor is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let mut s = Study::new("Rotor".into(), AnalysisId::MultiPurpose);
            s.body = Some("Rotor".into());
            s.settings.free_stream = 1e-5;
            s.settings.compressible = false;
            // Slow: a Reynolds number of about 10, which this coarse mesh can resolve. At 60 rpm, on the
            // same mesh, numerical diffusion takes nine tenths of the swirl. The catalog says so.
            s.settings.rotor_rpm = 0.6;
            s.settings.axis = 2;
            s.settings.element_size = 0.08;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        45 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 44) else { return };
            let wall = 0.6 * std::f64::consts::TAU / 60.0 * 0.05;
            let top = set.steps[0].field("U").and_then(|u| u.range()).map_or(0.0, |r| f64::from(r.1));
            let err = (top - wall).abs() / wall;
            test.check("Multi-purpose: the fastest air moves with the rotor's surface, within 10%", err < 0.10, format!("{top:.4} m/s against {wall:.4} m/s, {:.1}% off", err * 100.0));
            // Turning about +z carries the air round the same way, falling off with radius as
            // omega R^2 / r in the middle of a long cylinder.
            let mid_z = {
                let (a, b) = set.mesh.nodes.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[2]), b.max(p[2])));
                (a + b) / 2.0
            };
            let (cx, cy) = (0.0f64, 6.0f64);
            let u = set.steps[0].field("U").expect("velocity");
            let swirl_at = |r0: f64| -> (f64, f64) {
                let (mut sum, mut n) = (0.0f64, 0.0f64);
                for (i, p) in set.mesh.nodes.iter().enumerate() {
                    let (x, y) = (p[0] - cx, p[1] - cy);
                    let r = (x * x + y * y).sqrt();
                    if (r - r0).abs() < 0.01 && (p[2] - mid_z).abs() < 0.1 {
                        sum += (-y * f64::from(u.at(i)[0]) + x * f64::from(u.at(i)[1])) / r;
                        n += 1.0;
                    }
                }
                (sum / n.max(1.0), n)
            };
            let profile: Vec<String> = [0.06, 0.07, 0.085, 0.1, 0.13, 0.17].iter().map(|r| {
                let (s, n) = swirl_at(*r);
                format!("r {r}: {s:.4} (theory {:.4}, {n} nodes)", wall * 0.05 / r)
            }).collect();
            let (near, near_nodes) = swirl_at(0.07);
            let theory = wall * 0.05 / 0.07;
            test.check("Multi-purpose: the air turns with the rotor, close to it, within 35% of theory", near_nodes > 0.0 && (near - theory).abs() / theory < 0.35, profile.join("; "));
        }
        46 => {
            // A hot block in a slow stream, body-fitted first.
            let part = bridge::partition(&slot.host.as_ref().unwrap().app.session, "Block", MeshQuality::Fine).unwrap();
            let lo_x = part.positions.iter().fold(f64::INFINITY, |a, p| a.min(p[0]));
            let face = face_at_x(&part, lo_x).expect("the block's front face");
            let mut s = Study::new("Block, body-fitted cooling".into(), AnalysisId::ConjugateHeatTransfer);
            s.body = Some("Block".into());
            s.settings.free_stream = 0.2;
            s.settings.element_size = 0.06;
            s.conditions.push(Condition { kind: BcKind::HeatFlux { flux: 200.0 }, faces: vec![face.clone()] });
            test.problems_empty = s.problems().is_empty();
            let mut voxel = s.clone();
            voxel.name = "Block, voxel cooling".into();
            voxel.analysis = AnalysisId::ConjugateHeatTransferVoxel;
            voxel.settings.resolution = 32;
            state.studies.push(s);
            state.studies.push(voxel);
            let last = state.studies.len() - 2;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        47 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 46) else { return };
            let balance = set.plots.iter().find(|p| p.name == "heat balance").expect("a heat balance");
            let (power, carried) = (balance.points[0].1, balance.points[1].1);
            test.check("body-fitted CHT: the heat balance closes within 5%", (carried - power).abs() / power < 0.05, format!("{power:.2} W in, {carried:.2} W out"));
            let t = set.steps[0].field("T").expect("temperature");
            test.body_fitted_peak = t.range().map_or(0.0, |r| f64::from(r.1));
            panel::show_result(&mut state, test.job, "Block, body-fitted cooling", set);
            let ready = test.problems_empty;
            test.check("body-fitted CHT: the study was ready", ready, "");
        }
        48 => {}
        49 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_cht.png", test.out)));
        }
        50 => {
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        51 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 50) else { return };
            let t = set.steps[0].field("T").expect("temperature");
            let voxel_peak = t.range().map_or(0.0, |r| f64::from(r.1));
            // Rises above the 20 degree air. Two different meshes and flow solvers on the same part.
            let (a, b) = (test.body_fitted_peak - 20.0, voxel_peak - 20.0);
            let apart = (a - b).abs() / a.max(b).max(1e-9);
            test.check("the two CHT routes agree on the block's hottest rise, within 35%", apart < 0.35, format!("body-fitted {a:.2} K, voxel {b:.2} K, {:.0}% apart", apart * 100.0));
        }
        52 => {
            // Contact: one block pressed down on another. With contact the column carries E strain A, as
            // one piece. Without it, the top block would just move, carrying nothing.
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Anvil"}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 0], "p1": [50, 50]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "50 mm", "body_name": "Anvil", "operation": "new"}},
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Punch", "offset": 50}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 0], "p1": [50, 50]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "50 mm", "body_name": "Punch", "operation": "new"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("two touching blocks are modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let bodies = bridge::bodies(&h.app.session);
            test.check("they are two bodies", bodies.iter().any(|b| b == "Anvil") && bodies.iter().any(|b| b == "Punch"), format!("{bodies:?}"));
            let face_at_z = |part: &sim_core::geometry::Partition, z: f64| -> Option<FaceRef> {
                (0..part.faces).find_map(|face| {
                    let tris: Vec<_> = (0..part.triangles.len()).filter(|i| part.tri_face[*i] == face).collect();
                    if !tris.is_empty() && tris.iter().all(|i| part.triangle(*i).iter().all(|p| (p[2] - z).abs() < 1e-6)) {
                        let [a, b, c] = part.triangle(tris[0]);
                        Some(FaceRef { face, point: std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0), second: false })
                    } else {
                        None
                    }
                })
            };
            let anvil = bridge::partition(&h.app.session, "Anvil", MeshQuality::Fine).unwrap();
            let punch = bridge::partition(&h.app.session, "Punch", MeshQuality::Fine).unwrap();
            let z_of = |part: &sim_core::geometry::Partition| part.positions.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[2]), b.max(p[2])));
            let ((a0, a1), (p0, p1)) = (z_of(&anvil), z_of(&punch));
            test.check("the blocks touch", (a1 - p0).abs() < 1e-6, format!("anvil to z = {a1}, punch from z = {p0}"));
            let on_second = |f: FaceRef| FaceRef { second: true, ..f };
            let mut s = Study::new("Blocks in contact".into(), AnalysisId::NonlinearMechanical);
            s.body = Some("Anvil".into());
            s.second_body = Some("Punch".into());
            s.settings.element_size = 0.1;
            s.settings.load_increment = 0.25;
            s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![face_at_z(&anvil, a0).unwrap()] });
            s.conditions.push(Condition { kind: BcKind::Displacement { displacement: [0.0, 0.0, -1e-5] }, faces: vec![on_second(face_at_z(&punch, p1).unwrap())] });
            s.conditions.push(Condition { kind: BcKind::Contact { friction: 0.0 }, faces: vec![face_at_z(&anvil, a1).unwrap(), on_second(face_at_z(&punch, p0).unwrap())] });
            test.check("the contact study has what it needs", s.problems().is_empty(), format!("{:?}", s.problems()));
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        53 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 52) else { return };
            let reaction = set.plots.iter().find(|p| p.name.starts_with("Displacement") && p.name.ends_with("along z")).and_then(|p| p.points.last()).map_or(0.0, |p| p.1.abs());
            let young = state.studies.last().unwrap().material.young;
            let exact = young * (1e-5 / 0.1) * (0.05 * 0.05);
            let err = (reaction - exact).abs() / exact;
            test.check("contact: the column carries the force of one solid piece, within 5%", err < 0.05, format!("{:.0} N against {exact:.0} N, {:.1}% off", reaction, err * 100.0));
            panel::show_result(&mut state, test.job, "Blocks in contact", set);
        }
        54 => {}
        55 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_contact.png", test.out)));
        }
        56 => {
            // Two solvers on one case: air past a cube at Re 100, on OpenFOAM and then on the GPU.
            state.view = None;
            let h = slot.host.as_mut().unwrap();
            let script = json!({"commands": [
                {"command": "sketch.create", "params": {"plane": "XY", "name": "Cube"}},
                {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 3000000], "p1": [100, 3000100]}},
                {"command": "sketch.finish", "params": {}},
                {"command": "solid.extrude", "params": {"distance": "100 mm", "body_name": "Cube"}},
            ]});
            let r = h.app.session.run_script(&script);
            test.check("a cube is modelled", r.is_ok(), r.err().map_or(String::new(), |e| e.to_string()));
            let mut s = Study::new("Cube, OpenFOAM".into(), AnalysisId::Incompressible);
            s.body = Some("Cube".into());
            s.settings.free_stream = CUBE_SPEED;
            // Coarser than this, OpenFOAM's speed above the cube is 15% of the wind's away from the GPU's.
            s.settings.element_size = std::env::var("CUBE_ELEMENT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.015);
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        57 => {
            let Some(set) = wait(&mut test, &state, &mut slot, &mut exit, 56) else { return };
            test.first_answer = Some(set);
            let mut s = Study::new("Cube, GPU".into(), AnalysisId::IncompressibleLbm);
            s.body = Some("Cube".into());
            s.settings.free_stream = CUBE_SPEED;
            s.settings.resolution = 40;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, &mut slot, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        58 => {
            let Some(gpu) = wait(&mut test, &state, &mut slot, &mut exit, 57) else { return };
            let foam = test.first_answer.take().expect("OpenFOAM's answer");
            let h = slot.host.as_ref().unwrap();
            let part = bridge::partition(&h.app.session, "Cube", MeshQuality::Fine).unwrap();
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for p in &part.positions {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            let (y, z, l) = ((lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0, hi[0] - lo[0]);
            let places: [(&str, [f64; 3]); 5] = [("ahead", [lo[0] - 0.5 * l, y, z]), ("above", [(lo[0] + hi[0]) / 2.0, y, hi[2] + 0.3 * l]), ("wake 0.5 L", [hi[0] + 0.5 * l, y, z]), ("wake 1 L", [hi[0] + l, y, z]), ("wake 2 L", [hi[0] + 2.0 * l, y, z])];
            let speed_at = |set: &ResultSet, p: [f64; 3]| -> Result<f64, String> {
                let v = super::tools::probe_value(set, set.steps.len() - 1, "U", p)?;
                Ok(v.iter().map(|c| c * c).sum::<f64>().sqrt())
            };
            let (mut worst, mut rows, mut failed) = (0.0f64, Vec::new(), None);
            for (name, p) in places {
                match (speed_at(&foam, p), speed_at(&gpu, p)) {
                    (Ok(a), Ok(b)) => {
                        worst = worst.max((a - b).abs() / CUBE_SPEED);
                        rows.push(format!("{name}: {a:.5} and {b:.5}"));
                    }
                    (a, b) => failed = Some(format!("{name}: {a:?} {b:?}")),
                }
            }
            test.check("OpenFOAM and the GPU solver agree on air past a cube, within 10% of the wind's speed", failed.is_none() && worst < 0.10, format!("worst {:.1}%; {} m/s {}", worst * 100.0, rows.join(", "), failed.unwrap_or_default()));
            finish(&mut test, &mut slot, &mut exit);
        }
        _ => {}
    }
}

/// The assistant's tools, called as the assistant calls them.
pub fn tools_step(world: &mut World) {
    use serde_json::Value;
    let call = |world: &mut World, name: &str, input: Value| -> Result<Value, String> {
        let out = super::tools::start(world, name, &input);
        let text = out.summary();
        if out.is_error { Err(text) } else { serde_json::from_str(&text).map_err(|e| format!("{e}: {text}")) }
    };
    let check = |world: &mut World, name: &str, pass: bool, detail: String| world.resource_mut::<SimTest>().check(name, pass, detail);

    if world.resource::<SimTest>().tools_results {
        let job = world.resource::<SimTest>().job;
        let results = call(world, "sim_results", json!({ "job": job, "show": true }));
        check(world, "tool sim_results reports the fields", results.as_ref().is_ok_and(|r| r["step_count"].as_u64().is_some_and(|n| n > 0)), format!("{:?}", results.as_ref().map(|r| r["step_count"].clone())));
        // Bend at the free end: a 400 by 40 by 10 mm plate under 50 N, against beam theory.
        let young = world.resource::<SimState>().studies.last().map_or(0.0, |s| s.material.young);
        let exact = 50.0 * 0.4f64.powi(3) / (3.0 * young * (0.04 * 0.01f64.powi(3) / 12.0));
        let probe = call(world, "sim_probe", json!({ "job": job, "field": "DISP", "point": [0.399, 1.02, 0.005] }));
        let got = probe.as_ref().ok().and_then(|p| p["values"][2].as_f64()).unwrap_or(0.0);
        let err = (-got - exact).abs() / exact;
        check(world, "tool sim_probe reads the tip's deflection, within 5% of beam theory", err < 0.05, format!("{:.3} mm against {:.3} mm, {:.2}% off, {:?}", -got * 1e3, exact * 1e3, err * 100.0, probe.as_ref().map(|p| p["how"].clone())));
        world.resource_mut::<SimState>().view = None;
        world.resource_mut::<SimTest>().tools_results = false;
        return;
    }
    if !std::mem::take(&mut world.resource_mut::<SimTest>().tools_due) {
        return;
    }
    world.resource_mut::<SimTest>().job = 0;

    let listed = call(world, "design_commands", json!({ "filter": "extrude" }));
    check(world, "tool design_commands finds commands", listed.as_ref().is_ok_and(|l| l["total"].as_u64().is_some_and(|n| n > 0)), format!("{:?}", listed.as_ref().map(|l| l["total"].clone())));
    let refused = call(world, "design_run", json!({ "commands": [{ "command": "file.save_as", "params": { "path": "C:/x.sc" } }] }));
    check(world, "tool design_run leaves files to the user", refused.as_ref().is_err_and(|e| e.contains("leaves to the user")), format!("{refused:?}"));
    let made = call(world, "design_run", json!({ "commands": [
        { "command": "sketch.create", "params": { "plane": "XY", "name": "Tab" } },
        { "command": "sketch.rectangle.two_point", "params": { "p0": [0, 1000], "p1": [400, 1040] } },
        { "command": "sketch.finish", "params": {} },
        { "command": "solid.extrude", "params": { "distance": "10 mm", "body_name": "Tab" } },
    ] }));
    check(world, "tool design_run models a part", made.as_ref().is_ok_and(|m| m["bodies"].as_array().is_some_and(|b| b.iter().any(|n| n == "Tab"))), format!("{:?}", made.as_ref().map(|m| m["bodies"].clone())));
    let bad = call(world, "design_run", json!({ "commands": [{ "command": "no.such.command" }] }));
    check(world, "tool design_run reports a bad command", bad.is_err(), format!("{bad:?}"));
    let faces = call(world, "design_faces", json!({ "body": "Tab" }));
    let Ok(faces) = faces else {
        check(world, "tool design_faces lists faces", false, format!("{faces:?}"));
        return;
    };
    let along_x = |want: f64| -> Option<u64> { faces["faces"].as_array()?.iter().find(|f| f["outward"][0].as_f64() == Some(want))?["face"].as_u64() };
    let (root, tip) = (along_x(-1.0), along_x(1.0));
    check(world, "tool design_faces gives faces by direction", root.is_some() && tip.is_some(), format!("{} faces", faces["face_count"]));
    let catalog = call(world, "sim_catalog", json!({}));
    let can_run = catalog.as_ref().ok().and_then(|c| c["analyses"].as_array().map(|a| (a.len(), a.iter().find(|x| x["id"] == "static").map(|x| x["can_run_now"].clone()))));
    check(world, "tool sim_catalog has all 17, and says what can run", can_run.as_ref().is_some_and(|(n, s)| *n == 17 && *s == Some(json!(true))), format!("{can_run:?}"));
    let wrong = call(world, "sim_study_create", json!({ "analysis": "warp" }));
    check(world, "tool sim_study_create refuses an unknown analysis", wrong.is_err(), format!("{wrong:?}"));
    let created = call(world, "sim_study_create", json!({ "analysis": "static", "name": "Tab, by tool" }));
    let Some(study) = created.as_ref().ok().and_then(|c| c["study"].as_u64()) else {
        check(world, "tool sim_study_create makes a study", false, format!("{created:?}"));
        return;
    };
    let not_ready = call(world, "sim_run", json!({ "study": study }));
    check(world, "tool sim_run says what's still missing", not_ready.as_ref().is_err_and(|e| e.contains("Not ready")), format!("{not_ready:?}"));
    let typo = call(world, "sim_study_set", json!({ "study": study, "settings": { "element_sise": 0.1 } }));
    check(world, "tool sim_study_set refuses a setting that doesn't exist", typo.as_ref().is_err_and(|e| e.contains("no setting called")), format!("{typo:?}"));
    let set = call(world, "sim_study_set", json!({ "study": study, "body": "Tab", "settings": { "element_size": 0.012 }, "conditions": [
        { "kind": { "type": "fixed" }, "faces": [root] },
        { "kind": { "type": "force", "force": [0.0, 0.0, -50.0] }, "faces": [tip] },
    ] }));
    let needs = set.as_ref().ok().map(|s| s["still_needs"].clone());
    check(world, "tool sim_study_set sets a study up, ready to run", needs == Some(json!([])), format!("{needs:?} {:?}", set.as_ref().err()));
    // 40 by 10 mm at each end.
    let coverage = call(world, "sim_check_faces", json!({ "study": study }));
    let areas = coverage.as_ref().ok().and_then(|c| c["coverage"].as_array().map(|rows| rows.iter().filter_map(|r| r["area_m2"].as_f64()).collect::<Vec<_>>())).unwrap_or_default();
    check(world, "tool sim_check_faces says what each condition covers", areas.len() == 3 && (areas[0] - 4e-4).abs() < 1e-6 && (areas[1] - 4e-4).abs() < 1e-6, format!("{coverage:?}"));
    let started = call(world, "sim_run", json!({ "study": study }));
    match started.as_ref().ok().and_then(|s| s["job"].as_u64()) {
        Some(job) => world.resource_mut::<SimTest>().job = job,
        None => check(world, "tool sim_run starts the run", false, format!("{started:?}")),
    }
}

/// The current job's result once it's done; while it runs, keeps the test on `repeat_step`.
fn wait(test: &mut SimTest, state: &SimState, slot: &mut Slot, exit: &mut MessageWriter<AppExit>, repeat_step: usize) -> Option<std::sync::Arc<ResultSet>> {
    let info = state.jobs.list().into_iter().find(|j| j.id == test.job);
    match info.map(|j| j.state) {
        Some(State::Done) => state.jobs.result(test.job),
        Some(State::Failed { error }) => {
            test.check("the run finishes", false, error);
            finish(test, slot, exit);
            None
        }
        Some(_) => {
            test.step = repeat_step + 1;
            test.at = test.frame + 10;
            None
        }
        None => {
            test.check("the run starts", false, state.message.clone().unwrap_or_default());
            finish(test, slot, exit);
            None
        }
    }
}

fn finish(test: &mut SimTest, slot: &mut Slot, exit: &mut MessageWriter<AppExit>) {
    let pass = !test.checks.is_empty() && test.checks.iter().all(|c| c["pass"] == true);
    let report = json!({ "pass": pass, "checks": test.checks });
    let path = format!("{}.json", test.out);
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap_or_default());
    println!("{} ({path})", if pass { "ALL PASSED" } else { "FAILED" });
    if let Some(h) = slot.host.as_mut() {
        h.shutdown();
    }
    exit.write(if pass { AppExit::Success } else { AppExit::from_code(1) });
}
