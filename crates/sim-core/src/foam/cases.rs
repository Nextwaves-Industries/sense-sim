//! Building blocks of OpenFOAM cases.

use std::fmt::Write;

use super::Case;

/// `blockMeshDict` for one axis-aligned block, with one named patch per side: `xmin`, `xmax`, ...
/// A side named `empty` here becomes a 2D `empty` patch.
pub fn block_mesh(case: &mut Case, lo: [f64; 3], hi: [f64; 3], cells: [usize; 3], grading: [f64; 3], kinds: [(&str, &str); 6]) {
    let [x0, y0, z0] = lo;
    let [x1, y1, z1] = hi;
    let names = ["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"];
    let faces = ["(0 4 7 3)", "(1 2 6 5)", "(0 1 5 4)", "(3 7 6 2)", "(0 3 2 1)", "(4 5 6 7)"];
    let mut body = format!(
        "scale 1;\nvertices\n(\n ({x0} {y0} {z0}) ({x1} {y0} {z0}) ({x1} {y1} {z0}) ({x0} {y1} {z0})\n ({x0} {y0} {z1}) ({x1} {y0} {z1}) ({x1} {y1} {z1}) ({x0} {y1} {z1})\n);\nblocks ( hex (0 1 2 3 4 5 6 7) ({} {} {}) simpleGrading ({} {} {}) );\nedges ();\nboundary\n(\n",
        cells[0], cells[1], cells[2], grading[0], grading[1], grading[2]
    );
    // Sides that share a name and kind merge into one patch.
    let mut seen: Vec<&str> = Vec::new();
    for (i, (name, kind)) in kinds.iter().enumerate() {
        if seen.contains(name) {
            continue;
        }
        seen.push(name);
        let _ = writeln!(body, " {name} {{ type {kind}; faces (");
        for (j, (other, _)) in kinds.iter().enumerate() {
            if other == name {
                let _ = writeln!(body, "  {}", faces[j]);
            }
        }
        let _ = writeln!(body, " ); }}");
        let _ = names[i];
    }
    body.push_str(");\nmergePatchPairs ();\n");
    case.dict("system", "blockMeshDict", "dictionary", &body);
}

/// What `controlDict` needs.
pub struct Control<'a> {
    pub application: &'a str,
    pub end_time: f64,
    pub dt: f64,
    pub write_interval: f64,
    /// Variable time step with these Courant limits (flow, interface).
    pub adjustable: Option<(f64, f64)>,
    /// Extra entries, such as `functions { ... }`.
    pub extra: &'a str,
}

pub fn control_dict(case: &mut Case, c: &Control) {
    let adjust = match c.adjustable {
        Some((co, alpha)) => format!("adjustTimeStep yes;\nmaxCo {co};\nmaxAlphaCo {alpha};\nmaxDeltaT 1;\nwriteControl adjustableRunTime;\nwriteInterval {};", c.write_interval),
        None => format!("writeControl runTime;\nwriteInterval {};", c.write_interval),
    };
    let body = format!(
        "application {};\nstartFrom startTime;\nstartTime 0;\nstopAt endTime;\nendTime {};\ndeltaT {};\n{adjust}\npurgeWrite 2;\nwriteFormat ascii;\nwritePrecision 8;\nwriteCompression off;\ntimeFormat general;\ntimePrecision 8;\nrunTimeModifiable false;\n{}\n",
        c.application, c.end_time, c.dt, c.extra
    );
    case.dict("system", "controlDict", "dictionary", &body);
}

/// `boundaryField` entries: patch name to its text.
pub fn field(case: &mut Case, name: &str, class: &str, dimensions: &str, internal: &str, patches: &[(&str, &str)]) {
    let mut body = format!("dimensions {dimensions};\ninternalField uniform {internal};\nboundaryField\n{{\n");
    for (p, text) in patches {
        let _ = writeln!(body, " {p} {{ {text} }}");
    }
    body.push_str("}\n");
    case.dict("0", name, class, &body);
}

/// Dimension sets, in OpenFOAM's order: kg m s K mol A cd.
pub mod dim {
    pub const VELOCITY: &str = "[0 1 -1 0 0 0 0]";
    pub const KINEMATIC_PRESSURE: &str = "[0 2 -2 0 0 0 0]";
    pub const PRESSURE: &str = "[1 -1 -2 0 0 0 0]";
    pub const TEMPERATURE: &str = "[0 0 0 1 0 0 0]";
    pub const NONE: &str = "[0 0 0 0 0 0 0]";
}

/// `transportProperties` for one Newtonian fluid.
pub fn transport(case: &mut Case, nu: f64) {
    case.dict("constant", "transportProperties", "dictionary", &format!("transportModel Newtonian;\nnu {nu};\n"));
}

/// A laminar `turbulenceProperties`.
pub fn laminar(case: &mut Case) {
    case.dict("constant", "turbulenceProperties", "dictionary", "simulationType laminar;\n");
}

/// A RANS `turbulenceProperties` with this model.
pub fn rans(case: &mut Case, model: &str) {
    case.dict("constant", "turbulenceProperties", "dictionary", &format!("simulationType RAS;\nRAS {{ RASModel {model}; turbulence on; printCoeffs off; }}\n"));
}
