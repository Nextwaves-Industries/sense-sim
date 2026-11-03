//! UHF tag antennas that aren't the T-match dipole: different structures, each with its own way of
//! making the inductance a chip needs and its own trade between size, bandwidth, gain and how much it
//! minds what's stuck to it. All flat metal in one plane, the chip in a gap, so each solves in layers
//! (see `mom::layered`) and prints on one side of a film. Coplanar pieces merge when meshed.

use anyhow::{Result, bail};

use super::{Ctx, base, hint, p, poly, port};
use crate::cad;
use crate::geometry::{Geometry, Solid};
use crate::project::Param;

type P2 = [f64; 2];

/// An outline rather than a flat box: the mesher takes a box this thin for a line.
fn rect(name: &str, mat: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> Solid {
    poly(name, mat, 2, 0.0, vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]])
}

/// A trace `w` wide along a path, one piece.
fn trace(name: &str, mat: &str, path: &[P2], w: f64) -> Result<Solid> {
    let mut rings = cad::stroke(path, w, false);
    let Some(ring) = rings.pop() else { bail!("'{name}' needs a path of two different points.") };
    Ok(poly(name, mat, 2, 0.0, ring))
}

fn mirrored_x(path: &[P2]) -> Vec<P2> {
    path.iter().map(|q| [-q[0], q[1]]).collect()
}

/// The feed: a loop of width `a` and height `b` below `y = 0`, with the chip in a gap at the middle of
/// its bottom side. Its inductance is what the chip's capacitance resonates with.
fn feed_loop(c: &Ctx, y_top: f64, a: f64, b: f64, g: f64, wl: f64) -> Result<Solid> {
    if !(g > 0.0 && g < a - 2.0 * wl && b > 2.0 * wl) {
        bail!("The feed loop needs room: gap and trace have to fit inside its width and height.");
    }
    let y = y_top - b;
    trace("feed loop", c.metal, &[[g / 2.0, y], [a / 2.0, y], [a / 2.0, y_top], [-a / 2.0, y_top], [-a / 2.0, y], [-g / 2.0, y]], wl)
}

/// Half a meandered arm from `x_start` out to `x_end` along `y`, right-angled, with `n` periods of
/// height `hm` either side of the line, in steps of `s`.
fn meander_arm(x_start: f64, x_end: f64, y: f64, n: usize, hm: f64, s: f64) -> Result<Vec<P2>> {
    let mut path = vec![[x_start, y]];
    let mut x = x_start;
    for _ in 0..n {
        path.push([x, y + hm]);
        x += s;
        path.push([x, y + hm]);
        path.push([x, y - hm]);
        x += s;
        path.push([x, y - hm]);
        path.push([x, y]);
    }
    if x >= x_end {
        bail!("The meanders don't fit in this length: shorten them or lengthen Lx.");
    }
    path.push([x_end, y]);
    Ok(path)
}

fn tag_port(y: f64, g: f64, wl: f64, z0: f64) -> crate::geometry::PortDef {
    port("chip", [-g / 2.0, y, 0.0], [g / 2.0, y, 0.0], wl, z0)
}

fn z0_row() -> Param {
    p("Z0", "50", "Port impedance. It doesn't matter here: a tag is judged against its chip")
}

// ---------------------------------------------------------------------------
// A loop coupled to a radiator with no metal between them

pub(super) fn loop_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Lx", "0.29*lambda0", "Radiator, tip to tip"),
        p("w", "lambda0/330", "Radiator trace width"),
        p("a", "0.055*lambda0", "Feed loop width: with its height, the inductance the chip needs"),
        p("b", "0.03*lambda0", "Feed loop height"),
        p("wl", "lambda0/330", "Feed loop trace width"),
        p("gc", "0.006*lambda0", "From the loop's line to the radiator's: nearer couples more, which raises the resistance the chip sees"),
        p("g", "lambda0/330", "Chip gap"),
        p("n", "2", "Meander periods on each side"),
        p("Hm", "0.012*lambda0", "Meander height either side of the radiator's line"),
        p("s", "0.01*lambda0", "Meander step"),
        p("x0", "0.05*lambda0", "Where the meander starts, from the centre"),
        p("Lp", "0.02*lambda0", "End plate length, 0 for none: capacitive loading shortens the radiator"),
        p("Hp", "0.03*lambda0", "End plate height"),
        z0_row(),
    ]);
    v
}

/// Resistance and reactance tune nearly apart: the loop sets the reactance, its distance from the
/// radiator the resistance. The structure behind many commercial inlays.
pub(super) fn loop_coupled(c: &Ctx) -> Result<Geometry> {
    let (lx, w, a, b, wl, gc, g, hm, s, x0, lp, hp, z0) = (c.get("Lx")?, c.get("w")?, c.get("a")?, c.get("b")?, c.get("wl")?, c.get("gc")?, c.get("g")?, c.get("Hm")?, c.get("s")?, c.get("x0")?, c.get("Lp")?, c.get("Hp")?, c.get("Z0")?);
    let n = c.count("n", 0, 12)?;
    if !(gc > (w + wl) / 2.0) {
        bail!("The radiator has to clear the loop: gc must exceed half the two traces' widths.");
    }
    if n > 0 && !(x0 > a / 2.0 + w && hm > w && s > 2.0 * w) {
        bail!("The meander has to start outside the loop, with its steps wider than two traces.");
    }
    let y = gc;
    let tip = lx / 2.0 - lp.max(0.0);
    let arm = if n > 0 {
        let start = [0.0, y];
        let mut path = vec![start];
        path.extend(meander_arm(x0, tip, y, n, hm, s)?);
        path
    } else {
        vec![[0.0, y], [tip, y]]
    };
    let mut solids = vec![feed_loop(c, 0.0, a, b, g, wl)?, trace("radiator +", c.metal, &arm, w)?, trace("radiator -", c.metal, &mirrored_x(&arm), w)?];
    if lp > 0.0 && hp > w {
        solids.push(rect("end plate +", c.metal, tip, y - hp / 2.0, tip + lp, y + hp / 2.0));
        solids.push(rect("end plate -", c.metal, -tip - lp, y - hp / 2.0, -tip, y + hp / 2.0));
    }
    Ok(Geometry { solids, ports: vec![tag_port(-b, g, wl, z0)], hints: vec![hint([-lx / 2.0, -b - wl, 0.0], [lx / 2.0, y + hm.max(hp / 2.0) + w, 0.0], w.min(wl) * 4.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// One loop, two radiators of different lengths

pub(super) fn dual_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("L1", "0.33*lambda0", "Long radiator: the low end of the band"),
        p("L2", "0.27*lambda0", "Short radiator: the high end of the band"),
        p("w", "lambda0/330", "Radiator trace width"),
        p("a", "0.055*lambda0", "Feed loop width"),
        p("b", "0.03*lambda0", "Feed loop height"),
        p("wl", "lambda0/330", "Feed loop trace width"),
        p("g1", "0.006*lambda0", "From the loop's top line to the long radiator's"),
        p("g2", "0.006*lambda0", "From the loop's bottom line to the short radiator's"),
        p("g", "lambda0/330", "Chip gap"),
        p("t1", "0.02*lambda0", "Folded tips of the long radiator, away from the loop"),
        p("t2", "0.02*lambda0", "Folded tips of the short radiator, away from the loop"),
        p("n1", "1", "Meander periods on each side of the long radiator, for a label shorter than half a wave"),
        p("n2", "1", "Meander periods on each side of the short radiator"),
        p("Hm", "0.012*lambda0", "Meander height either side of a radiator's line"),
        p("s", "0.01*lambda0", "Meander step"),
        p("x0", "0.05*lambda0", "Where the meanders start, from the centre"),
        z0_row(),
    ]);
    v
}

/// Two couplings to one loop make two resonances, so a tag can hold both 866 and 915 MHz with one chip.
pub(super) fn dual_radiator(c: &Ctx) -> Result<Geometry> {
    let (l1, l2, w, a, b, wl, g1, g2, g, t1, t2, hm, s, x0, z0) = (c.get("L1")?, c.get("L2")?, c.get("w")?, c.get("a")?, c.get("b")?, c.get("wl")?, c.get("g1")?, c.get("g2")?, c.get("g")?, c.get("t1")?, c.get("t2")?, c.get("Hm")?, c.get("s")?, c.get("x0")?, c.get("Z0")?);
    let (n1, n2) = (c.count("n1", 0, 12)?, c.count("n2", 0, 12)?);
    if !(g1 > (w + wl) / 2.0 && g2 > (w + wl) / 2.0) {
        bail!("Both radiators have to clear the loop.");
    }
    if n1 + n2 > 0 && !(x0 > a / 2.0 + w && hm > w && s > 2.0 * w) {
        bail!("Meanders have to start outside the loop, with steps wider than two traces.");
    }
    let dip = |n: usize| if n > 0 { hm } else { 0.0 };
    if !(g1 + b + g2 > dip(n1) + dip(n2) + w) {
        bail!("The two radiators' meanders run into each other: move them apart or lower Hm.");
    }
    // The chip sits at the middle of the loop's bottom, between the loop and the short radiator.
    let (y1, y2) = (g1, -b - g2);
    let arm = |y: f64, l: f64, t: f64, sign: f64, n: usize| -> Result<Vec<P2>> {
        let mut path = vec![[0.0, y]];
        if n > 0 {
            path.extend(meander_arm(x0, l / 2.0, y, n, hm, s)?);
        } else {
            path.push([l / 2.0, y]);
        }
        if t > 2.0 * w {
            path.push([l / 2.0, y + sign * t]);
        }
        Ok(path)
    };
    let (long, short) = (arm(y1, l1, t1, 1.0, n1)?, arm(y2, l2, t2, -1.0, n2)?);
    let solids = vec![feed_loop(c, 0.0, a, b, g, wl)?, trace("long radiator +", c.metal, &long, w)?, trace("long radiator -", c.metal, &mirrored_x(&long), w)?, trace("short radiator +", c.metal, &short, w)?, trace("short radiator -", c.metal, &mirrored_x(&short), w)?];
    let half = l1.max(l2) / 2.0;
    Ok(Geometry { solids, ports: vec![tag_port(-b, g, wl, z0)], hints: vec![hint([-half, y2 - t2.max(hm).max(0.0) - w, 0.0], [half, y1 + t1.max(hm).max(0.0) + w, 0.0], w.min(wl) * 4.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// Wide arms ending in plates

pub(super) fn dogbone_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Lx", "0.27*lambda0", "Overall length"),
        p("wa", "0.02*lambda0", "Arm width: wider lowers the Q"),
        p("Lp", "0.05*lambda0", "End plate length"),
        p("Hp", "0.07*lambda0", "End plate height: plates carry charge, so the arms can be shorter"),
        p("a", "0.05*lambda0", "T-match width"),
        p("b", "0.025*lambda0", "T-match height"),
        p("wt", "lambda0/330", "T-match trace width"),
        p("g", "lambda0/330", "Chip gap"),
        z0_row(),
    ]);
    v
}

/// A fat dipole with heavy capacitive tips: a lot of aperture and a low Q for its length.
pub(super) fn dogbone(c: &Ctx) -> Result<Geometry> {
    let (lx, wa, lp, hp, a, b, wt, g, z0) = (c.get("Lx")?, c.get("wa")?, c.get("Lp")?, c.get("Hp")?, c.get("a")?, c.get("b")?, c.get("wt")?, c.get("g")?, c.get("Z0")?);
    if !(g < a - 2.0 * wt && b > wa / 2.0 + wt && lx > a + 2.0 * lp && hp > wa) {
        bail!("The T-match and plates need room: check a, b, Lp and Hp against Lx and the arm width.");
    }
    let tip = lx / 2.0 - lp;
    let t_right = [[g / 2.0, b], [a / 2.0, b], [a / 2.0, wa / 2.0 - wt / 2.0]];
    let solids = vec![
        rect("arms", c.metal, -tip, -wa / 2.0, tip, wa / 2.0),
        rect("end plate +", c.metal, tip, -hp / 2.0, tip + lp, hp / 2.0),
        rect("end plate -", c.metal, -tip - lp, -hp / 2.0, -tip, hp / 2.0),
        trace("T-match +", c.metal, &t_right, wt)?,
        trace("T-match -", c.metal, &mirrored_x(&t_right), wt)?,
    ];
    Ok(Geometry { solids, ports: vec![tag_port(b, g, wt, z0)], hints: vec![hint([-lx / 2.0, -hp / 2.0, 0.0], [lx / 2.0, hp.max(2.0 * b) / 2.0 + wt, 0.0], wt * 4.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// A bow-tie with a shunt hairpin

pub(super) fn bowtie_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Lx", "0.3*lambda0", "Tip to tip"),
        p("Hb", "0.07*lambda0", "Height at the wide end, either side of the axis"),
        p("wn", "0.006*lambda0", "Height at the neck, either side of the axis"),
        p("x0", "0.03*lambda0", "Where the flare starts, from the centre"),
        p("xs", "0.025*lambda0", "Where the hairpin joins the feed line, from the centre"),
        p("hs", "0.03*lambda0", "Hairpin height: sets the inductance"),
        p("wt", "lambda0/330", "Feed line and hairpin trace width"),
        p("g", "lambda0/330", "Chip gap"),
        z0_row(),
    ]);
    v
}

/// The chip across a gap in a flat flared dipole, with an inductive hairpin in parallel. Low Q and the
/// widest band here; it asks for the most area.
pub(super) fn bowtie_tag(c: &Ctx) -> Result<Geometry> {
    let (lx, hb, wn, x0, xs, hs, wt, g, z0) = (c.get("Lx")?, c.get("Hb")?, c.get("wn")?, c.get("x0")?, c.get("xs")?, c.get("hs")?, c.get("wt")?, c.get("g")?, c.get("Z0")?);
    if !(g / 2.0 + wt < xs && xs <= x0 && x0 < lx / 2.0 && wn > wt / 2.0 && hb >= wn && hs > wt) {
        bail!("The feed line, hairpin and flare overlap: check xs, x0, hs against the trace width.");
    }
    let flare = |sign: f64| vec![[sign * x0, -wn], [sign * lx / 2.0, -hb], [sign * lx / 2.0, hb], [sign * x0, wn]];
    let solids = vec![
        poly("flare +", c.metal, 2, 0.0, flare(1.0)),
        poly("flare -", c.metal, 2, 0.0, flare(-1.0)),
        rect("feed +", c.metal, g / 2.0, -wt / 2.0, x0 + wt, wt / 2.0),
        rect("feed -", c.metal, -x0 - wt, -wt / 2.0, -g / 2.0, wt / 2.0),
        trace("hairpin", c.metal, &[[xs, 0.0], [xs, hs], [-xs, hs], [-xs, 0.0]], wt)?,
    ];
    Ok(Geometry { solids, ports: vec![tag_port(0.0, g, wt, z0)], hints: vec![hint([-lx / 2.0, -hb, 0.0], [lx / 2.0, hb.max(hs + wt), 0.0], wt * 4.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// A plate with an inductive slot

pub(super) fn slot_plate_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Lx", "0.3*lambda0", "Plate length"),
        p("Ly", "0.08*lambda0", "Plate height"),
        p("a", "0.06*lambda0", "Slot width: with its height, the inductance"),
        p("b", "0.04*lambda0", "Slot height"),
        p("g", "lambda0/330", "Slit width, where the chip sits"),
        p("wp", "lambda0/330", "Width of metal the chip bonds across"),
        p("ys", "0.005*lambda0", "Chip from the plate's edge"),
        z0_row(),
    ]);
    v
}

/// No traces at all: a plate, a window cut in it, and a slit from the window to the edge with the chip
/// across it. Solid metal keeps loss low, and with a spacer it works over a metal surface.
pub(super) fn slot_plate(c: &Ctx) -> Result<Geometry> {
    let (lx, ly, a, b, g, wp, ys, z0) = (c.get("Lx")?, c.get("Ly")?, c.get("a")?, c.get("b")?, c.get("g")?, c.get("wp")?, c.get("ys")?, c.get("Z0")?);
    let (edge, slot_lo, slot_hi) = (-ly / 2.0, -b / 2.0, b / 2.0);
    if !(a > g && a < lx && b < ly && edge + ys < slot_lo && ys > 0.0) {
        bail!("The slot has to fit in the plate, with the chip between the slot and the edge.");
    }
    let (half, ha, hg) = (lx / 2.0, a / 2.0, g / 2.0);
    let solids = vec![
        rect("plate top", c.metal, -half, slot_hi, half, ly / 2.0),
        rect("plate left", c.metal, -half, slot_lo, -ha, slot_hi),
        rect("plate right", c.metal, ha, slot_lo, half, slot_hi),
        rect("plate bottom left", c.metal, -half, edge, -hg, slot_lo),
        rect("plate bottom right", c.metal, hg, edge, half, slot_lo),
    ];
    let y = edge + ys;
    Ok(Geometry { solids, ports: vec![port("chip", [-hg, y, 0.0], [hg, y, 0.0], wp, z0)], hints: vec![hint([-ha, edge, 0.0], [ha, slot_hi, 0.0], g.max(wp))], ..Default::default() })
}

// ---------------------------------------------------------------------------
// A two-arm square spiral

pub(super) fn spiral_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("r0", "0.03*lambda0", "Half-size of the first turn"),
        p("pitch", "0.012*lambda0", "Spacing from one arm to the other"),
        p("turns", "7", "Sides of the spiral on each arm"),
        p("last", "0.8", "How much of the last side, 0 to 1"),
        p("xs", "0.015*lambda0", "Where the hairpin joins the feed, from the centre"),
        p("hs", "0.018*lambda0", "Hairpin height: sets the inductance"),
        p("w", "lambda0/330", "Trace width"),
        p("g", "lambda0/330", "Chip gap"),
        z0_row(),
    ]);
    v
}

/// One arm, starting from the feed: out along +x, then round anticlockwise. Side `k` runs on a line
/// `r0 + (k/2)·pitch` from the centre, so the other arm, this one's mirror through the centre, falls
/// exactly between its turns.
fn spiral_arm(g: f64, r0: f64, pitch: f64, sides: usize, last: f64) -> Vec<P2> {
    let line = |k: usize| {
        let at = r0 + (k / 2) as f64 * pitch;
        if k % 4 < 2 { at } else { -at }
    };
    let mut path = vec![[g / 2.0, 0.0], [r0, 0.0]];
    let mut at = [r0, 0.0];
    for k in 0..sides {
        // Odd sides run along x until the next line, even ones along y.
        let mut to = if k % 2 == 0 { [at[0], line(k + 1)] } else { [line(k + 1), at[1]] };
        if k + 1 == sides {
            let t = last.clamp(0.05, 1.0);
            to = [at[0] + (to[0] - at[0]) * t, at[1] + (to[1] - at[1]) * t];
        }
        path.push(to);
        at = to;
    }
    path
}

/// Folds a long radiator into a square, for a label with no length to spare. Gain and bandwidth pay
/// for it, and the pattern is rounder than a dipole's.
pub(super) fn spiral(c: &Ctx) -> Result<Geometry> {
    let (r0, pitch, last, xs, hs, w, g, z0) = (c.get("r0")?, c.get("pitch")?, c.get("last")?, c.get("xs")?, c.get("hs")?, c.get("w")?, c.get("g")?, c.get("Z0")?);
    let sides = c.count("turns", 1, 24)?;
    if !(pitch > 2.0 * w && r0 > hs + w && r0 > xs + w && xs > g / 2.0 + w && hs > w) {
        bail!("The spiral's pitch has to clear two traces, and the hairpin has to fit in the first turn.");
    }
    let arm = spiral_arm(g, r0, pitch, sides, last);
    let other: Vec<P2> = arm.iter().map(|q| [-q[0], -q[1]]).collect();
    let solids = vec![trace("arm +", c.metal, &arm, w)?, trace("arm -", c.metal, &other, w)?, trace("hairpin", c.metal, &[[xs, 0.0], [xs, hs], [-xs, hs], [-xs, 0.0]], w)?];
    let reach = arm.iter().fold(0.0f64, |m, q| m.max(q[0].abs()).max(q[1].abs())) + w;
    Ok(Geometry { solids, ports: vec![tag_port(0.0, g, w, z0)], hints: vec![hint([-reach, -reach, 0.0], [reach, reach, 0.0], w * 4.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// Arms filled by a space-filling curve

pub(super) fn fractal_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Ax", "0.1*lambda0", "Width of the block each arm fills"),
        p("Ay", "0.07*lambda0", "Height of the block each arm fills"),
        p("order", "2", "Order of the Hilbert curve: 1 to 3, each four times the wire"),
        p("x0", "0.03*lambda0", "Where the block starts, from the centre"),
        p("a", "0.04*lambda0", "T-match width"),
        p("b", "0.03*lambda0", "T-match height"),
        p("w", "lambda0/330", "Trace width"),
        p("g", "lambda0/330", "Chip gap"),
        z0_row(),
    ]);
    v
}

/// A Hilbert curve of this order on the unit square, starting at the middle of its left side's foot
/// and ending at the foot of the right side.
fn hilbert(order: usize) -> Vec<P2> {
    fn step(x0: f64, y0: f64, xi: f64, xj: f64, yi: f64, yj: f64, n: usize, out: &mut Vec<P2>) {
        if n == 0 {
            out.push([x0 + (xi + yi) / 2.0, y0 + (xj + yj) / 2.0]);
            return;
        }
        step(x0, y0, yi / 2.0, yj / 2.0, xi / 2.0, xj / 2.0, n - 1, out);
        step(x0 + xi / 2.0, y0 + xj / 2.0, xi / 2.0, xj / 2.0, yi / 2.0, yj / 2.0, n - 1, out);
        step(x0 + xi / 2.0 + yi / 2.0, y0 + xj / 2.0 + yj / 2.0, xi / 2.0, xj / 2.0, yi / 2.0, yj / 2.0, n - 1, out);
        step(x0 + xi / 2.0 + yi, y0 + xj / 2.0 + yj, -yi / 2.0, -yj / 2.0, -xi / 2.0, -xj / 2.0, n - 1, out);
    }
    let mut out = Vec::new();
    step(0.0, 0.0, 1.0, 0.0, 0.0, 1.0, order, &mut out);
    out
}

/// A lot of wire in a small block: compact, with self-similar currents. Trace loss rises with order.
pub(super) fn fractal(c: &Ctx) -> Result<Geometry> {
    let (ax, ay, x0, a, b, w, g, z0) = (c.get("Ax")?, c.get("Ay")?, c.get("x0")?, c.get("a")?, c.get("b")?, c.get("w")?, c.get("g")?, c.get("Z0")?);
    let order = c.count("order", 1, 3)?;
    let cells = (1usize << order) as f64;
    if !(ax / cells > 2.0 * w && ay / cells > 2.0 * w) {
        bail!("At this order the curve's lines are closer than two traces: enlarge the block or thin the trace.");
    }
    if !(x0 > a / 2.0 + w && g < a - 2.0 * w && b > 2.0 * w) {
        bail!("The T-match has to fit between the arms, with room for the gap.");
    }
    // The curve's first point is on the axis, where the line from the T-match reaches it, and the
    // block fills upward from there.
    let foot = ay / cells / 2.0;
    let mut arm: Vec<P2> = vec![[0.0, 0.0]];
    arm.extend(hilbert(order).iter().map(|q| [x0 + q[0] * ax, q[1] * ay - foot]));
    let t_right = [[g / 2.0, b], [a / 2.0, b], [a / 2.0, 0.0]];
    let solids = vec![trace("arm +", c.metal, &arm, w)?, trace("arm -", c.metal, &mirrored_x(&arm), w)?, trace("T-match +", c.metal, &t_right, w)?, trace("T-match -", c.metal, &mirrored_x(&t_right), w)?];
    let reach = x0 + ax;
    Ok(Geometry { solids, ports: vec![tag_port(b, g, w, z0)], hints: vec![hint([-reach, -foot - w, 0.0], [reach, ay.max(b) + w, 0.0], w * 4.0)], ..Default::default() })
}

// ---------------------------------------------------------------------------
// Arms turned at right angles, for less of a null

pub(super) fn bent_params() -> Vec<Param> {
    let mut v = base("915MHz");
    v.extend([
        p("Xa", "0.1*lambda0", "Each arm's run along x, from the centre"),
        p("Ya", "0.06*lambda0", "Each arm's run along y, up on one side and down on the other"),
        p("w", "lambda0/200", "Trace width"),
        p("a", "0.04*lambda0", "T-match width"),
        p("b", "0.03*lambda0", "T-match height"),
        p("wt", "lambda0/330", "T-match trace width"),
        p("g", "lambda0/330", "Chip gap"),
        z0_row(),
    ]);
    v
}

/// Current along both x and y fills in the deep nulls off a straight dipole's ends, so the tag reads at
/// more orientations for a little peak gain.
pub(super) fn bent_arms(c: &Ctx) -> Result<Geometry> {
    let (xa, ya, w, a, b, wt, g, z0) = (c.get("Xa")?, c.get("Ya")?, c.get("w")?, c.get("a")?, c.get("b")?, c.get("wt")?, c.get("g")?, c.get("Z0")?);
    if !(xa > a / 2.0 + w && ya > w && g < a - 2.0 * wt && b > 2.0 * wt) {
        bail!("The arms have to run past the T-match, with room for the gap.");
    }
    let arm = [[0.0, 0.0], [xa, 0.0], [xa, ya]];
    let other = [[0.0, 0.0], [-xa, 0.0], [-xa, -ya]];
    let t_right = [[g / 2.0, b], [a / 2.0, b], [a / 2.0, w / 2.0 - wt / 2.0]];
    let solids = vec![trace("arm +", c.metal, &arm, w)?, trace("arm -", c.metal, &other, w)?, trace("T-match +", c.metal, &t_right, wt)?, trace("T-match -", c.metal, &mirrored_x(&t_right), wt)?];
    let top = ya.max(b) + w;
    Ok(Geometry { solids, ports: vec![tag_port(b, g, wt, z0)], hints: vec![hint([-xa - w, -ya - w, 0.0], [xa + w, top, 0.0], w.min(wt) * 4.0)], ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::C64;
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::Project;

    const IDS: [&str; 7] = ["tag_loop", "tag_dual", "tag_dogbone", "tag_bowtie", "tag_slot", "tag_spiral", "tag_fractal"];

    #[test]
    fn a_hilbert_curve_fills_its_square_in_one_line() {
        for order in 1..=3usize {
            let c = hilbert(order);
            assert_eq!(c.len(), 4usize.pow(order as u32));
            let step = 1.0 / (1usize << order) as f64;
            for w in c.windows(2) {
                let d = (w[1][0] - w[0][0]).abs() + (w[1][1] - w[0][1]).abs();
                assert!((d - step).abs() < 1e-9, "order {order}: a jump of {d}");
            }
        }
    }

    #[test]
    fn a_spirals_two_arms_never_touch() {
        let w = 0.001;
        let arm = spiral_arm(0.001, 0.01, 0.004, 9, 1.0);
        let other: Vec<P2> = arm.iter().map(|q| [-q[0], -q[1]]).collect();
        let near = |a: P2, b: P2, p: P2| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
        };
        for s in arm.windows(2).skip(1) {
            for q in &other[1..] {
                assert!(near(s[0], s[1], *q) > 2.0 * w, "{s:?} comes to {q:?}");
            }
        }
    }

    /// Every structure builds, meshes as one thing with a feed, and is inductive at the design frequency
    /// or close to it, which is what a chip needs of it.
    #[test]
    fn every_structure_solves_as_a_single_antenna() {
        for id in IDS.into_iter().chain(["tag_bent"]) {
            let p = Project::from_template(id, Some("915MHz")).unwrap_or_else(|e| panic!("{id}: {e:#}"));
            let g = p.geometry().unwrap_or_else(|e| panic!("{id}: {e:#}"));
            assert!(g.solver_fit().surface_mom, "{id}");
            let m = MomProblem::new(&g, 915e6, MomOptions { edges_per_lambda: 24.0, ..Default::default() }).unwrap_or_else(|e| panic!("{id}: {e:#}"));
            let z: C64 = m.solve(915e6, false).unwrap_or_else(|e| panic!("{id}: {e:#}")).z[0][0];
            println!("{id:<12} {:>5} unknowns: Z = {:.1} {:+.1}j", m.unknowns(), z.re, z.im);
            assert!(z.re.is_finite() && z.im.is_finite() && z.re > 0.0, "{id}: {z}");
        }
    }

    /// This one took the machine's memory: 118 GB and still doubling. It has to end, in words.
    #[test]
    fn a_mesh_that_wont_stop_is_refused_rather_than_followed() {
        let mut p = Project::from_template("tag_fractal", Some("915MHz")).unwrap();
        for (name, v) in [("Ax", "0.05893"), ("Ay", "0.04644"), ("order", "2"), ("x0", "0.02228"), ("a", "0.03057"), ("b", "0.00829"), ("w", "0.00189"), ("g", "0.00157")] {
            p.params.iter_mut().find(|r| r.name == name).unwrap().expr = v.into();
        }
        let g = p.geometry().unwrap();
        let t = std::time::Instant::now();
        let out = MomProblem::new(&g, 928e6, MomOptions { edges_per_lambda: 96.0, ..Default::default() });
        assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
        match out {
            Ok(m) => assert!(m.unknowns() < 60_000, "{}", m.unknowns()),
            Err(e) => assert!(e.to_string().contains("keeps refining"), "{e}"),
        }
    }

    #[test]
    fn impossible_dimensions_say_so() {
        for (id, var, value) in [("tag_loop", "gc", "0.1mm"), ("tag_dual", "g1", "0.1mm"), ("tag_dogbone", "Hp", "0.1mm"), ("tag_bowtie", "xs", "0.1mm"), ("tag_slot", "b", "1m"), ("tag_spiral", "pitch", "0.1mm"), ("tag_fractal", "Ax", "1mm"), ("tag_bent", "Xa", "1mm")] {
            let mut p = Project::from_template(id, Some("915MHz")).unwrap();
            p.params.iter_mut().find(|r| r.name == var).unwrap_or_else(|| panic!("{id}: no {var}")).expr = value.into();
            assert!(p.geometry().is_err(), "{id} with {var} = {value}");
        }
    }
}
