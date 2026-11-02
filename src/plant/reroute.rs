//! Moving a piped object by hand keeps its links but not its pipes: the
//! flanges would part. When the user lets go of an object whose pipe ports
//! no longer meet their partners, each run of generated pipes and elbows
//! from that port to the next real part (tank, valve, tee, pump...) is
//! routed again: longer, shorter or with new elbows as needed.
//!
//! The new pieces load first; the old run and its links are swapped out in
//! one frame once they are all in the scene, so the plant never sees an
//! open pipe end in between.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::connect::{self, Link, LinkKind, Links, PortRef};
use super::parts::Spec;
use super::ports::PortKind;
use super::recipes::Namer;
use super::route::{self, Obstacle, Route};
use super::sim::{Model, Obj};
use crate::scene::{DragState, Falling, PendingSpawn, PendingSpawns, PlacedObject, Status};

/// Ports closer than this (m, degrees) still count as mated.
const MATED_M: f32 = 0.005;
const MATED_DEG: f32 = 3.0;
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// One pipe run to replace.
pub struct Rerun {
    /// The moved object's port and the far end of the run.
    pub from: PortRef,
    pub to: PortRef,
    /// The old pipes and elbows between them.
    pub remove: Vec<String>,
    pub route: Route,
}

/// Generated straight pipes and elbows: what a route is made of.
fn run_piece(o: &Obj) -> bool {
    matches!(Spec::of_asset(&o.asset), Some(Spec::Pipe { .. } | Spec::Elbow { .. }))
}

pub fn obstacles(model: &Model, skip: &HashSet<&str>) -> Vec<Obstacle> {
    use crate::agent::structures::Kind;
    model
        .objs
        .iter()
        .filter(|o| !skip.contains(o.name.as_str()) && !matches!(Kind::of_asset(&o.asset), Some(Kind::Floor | Kind::Ceiling)))
        .map(|o| {
            let (min, max) = o.world_bounds();
            Obstacle { name: o.name.clone(), min, max }
        })
        .collect()
}

/// New runs for every pipe port of `moved` that has parted from its partner.
/// Errors name the port and why it was left as it is.
pub fn plan(model: &Model, links: &Links, moved: &str, names: &mut Namer) -> (Vec<Rerun>, Vec<String>) {
    let (mut runs, mut errors) = (Vec::new(), Vec::new());
    let Some(o) = model.get(moved) else { return (runs, errors) };
    // Dragging a pipe itself: nothing sensible to re-route around it.
    if run_piece(o) {
        return (runs, errors);
    }
    for p in o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe) {
        let me = PortRef::new(moved, &p.id);
        let Some((link, other)) = links.partner(&me) else { continue };
        if link.kind != LinkKind::Pipe {
            continue;
        }
        let (Some(a), Some(b)) = (model.port_world(&me), model.port_world(other)) else { continue };
        let (err, ang) = connect::misalignment(LinkKind::Pipe, (a.0, a.1), (b.0, b.1));
        if err < MATED_M && ang < MATED_DEG {
            continue;
        }
        // Walk the run of generated pipes and elbows to the next real part.
        let mut remove: Vec<String> = Vec::new();
        let mut cur = other.clone();
        let end = loop {
            let Some(x) = model.get(&cur.object) else { break Err(format!("{} is missing", cur.object)) };
            if !run_piece(x) {
                break Ok(cur);
            }
            if remove.len() > 500 {
                break Err("the pipe run does not end".into());
            }
            remove.push(x.name.clone());
            let Some(exit) = x.info.ports.iter().find(|q| q.kind == PortKind::Pipe && !q.id.eq_ignore_ascii_case(&cur.port)) else {
                break Err(format!("{} has one pipe end", x.name));
            };
            let exit = PortRef::new(&x.name, &exit.id);
            match links.partner(&exit) {
                Some((_, next)) if next.object == moved => break Err("the pipe run loops back to it".into()),
                Some((_, next)) => cur = next.clone(),
                None => break Err(format!("the run ends open at {}", exit.label())),
            }
        };
        let end = match end {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{}: {e}", me.label()));
                continue;
            }
        };
        let Some(c) = model.port_world(&end) else { continue };
        let skip: HashSet<&str> = remove.iter().map(String::as_str).collect();
        let obs = obstacles(model, &skip);
        let mut nm = |k: &str| names.name(k);
        match route::plan((&me, a.0, a.1), (&end, c.0, c.1), p.size, &[], &obs, &mut nm) {
            Ok(route) => runs.push(Rerun { from: me, to: end, remove, route }),
            Err(e) => errors.push(format!("{} → {}: {e}", me.label(), end.label())),
        }
    }
    (runs, errors)
}

/// Runs waiting for their new pieces to load.
#[derive(Resource, Default)]
pub struct Rerouting(Vec<Job>);

impl Rerouting {
    pub fn busy(&self) -> bool {
        !self.0.is_empty()
    }
}

struct Job {
    run: Rerun,
    added: Vec<String>,
    started: Instant,
}

/// Re-routes the pipes of each object the user drops, once it has landed.
#[allow(clippy::too_many_arguments)]
pub fn reroute_dropped(
    mut commands: Commands,
    drag: Res<DragState>,
    mut last_held: Local<Option<Entity>>,
    mut dropped: Local<Vec<Entity>>,
    mut jobs: ResMut<Rerouting>,
    objects: Query<(Entity, &PlacedObject, &Transform)>,
    falling: Query<(), With<Falling>>,
    mut links: ResMut<Links>,
    mut pending: ResMut<PendingSpawns>,
    mut status: ResMut<Status>,
) {
    let held = drag.holding();
    if let Some(e) = *last_held
        && held != Some(e)
        && !dropped.contains(&e)
    {
        dropped.push(e);
    }
    *last_held = held;

    // Swap in finished runs.
    if !jobs.0.is_empty() {
        let present: HashSet<&str> = objects.iter().map(|(_, o, _)| o.name.as_str()).collect();
        let waiting: HashSet<String> = pending.0.iter().filter_map(|p| p.name.clone()).collect();
        let mut keep = Vec::new();
        for job in std::mem::take(&mut jobs.0) {
            if job.added.iter().all(|n| present.contains(n.as_str())) {
                let gone: HashSet<&str> = job.run.remove.iter().map(String::as_str).collect();
                for (e, o, _) in &objects {
                    if gone.contains(o.name.as_str()) {
                        commands.entity(e).despawn();
                    }
                }
                links.0.retain(|l| !gone.contains(l.a.object.as_str()) && !gone.contains(l.b.object.as_str()) && l.other(&job.run.from).is_none());
                for l in job.run.route.links.iter().cloned() {
                    links.add(l);
                }
                status.0 = format!(
                    "Re-routed {} → {}: {:.2} m, {} elbow{}",
                    job.run.from.label(),
                    job.run.to.label(),
                    job.run.route.length,
                    job.run.route.elbows,
                    if job.run.route.elbows == 1 { "" } else { "s" }
                );
            } else if job.started.elapsed() > LOAD_TIMEOUT || !job.added.iter().all(|n| present.contains(n.as_str()) || waiting.contains(n)) {
                // A piece failed to load: leave the old run, drop what came.
                let added: HashSet<&str> = job.added.iter().map(String::as_str).collect();
                for (e, o, _) in &objects {
                    if added.contains(o.name.as_str()) {
                        commands.entity(e).despawn();
                    }
                }
                pending.0.retain(|p| !p.name.as_deref().is_some_and(|n| added.contains(n)));
                status.0 = format!("Could not re-route {}: the new pipe parts did not load", job.run.from.label());
            } else {
                keep.push(job);
            }
        }
        jobs.0 = keep;
    }

    if dropped.is_empty() {
        return;
    }
    // Wait until it has landed (gravity) and nothing else is mid-swap.
    let ready: Vec<Entity> = dropped.iter().copied().filter(|e| Some(*e) != held && !falling.contains(*e)).collect();
    if ready.is_empty() || jobs.busy() {
        return;
    }
    dropped.retain(|e| !ready.contains(e));
    let model = Model::build(objects.iter());
    let mut taken: HashSet<String> = objects.iter().map(|(_, o, _)| o.name.clone()).collect();
    taken.extend(pending.0.iter().filter_map(|p| p.name.clone()));
    let mut names = Namer::new("", taken);
    let mut errors = Vec::new();
    for e in ready {
        let Ok((_, o, _)) = objects.get(e) else { continue };
        let (runs, errs) = plan(&model, &links, &o.name, &mut names);
        errors.extend(errs);
        for run in runs {
            // Re-routing replaces the pipe, which a lock rules out.
            if run.remove.iter().any(|n| objects.iter().any(|(_, o, _)| o.locked && o.name == *n)) {
                errors.push(format!("{}: part of this pipe is locked", run.from.label()));
                continue;
            }
            if run.route.pieces.is_empty() {
                // Ports face each other at the right spot again: just relink.
                let gone: HashSet<&str> = run.remove.iter().map(String::as_str).collect();
                for (e, o, _) in &objects {
                    if gone.contains(o.name.as_str()) {
                        commands.entity(e).despawn();
                    }
                }
                links.0.retain(|l| !gone.contains(l.a.object.as_str()) && !gone.contains(l.b.object.as_str()) && l.other(&run.from).is_none());
                links.add(Link { a: run.from.clone(), b: run.to.clone(), kind: LinkKind::Pipe });
                continue;
            }
            let mut added = Vec::new();
            let mut ok = true;
            let mut spawns = Vec::new();
            for p in &run.route.pieces {
                match p.spec.usd() {
                    Ok(asset) => {
                        added.push(p.name.clone());
                        spawns.push(PendingSpawn { asset, transform: p.transform, name: Some(p.name.clone()), joints: None, select: false, locked: false });
                    }
                    Err(err) => {
                        errors.push(format!("{}: cannot write pipe parts: {err}", run.from.label()));
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                pending.0.extend(spawns);
                jobs.0.push(Job { run, added, started: Instant::now() });
            }
        }
    }
    if !errors.is_empty() {
        status.0 = format!("Pipes not re-routed: {}", errors.join("; "));
    }
}
