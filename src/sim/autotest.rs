//! `--sim-test OUT`: the whole path in the real app. Test parts are placed in the scene as USD objects;
//! a beam's end faces are found by geometry and picked as the UI picks them; a Static study runs through
//! Gmsh and CalculiX; and the deflection must match beam theory. If labels went wrong anywhere, the
//! answer would too. Writes `OUT.json`, and pictures of the dialog and the results.

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

use super::{SimState, dialog, panel, view};

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

type Surface = (Vec<[f64; 3]>, Vec<[u32; 3]>);

/// A closed box, in metres with Z up.
fn box_part(lo: [f64; 3], hi: [f64; 3]) -> Surface {
    let corner = |i: u32| [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }];
    let points = (0..8).map(corner).collect();
    let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    (points, quads.iter().flat_map(|q: &[u32; 4]| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect())
}

/// Sides around an axis: fine enough that the crease angle sees one curved face, and that a radius is
/// out by 0.05% at most.
const SIDES: u32 = 96;

/// A closed ring from `from` to `to` along an axis, or a solid cylinder if `inner` is zero. `at` makes a
/// point from (along the axis, first across, second across).
fn round_part(from: f64, to: f64, inner: f64, outer: f64, at: impl Fn(f64, f64, f64) -> [f64; 3]) -> Surface {
    let angle = |i: u32| f64::from(i % SIDES) * std::f64::consts::TAU / f64::from(SIDES);
    let (mut points, mut triangles) = (Vec::new(), Vec::new());
    let mut circle = |along: f64, r: f64| {
        let first = points.len() as u32;
        for i in 0..SIDES {
            points.push(at(along, r * angle(i).cos(), r * angle(i).sin()));
        }
        first
    };
    let (bottom, top) = (circle(from, outer), circle(to, outer));
    let next = |i: u32| (i + 1) % SIDES;
    for i in 0..SIDES {
        triangles.push([bottom + i, bottom + next(i), top + next(i)]);
        triangles.push([bottom + i, top + next(i), top + i]);
    }
    if inner > 0.0 {
        let (low, high) = (circle(from, inner), circle(to, inner));
        for i in 0..SIDES {
            triangles.push([low + i, high + next(i), low + next(i)]);
            triangles.push([low + i, high + i, high + next(i)]);
            triangles.push([bottom + i, low + i, low + next(i)]);
            triangles.push([bottom + i, low + next(i), bottom + next(i)]);
            triangles.push([top + i, high + next(i), high + i]);
            triangles.push([top + i, top + next(i), high + next(i)]);
        }
    } else {
        let (low, high) = (circle(from, 0.0), circle(to, 0.0));
        for i in 0..SIDES {
            // A fan: every point of these two circles is the centre.
            triangles.push([low, bottom + next(i), bottom + i]);
            triangles.push([high, top + i, top + next(i)]);
        }
    }
    (points, triangles)
}

/// What the test works on, each well clear of the others.
fn parts() -> Vec<(&'static str, Surface)> {
    vec![
        ("Beam", box_part([0.0, 0.0, 0.0], [1.0, 0.05, 0.05])),
        ("Pipe", round_part(0.0, 1.0, 0.0, 0.025, |x, a, b| [x, -0.3 + a, b])),
        ("Tab", box_part([0.0, 1.0, 0.0], [0.4, 1.04, 0.01])),
        // Wider than it is thick, so that the wrong axis would give a very different answer.
        ("Plank", box_part([0.0, 2.0, 0.0], [1.0, 2.05, 0.02])),
        ("Coil", round_part(0.0, 0.1, 0.05, 0.06, |z, a, b| [a, -2.0 + b, z])),
        ("Cube", box_part([0.0, 3.0, 0.0], [0.1, 3.1, 0.1])),
        ("Block", box_part([0.0, 4.0, 0.0], [0.1, 4.1, 0.1])),
        ("Rotor", round_part(0.0, 0.4, 0.0, 0.05, |z, a, b| [a, 6.0 + b, z])),
        ("Tower", box_part([0.0, 500.0, 0.0], [20.0, 520.0, 60.0])),
    ]
}

/// A surface as a USDA file. Metres and Z up are stated in it, and the loader takes it from there.
fn usda_of((points, triangles): &Surface, name: &str) -> String {
    let counts = vec!["3"; triangles.len()].join(", ");
    let indices = triangles.iter().map(|t| format!("{}, {}, {}", t[0], t[1], t[2])).collect::<Vec<_>>().join(", ");
    let at = points.iter().map(|p| format!("({}, {}, {})", p[0] as f32, p[1] as f32, p[2] as f32)).collect::<Vec<_>>().join(", ");
    format!("#usda 1.0\n(\n    defaultPrim = \"{name}\"\n    metersPerUnit = 1\n    upAxis = \"Z\"\n)\n\ndef Xform \"{name}\" (\n    kind = \"component\"\n)\n{{\n    def Mesh \"Mesh\"\n    {{\n        uniform token subdivisionScheme = \"none\"\n        int[] faceVertexCounts = [{counts}]\n        int[] faceVertexIndices = [{indices}]\n        point3f[] points = [{at}]\n    }}\n}}\n")
}

/// The loader stands an asset on the origin, bottom-centred: this puts it back where it was modelled,
/// so that the solvers' frame has it at the coordinates above.
fn back_in_place((points, _): &Surface) -> Transform {
    let range = |k: usize| points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[k]), b.max(p[k])));
    let ((x0, x1), (y0, y1), (z0, _)) = (range(0), range(1), range(2));
    Transform::from_xyz(((x0 + x1) / 2.0) as f32, z0 as f32, (-(y0 + y1) / 2.0) as f32)
}

/// A study on an object in the scene, its material taken as confirmed unless a check says otherwise.
fn on_scene(name: &str, analysis: AnalysisId, object: &str) -> Study {
    let mut s = Study::new(name.into(), analysis);
    s.body = Some(format!("{}{object}", sim_core::study::SCENE_PREFIX));
    s.material_confirmed = true;
    s
}

/// The same but for rounding: a number written to JSON and read back can come back a digit off in its
/// 17th place. Metres, and a picked face's tolerance is 1 mm.
fn near(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::{Array, Number, Object};
    match (a, b) {
        (Number(x), Number(y)) => (x.as_f64().unwrap_or(f64::NAN) - y.as_f64().unwrap_or(f64::NAN)).abs() < 1e-12,
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| near(p, q)),
        (Object(x), Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| near(v, w))),
        _ => a == b,
    }
}

/// Which study differs and how, empty if none does, for a failed check to say.
fn first_difference(got: &[Study], want: &[Study]) -> String {
    if got.len() != want.len() {
        return format!("{} studies, expected {}", got.len(), want.len());
    }
    for (a, b) in got.iter().zip(want) {
        let value = |s: &Study| serde_json::to_value(s).unwrap_or_default();
        if !near(&value(a), &value(b)) {
            let (ta, tb) = (serde_json::to_string(a).unwrap_or_default(), serde_json::to_string(b).unwrap_or_default());
            let at = ta.bytes().zip(tb.bytes()).position(|(x, y)| x != y).unwrap_or(ta.len().min(tb.len()));
            let around = |t: &str| t.get(at.saturating_sub(60)..(at + 40).min(t.len())).unwrap_or_default().to_string();
            return format!("{}: got ...{} want ...{}", b.name, around(&ta), around(&tb));
        }
    }
    String::new()
}

/// A part's surface, once it has been read from the scene.
fn part_of(state: &SimState, object: &str) -> sim_core::geometry::Partition {
    state.scene.bodies[object].regions.partition.clone()
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
    mut exit: MessageWriter<AppExit>,
    mut pending: ResMut<crate::scene::PendingSpawns>,
    mut actions: ResMut<crate::scene::Actions>,
    looks: Query<(), With<view::LooksMarker>>,
    (mut window_events, mut mouse, mut orbit, camera, window_id, picks): (
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
    if test.frame < test.at {
        return;
    }
    let step = test.step;
    test.step += 1;
    test.at = test.frame + 20;
    match step {
        0 => {
            // The test's parts, as USD objects. A study on each makes the scene read them.
            for (name, surface) in parts() {
                let asset = std::env::temp_dir().join(format!("sense-sim-test-{name}.usda"));
                std::fs::write(&asset, usda_of(&surface, name)).expect("writing a test part");
                pending.0.push(crate::scene::PendingSpawn { asset, transform: back_in_place(&surface), name: Some(name.into()), joints: None, select: false, locked: false });
                state.studies.push(on_scene(name, AnalysisId::Static, name));
            }
            test.step = 100;
        }
        100 => {
            if !parts().iter().all(|(name, _)| state.scene.bodies.contains_key(*name)) {
                test.step = 100;
                test.at = test.frame + 10;
                if test.frame > 40000 || !state.scene.errors.is_empty() {
                    test.check("the test parts reach the scene", false, format!("{:?}", state.scene.errors));
                    finish(&mut test, &mut exit);
                }
                return;
            }
            state.studies.clear();
            test.step = 1;
        }
        1 => {
            state.panel_open = true;
            state.dialog = Some(dialog::Dialog { analysis: Some(AnalysisId::ConjugateHeatTransferVoxel), ..Default::default() });
        }
        2 => {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{}_dialog.png", test.out)));
        }
        3 => {
            state.dialog = None;
            let offered = state.scene.names.iter().any(|n| n == "Beam");
            test.check("the object is offered to the study", offered, format!("{:?}", state.scene.names));
            let part = part_of(&state, "Beam");
            let closed = part.is_closed();
            test.check("its surface is closed", closed, format!("{} faces, {} triangles", part.faces, part.triangles.len()));
            let (fixed, tip) = (face_at_x(&part, 0.0), face_at_x(&part, 1.0));
            test.check("both end faces are found", fixed.is_some() && tip.is_some(), "");
            let mut s = on_scene("Beam, static", AnalysisId::Static, "Beam");
            s.settings.element_size = 0.012;
            s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![fixed.unwrap()] });
            s.conditions.push(Condition { kind: BcKind::Force { force: [0.0, 0.0, -1000.0] }, faces: vec![tip.unwrap()] });
            test.check("the study has what it needs", s.problems().is_empty(), format!("{:?}", s.problems()));
            state.studies.push(s);
            state.active = Some(0);
            // Before running: what the picks really land on. 50 by 50 mm at each end, and 4 sides left.
            let rows = panel::preview_faces(&mut state, 0);
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
            // A pick the object has moved out from under: refused, not coloured or solved.
            let mut moved = state.studies[0].clone();
            moved.conditions[0].faces[0].point = [0.5, 0.025, 0.025];
            state.studies.push(moved);
            let refused = panel::preview_faces(&mut state, 1);
            test.check("checking faces refuses a pick that has gone stale", refused.as_ref().is_err_and(|e| e.contains("pick the face again")), format!("{refused:?}"));
            state.studies.pop();
            state.active = Some(0);
            state.view = None;
            panel::run(&mut state, 0);
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
                    finish(&mut test, &mut exit);
                    return;
                }
                Some(_) => {
                    test.step = 4;
                    test.at = test.frame + 10;
                    if test.frame > 40000 {
                        test.check("the solve finishes", false, "timed out");
                        finish(&mut test, &mut exit);
                    }
                    return;
                }
                None => {
                    test.check("the solve starts", false, state.message.clone().unwrap_or_default());
                    finish(&mut test, &mut exit);
                    return;
                }
            }
            let set: std::sync::Arc<ResultSet> = state.jobs.result(test.job).expect("a finished job has a result");
            let last = set.steps.last().expect("a step");
            let disp = last.field("DISP").expect("displacement");
            let (mut sum, mut n) = (0.0f64, 0.0f64);
            for (i, p) in set.mesh.nodes.iter().enumerate() {
                if (p[0] - 1.0).abs() < 1e-5 {
                    sum += f64::from(disp.at(i)[2]);
                    n += 1.0;
                }
            }
            let tip = -sum / n;
            let err = (tip - 3.054e-3).abs() / 3.054e-3;
            test.check("the deflection matches beam theory within 1%", err < 0.01, format!("{:.4} mm against 3.054 mm, {:.2}% off", tip * 1000.0, err * 100.0));
            let (lo, hi) = set.mesh.nodes.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            test.check("results are in metres", (hi - lo - 1.0).abs() < 1e-5, format!("x spans {lo} to {hi}"));
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
                finish(&mut test, &mut exit);
                return;
            }
            state.view = None;
            let part = part_of(&state, "Beam");
            // Electrostatics between the beam's ends.
            let mut s = on_scene("Beam, potential", AnalysisId::Electromagnetics, "Beam");
            s.settings.element_size = 0.03;
            s.conditions.push(Condition { kind: BcKind::Potential { value: 0.0 }, faces: vec![face_at_x(&part, 0.0).unwrap()] });
            s.conditions.push(Condition { kind: BcKind::Potential { value: 100.0 }, faces: vec![face_at_x(&part, 1.0).unwrap()] });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        8 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 7) else { return };
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
            let part = part_of(&state, "Pipe");
            let mut s = on_scene("Pipe, flow", AnalysisId::Incompressible, "Pipe");
            s.settings.element_size = 0.008;
            s.conditions.push(Condition { kind: BcKind::VelocityInlet { speed: PIPE_SPEED }, faces: vec![face_at_x(&part, 0.0).unwrap()] });
            s.conditions.push(Condition { kind: BcKind::PressureOutlet { pressure: 0.0 }, faces: vec![face_at_x(&part, 1.0).unwrap()] });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        9 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 8) else { return };
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
            let part = part_of(&state, "Beam");
            let mut s = on_scene("Beam, cooling", AnalysisId::ConjugateHeatTransferVoxel, "Beam");
            s.settings.resolution = 32;
            s.settings.free_stream = 2.0;
            s.conditions.push(Condition { kind: BcKind::HeatFlux { flux: 2000.0 }, faces: vec![face_at_x(&part, 0.0).unwrap()] });
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        13 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 12) else { return };
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
            let mut s = on_scene("Tower, wind", AnalysisId::PedestrianWindComfort, "Tower");
            s.settings.resolution = 16;
            s.settings.directions = 2;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        21 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 20) else { return };
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
            // A plank, wider than it is thick, so that the wrong axis would give a very different answer.
            state.view = None;
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
                    finish(&mut test, &mut exit);
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
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        26 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 25) else { return };
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
                finish(&mut test, &mut exit);
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
            if std::env::var_os("SENSESIM_TEST_USD").is_some() {
                finish(&mut test, &mut exit);
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
                finish(&mut test, &mut exit);
                return;
            }
        }
        31 => {
            let Some(_) = wait(&mut test, &state, &mut exit, 30) else { return };
            test.tools_results = true;
        }
        32 => {
            if test.tools_results {
                test.step = 32;
                test.at = test.frame + 5;
                return;
            }
            // Saving: the studies go in the layout.
            let path = std::env::temp_dir().join("sense-sim-layout-test.json");
            let _ = std::fs::remove_file(&path);
            test.saved = state.studies.clone();
            actions.0.push(crate::scene::Action::SaveLayout(path));
        }
        33 => {
            let path = std::env::temp_dir().join("sense-sim-layout-test.json");
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let (studies, skipped) = super::persist::parse_section(&v["simulation"]);
            let differ = first_difference(&studies, &test.saved);
            test.check("the studies are saved with the layout", differ.is_empty() && skipped == 0, format!("{} studies, {skipped} skipped. {differ}", studies.len()));
            actions.0.push(crate::scene::Action::ClearScene);
        }
        34 => {
            test.check("a cleared scene has no studies", state.studies.is_empty(), format!("{}", state.studies.len()));
            actions.0.push(crate::scene::Action::LoadLayout(std::env::temp_dir().join("sense-sim-layout-test.json")));
        }
        35 => {
            let differ = first_difference(&state.studies, &test.saved);
            test.check("loading the layout brings the studies back", differ.is_empty(), format!("{} studies. {differ}", state.studies.len()));
            test.step = 101;
        }
        101 => {
            // The objects came back with the layout: the scene reads what the studies want again.
            let names_back = parts().iter().all(|(name, _)| state.scene.names.iter().any(|n| n == *name));
            let read = state.studies.iter().filter_map(|s| s.scene_object()).all(|o| state.scene.bodies.contains_key(o));
            if !names_back || !read {
                test.step = 101;
                test.at = test.frame + 10;
                if test.frame > 40000 {
                    test.check("the objects come back with the layout", false, format!("{:?} {:?}", state.scene.names, state.scene.errors));
                    finish(&mut test, &mut exit);
                }
                return;
            }
            state.scene.errors.clear();
            test.step = 36;
        }
        36 => {
            // A coil's region is cut out of the air, which takes CAD geometry: refused, with the reason.
            let mut s = on_scene("Coil", AnalysisId::Electromagnetics, "Coil");
            s.settings.coil_ampere_turns = 1000.0;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            let refused = panel::start_run(&mut state, last);
            test.check("a coil is refused, and says why", refused.as_ref().is_err_and(|e| e.contains("CAD body")), format!("{refused:?}"));
            state.studies.pop();
            test.step = 40;
        }
        40 => {
            // Mach 2 at a block: a bow shock, so the nose sees the pitot pressure behind a normal shock.
            state.view = None;
            let mut s = on_scene("Block, Mach 2", AnalysisId::Compressible, "Block");
            s.settings.free_stream = 2.0 * (1.4f64 * 8314.46 / 28.96 * 293.15).sqrt();
            s.settings.element_size = 0.05;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        41 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 40) else { return };
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
            let mut s = on_scene("Rotor", AnalysisId::MultiPurpose, "Rotor");
            s.settings.free_stream = 1e-5;
            s.settings.compressible = false;
            // Slow: a Reynolds number of about 10, which this coarse mesh can resolve. At 60 rpm, on the
            // same mesh, numerical diffusion takes nine tenths of the swirl. The catalog says so.
            s.settings.rotor_rpm = 0.6;
            s.settings.axis = 2;
            s.settings.element_size = 0.08;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        45 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 44) else { return };
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
            let part = part_of(&state, "Block");
            let lo_x = part.positions.iter().fold(f64::INFINITY, |a, p| a.min(p[0]));
            let face = face_at_x(&part, lo_x).expect("the block's front face");
            let mut s = on_scene("Block, body-fitted cooling", AnalysisId::ConjugateHeatTransfer, "Block");
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
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        47 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 46) else { return };
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
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        51 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 50) else { return };
            let t = set.steps[0].field("T").expect("temperature");
            let voxel_peak = t.range().map_or(0.0, |r| f64::from(r.1));
            // Rises above the 20 degree air. Two different meshes and flow solvers on the same part.
            let (a, b) = (test.body_fitted_peak - 20.0, voxel_peak - 20.0);
            let apart = (a - b).abs() / a.max(b).max(1e-9);
            test.check("the two CHT routes agree on the block's hottest rise, within 35%", apart < 0.35, format!("body-fitted {a:.2} K, voxel {b:.2} K, {:.0}% apart", apart * 100.0));
            test.step = 56;
        }
        56 => {
            // Two solvers on one case: air past a cube at Re 100, on OpenFOAM and then on the GPU.
            state.view = None;
            let mut s = on_scene("Cube, OpenFOAM", AnalysisId::Incompressible, "Cube");
            s.settings.free_stream = CUBE_SPEED;
            // Coarser than this, OpenFOAM's speed above the cube is 15% of the wind's away from the GPU's.
            s.settings.element_size = std::env::var("CUBE_ELEMENT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.015);
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        57 => {
            let Some(set) = wait(&mut test, &state, &mut exit, 56) else { return };
            test.first_answer = Some(set);
            let mut s = on_scene("Cube, GPU", AnalysisId::IncompressibleLbm, "Cube");
            s.settings.free_stream = CUBE_SPEED;
            s.settings.resolution = 40;
            state.studies.push(s);
            let last = state.studies.len() - 1;
            panel::run(&mut state, last);
            test.job = state.jobs.list().last().map_or(0, |j| j.id);
        }
        58 => {
            let Some(gpu) = wait(&mut test, &state, &mut exit, 57) else { return };
            let foam = test.first_answer.take().expect("OpenFOAM's answer");
            let part = part_of(&state, "Cube");
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
            finish(&mut test, &mut exit);
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

    let tab = format!("{}Tab", sim_core::study::SCENE_PREFIX);
    let faces = call(world, "sim_faces", json!({ "body": tab }));
    let Ok(faces) = faces else {
        check(world, "tool sim_faces lists faces", false, format!("{faces:?}"));
        return;
    };
    let along_x = |want: f64| -> Option<u64> { faces["faces"].as_array()?.iter().find(|f| f["outward"][0].as_f64() == Some(want))?["face"].as_u64() };
    let (root, tip) = (along_x(-1.0), along_x(1.0));
    check(world, "tool sim_faces gives faces by direction", root.is_some() && tip.is_some(), format!("{} faces", faces["face_count"]));
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
    let contact = call(world, "sim_study_set", json!({ "study": study, "second_body": tab }));
    check(world, "tool sim_study_set says contact can't be set up", contact.as_ref().is_err_and(|e| e.contains("Contact")), format!("{contact:?}"));
    let material = sim_core::materials::solids().remove(0).name;
    let set = call(world, "sim_study_set", json!({ "study": study, "body": tab, "material": material, "settings": { "element_size": 0.012 }, "conditions": [
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
fn wait(test: &mut SimTest, state: &SimState, exit: &mut MessageWriter<AppExit>, repeat_step: usize) -> Option<std::sync::Arc<ResultSet>> {
    let info = state.jobs.list().into_iter().find(|j| j.id == test.job);
    match info.map(|j| j.state) {
        Some(State::Done) => state.jobs.result(test.job),
        Some(State::Failed { error }) => {
            test.check("the run finishes", false, error);
            finish(test, exit);
            None
        }
        Some(_) => {
            test.step = repeat_step + 1;
            test.at = test.frame + 10;
            None
        }
        None => {
            test.check("the run starts", false, state.message.clone().unwrap_or_default());
            finish(test, exit);
            None
        }
    }
}

fn finish(test: &mut SimTest, exit: &mut MessageWriter<AppExit>) {
    let pass = !test.checks.is_empty() && test.checks.iter().all(|c| c["pass"] == true);
    let report = json!({ "pass": pass, "checks": test.checks });
    let path = format!("{}.json", test.out);
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap_or_default());
    println!("{} ({path})", if pass { "ALL PASSED" } else { "FAILED" });
    exit.write(if pass { AppExit::Success } else { AppExit::from_code(1) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::geometry::regions;

    /// Every test part must read as the solid the checks expect, or a later check fails for the wrong reason.
    #[test]
    fn the_test_parts_read_as_closed_solids_with_the_right_faces() {
        for (name, (points, triangles)) in parts() {
            let r = regions::from_triangles(name, &points, &triangles, regions::CREASE_DEGREES).unwrap();
            let want = match name {
                "Pipe" | "Rotor" => 3,
                "Coil" => 4,
                _ => 6,
            };
            assert_eq!(r.partition.faces as usize, want, "{name}");
            assert!(r.solid_problem().is_none(), "{name}: {:?}", r.solid_problem());
        }
    }

    #[test]
    fn a_rounded_digit_is_the_same_study_and_a_real_change_isnt() {
        let mut a = on_scene("x", AnalysisId::Static, "Beam");
        a.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![FaceRef { face: 0, point: [0.0, -0.012157924349108859, 0.0], second: false }] });
        let mut rounded = a.clone();
        rounded.conditions[0].faces[0].point[1] = -0.01215792434910886;
        assert_eq!(first_difference(std::slice::from_ref(&a), std::slice::from_ref(&rounded)), "");
        let mut moved = a.clone();
        moved.conditions[0].faces[0].point[1] = -0.0122;
        assert!(!first_difference(std::slice::from_ref(&a), std::slice::from_ref(&moved)).is_empty());
        let mut other = a.clone();
        other.conditions[0].faces[0].face = 1;
        assert!(!first_difference(std::slice::from_ref(&a), std::slice::from_ref(&other)).is_empty());
    }

    #[test]
    fn a_pipes_ends_are_found_by_where_they_are() {
        let (_, (points, triangles)) = parts().into_iter().find(|p| p.0 == "Pipe").unwrap();
        let part = regions::from_triangles("Pipe", &points, &triangles, regions::CREASE_DEGREES).unwrap().partition;
        assert!(face_at_x(&part, 0.0).is_some() && face_at_x(&part, 1.0).is_some());
    }
}
