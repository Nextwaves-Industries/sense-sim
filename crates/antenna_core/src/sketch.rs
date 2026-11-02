//! Editing a model by shape: what the Studio's drawing tools do, without the screen.
//! Everything here works in a plane's own `(u, v)`, the [`crate::geometry::plane_point`]
//! convention, in metres, and leaves the result in the project as ordinary parts and
//! ports, so what is drawn solves, saves and exports like anything else.

use anyhow::{Context, Result, anyhow, bail};

use crate::cad::{self, CadFeed, P2, signed_area};
use crate::geometry::{Geometry, P3, Shape, plane_point};
use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Param, Project};
use crate::{expr, material};

/// Where a shape goes and what it is made of. `at` and `thick` are expressions, so a
/// layer can follow a variable: `0`, `h`, `1.6mm`.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub axis: usize,
    pub at: String,
    pub material: String,
    pub thick: String,
}

pub fn rectangle(a: P2, b: P2) -> Vec<P2> {
    vec![a, [b[0], a[1]], b, [a[0], b[1]]]
}

/// A circle whose chords stay within `chord` of the true curve.
pub fn circle(c: P2, r: f64, chord: f64) -> Vec<P2> {
    let step = 2.0 * (1.0 - (chord / r.max(1e-12)).min(0.5)).acos();
    let n = ((std::f64::consts::TAU / step.max(1e-3)).ceil() as usize).clamp(12, 96);
    (0..n).map(|k| { let t = std::f64::consts::TAU * k as f64 / n as f64; [c[0] + r * t.cos(), c[1] + r * t.sin()] }).collect()
}

/// A strip `w` wide along a path.
pub fn strip(path: &[P2], w: f64) -> Result<Vec<P2>> {
    if !(w > 0.0) {
        bail!("A strip needs a width above zero.");
    }
    cad::stroke(path, w, false).into_iter().next().ok_or_else(|| anyhow!("A strip needs two different points."))
}

/// The first free `base 1`, `base 2`, …
pub fn free_name(p: &Project, base: &str) -> String {
    let taken = |n: &str| p.extra.iter().any(|e| e.name == n) || p.extra_ports.iter().any(|e| e.name == n);
    (1..).map(|k| format!("{base} {k}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

pub fn height_of(p: &Project, at: &str) -> Result<f64> {
    let vars = p.vars()?;
    expr::eval(at, &vars).with_context(|| format!("the height '{at}'"))
}

fn cell_for(pts: &[P2]) -> f64 {
    let n = cad::narrowest_of(std::slice::from_ref(&pts.to_vec()));
    if n.is_finite() { n / 3.0 } else { 0.0 }
}

/// Adds a closed outline as a new part and returns its name.
pub fn add_shape(p: &mut Project, base: &str, sheet: &Sheet, pts: Vec<P2>) -> Result<String> {
    if pts.len() < 3 {
        bail!("A shape needs at least three different points.");
    }
    if signed_area(&pts).abs() <= 0.0 {
        bail!("That shape has no area.");
    }
    height_of(p, &sheet.at)?;
    let vars = p.vars()?;
    expr::eval(&sheet.thick, &vars).with_context(|| format!("the thickness '{}'", sheet.thick))?;
    if material::by_name(&sheet.material).is_none() {
        bail!("There's no material called {}.", sheet.material);
    }
    let name = free_name(p, base);
    let cell = cell_for(&pts);
    p.extra.push(ExtraSolid { name: name.clone(), material: sheet.material.clone(), prim: ExtraPrim::Outline { file: "drawn".into(), axis: sheet.axis, at: sheet.at.clone(), pts, thick: sheet.thick.clone(), cell } });
    Ok(name)
}

/// Parts the tools can edit, in the project's order: `(index, axis, height expression, outline)`.
pub fn outlines(p: &Project) -> Vec<(usize, usize, &str, &[P2])> {
    p.extra
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.prim {
            ExtraPrim::Outline { axis, at, pts, .. } => Some((i, *axis, at.as_str(), pts.as_slice())),
            _ => None,
        })
        .collect()
}

fn is_metal(name: &str) -> bool {
    material::by_name(name).is_none_or(|m| m.is_conductor())
}

/// Cuts a hole in the metal under it and returns the part it cut.
pub fn cut_in(p: &mut Project, axis: usize, at: f64, hole: &[P2]) -> Result<String> {
    if hole.len() < 3 {
        bail!("A cut needs at least three different points.");
    }
    let vars = p.vars()?;
    let mut found = None;
    for (i, a, h, pts) in outlines(p) {
        if a == axis && expr::eval(h, &vars).is_ok_and(|v| (v - at).abs() < 1e-9) && is_metal(&p.extra[i].material) && hole.iter().all(|q| cad::point_inside(pts, *q)) {
            found = Some(i);
            break;
        }
    }
    let Some(i) = found else {
        bail!("A cut has to sit wholly inside one shape on this plane. A template's parts need making editable first.");
    };
    if let ExtraPrim::Outline { pts, cell, .. } = &mut p.extra[i].prim {
        *pts = cad::cut_hole(pts, hole);
        let finer = cell_for(hole);
        if finer > 0.0 && (*cell <= 0.0 || finer < *cell) {
            *cell = finer;
        }
    }
    Ok(p.extra[i].name.clone())
}

/// The flat metal of a model on one plane, as outlines in the plane's `(u, v)`.
pub fn flat_metal(g: &Geometry, axis: usize, at: f64) -> Vec<Vec<P2>> {
    let mut out = Vec::new();
    for s in g.solids.iter().filter(|s| is_metal(&s.material)) {
        match &s.shape {
            Shape::Box { min, max } if max[axis] - min[axis] <= 0.0 && (min[axis] - at).abs() < 1e-9 => {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                out.push(rectangle([min[u], min[v]], [max[u], max[v]]));
            }
            Shape::Polygon { axis: a, at: h, pts, thick } if *a == axis && *thick <= 0.0 && (*h - at).abs() < 1e-9 => out.push(pts.clone()),
            _ => {}
        }
    }
    out
}

/// A second point pulled onto a line along u or v through the first: ports lie along an axis.
pub fn along_axis(a: P2, b: P2) -> P2 {
    if (b[0] - a[0]).abs() >= (b[1] - a[1]).abs() { [b[0], a[1]] } else { [a[0], b[1]] }
}

fn ensure_z0(p: &mut Project) {
    if !p.params.iter().any(|r| r.name == "Z0") {
        p.params.push(Param::new("Z0", "50", "Port impedance"));
    }
}

fn num3(q: P3) -> [String; 3] {
    [format!("{:e}", q[0]), format!("{:e}", q[1]), format!("{:e}", q[2])]
}

/// Where the metal stops, going from `from` towards `to` along the line through them.
fn metal_edge(all: &[Vec<P2>], from: P2, to: P2) -> Option<P2> {
    let axis = if (to[0] - from[0]).abs() >= (to[1] - from[1]).abs() { 0 } else { 1 };
    let across = 1 - axis;
    let sign = (to[axis] - from[axis]).signum();
    let hit = all
        .iter()
        .flat_map(|o| cad::line_hits(o, axis, from[across]))
        .filter(|h| (h - from[axis]) * sign > 0.0)
        .min_by(|x, y| (x - from[axis]).abs().total_cmp(&(y - from[axis]).abs()))?;
    let mut q = from;
    q[axis] = hit;
    Some(q)
}

/// Feeds across the gap between `a` and `b`, or across a line on unbroken metal. The
/// line is pulled onto an axis and its ends onto the metal. Returns the port's name.
pub fn add_feed(p: &mut Project, axis: usize, at: &str, a: P2, b: P2) -> Result<String> {
    let g = p.scatterer()?;
    let h = height_of(p, at)?;
    let metal = flat_metal(&g, axis, h);
    if metal.is_empty() {
        bail!("There's no flat metal on this plane to feed. Draw it first, or choose the plane it's on.");
    }
    let b = along_axis(a, b);
    if a == b {
        bail!("A feed needs two different points.");
    }
    let on = |q: P2| metal.iter().any(|o| cad::point_inside(o, q));
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let (pa, pb, width) = if on(a) && on(b) && on(mid) {
        // Unbroken metal: the port is a cut across it, as wide as the metal there.
        (a, b, 0.0)
    } else {
        // An end drawn on the metal means the gap starts where that metal stops.
        let start = if on(a) { metal_edge(&metal, a, b).unwrap_or(a) } else { a };
        let end = if on(b) { metal_edge(&metal, b, a).unwrap_or(b) } else { b };
        let layer = cad::sheet_layer("this plane", metal);
        match cad::gap_feed(&[layer], [start, end]).map_err(|e| anyhow!("{}", e.replace("feed line", "feed")))? {
            CadFeed::Gap { a, b, width, .. } => (a, b, width),
            CadFeed::Probe { .. } => bail!("That isn't a gap."),
        }
    };
    ensure_z0(p);
    let name = free_name(p, "Feed");
    let end = |q: P2| {
        let mut s = num3(plane_point(axis, 0.0, q));
        s[axis] = at.to_string();
        s
    };
    p.extra_ports.push(ExtraPort { name: name.clone(), a: end(pa), b: end(pb), width: format!("{width:e}"), z0: "Z0".into() });
    Ok(name)
}

/// A probe at `(u, v)` between the lowest and highest metal over that point, along
/// the plane's axis. Returns the port's name.
pub fn add_probe(p: &mut Project, axis: usize, at: P2) -> Result<String> {
    let g = p.scatterer()?;
    let mut heights: Vec<f64> = Vec::new();
    if axis == 2 && g.ground {
        heights.push(0.0);
    }
    for s in g.solids.iter().filter(|s| is_metal(&s.material)) {
        let here = match &s.shape {
            Shape::Box { min, max } if max[axis] - min[axis] <= 0.0 => {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                (at[0] >= min[u] && at[0] <= max[u] && at[1] >= min[v] && at[1] <= max[v]).then_some(min[axis])
            }
            Shape::Polygon { axis: a, at: h, pts, thick } if *a == axis && *thick <= 0.0 => cad::point_inside(pts, at).then_some(*h),
            _ => None,
        };
        heights.extend(here);
    }
    heights.sort_by(f64::total_cmp);
    heights.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let (Some(lo), Some(hi)) = (heights.first().copied(), heights.last().copied()) else {
        bail!("A probe joins two sheets, and this point has no metal over it.");
    };
    if heights.len() < 2 {
        bail!("A probe joins two sheets, and this point has metal at only one height.");
    }
    ensure_z0(p);
    let name = free_name(p, "Feed");
    p.extra_ports.push(ExtraPort { name: name.clone(), a: num3(plane_point(axis, lo, at)), b: num3(plane_point(axis, hi, at)), width: "0".into(), z0: "Z0".into() });
    Ok(name)
}

/// The nearest editable part under a ray, and the point it hit, in its plane. Where
/// shapes overlap on one plane the smaller wins, so a patch on a ground can be picked.
pub fn pick(p: &Project, origin: P3, dir: P3) -> Option<(usize, P2)> {
    let vars = p.vars().ok()?;
    let mut best: Option<(f64, f64, usize, P2)> = None;
    for (i, axis, at, pts) in outlines(p) {
        let Ok(h) = expr::eval(at, &vars) else { continue };
        if dir[axis].abs() < 1e-6 {
            continue;
        }
        let t = (h - origin[axis]) / dir[axis];
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let q = [origin[u] + dir[u] * t, origin[v] + dir[v] * t];
        if !cad::point_inside(pts, q) {
            continue;
        }
        let area = signed_area(pts).abs();
        let nearer = best.as_ref().is_none_or(|b| t < b.0 - 1e-9 || ((t - b.0).abs() <= 1e-9 && area < b.1));
        if nearer {
            best = Some((t, area, i, q));
        }
    }
    best.map(|b| (b.2, b.3))
}

fn outline_mut(p: &mut Project, part: usize) -> Option<&mut Vec<P2>> {
    match &mut p.extra.get_mut(part)?.prim {
        ExtraPrim::Outline { pts, .. } => Some(pts),
        _ => None,
    }
}

/// Moves the corner at `from`, and any keyhole twin sitting on it, to `to`.
pub fn move_corner(p: &mut Project, part: usize, from: P2, to: P2) {
    if let Some(pts) = outline_mut(p, part) {
        for q in pts.iter_mut().filter(|q| ((q[0] - from[0]).abs() + (q[1] - from[1]).abs()) < 1e-12) {
            *q = to;
        }
    }
}

pub fn move_part(p: &mut Project, part: usize, by: P2) {
    if let Some(pts) = outline_mut(p, part) {
        for q in pts.iter_mut() {
            *q = [q[0] + by[0], q[1] + by[1]];
        }
    }
}

/// Puts a new corner on the edge after corner `edge`.
pub fn insert_corner(p: &mut Project, part: usize, edge: usize, at: P2) {
    if let Some(pts) = outline_mut(p, part)
        && edge < pts.len()
    {
        pts.insert(edge + 1, at);
    }
}

/// Removes a corner, unless it's a keyhole's or too few would be left.
pub fn remove_corner(p: &mut Project, part: usize, at: P2) -> Result<()> {
    let Some(pts) = outline_mut(p, part) else { bail!("This part has no corners to edit.") };
    let same = |q: &P2| ((q[0] - at[0]).abs() + (q[1] - at[1]).abs()) < 1e-12;
    if pts.iter().filter(|q| same(q)).count() > 1 {
        bail!("This corner joins a hole to its outline, so it has to stay.");
    }
    if pts.len() <= 3 {
        bail!("A shape needs at least three corners.");
    }
    pts.retain(|q| !same(q));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::C64;
    use crate::mom::{MomOptions, MomProblem};

    fn sheet(axis: usize, at: &str) -> Sheet {
        Sheet { axis, at: at.into(), material: "PEC".into(), thick: "0".into() }
    }

    fn solve_z(p: &Project, f: f64, edges: f64) -> C64 {
        let g = p.geometry().unwrap();
        let opts = MomOptions { edges_per_lambda: edges, ..Default::default() };
        MomProblem::new(&g, f, opts).unwrap().solve(f, false).unwrap().z[0][0]
    }

    fn set(p: &mut Project, name: &str, expr: &str) {
        match p.params.iter_mut().find(|r| r.name == name) {
            Some(r) => r.expr = expr.into(),
            None => p.params.push(Param::new(name, expr, "")),
        }
    }

    /// Drawn the way a person would: two rectangles, then a feed clicked between them.
    #[test]
    fn a_dipole_drawn_by_hand_solves_like_the_template() {
        let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let v = t.vars().unwrap();
        let (l, w, g) = (v["L"], v["w"], v["g"]);
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        set(&mut p, "Z0", "73");
        // On the plane y = 0, whose (u, v) is (z, x).
        let s = sheet(1, "0");
        add_shape(&mut p, "Arm", &s, rectangle([g / 2.0, -w / 2.0], [l / 2.0, w / 2.0])).unwrap();
        add_shape(&mut p, "Arm", &s, rectangle([-l / 2.0, -w / 2.0], [-g / 2.0, w / 2.0])).unwrap();
        // Clicked from one arm to the other, a little off-axis: the feed finds the gap.
        let name = add_feed(&mut p, 1, "0", [-g * 2.0, 0.0], [g * 2.0, 0.0005]).unwrap();
        assert_eq!(name, "Feed 1");
        let port = &p.geometry().unwrap().ports[0];
        assert!((port.a[2] + g / 2.0).abs() < 1e-9 && (port.b[2] - g / 2.0).abs() < 1e-9, "{port:?}");
        assert!((port.width - w).abs() < 1e-6, "{port:?}");
        let (zt, zd) = (solve_z(&t, 1e9, 14.0), solve_z(&p, 1e9, 14.0));
        println!("template {zt:.2} Ω, drawn {zd:.2} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.02, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn a_slot_cut_by_hand_solves_like_the_template() {
        let t = Project::from_template("slot", Some("2.45GHz")).unwrap();
        let v = t.vars().unwrap();
        let (gx, gy, ls, ws) = (v["Gx"], v["Gy"], v["Ls"], v["ws"]);
        let mut p = Project::from_template("custom", Some("2.45GHz")).unwrap();
        set(&mut p, "Z0", "500");
        let s = sheet(2, "0");
        add_shape(&mut p, "Plate", &s, rectangle([-gx / 2.0, -gy / 2.0], [gx / 2.0, gy / 2.0])).unwrap();
        let cut = cut_in(&mut p, 2, 0.0, &rectangle([-ws / 2.0, -ls / 2.0], [ws / 2.0, ls / 2.0])).unwrap();
        assert_eq!(cut, "Plate 1");
        // Clicked across the slot from metal to metal.
        add_feed(&mut p, 2, "0", [-ws, 0.0], [ws, 0.0]).unwrap();
        let wp = v["wp"];
        p.extra_ports[0].width = format!("{wp:e}");
        let edges = t.setup.edges_per_lambda;
        let (zt, zd) = (solve_z(&t, 2.45e9, edges), solve_z(&p, 2.45e9, edges));
        println!("template {zt:.1} Ω, drawn {zd:.1} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.08, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn a_patch_with_a_clicked_probe_solves_like_the_template() {
        let t = Project::from_template("patch_air", Some("2.45GHz")).unwrap();
        let g = t.geometry().unwrap();
        let tp = &g.ports[0];
        let mut p = Project::from_template("custom", Some("2.45GHz")).unwrap();
        let z0 = g.ports[0].z0;
        set(&mut p, "Z0", &format!("{z0}"));
        set(&mut p, "h", &format!("{:e}", tp.b[2]));
        for (name, s) in g.solids.iter().map(|s| (s.name.clone(), s)) {
            let Shape::Box { min, max } = &s.shape else { continue };
            let at = if min[2] > 0.0 { "h" } else { "0" };
            add_shape(&mut p, &name, &sheet(2, at), rectangle([min[0], min[1]], [max[0], max[1]])).unwrap();
        }
        assert_eq!(p.extra.len(), g.solids.len());
        add_probe(&mut p, 2, [tp.a[0], tp.a[1]]).unwrap();
        p.extra_ports[0].width = format!("{:e}", tp.width);
        let (zt, zd) = (solve_z(&t, 2.45e9, 12.0), solve_z(&p, 2.45e9, 12.0));
        println!("template {zt:.1} Ω, drawn {zd:.1} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.05, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn feeds_and_probes_say_what_is_wrong() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        assert!(add_feed(&mut p, 2, "0", [0.0, 0.0], [0.01, 0.0]).is_err(), "nothing drawn yet");
        add_shape(&mut p, "Plate", &sheet(2, "0"), rectangle([0.0, 0.0], [0.1, 0.1])).unwrap();
        let e = add_probe(&mut p, 2, [0.05, 0.05]).unwrap_err().to_string();
        assert!(e.contains("only one height"), "{e}");
        let e = add_probe(&mut p, 2, [0.5, 0.5]).unwrap_err().to_string();
        assert!(e.contains("no metal"), "{e}");
        let e = add_feed(&mut p, 2, "0", [0.3, 0.3], [0.4, 0.3]).unwrap_err().to_string();
        assert!(e.contains("metal"), "{e}");
        // On unbroken metal the port is a line across it.
        add_feed(&mut p, 2, "0", [0.05, 0.04], [0.05, 0.06]).unwrap();
        assert_eq!(p.geometry().unwrap().ports.len(), 1);
        let opts = MomOptions { edges_per_lambda: 12.0, ..Default::default() };
        assert!(MomProblem::new(&p.geometry().unwrap(), 1e9, opts).is_ok());
    }

    #[test]
    fn a_strip_meander_is_one_piece_with_the_right_area() {
        let path = [[0.0, 0.0], [0.02, 0.0], [0.02, 0.01], [0.0, 0.01], [0.0, 0.02], [0.02, 0.02]];
        let pts = strip(&path, 0.002).unwrap();
        // Length of the centre line times the width.
        let want = (0.02 + 0.01 + 0.02 + 0.01 + 0.02) * 0.002;
        assert!((signed_area(&pts).abs() - want).abs() / want < 1e-6, "{}", signed_area(&pts));
        assert!(strip(&path, 0.0).is_err());
        assert!(strip(&[[0.0, 0.0]], 0.002).is_err());
    }

    #[test]
    fn corners_move_with_their_keyhole_twins_and_stay_valid() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut p, "Plate", &sheet(2, "0"), rectangle([0.0, 0.0], [0.1, 0.1])).unwrap();
        cut_in(&mut p, 2, 0.0, &rectangle([0.04, 0.04], [0.06, 0.06])).unwrap();
        let pts = outlines(&p)[0].3.to_vec();
        let twin = *pts.iter().find(|q| pts.iter().filter(|r| r == q).count() == 2).unwrap();
        let before = signed_area(&pts).abs();
        assert!(remove_corner(&mut p, 0, twin).is_err(), "a keyhole's corner stays");
        move_corner(&mut p, 0, twin, [twin[0] + 0.001, twin[1]]);
        let moved = outlines(&p)[0].3.to_vec();
        assert_eq!(moved.iter().filter(|q| **q == [twin[0] + 0.001, twin[1]]).count(), 2);
        assert!((signed_area(&moved).abs() - before).abs() < 2e-4);
        move_part(&mut p, 0, [0.01, 0.02]);
        let (lo, _) = p.scatterer().unwrap().bounds();
        assert!((lo[0] - 0.01).abs() < 1e-9 && (lo[1] - 0.02).abs() < 1e-9, "{lo:?}");
        insert_corner(&mut p, 0, 0, [0.05, 0.015]);
        assert_eq!(outlines(&p)[0].3.len(), moved.len() + 1);
        let mut q = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut q, "T", &sheet(2, "0"), vec![[0.0, 0.0], [0.1, 0.0], [0.0, 0.1]]).unwrap();
        assert!(remove_corner(&mut q, 0, [0.0, 0.0]).is_err(), "three corners is the least");
    }

    #[test]
    fn picking_takes_the_nearest_and_then_the_smaller() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        add_shape(&mut p, "Ground", &sheet(2, "0"), rectangle([-0.1, -0.1], [0.1, 0.1])).unwrap();
        add_shape(&mut p, "On it", &sheet(2, "0"), rectangle([-0.01, -0.01], [0.01, 0.01])).unwrap();
        add_shape(&mut p, "Above", &sheet(2, "0.02"), rectangle([0.03, 0.03], [0.05, 0.05])).unwrap();
        let down = [0.0, 0.0, -1.0];
        assert_eq!(pick(&p, [0.0, 0.0, 1.0], down).unwrap().0, 1, "smaller on one plane");
        assert_eq!(pick(&p, [0.04, 0.04, 1.0], down).unwrap().0, 2, "nearer the eye");
        assert_eq!(pick(&p, [-0.05, 0.05, 1.0], down).unwrap().0, 0);
        assert!(pick(&p, [0.5, 0.5, 1.0], down).is_none());
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        assert!(add_shape(&mut p, "x", &sheet(2, "nope"), rectangle([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(add_shape(&mut p, "x", &sheet(2, "0"), vec![[0.0, 0.0], [1.0, 1.0]]).is_err());
        assert!(add_shape(&mut p, "x", &sheet(2, "0"), vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]).is_err(), "no area");
        let mut wrong = sheet(2, "0");
        wrong.material = "Kryptonite".into();
        assert!(add_shape(&mut p, "x", &wrong, rectangle([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(cut_in(&mut p, 2, 0.0, &rectangle([0.0, 0.0], [1.0, 1.0])).is_err());
        assert!(p.extra.is_empty());
    }

    #[test]
    fn a_circle_follows_its_chord_limit() {
        let c = circle([0.0, 0.0], 0.05, 50e-6);
        assert!(c.len() >= 12 && c.len() <= 96);
        let exact = std::f64::consts::PI * 0.05 * 0.05;
        assert!((signed_area(&c).abs() - exact).abs() / exact < 0.01, "{} points: {}", c.len(), signed_area(&c));
        assert!(circle([0.0, 0.0], 0.05, 1e-3).len() < c.len());
    }
}
