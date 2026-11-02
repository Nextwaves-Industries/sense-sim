//! Moves input from Sense Sim's egui context to SolidCraft's private one. The two differ by an
//! offset (SolidCraft is drawn in part of the window) and a scale (SolidCraft has its own UI zoom).

use egui::{Event, Pos2, Rect};

/// Events that must not reach Sense Sim while the design workspace is open: otherwise Ctrl+S would
/// save the layout as well as the design, and typing would drive the scene's shortcuts.
pub fn is_keyboard(e: &Event) -> bool {
    matches!(e, Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_) | Event::Ime(_))
}

/// Maps a point of the host's window into the private context.
pub fn to_private(p: Pos2, rect: Rect, zoom: f32) -> Pos2 {
    ((p - rect.min) / zoom.max(0.01)).to_pos2()
}

/// Maps a point of the private context back into the host's window.
pub fn to_host(p: Pos2, rect: Rect, zoom: f32) -> Pos2 {
    rect.min + p.to_vec2() * zoom
}

/// Keeps what belongs to the workspace, in its coordinates. Pointer events outside `rect` are
/// dropped, except that a press seen inside is matched by its release wherever that happens, so a
/// drag that leaves the area still ends. `pressed` tracks that between frames.
pub fn translate(events: &[Event], rect: Rect, zoom: f32, pressed: &mut u8) -> Vec<Event> {
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        match e {
            Event::PointerMoved(p) => {
                if rect.contains(*p) || *pressed != 0 {
                    out.push(Event::PointerMoved(to_private(*p, rect, zoom)));
                } else {
                    out.push(Event::PointerGone);
                }
            }
            Event::PointerButton { pos, button, pressed: down, modifiers } => {
                let bit = 1u8 << (*button as u8).min(7);
                let inside = rect.contains(*pos);
                if *down && !inside {
                    continue;
                }
                if !*down && *pressed & bit == 0 {
                    continue;
                }
                if *down {
                    *pressed |= bit;
                } else {
                    *pressed &= !bit;
                }
                out.push(Event::PointerButton { pos: to_private(*pos, rect, zoom), button: *button, pressed: *down, modifiers: *modifiers });
            }
            Event::PointerGone => {
                if *pressed == 0 {
                    out.push(Event::PointerGone);
                }
            }
            Event::MouseWheel { unit, delta, phase, modifiers } => out.push(Event::MouseWheel { unit: *unit, delta: *delta / zoom.max(0.01), phase: *phase, modifiers: *modifiers }),
            Event::Touch { device_id, id, phase, pos, force } => {
                if rect.contains(*pos) {
                    out.push(Event::Touch { device_id: *device_id, id: *id, phase: *phase, pos: to_private(*pos, rect, zoom), force: *force });
                }
            }
            // Screenshots and accessibility requests are about the host's own context.
            Event::Screenshot { .. } => {}
            #[allow(unreachable_patterns)]
            Event::AccessKitActionRequest(_) => {}
            other => out.push(other.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, PointerButton, pos2, vec2};

    fn rect() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0))
    }

    fn press(p: Pos2, down: bool) -> Event {
        Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: Modifiers::NONE }
    }

    #[test]
    fn points_map_there_and_back() {
        let p = pos2(300.0, 250.0);
        let q = to_private(p, rect(), 1.25);
        assert_eq!(q, pos2(160.0, 160.0));
        assert_eq!(to_host(q, rect(), 1.25), p);
    }

    #[test]
    fn a_drag_leaving_the_area_still_ends() {
        let mut pressed = 0;
        let out = translate(&[press(pos2(200.0, 100.0), true)], rect(), 1.0, &mut pressed);
        assert_eq!(out.len(), 1);
        assert_ne!(pressed, 0);
        // Moves stay delivered while the button is held, even outside.
        let out = translate(&[Event::PointerMoved(pos2(5.0, 5.0))], rect(), 1.0, &mut pressed);
        assert!(matches!(out[0], Event::PointerMoved(_)));
        let out = translate(&[press(pos2(5.0, 5.0), false)], rect(), 1.0, &mut pressed);
        assert_eq!(out.len(), 1);
        assert_eq!(pressed, 0);
    }

    #[test]
    fn a_press_outside_is_not_the_workspaces() {
        let mut pressed = 0;
        assert!(translate(&[press(pos2(5.0, 5.0), true)], rect(), 1.0, &mut pressed).is_empty());
        assert_eq!(pressed, 0);
        // Nor the release that goes with it.
        assert!(translate(&[press(pos2(200.0, 100.0), false)], rect(), 1.0, &mut pressed).is_empty());
    }

    #[test]
    fn leaving_the_area_says_so() {
        let mut pressed = 0;
        let out = translate(&[Event::PointerMoved(pos2(5.0, 5.0))], rect(), 1.0, &mut pressed);
        assert!(matches!(out[0], Event::PointerGone));
    }

    #[test]
    fn keyboard_events_are_recognised() {
        assert!(is_keyboard(&Event::Text("a".into())));
        assert!(is_keyboard(&Event::Copy));
        assert!(!is_keyboard(&Event::PointerGone));
    }
}
