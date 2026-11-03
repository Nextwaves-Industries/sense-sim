//! Parametric antennas for the RTLS bands, as openEMS geometry: a strip
//! dipole, a quarter-wave monopole on a ground plane and a probe-fed patch.
//! The antenna's main axis is z.

use super::csx::{Bx, C0, Dielectric, Dump, DumpKind, Port, Sim};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Band {
    /// UWB channel 5 (6.4896 GHz, 499.2 MHz wide).
    #[default]
    Uwb5,
    /// UWB channel 9 (7.9872 GHz).
    Uwb9,
    /// BLE / 2.4 GHz ISM.
    Ble,
    /// UHF RFID (865-928 MHz).
    Rfid,
    /// WiFi at 5 GHz (U-NII-1 and 2).
    Wifi5,
}

impl Band {
    pub const ALL: [Band; 5] = [Band::Uwb5, Band::Uwb9, Band::Ble, Band::Rfid, Band::Wifi5];

    pub fn label(self) -> &'static str {
        match self {
            Band::Uwb5 => "UWB ch5 · 6.5 GHz",
            Band::Uwb9 => "UWB ch9 · 8.0 GHz",
            Band::Ble => "BLE / WiFi · 2.44 GHz",
            Band::Rfid => "UHF RFID · 915 MHz",
            Band::Wifi5 => "WiFi · 5.25 GHz",
        }
    }

    /// Design (centre) frequency, Hz.
    pub fn center(self) -> f64 {
        match self {
            Band::Uwb5 => 6.4896e9,
            Band::Uwb9 => 7.9872e9,
            Band::Ble => 2.44e9,
            Band::Rfid => 0.915e9,
            Band::Wifi5 => 5.25e9,
        }
    }

    /// Channel / band edges, Hz (shaded on the S11 plot).
    pub fn channel(self) -> (f64, f64) {
        match self {
            Band::Uwb5 => (6.24e9, 6.74e9),
            Band::Uwb9 => (7.74e9, 8.24e9),
            Band::Ble => (2.40e9, 2.4835e9),
            Band::Rfid => (0.865e9, 0.928e9),
            Band::Wifi5 => (5.15e9, 5.35e9),
        }
    }

    /// The band this frequency falls in, if any is simulated.
    pub fn at_mhz(f: f32) -> Option<Band> {
        let f = f as f64 * 1e6;
        Self::ALL.into_iter().find(|b| (b.channel().0..=b.channel().1).contains(&f))
    }

    /// Sweep for antenna runs: wide enough to see the resonance move.
    pub fn sweep(self) -> (f64, f64) {
        let f = self.center();
        (f * 0.5, f * 1.5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Dipole,
    Monopole,
    Patch,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Dipole, Kind::Monopole, Kind::Patch];
    pub fn label(self) -> &'static str {
        match self {
            Kind::Dipole => "Half-wave dipole",
            Kind::Monopole => "Monopole on ground plane",
            Kind::Patch => "Patch (probe fed)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    Fast,
    #[default]
    Normal,
    Fine,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Fast, Quality::Normal, Quality::Fine];
    pub fn label(self) -> &'static str {
        match self {
            Quality::Fast => "Fast (λ/12)",
            Quality::Normal => "Normal (λ/20)",
            Quality::Fine => "Fine (λ/30)",
        }
    }
    pub fn cells_per_lambda(self) -> f64 {
        match self {
            Quality::Fast => 12.0,
            Quality::Normal => 20.0,
            Quality::Fine => 30.0,
        }
    }
}

/// Antenna design: kind, band and the main dimensions (metres), editable.
#[derive(Clone, Debug, PartialEq)]
pub struct Design {
    pub kind: Kind,
    pub band: Band,
    /// Dipole total length / monopole height / patch length.
    pub length: f64,
    /// Strip width / patch width.
    pub width: f64,
    /// Feed gap (dipole) or substrate height (patch).
    pub gap: f64,
    /// Patch substrate permittivity.
    pub eps_r: f64,
    /// Patch substrate loss tangent at the band centre.
    pub tan_d: f64,
    /// Patch probe offset from the centre along the length.
    pub feed: f64,
    pub quality: Quality,
}

impl Design {
    /// Textbook starting dimensions for a kind and band.
    pub fn tuned(kind: Kind, band: Band, quality: Quality) -> Self {
        let f = band.center();
        let lambda = C0 / f;
        match kind {
            Kind::Dipole => Design { kind, band, length: 0.47 * lambda, width: lambda / 50.0, gap: lambda / 60.0, eps_r: 1.0, tan_d: 0.0, feed: 0.0, quality },
            Kind::Monopole => Design { kind, band, length: 0.235 * lambda, width: lambda / 50.0, gap: lambda / 60.0, eps_r: 1.0, tan_d: 0.0, feed: 0.0, quality },
            Kind::Patch => {
                // Thin FR4 above 2 GHz, foam for UHF RFID reader patches.
                let (eps_r, tan_d, h): (f64, f64, f64) = if f > 2e9 { (4.3, 0.02, (lambda * 0.02).clamp(0.0008, 0.0016)) } else { (1.07, 0.0005, 0.008) };
                let w = C0 / (2.0 * f) * (2.0 / (eps_r + 1.0)).sqrt();
                let eeff = (eps_r + 1.0) / 2.0 + (eps_r - 1.0) / 2.0 / (1.0 + 12.0 * h / w).sqrt();
                let dl = 0.412 * h * (eeff + 0.3) * (w / h + 0.264) / ((eeff - 0.258) * (w / h + 0.8));
                let l = C0 / (2.0 * f * eeff.sqrt()) - 2.0 * dl;
                Design { kind, band, length: l, width: w, gap: h, eps_r, tan_d, feed: l / 6.0, quality }
            }
        }
    }

    pub fn port_ohm(&self) -> f64 {
        match self.kind {
            Kind::Dipole => 73.0,
            _ => 50.0,
        }
    }

    /// Antenna geometry, ports, NF2FF and field-slice dumps and the mesh.
    pub fn sim(&self) -> Sim {
        let (fa, fb) = self.band.sweep();
        // -50 dB, as Antenna Studio: at -40 dB the energy left alone is enough to blur a deep match.
        let mut sim = Sim { f0: (fa + fb) / 2.0, fc: (fb - fa) / 2.0, end_criteria: 1e-5, max_timesteps: 200_000, ..Default::default() };
        let lambda_min = C0 / fb;
        let max_cell = lambda_min / self.quality.cells_per_lambda() / self.eps_r.sqrt();
        let (w, g) = (self.width, self.gap);
        let body: Bx = match self.kind {
            Kind::Dipole => {
                let arm = (self.length - g) / 2.0;
                sim.metal.push(Bx::new([-w / 2.0, 0.0, g / 2.0], [w / 2.0, 0.0, g / 2.0 + arm]));
                sim.metal.push(Bx::new([-w / 2.0, 0.0, -g / 2.0 - arm], [w / 2.0, 0.0, -g / 2.0]));
                sim.ports.push(Port { nr: 1, gap: Bx::new([-w / 2.0, 0.0, -g / 2.0], [w / 2.0, 0.0, g / 2.0]), dir: 2, r_ohm: self.port_ohm(), excite: true });
                Bx::new([-w, -w, -self.length / 2.0], [w, w, self.length / 2.0])
            }
            Kind::Monopole => {
                let lambda0 = C0 / self.band.center();
                let gp = 0.6 * lambda0;
                sim.metal.push(Bx::new([-gp / 2.0, -gp / 2.0, 0.0], [gp / 2.0, gp / 2.0, 0.0]));
                sim.metal.push(Bx::new([-w / 2.0, 0.0, g], [w / 2.0, 0.0, g + self.length]));
                sim.ports.push(Port { nr: 1, gap: Bx::new([-w / 2.0, 0.0, 0.0], [w / 2.0, 0.0, g]), dir: 2, r_ohm: self.port_ohm(), excite: true });
                Bx::new([-gp / 2.0, -gp / 2.0, 0.0], [gp / 2.0, gp / 2.0, g + self.length])
            }
            Kind::Patch => {
                let (l, h) = (self.length, self.gap);
                // Ground plane: patch + 6 substrate heights, at least 1.3× the patch.
                let gx = (w + 6.0 * h).max(1.3 * w);
                let gy = (l + 6.0 * h).max(1.3 * l);
                sim.metal.push(Bx::new([-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, 0.0]));
                sim.metal.push(Bx::new([-w / 2.0, -l / 2.0, h], [w / 2.0, l / 2.0, h]));
                if self.eps_r > 1.0001 {
                    // κ = ω·ε₀·εr·tan δ: exact at the band centre.
                    let kappa = 2.0 * std::f64::consts::PI * self.band.center() * 8.854_187_8e-12 * self.eps_r * self.tan_d;
                    sim.dielectrics.push(Dielectric { name: "substrate".into(), eps_r: self.eps_r, kappa, boxes: vec![Bx::new([-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, h])] });
                }
                let pw = (max_cell * 0.5).min(h / 2.0);
                sim.ports.push(Port { nr: 1, gap: Bx::new([-pw / 2.0, self.feed - pw / 2.0, 0.0], [pw / 2.0, self.feed + pw / 2.0, h]), dir: 2, r_ohm: self.port_ohm(), excite: true });
                Bx::new([-gx / 2.0, -gy / 2.0, 0.0], [gx / 2.0, gy / 2.0, h])
            }
        };
        // Room for the near field: λ/4 at the centre frequency outside the NF2FF box,
        // which sits clear of the 8-cell PML.
        let lambda0 = C0 / self.band.center();
        let nf_pad = lambda0 / 4.0;
        let nf = Bx::new(body.min.map(|v| v - nf_pad), body.max.map(|v| v + nf_pad));
        let pml_pad = 12.0 * max_cell;
        let domain = Bx::new(nf.min.map(|v| v - pml_pad), nf.max.map(|v| v + pml_pad));
        let fc = self.band.center();
        for (face, axis, sign) in super::post::FACES {
            let mut b = nf;
            if sign < 0.0 {
                b.max[axis] = b.min[axis];
            } else {
                b.min[axis] = b.max[axis];
            }
            sim.dumps.push(Dump { name: format!("nf2ff_E_{face}"), kind: DumpKind::EFreq, freqs: vec![fc], region: b });
            sim.dumps.push(Dump { name: format!("nf2ff_H_{face}"), kind: DumpKind::HFreq, freqs: vec![fc], region: b });
        }
        // Field slice through the antenna (the plane containing its axis).
        let mut slice = Bx::new(nf.min, nf.max);
        slice.min[1] = 0.0;
        slice.max[1] = 0.0;
        sim.dumps.push(Dump { name: "slice".into(), kind: DumpKind::EFreq, freqs: vec![fc], region: slice });
        // Make the NF2FF faces and the slice plane mesh lines.
        let mut with_nf = sim.clone();
        with_nf.metal.push(nf);
        with_nf.metal.push(Bx::new([0.0; 3], [0.0; 3]));
        // Resolve the radiator itself: its main dimension over ≥ 24 cells (more for finer quality).
        let feature = match self.kind {
            Kind::Dipole | Kind::Monopole => self.length,
            Kind::Patch => self.length.min(self.width),
        };
        let fine_cell = (feature / (self.quality.cells_per_lambda() * 2.0)).min(max_cell);
        let radiator = match self.kind {
            Kind::Patch => Bx::new([-w / 2.0, -self.length / 2.0, 0.0], [w / 2.0, self.length / 2.0, g]),
            Kind::Monopole => Bx::new([-w / 2.0, -w / 2.0, 0.0], [w / 2.0, w / 2.0, g + self.length]),
            Kind::Dipole => Bx::new([-w / 2.0, -w / 2.0, -self.length / 2.0], [w / 2.0, w / 2.0, self.length / 2.0]),
        };
        sim.mesh = super::csx::auto_mesh_fine(&with_nf, domain, max_cell, Some((radiator, fine_cell)));
        sim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designs_have_sane_size_and_cell_budget() {
        for band in Band::ALL {
            for kind in Kind::ALL {
                let d = Design::tuned(kind, band, Quality::Normal);
                let s = d.sim();
                assert!(s.cells() > 1000, "{kind:?} {band:?} too coarse");
                assert!(s.cells() < 30_000_000, "{kind:?} {band:?}: {} cells", s.cells());
                assert_eq!(s.ports.len(), 1);
                assert_eq!(s.dumps.len(), 13);
            }
        }
        let p = Design::tuned(Kind::Patch, Band::Ble, Quality::Normal);
        // FR-4 is lossy: tan δ 0.02 at the design frequency, as ω·ε₀·εr·tan δ.
        let want = 2.0 * std::f64::consts::PI * 2.44e9 * 8.854e-12 * 4.3 * 0.02;
        let k = p.sim().dielectrics[0].kappa;
        assert!((k / want - 1.0).abs() < 0.01, "kappa {k} vs {want}");
        // 2.44 GHz FR4 patch ≈ 29 mm long, 38 mm wide.
        assert!((0.026..0.032).contains(&p.length), "{}", p.length);
        assert!((0.034..0.042).contains(&p.width), "{}", p.width);
    }
}
