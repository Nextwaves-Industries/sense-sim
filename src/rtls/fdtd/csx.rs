//! openEMS input: geometry (CSXCAD properties with box primitives), a
//! rectilinear mesh with lines on every edge, and the FDTD settings, written
//! as the XML `openEMS.exe` reads. Units are metres (`DeltaUnit = 1`).

use std::fmt::Write;

pub const C0: f64 = 299_792_458.0;

pub type P3 = [f64; 3];

/// An axis-aligned box (may be flat or a line along some axes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bx {
    pub min: P3,
    pub max: P3,
}

impl Bx {
    pub fn new(a: P3, b: P3) -> Self {
        Self { min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])], max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])] }
    }
}

/// A lossy dielectric (relative permittivity, conductivity in S/m).
#[derive(Clone, Debug, PartialEq)]
pub struct Dielectric {
    pub name: String,
    pub eps_r: f64,
    pub kappa: f64,
    pub boxes: Vec<Bx>,
}

/// Lumped port across `gap` along axis `dir` (0 = x, 1 = y, 2 = z).
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub nr: u32,
    pub gap: Bx,
    pub dir: usize,
    pub r_ohm: f64,
    pub excite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpKind {
    /// Frequency-domain E field (`DumpType 10`).
    EFreq,
    /// Frequency-domain H field (`DumpType 11`).
    HFreq,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dump {
    pub name: String,
    pub kind: DumpKind,
    /// Frequencies sampled (one file per frequency).
    pub freqs: Vec<f64>,
    pub region: Bx,
}

/// Everything one openEMS run needs.
#[derive(Clone, Debug, Default)]
pub struct Sim {
    /// Gaussian pulse: centre and 20 dB half bandwidth (Hz).
    pub f0: f64,
    pub fc: f64,
    pub metal: Vec<Bx>,
    pub dielectrics: Vec<Dielectric>,
    pub ports: Vec<Port>,
    pub dumps: Vec<Dump>,
    pub mesh: [Vec<f64>; 3],
    /// Energy decay that ends the run (1e-4 = -40 dB).
    pub end_criteria: f64,
    pub max_timesteps: u64,
}

impl Sim {
    pub fn f_max(&self) -> f64 {
        self.f0 + self.fc
    }

    pub fn cells(&self) -> u64 {
        self.mesh.iter().map(|m| m.len().saturating_sub(1).max(1) as u64).product()
    }

    /// Rough run time at a typical desktop speed (cells × steps / 80 MC/s).
    pub fn estimate_s(&self) -> f64 {
        let min_cell = self
            .mesh
            .iter()
            .flat_map(|m| m.windows(2).map(|w| w[1] - w[0]))
            .fold(f64::MAX, f64::min)
            .max(1e-6);
        let dt = min_cell / (C0 * 3f64.sqrt());
        // A Gaussian pulse plus ringing: ~ 12 pulse widths, more for resonant structures.
        let steps = (12.0 / (self.fc.max(1.0) * dt)).min(self.max_timesteps as f64);
        self.cells() as f64 * steps / 80e6
    }

    pub fn to_xml(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?>"#);
        let _ = writeln!(s, "<openEMS>");
        let _ = writeln!(s, r#"  <FDTD NumberOfTimesteps="{}" endCriteria="{:e}" f_max="{:e}">"#, self.max_timesteps, self.end_criteria, self.f_max());
        let _ = writeln!(s, r#"    <Excitation Type="0" f0="{:e}" fc="{:e}"/>"#, self.f0, self.fc);
        let _ = writeln!(s, r#"    <BoundaryCond xmin="PML_8" xmax="PML_8" ymin="PML_8" ymax="PML_8" zmin="PML_8" zmax="PML_8"/>"#);
        let _ = writeln!(s, "  </FDTD>");
        let _ = writeln!(s, r#"  <ContinuousStructure CoordSystem="0">"#);
        let _ = writeln!(s, "    <Properties>");
        if !self.metal.is_empty() {
            let _ = writeln!(s, r#"      <Metal Name="metal">"#);
            boxes(&mut s, &self.metal, 10);
            let _ = writeln!(s, "      </Metal>");
        }
        for d in &self.dielectrics {
            let _ = writeln!(s, r#"      <Material Name="{}">"#, xml_escape(&d.name));
            let _ = writeln!(s, r#"        <Property Epsilon="{}" Kappa="{:e}"/>"#, d.eps_r, d.kappa);
            boxes(&mut s, &d.boxes, 5);
            let _ = writeln!(s, "      </Material>");
        }
        for p in &self.ports {
            let n = p.nr;
            let mut e = [0i32; 3];
            // As AddLumpedPort: excite against the port direction.
            let direction = if p.gap.max[p.dir] >= p.gap.min[p.dir] { 1 } else { -1 };
            e[p.dir] = -direction;
            let _ = writeln!(s, r#"      <LumpedElement Name="port_resist_{n}" Direction="{}" Caps="1" R="{}">"#, p.dir, p.r_ohm);
            boxes(&mut s, &[p.gap], 20);
            let _ = writeln!(s, "      </LumpedElement>");
            if p.excite {
                let _ = writeln!(s, r#"      <Excitation Name="port_excite_{n}" Type="0" Excite="{},{},{}">"#, e[0], e[1], e[2]);
                boxes(&mut s, &[p.gap], 20);
                let _ = writeln!(s, "      </Excitation>");
            }
            // Voltage along the gap through its centre; current through the mid plane.
            let c = [(p.gap.min[0] + p.gap.max[0]) * 0.5, (p.gap.min[1] + p.gap.max[1]) * 0.5, (p.gap.min[2] + p.gap.max[2]) * 0.5];
            let (mut ua, mut ub) = (c, c);
            ua[p.dir] = p.gap.min[p.dir];
            ub[p.dir] = p.gap.max[p.dir];
            let _ = writeln!(s, r#"      <ProbeBox Name="port_ut{n}" Type="0" Weight="-1">"#);
            boxes(&mut s, &[Bx { min: ua, max: ub }], 20);
            let _ = writeln!(s, "      </ProbeBox>");
            let (mut ia, mut ib) = (p.gap.min, p.gap.max);
            ia[p.dir] = c[p.dir];
            ib[p.dir] = c[p.dir];
            let _ = writeln!(s, r#"      <ProbeBox Name="port_it{n}" Type="1" Weight="1" NormDir="{}">"#, p.dir);
            boxes(&mut s, &[Bx { min: ia, max: ib }], 20);
            let _ = writeln!(s, "      </ProbeBox>");
        }
        for d in &self.dumps {
            let t = match d.kind {
                DumpKind::EFreq => 10,
                DumpKind::HFreq => 11,
            };
            let _ = writeln!(s, r#"      <DumpBox Name="{}" DumpType="{t}" DumpMode="1" FileType="0">"#, xml_escape(&d.name));
            let f: Vec<String> = d.freqs.iter().map(|f| format!("{f:e}")).collect();
            let _ = writeln!(s, "        <FD_Samples>{}</FD_Samples>", f.join(","));
            boxes(&mut s, &[d.region], 0);
            let _ = writeln!(s, "      </DumpBox>");
        }
        let _ = writeln!(s, "    </Properties>");
        let _ = writeln!(s, r#"    <RectilinearGrid DeltaUnit="1" CoordSystem="0">"#);
        for (tag, m) in ["XLines", "YLines", "ZLines"].iter().zip(&self.mesh) {
            let v: Vec<String> = m.iter().map(|x| format!("{x:.9}")).collect();
            let _ = writeln!(s, "      <{tag}>{}</{tag}>", v.join(","));
        }
        let _ = writeln!(s, "    </RectilinearGrid>");
        let _ = writeln!(s, "  </ContinuousStructure>");
        let _ = writeln!(s, "</openEMS>");
        s
    }
}

fn boxes(s: &mut String, bs: &[Bx], prio: i32) {
    let _ = writeln!(s, "        <Primitives>");
    for b in bs {
        let _ = writeln!(
            s,
            r#"          <Box Priority="{prio}"><P1 X="{:.9}" Y="{:.9}" Z="{:.9}"/><P2 X="{:.9}" Y="{:.9}" Z="{:.9}"/></Box>"#,
            b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]
        );
    }
    let _ = writeln!(s, "        </Primitives>");
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Mesh lines along one axis: every `fixed` line (box edges, port gaps) is kept,
/// gaps are filled so no cell exceeds `max_cell`, and neighbouring cells grow by
/// at most `ratio` (openEMS gets inaccurate with abrupt cell size changes).
pub fn mesh_axis(fixed: &[f64], lo: f64, hi: f64, max_cell: f64, ratio: f64) -> Vec<f64> {
    let mut f: Vec<f64> = fixed.iter().copied().filter(|x| (lo..=hi).contains(x)).chain([lo, hi]).collect();
    f.sort_by(f64::total_cmp);
    let tiny = max_cell * 1e-3;
    f.dedup_by(|a, b| (*a - *b).abs() < tiny);
    if f.len() < 2 {
        return f;
    }
    // Size of the smallest cell touching each fixed line (fixed spacing near it).
    let mut out = vec![f[0]];
    let mut near: Vec<f64> = (0..f.len())
        .map(|i| {
            let l = if i > 0 { f[i] - f[i - 1] } else { f64::MAX };
            let r = if i + 1 < f.len() { f[i + 1] - f[i] } else { f64::MAX };
            l.min(r).min(max_cell)
        })
        .collect();
    for i in 0..f.len() - 1 {
        let (a, b) = (f[i], f[i + 1]);
        out.extend(fill_gap(a, b, near[i], near[i + 1], max_cell, ratio));
        // The next gap has to grow from the cell this one really ended on.
        near[i + 1] = near[i + 1].min(b - out[out.len() - 1]);
        out.push(b);
    }
    out
}

/// Lines inside a gap, so no cell exceeds `max_cell` and each grows from its neighbour by
/// at most `ratio`: `ca` and `cb` are the finest cells touching its two ends. It steps from
/// the finer end towards `max_cell`, then fills what's left evenly.
fn fill_gap(a: f64, b: f64, ca: f64, cb: f64, max_cell: f64, ratio: f64) -> Vec<f64> {
    let (mut pa, mut pb, mut sl, mut sr) = (a, b, ca, cb);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut middle = Vec::new();
    for _ in 0..1024 {
        let rest = pb - pa;
        let (nl, nr) = ((sl * ratio).min(max_cell), (sr * ratio).min(max_cell));
        if rest <= nl.min(nr) * 1.0001 {
            break;
        }
        if sl >= max_cell * 0.9999 && sr >= max_cell * 0.9999 {
            // Barely over one cell would halve into cells too small beside full ones:
            // take a full neighbour in and share it out.
            if rest < ratio * max_cell {
                if left.pop().is_some() {
                    pa = left.last().copied().unwrap_or(a);
                } else if right.pop().is_some() {
                    pb = right.last().copied().unwrap_or(b);
                }
            }
            let rest = pb - pa;
            // Tolerance: a gap of exactly k cells (rounding aside) stays k cells.
            let n = (rest / max_cell - 1e-6).ceil().max(1.0) as usize;
            middle = (1..n).map(|k| pa + rest * k as f64 / n as f64).collect();
            break;
        }
        // Two equal cells rather than leaving a sliver.
        if sl <= sr {
            let step = if rest < 2.0 * nl { rest / 2.0 } else { nl };
            pa += step;
            left.push(pa);
            sl = step;
        } else {
            let step = if rest < 2.0 * nr { rest / 2.0 } else { nr };
            pb -= step;
            right.push(pb);
            sr = step;
        }
    }
    left.extend(middle);
    left.extend(right.into_iter().rev());
    left
}

/// Builds the three mesh axes for a domain, keeping every primitive edge.
pub fn auto_mesh(sim: &Sim, domain: Bx, max_cell: f64) -> [Vec<f64>; 3] {
    auto_mesh_fine(sim, domain, max_cell, None)
}

/// As `auto_mesh`, with cells of at most `fine.1` inside the box `fine.0` (the
/// antenna: metal edges shift by up to half a cell, so the structure itself
/// must be resolved much finer than the wavelength), graded outwards.
pub fn auto_mesh_fine(sim: &Sim, domain: Bx, max_cell: f64, fine: Option<(Bx, f64)>) -> [Vec<f64>; 3] {
    let mut fixed: [Vec<f64>; 3] = Default::default();
    let mut add = |b: &Bx| {
        for k in 0..3 {
            fixed[k].push(b.min[k]);
            fixed[k].push(b.max[k]);
        }
    };
    sim.metal.iter().for_each(&mut add);
    sim.dielectrics.iter().flat_map(|d| d.boxes.iter()).for_each(&mut add);
    for p in &sim.ports {
        add(&p.gap);
        // The probes sit on the gap centre: make it a mesh line too.
        let mut c = p.gap;
        for k in 0..3 {
            c.min[k] = (p.gap.min[k] + p.gap.max[k]) * 0.5;
            c.max[k] = c.min[k];
        }
        add(&c);
    }
    if let Some((b, cell)) = fine {
        for k in 0..3 {
            let n = ((b.max[k] - b.min[k]) / cell).ceil() as usize;
            for i in 0..=n {
                fixed[k].push(b.min[k] + (b.max[k] - b.min[k]) * i as f64 / n.max(1) as f64);
            }
        }
    }
    std::array::from_fn(|k| mesh_axis(&fixed[k], domain.min[k], domain.max[k], max_cell, 1.4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_keeps_edges_and_bounds_cells() {
        let m = mesh_axis(&[-0.0005, 0.0005, 0.0105, -0.0105], -0.04, 0.04, 0.002, 1.4);
        for e in [-0.0005, 0.0005, 0.0105, -0.0105, -0.04, 0.04] {
            assert!(m.iter().any(|x| (x - e).abs() < 1e-12), "edge {e} missing");
        }
        let cells: Vec<f64> = m.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(cells.iter().all(|c| *c > 0.0 && *c <= 0.002 * 1.0001));
        // Smooth: no neighbour more than ~1.5x larger.
        for w in cells.windows(2) {
            let r = w[1].max(w[0]) / w[1].min(w[0]);
            assert!(r < 1.6, "ratio {r}");
        }
    }

    #[test]
    fn mesh_grades_a_gap_no_wider_than_a_cell() {
        // A 0.4 mm feature beside a 10 mm gap, with cells of up to 12 mm: the audit's 31:1.
        let m = mesh_axis(&[0.0, 0.0004, 0.0104], -0.05, 0.05, 0.012, 1.4);
        for e in [0.0, 0.0004, 0.0104] {
            assert!(m.iter().any(|x| (x - e).abs() < 1e-12), "edge {e} missing");
        }
        let cells: Vec<f64> = m.windows(2).map(|w| w[1] - w[0]).collect();
        for w in cells.windows(2) {
            let r = w[1].max(w[0]) / w[1].min(w[0]);
            assert!(r < 1.6, "ratio {r}: {cells:?}");
        }
    }

    #[test]
    fn xml_has_port_and_probes() {
        let mut sim = Sim { f0: 6.5e9, fc: 3.5e9, end_criteria: 1e-4, max_timesteps: 10000, ..Default::default() };
        sim.ports.push(Port { nr: 1, gap: Bx::new([-0.0005, 0.0, -0.0005], [0.0005, 0.0, 0.0005]), dir: 2, r_ohm: 73.0, excite: true });
        let x = sim.to_xml();
        assert!(x.contains(r#"<Excitation Name="port_excite_1" Type="0" Excite="0,0,-1">"#));
        assert!(x.contains(r#"<ProbeBox Name="port_ut1" Type="0" Weight="-1">"#));
        assert!(x.contains(r#"<ProbeBox Name="port_it1" Type="1" Weight="1" NormDir="2">"#));
    }
}
