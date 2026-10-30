//! Lets OS input methods (Vietnamese Telex / VNI, Chinese, Japanese…) type
//! into egui text fields. Bevy windows start with IME off and bevy_egui reads
//! IME events but never turns it on, so composing keyboards could barely type.
//! IME is on while a text field has focus, with the candidate window placed
//! at the text cursor.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::EguiContexts;

pub fn sync_ime(mut contexts: EguiContexts, mut window: Single<&mut Window, With<PrimaryWindow>>) -> Result {
    let ctx = contexts.ctx_mut()?;
    let ime = ctx.output(|o| o.ime);
    let want = ime.is_some() || ctx.egui_wants_keyboard_input();
    if window.ime_enabled != want {
        window.ime_enabled = want;
    }
    if let Some(ime) = ime {
        let p = ime.cursor_rect.left_bottom() * ctx.zoom_factor();
        let pos = Vec2::new(p.x, p.y);
        if window.ime_position != pos {
            window.ime_position = pos;
        }
    }
    Ok(())
}
