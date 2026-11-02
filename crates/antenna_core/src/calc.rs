//! Antenna and RF formulas for the assistant, so numbers are computed rather than
//! recalled. Every result names its assumptions. These are starting points for a
//! simulation: closed-form sizes are expected to be off by a few percent.

use std::f64::consts::PI;

use serde_json::{Value, json};

const C: f64 = 299_792_458.0;

pub const FORMULAS: &str = "wavelength {f, er}; match {s11_db | vswr | return_loss_db}; power {dbm | watts}; erp_eirp {erp_dbm | eirp_dbm}; fspl {f, d_m}; link_range {f, tx_dbm, gt_dbi, gr_dbi, sens_dbm, margin_db}; far_field {f, d_m}; dipole {f, velocity_factor}; monopole {f, velocity_factor}; patch {f, er, h_m, z0}; microstrip {er, h_m, z0 | w_m}; chu {f, a_m}; bode_fano {f, q, s11_db}; pol_mismatch {angle_deg | case: linear_circular | opposite_circular}; skin_depth {f, sigma}; thermal_noise {bw_hz, nf_db}. f is f_hz, f_mhz or f_ghz.";

fn num(i: &Value, key: &str) -> Option<f64> {
    i[key].as_f64()
}

fn need(i: &Value, key: &str) -> Result<f64, String> {
    num(i, key).ok_or_else(|| format!("give {key}"))
}

fn positive(i: &Value, key: &str) -> Result<f64, String> {
    let v = need(i, key)?;
    if v > 0.0 && v.is_finite() { Ok(v) } else { Err(format!("{key} must be above 0")) }
}

fn freq(i: &Value) -> Result<f64, String> {
    let f = num(i, "f_hz").or(num(i, "f_mhz").map(|v| v * 1e6)).or(num(i, "f_ghz").map(|v| v * 1e9)).ok_or("give the frequency as f_hz, f_mhz or f_ghz")?;
    if f > 0.0 { Ok(f) } else { Err("the frequency must be above 0".to_string()) }
}

fn round(v: f64) -> f64 {
    if !v.is_finite() || v == 0.0 {
        return v;
    }
    let scale = 10f64.powi(5 - v.abs().log10().floor() as i32);
    (v * scale).round() / scale
}

/// Half-power reflection facts from a reflection magnitude.
fn match_from_gamma(gamma: f64) -> Value {
    let g = gamma.clamp(0.0, 1.0);
    json!({
        "s11_db": round(20.0 * g.max(1e-12).log10()),
        "return_loss_db": round(-20.0 * g.max(1e-12).log10()),
        "gamma": round(g),
        "vswr": if g >= 1.0 { Value::Null } else { json!(round((1.0 + g) / (1.0 - g))) },
        "reflected_power_pct": round(g * g * 100.0),
        "mismatch_loss_db": if g >= 1.0 { Value::Null } else { json!(round(-10.0 * (1.0 - g * g).log10())) },
    })
}

/// Rectangular patch size (Balanis, transmission-line model).
pub fn patch(f: f64, er: f64, h: f64) -> (f64, f64, f64, f64) {
    let w = C / (2.0 * f) * (2.0 / (er + 1.0)).sqrt();
    let ereff = (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * (1.0 + 12.0 * h / w).powf(-0.5);
    let dl = 0.412 * h * (ereff + 0.3) * (w / h + 0.264) / ((ereff - 0.258) * (w / h + 0.8));
    let l = C / (2.0 * f * ereff.sqrt()) - 2.0 * dl;
    (w, l, ereff, dl)
}

/// Microstrip impedance and effective permittivity (Hammerstad, zero-thickness trace).
pub fn microstrip_z0(w: f64, h: f64, er: f64) -> (f64, f64) {
    let u = w / h;
    if u <= 1.0 {
        let e = (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * ((1.0 + 12.0 / u).powf(-0.5) + 0.04 * (1.0 - u).powi(2));
        (60.0 / e.sqrt() * (8.0 / u + u / 4.0).ln(), e)
    } else {
        let e = (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * (1.0 + 12.0 / u).powf(-0.5);
        (120.0 * PI / (e.sqrt() * (u + 1.393 + 0.667 * (u + 1.444).ln())), e)
    }
}

/// Trace width for `z0`, by bisection in log width.
pub fn microstrip_width(z0: f64, h: f64, er: f64) -> f64 {
    let (mut lo, mut hi) = ((h * 0.01).ln(), (h * 50.0).ln());
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        if microstrip_z0(mid.exp(), h, er).0 > z0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    ((lo + hi) / 2.0).exp()
}

/// Chu's minimum Q of an antenna inside a sphere with `ka`.
pub fn chu_q(ka: f64) -> f64 {
    1.0 / ka.powi(3) + 1.0 / ka
}

/// Harrington's normal directivity limit for a sphere with `ka`, as a ratio.
pub fn harrington_directivity(ka: f64) -> f64 {
    ka * ka + 2.0 * ka
}

/// Fractional bandwidth a tuned antenna of quality `q` holds within a VSWR.
pub fn fractional_bandwidth(q: f64, vswr: f64) -> f64 {
    (vswr - 1.0) / (q * vswr.sqrt())
}

/// Runs one formula by name. `Err` is a message for the model.
pub fn run(formula: &str, i: &Value) -> Result<Value, String> {
    let lambda = |f: f64| C / f;
    Ok(match formula {
        "wavelength" => {
            let f = freq(i)?;
            let er = num(i, "er").unwrap_or(1.0);
            let l = lambda(f) / er.sqrt();
            json!({ "wavelength_m": round(l), "half_mm": round(l * 500.0), "quarter_mm": round(l * 250.0), "note": "In a medium of relative permittivity er." })
        }
        "match" => {
            let gamma = if let Some(s) = num(i, "s11_db") {
                10f64.powf(s.min(0.0) / 20.0)
            } else if let Some(r) = num(i, "return_loss_db") {
                10f64.powf(-r.abs() / 20.0)
            } else if let Some(v) = num(i, "vswr") {
                if v < 1.0 {
                    return Err("VSWR is at least 1".to_string());
                }
                (v - 1.0) / (v + 1.0)
            } else {
                return Err("give s11_db, return_loss_db or vswr".to_string());
            };
            match_from_gamma(gamma)
        }
        "power" => {
            let dbm = if let Some(d) = num(i, "dbm") { d } else { 10.0 * (positive(i, "watts")? * 1e3).log10() };
            json!({ "dbm": round(dbm), "dbw": round(dbm - 30.0), "watts": round(10f64.powf((dbm - 30.0) / 10.0)) })
        }
        "erp_eirp" => {
            let eirp = if let Some(e) = num(i, "eirp_dbm") { e } else { need(i, "erp_dbm")? + 2.15 };
            json!({ "eirp_dbm": round(eirp), "erp_dbm": round(eirp - 2.15), "note": "EIRP = ERP + 2.15 dB." })
        }
        "fspl" => {
            let (f, d) = (freq(i)?, positive(i, "d_m")?);
            json!({ "fspl_db": round(20.0 * (4.0 * PI * d / lambda(f)).log10()), "note": "Free space only: walls, multipath and fading come on top." })
        }
        "link_range" => {
            let f = freq(i)?;
            let budget = need(i, "tx_dbm")? + num(i, "gt_dbi").unwrap_or(0.0) + num(i, "gr_dbi").unwrap_or(0.0) - need(i, "sens_dbm")? - num(i, "margin_db").unwrap_or(0.0);
            json!({ "budget_db": round(budget), "range_m": round(lambda(f) / (4.0 * PI) * 10f64.powf(budget / 20.0)), "note": "Free-space range: an upper bound indoors." })
        }
        "far_field" => {
            let (f, d) = (freq(i)?, positive(i, "d_m")?);
            let l = lambda(f);
            let fraunhofer = 2.0 * d * d / l;
            json!({ "fraunhofer_m": round(fraunhofer), "min_far_field_m": round(fraunhofer.max(5.0 * d).max(1.6 * l)), "note": "Largest of 2D^2/lambda, 5D and 1.6 lambda." })
        }
        "dipole" | "monopole" => {
            let f = freq(i)?;
            let vf = num(i, "velocity_factor").unwrap_or(0.95);
            let share = if formula == "dipole" { 0.5 } else { 0.25 };
            json!({ "length_mm": round(share * lambda(f) * vf * 1e3), "free_space_mm": round(share * lambda(f) * 1e3), "velocity_factor": vf, "input_resistance_ohm": if formula == "dipole" { 73 } else { 36 }, "note": "Starting length: the factor depends on wire thickness. Tune by simulation." })
        }
        "patch" => {
            let (f, er, h) = (freq(i)?, positive(i, "er")?, positive(i, "h_m")?);
            let z0 = num(i, "z0").unwrap_or(50.0);
            let (w, l, ereff, dl) = patch(f, er, h);
            // A closed form for the edge resistance, then the cos^2 inset law.
            let edge = 90.0 * er * er / (er - 1.0).max(1e-3) * (l / w).powi(2);
            let inset = if edge > z0 { Some((l / PI) * (z0 / edge).sqrt().acos()) } else { None };
            json!({
                "width_mm": round(w * 1e3), "length_mm": round(l * 1e3), "ereff": round(ereff), "delta_l_mm": round(dl * 1e3),
                "edge_resistance_ohm": round(edge),
                "inset_from_edge_mm": inset.map(|v| round(v * 1e3)),
                "bandwidth_note": "Bandwidth grows roughly with substrate height: thin patches are a few percent or less.",
                "note": "Transmission-line model, from Balanis. Expect to retune the length by simulation. The edge resistance is rough, and unreliable for er near 1."
            })
        }
        "microstrip" => {
            let (er, h) = (positive(i, "er")?, positive(i, "h_m")?);
            let w = match (num(i, "w_m"), num(i, "z0")) {
                (Some(w), _) if w > 0.0 => w,
                (_, Some(z)) if z > 0.0 => microstrip_width(z, h, er),
                _ => return Err("give z0 or w_m".to_string()),
            };
            let (z, e) = microstrip_z0(w, h, er);
            json!({ "width_mm": round(w * 1e3), "z0_ohm": round(z), "ereff": round(e), "note": "Hammerstad, zero trace thickness, no dispersion." })
        }
        "chu" => {
            let (f, a) = (freq(i)?, positive(i, "a_m")?);
            let ka = 2.0 * PI * a / lambda(f);
            let q = chu_q(ka);
            json!({
                "ka": round(ka),
                "min_q": round(q),
                "max_fractional_bandwidth_vswr2": round(fractional_bandwidth(q, 2.0)),
                "max_normal_directivity_dbi": round(10.0 * harrington_directivity(ka).max(1.5).log10()),
                "electrically_small": ka < 0.5,
                "note": "a is the radius of the smallest sphere around the antenna and its currents, including the ground plane if it radiates. Limits for a lossless single-mode antenna; loss can widen bandwidth at the cost of efficiency."
            })
        }
        "bode_fano" => {
            let (f, q) = (freq(i)?, positive(i, "q")?);
            let s = num(i, "s11_db").unwrap_or(-10.0);
            let gamma = 10f64.powf(s.min(-0.01) / 20.0);
            let df = crate::matching::bode_fano_bandwidth(q, f, gamma);
            json!({ "max_bandwidth_hz": round(df), "max_fractional_bandwidth": round(df / f), "note": "The widest band any lossless matching network can hold at this S11. Real networks reach less." })
        }
        "pol_mismatch" => {
            let loss = match i["case"].as_str() {
                Some("linear_circular") => 3.01,
                Some("opposite_circular") => return Ok(json!({ "loss_db": Value::Null, "note": "Ideal opposite circular polarisations are orthogonal: the loss is unbounded in theory, typically 15-25 dB in practice (rule of thumb)." })),
                Some(other) => return Err(format!("unknown case '{other}'")),
                None => {
                    let a = need(i, "angle_deg")?.to_radians().cos().abs();
                    if a < 1e-9 {
                        return Ok(json!({ "loss_db": Value::Null, "note": "Crossed linear antennas are orthogonal: the loss is unbounded in theory." }));
                    }
                    -20.0 * a.log10()
                }
            };
            json!({ "loss_db": round(loss) })
        }
        "skin_depth" => {
            let (f, sigma) = (freq(i)?, num(i, "sigma").unwrap_or(5.8e7));
            if sigma <= 0.0 {
                return Err("sigma must be above 0".to_string());
            }
            json!({ "skin_depth_um": round((1.0 / (PI * f * 4e-7 * PI * sigma)).sqrt() * 1e6), "note": "Non-magnetic conductor; default sigma is copper." })
        }
        "thermal_noise" => {
            let bw = positive(i, "bw_hz")?;
            let nf = num(i, "nf_db").unwrap_or(0.0);
            json!({ "noise_floor_dbm": round(-174.0 + 10.0 * bw.log10() + nf), "note": "At 290 K: -174 dBm/Hz + 10 log(bandwidth) + noise figure." })
        }
        other => return Err(format!("unknown formula '{other}'. Formulas: {FORMULAS}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(v: &Value, want: f64, tol: f64) {
        let got = v.as_f64().unwrap_or(f64::NAN);
        assert!((got - want).abs() <= tol, "got {got}, want {want}");
    }

    /// Balanis, Antenna Theory, Example 14.1: 10 GHz, er 2.2, h 1.588 mm.
    #[test]
    fn patch_matches_the_textbook_example() {
        let r = run("patch", &json!({ "f_ghz": 10.0, "er": 2.2, "h_m": 1.588e-3 })).unwrap();
        close(&r["width_mm"], 11.86, 0.02);
        close(&r["ereff"], 1.972, 0.005);
        close(&r["delta_l_mm"], 0.81, 0.01);
        close(&r["length_mm"], 9.06, 0.03);
    }

    #[test]
    fn match_conversions_agree_both_ways() {
        let a = run("match", &json!({ "vswr": 2.0 })).unwrap();
        close(&a["s11_db"], -9.542, 0.001);
        close(&a["reflected_power_pct"], 11.11, 0.01);
        close(&a["mismatch_loss_db"], 0.5115, 0.001);
        let b = run("match", &json!({ "s11_db": -10.0 })).unwrap();
        close(&b["vswr"], 1.925, 0.001);
        close(&run("match", &json!({ "return_loss_db": 10.0 })).unwrap()["vswr"], 1.925, 0.001);
        assert!(run("match", &json!({ "vswr": 0.5 })).is_err());
        assert!(run("match", &json!({ "s11_db": 0.0 })).unwrap()["vswr"].is_null(), "total reflection has no finite VSWR");
    }

    #[test]
    fn free_space_loss_and_range_are_inverse() {
        close(&run("fspl", &json!({ "f_ghz": 2.4, "d_m": 100.0 })).unwrap()["fspl_db"], 80.05, 0.02);
        let r = run("link_range", &json!({ "f_ghz": 2.4, "tx_dbm": 0.0, "sens_dbm": -80.05 })).unwrap();
        close(&r["range_m"], 100.0, 0.2);
    }

    /// FR-4 at 1.6 mm: published calculators give about 3 mm for 50 ohm.
    #[test]
    fn microstrip_width_round_trips() {
        let r = run("microstrip", &json!({ "er": 4.4, "h_m": 1.6e-3, "z0": 50.0 })).unwrap();
        close(&r["width_mm"], 3.06, 0.1);
        close(&r["z0_ohm"], 50.0, 0.01);
        let w = r["width_mm"].as_f64().unwrap() * 1e-3;
        close(&run("microstrip", &json!({ "er": 4.4, "h_m": 1.6e-3, "w_m": w })).unwrap()["z0_ohm"], 50.0, 0.1);
    }

    #[test]
    fn chu_limit_at_ka_half() {
        let r = run("chu", &json!({ "f_ghz": 1.0, "a_m": 0.5 * C / 1e9 / (2.0 * PI) })).unwrap();
        close(&r["ka"], 0.5, 1e-4);
        close(&r["min_q"], 10.0, 1e-3);
        close(&r["max_fractional_bandwidth_vswr2"], 0.0707, 1e-3);
    }

    #[test]
    fn dipole_and_units() {
        close(&run("dipole", &json!({ "f_mhz": 915.0 })).unwrap()["length_mm"], 155.6, 0.2);
        close(&run("monopole", &json!({ "f_hz": 915e6, "velocity_factor": 1.0 })).unwrap()["length_mm"], 81.9, 0.1);
        assert!(run("dipole", &json!({})).is_err());
    }

    #[test]
    fn other_formulas() {
        close(&run("power", &json!({ "watts": 1.0 })).unwrap()["dbm"], 30.0, 1e-6);
        close(&run("erp_eirp", &json!({ "erp_dbm": 27.0 })).unwrap()["eirp_dbm"], 29.15, 1e-6);
        close(&run("far_field", &json!({ "f_ghz": 3.0, "d_m": 1.0 })).unwrap()["fraunhofer_m"], 20.01, 0.02);
        close(&run("pol_mismatch", &json!({ "angle_deg": 45.0 })).unwrap()["loss_db"], 3.01, 0.01);
        assert!(run("pol_mismatch", &json!({ "angle_deg": 90.0 })).unwrap()["loss_db"].is_null());
        close(&run("skin_depth", &json!({ "f_ghz": 1.0 })).unwrap()["skin_depth_um"], 2.09, 0.01);
        close(&run("thermal_noise", &json!({ "bw_hz": 1e6, "nf_db": 6.0 })).unwrap()["noise_floor_dbm"], -108.0, 1e-6);
    }

    #[test]
    fn an_unknown_formula_lists_the_real_ones() {
        assert!(run("nope", &json!({})).unwrap_err().contains("patch"));
    }
}
