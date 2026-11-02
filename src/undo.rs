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
    locked: bool,
}

/// Objects, the plant links between them (a re-routed pipe changes both), and where real
/// devices were placed. A device's live status isn't an edit, so it stays out.
#[derive(Clone, PartialEq)]
struct Snapshot {
    objs: Vec<ObjSnap>,
    links: Vec<crate::plant::connect::Link>,
    twin: std::collections::BTreeMap<twin_proto::model::DeviceId, crate::twin::Placement>,
}

#[derive(Resource, Default)]
pub struct UndoStack {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Option<Snapshot>,
    /// Counts edits; the scene is saved while this equals `saved`.
    revision: u64,
    saved: u64,
    /// A layout is being read: frames left until its result can become the baseline.
    rebase: Option<u8>,
}

impl UndoStack {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// Counts edits, for noticing that one just happened.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Changed since it was last saved or opened.
    pub fn dirty(&self) -> bool {
        self.revision != self.saved
    }
    pub fn mark_saved(&mut self) {
        self.saved = self.revision;
    }
    /// An edit the snapshots don't cover.
    pub fn touch(&mut self) {
        self.revision += 1;
    }
    /// The scene is being opened: once it has settled that is the starting point,
    /// not an edit. Some frames must pass: a load's entities arrive through commands.
    pub fn rebase(&mut self) {
        self.rebase = Some(3);
    }
}

/// Requests from menus/buttons.
#[derive(Resource, Default)]
pub struct UndoRequest {
    pub undo: bool,
    pub redo: bool,
}

fn snapshot(objects: &Query<(Entity, &PlacedObject, &Transform)>, links: Option<&crate::plant::Links>, twin: Option<&crate::twin::TwinScene>) -> Snapshot {
    let mut s: Vec<ObjSnap> = objects
        .iter()
        .map(|(_, o, t)| ObjSnap { name: o.name.clone(), asset: o.asset.clone(), transform: *t, joints: o.joints.clone(), locked: o.locked })
        .collect();
    s.sort_by(|a, b| a.name.cmp(&b.name));
    Snapshot { objs: s, links: links.map(|l| l.0.clone()).unwrap_or_default(), twin: twin.map(|t| t.placed.clone()).unwrap_or_default() }
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
    mut links: Option<ResMut<crate::plant::Links>>,
    rerouting: Option<Res<crate::plant::reroute::Rerouting>>,
    (edited, mut removed, downloads, mut stale, mut twin): (
        Query<(), (With<PlacedObject>, Or<(Changed<PlacedObject>, Changed<Transform>)>)>,
        RemovedComponents<PlacedObject>,
        Res<crate::scene::PendingDownloads>,
        Local<bool>,
        Option<ResMut<crate::twin::TwinScene>>,
    ),
) {
    // Comparing means copying the whole scene, so it only happens after a change.
    *stale |= !edited.is_empty() || removed.read().next().is_some() || links.as_ref().is_some_and(|l| l.is_changed()) || twin.as_ref().is_some_and(|t| t.is_changed());
    if let Some(n) = stack.rebase.as_mut() {
        *n = n.saturating_sub(1);
    }
    // Only record once the scene has settled (nothing loading, held or falling).
    if !pending.0.is_empty() || cache.loading_count() > 0 || mouse.pressed(MouseButton::Left) || !falling.is_empty() || rerouting.is_some_and(|r| r.busy()) {
        return;
    }
    if let Some(n) = stack.rebase {
        if n > 0 || !downloads.0.is_empty() {
            return;
        }
        stack.rebase = None;
        stack.undo.clear();
        stack.redo.clear();
        stack.last = Some(snapshot(&objects, links.as_deref(), twin.as_deref()));
        stack.saved = stack.revision;
        *stale = false;
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut undo = std::mem::take(&mut request.undo);
    let mut redo = std::mem::take(&mut request.redo);
    if ctrl && !block.keyboard {
        undo |= keys.just_pressed(KeyCode::KeyZ) && !shift;
        redo |= keys.just_pressed(KeyCode::KeyY) || (keys.just_pressed(KeyCode::KeyZ) && shift);
    }
    if !*stale && stack.last.is_some() && !undo && !redo {
        return;
    }
    *stale = false;
    let current = snapshot(&objects, links.as_deref(), twin.as_deref());
    // The running plant (spinning joints, items on conveyors) is not an
    // edit: fold its changes into the baseline so they never become steps.
    if let Some(plant) = &plant
        && let Some(last) = stack.last.as_mut()
    {
        let live = &plant.live;
        if !live.posed.is_empty() || !live.moved.is_empty() {
            for s in last.objs.iter_mut() {
                let (posed, moved) = (live.posed.contains(&s.name), live.moved.contains(&s.name));
                if (posed || moved)
                    && let Some(c) = current.objs.iter().find(|c| c.name == s.name)
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
            stack.revision += 1;
        }
        _ => {}
    }

    let target = if undo {
        stack.undo.pop().inspect(|_| stack.redo.push(current.clone()))
    } else if redo {
        stack.redo.pop().inspect(|_| stack.undo.push(current.clone()))
    } else {
        None
    };
    let Some(target) = target else { return };
    stack.revision += 1;

    // Restore: rebuild the scene from the snapshot (prototypes are cached).
    let selected_name = selection.selected.and_then(|e| objects.get(e).ok()).map(|(_, o, _)| o.name.clone());
    for (e, _, _) in &objects {
        commands.entity(e).despawn();
    }
    selection.set(None);
    if let Some(links) = links.as_mut() {
        links.0 = target.links.clone();
    }
    if let Some(twin) = twin.as_mut() {
        twin.placed = target.twin.clone();
    }
    for o in &target.objs {
        pending.0.push(PendingSpawn {
            asset: o.asset.clone(),
            transform: o.transform,
            name: Some(o.name.clone()),
            joints: Some(o.joints.clone()),
            select: selected_name.as_deref() == Some(o.name.as_str()),
            locked: o.locked,
        });
    }
    status.0 = format!("{} ({} left)", if undo { "Undo" } else { "Redo" }, if undo { stack.undo.len() } else { stack.redo.len() });
    stack.last = Some(target);
}
