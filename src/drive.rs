//! Debug builds only: `--drive "wait:5;click:96,722;probe"` plays mouse input
//! through the whole app, as the OS window would, so the UI can be exercised in
//! scripted runs (with `--screenshot`) without touching the real pointer.
//!
//! Steps, separated by `;`, in logical points: `wait:SECONDS`, `move:X,Y`,
//! `click:X,Y`, `rclick:X,Y`, `drag:X,Y,X2,Y2`, `rdrag:X,Y,X2,Y2`, `wheel:LINES`,
//! `type:TEXT` for characters, `key:NAME` for a named key, as in `key:Enter`,
//! `key:ArrowDown`, `key:Tab`, `key:Escape`, `key:Backspace` or `key:ctrl+Space`,
//! and `probe`, which prints whether the UI claims the pointer.
//!
//! The window's own cursor position is left alone (setting it would warp the real
//! pointer), so scene picking, which reads it, does not see the played moves.

use std::collections::VecDeque;

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyCode, KeyboardInput, NativeKeyCode};
use bevy::input::mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::window::{CursorMoved, PrimaryWindow, WindowEvent};

use crate::scene::UiBlock;

enum Step {
    Wait(f32),
    Move(Vec2),
    Button(MouseButton, ButtonState),
    Wheel(f32),
    Key(KeyCode, Key, ButtonState),
    Probe,
}

/// A key the physical code and meaning of which egui can read.
fn named_key(name: &str) -> Option<(KeyCode, Key)> {
    Some(match name {
        "Enter" => (KeyCode::Enter, Key::Enter),
        "Tab" => (KeyCode::Tab, Key::Tab),
        "Escape" => (KeyCode::Escape, Key::Escape),
        "Backspace" => (KeyCode::Backspace, Key::Backspace),
        "Space" => (KeyCode::Space, Key::Space),
        "ArrowDown" => (KeyCode::ArrowDown, Key::ArrowDown),
        "ArrowUp" => (KeyCode::ArrowUp, Key::ArrowUp),
        "ArrowLeft" => (KeyCode::ArrowLeft, Key::ArrowLeft),
        "ArrowRight" => (KeyCode::ArrowRight, Key::ArrowRight),
        _ => return None,
    })
}

#[derive(Resource)]
pub struct Drive {
    steps: VecDeque<Step>,
    wait: f32,
    at: Vec2,
}

impl Drive {
    pub fn parse(script: &str) -> Self {
        let mut steps = VecDeque::new();
        for part in script.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (op, rest) = part.split_once(':').unwrap_or((part, ""));
            let n: Vec<f32> = rest.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            let at = |i: usize| Vec2::new(n.get(i).copied().unwrap_or(0.0), n.get(i + 1).copied().unwrap_or(0.0));
            match op {
                "wait" => steps.push_back(Step::Wait(n.first().copied().unwrap_or(0.0))),
                "move" => steps.push_back(Step::Move(at(0))),
                "click" | "rclick" => {
                    let b = if op == "click" { MouseButton::Left } else { MouseButton::Right };
                    steps.push_back(Step::Move(at(0)));
                    steps.push_back(Step::Button(b, ButtonState::Pressed));
                    steps.push_back(Step::Button(b, ButtonState::Released));
                }
                "drag" | "rdrag" => {
                    let b = if op == "drag" { MouseButton::Left } else { MouseButton::Right };
                    steps.push_back(Step::Move(at(0)));
                    steps.push_back(Step::Button(b, ButtonState::Pressed));
                    for k in 1..=8 {
                        steps.push_back(Step::Move(at(0).lerp(at(2), k as f32 / 8.0)));
                    }
                    steps.push_back(Step::Button(b, ButtonState::Released));
                }
                "wheel" => steps.push_back(Step::Wheel(n.first().copied().unwrap_or(0.0))),
                "type" => {
                    for c in rest.chars() {
                        let code = KeyCode::Unidentified(NativeKeyCode::Unidentified);
                        steps.push_back(Step::Key(code, Key::Character(c.to_string().into()), ButtonState::Pressed));
                        steps.push_back(Step::Key(code, Key::Character(c.to_string().into()), ButtonState::Released));
                    }
                }
                "key" => {
                    let (ctrl, name) = rest.strip_prefix("ctrl+").map_or((false, rest), |n| (true, n));
                    let Some((code, key)) = named_key(name) else {
                        warn!("--drive: unknown key {name}");
                        continue;
                    };
                    if ctrl {
                        steps.push_back(Step::Key(KeyCode::ControlLeft, Key::Control, ButtonState::Pressed));
                    }
                    steps.push_back(Step::Key(code, key.clone(), ButtonState::Pressed));
                    steps.push_back(Step::Key(code, key, ButtonState::Released));
                    if ctrl {
                        steps.push_back(Step::Key(KeyCode::ControlLeft, Key::Control, ButtonState::Released));
                    }
                }
                "probe" => steps.push_back(Step::Probe),
                other => warn!("--drive: unknown step {other}"),
            }
        }
        Self { steps, wait: 0.0, at: Vec2::ZERO }
    }
}

/// Plays one step at a time, a few frames apart, so egui sees each on its own.
pub fn play(
    mut drive: ResMut<Drive>,
    time: Res<Time>,
    window: Single<Entity, With<PrimaryWindow>>,
    block: Res<UiBlock>,
    mut moved: MessageWriter<CursorMoved>,
    mut buttons: MessageWriter<MouseButtonInput>,
    mut motion: MessageWriter<MouseMotion>,
    mut wheel: MessageWriter<MouseWheel>,
    mut keys: MessageWriter<KeyboardInput>,
    mut window_events: MessageWriter<WindowEvent>,
) {
    drive.wait -= time.delta_secs();
    if drive.wait > 0.0 {
        return;
    }
    let Some(step) = drive.steps.pop_front() else { return };
    drive.wait = 0.1;
    match step {
        Step::Wait(s) => drive.wait = s,
        Step::Move(p) => {
            let delta = p - drive.at;
            drive.at = p;
            let event = CursorMoved { window: *window, position: p, delta: Some(delta) };
            // As the window backend does: egui reads the first, the rest of Bevy the second.
            window_events.write(WindowEvent::from(event.clone()));
            moved.write(event);
            motion.write(MouseMotion { delta });
        }
        Step::Button(button, state) => {
            let event = MouseButtonInput { button, state, window: *window };
            window_events.write(WindowEvent::from(event));
            buttons.write(event);
        }
        Step::Wheel(lines) => {
            let event = MouseWheel { unit: MouseScrollUnit::Line, x: 0.0, y: lines, window: *window, phase: bevy::input::touch::TouchPhase::Moved };
            window_events.write(WindowEvent::from(event));
            wheel.write(event);
        }
        Step::Key(key_code, logical_key, state) => {
            keys.write(KeyboardInput { key_code, logical_key, state, text: None, repeat: false, window: *window });
        }
        Step::Probe => println!("probe at {:?}: pointer over UI = {}", drive.at, block.pointer),
    }
}
