//! UHF RFID tag chips, and how well an antenna delivers power to one.
//!
//! A tag isn't matched to 50 Ω. Its chip is strongly capacitive, so the antenna must be inductive
//! and match the chip's complex conjugate. S11 against a real impedance says nothing about that, so
//! a tag is judged by the share of the power it collects that reaches the chip.

use std::f64::consts::PI;

use crate::C64;

/// A tag chip. Every number here comes from where `source` says, and one that isn't published
/// stays `None`: a design can't honestly match an impedance someone guessed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagChip {
    pub id: &'static str,
    pub name: &'static str,
    /// Read sensitivity, dBm.
    pub read_dbm: Option<f64>,
    pub write_dbm: Option<f64>,
    /// What the antenna must be the conjugate of, in series form (R, X), near 915 MHz.
    pub z: Option<(f64, f64)>,
    pub source: &'static str,
    /// What to know before using these figures.
    pub note: &'static str,
}

const UCODE9: &str = "NXP SL3S1206 UCODE 9 data sheet, rev. 3.5, Table 11";

pub const CHIPS: [TagChip; 7] = [
    TagChip { id: "ucode9", name: "NXP UCODE 9, blade diced", read_dbm: Some(-24.0), write_dbm: Some(-22.0), z: Some((15.0, -231.0)), source: UCODE9, note: "Typical assembled impedance at 915 MHz, assuming 35 fF of assembly capacitance. The chip alone is 9-j243. Sensitivity is stated on a 2.15 dBi antenna." },
    TagChip { id: "ucode9_plasma", name: "NXP UCODE 9, plasma diced", read_dbm: Some(-24.0), write_dbm: Some(-22.0), z: Some((16.0, -237.0)), source: UCODE9, note: "Typical assembled impedance at 915 MHz. The chip alone is 10-j248." },
    TagChip { id: "ucode9_slit", name: "NXP UCODE 9, single-slit assembly", read_dbm: Some(-24.0), write_dbm: Some(-22.0), z: Some((10.0, -191.0)), source: UCODE9, note: "Typical assembled impedance at 915 MHz for a single-slit antenna, with 195 fF of assembly and test pad capacitance." },
    TagChip { id: "qstar_7u", name: "Quanray Qstar-7U", read_dbm: Some(-24.0), write_dbm: Some(-21.0), z: None, source: "Launch announcement and reseller listings, not a manufacturer data sheet", note: "No impedance is published, so get it from Quanray or measure an assembled tag, then give it as chip_z. One listing gives -23 dBm read, so treat the sensitivity as approximate too." },
    TagChip { id: "m730", name: "Impinj M730", read_dbm: Some(-24.0), write_dbm: Some(-21.0), z: None, source: "Impinj Paragon EX5030 inlay data sheet, for sensitivity", note: "No Impinj impedance figure was found. A paper gives 15-j189 at 870.3 MHz, not 915 MHz: get the value from Impinj." },
    TagChip { id: "m830", name: "Impinj M830", read_dbm: Some(-25.5), write_dbm: None, z: None, source: "Impinj M800 series product brief, for sensitivity", note: "No Impinj impedance figure was found: get it from Impinj." },
    TagChip { id: "ucode8", name: "NXP UCODE 8", read_dbm: Some(-23.0), write_dbm: Some(-18.0), z: None, source: "NXP UCODE 8 data sheet, for sensitivity", note: "A paper uses 15-j217, but it wasn't checked against the data sheet: confirm it before designing to it." },
];

pub fn chip(id: &str) -> Option<&'static TagChip> {
    CHIPS.iter().find(|c| c.id == id)
}

/// A chip's series impedance at `f`, from its value at 915 MHz: a resistance and a capacitance. Its
/// reactance moves with frequency, which is what makes a band as wide as 860-960 MHz hard.
pub fn chip_at(z915: (f64, f64), f: f64) -> C64 {
    C64::new(z915.0, z915.1 * 915e6 / f)
}

/// The share of the power an antenna collects that reaches the chip, 0 to 1: one minus the power
/// reflection |(Za - Zc*) / (Za + Zc)|² of NXP AN1715, equation 36.
pub fn power_transfer(za: C64, zc: C64) -> f64 {
    let s = (za - zc.conj()) / (za + zc);
    (1.0 - s.norm_sqr()).clamp(0.0, 1.0)
}

/// Read range in free space, metres, limited by the chip (forward link). AN1715 equation 37, with
/// the sensitivity taken the conservative way: as data sheets state it, on a matched 2.15 dBi
/// antenna, so a matched dipole reaches where the incident power equals the data sheet figure.
/// Real tags on real products reach less: this is what the antenna alone allows.
pub fn read_range_m(f_hz: f64, eirp_dbm: f64, gain_dbi: f64, transfer: f64, read_dbm: f64, polarisation_loss: f64) -> f64 {
    const DIPOLE_DBI: f64 = 2.15;
    let lambda = crate::C0 / f_hz;
    let margin_db = eirp_dbm - read_dbm + (gain_dbi - DIPOLE_DBI) + 10.0 * (transfer.max(1e-12) * polarisation_loss.max(1e-12)).log10();
    lambda / (4.0 * PI) * 10f64.powf(margin_db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(r: f64, x: f64) -> C64 {
        C64::new(r, x)
    }

    #[test]
    fn a_conjugate_match_delivers_everything() {
        assert!((power_transfer(z(15.0, 231.0), z(15.0, -231.0)) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn it_agrees_with_the_usual_form_of_the_formula() {
        for (za, zc) in [(z(20.0, 200.0), z(15.0, -231.0)), (z(50.0, 0.0), z(15.0, -231.0)), (z(5.0, 300.0), z(10.0, -191.0))] {
            let usual = 4.0 * za.re * zc.re / (za + zc).norm_sqr();
            assert!((power_transfer(za, zc) - usual).abs() < 1e-12, "{za} {zc}");
        }
    }

    /// Why S11 can't judge a tag: a plain 50 Ω dipole delivers almost nothing to a chip.
    #[test]
    fn an_ordinary_antenna_barely_powers_a_chip() {
        let t = power_transfer(z(73.0, 0.0), z(15.0, -231.0));
        assert!(t < 0.1, "{t}");
    }

    /// AN1715's own case, D0 = 7.811 m: 1 W into a 2.15 dBi reader antenna, a 2.15 dBi tag and
    /// -15.23 dBm at the chip, at 915 MHz, where they take lambda as 32.8 cm.
    #[test]
    fn the_range_formula_reproduces_the_application_notes_example() {
        // Their threshold is at the chip, so here the antenna's gain counts on top of it.
        let (eirp, gain, chip_dbm) = (30.0 + 2.15, 2.15, -15.23);
        let got = read_range_m(915e6, eirp, gain, 1.0, chip_dbm - gain, 1.0);
        assert!((got - 7.811).abs() < 0.03, "{got}");
    }

    #[test]
    fn range_falls_as_it_should() {
        let matched = read_range_m(915e6, 36.0, 2.15, 1.0, -24.0, 1.0);
        // Half the power to the chip is 3 dB, which is range divided by root two.
        let half = read_range_m(915e6, 36.0, 2.15, 0.5, -24.0, 1.0);
        assert!((matched / half - 2f64.sqrt()).abs() < 1e-9);
        // A linear tag under a circular reader loses half too.
        let circular = read_range_m(915e6, 36.0, 2.15, 1.0, -24.0, 0.5);
        assert!((matched / circular - 2f64.sqrt()).abs() < 1e-9);
        // 60 dB of budget at 915 MHz: about 26 m, which is the forward limit in free space.
        assert!((matched - 26.1).abs() < 0.3, "{matched}");
    }

    #[test]
    fn unpublished_figures_stay_empty_and_say_why() {
        let q = chip("qstar_7u").unwrap();
        assert!(q.z.is_none(), "an impedance nobody published must not appear here");
        assert!(q.note.contains("Quanray"));
        assert!(!q.source.contains("data sheet") || q.source.contains("not a manufacturer"));
        for c in CHIPS {
            assert!(!c.source.is_empty(), "{}", c.id);
            if let Some((r, x)) = c.z {
                assert!(r > 0.0 && x < 0.0, "a chip is resistive and capacitive: {}", c.id);
            }
        }
        assert!(chip("ucode9").unwrap().z == Some((15.0, -231.0)));
        assert!(chip("nope").is_none());
    }
}
