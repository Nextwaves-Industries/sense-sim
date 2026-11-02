//! What passes between the workbench shell and the design workspace each frame: the design strip's
//! commands one way, what it shows the other. Copied to `src/design/shell.rs` by the port script: it
//! needs the shell's types, which only a checkout with workbenches has.

use bevy::input::ButtonState;
use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::{Commands, Entity, MessageWriter, MouseButton, Vec2};
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{CursorMoved, WindowEvent};
use solidcraft_ui_egui::documents;
use solidcraft_ui_egui::toolbar::{self, FileAction};

use super::host::DesignHost;
use super::{Slot, Workspace};
use crate::ui::{DesignCommand, DesignInfo, LeftPanelSlot};

/// Runs what the strip asked for and tells it how things stand. True if SolidCraft has something new
/// to draw.
pub fn exchange(panel: &mut LeftPanelSlot, shell: &egui::Context, h: &mut DesignHost, rect: egui::Rect) -> bool {
    let commands = std::mem::take(&mut panel.design_commands);
    let mut changed = !commands.is_empty();
    for command in commands {
        match command {
            DesignCommand::New => FileAction::New.run(&mut h.app),
            DesignCommand::Save => FileAction::Save.run(&mut h.app),
            DesignCommand::Undo => FileAction::Undo.run(&mut h.app),
            DesignCommand::Redo => FileAction::Redo.run(&mut h.app),
            DesignCommand::Settings => FileAction::Settings.run(&mut h.app),
            DesignCommand::Show(i) => documents::switch(&mut h.app, i),
            DesignCommand::Close(i) => documents::close(&mut h.app, i, false),
            DesignCommand::FileMenu(x) => {
                let at = (x - rect.left()) / h.ctx.zoom_factor().max(0.01);
                let ctx = h.ctx.clone();
                toolbar::toggle_file_menu(&mut h.app, &ctx, at);
            }
        }
    }

    // A click on the shell's bars never reaches SolidCraft, so its menu wouldn't see it as a click away.
    let button = panel.design_file_button;
    let pressed_away = shell.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !button.is_some_and(|b| b.contains(p))));
    if pressed_away && toolbar::file_menu_open(&h.ctx) {
        toolbar::close_file_menu(&h.ctx);
        changed = true;
    }

    panel.design_info = DesignInfo {
        designs: documents::tabs_info(&h.app),
        active: h.app.docs.active,
        can_save: FileAction::Save.enabled(&h.app),
        can_undo: FileAction::Undo.enabled(&h.app),
        can_redo: FileAction::Redo.enabled(&h.app),
        file_menu_open: toolbar::file_menu_open(&h.ctx),
    };
    changed
}

/// The steps `--design-test` gives the strip, between its own.
pub const TEST_FIRST: usize = 40;
pub const TEST_LAST: usize = 61;
/// Presses: egui reads a long press as something other than a click, so the release follows quickly.
pub const TEST_PRESSES: [usize; 2] = [41, 45];

/// One step of the strip's test. Returns a check to record, if this step makes one.
#[allow(clippy::too_many_arguments)]
pub fn test_step(
    step: usize,
    slot: &mut Slot,
    panel: &mut LeftPanelSlot,
    workspace: &mut Workspace,
    status: &crate::scene::Status,
    commands: &mut Commands,
    events: &mut MessageWriter<WindowEvent>,
    window: Entity,
    out: &str,
    remembered: &mut usize,
) -> Option<(&'static str, bool, String)> {
    let move_to = |events: &mut MessageWriter<WindowEvent>, position: Vec2| {
        events.write(WindowEvent::CursorMoved(CursorMoved { window, position, delta: None }));
    };
    let button = |events: &mut MessageWriter<WindowEvent>, state: ButtonState| {
        events.write(WindowEvent::MouseButtonInput(MouseButtonInput { button: MouseButton::Left, state, window }));
    };
    let mut shot = |name: &str| {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{out}_{name}.png")));
    };
    let Slot { host, prompt, .. } = slot;
    let h = host.as_mut()?;
    let canvas = panel.studio_rect?;
    match step {
        // A real click on the strip's File button opens SolidCraft's menu.
        40 => move_to(events, panel.design_file_button.map_or(Vec2::ZERO, |r| Vec2::new(r.center().x, r.center().y))),
        41 | 45 => button(events, ButtonState::Pressed),
        42 | 46 => button(events, ButtonState::Released),
        43 => {
            shot("file_menu");
            return Some(("the strip's File button opens the design's File menu", toolbar::file_menu_open(&h.ctx), String::new()));
        }
        // A click on the shell's own bars, which SolidCraft can't see, closes it.
        44 => move_to(events, Vec2::new(canvas.right() - 40.0, canvas.top() - 45.0)),
        47 => {
            let stayed = panel.design_open;
            return Some(("a click on the shell's bars closes that menu and stays in the workspace", !toolbar::file_menu_open(&h.ctx) && stayed, format!("still open: {}, in the workspace: {stayed}", toolbar::file_menu_open(&h.ctx))));
        }
        48 => {
            *remembered = h.app.session.undo.len();
            panel.design_commands.push(DesignCommand::Undo);
        }
        49 => {
            let now = h.app.session.undo.len();
            panel.design_commands.push(DesignCommand::Redo);
            return Some(("the strip's Undo undoes in the design", now + 1 == *remembered, format!("undo steps {} -> {now}", *remembered)));
        }
        50 => {
            let now = h.app.session.undo.len();
            panel.design_commands.push(DesignCommand::New);
            return Some(("the strip's Redo redoes in the design", now == *remembered, format!("undo steps {} -> {now}", *remembered)));
        }
        51 => {
            let (n, active) = (panel.design_info.designs.len(), panel.design_info.active);
            panel.design_commands.push(DesignCommand::Show(0));
            return Some(("the strip's + opens a second design and shows it", n == 2 && active == 1, format!("{n} designs, showing {active}")));
        }
        52 => {
            let (active, bodies) = (panel.design_info.active, super::bridge::bodies(&h.app.session).len());
            panel.design_commands.push(DesignCommand::Close(1));
            return Some(("a design tab brings the first design back, with its bodies", active == 0 && bodies > 0, format!("showing {active}, {bodies} bodies")));
        }
        53 => {
            let n = panel.design_info.designs.len();
            panel.design_send = true;
            return Some(("closing a design tab closes that design", n == 1, format!("{n} designs")));
        }
        54 => {
            panel.design_licences = true;
            return Some(("the strip's Send to scene places the bodies", status.0.starts_with("Placed"), status.0.clone()));
        }
        55 => {
            shot("licences");
            return Some(("the strip's Licences is heard", !panel.design_licences, String::new()));
        }
        56 => *workspace = Workspace::Scene,
        57 => {
            shot("scene");
            *workspace = Workspace::Design;
            return Some(("leaving the workspace closes it for the shell, so the scene tabs come back", !panel.design_open, String::new()));
        }
        // A face pick needs its strip back: the prompt and Done are there.
        58 => *prompt = Some("Pick the faces to load, then press Done".into()),
        59 => shot("pick"),
        60 => *prompt = None,
        _ => {}
    }
    None
}
