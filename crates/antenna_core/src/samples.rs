//! Sample projects: real uses of the catalogue's antennas, each in the band it is for and
//! trimmed against a solver, to open and look at. A template is a starting point for any
//! frequency; a sample says what one antenna does for one job, and is held to it by a test.

use anyhow::{Context, Result};

use crate::project::{Drive, ExtraPort, ExtraPrim, ExtraSolid, Form, Project, SimResult};
use crate::reconfig::PolGoal;
use crate::{C64, db20, linspace, sketch};

/// What a sample is used for, as the list groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Rfid,
    Iot,
    Wifi,
    Gnss,
    Uwb,
    Radio,
    Microwave,
    Reconfigurable,
    Arrays,
    Drawn,
}

impl Group {
    pub const ALL: [Group; 10] = [Group::Drawn, Group::Rfid, Group::Iot, Group::Wifi, Group::Gnss, Group::Uwb, Group::Radio, Group::Microwave, Group::Reconfigurable, Group::Arrays];
    pub fn label(self) -> &'static str {
        match self {
            Group::Rfid => "UHF RFID",
            Group::Iot => "LoRa, BLE and sub-GHz",
            Group::Wifi => "Wi-Fi",
            Group::Gnss => "GNSS and satellite",
            Group::Uwb => "UWB",
            Group::Radio => "Broadcast and radio",
            Group::Microwave => "Microwave",
            Group::Reconfigurable => "Reconfigurable",
            Group::Arrays => "Phased arrays and direction finding",
            Group::Drawn => "Drawn, with its dimensions named",
        }
    }
}

/// Where opening a sample lands, and what it solves on the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opens {
    /// An ordinary run, then the 3D pattern.
    FarField,
    /// Every port once, then the states.
    Reconfig,
    /// The field through the aisle.
    Gate,
    /// An ordinary run, on the Draw page: what it shows is how the model was drawn.
    Drawing,
    /// An ordinary run of every port, then the beam it can steer, coupling and all.
    Phased,
    /// An ordinary run of every port, then what it reads of sources around it.
    DirectionFinding,
}

impl Opens {
    /// A run with patterns in the band, as opposed to a job of its own.
    pub fn solves_far_field(self) -> bool {
        matches!(self, Opens::FarField | Opens::Drawing)
    }
}

pub struct Sample {
    pub id: &'static str,
    pub title: &'static str,
    pub group: Group,
    /// What it is for and what is worth looking at.
    pub about: &'static str,
    pub template: &'static str,
    pub f0: &'static str,
    /// The band it is for, GHz.
    pub band: (f64, f64),
    /// What the sweep covers, GHz.
    pub sweep: (f64, f64),
    /// Variables that differ from the template.
    pub sets: &'static [(&'static str, &'static str)],
    pub metal: &'static str,
    /// `None`: the template's own.
    pub substrate: Option<&'static str>,
    /// Amplitude and phase per port, when the ports work together.
    pub drive: &'static [(f64, f64)],
    pub opens: Opens,
    /// The worst reflection it's held to across the band, dB. Above 0: not held to one.
    pub limit_db: f64,
    /// The least realized gain it's held to at the band's centre, dBi.
    pub min_gain_dbi: f64,
    /// Which sense of circular polarisation it's held to.
    pub pol: Option<PolGoal>,
    /// A tag's chip, from `tag::CHIPS`: it's then judged by power into that chip, not by reflection.
    pub chip: Option<&'static str>,
    /// The least power transfer into the chip it's held to across the band, 0 to 1.
    pub min_transfer: f64,
    /// Draws the model on a blank project, for a sample that shows drawing.
    pub draw: Option<fn(&mut Project) -> Result<()>>,
}

fn pair(a: &str, b: &str) -> [String; 2] {
    [a.to_string(), b.to_string()]
}

fn drawn(p: &mut Project, name: &str, axis: usize, at: &str, form: Form, holes: Vec<Vec<[String; 2]>>) {
    let material = p.metal.clone();
    p.extra.push(ExtraSolid { name: name.into(), material, prim: ExtraPrim::Drawn { axis, at: at.into(), thick: "0".into(), form, holes } });
}

fn feed(p: &mut Project, a: [&str; 3], b: [&str; 3], width: &str) {
    p.extra_ports.push(ExtraPort { name: "feed".into(), a: a.map(String::from), b: b.map(String::from), width: width.into(), z0: "Z0".into() });
}

fn vars(p: &mut Project, rows: &[(&str, &str, &str)]) -> Result<()> {
    rows.iter().try_for_each(|(n, e, note)| sketch::add_variable(p, n, e, note))
}

/// Two rectangles and a gap, each dimension a variable.
fn draw_dipole(p: &mut Project) -> Result<()> {
    vars(p, &[("L", "0.47*lambda0", "Tip to tip"),("w", "lambda0/50", "Arm width"), ("g", "lambda0/60", "Feed gap")])?;
    p.params.iter_mut().find(|r| r.name == "Z0").context("Z0")?.expr = "73".into();
    for (name, c) in [("Right arm", "(L+g)/4"), ("Left arm", "-(L+g)/4")] {
        drawn(p, name, 2, "0", Form::Rect { c: pair(c, "0"), size: pair("(L-g)/2", "w") }, Vec::new());
    }
    feed(p, ["-g/2", "0", "0"], ["g/2", "0", "0"], "w");
    Ok(())
}

/// A plate over a plate, joined by a probe.
fn draw_patch(p: &mut Project) -> Result<()> {
    vars(p, &[("h", "0.04*lambda0", "Height over the ground"), ("L", "0.443*lambda0", "Patch length, sets the frequency"), ("W", "0.55*lambda0", "Patch width"), ("G", "0.9*lambda0", "Ground plate size"), ("yf", "0.25*L", "Probe offset, sets the match"), ("wp", "lambda0/40", "Probe width")])?;
    drawn(p, "Ground", 2, "0", Form::Rect { c: pair("0", "0"), size: pair("G", "G") }, Vec::new());
    drawn(p, "Patch", 2, "h", Form::Rect { c: pair("0", "0"), size: pair("W", "L") }, Vec::new());
    feed(p, ["0", "yf", "0"], ["0", "yf", "h"], "wp");
    Ok(())
}

/// A plate with a slot cut in it. The hole's corners are expressions like any other.
fn draw_slot(p: &mut Project) -> Result<()> {
    vars(p, &[("Ls", "0.42*lambda0", "Slot length, sets the frequency"),("ws", "lambda0/40", "Slot width"), ("Gx", "0.7*lambda0", "Plate across the slot"), ("Gy", "0.8*lambda0", "Plate along the slot"), ("wp", "lambda0/60", "Feed width")])?;
    p.params.iter_mut().find(|r| r.name == "Z0").context("Z0")?.expr = "500".into();
    let hole = vec![pair("-ws/2", "-Ls/2"), pair("ws/2", "-Ls/2"), pair("ws/2", "Ls/2"), pair("-ws/2", "Ls/2")];
    drawn(p, "Plate", 2, "0", Form::Rect { c: pair("0", "0"), size: pair("Gx", "Gy") }, vec![hole]);
    feed(p, ["-ws/2", "0", "0"], ["ws/2", "0", "0"], "wp");
    Ok(())
}

/// A dipole folded to fit, drawn as one trace per arm so its width and every bend can be moved.
fn draw_meander(p: &mut Project) -> Result<()> {
    vars(p, &[("X", "0.0925*lambda0", "Straight run of each arm"), ("Y", "0.03*lambda0", "Height of a fold"), ("d", "0.025*lambda0", "Fold spacing"), ("w", "lambda0/120", "Trace width"), ("g", "lambda0/60", "Feed gap")])?;
    p.params.iter_mut().find(|r| r.name == "Z0").context("Z0")?.expr = "40".into();
    // A narrow trace with tight bends needs more than the usual mesh to resolve them.
    p.setup.edges_per_lambda = 30.0;
    for (name, s) in [("Right arm", ""), ("Left arm", "-")] {
        let u = |e: &str| format!("{s}({e})");
        let path = vec![
            [u("g/2"), "0".to_string()],
            [u("g/2+X"), "0".to_string()],
            [u("g/2+X"), "Y".to_string()],
            [u("g/2+X+d"), "Y".to_string()],
            [u("g/2+X+d"), "-Y".to_string()],
            [u("g/2+X+2*d"), "-Y".to_string()],
            [u("g/2+X+2*d"), "Y".to_string()],
        ];
        let material = p.metal.clone();
        p.extra.push(ExtraSolid { name: name.into(), material, prim: ExtraPrim::Strip { axis: 2, at: "0".into(), path, w: "w".into() } });
    }
    feed(p, ["-g/2", "0", "0"], ["g/2", "0", "0"], "w");
    Ok(())
}

const fn base(id: &'static str, title: &'static str, group: Group, about: &'static str, template: &'static str, f0: &'static str, band: (f64, f64), sweep: (f64, f64)) -> Sample {
    Sample { id, title, group, about, template, f0, band, sweep, sets: &[], metal: "PEC", substrate: None, drive: &[], opens: Opens::FarField, limit_db: -10.0, min_gain_dbi: -40.0, pol: None, chip: None, min_transfer: 0.0, draw: None }
}

static SAMPLES: [Sample; 38] = [
    Sample { draw: Some(draw_dipole), opens: Opens::Drawing, metal: "Copper", min_gain_dbi: 1.5, ..base("drawn_dipole", "Drawn dipole, 915 MHz", Group::Drawn, "Two rectangles and a feed, the simplest thing to draw. Select an arm: its centre and sides are written in L, w and g, so changing L in the table moves both arms.", "custom", "915MHz", (0.902, 0.928), (0.75, 1.1)) },
    Sample { draw: Some(draw_patch), opens: Opens::Drawing, metal: "Aluminium", min_gain_dbi: 7.0, ..base("drawn_patch", "Drawn patch on two sheets, 915 MHz", Group::Drawn, "A plate over a ground, drawn on two heights and joined by a probe. The upper sheet's height is the variable h, and yf moves the probe to set the match.", "custom", "915MHz", (0.91, 0.92), (0.82, 1.01)) },
    Sample { draw: Some(draw_slot), opens: Opens::Drawing, metal: "Copper", ..base("drawn_slot", "Drawn slot in a plate, 2.45 GHz", Group::Drawn, "A hole cut from a plate. The hole's corners are written in Ls and ws, so the slot can be swept and optimised like a template's dimensions.", "custom", "2.45GHz", (2.42, 2.48), (2.1, 2.8)) },
    Sample { draw: Some(draw_meander), opens: Opens::Drawing, metal: "Copper", min_gain_dbi: 0.5, ..base("drawn_meander", "Drawn meander dipole, 915 MHz", Group::Drawn, "Each arm one trace with every bend an expression: X, Y, d and w are all you need to fold it to a size. Shorter than a straight dipole, and narrower in band for it.", "custom", "915MHz", (0.91, 0.92), (0.75, 1.1)) },
    Sample { sets: &[("Lx", "95mm"), ("w", "1mm"), ("a", "28mm"), ("b", "2.6mm"), ("g", "1mm"), ("n", "4"), ("Hm", "3.5mm"), ("s", "3.5mm"), ("x0", "16mm")], metal: "Aluminium", limit_db: 1.0, chip: Some("ucode9"), min_transfer: 0.7, ..base("rfid_tag_inlay", "RFID tag inlay, 95 x 8 mm", Group::Rfid, "A meandered dipole with a T-match, inductive to suit a capacitive chip, here UCODE 9. Judged by power reaching the chip, since S11 means nothing for a tag.", "rfid_tag", "915MHz", (0.902, 0.928), (0.82, 1.01)) },
    Sample { sets: &[("h", "0.025*lambda0"), ("L", "0.46*lambda0"), ("t", "0.075*lambda0"), ("xf", "0.14*lambda0")], metal: "Aluminium", min_gain_dbi: 8.0, pol: Some(PolGoal::Rhcp), limit_db: -9.5, ..base("rfid_reader_cp_eu", "RFID reader panel, circular, EU band", Group::Rfid, "An air-spaced patch with two corners cut and one feed: right-hand circular over 865-868 MHz with no hybrid, the low-cost way to read tags at any rotation.", "rfid_reader_cp", "866.5MHz", (0.865, 0.868), (0.8, 0.94)) },
    Sample { sets: &[("h", "0.04*lambda0"), ("L", "0.448*lambda0"), ("xf", "0.11*lambda0")], drive: &[(1.0, 0.0), (1.0, -90.0)], metal: "Aluminium", min_gain_dbi: 8.0, pol: Some(PolGoal::Rhcp), ..base("rfid_reader_dual_us", "RFID reader panel, circular, US band", Group::Rfid, "An air-spaced patch fed on two axes in quadrature: right-hand circular across all of 902-928 MHz, where one feed runs out of axial ratio. The shape of a commercial panel.", "rfid_reader_dual", "915MHz", (0.902, 0.928), (0.82, 1.01)) },
    Sample { sets: &[("h", "0.04*lambda0"), ("L", "0.443*lambda0"), ("W", "0.55*lambda0"), ("yf", "0.25*L"), ("wp", "lambda0/40")], metal: "Aluminium", min_gain_dbi: 9.0, ..base("rfid_panel", "RFID reader panel, US band", Group::Rfid, "An air-spaced aluminium patch for a UHF reader across the 902–928 MHz band: the low-loss panel that goes on a dock door or a shelf end.", "patch_air", "915MHz", (0.902, 0.928), (0.82, 1.01)) },
    Sample { sets: &[("n_dir", "6")], metal: "Aluminium", min_gain_dbi: 9.0, ..base("rfid_yagi", "RFID long-range Yagi, EU band", Group::Rfid, "Reflector, driven element and six directors for the 865–868 MHz band, for reading across a yard: more range, in a narrower beam.", "yagi", "866.5MHz", (0.865, 0.868), (0.78, 0.95)) },
    Sample { opens: Opens::Gate, limit_db: 1.0, ..base("rfid_gate_eu", "RFID security gate, EU band", Group::Rfid, "Five separately fed elements on a pedestal, tested for read rate through the aisle: the tags read walking through, and those on display that shouldn't be.", "rfid_gate", "866.5MHz", (0.865, 0.868), (0.865, 0.868)) },
    Sample { sets: &[("h", "0.242*lambda0"), ("g", "lambda0/120")], metal: "Brass", min_gain_dbi: 4.0, ..base("lora_whip", "LoRa ground-plane whip, 868 MHz", Group::Iot, "A quarter-wave brass whip for a gateway on a metal roof, 863–870 MHz: about 5 dBi at a low angle, and all round.", "monopole", "868MHz", (0.863, 0.870), (0.7, 1.05)) },
    Sample { metal: "Copper", min_gain_dbi: 3.0, ..base("subghz_pifa", "Sub-GHz PIFA, 915 MHz", Group::Iot, "A shorted plate over a device's ground, the shape inside gateways and handhelds: compact, and narrow in band because of it.", "pifa", "915MHz", (0.902, 0.928), (0.75, 1.1)) },
    Sample { metal: "Copper", min_gain_dbi: 3.0, ..base("ble_meander", "BLE meander line, 2.44 GHz", Group::Iot, "A monopole folded to fit a small module, 2.4–2.4835 GHz. Its 15 Ω feed needs a match to 50 Ω: try it on the Matching page.", "meander", "2.44GHz", (2.4, 2.4835), (2.0, 2.9)) },
    Sample { metal: "Copper", min_gain_dbi: 3.0, ..base("ism433_coil", "433 MHz stubby coil", Group::Iot, "A normal-mode helix, as on a key fob receiver or a remote sensor: a monopole's pattern from a coil a tenth of its height.", "helix_normal", "433.9MHz", (0.43305, 0.43479), (0.38, 0.49)) },
    Sample { metal: "Copper", min_gain_dbi: 8.0, ..base("wifi_corner", "Wi-Fi corner reflector, 2.4 GHz", Group::Wifi, "A dipole in a 90° corner of two plates, 2.4–2.4835 GHz: a cheap directional antenna for a point-to-point link.", "corner_reflector", "2.44GHz", (2.4, 2.4835), (2.1, 2.8)) },
    Sample { metal: "Copper", min_gain_dbi: -2.0, ..base("wifi_ifa", "Wi-Fi PCB inverted-F, 2.4 GHz", Group::Wifi, "A trace at the edge of a 1.6 mm FR-4 board, as on BLE and Wi-Fi modules. It costs nothing to make, and the board's loss shows in its efficiency.", "ifa_pcb", "2.44GHz", (2.4, 2.4835), (2.0, 2.9)) },
    Sample { metal: "Copper", min_gain_dbi: 0.0, ..base("wifi_mimo", "Wi-Fi two-port patch pair", Group::Wifi, "Two FR-4 patches half a wavelength apart, for 2×2 MIMO. Look at coupling, envelope correlation and diversity on the MIMO page.", "mimo_patch", "2.45GHz", (2.42, 2.48), (2.2, 2.7)) },
    Sample { sets: &[("h", "0.813mm")], metal: "Copper", substrate: Some("RO4003C"), min_gain_dbi: 3.0, ..base("wifi5_patch", "5.8 GHz inset patch on Rogers", Group::Wifi, "A microstrip-fed patch on 0.813 mm RO4003C, 5.725–5.85 GHz: a laminate with far less loss than FR-4, at a price.", "patch_inset", "5.8GHz", (5.725, 5.85), (5.2, 6.4)) },
    Sample { min_gain_dbi: 10.0, pol: Some(PolGoal::Rhcp), metal: "Copper", ..base("gps_helix", "GPS L1 axial-mode helix", Group::Gnss, "A right-hand wound helix, 1575.42 MHz: circularly polarised end-fire, the polarisation GPS satellites transmit. Its feed is about 140 Ω.", "helix_axial", "1575.42MHz", (1.565, 1.586), (1.2, 1.9)) },
    Sample { drive: &[(1.0, 0.0), (1.0, -90.0)], metal: "Aluminium", ..base("noaa_turnstile", "Weather-satellite turnstile, 137 MHz", Group::Gnss, "Two crossed dipoles driven 90° apart, for the 137–138 MHz downlink: circular overhead, the way a polar orbiter sees it.", "turnstile", "137.5MHz", (0.137, 0.138), (0.115, 0.16)) },
    Sample { metal: "Copper", min_gain_dbi: -2.0, ..base("uwb_disc", "UWB disc monopole, channels 5 to 9", Group::Uwb, "A printed disc over a cut-back ground on FR-4, 6.24–8.24 GHz, as on a UWB tag: wide enough for both channels, and nearly all round.", "uwb_monopole", "7.2GHz", (6.24, 8.24), (4.0, 10.0)) },
    Sample { metal: "Copper", min_gain_dbi: 3.0, ..base("uwb_vivaldi", "UWB test Vivaldi, 3 to 10 GHz", Group::Uwb, "An exponentially tapered slot on RO4003C: the end-fire test antenna for UWB, whose beam and match hold over more than an octave.", "vivaldi", "6GHz", (3.5, 9.5), (3.0, 10.6)) },
    Sample { sets: &[("L", "0.45*lambda0"), ("s", "0.06*lambda0"), ("a", "0.006*lambda0")], metal: "Aluminium", min_gain_dbi: 1.5, ..base("fm_folded", "FM broadcast folded dipole", Group::Radio, "A folded dipole for 88–108 MHz, four times a plain dipole's impedance on its 300 Ω feed, and wide enough for the whole band with thick tube.", "folded_dipole", "98MHz", (0.088, 0.108), (0.075, 0.125)) },
    Sample { sets: &[("n_dir", "3")], metal: "Aluminium", min_gain_dbi: 7.0, ..base("ham_yagi", "2 m amateur Yagi", Group::Radio, "A five-element beam for the 144–148 MHz band: gain against a dipole, front-to-back, and how both fall away towards the band's edges.", "yagi", "146MHz", (0.144, 0.148), (0.13, 0.165)) },
    Sample { metal: "Aluminium", limit_db: -6.0, ..base("scanner_discone", "Scanner discone, 0.6 to 2 GHz", Group::Radio, "A wire-grid disc and cone for a receiver: all round over several octaves, trading gain for bandwidth.", "discone", "1GHz", (0.7, 2.0), (0.5, 2.3)) },
    Sample { metal: "Aluminium", min_gain_dbi: 5.0, ..base("tv_lpda", "UHF TV log-periodic", Group::Radio, "A twin-boom log-periodic array for 470–790 MHz: the element that's working moves along the boom with frequency, so the pattern holds.", "lpda", "630MHz", (0.47, 0.79), (0.42, 0.88)) },
    Sample { sets: &[("Z0", "100"), ("L", "0.36*lambda0"), ("W", "0.26*lambda0")], metal: "Aluminium", limit_db: -6.0, ..base("gpr_bowtie", "Bow-tie for ground radar, 400 MHz", Group::Radio, "A flared flat dipole on a 100 Ω feed, far wider in band than a strip, for the short pulses of a ground-penetrating radar.", "bowtie", "400MHz", (0.34, 0.5), (0.26, 0.62)) },
    Sample { metal: "Aluminium", limit_db: -6.0, min_gain_dbi: 13.0, ..base("xband_horn", "X-band standard-gain horn", Group::Microwave, "A probe-fed pyramidal horn at 10 GHz, the reference antenna of a test range: high gain and low side lobes.", "horn", "10GHz", (9.5, 10.5), (8.8, 11.2)) },
    Sample { metal: "Aluminium", min_gain_dbi: 5.0, ..base("cband_waveguide", "C-band open waveguide, 5.8 GHz", Group::Microwave, "A probe in a rectangular guide, open at one end: a broad, simple feed for a dish or a near-field probe.", "oewg", "5.8GHz", (5.6, 6.0), (5.0, 6.6)) },
    Sample { metal: "Copper", min_gain_dbi: 2.0, ..base("cband_dra", "Dielectric resonator, 5 GHz", Group::Microwave, "A ceramic puck on a ground plate, probe fed: no metal resonator to lose power in, which is why it's used high in frequency.", "dra", "5GHz", (4.9, 5.1), (4.2, 6.0)) },
    Sample { sets: &[], ..base("slot_plate", "Slot in a plate, 2.45 GHz", Group::Microwave, "The dual of a dipole, cut into a sheet of metal so it sits flush: the same pattern with E and H swapped, and a high feed impedance.", "slot", "2.45GHz", (2.42, 2.48), (2.2, 2.7)) },
    Sample { opens: Opens::Phased, limit_db: 1.0, sets: &[("nx", "8"), ("ny", "8")], ..base("phased_panel", "Beam-steering panel, 8 by 8", Group::Arrays, "Sixty-four dipoles over a ground plane, each with its own port. Steer it: the gain, the active match of every port and any blindness all come from one solve, so scanning needs no new one.", "array_dipole", "2.45GHz", (2.4, 2.5), (2.2, 2.7)) },
    Sample { opens: Opens::DirectionFinding, limit_db: 1.0, ..base("df_ring", "DF ring, 8 monopoles", Group::Arrays, "Eight monopoles round a half-wave radius, as an angle-of-arrival locator might have them. Two transmitters at once come apart with MUSIC, read through the array's own solved coupling.", "array_ring", "2.44GHz", (2.4, 2.4835), (2.2, 2.7)) },
    Sample { opens: Opens::DirectionFinding, limit_db: 1.0, sets: &[("n", "4"), ("R", "0.125*lambda0")], ..base("df_adcock", "Adcock DF, marine VHF", Group::Arrays, "Four monopoles a quarter wave across, as on a coast station at 156.8 MHz: Watson-Watt reads a bearing with no receiver per direction, at the cost of an error that cycles every 90°.", "array_ring", "156.8MHz", (0.156, 0.162), (0.14, 0.18)) },
    Sample { opens: Opens::Reconfig, limit_db: 1.0, ..base("switched_dipole", "Band-switching dipole", Group::Reconfigurable, "PIN diodes join sections of a dipole for three bands from one wire. Flip them in the state table: the change is instant, with no new solve.", "reconfig_dipole", "2.45GHz", (1.4, 2.5), (1.1025, 3.0625)) },
    Sample { opens: Opens::Reconfig, limit_db: 1.0, ..base("tuned_pifa", "Varactor-tuned PIFA", Group::Reconfigurable, "A varactor at the plate's open end tunes it 540–900 MHz with bias. Look at the tuning curve, and at how much the part loses and how hard it's driven at low bias.", "reconfig_pifa", "900MHz", (0.54, 0.9), (0.45, 1.17)) },
    Sample { opens: Opens::Reconfig, limit_db: 1.0, ..base("beam_array", "Switched-beam parasitic array", Group::Reconfigurable, "A monopole ringed by four parasitics: shorted ones reflect, so PIN diodes turn a 9 dBi beam in 90° steps.", "reconfig_parasitic", "2.45GHz", (2.42, 2.48), (2.0825, 2.8175)) },
    Sample { opens: Opens::Reconfig, limit_db: 1.0, ..base("pol_patch", "Polarisation-switching patch", Group::Reconfigurable, "Switched corners make a square patch linear or either hand of circular, at one frequency. Worth comparing with a fixed circular patch on a real link.", "reconfig_pol_patch", "2.45GHz", (2.44, 2.46), (2.156, 2.744)) },
];

pub fn all() -> &'static [Sample] {
    &SAMPLES
}

pub fn by_id(id: &str) -> Option<&'static Sample> {
    SAMPLES.iter().find(|s| s.id == id)
}

impl Sample {
    /// The project: the template at this frequency with this sample's values, band and materials.
    pub fn project(&self) -> Result<Project> {
        let mut p = Project::from_template(self.template, Some(self.f0)).with_context(|| format!("sample '{}'", self.id))?;
        p.name = self.title.to_string();
        if let Some(draw) = self.draw {
            p.metal = self.metal.to_string();
            draw(&mut p).with_context(|| format!("sample '{}'", self.id))?;
        }
        for (name, value) in self.sets {
            let row = p.params.iter_mut().find(|r| r.name == *name).with_context(|| format!("sample '{}': no variable '{name}'", self.id))?;
            row.expr = value.to_string();
        }
        p.metal = self.metal.to_string();
        if let Some(s) = self.substrate {
            p.substrate = s.to_string();
        }
        p.setup.f_lo = self.sweep.0 * 1e9;
        p.setup.f_hi = self.sweep.1 * 1e9;
        // A reconfigurable sample's states look after their own patterns.
        if self.opens.solves_far_field() {
            let centre = 0.5 * (self.band.0 + self.band.1) * 1e9;
            let wide = (self.band.1 - self.band.0) / (0.5 * (self.band.0 + self.band.1)) > 0.02;
            p.setup.far_freqs = if wide { vec![self.band.0 * 1e9, centre, self.band.1 * 1e9] } else { vec![centre] };
            p.setup.far_step = 5.0;
        }
        p.drive = self.drive.iter().map(|d| Drive { amp: d.0, phase: d.1 }).collect();
        // What each opens onto, so there's something to read straight away.
        match self.id {
            "phased_panel" => p.phased.plan.beams = vec![crate::phased::beam::Beam { theta: 30.0, phi: 0.0, share: 1.0 }],
            "df_ring" => {
                use crate::phased::doa::{Method, Scan, Source};
                p.phased.df.scan = Scan::Azimuth { theta: 80.0 };
                p.phased.df.method = Method::Music;
                p.phased.df.step = 1.0;
                p.phased.df.sources = vec![Source { theta: 80.0, phi: 50.0, ..Default::default() }, Source { theta: 80.0, phi: 200.0, ..Default::default() }];
                p.phased.df.compare = vec![Method::Music, Method::Capon, Method::Beamscan];
            }
            "df_adcock" => {
                use crate::phased::doa::{Method, Scan, Source};
                p.phased.df.scan = Scan::Azimuth { theta: 80.0 };
                p.phased.df.method = Method::WatsonWatt;
                p.phased.df.step = 1.0;
                p.phased.df.sources = vec![Source { theta: 80.0, phi: 65.0, ..Default::default() }];
                p.phased.df.compare = vec![Method::WatsonWatt, Method::PseudoDoppler, Method::Music];
            }
            _ => {}
        }
        Ok(p)
    }

    /// What a test holds it to, in words, or nothing if it isn't held to anything yet.
    pub fn held_to(&self) -> String {
        let mut out = Vec::new();
        if self.chip.is_some() {
            out.push(format!("at least {:.0} % of the power into the chip", self.min_transfer * 100.0));
        }
        if self.limit_db < 0.0 {
            out.push(format!("reflection under {:.1} dB across the band", self.limit_db));
        }
        if self.min_gain_dbi > -40.0 {
            out.push(format!("at least {:.1} dBi", self.min_gain_dbi));
        }
        if let Some(p) = self.pol {
            out.push(format!("{} with under 3 dB axial ratio", if p == PolGoal::Rhcp { "right-hand circular" } else { "left-hand circular" }));
        }
        out.join(", ")
    }

    pub fn band_text(&self) -> String {
        let fmt = |ghz: f64| if ghz >= 1.0 { format!("{ghz:.4}").trim_end_matches('0').trim_end_matches('.').to_string() } else { format!("{:.2}", ghz * 1e3).trim_end_matches('0').trim_end_matches('.').to_string() };
        let unit = if self.band.1 >= 1.0 && self.band.0 >= 1.0 { "GHz" } else if self.band.1 < 1.0 { "MHz" } else { "" };
        if unit.is_empty() {
            format!("{:.0} MHz – {} GHz", self.band.0 * 1e3, fmt(self.band.1))
        } else {
            format!("{}–{} {unit}", fmt(self.band.0), fmt(self.band.1))
        }
    }
}

/// A sample's figures, from a result of any solver.
#[derive(Clone, Copy, Debug)]
pub struct Figures {
    /// Worst reflection across the band, of every port it drives.
    pub worst_db: f64,
    /// Peak realized gain at the band's centre.
    pub gain_dbi: f64,
    pub axial_ratio_db: f64,
    pub rhcp: bool,
    /// Where feed 1 matches best.
    pub resonance: Option<f64>,
    /// The worst power transfer into its chip across the band, for a tag.
    pub worst_transfer: Option<f64>,
}

impl Sample {
    /// The weights its ports are driven with: its own, or the first port alone.
    pub fn weights(&self, ports: usize) -> Vec<C64> {
        if self.drive.is_empty() {
            (0..ports).map(|k| C64::new(if k == 0 { 1.0 } else { 0.0 }, 0.0)).collect()
        } else {
            self.drive.iter().map(|d| C64::from_polar(d.0, d.1.to_radians())).collect()
        }
    }

    pub fn centre(&self) -> f64 {
        0.5 * (self.band.0 + self.band.1) * 1e9
    }

    /// What this result says the sample does. `None` without a pattern to judge.
    pub fn figures(&self, r: &SimResult) -> Option<Figures> {
        let ports = r.network.ports();
        let weights = self.weights(ports);
        let band = r.network.interpolate(&linspace(self.band.0 * 1e9, self.band.1 * 1e9, 21));
        let mut worst_db = f64::NEG_INFINITY;
        for (port, w) in weights.iter().enumerate() {
            if w.norm() > 0.0 {
                for v in band.active_reflection(port, &weights) {
                    worst_db = worst_db.max(db20(v.norm()));
                }
            }
        }
        let m = r.combined(r.far_index(self.centre())?, &weights)?.metrics();
        let resonance = r.network.resonances(0).into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        let worst_transfer = self.chip.and_then(crate::tag::chip).and_then(|c| c.z).map(|(re, im)| band.zin(0).iter().map(|z| crate::tag::power_transfer(*z, C64::new(re, im))).fold(1.0, f64::min));
        Some(Figures { worst_db, gain_dbi: m.peak_realized_dbi, axial_ratio_db: m.axial_ratio_db, rhcp: m.rhcp_at_peak, resonance, worst_transfer })
    }

    /// Where it falls short of what it's held to, in words.
    pub fn shortfalls(&self, f: &Figures) -> Vec<String> {
        let mut out = Vec::new();
        if self.chip.is_some() && !f.worst_transfer.is_some_and(|t| t >= self.min_transfer) {
            out.push(format!("power into the chip {:?} against {:.2}", f.worst_transfer, self.min_transfer));
        }
        if self.limit_db < 0.0 && !(f.worst_db <= self.limit_db) {
            out.push(format!("worst match {:.1} dB against {:.1}", f.worst_db, self.limit_db));
        }
        if !(f.gain_dbi >= self.min_gain_dbi) {
            out.push(format!("gain {:.1} dBi against {:.1}", f.gain_dbi, self.min_gain_dbi));
        }
        if let Some(p) = self.pol
            && (f.axial_ratio_db > 3.0 || f.rhcp != (p == PolGoal::Rhcp))
        {
            out.push(format!("axial ratio {:.1} dB, {}", f.axial_ratio_db, if f.rhcp { "RHCP" } else { "LHCP" }));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Geometry;

    /// Every antenna is drawn at the origin: the floor's axes cross on it, and nothing looks adrift.
    /// What's held up has to be. A gate's z is the height its test reads tags at, and a dipole over a
    /// ground plane belongs a quarter wave above it.
    #[test]
    fn everything_stands_at_the_origin() {
        let check = |name: &str, p: &Project| {
            let g = p.scatterer().unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let (lo, hi) = g.bounds();
            let held_up = g.ground || p.template == "rfid_gate";
            for k in 0..if held_up { 2 } else { 3 } {
                assert!(lo[k] <= 1e-9 && hi[k] >= -1e-9, "{name}: axis {k} runs {:.2} to {:.2} mm, missing the origin", lo[k] * 1e3, hi[k] * 1e3);
            }
        };
        for t in templates::all().iter().filter(|t| t.id != "custom") {
            check(t.id, &Project::from_template(t.id, None).unwrap());
        }
        for s in all() {
            check(s.id, &s.project().unwrap());
        }
    }
    use crate::mom::{MomOptions, MomProblem};
    use crate::project::SolverKind;
    use crate::sparam::Network;
    use crate::templates;

    const QUICK_UNKNOWNS: usize = 1000;

    #[test]
    fn every_sample_builds_and_covers_its_band() {
        for (i, s) in all().iter().enumerate() {
            assert!(!all()[i + 1..].iter().any(|o| o.id == s.id), "{}", s.id);
            assert!(templates::by_id(s.template).is_some(), "{}", s.id);
            let p = s.project().unwrap_or_else(|e| panic!("{}: {e:#}", s.id));
            let g = p.geometry().unwrap_or_else(|e| panic!("{}: {e:#}", s.id));
            assert!(!g.ports.is_empty(), "{}", s.id);
            assert_eq!(p.name, s.title);
            assert!(s.band.0 < s.band.1, "{}", s.id);
            assert!(s.sweep.0 <= s.band.0 && s.band.1 <= s.sweep.1, "{}: sweep {:?} band {:?}", s.id, s.sweep, s.band);
            assert!(p.setup.check_band().is_ok(), "{}", s.id);
            assert!(!s.about.is_empty() && s.about.len() < 260, "{}: {}", s.id, s.about.len());
            if !s.drive.is_empty() {
                assert_eq!(s.drive.len(), g.ports.len(), "{}", s.id);
            }
        }
    }

    #[test]
    fn every_group_has_samples_and_bands_read_well() {
        for g in Group::ALL {
            assert!(all().iter().any(|s| s.group == g), "{}", g.label());
        }
        assert_eq!(by_id("rfid_panel").unwrap().band_text(), "902–928 MHz");
        let tag = by_id("rfid_tag_inlay").unwrap();
        assert!(tag.chip.and_then(crate::tag::chip).is_some_and(|c| c.z.is_some()), "a tag sample needs a chip with a published impedance");
        assert_eq!(by_id("wifi_ifa").unwrap().band_text(), "2.4–2.4835 GHz");
        assert_eq!(by_id("ism433_coil").unwrap().band_text(), "433.05–434.79 MHz");
        assert_eq!(by_id("scanner_discone").unwrap().band_text(), "700 MHz – 2 GHz");
    }

    /// A drawn sample shows that a model made on the canvas is an ordinary project: its variables
    /// move it, and it's drawn with the kinds of part the page can edit.
    #[test]
    fn drawn_samples_are_drawn_and_follow_their_variables() {
        let list: Vec<&Sample> = all().iter().filter(|s| s.draw.is_some()).collect();
        assert!(list.len() >= 4);
        for s in list {
            assert_eq!(s.group, Group::Drawn, "{}", s.id);
            assert_eq!(s.opens, Opens::Drawing, "{}", s.id);
            let p = s.project().unwrap();
            assert_eq!(p.template, "custom", "{}", s.id);
            let vars = p.vars().unwrap();
            for part in 0..p.extra.len() {
                assert!(!sketch::handles(&p, part).is_empty(), "{}: part {part} can't be grabbed", s.id);
            }
            let before = format!("{:?} {:?}", p.geometry().unwrap().solids, p.geometry().unwrap().ports);
            for row in p.params.iter().filter(|r| !r.derived) {
                let mut q = p.clone();
                q.params.iter_mut().find(|r| r.name == row.name).unwrap().expr = format!("{:e}", vars[&row.name] * 1.05);
                let after = q.geometry().map_or_else(|e| format!("{e:#}"), |g| format!("{:?} {:?}", g.solids, g.ports));
                assert_ne!(before, after, "{}: {} changes nothing", s.id, row.name);
            }
        }
    }

    #[test]
    fn samples_say_what_they_are_held_to() {
        assert!(by_id("rfid_reader_cp_eu").unwrap().held_to().contains("right-hand"));
        assert!(by_id("rfid_tag_inlay").unwrap().held_to().contains("chip"));
        assert!(by_id("wifi_corner").unwrap().held_to().contains("dBi"));
    }

    fn mom_opts(p: &Project) -> MomOptions {
        MomOptions { segments_per_lambda: p.setup.segments_per_lambda, edges_per_lambda: p.setup.edges_per_lambda, theta_step: 5.0, phi_step: 5.0 }
    }

    /// A result as a run gives it, on fewer solved frequencies.
    fn solve(p: &Project, g: &Geometry) -> (SimResult, usize, f64) {
        let t0 = std::time::Instant::now();
        let problem = MomProblem::new(g, p.setup.f_hi, mom_opts(p)).unwrap_or_else(|e| panic!("{}: {e:#}", p.name));
        let fs = linspace(p.setup.f_lo, p.setup.f_hi, p.setup.mom_samples.min(31));
        let z: Vec<_> = fs.iter().map(|f| problem.solve(*f, false).unwrap().z).collect();
        let network = Network::from_z(fs, &z, g.ports.iter().map(|x| x.z0).collect()).interpolate(&linspace(p.setup.f_lo, p.setup.f_hi, 401));
        let mut far: Vec<Vec<crate::farfield::FarField>> = vec![Vec::new(); g.ports.len()];
        for f in &p.setup.far_freqs {
            for (port, ff) in problem.solve(*f, true).unwrap().far.into_iter().enumerate() {
                far[port].push(ff);
            }
        }
        (SimResult { network, far, far_f: p.setup.far_freqs.clone(), ..Default::default() }, problem.unknowns(), t0.elapsed().as_secs_f64())
    }

    /// A sample says what it's for, so it has to do it. These solve quickly enough to check
    /// on every test run; the slower ones ship solved, and those stored results are checked
    /// against the same figures in the app's tests.
    #[test]
    fn quick_samples_do_what_they_say() {
        let mut problems = Vec::new();
        let mut checked = 0;
        for s in all().iter().filter(|s| s.opens.solves_far_field()) {
            let p = s.project().unwrap();
            let g = p.geometry().unwrap();
            if p.solver_for(&g) == SolverKind::Fdtd {
                continue;
            }
            // A drawn sample is a promise about the drawing tools, so it's checked whatever its size.
            if s.draw.is_none() && MomProblem::new(&g, p.setup.f_hi, mom_opts(&p)).unwrap().unknowns() > QUICK_UNKNOWNS {
                continue;
            }
            let (r, n, secs) = solve(&p, &g);
            let f = s.figures(&r).unwrap();
            println!("{:18} {n:5} unk {secs:5.1} s worst {:6.1} dB gain {:5.1} dBi AR {:4.1} res {:?}", s.id, f.worst_db, f.gain_dbi, f.axial_ratio_db, f.resonance.map(|x| (x / 1e6).round()));
            problems.extend(s.shortfalls(&f).into_iter().map(|c| format!("{}: {c}", s.id)));
            checked += 1;
        }
        assert!(checked >= 10, "{checked}");
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// A reference design is what a designer starts from, so it's solved here whatever its size:
    /// the test above skips the large ones.
    #[test]
    fn every_reference_design_does_what_it_says() {
        let mut problems = Vec::new();
        for r in crate::references::REFERENCES.iter() {
            let s = by_id(r.sample).unwrap();
            if s.opens != Opens::FarField {
                continue;
            }
            let p = s.project().unwrap();
            let g = p.geometry().unwrap();
            if p.solver_for(&g) == SolverKind::Fdtd {
                problems.push(format!("{}: needs FDTD, so this test can't hold it to anything", s.id));
                continue;
            }
            let (result, n, secs) = solve(&p, &g);
            let f = s.figures(&result).unwrap();
            println!("{:20} {n:5} unk {secs:5.1} s worst {:6.1} dB gain {:5.1} dBi AR {:5.1} {} transfer {:?}", s.id, f.worst_db, f.gain_dbi, f.axial_ratio_db, if f.rhcp { "RHCP" } else { "LHCP" }, f.worst_transfer);
            problems.extend(s.shortfalls(&f).into_iter().map(|c| format!("{}: {c}", s.id)));
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Tries values without a rebuild, for setting a sample's numbers. Not a pass/fail:
    /// `SURVEY="rfid_panel:h=0.05*lambda0,L=0.42*lambda0;..." cargo test -p antenna_core --release samples::tests::survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn survey() {
        let list = std::env::var("SURVEY").unwrap_or_default();
        for case in list.split(';').filter(|c| !c.is_empty()) {
            let (id, sets) = case.split_once(':').unwrap_or((case, ""));
            let s = by_id(id).unwrap_or_else(|| panic!("no sample {id}"));
            let mut p = s.project().unwrap();
            for set in sets.split(',').filter(|x| !x.is_empty()) {
                let (k, v) = set.split_once('=').unwrap();
                p.params.iter_mut().find(|r| r.name == k).unwrap_or_else(|| panic!("{k}")).expr = v.to_string();
            }
            let g = p.geometry().unwrap_or_else(|e| panic!("{case}: {e:#}"));
            let (r, n, secs) = solve(&p, &g);
            let f = s.figures(&r).unwrap();
            println!("{case:60} {n:5} unk {secs:5.1} s worst {:6.1} dB gain {:5.1} dBi AR {:4.1} res {:?} {:?}", f.worst_db, f.gain_dbi, f.axial_ratio_db, f.resonance.map(|x| (x / 1e6).round()), s.shortfalls(&f));
        }
    }
}
