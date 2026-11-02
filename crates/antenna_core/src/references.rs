//! Reference designs: for a job, the structure real products use, why, the order to tune it in, what
//! goes wrong, and where the facts come from. A designer starts here rather than from the simplest
//! shape that might work, because a bare dipole is the wrong answer for most real jobs.
//!
//! Each opens as a sample that a test holds to what it says, so a reference is a design this app has
//! solved, not a description of one. What was solved is always a model: the label stays Simulated.

use crate::samples::{self, Group};

pub struct Reference {
    /// The sample it opens as.
    pub sample: &'static str,
    /// What someone would ask for.
    pub job: &'static str,
    /// The words it's found by, beyond its title.
    pub keywords: &'static [&'static str],
    /// What it is, and why it's built this way and not a simpler way.
    pub why: &'static str,
    /// What each dimension does, in the order to change them.
    pub tune: &'static [&'static str],
    /// What a real product should reach.
    pub targets: &'static [&'static str],
    /// What this model leaves out, and what goes wrong in practice.
    pub limits: &'static [&'static str],
    pub sources: &'static [&'static str],
}

const AN1715: &str = "NXP AN1715, UHF RFID PCB antenna design, rev. 3.0: https://www.nxp.com/docs/en/application-note/AN171530.pdf";
const UCODE9: &str = "NXP SL3S1206 UCODE 9 data sheet, rev. 3.5, table 11: https://www.nxp.com/docs/en/data-sheet/SL3S1206.pdf";
const READER_KB: &str = "This app's knowledge base, 'RFID reader antenna selection and installation', which cites the product sheets";

pub static REFERENCES: [Reference; 5] = [
    Reference {
        sample: "rfid_tag_inlay",
        job: "UHF RFID tag or inlay, for labels on non-metal goods",
        keywords: &["rfid", "uhf", "tag", "inlay", "label", "transponder", "t-match", "tmatch", "meander", "chip", "ucode", "impinj", "qstar", "quanray", "monza"],
        why: "A tag chip is strongly capacitive (UCODE 9: 15-j231 ohm), so the antenna must be inductive and match its complex conjugate, not 50 ohm. A plain dipole near resonance delivers under a tenth of the power it collects. A T-match or small loop supplies the inductance, and a dipole around half a wavelength supplies the gain; meanders fold it into a label.",
        tune: &[
            "Reactance first: T-match width a and height b set the inductance, and with it the Xa the chip needs. Larger a or b raises it.",
            "Then resistance: meander count n and pitch s shift the dipole's own resonance, which sets how much resistance the T-match transforms up. Closer to resonance raises Ra.",
            "Then centre the band: if power transfer peaks low or high, change Lx or the meander total, and put reactance back with a or b.",
            "Judge by power transfer into the chip with antenna_spec chip, never by S11: S11 against 50 ohm means nothing for a tag.",
        ],
        targets: &["Power transfer into the chip of 0.7 or more across the band", "About 1 to 2 dBi: a label this size has little more", "Read range is limited by the chip: compare the free-space figure with the data sheet's"],
        limits: &[
            "The model is metal in free space. A real inlay sits on PET, is stuck to a product and has a chip with some assembly capacitance, so expect the band to move down. Tune on the real material, and measure.",
            "Bandwidth is set by the chip's Q, about 15 for UCODE 9, so a global 860-960 MHz tag gives up peak power transfer for coverage.",
            "Quanray Qstar-7U and the Impinj chips have no published impedance here: get it from the vendor, or measure an assembled tag, and give it as chip_z.",
        ],
        sources: &[AN1715, UCODE9],
    },
    Reference {
        sample: "rfid_reader_dual_us",
        job: "UHF RFID reader antenna for the 902-928 MHz band, circular",
        keywords: &["rfid", "uhf", "reader", "antenna", "panel", "circular", "cp", "rhcp", "fcc", "us", "902", "928", "dock", "door", "portal", "hybrid", "dual feed"],
        why: "Tags can sit at any rotation, so a reader panel is usually circular, which reads a linear tag at any angle for 3 dB of loss. Two feeds in quadrature hold the axial ratio over all of 902-928 MHz, which is 2.8 %, wider than one feed with cut corners manages. Air under the patch keeps the loss low: this is the shape of a commercial panel.",
        tune: &[
            "Resonance first: patch side L centres the band.",
            "Then the match: feed offset xf, further out for more resistance. Judge it as active reflection, with both feeds driven, since each loads the other.",
            "Then bandwidth: a taller air gap h widens it, until probe inductance spoils the match.",
            "Hand: swapping the feeds or the sign of the 90 degrees swaps RHCP and LHCP.",
        ],
        targets: &["Axial ratio 3 dB or lower at boresight, and commercial panels claim about 1 dB", "About 8 to 9 dBic", "Active reflection -10 dB or lower across the band, and better on a product", "Front to back over 18 dB, which needs a ground plate larger than this one"],
        limits: &[
            "The two feeds assume a perfect 90 degree split. A real hybrid has its own loss and imbalance, which limit the axial ratio.",
            "In the US, gain above 6 dBi costs conducted power one for one, so more gain narrows the beam without adding legal reach.",
            "No radome is modelled, and one detunes the patch downwards.",
        ],
        sources: &[READER_KB],
    },
    Reference {
        sample: "rfid_reader_cp_eu",
        job: "UHF RFID reader antenna for the 865-868 MHz band, circular, low cost",
        keywords: &["rfid", "uhf", "reader", "antenna", "panel", "circular", "cp", "rhcp", "etsi", "eu", "europe", "865", "868", "truncated", "cut corner", "single feed"],
        why: "With only 3 MHz to cover, one feed is enough: cutting two opposite corners splits the patch's mode in two, a quarter cycle apart. No hybrid means a cheaper panel. It doesn't stretch to 902-928 MHz, where the two-feed panel is the answer.",
        tune: &[
            "Resonance first: patch side L.",
            "Then circularity: corner size t splits the two modes. Too little and it's elliptical, too much and the match suffers.",
            "Then the match: feed offset xf, trading against the axial ratio.",
            "Hand: cutting the other pair of corners swaps RHCP and LHCP.",
        ],
        targets: &["Axial ratio 3 dB or lower across 865-868 MHz", "About 8 to 9 dBic", "Reflection about -10 dB, and a matching network can improve on that"],
        limits: &["Axial ratio bandwidth is narrow, so tolerances matter: run antenna_yield before committing to a build.", "In Europe at full power the horizontal beam must be 90 degrees or narrower.", "No radome is modelled."],
        sources: &[READER_KB],
    },
    Reference {
        sample: "rfid_panel",
        job: "UHF RFID reader antenna, linear, when the tags are all oriented the same way",
        keywords: &["rfid", "uhf", "reader", "antenna", "panel", "linear", "patch", "shelf", "conveyor", "oriented"],
        why: "When tags pass in a known orientation, a linear panel gets back the 3 dB a circular one gives up. If the orientation isn't controlled, it loses the tag entirely when crossed.",
        tune: &["Resonance first: patch length L.", "Then the match: probe offset yf.", "Then bandwidth: a taller air gap h."],
        targets: &["About 9 dBi", "Reflection -10 dB or lower across the band"],
        limits: &["A tag crossed to it can't be read at all. For unknown orientation, use a circular panel."],
        sources: &[READER_KB],
    },
    Reference {
        sample: "rfid_gate_eu",
        job: "RFID security gate or portal at a shop or warehouse door",
        keywords: &["rfid", "gate", "portal", "door", "security", "eas", "pedestal", "aisle"],
        why: "A gate is judged by what it reads in the aisle and what it wrongly reads beside it, so what matters is the field through the aisle under each phase state, not one antenna's pattern.",
        tune: &["Set the excitation states with antenna_states.", "Judge with antenna_gate_test: read rate through the aisle, and stray reads from displays nearby.", "Find better phases with antenna_find_phases."],
        targets: &["Read rate through the aisle against stray reads, from the gate test"],
        limits: &["It solves the field, and takes the tag and the shopper as simple models: confirm on the floor."],
        sources: &[READER_KB],
    },
];

/// Which references fit a request, best first. Nothing fitting means all of them.
pub fn find(request: &str) -> Vec<&'static Reference> {
    let words: Vec<String> = request.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1).map(str::to_string).collect();
    let mut scored: Vec<(usize, &Reference)> = REFERENCES.iter().map(|r| (words.iter().filter(|w| r.keywords.iter().any(|k| k.split(' ').any(|p| p == w.as_str()))).count(), r)).filter(|(n, _)| *n > 0).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    if scored.is_empty() { REFERENCES.iter().collect() } else { scored.into_iter().map(|(_, r)| r).collect() }
}

/// Samples of other uses with no write-up of their own yet: a real starting point all the same.
pub fn other_samples() -> impl Iterator<Item = &'static samples::Sample> {
    samples::all().iter().filter(|s| !REFERENCES.iter().any(|r| r.sample == s.id) && s.group != Group::Reconfigurable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_reference_opens_as_a_sample_and_says_where_its_facts_come_from() {
        for r in &REFERENCES {
            assert!(samples::by_id(r.sample).is_some(), "{}: no such sample", r.sample);
            assert!(!r.sources.is_empty() && !r.tune.is_empty() && !r.targets.is_empty() && !r.limits.is_empty(), "{}", r.sample);
        }
    }

    #[test]
    fn a_request_finds_the_right_structure() {
        assert_eq!(find("I need an RFID tag antenna with a UCODE 9 chip")[0].sample, "rfid_tag_inlay");
        assert_eq!(find("Quanray Qstar 7U inlay")[0].sample, "rfid_tag_inlay");
        assert_eq!(find("circular reader panel for a dock door, US band 902-928")[0].sample, "rfid_reader_dual_us");
        assert_eq!(find("low cost RFID reader antenna for Europe, ETSI 865-868")[0].sample, "rfid_reader_cp_eu");
        assert_eq!(find("security gate at a shop door")[0].sample, "rfid_gate_eu");
        assert_eq!(find("something nobody has written up").len(), REFERENCES.len(), "nothing fitting shows them all");
    }

    #[test]
    fn the_other_samples_are_offered_without_repeating_the_references() {
        let others: Vec<&str> = other_samples().map(|s| s.id).collect();
        assert!(others.contains(&"uwb_disc") && others.contains(&"gps_helix"));
        assert!(!others.contains(&"rfid_tag_inlay"));
    }
}
