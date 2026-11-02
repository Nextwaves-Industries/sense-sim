//! Small 3D helpers: rigid and scaled transforms, the IFC axis rules, and the
//! plan-view box fit.

use std::f64::consts::{FRAC_PI_2, PI};

pub(crate) type V3 = [f64; 3];

pub(crate) const EX: V3 = [1.0, 0.0, 0.0];
pub(crate) const EY: V3 = [0.0, 1.0, 0.0];
pub(crate) const EZ: V3 = [0.0, 0.0, 1.0];

pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn mul(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}

pub(crate) fn unit(a: V3) -> Option<V3> {
    let l = length(a);
    (l.is_finite() && l > 1e-12).then(|| mul(a, 1.0 / l))
}

/// Affine transform: columns `x`, `y`, `z` plus translation `t`. The columns are
/// not always unit length, because a mapped item can scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Xf {
    pub x: V3,
    pub y: V3,
    pub z: V3,
    pub t: V3,
}

impl Xf {
    pub(crate) const IDENTITY: Xf = Xf { x: EX, y: EY, z: EZ, t: [0.0; 3] };

    pub(crate) fn point(&self, p: V3) -> V3 {
        add(self.t, self.vector(p))
    }

    pub(crate) fn vector(&self, v: V3) -> V3 {
        add(add(mul(self.x, v[0]), mul(self.y, v[1])), mul(self.z, v[2]))
    }

    /// `self` applied after `inner`.
    pub(crate) fn then(&self, inner: &Xf) -> Xf {
        Xf {
            x: self.vector(inner.x),
            y: self.vector(inner.y),
            z: self.vector(inner.z),
            t: self.point(inner.t),
        }
    }

    pub(crate) fn is_finite(&self) -> bool {
        [self.x, self.y, self.z, self.t].iter().flatten().all(|c| c.is_finite())
    }
}

/// EXPRESS `IfcFirstProjAxis`: `arg` (or a default) with its `z` component removed.
/// A direction parallel to `z` is invalid input and falls back to the default
/// rather than producing a singular frame.
fn first_proj_axis(z: V3, arg: Option<V3>) -> V3 {
    let v = arg
        .and_then(unit)
        .filter(|v| length(cross(*v, z)) > 1e-9)
        .unwrap_or(if z[0].abs() > 1.0 - 1e-9 { EY } else { EX });
    unit(sub(v, mul(z, dot(v, z)))).unwrap_or(EX)
}

/// Frame of an `IfcAxis2Placement3D`, following EXPRESS `IfcBuildAxes`.
pub(crate) fn placement_frame(location: V3, axis: Option<V3>, ref_direction: Option<V3>) -> Xf {
    let z = axis.and_then(unit).unwrap_or(EZ);
    let x = first_proj_axis(z, ref_direction);
    let y = cross(z, x);
    Xf { x, y, z, t: location }
}

/// Frame of an `IfcAxis2Placement2D`.
pub(crate) fn placement_frame_2d(location: V3, ref_direction: Option<V3>) -> Xf {
    let x = ref_direction
        .map(|d| [d[0], d[1], 0.0])
        .and_then(unit)
        .unwrap_or(EX);
    Xf { x, y: [-x[1], x[0], 0.0], z: EZ, t: [location[0], location[1], 0.0] }
}

/// `IfcCartesianTransformationOperator`, following EXPRESS `IfcBaseAxis`. Unlike
/// a placement, axis 2 is free to make a left-handed (mirroring) frame.
pub(crate) fn operator_frame(
    axis1: Option<V3>,
    axis2: Option<V3>,
    axis3: Option<V3>,
    origin: V3,
    scale: [f64; 3],
) -> Xf {
    let z = axis3.and_then(unit).unwrap_or(EZ);
    let x = first_proj_axis(z, axis1);
    let v = axis2.and_then(unit).unwrap_or(EY);
    let y = unit(sub(sub(v, mul(z, dot(v, z))), mul(x, dot(v, x)))).unwrap_or_else(|| cross(z, x));
    Xf { x: mul(x, scale[0]), y: mul(y, scale[1]), z: mul(z, scale[2]), t: origin }
}

/// Wrap an angle to (-pi, pi].
pub(crate) fn wrap_angle(a: f64) -> f64 {
    let wrapped = (a + PI).rem_euclid(2.0 * PI) - PI;
    if wrapped <= -PI { PI } else { wrapped }
}

/// The yaw implied by a placement: its X axis in plan, or its Y axis when X points up.
pub(crate) fn frame_yaw(frame: &Xf) -> f64 {
    for axis in [frame.x, frame.y] {
        let horizontal = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
        if horizontal > 1e-3 * length(axis).max(1e-12) {
            return axis[1].atan2(axis[0]);
        }
    }
    0.0
}

/// Convex hull of the points' plan view (monotone chain). Input must be finite.
pub(crate) fn plan_hull(points: &[V3]) -> Vec<[f64; 2]> {
    let mut pts: Vec<[f64; 2]> = points.iter().map(|p| [p[0], p[1]]).collect();
    pts.sort_unstable_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let chain = |points: &mut dyn Iterator<Item = [f64; 2]>| {
        let mut out: Vec<[f64; 2]> = Vec::new();
        for p in points {
            while let [.., o, a] = out[..] {
                if (a[0] - o[0]) * (p[1] - o[1]) - (a[1] - o[1]) * (p[0] - o[0]) > 0.0 {
                    break;
                }
                out.pop();
            }
            out.push(p);
        }
        out.pop();
        out
    };
    let mut hull = chain(&mut pts.iter().copied());
    hull.extend(chain(&mut pts.iter().rev().copied()));
    hull
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlanFit {
    pub yaw: f64,
    /// Long side, then short side.
    pub size: [f64; 2],
    pub center: [f64; 2],
}

/// `(min_u, max_u, min_v, max_v)` of the hull in a frame rotated by `yaw`.
fn extents(hull: &[[f64; 2]], yaw: f64) -> [f64; 4] {
    let (s, c) = yaw.sin_cos();
    let mut e = [f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY];
    for p in hull {
        let u = p[0] * c + p[1] * s;
        let v = -p[0] * s + p[1] * c;
        e = [e[0].min(u), e[1].max(u), e[2].min(v), e[3].max(v)];
    }
    e
}

fn area(e: [f64; 4]) -> f64 {
    (e[1] - e[0]) * (e[3] - e[2])
}

/// Hull edges tried as box directions; a larger hull is thinned to this many.
const MAX_FIT_EDGES: usize = 256;
/// An outline filling at least this share of its smallest box counts as rectangular.
const RECTANGULAR_FILL: f64 = 0.95;
/// How much smaller the smallest box must be to replace the placement's yaw. A
/// rectangular outline's sides really are its directions, so a small gain counts:
/// that is a wall drawn at an angle under an unrotated placement. Any other outline
/// must gain much more, or a faceted round column, whose many edges each offer a
/// slightly smaller box, would lose the model's frame for an arbitrary one.
const REPLACE_FRAME_GAIN_RECTANGULAR: f64 = 0.005;
const REPLACE_FRAME_GAIN_OTHER: f64 = 0.10;

/// Area of a convex polygon, by the shoelace formula.
fn polygon_area(hull: &[[f64; 2]]) -> f64 {
    let twice: f64 = (0..hull.len())
        .map(|i| {
            let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    twice.abs() * 0.5
}

/// Upright box around the points in plan: in the placement's yaw if that fits well,
/// otherwise the smallest box. The result's first side is the longer one, turning
/// the yaw by 90 degrees when the chosen X is the short side.
pub(crate) fn fit_plan(points: &[V3], frame_yaw: f64) -> PlanFit {
    let hull = plan_hull(points);
    let mut yaw = frame_yaw;
    let mut e = extents(&hull, yaw);
    let frame_area = area(e);
    let outline_area = polygon_area(&hull);
    let stride = (hull.len() / MAX_FIT_EDGES).max(1);
    let mut best: Option<(f64, f64)> = None;
    for i in (0..hull.len()).step_by(stride) {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        if dx == 0.0 && dy == 0.0 {
            continue;
        }
        let candidate = dy.atan2(dx);
        let candidate_area = area(extents(&hull, candidate));
        if best.is_none_or(|(a, _)| candidate_area < a) {
            best = Some((candidate_area, candidate));
        }
    }
    let required_gain = |best_area: f64| {
        if outline_area >= best_area * RECTANGULAR_FILL {
            REPLACE_FRAME_GAIN_RECTANGULAR
        } else {
            REPLACE_FRAME_GAIN_OTHER
        }
    };
    if let Some((best_area, candidate)) = best
        && frame_area > best_area * (1.0 + required_gain(best_area)) + 1e-12
    {
        // Direction of an edge, so only defined modulo pi: report it in (-pi/2, pi/2].
        yaw = if candidate > FRAC_PI_2 {
            candidate - PI
        } else if candidate <= -FRAC_PI_2 {
            candidate + PI
        } else {
            candidate
        };
        e = extents(&hull, yaw);
    }
    let (s, c) = yaw.sin_cos();
    let (mid_u, mid_v) = ((e[0] + e[1]) * 0.5, (e[2] + e[3]) * 0.5);
    let center = [mid_u * c - mid_v * s, mid_u * s + mid_v * c];
    let (mut long, mut short) = (e[1] - e[0], e[3] - e[2]);
    if short > long * (1.0 + 1e-9) + 1e-12 {
        std::mem::swap(&mut long, &mut short);
        yaw += FRAC_PI_2;
    }
    PlanFit { yaw: wrap_angle(yaw), size: [long, short], center }
}
