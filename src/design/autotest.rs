//! `--design-test OUT`: a scripted run of the Design workspace through real window events, the way
//! a user's mouse and keyboard reach it (Bevy's input, then bevy_egui, then the translation into
//! SolidCraft's context). Writes `OUT.json` with what happened and `OUT.png` of the window.

use bevy::app::AppExit;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{CursorMoved, PrimaryWindow, WindowEvent};
use serde_json::json;

use super::{Slot, Workspace};

#[derive(Resource)]
pub struct DesignTest {
    pub out: String,
    frame: u32,
    step: usize,
    at: u32,
    checks: Vec<serde_json::Value>,
    undo_before: usize,
    picking: bool,
    /// `SENSESIM_SOAK_HOURS`: when to stop repeating the work, with the checks over.
    soak: Option<Soak>,
}

/// A long run of ordinary work in one process, to find what only time finds. Each cycle models a part
/// through commands, measures it, clicks and presses keys through real window events, undoes it all,
/// and goes to the scene and back. A row per cycle goes to `OUT_soak.csv`: leaks show in its trend.
struct Soak {
    started: std::time::Instant,
    hours: f64,
    base_undo: usize,
    cycles: u64,
    failures: u64,
    first_failures: Vec<String>,
    file: std::path::PathBuf,
}

impl Soak {
    fn fail(&mut self, what: String) {
        self.failures += 1;
        if self.first_failures.len() < 10 {
            self.first_failures.push(format!("cycle {}: {what}", self.cycles));
        }
    }
}

impl DesignTest {
    pub fn new(out: String) -> Self {
        let soak = std::env::var("SENSESIM_SOAK_HOURS").ok().and_then(|h| h.parse().ok()).map(|hours| {
            let file = std::path::PathBuf::from(format!("{out}_soak.csv"));
            let _ = std::fs::write(&file, "seconds,cycle,frames,mean_ms,worst_ms,failures,bodies\n");
            Soak { started: std::time::Instant::now(), hours, base_undo: 0, cycles: 0, failures: 0, first_failures: Vec::new(), file }
        });
        Self { out, frame: 0, step: 0, at: 0, checks: Vec::new(), undo_before: 0, picking: false, soak }
    }
    fn check(&mut self, name: &str, pass: bool, detail: impl Into<serde_json::Value>) {
        println!("[{}] {name}", if pass { "pass" } else { "FAIL" });
        self.checks.push(json!({ "name": name, "pass": pass, "detail": detail.into() }));
    }
}

/// The ribbon's New Sketch button, in points from the top of the workspace: SolidCraft's toolbar is
/// its first row, under the strip when the strip shows.
const NEW_SKETCH: Vec2 = Vec2::new(143.0, 56.0 + if super::SHELL_HAS_WAY_BACK { 0.0 } else { super::STRIP_HEIGHT });

#[allow(clippy::too_many_arguments)]
pub fn run(
    mut test: ResMut<DesignTest>,
    mut commands: Commands,
    mut workspace: ResMut<Workspace>,
    mut slot: NonSendMut<Slot>,
    window: Single<Entity, With<PrimaryWindow>>,
    mut window_events: MessageWriter<WindowEvent>,
    mut keys: MessageWriter<KeyboardInput>,
    mut exit: MessageWriter<AppExit>,
    cameras: Query<&Camera, With<crate::scene::OrbitCamera>>,
    scene_undo: Res<crate::undo::UndoStack>,
) {
    test.frame += 1;
    let window = *window;
    if test.frame == 5 {
        *workspace = Workspace::Design;
    }
    let Some(h) = slot.host.as_mut() else {
        if let Some(e) = slot.failed.clone() {
            test.check("the workspace starts", false, e);
            finish(&mut test, &mut slot, &mut exit);
        } else if test.frame > 1500 {
            test.check("the workspace starts", false, "timed out");
            finish(&mut test, &mut slot, &mut exit);
        }
        return;
    };
    // Give each step time to go through a few frames.
    if test.frame < test.at {
        return;
    }
    let step = test.step;
    test.step += 1;
    // A press and its release must be close, or egui reads a long press instead of a click.
    test.at = test.frame + if step == 3 || step == 24 { 2 } else { 25 };
    match step {
        0 => {
            let name = h.adapter_name().to_string();
            test.check("the workspace starts on the GPU", h.gpu_viewport(), name);
            let off = cameras.iter().all(|c| !c.is_active);
            test.check("the scene's 3D camera is off while designing", off, "");
            h.app.home.open = false;
            // The same route the UI's own buttons take.
            let script = solidcraft_engine::sample::script();
            let r = h.app.session.run_script(&script);
            test_result(&mut test, "a part is modelled through commands", r.map(|v| v.len()));
        }
        1 => {
            h.app.cam.set_view(solidcraft_engine::render::StandardView::Iso);
            h.app.fit_view();
            let m = h.app.session.execute("inspect.measure", &json!({}));
            let volume = m.as_ref().ok().and_then(|v| v.pointer("/bodies/0/volume_mm3").and_then(|x| x.as_f64()));
            let detail = m.as_ref().map(|v| v.to_string()).unwrap_or_else(|e| e.to_string());
            test.check("the part measures as a solid", volume.is_some_and(|v| v > 0.0), detail);
            test.undo_before = h.app.session.undo.len();
        }
        // A real click on the ribbon's New Sketch.
        2 => {
            window_events.write(WindowEvent::CursorMoved(CursorMoved { window, position: NEW_SKETCH, delta: None }));
        }
        3 => {
            window_events.write(WindowEvent::MouseButtonInput(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Pressed, window }));
        }
        4 => {
            window_events.write(WindowEvent::MouseButtonInput(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Released, window }));
        }
        5 => {
            let picking = h.app.picking_sketch_plane();
            test.picking = picking;
            test.check("a mouse click reaches SolidCraft's ribbon (New Sketch asks for a plane)", picking, "");
            press(&mut keys, window, KeyCode::Escape, Key::Escape);
        }
        6 => {
            release(&mut keys, window, KeyCode::Escape, Key::Escape);
            // Only means something if picking had started.
            let ended = test.picking && !h.app.picking_sketch_plane();
            test.check("the keyboard reaches SolidCraft (Esc ends picking)", ended, "");
            press(&mut keys, window, KeyCode::ControlLeft, Key::Control);
        }
        7 => press(&mut keys, window, KeyCode::KeyZ, Key::Character("z".into())),
        8 => release(&mut keys, window, KeyCode::KeyZ, Key::Character("z".into())),
        9 => {
            release(&mut keys, window, KeyCode::ControlLeft, Key::Control);
            let after = h.app.session.undo.len();
            let before = test.undo_before;
            test.check("Ctrl+Z undoes in the design", after < before, format!("undo steps {before} -> {after}"));
            test.check("Ctrl+Z leaves the scene's own undo alone", !scene_undo.can_undo(), "");
            let path = format!("{}.png", test.out);
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        }
        10 => {}
        11 => {
            *workspace = Workspace::Scene;
        }
        12 => {
            let on = cameras.iter().all(|c| c.is_active);
            test.check("back in the scene, the 3D camera is on", on, "");
            let avg = slot.total_ms / slot.drawn.max(1) as f64;
            let (drawn, worst) = (slot.drawn, slot.worst_ms);
            test.check("frames stay under 33 ms on average", avg < 33.0, format!("{drawn} frames, mean {avg:.1} ms, worst {worst:.1} ms"));
            if test.soak.is_some() {
                test.step = 20;
            } else {
                finish(&mut test, &mut slot, &mut exit);
            }
        }
        // The soak's cycle.
        20 => {
            *workspace = Workspace::Design;
            slot.total_ms = 0.0;
            slot.drawn = 0;
            slot.worst_ms = 0.0;
        }
        21 => {
            let base = h.app.session.undo.len();
            let r = h.app.session.run_script(&solidcraft_engine::sample::script());
            let soak = test.soak.as_mut().unwrap();
            soak.base_undo = base;
            if let Err(e) = r {
                soak.fail(format!("modelling: {e}"));
            }
        }
        22 => {
            h.app.cam.set_view(solidcraft_engine::render::StandardView::Iso);
            h.app.fit_view();
            let m = h.app.session.execute("inspect.measure", &json!({}));
            let volume = m.as_ref().ok().and_then(|v| v.pointer("/bodies/0/volume_mm3").and_then(|x| x.as_f64()));
            if !volume.is_some_and(|v| v > 0.0) {
                test.soak.as_mut().unwrap().fail(format!("measuring: {m:?}"));
            }
        }
        23 => {
            window_events.write(WindowEvent::CursorMoved(CursorMoved { window, position: NEW_SKETCH, delta: None }));
        }
        24 => {
            window_events.write(WindowEvent::MouseButtonInput(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Pressed, window }));
        }
        25 => {
            window_events.write(WindowEvent::MouseButtonInput(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Released, window }));
        }
        26 => {
            if !h.app.picking_sketch_plane() {
                test.soak.as_mut().unwrap().fail("a click didn't reach the ribbon".to_string());
            }
            press(&mut keys, window, KeyCode::Escape, Key::Escape);
        }
        27 => {
            release(&mut keys, window, KeyCode::Escape, Key::Escape);
            if h.app.picking_sketch_plane() {
                test.soak.as_mut().unwrap().fail("Esc didn't reach SolidCraft".to_string());
            }
        }
        28 => {
            // Back to where the cycle began, or the work would pile up instead of repeating.
            let base = test.soak.as_ref().unwrap().base_undo;
            let mut undo_failed = None;
            while h.app.session.undo.len() > base {
                if let Err(e) = h.app.session.execute("edit.undo", &json!({})) {
                    undo_failed = Some(e.to_string());
                    break;
                }
            }
            if let Some(e) = undo_failed {
                test.soak.as_mut().unwrap().fail(format!("undoing: {e}"));
            }
            *workspace = Workspace::Scene;
        }
        29 => {
            let bodies = super::bridge::bodies(&h.app.session).len();
            let (drawn, worst, total) = (slot.drawn, slot.worst_ms, slot.total_ms);
            let on = cameras.iter().all(|c| c.is_active);
            let soak = test.soak.as_mut().unwrap();
            if !on {
                soak.fail("the scene's camera didn't come back".to_string());
            }
            soak.cycles += 1;
            let elapsed = soak.started.elapsed().as_secs_f64();
            let row = format!("{elapsed:.0},{},{drawn},{:.2},{worst:.1},{},{bodies}\n", soak.cycles, total / drawn.max(1) as f64, soak.failures);
            let _ = std::fs::OpenOptions::new().append(true).open(&soak.file).and_then(|mut f| std::io::Write::write_all(&mut f, row.as_bytes()));
            if elapsed >= soak.hours * 3600.0 {
                let (cycles, failures, first) = (soak.cycles, soak.failures, soak.first_failures.clone());
                test.check("the soak finishes with nothing having gone wrong", failures == 0, format!("{cycles} cycles in {:.2} hours, {failures} failures {first:?}", elapsed / 3600.0));
                finish(&mut test, &mut slot, &mut exit);
            } else {
                test.step = 20;
            }
        }
        _ => {}
    }
}

fn test_result<T: std::fmt::Debug, E: std::fmt::Display>(test: &mut DesignTest, name: &str, r: Result<T, E>) {
    match r {
        Ok(v) => test.check(name, true, format!("{v:?}")),
        Err(e) => test.check(name, false, e.to_string()),
    }
}

fn press(keys: &mut MessageWriter<KeyboardInput>, window: Entity, key_code: KeyCode, logical_key: Key) {
    keys.write(KeyboardInput { key_code, logical_key, state: ButtonState::Pressed, text: None, repeat: false, window });
}

fn release(keys: &mut MessageWriter<KeyboardInput>, window: Entity, key_code: KeyCode, logical_key: Key) {
    keys.write(KeyboardInput { key_code, logical_key, state: ButtonState::Released, text: None, repeat: false, window });
}

fn finish(test: &mut DesignTest, slot: &mut Slot, exit: &mut MessageWriter<AppExit>) {
    let pass = test.checks.iter().all(|c| c["pass"] == true);
    let report = json!({ "pass": pass, "checks": test.checks, "frames": slot.drawn, "mean_ms": slot.total_ms / slot.drawn.max(1) as f64, "worst_ms": slot.worst_ms });
    let path = format!("{}.json", test.out);
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap_or_default());
    println!("{} ({path})", if pass { "ALL PASSED" } else { "FAILED" });
    if let Some(h) = slot.host.as_mut() {
        h.shutdown();
    }
    exit.write(if pass { AppExit::Success } else { AppExit::from_code(1) });
}
