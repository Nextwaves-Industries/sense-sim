//! Writes CalculiX input decks.
//!
//! Gmsh's own `.inp` output is not used: it mixes in surface elements CalculiX would read as
//! shells, and has no element-face sets, which loads, film and contact need. The tetrahedra are
//! written here in Abaqus node order, which differs from Gmsh's in the last two mid-side nodes.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use anyhow::{Result, anyhow, bail};

use crate::mesh::msh::TetMesh;

/// Isotropic material, in SI. Only what the procedure needs must be present.
#[derive(Debug, Clone)]
pub struct Material {
    pub name: String,
    pub young: f64,
    pub poisson: f64,
    pub density: f64,
    pub conductivity: Option<f64>,
    pub specific_heat: Option<f64>,
    pub expansion: Option<f64>,
    /// Hardening: (true stress, plastic strain) points, starting at the yield stress.
    pub plastic: Vec<(f64, f64)>,
}

/// A face group: named sets of the mesh's surface entities.
#[derive(Debug, Clone, Default)]
pub struct Groups(pub Vec<(String, Vec<i32>)>);

impl Groups {
    fn entities(&self, name: &str) -> Result<&[i32]> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, e)| e.as_slice()).ok_or_else(|| anyhow!("no face group named {name:?}"))
    }
}

/// Conditions, each applied to a face group by name.
#[derive(Debug, Clone)]
pub enum Condition {
    /// All displacements zero.
    Fixed { group: String },
    /// One displacement direction (0 = x) fixed to `value`.
    Displacement { group: String, axis: usize, value: f64 },
    /// The same on particular nodes (0-based): for a line of them, which a face group can't name.
    DisplacementAt { nodes: Vec<u32>, axis: usize, value: f64 },
    /// A temperature for every node of a static run, as a thermal load: one value per mesh node. Goes
    /// with `prescribed_temperature`, which gives the temperature it's measured from.
    TemperatureField { values: Vec<f64> },
    /// Two groups that press on each other but can't pass through. `slope` is the penalty, in Pa/m: how
    /// hard the surfaces resist each millimetre of overlap. Stiff enough not to change the answer, soft
    /// enough for the solver to converge: about 10 to 50 times the Young's modulus over a small element.
    Contact { slave: String, master: String, slope: f64, friction: Option<f64> },
    /// A total force in newtons, spread over the group with consistent nodal weights.
    Force { group: String, force: [f64; 3] },
    /// Pressure in pascals, pushing into the surface when positive.
    Pressure { group: String, pressure: f64 },
    /// Acceleration of every element, in m/s².
    Gravity { acceleration: [f64; 3] },
    /// Fixed temperature, in the deck's temperature unit.
    Temperature { group: String, value: f64 },
    /// Heat flowing into the surface, in W/m².
    HeatFlux { group: String, flux: f64 },
    /// Convection: coefficient W/m²K and ambient temperature.
    Convection { group: String, coefficient: f64, ambient: f64 },
    /// Volumetric heat source, W/m³.
    HeatSource { power: f64 },
}

/// The kind of run, with what each needs.
#[derive(Debug, Clone)]
pub enum Procedure {
    Static { nlgeom: bool, increment: f64 },
    Frequency { modes: usize },
    /// Direct time integration.
    Dynamic { dt: f64, duration: f64 },
    /// Modal steady-state: `modes` are extracted first; `damping` is the modal ratio.
    Harmonic { modes: usize, f_min: f64, f_max: f64, points: usize, damping: f64 },
    /// `transient`: (dt, duration); `None` for steady state.
    Heat { transient: Option<(f64, f64)> },
    Thermomechanical { transient: Option<(f64, f64)>, reference: f64 },
}

#[derive(Debug, Clone)]
pub struct Model<'a> {
    pub mesh: &'a TetMesh,
    pub material: Material,
    pub groups: Groups,
    pub conditions: Vec<Condition>,
    pub procedure: Procedure,
    /// Starting temperature of thermal runs.
    pub initial_temperature: Option<f64>,
    /// A prescribed uniform temperature in a `Static` run, for thermal stress without a thermal
    /// solve, with the reference temperature it is measured from.
    pub prescribed_temperature: Option<(f64, f64)>,
}

/// (corner a, b, c) of each CalculiX face S1..S4 of a tetrahedron, 0-based.
const TET_FACES: [[usize; 3]; 4] = [[0, 1, 2], [0, 3, 1], [1, 3, 2], [2, 3, 0]];

/// Maps Gmsh's tet10 order to Abaqus's: mid-side nodes 8 and 9 trade places.
pub fn abaqus_tet(t: &[u32; 10]) -> [u32; 10] {
    [t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[9], t[8]]
}

/// Finds, for each boundary triangle, its element (0-based) and face (1-4).
struct FaceFinder(HashMap<[u32; 3], (u32, u8)>);

impl FaceFinder {
    fn new(mesh: &TetMesh) -> Self {
        let mut map = HashMap::with_capacity(mesh.tets.len() * 4);
        for (e, t) in mesh.tets.iter().enumerate() {
            for (f, c) in TET_FACES.iter().enumerate() {
                let mut key = [t[c[0]], t[c[1]], t[c[2]]];
                key.sort_unstable();
                map.insert(key, (e as u32, f as u8 + 1));
            }
        }
        Self(map)
    }

    fn find(&self, tri: &[u32; 6]) -> Option<(u32, u8)> {
        let mut key = [tri[0], tri[1], tri[2]];
        key.sort_unstable();
        self.0.get(&key).copied()
    }
}

/// Weights by which a uniform traction spreads over a quadratic triangle's six nodes, and its
/// area. Integrated with shape functions, so curved faces are right too. Corner weights of a flat
/// triangle are exactly zero and mid-side weights a third of its area.
pub fn tri6_weights(nodes: &[[f64; 3]], t: &[u32; 6]) -> ([f64; 6], f64) {
    // A rule exact to degree 4 is enough for the shape function times a linear Jacobian norm.
    const GAUSS: [(f64, f64, f64); 6] = [
        (0.445948490915965, 0.445948490915965, 0.223381589678011),
        (0.445948490915965, 0.108103018168070, 0.223381589678011),
        (0.108103018168070, 0.445948490915965, 0.223381589678011),
        (0.091576213509771, 0.091576213509771, 0.109951743655322),
        (0.091576213509771, 0.816847572980459, 0.109951743655322),
        (0.816847572980459, 0.091576213509771, 0.109951743655322),
    ];
    let p: Vec<[f64; 3]> = t.iter().map(|i| nodes[*i as usize]).collect();
    let mut w = [0.0; 6];
    let mut area = 0.0;
    for (r, s, g) in GAUSS {
        let u = 1.0 - r - s;
        // Node order: corners 0,1,2, then edges 01, 12, 20. u is corner 0, r corner 1, s corner 2.
        let n = [u * (2.0 * u - 1.0), r * (2.0 * r - 1.0), s * (2.0 * s - 1.0), 4.0 * u * r, 4.0 * r * s, 4.0 * s * u];
        let dr = [1.0 - 4.0 * u, 4.0 * r - 1.0, 0.0, 4.0 * (u - r), 4.0 * s, -4.0 * s];
        let ds = [1.0 - 4.0 * u, 0.0, 4.0 * s - 1.0, -4.0 * r, 4.0 * r, 4.0 * (u - s)];
        let mut a = [0.0; 3];
        let mut b = [0.0; 3];
        for i in 0..6 {
            for k in 0..3 {
                a[k] += dr[i] * p[i][k];
                b[k] += ds[i] * p[i][k];
            }
        }
        let c = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        // The weights of this rule sum to 1 over the reference triangle of area 1/2.
        let j = 0.5 * (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        area += g * j;
        for i in 0..6 {
            w[i] += g * j * n[i];
        }
    }
    (w, area)
}

/// The deck's text. Node and element numbers are one more than their index.
pub fn write(m: &Model) -> Result<String> {
    if m.mesh.tets.is_empty() {
        bail!("the mesh has no tetrahedra");
    }
    let mut s = String::with_capacity(m.mesh.nodes.len() * 64 + m.mesh.tets.len() * 96);
    let finder = FaceFinder::new(m.mesh);
    let _ = writeln!(s, "*HEADING\nSense Sim");
    s.push_str("*NODE, NSET=NALL\n");
    for (i, n) in m.mesh.nodes.iter().enumerate() {
        // CalculiX reads at most 20 characters per number: 13 significant digits always fit.
        let _ = writeln!(s, "{},{:.12e},{:.12e},{:.12e}", i + 1, n[0], n[1], n[2]);
    }
    s.push_str("*ELEMENT, TYPE=C3D10, ELSET=EALL\n");
    for (e, t) in m.mesh.tets.iter().enumerate() {
        let a = abaqus_tet(t);
        let _ = writeln!(s, "{},{},{},{},{},{},{},{},{},{},{}", e + 1, a[0] + 1, a[1] + 1, a[2] + 1, a[3] + 1, a[4] + 1, a[5] + 1, a[6] + 1, a[7] + 1, a[8] + 1, a[9] + 1);
    }

    // Node sets, for every group a condition names.
    let mut used = BTreeSet::new();
    for c in &m.conditions {
        match c {
            Condition::Fixed { group } | Condition::Displacement { group, .. } | Condition::Temperature { group, .. } => {
                used.insert(group.as_str());
            }
            _ => {}
        }
    }
    for name in &used {
        let nodes = group_nodes(m, name)?;
        write_id_set(&mut s, &format!("*NSET, NSET={}", set_name(name)), &nodes);
    }

    write_material(&mut s, m)?;
    let _ = writeln!(s, "*SOLID SECTION, ELSET=EALL, MATERIAL={}", set_name(&m.material.name));
    write_contact(&mut s, m, &finder)?;
    if let Some(t) = m.initial_temperature {
        let _ = writeln!(s, "*INITIAL CONDITIONS, TYPE=TEMPERATURE\nNALL,{t:.9e}");
    } else if let Some((_, reference)) = m.prescribed_temperature {
        let _ = writeln!(s, "*INITIAL CONDITIONS, TYPE=TEMPERATURE\nNALL,{reference:.9e}");
    }

    match &m.procedure {
        Procedure::Static { nlgeom, increment } => {
            if *nlgeom {
                s.push_str("*STEP, NLGEOM, INC=2000\n");
            } else {
                s.push_str("*STEP\n");
            }
            let _ = writeln!(s, "*STATIC\n{increment},1.0,1e-8,{increment}");
            // Before the conditions, so that a temperature field in them overrides this.
            if let Some((t, _)) = m.prescribed_temperature {
                let _ = writeln!(s, "*TEMPERATURE\nNALL,{t:.9e}");
            }
            write_conditions(&mut s, m, &finder)?;
            write_reactions(&mut s, m);
            s.push_str("*NODE FILE\nU\n*EL FILE\nS,E\n*END STEP\n");
        }
        Procedure::Frequency { modes } => {
            let _ = writeln!(s, "*STEP\n*FREQUENCY\n{modes}");
            write_kinematic(&mut s, m)?;
            s.push_str("*NODE FILE\nU\n*END STEP\n");
        }
        Procedure::Dynamic { dt, duration } => {
            // Fixed step, so the output is a uniform time history. alpha = 0: no numerical damping.
            let _ = writeln!(s, "*STEP, INC=1000000\n*DYNAMIC, ALPHA=0, DIRECT\n{dt},{duration}");
            write_conditions(&mut s, m, &finder)?;
            s.push_str("*NODE FILE\nU\n*EL FILE, FREQUENCY=10\nS\n*END STEP\n");
        }
        Procedure::Harmonic { modes, f_min, f_max, points, damping } => {
            let _ = writeln!(s, "*STEP\n*FREQUENCY, STORAGE=YES\n{modes}");
            write_kinematic(&mut s, m)?;
            s.push_str("*END STEP\n");
            let _ = writeln!(s, "*STEP\n*STEADY STATE DYNAMICS\n{f_min},{f_max},{points},1.0");
            let _ = writeln!(s, "*MODAL DAMPING\n1,{modes},{damping}");
            write_loads(&mut s, m, &finder)?;
            s.push_str("*NODE FILE\nU\n*END STEP\n");
        }
        Procedure::Heat { transient } => {
            match transient {
                None => s.push_str("*STEP\n*HEAT TRANSFER, STEADY STATE\n1.0,1.0\n"),
                Some((dt, duration)) => {
                    let _ = writeln!(s, "*STEP, INC=1000000\n*HEAT TRANSFER, DIRECT\n{dt},{duration}");
                }
            }
            write_conditions(&mut s, m, &finder)?;
            s.push_str("*NODE FILE\nNT\n*EL FILE\nHFL\n*END STEP\n");
        }
        Procedure::Thermomechanical { transient, .. } => {
            match transient {
                None => s.push_str("*STEP\n*COUPLED TEMPERATURE-DISPLACEMENT, STEADY STATE\n1.0,1.0\n"),
                Some((dt, duration)) => {
                    let _ = writeln!(s, "*STEP, INC=1000000\n*COUPLED TEMPERATURE-DISPLACEMENT, DIRECT\n{dt},{duration}");
                }
            }
            write_conditions(&mut s, m, &finder)?;
            s.push_str("*NODE FILE\nU,NT\n*EL FILE\nS,E,HFL\n*END STEP\n");
        }
    }
    Ok(s)
}

/// Surfaces and the interaction between them. These belong to the model, not to a step.
fn write_contact(s: &mut String, m: &Model, finder: &FaceFinder) -> Result<()> {
    let mut count = 0;
    for c in &m.conditions {
        let Condition::Contact { slave, master, slope, friction } = c else { continue };
        count += 1;
        for (name, group) in [("SLAVE", slave), ("MASTER", master)] {
            let _ = writeln!(s, "*SURFACE, NAME={name}{count}");
            for t in group_tris(m, group)? {
                let (e, f) = finder.find(t).ok_or_else(|| anyhow!("a triangle of {group:?} is on no tetrahedron"))?;
                let _ = writeln!(s, "{},S{}", e + 1, f);
            }
        }
        let _ = writeln!(s, "*SURFACE INTERACTION, NAME=INTERACTION{count}");
        let _ = writeln!(s, "*SURFACE BEHAVIOR, PRESSURE-OVERCLOSURE=LINEAR\n{slope:.9e}");
        if let Some(mu) = friction {
            // The second number is the stick slope, the usual 50 times the normal one's share.
            let _ = writeln!(s, "*FRICTION\n{mu:.6},{:.9e}", slope * 0.02);
        }
        let _ = writeln!(s, "*CONTACT PAIR, INTERACTION=INTERACTION{count}, TYPE=SURFACE TO SURFACE\nSLAVE{count},MASTER{count}");
    }
    Ok(())
}

/// The total reaction on each held group, per increment, for the `.dat` file.
fn write_reactions(s: &mut String, m: &Model) {
    let mut seen = BTreeSet::new();
    for c in &m.conditions {
        if let Condition::Fixed { group } | Condition::Displacement { group, .. } = c
            && seen.insert(group.as_str())
        {
            let _ = writeln!(s, "*NODE PRINT, NSET={}, TOTALS=ONLY\nRF", set_name(group));
        }
    }
}

/// CalculiX set names: at most 80 characters, no commas or spaces.
fn set_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c.to_ascii_uppercase() } else { '_' }).take(80).collect()
}

fn group_nodes(m: &Model, name: &str) -> Result<Vec<u32>> {
    let mut nodes = BTreeSet::new();
    for t in group_tris(m, name)? {
        nodes.extend(t.iter().copied());
    }
    if nodes.is_empty() {
        bail!("face group {name:?} has no mesh on it");
    }
    Ok(nodes.into_iter().collect())
}

fn group_tris<'a>(m: &'a Model, name: &str) -> Result<Vec<&'a [u32; 6]>> {
    let mut out = Vec::new();
    for e in m.groups.entities(name)? {
        let surf = m.mesh.surfaces.iter().find(|s| s.tag == *e).ok_or_else(|| anyhow!("face group {name:?} refers to surface {e}, which is not in the mesh"))?;
        out.extend(surf.tris.iter());
    }
    if out.is_empty() {
        bail!("face group {name:?} has no triangles");
    }
    Ok(out)
}

/// Ids (one more than indices), wrapped at 16 per line as CalculiX asks.
fn write_id_set(s: &mut String, header: &str, ids: &[u32]) {
    s.push_str(header);
    for (i, id) in ids.iter().enumerate() {
        s.push(if i % 16 == 0 { '\n' } else { ',' });
        let _ = write!(s, "{}", id + 1);
    }
    s.push('\n');
}

fn write_material(s: &mut String, m: &Model) -> Result<()> {
    let mat = &m.material;
    let _ = writeln!(s, "*MATERIAL, NAME={}", set_name(&mat.name));
    let _ = writeln!(s, "*ELASTIC\n{:.9e},{:.9e}", mat.young, mat.poisson);
    let _ = writeln!(s, "*DENSITY\n{:.9e}", mat.density);
    let thermal = matches!(m.procedure, Procedure::Heat { .. } | Procedure::Thermomechanical { .. });
    if thermal {
        let k = mat.conductivity.ok_or_else(|| anyhow!("{} has no thermal conductivity", mat.name))?;
        let _ = writeln!(s, "*CONDUCTIVITY\n{k:.9e}");
        let transient = matches!(m.procedure, Procedure::Heat { transient: Some(_) } | Procedure::Thermomechanical { transient: Some(_), .. });
        if transient || mat.specific_heat.is_some() {
            let c = mat.specific_heat.ok_or_else(|| anyhow!("{} has no specific heat", mat.name))?;
            let _ = writeln!(s, "*SPECIFIC HEAT\n{c:.9e}");
        }
    }
    let reference = match (&m.procedure, m.prescribed_temperature) {
        (Procedure::Thermomechanical { reference, .. }, _) => Some(*reference),
        (_, Some((_, reference))) => Some(reference),
        _ => None,
    };
    if let Some(reference) = reference {
        let a = mat.expansion.ok_or_else(|| anyhow!("{} has no thermal expansion", mat.name))?;
        let _ = writeln!(s, "*EXPANSION, ZERO={reference:.9e}\n{a:.9e}");
    }
    if !mat.plastic.is_empty() {
        s.push_str("*PLASTIC\n");
        for (stress, strain) in &mat.plastic {
            let _ = writeln!(s, "{stress:.9e},{strain:.9e}");
        }
    }
    Ok(())
}

/// Boundary conditions and loads.
fn write_conditions(s: &mut String, m: &Model, finder: &FaceFinder) -> Result<()> {
    write_kinematic(s, m)?;
    write_loads(s, m, finder)
}

fn write_kinematic(s: &mut String, m: &Model) -> Result<()> {
    let mut any = false;
    for c in &m.conditions {
        let line = match c {
            Condition::DisplacementAt { nodes, axis, value } => {
                if *axis > 2 {
                    bail!("displacement axis {axis} is not 0, 1 or 2");
                }
                if nodes.is_empty() {
                    bail!("a displacement is set on no nodes");
                }
                nodes.iter().map(|n| format!("{},{},{},{value:.9e}", n + 1, axis + 1, axis + 1)).collect::<Vec<_>>().join("\n")
            }
            Condition::Fixed { group } => format!("{},1,3,0.0", set_name(group)),
            Condition::Displacement { group, axis, value } => {
                if *axis > 2 {
                    bail!("displacement axis {axis} is not 0, 1 or 2");
                }
                format!("{},{},{},{value:.9e}", set_name(group), axis + 1, axis + 1)
            }
            Condition::Temperature { group, value } => format!("{},11,11,{value:.9e}", set_name(group)),
            _ => continue,
        };
        if !any {
            s.push_str("*BOUNDARY\n");
            any = true;
        }
        let _ = writeln!(s, "{line}");
    }
    Ok(())
}

fn write_loads(s: &mut String, m: &Model, finder: &FaceFinder) -> Result<()> {
    for c in &m.conditions {
        match c {
            Condition::Force { group, force } => {
                let tris = group_tris(m, group)?;
                let mut weights: HashMap<u32, f64> = HashMap::new();
                let mut total = 0.0;
                for t in &tris {
                    let (w, area) = tri6_weights(&m.mesh.nodes, t);
                    total += area;
                    for (n, wi) in t.iter().zip(w) {
                        *weights.entry(*n).or_default() += wi;
                    }
                }
                if total <= 0.0 {
                    bail!("face group {group:?} has no area");
                }
                let mut rows: Vec<(u32, f64)> = weights.into_iter().collect();
                rows.sort_unstable_by_key(|r| r.0);
                s.push_str("*CLOAD\n");
                for (n, w) in rows {
                    for (axis, f) in force.iter().enumerate() {
                        if *f != 0.0 {
                            let _ = writeln!(s, "{},{},{:.12e}", n + 1, axis + 1, f * w / total);
                        }
                    }
                }
            }
            Condition::Pressure { group, pressure } => {
                s.push_str("*DLOAD\n");
                for t in group_tris(m, group)? {
                    let (e, f) = finder.find(t).ok_or_else(|| anyhow!("a triangle of {group:?} is on no tetrahedron"))?;
                    let _ = writeln!(s, "{},P{},{:.9e}", e + 1, f, pressure);
                }
            }
            Condition::Gravity { acceleration } => {
                let mag = (acceleration[0].powi(2) + acceleration[1].powi(2) + acceleration[2].powi(2)).sqrt();
                if mag > 0.0 {
                    let _ = writeln!(s, "*DLOAD\nEALL,GRAV,{mag:.9e},{:.9e},{:.9e},{:.9e}", acceleration[0] / mag, acceleration[1] / mag, acceleration[2] / mag);
                }
            }
            Condition::HeatFlux { group, flux } => {
                s.push_str("*DFLUX\n");
                for t in group_tris(m, group)? {
                    let (e, f) = finder.find(t).ok_or_else(|| anyhow!("a triangle of {group:?} is on no tetrahedron"))?;
                    let _ = writeln!(s, "{},S{},{:.9e}", e + 1, f, flux);
                }
            }
            Condition::Convection { group, coefficient, ambient } => {
                s.push_str("*FILM\n");
                for t in group_tris(m, group)? {
                    let (e, f) = finder.find(t).ok_or_else(|| anyhow!("a triangle of {group:?} is on no tetrahedron"))?;
                    let _ = writeln!(s, "{},F{},{:.9e},{:.9e}", e + 1, f, ambient, coefficient);
                }
            }
            Condition::HeatSource { power } => {
                let _ = writeln!(s, "*DFLUX\nEALL,BF,{power:.9e}");
            }
            Condition::TemperatureField { values } => {
                if values.len() != m.mesh.nodes.len() {
                    bail!("a temperature field has {} values for {} nodes", values.len(), m.mesh.nodes.len());
                }
                s.push_str("*TEMPERATURE\n");
                for (n, t) in values.iter().enumerate() {
                    let _ = writeln!(s, "{},{t:.9e}", n + 1);
                }
            }
            Condition::Fixed { .. } | Condition::Displacement { .. } | Condition::DisplacementAt { .. } | Condition::Temperature { .. } | Condition::Contact { .. } => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::msh::SurfaceEntity;

    fn one_tet() -> TetMesh {
        TetMesh {
            nodes: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [0.5, 0., 0.], [0.5, 0.5, 0.], [0., 0.5, 0.], [0., 0., 0.5], [0., 0.5, 0.5], [0.5, 0., 0.5]],
            tets: vec![[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]],
            tet_volume: vec![1],
            surfaces: vec![SurfaceEntity { tag: 7, tris: vec![[0, 1, 2, 4, 5, 6]] }],
            ..Default::default()
        }
    }

    fn steel() -> Material {
        Material { name: "steel".into(), young: 210e9, poisson: 0.3, density: 7850.0, conductivity: Some(50.0), specific_heat: Some(480.0), expansion: Some(12e-6), plastic: vec![] }
    }

    #[test]
    fn abaqus_order_puts_each_midside_node_on_its_edge() {
        let m = one_tet();
        let a = abaqus_tet(&m.tets[0]);
        // Abaqus C3D10, 0-based: 4=01, 5=12, 6=20, 7=03, 8=13, 9=23.
        for (mid, (p, q)) in [(4, (0, 1)), (5, (1, 2)), (6, (2, 0)), (7, (0, 3)), (8, (1, 3)), (9, (2, 3))] {
            assert_eq!(m.nodes[a[mid] as usize], m.midpoint(a[p], a[q]), "mid-side node {mid}");
        }
    }

    #[test]
    fn flat_triangle_weights_are_all_on_midside_nodes() {
        let m = one_tet();
        let (w, area) = tri6_weights(&m.nodes, &m.surfaces[0].tris[0]);
        assert!((area - 0.5).abs() < 1e-12);
        for corner in &w[..3] {
            assert!(corner.abs() < 1e-12, "{corner}");
        }
        for mid in &w[3..] {
            assert!((mid - 0.5 / 3.0).abs() < 1e-12, "{mid}");
        }
    }

    #[test]
    fn a_triangle_finds_the_face_it_is_on() {
        let m = one_tet();
        let f = FaceFinder::new(&m);
        assert_eq!(f.find(&[0, 1, 2, 4, 5, 6]), Some((0, 1)));
        assert_eq!(f.find(&[2, 1, 0, 5, 4, 6]), Some((0, 1)), "winding does not matter");
        assert_eq!(f.find(&[0, 3, 1, 7, 9, 4]), Some((0, 2)));
        assert_eq!(f.find(&[0, 1, 9, 4, 5, 6]), None);
    }

    #[test]
    fn deck_refuses_missing_groups_and_materials() {
        let mesh = one_tet();
        let mut m = Model { mesh: &mesh, material: steel(), groups: Groups::default(), conditions: vec![Condition::Fixed { group: "nope".into() }], procedure: Procedure::Static { nlgeom: false, increment: 1.0 }, initial_temperature: None, prescribed_temperature: None };
        assert!(write(&m).unwrap_err().to_string().contains("nope"));
        m.conditions.clear();
        m.material.conductivity = None;
        m.procedure = Procedure::Heat { transient: None };
        assert!(write(&m).unwrap_err().to_string().contains("conductivity"));
    }

    #[test]
    fn every_number_fits_calculix_field_width() {
        let mut mesh = one_tet();
        mesh.nodes[0] = [-3.641531520800000e-17, 1.030000000000000e-1, -9.999999999999999e-100];
        let m = Model { mesh: &mesh, material: steel(), groups: Groups::default(), conditions: vec![], procedure: Procedure::Frequency { modes: 1 }, initial_temperature: None, prescribed_temperature: None };
        let text = write(&m).unwrap();
        let node = text.lines().find(|l| l.starts_with("1,")).unwrap();
        for field in node.split(',') {
            assert!(field.len() <= 20, "{field:?} is {} characters", field.len());
        }
    }

    #[test]
    fn set_names_are_safe_for_the_format() {
        assert_eq!(set_name("Left face, top"), "LEFT_FACE__TOP");
    }
}
