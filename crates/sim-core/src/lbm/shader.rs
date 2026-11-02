//! Generates the D3Q19 compute shader. The loops over directions are written out in the generated
//! text, as plain scalars: arrays in shader locals cost real time on every backend.
//!
//! Streaming is the A-A pattern (Bailey et al., 2009), in place on one set of buffers:
//! - an **even** step reads each cell's own slots and writes the collided value to the opposite
//!   slot of the same cell;
//! - an **odd** step reads from the neighbours' opposite slots and writes to the neighbours' own.
//!
//! So after an odd step (an even number of steps in all) `f[i][x]` is plainly `f_i(x)`.
//! Populations are stored as `f_i - w_i`, which keeps single precision useful at low Mach.

use std::fmt::Write;

/// Lattice velocities. Directions come in opposite pairs, `(1, 2)`, `(3, 4)`, and so on.
pub const C: [[i32; 3]; 19] = [
    [0, 0, 0],
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
    [1, 1, 0],
    [-1, -1, 0],
    [1, 0, 1],
    [-1, 0, -1],
    [0, 1, 1],
    [0, -1, -1],
    [1, -1, 0],
    [-1, 1, 0],
    [1, 0, -1],
    [-1, 0, 1],
    [0, 1, -1],
    [0, -1, 1],
];

pub fn weight(i: usize) -> f64 {
    match i {
        0 => 1.0 / 3.0,
        1..=6 => 1.0 / 18.0,
        _ => 1.0 / 36.0,
    }
}

/// The opposite of direction `i`.
pub const fn opposite(i: usize) -> usize {
    if i == 0 {
        0
    } else if i % 2 == 1 {
        i + 1
    } else {
        i - 1
    }
}

/// Cell types in the flags buffer.
pub const FLUID: u32 = 0;
pub const WALL: u32 = 1;
pub const MOVING_WALL: u32 = 2;
pub const INLET: u32 = 3;
pub const OUTLET: u32 = 4;
/// A wall the flow slides along, for a floor whose drag comes from `ROUGH_LAYER` above it.
pub const SLIP_FLOOR: u32 = 5;
/// A flag on a fluid cell, on top of its kind: the layer above a rough floor, feeling the drag a
/// log-law wall would exert. It stands in for resolving the wall, which wind studies can't afford.
pub const ROUGH_LAYER: u32 = 16;

fn f32_lit(v: f64) -> String {
    format!("{:.9e}", v)
}

/// The constant multiplying `c_i . u` in direction `i` of the equilibrium, in place of `3 w_i`.
///
/// 1/6 and 1/12 have no exact binary form, and with the nearest floats the momentum of the
/// equilibrium comes out as `(1 + 7.6e-9)` times the true one, so every collision adds that much
/// momentum: a real bias, not noise, seen as a Taylor-Green vortex decaying too slowly on long
/// runs. These two are dyadic and chosen so that `sum c_x^2 alpha = 2 a + 8 d` is exactly 1.
pub fn alpha(i: usize) -> f64 {
    // 1/6 with its last two bits cleared, so that 1 - 2a is representable too.
    let axis = f64::from((1.0f32 / 6.0).to_bits() & !3u32).max(0.0);
    let axis = f64::from(f32::from_bits(axis as u32));
    let diagonal = (1.0 - 2.0 * axis) / 8.0;
    match i {
        0 => 0.0,
        1..=6 => axis,
        _ => diagonal,
    }
}

/// `sum_i(c_i[axis] * f_i)`, only the terms that exist.
fn moment(axis: usize, prefix: &str) -> String {
    let mut plus = Vec::new();
    let mut minus = Vec::new();
    for (i, c) in C.iter().enumerate() {
        match c[axis] {
            1 => plus.push(format!("{prefix}{i}")),
            -1 => minus.push(format!("{prefix}{i}")),
            _ => {}
        }
    }
    format!("({}) - ({})", plus.join(" + "), minus.join(" + "))
}

/// Text of the collision, reading `f0..f18` and writing `g0..g18` (the post-collision values).
fn collision() -> String {
    let mut s = String::new();
    let all: Vec<String> = (0..19).map(|i| format!("f{i}")).collect();
    let _ = writeln!(s, "    let drho = {};", all.join(" + "));
    let _ = writeln!(s, "    let rho = 1.0 + drho;");
    let _ = writeln!(s, "    let inv = 1.0 / rho;");
    // Momentum is kept as it is, not as rho * (m / rho): that round trip isn't exact in floats, and
    // an equilibrium with a slightly different momentum adds or removes some at every collision.
    let _ = writeln!(s, "    let m0x = {};", moment(0, "f"));
    let _ = writeln!(s, "    let m0y = {};", moment(1, "f"));
    let _ = writeln!(s, "    var fx_cell = p.fx;");
    let _ = writeln!(s, "    var fy_cell = p.fy;");
    let _ = writeln!(s, "    if ((flags[n] & {ROUGH_LAYER}u) != 0u) {{ let ax = m0x * inv; let ay = m0y * inv; let sp = sqrt(ax * ax + ay * ay); fx_cell = fx_cell - p.rough * sp * ax; fy_cell = fy_cell - p.rough * sp * ay; }}");
    let _ = writeln!(s, "    var mx = m0x + 0.5 * fx_cell;");
    let _ = writeln!(s, "    var my = m0y + 0.5 * fy_cell;");
    let _ = writeln!(s, "    let fz_cell = p.fz + cell_force[n];");
    let _ = writeln!(s, "    var mz = {} + 0.5 * fz_cell;", moment(2, "f"));
    let _ = writeln!(s, "    var ux = mx * inv;");
    let _ = writeln!(s, "    var uy = my * inv;");
    let _ = writeln!(s, "    var uz = mz * inv;");
    // The cell's type decides what happens to it. Walls swap, which is halfway bounce-back once
    // the population has streamed to the fluid and back.
    let _ = writeln!(s, "    let kind = flags[n] & 15u;");
    let _ = writeln!(s, "    if (kind == {WALL}u || kind == {MOVING_WALL}u) {{");
    for (i, c) in C.iter().enumerate() {
        let o = opposite(i);
        if i == 0 {
            let _ = writeln!(s, "      g0 = f0;");
        } else {
            // Moving walls add 6 w (c . u_wall). The wall's own density is taken as the reference.
            let _ = writeln!(s, "      g{i} = f{o} + select(0.0, {w} * ({cx}.0 * p.wall_x + {cy}.0 * p.wall_y + {cz}.0 * p.wall_z), kind == {MOVING_WALL}u);", w = f32_lit(6.0 * weight(i)), cx = c[0], cy = c[1], cz = c[2]);
        }
    }
    let _ = writeln!(s, "    }} else if (kind == {SLIP_FLOOR}u) {{");
    for (i, c) in C.iter().enumerate() {
        // What came down goes back up, keeping its horizontal motion.
        let mirror = C.iter().position(|m| m[0] == c[0] && m[1] == c[1] && m[2] == -c[2]).unwrap_or(i);
        let _ = writeln!(s, "      g{i} = f{mirror};");
    }
    let _ = writeln!(s, "    }} else {{");
    let _ = writeln!(s, "    var drho_eq = drho;");
    let _ = writeln!(s, "    let open_cell = kind == {INLET}u || kind == {OUTLET}u;");
    let _ = writeln!(s, "    if (kind == {INLET}u) {{");
    // A uniform inlet, or a log-law profile with height when a roughness length is given.
    let _ = writeln!(s, "      var scale = 1.0;");
    let _ = writeln!(s, "      if (p.inlet_z0 > 0.0) {{ let height = f32(n / (p.nx * p.ny)); scale = log((max(height - 0.5, 0.0) + p.inlet_z0) / p.inlet_z0) / p.inlet_log_ref; }}");
    let _ = writeln!(s, "      if (p.inlet_channel > 0.0) {{ let rem2 = n - (n / (p.nx * p.ny)) * p.nx * p.ny; let row_y = f32(rem2 / p.nx) - 0.5; scale = 4.0 * row_y * (p.inlet_channel - row_y) / (p.inlet_channel * p.inlet_channel); }}");
    let _ = writeln!(s, "      ux = p.inlet_x * scale; uy = p.inlet_y * scale; uz = p.inlet_z * scale; mx = rho * ux; my = rho * uy; mz = rho * uz;");
    let _ = writeln!(s, "    }} else if (kind == {OUTLET}u) {{ drho_eq = 0.0; mx = ux; my = uy; mz = uz; }}");
    let _ = writeln!(s, "    let usq = 1.5 * (ux * ux + uy * uy + uz * uz);");
    // Never `rho - 1`: that throws away the precision storing f - w exists to keep.
    let _ = writeln!(s, "    let rho_eq = 1.0 + drho_eq;");
    for (i, c) in C.iter().enumerate() {
        let w = f32_lit(weight(i));
        let cu = format!("({}.0 * ux + {}.0 * uy + {}.0 * uz)", c[0], c[1], c[2]);
        let _ = writeln!(s, "    let cu{i} = {cu};");
        let a = f32_lit(alpha(i));
        let _ = writeln!(s, "    let e{i} = {w} * (drho_eq + rho_eq * (4.5 * cu{i} * cu{i} - usq)) + {a} * ({}.0 * mx + {}.0 * my + {}.0 * mz);", c[0], c[1], c[2]);
    }
    let _ = writeln!(s, "    if (open_cell) {{");
    for i in 0..19 {
        let _ = writeln!(s, "      g{i} = e{i};");
    }
    let _ = writeln!(s, "    }} else {{");
    // Turbulence: Smagorinsky, from the non-equilibrium stress (Hou et al., 1996). Without it the
    // relaxation stays the constant laminar one.
    let _ = writeln!(s, "    var wp = p.omega_plus;");
    let _ = writeln!(s, "    var wm = p.omega_minus;");
    let _ = writeln!(s, "    let fu = 3.0 * (ux * fx_cell + uy * fy_cell + uz * fz_cell);");
    let _ = writeln!(s, "    if (p.les > 0.0) {{");
    let mut stress = |name: &str, a: usize, b: usize| {
        let mut terms = Vec::new();
        for (i, c) in C.iter().enumerate() {
            let k = c[a] * c[b];
            if k != 0 {
                terms.push(format!("{}(f{i} - e{i})", if k < 0 { "-" } else { "+" }));
            }
        }
        let _ = writeln!(s, "      let {name} = 0.0 {};", terms.join(" "));
    };
    stress("qxx", 0, 0);
    stress("qyy", 1, 1);
    stress("qzz", 2, 2);
    stress("qxy", 0, 1);
    stress("qxz", 0, 2);
    stress("qyz", 1, 2);
    let _ = writeln!(s, "      let q = sqrt(qxx * qxx + qyy * qyy + qzz * qzz + 2.0 * (qxy * qxy + qxz * qxz + qyz * qyz));");
    let _ = writeln!(s, "      let tau0 = 1.0 / p.omega_plus;");
    // Near a wall, Prandtl's mixing length: it grows with height, which is what holds a log-law
    // profile up when nothing upstream supplies resolved turbulence.
    let _ = writeln!(s, "      var les_here = p.les;");
    let _ = writeln!(s, "      if (p.mixing_max > 0.0) {{ let l = min(0.41 * max(f32(n / (p.nx * p.ny)) - 0.5, 0.0), p.mixing_max); les_here = 25.4558 * l * l; }}");
    let _ = writeln!(s, "      let tau = 0.5 * (tau0 + sqrt(tau0 * tau0 + les_here * q * inv));");
    let _ = writeln!(s, "      wp = 1.0 / tau;");
    // Regularised: of what isn't in equilibrium, only the stress survives. The higher moments that
    // make low-viscosity lattice runs blow up are dropped at every step (Latt and Chopard, 2006).
    let _ = writeln!(s, "      let qt = (qxx + qyy + qzz) / 3.0;");
    let _ = writeln!(s, "      let sr = 1.0 - 0.5 * wp;");
    for (i, c) in C.iter().enumerate() {
        let w = f32_lit(weight(i));
        let (x, y, z) = (c[0], c[1], c[2]);
        let cf = format!("{x}.0 * fx_cell + {y}.0 * fy_cell + {z}.0 * fz_cell");
        let _ = writeln!(s, "      g{i} = e{i} + (1.0 - wp) * {w} * 4.5 * ({xx}.0 * qxx + {yy}.0 * qyy + {zz}.0 * qzz + 2.0 * ({xy}.0 * qxy + {xz}.0 * qxz + {yz}.0 * qyz) - qt) + sr * {w} * (3.0 * ({cf}) + 9.0 * cu{i} * ({cf}) - fu);", xx = x * x, yy = y * y, zz = z * z, xy = x * y, xz = x * z, yz = y * z);
    }
    let _ = writeln!(s, "    }} else {{");
    // The rest position has no odd part.
    let _ = writeln!(s, "    let sp = 1.0 - 0.5 * wp;");
    let _ = writeln!(s, "    let sm = 1.0 - 0.5 * wm;");
    let _ = writeln!(s, "    g0 = f0 - wp * (f0 - e0) - sp * {w} * fu;", w = f32_lit(weight(0)));
    for i in (1..19).step_by(2) {
        let o = i + 1;
        let w = f32_lit(weight(i));
        let c = C[i];
        let _ = writeln!(s, "    {{");
        let _ = writeln!(s, "      let a = 0.5 * (f{i} + f{o}); let b = 0.5 * (f{i} - f{o});");
        let _ = writeln!(s, "      let ea = 0.5 * (e{i} + e{o}); let eb = 0.5 * (e{i} - e{o});");
        let _ = writeln!(s, "      let cf = {}.0 * fx_cell + {}.0 * fy_cell + {}.0 * fz_cell;", c[0], c[1], c[2]);
        let _ = writeln!(s, "      let sa = {w} * (9.0 * cu{i} * cf - fu);");
        let _ = writeln!(s, "      let sb = {w} * 3.0 * cf;");
        let _ = writeln!(s, "      let plus = a - wp * (a - ea) + sp * sa;");
        let _ = writeln!(s, "      let minus = b - wm * (b - eb) + sm * sb;");
        let _ = writeln!(s, "      g{i} = plus + minus;");
        let _ = writeln!(s, "      g{o} = plus - minus;");
        let _ = writeln!(s, "    }}");
    }
    let _ = writeln!(s, "    }}");
    let _ = writeln!(s, "    }}");
    let _ = writeln!(s, "    }}");
    s
}

/// Neighbour coordinates and the index of the cell one step along each direction.
fn neighbours() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "    let z = n / (p.nx * p.ny);");
    let _ = writeln!(s, "    let rem = n - z * p.nx * p.ny;");
    let _ = writeln!(s, "    let y = rem / p.nx;");
    let _ = writeln!(s, "    let x = rem - y * p.nx;");
    let _ = writeln!(s, "    let xp = select(x + 1u, 0u, x + 1u == p.nx);");
    let _ = writeln!(s, "    let xm = select(x - 1u, p.nx - 1u, x == 0u);");
    let _ = writeln!(s, "    let yp = select(y + 1u, 0u, y + 1u == p.ny) * p.nx;");
    let _ = writeln!(s, "    let ym = select(y - 1u, p.ny - 1u, y == 0u) * p.nx;");
    let _ = writeln!(s, "    let y0 = y * p.nx;");
    let _ = writeln!(s, "    let zp = select(z + 1u, 0u, z + 1u == p.nz) * p.nx * p.ny;");
    let _ = writeln!(s, "    let zm = select(z - 1u, p.nz - 1u, z == 0u) * p.nx * p.ny;");
    let _ = writeln!(s, "    let z0 = z * p.nx * p.ny;");
    for (i, c) in C.iter().enumerate().skip(1) {
        let xs = ["xm", "x", "xp"][(c[0] + 1) as usize];
        let ys = ["ym", "y0", "yp"][(c[1] + 1) as usize];
        let zs = ["zm", "z0", "zp"][(c[2] + 1) as usize];
        let _ = writeln!(s, "    let j{i} = {xs} + {ys} + {zs};");
    }
    s
}

/// The whole shader. Bindings: group 0 has the 19 population buffers, then flags and parameters;
/// group 1 has the output of the `macros` entry point: density and velocity.
pub fn source() -> String {
    let mut s = String::new();
    s.push_str(
        "struct Params {\n  nx: u32, ny: u32, nz: u32, n: u32,\n  omega_plus: f32, omega_minus: f32, les: f32, row: u32,\n  fx: f32, fy: f32, fz: f32, pad1: f32,\n  wall_x: f32, wall_y: f32, wall_z: f32, pad2: f32,\n  inlet_x: f32, inlet_y: f32, inlet_z: f32, inlet_z0: f32,\n  inlet_log_ref: f32, inlet_channel: f32, rough: f32, mixing_max: f32,\n};\n",
    );
    for i in 0..19 {
        let _ = writeln!(s, "@group(0) @binding({i}) var<storage, read_write> a{i}: array<f32>;");
    }
    s.push_str("@group(0) @binding(19) var<storage, read> flags: array<u32>;\n@group(0) @binding(20) var<uniform> p: Params;\n@group(0) @binding(21) var<storage, read> cell_force: array<f32>;\n");
    s.push_str("@group(1) @binding(0) var<storage, read_write> out_rho: array<f32>;\n@group(1) @binding(1) var<storage, read_write> out_ux: array<f32>;\n@group(1) @binding(2) var<storage, read_write> out_uy: array<f32>;\n@group(1) @binding(3) var<storage, read_write> out_uz: array<f32>;\n");

    let declare = |s: &mut String| {
        for i in 0..19 {
            let _ = writeln!(s, "    var g{i}: f32 = 0.0;");
        }
    };

    // Even step: own slots in, opposite slots out.
    s.push_str("@compute @workgroup_size(128) fn even(@builtin(global_invocation_id) id: vec3<u32>) {\n    let n = id.x + id.y * p.row;\n    if (n >= p.n) { return; }\n");
    for i in 0..19 {
        let _ = writeln!(s, "    let f{i} = a{i}[n];");
    }
    declare(&mut s);
    s.push_str(&collision());
    for i in 0..19 {
        let _ = writeln!(s, "    a{}[n] = g{i};", opposite(i));
    }
    s.push_str("}\n");

    // Odd step: f_i(x) is in slot opposite(i) of the cell behind; it leaves into slot i of the cell ahead.
    s.push_str("@compute @workgroup_size(128) fn odd(@builtin(global_invocation_id) id: vec3<u32>) {\n    let n = id.x + id.y * p.row;\n    if (n >= p.n) { return; }\n");
    s.push_str(&neighbours());
    let _ = writeln!(s, "    let f0 = a0[n];");
    for i in 1..19 {
        let o = opposite(i);
        let _ = writeln!(s, "    let f{i} = a{o}[j{o}];");
    }
    declare(&mut s);
    s.push_str(&collision());
    let _ = writeln!(s, "    a0[n] = g0;");
    for i in 1..19 {
        let _ = writeln!(s, "    a{i}[j{i}] = g{i};");
    }
    s.push_str("}\n");

    // Density and velocity, valid after an even number of steps.
    s.push_str("@compute @workgroup_size(128) fn macros(@builtin(global_invocation_id) id: vec3<u32>) {\n    let n = id.x + id.y * p.row;\n    if (n >= p.n) { return; }\n");
    for i in 0..19 {
        let _ = writeln!(s, "    let f{i} = a{i}[n];");
    }
    let all: Vec<String> = (0..19).map(|i| format!("f{i}")).collect();
    let _ = writeln!(s, "    let rho = 1.0 + {};", all.join(" + "));
    let _ = writeln!(s, "    out_rho[n] = rho;");
    let _ = writeln!(s, "    out_ux[n] = ({} + 0.5 * p.fx) / rho;", moment(0, "f"));
    let _ = writeln!(s, "    out_uy[n] = ({} + 0.5 * p.fy) / rho;", moment(1, "f"));
    let _ = writeln!(s, "    out_uz[n] = ({} + 0.5 * (p.fz + cell_force[n])) / rho;", moment(2, "f"));
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lattice_is_consistent() {
        let mut sum = 0.0;
        for (i, c) in C.iter().enumerate() {
            sum += weight(i);
            let o = opposite(i);
            assert_eq!([c[0], c[1], c[2]], [-C[o][0], -C[o][1], -C[o][2]], "{i} and {o} must be opposite");
            assert_eq!(opposite(o), i);
        }
        assert!((sum - 1.0).abs() < 1e-12);
        // Second moment: sum w c_a c_b = delta_ab / 3.
        for a in 0..3 {
            for b in 0..3 {
                let m: f64 = C.iter().enumerate().map(|(i, c)| weight(i) * f64::from(c[a] * c[b])).sum();
                assert!((m - if a == b { 1.0 / 3.0 } else { 0.0 }).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn the_equilibrium_conserves_momentum_exactly() {
        for i in 0..19 {
            let a = alpha(i);
            assert_eq!(f64::from(a as f32), a, "direction {i}: {a} must be exact in single precision");
            if i > 0 {
                assert!((a - 3.0 * weight(i)).abs() < 1e-6, "direction {i}: {a} is far from 3w");
            }
        }
        // Exact, in binary, with no tolerance.
        for axis in 0..3 {
            let sum: f64 = C.iter().enumerate().map(|(i, c)| alpha(i) * f64::from(c[axis] * c[axis])).sum();
            assert_eq!(sum, 1.0, "axis {axis}");
        }
    }

    #[test]
    fn the_shader_is_valid_wgsl() {
        let text = source();
        let module = naga::front::wgsl::parse_str(&text).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&text)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&module).expect("the shader validates");
        let names: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["even", "odd", "macros"]);
    }
}
