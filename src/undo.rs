//! Scene-level undo/redo (Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z).
//!
//! Rather than instrumenting every edit, this watches the scene: whenever it
//! settles (no button held, nothing loading) in a state that differs from the
//! last one, the previous state goes on the undo stack. Moves, rotations,
//! inspector edits, joint poses, placing, duplicating and deleting are all
//! covered, and a whole drag counts as one step.

use bevy::prelude::*;

use crate::prototype::PrototypeCache;
use crate::scene::{Falling, PendingSpawn, PendingSpawns, PlacedObject, Selection, Status, UiBlock};

const MAX_STEPS: usize = 100;

#[derive(Clone, PartialEq)]
struct ObjSnap {
    name: String,
    asset: std::path::PathBuf,
    transform: Transform,
    joints: Vec<f32>,
}

type Snapshot = Vec<ObjSnap>;

#[derive(Resource, Default)]
pub struct UndoStack {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Option<Snapshot>,
}

impl UndoStack {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Requests from menus/buttons.
#[derive(Resource, Default)]
pub struct UndoRequest {
    pub undo: bool,
    pub redo: bool,
}

fn snapshot(objects: &Query<(Entity, &PlacedObject, &Transform)>) -> Snapshot {
    let mut s: Snapshot = objects
        .iter()
        .map(|(_, o, t)| ObjSnap { name: o.name.clone(), asset: o.asset.clone(), transform: *t, joints: o.joints.clone() })
        .collect();
    s.sort_by(|a, b| a.name.cmp(&b.name));
    s
}

#[allow(clippy::too_many_arguments)]
pub fn undo_system(
    mut commands: Commands,
    mut stack: ResMut<UndoStack>,
    mut request: ResMut<UndoRequest>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    block: Res<UiBlock>,
    cache: Res<PrototypeCache>,
    mut pending: ResMut<PendingSpawns>,
    mut selection: ResMut<Selection>,
    mut status: ResMut<Status>,
    objects: Query<(Entity, &PlacedObject, &Transform)>,
    falling: Query<(), With<Falling>>,
    plant: Option<Res<crate::plant::Plant>>,
) {
    // Only record once the scene has settled (nothing loading, held or falling).
    if !pending.0.is_empty() || cache.loading_count() > 0 || mouse.pressed(MouseButton::Left) || !falling.is_empty() {
        return;
    }
    let current = snapshot(&objects);
    // The running plant (spinning joints, items on conveyors) is not an
    // edit: fold its changes into the baseline so they never become steps.
    if let Some(plant) = &plant
        && let Some(last) = stack.last.as_mut()
    {
        let live = &plant.live;
        if !live.posed.is_empty() || !live.moved.is_empty() {
            for s in last.iter_mut() {
                let (posed, moved) = (live.posed.contains(&s.name), live.moved.contains(&s.name));
                if (posed || moved)
                    && let Some(c) = current.iter().find(|c| c.name == s.name)
                {
                    if posed {
                        s.joints = c.joints.clone();
                    }
                    if moved {
                        s.transform = c.transform;
                    }
                }
            }
        }
    }
    match &stack.last {
        None => stack.last = Some(current.clone()),
        Some(last) if *last != current => {
            let prev = last.clone();
            stack.undo.push(prev);
            if stack.undo.len() > MAX_STEPS {
                stack.undo.remove(0);
            }
            stack.redo.clear();
            stack.last = Some(current.clone());
        }
        _ => {}
    }

    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut undo = std::mem::take(&mut request.undo);
    let mut redo = std::mem::take(&mut request.redo);
    if ctrl && !block.keyboard {
        undo |= keys.just_pressed(KeyCode::KeyZ) && !shift;
        redo |= keys.just_pressed(KeyCode::KeyY) || (keys.just_pressed(KeyCode::KeyZ) && shift);
    }
    let target = if undo {
        stack.undo.pop().inspect(|_| stack.redo.push(current.clone()))
    } else if redo {
        stack.redo.pop().inspect(|_| stack.undo.push(current.clone()))
    } else {
        None
    };
    let Some(target) = target else { return };

    // Restore: rebuild the scene from the snapshot (prototypes are cached).
    let selected_name = selection.selected.and_then(|e| objects.get(e).ok()).map(|(_, o, _)| o.name.clone());
    for (e, _, _) in &objects {
        commands.entity(e).despawn();
    }
    selection.set(None);
    for o in &target {
        pending.0.push(PendingSpawn {
            asset: o.asset.clone(),
            transform: o.transform,
            name: Some(o.name.clone()),
            joints: Some(o.joints.clone()),
            select: selected_name.as_deref() == Some(o.name.as_str()),
        });
    }
    status.0 = format!("{} ({} left)", if undo { "Undo" } else { "Redo" }, if undo { stack.undo.len() } else { stack.redo.len() });
    stack.last = Some(target);
}
