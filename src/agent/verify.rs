//! Self-check of the scene, measured in code rather than judged by the LLM:
//! objects floating above or sunk into what's below them, solid objects that
//! interpenetrate, and (for photo rebuilds) how well each placed object lines
//! up with its detection in the photo.

use bevy::ecs::system::SystemState;
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastBackfaces, RayCastVisibility};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::vision::geometry;
use super::vision::perceive::{PhotoScene, scenes};
use super::structures::Kind;
use crate::scene::{PartOf, PlacedObject};

/// Gap/penetration tolerance in meters.
const TOL: f32 = 0.03;

/// Open structures that other objects legitimately sit inside (their
/// bounding box overlaps whatever is on their shelves).
const OPEN: [&str; 14] =
    ["rack", "shelf", "shelv", "table", "desk", "cart", "trolley", "cabinet", "bench", "workbench", "stand", "frame", "conveyor", "pallet"];

fn is_open(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    OPEN.iter().any(|k| n.contains(k))
}

struct Obj {
    entity: Entity,
    name: String,
    asset: String,
    path: std::path::PathBuf,
    min: Vec3,
    max: Vec3,
    /// Ceiling or roof beam: hangs, so it has no support to check.
    hanging: bool,
}

fn world_bounds(o: &PlacedObject, t: &Transform) -> (Vec3, Vec3) {
    let (a, b) = (o.proto.aabb_min, o.proto.aabb_max);
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for x in [a.x, b.x] {
        for y in [a.y, b.y] {
            for z in [a.z, b.z] {
                let p = t.transform_point(Vec3::new(x, y, z));
                mn = mn.min(p);
                mx = mx.max(p);
            }
        }
    }
    (mn, mx)
}

fn overlap(a: &Obj, b: &Obj) -> Vec3 {
    (a.max.min(b.max) - a.min.max(b.min)).max(Vec3::ZERO)
}

/// Checks the whole scene. Returns `{passed, objects, issues[]}`.
pub fn verify_scene(world: &mut World) -> Value {
    let objs: Vec<Obj> = {
        let mut q = world.query::<(Entity, &PlacedObject, &Transform)>();
        q.iter(world)
            .map(|(e, o, t)| {
                let (min, max) = world_bounds(o, t);
                let asset = o.asset.parent().and_then(|p| p.parent()).and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                // Piping and shaft-mounted pulleys hang on their connections.
                let hanging = super::assetgen::hangs(&o.asset)
                    || matches!(
                        crate::plant::parts::Spec::of_asset(&o.asset).map(|s| s.role()),
                        Some(crate::plant::ports::Role::Fitting | crate::plant::ports::Role::Valve | crate::plant::ports::Role::Rotor)
                    );
                Obj { entity: e, name: o.name.clone(), asset, path: o.asset.clone(), min, max, hanging }
            })
            .collect()
    };
    let mut issues: Vec<String> = Vec::new();

    // USD meshes (shelf decks especially) are often single-sided with either
    // winding; count hits from both sides. Not the ceiling: it must stay
    // see-through from above so clicks reach the objects under it.
    let parts: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &PartOf), Without<RayCastBackfaces>>();
        q.iter(world).filter(|(_, p)| !objs.iter().any(|o| o.entity == p.0 && o.hanging)).map(|(e, _)| e).collect()
    };
    for e in parts {
        world.entity_mut(e).insert(RayCastBackfaces);
    }

    // Support: cast down from just above each object's base, ignoring itself.
    let mut state: SystemState<(MeshRayCast, Query<&PartOf>)> = SystemState::new(world);
    if let Ok((mut ray_cast, part_of)) = state.get_mut(world) {
        let name_of = |e: Entity, part_of: &Query<&PartOf>| {
            part_of.get(e).ok().and_then(|p| objs.iter().find(|x| x.entity == p.0)).map(|x| x.name.clone())
        };
        let all_hits = |_: Entity| false;
        for o in objs.iter().filter(|o| !o.hanging) {
            let base = o.min.y;
            let height = o.max.y - o.min.y;
            let c = (o.min + o.max) * 0.5;
            let half = (o.max - o.min) * 0.5;
            let me = o.entity;
            let not_me = |e: Entity| part_of.get(e).map(|p| p.0 != me).unwrap_or(false);
            let settings = MeshRayCastSettings::default()
                .with_visibility(RayCastVisibility::Any)
                .with_filter(&not_me)
                .with_early_exit_test(&all_hits);
            // Support: the highest surface in a window just under the base, sampled
            // at the center and four inset footprint points.
            let window = (0.35 * height).clamp(0.05, 0.3);
            let mut top_below: f32 = 0.0; // the floor
            let mut on_what = "the floor".to_string();
            for (dx, dz) in [(0.0, 0.0), (0.6, 0.6), (-0.6, 0.6), (0.6, -0.6), (-0.6, -0.6)] {
                let origin = Vec3::new(c.x + half.x * dx, base + window, c.z + half.z * dz);
                let hits = ray_cast.cast_ray(Ray3d::new(origin, Dir3::NEG_Y), &settings);
                if let Some((e, h)) = hits.first()
                    && h.point.y > top_below
                {
                    top_below = h.point.y;
                    on_what = name_of(*e, &part_of).unwrap_or(on_what);
                }
            }
            let gap = base - top_below;
            if gap > TOL {
                issues.push(format!("{} floats {:.0} cm above {on_what} (base y={:.2}, surface y={:.2})", o.name, gap * 100.0, base, top_below));
            } else if gap < -TOL {
                issues.push(format!("{} is sunk {:.0} cm into {on_what} (base y={:.2}, surface y={:.2})", o.name, -gap * 100.0, base, top_below));
            }
            // A surface crossing the object (e.g. a shelf deck cutting through a
            // box placed at the wrong height). Open structures contain things by design.
            // Nor do walls and columns count: roof beams bear on them.
            let carries_roof = matches!(Kind::of_asset(&o.path), Some(Kind::Wall | Kind::Column));
            if !is_open(&o.asset) && !carries_roof {
                let origin = Vec3::new(c.x, base + 0.01, c.z);
                let first = ray_cast.cast_ray(Ray3d::new(origin, Dir3::Y), &settings).first().map(|(e, h)| (*e, h.point.y));
                if let Some((e, hit_y)) = first
                    && hit_y < o.max.y - TOL
                {
                    let other = name_of(e, &part_of).unwrap_or_else(|| "another object".into());
                    // Top of that surface: first hit looking down from just above the object.
                    let from = Vec3::new(c.x, o.max.y + 0.02, c.z);
                    let top = ray_cast
                        .cast_ray(Ray3d::new(from, Dir3::NEG_Y), &settings)
                        .first()
                        .map(|(_, d)| d.point.y)
                        .filter(|y| *y >= hit_y)
                        .unwrap_or(hit_y);
                    issues.push(format!(
                        "{} is cut by {other} at y={:.2} (it spans y={:.2}..{:.2}); set its y to {:.2} to rest on that surface, or lower it below",
                        o.name, hit_y, base, o.max.y, top
                    ));
                }
            }
        }
    }

    // Interpenetration of solid (closed) objects. Parts joined port to port
    // touch by design, and piping is checked by the plant checks below.
    let links = world.resource::<crate::plant::Links>().clone();
    let piping = |o: &Obj| (o.hanging && !super::assetgen::hangs(&o.path)) || super::assetgen::is_photo_standin(&o.path);
    for (i, a) in objs.iter().enumerate() {
        for b in &objs[i + 1..] {
            if is_open(&a.asset) || is_open(&b.asset) {
                continue;
            }
            // Walls meet at corners, and the bounds of angled ones overlap widely.
            if Kind::of_asset(&a.path) == Some(Kind::Wall) && Kind::of_asset(&b.path) == Some(Kind::Wall) {
                continue;
            }
            if piping(a) || piping(b) || links.0.iter().any(|l| l.touches(&a.name) && l.touches(&b.name)) {
                continue;
            }
            let ov = overlap(a, b);
            if ov.x > TOL && ov.y > TOL && ov.z > TOL {
                let va = (a.max - a.min).element_product();
                let vb = (b.max - b.min).element_product();
                let frac = ov.element_product() / va.min(vb).max(1e-6);
                if frac > 0.05 {
                    issues.push(format!(
                        "{} and {} intersect ({:.0}% of the smaller one; overlap {:.2} x {:.2} x {:.2} m)",
                        a.name,
                        b.name,
                        frac * 100.0,
                        ov.x,
                        ov.y,
                        ov.z
                    ));
                }
            }
        }
    }

    // Alignment with reference photos.
    let alignment = photo_alignment(world, &mut issues);

    // A rebuilt room has a roof: the photo rarely shows enough of one to
    // measure, so this is where forgetting it gets caught.
    if !objs.is_empty() && !objs.iter().any(|o| Kind::of_asset(&o.path) == Some(Kind::Ceiling)) {
        let photos = scenes().lock().unwrap();
        let live = |sc: &&PhotoScene| sc.placed.keys().any(|n| objs.iter().any(|o| o.name == *n));
        if let Some(sc) = photos.values().filter(|sc| !sc.no_ceiling).find(|sc| live(sc)) {
            issues.push(format!(
                "the rebuild of {} has no ceiling: add_structure {{from_photo: \"{}\"}} (or kind \"ceiling\" with height_m). If the photo really is outdoors, pass ceiling false.",
                sc.image_id, sc.image_id
            ));
        }
    }

    // Connected systems: misaligned links, open pipe ends, undriven pumps,
    // unlinked conveyor transfers, pipes through objects, plant alarms.
    issues.extend(crate::plant::tools::check_issues(world));

    let total = issues.len();
    issues.truncate(40);
    json!({
        "passed": total == 0,
        "objects": objs.len(),
        "issue_count": total,
        "issues": issues,
        "photo_alignment": alignment,
        "note": "Floating/sunk is measured by casting rays down from each object's base; intersections ignore open structures (racks, shelves, tables, pallets) whose bounds legitimately contain other objects. Photo alignment is the overlap (IoU) between each object's outline projected into the photo and its detection. Connected systems (pipes, shafts, belts, conveyors) are checked port to port: links must line up within 5 mm / 2°, pipe ends must not be left open, pumps must be driven, and the running plant must not raise alarms.",
    })
}

// ---------------------------------------------------------------------------
// Photo alignment

/// Minimum mean IoU for a photo rebuild to pass, and the per-object floor.
const MEAN_IOU: f32 = 0.5;
const MIN_IOU: f32 = 0.25;

fn local_corners(o: &PlacedObject) -> [Vec3; 8] {
    let (a, b) = (o.proto.aabb_min, o.proto.aabb_max);
    let mut c = [Vec3::ZERO; 8];
    let mut i = 0;
    for x in [a.x, b.x] {
        for y in [a.y, b.y] {
            for z in [a.z, b.z] {
                c[i] = Vec3::new(x, y, z);
                i += 1;
            }
        }
    }
    c
}

fn projected(sc: &PhotoScene, local: &[Vec3; 8], t: &Transform) -> Option<[f32; 4]> {
    let w: Vec<Vec3> = local.iter().map(|p| t.transform_point(*p)).collect();
    sc.project(&w)
}

/// Where to move an object so its projection matches the detection.
fn hint(p: [f32; 4], d: [f32; 4]) -> String {
    let mut h = Vec::new();
    let (pcx, dcx) = ((p[0] + p[2]) * 0.5, (d[0] + d[2]) * 0.5);
    let (ph, dh) = (p[3] - p[1], d[3] - d[1]);
    if ph > dh * 1.25 {
        h.push("it looks too large/near - move it farther from the camera or check the asset size");
    } else if ph < dh * 0.8 {
        h.push("it looks too small/far - move it nearer the camera or check the asset size");
    }
    if pcx > dcx + 0.04 {
        h.push("it sits too far right in the photo");
    } else if pcx < dcx - 0.04 {
        h.push("it sits too far left in the photo");
    }
    if p[3] < d[3] - 0.04 {
        h.push("its base is too high/far in the photo");
    } else if p[3] > d[3] + 0.04 {
        h.push("its base is too low/near in the photo");
    }
    h.join("; ")
}

fn photo_alignment(world: &mut World, issues: &mut Vec<String>) -> Vec<Value> {
    let scenes: Vec<PhotoScene> = scenes().lock().unwrap().values().cloned().collect();
    let mut out = Vec::new();
    let mut q = world.query::<(&PlacedObject, &Transform)>();
    for sc in scenes.iter().filter(|s| !s.placed.is_empty()) {
        let mut ious = Vec::new();
        let mut per = Vec::new();
        for (name, &id) in &sc.placed {
            if name.starts_with("#skip") {
                continue;
            }
            let Some((o, t)) = q.iter(world).find(|(o, _)| &o.name == name) else { continue };
            let Some(det) = sc.objects.get(id) else { continue };
            let iou = projected(sc, &local_corners(o), t).map(|p| (geometry::iou(p, det.bbox), p)).unwrap_or((0.0, [0.0; 4]));
            ious.push(iou.0);
            per.push(json!({ "object": name, "detection": id, "iou": (iou.0 * 100.0).round() / 100.0 }));
            if iou.0 < MIN_IOU {
                let h = if iou.0 > 0.0 { hint(iou.1, det.bbox) } else { "it is not visible from the photo camera at all".into() };
                issues.push(format!("{name} doesn't line up with detection #{id} ({}) in {} (IoU {:.2}): {h}", det.label, sc.image_id, iou.0));
            }
        }
        let handled: std::collections::HashSet<usize> = sc.placed.values().copied().collect();
        let unplaced: Vec<usize> = sc.objects.iter().map(|o| o.id).filter(|id| !handled.contains(id)).collect();
        for id in &unplaced {
            issues.push(format!(
                "detection #{id} ({}) in {} has no object - place it with place_from_photo or list it in skip",
                sc.objects[*id].label, sc.image_id
            ));
        }
        let mean = if ious.is_empty() { 0.0 } else { ious.iter().sum::<f32>() / ious.len() as f32 };
        if !ious.is_empty() && mean < MEAN_IOU {
            issues.push(format!("{}: mean photo alignment IoU {:.2} is below {MEAN_IOU} - run optimize_layout or fix the objects listed", sc.image_id, mean));
        }
        out.push(json!({ "image_id": sc.image_id, "mean_iou": (mean * 100.0).round() / 100.0, "objects": per, "unplaced": unplaced }));
    }
    out
}

/// Coordinate descent on x, z, yaw and scale of every object placed from a
/// photo, maximizing its IoU with the detection minus an overlap penalty.
pub fn optimize_photo_layout(world: &mut World, image: &str) -> Result<Value, String> {
    let sc = scenes().lock().unwrap().get(image).cloned().ok_or_else(|| format!("no perceive_photo result for '{image}'"))?;
    struct Item {
        entity: Entity,
        name: String,
        det: [f32; 4],
        local: [Vec3; 8],
        t: Transform,
        open: bool,
        scale0: f32,
    }
    let mut items: Vec<Item> = Vec::new();
    let mut others: Vec<(Entity, Vec3, Vec3, bool)> = Vec::new();
    {
        let mut q = world.query::<(Entity, &PlacedObject, &Transform)>();
        for (e, o, t) in q.iter(world) {
            let (mn, mx) = world_bounds(o, t);
            let asset = o.asset.parent().and_then(|p| p.parent()).and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let open = is_open(&asset);
            others.push((e, mn, mx, open));
            // Stand-ins already sit exactly where the photo camera sees them, and
            // locked objects stay where the user wants them.
            if super::assetgen::is_photo_standin(&o.asset) || o.locked {
                continue;
            }
            if let Some(&id) = sc.placed.get(&o.name)
                && let Some(det) = sc.objects.get(id)
            {
                items.push(Item { entity: e, name: o.name.clone(), det: det.bbox, local: local_corners(o), t: *t, open, scale0: t.scale.x });
            }
        }
    }
    if items.is_empty() {
        if sc.placed.is_empty() {
            return Err("no objects placed from this photo yet".into());
        }
        return Ok(json!({ "image_id": image, "note": "nothing to optimize: photo stand-ins already line up by construction" }));
    }
    let bounds = |local: &[Vec3; 8], t: &Transform| {
        let mut mn = Vec3::splat(f32::MAX);
        let mut mx = Vec3::splat(f32::MIN);
        for p in local {
            let w = t.transform_point(*p);
            mn = mn.min(w);
            mx = mx.max(w);
        }
        (mn, mx)
    };
    let mut before = 0.0;
    let mut after = 0.0;
    for it in &mut items {
        let score = |t: &Transform| -> f32 {
            let iou = projected(&sc, &it.local, t).map(|p| geometry::iou(p, it.det)).unwrap_or(0.0);
            let (mn, mx) = bounds(&it.local, t);
            let mut pen = 0.0;
            if !it.open {
                for (e, omn, omx, oopen) in &others {
                    if *e == it.entity || *oopen {
                        continue;
                    }
                    let ov = (mx.min(*omx) - mn.max(*omn)).max(Vec3::ZERO);
                    let v = ov.element_product();
                    if v > 0.0 {
                        pen += v / (mx - mn).element_product().max(1e-6);
                    }
                }
            }
            iou - 2.0 * pen
        };
        let start = score(&it.t);
        before += projected(&sc, &it.local, &it.t).map(|p| geometry::iou(p, it.det)).unwrap_or(0.0);
        let dist = Vec2::new(it.t.translation.x, it.t.translation.z).length().max(1.0);
        let mut best = it.t;
        let mut best_s = start;
        let mut step = 0.08 * dist;
        let mut ang = 10f32.to_radians();
        for _ in 0..60 {
            let mut improved = false;
            let mut cands = Vec::new();
            for d in [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z] {
                let mut t = best;
                t.translation += d * step;
                cands.push(t);
            }
            for a in [ang, -ang] {
                let mut t = best;
                t.rotate_y(a);
                cands.push(t);
            }
            for k in [1.06f32, 1.0 / 1.06] {
                let s = (best.scale.x * k).clamp(it.scale0 * 0.8, it.scale0 * 1.25);
                let mut t = best;
                t.scale = Vec3::splat(s);
                cands.push(t);
            }
            for t in cands {
                let s = score(&t);
                if s > best_s + 1e-4 {
                    best_s = s;
                    best = t;
                    improved = true;
                }
            }
            if !improved {
                step *= 0.5;
                ang *= 0.5;
                if step < 0.01 {
                    break;
                }
            }
        }
        it.t = best;
        after += projected(&sc, &it.local, &best).map(|p| geometry::iou(p, it.det)).unwrap_or(0.0);
        // Keep the overlap reference current for the next objects.
        if let Some(o) = others.iter_mut().find(|o| o.0 == it.entity) {
            let (mn, mx) = bounds(&it.local, &best);
            o.1 = mn;
            o.2 = mx;
        }
    }
    let n = items.len() as f32;
    let mut moved = Vec::new();
    for it in &items {
        if let Some(mut t) = world.get_mut::<Transform>(it.entity) {
            if (t.translation - it.t.translation).length() > 0.01 || t.rotation.angle_between(it.t.rotation) > 0.01 || (t.scale.x - it.t.scale.x).abs() > 0.01 {
                moved.push(it.name.clone());
            }
            *t = it.t;
        }
    }
    Ok(json!({
        "image_id": image,
        "objects": items.len(),
        "moved": moved,
        "mean_iou_before": ((before / n) * 100.0).round() / 100.0,
        "mean_iou_after": ((after / n) * 100.0).round() / 100.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_structures() {
        assert!(is_open("pallet_rackable_black_a01"));
        assert!(is_open("rack_bulk_storage_a03"));
        assert!(!is_open("box_cardboard_a01"));
    }
}
