//! Outcome recipes: each turns solver results into the number a product actually
//! lives or dies by, and hands it to the [`proof`] loop to get a range, a verdict
//! and a place to compare against measurements.
//!
//! - [`uwb_bias`]: how an antenna's shape shifts UWB ranges with direction.
//! - [`shelf`]: read rate and stray reads of an RFID antenna on a real shelf.
//! - [`aoa`]: angle error of a direction-finding array, and a calibration table.
//! - [`integrate`]: what the enclosure and nearby parts do to an antenna, and fixes.
//! - [`tagging`]: which RFID tag and position work on a given product.
//! - [`coexist`]: how radios in one device hurt each other.
//! - [`fit`]: fitting a model to a VNA measurement, and yield against field performance.
//! - [`gate`]: read rate of an RFID security gate under its excitation states.
//! - [`tunnel`]: RFID on a conveyor: belt speed against missed and misplaced reads.
//! - [`medical`]: which nearby networks to test a wireless medical device against.
//! - [`sar`]: a design-stage check of RF exposure from a body-worn device.

pub mod aoa;
pub mod coexist;
pub mod fit;
pub mod gate;
pub mod integrate;
pub mod medical;
pub mod sar;
pub mod shelf;
pub mod tagging;
pub mod tunnel;
pub mod uwb_bias;

use anyhow::Result;

use crate::farfield::FarField;
use crate::geometry::Geometry;
use crate::linspace;
use crate::mom::{MomOptions, MomProblem};
use crate::sparam::Network;

/// Far fields of port `port` at each of `freqs`, with the method of moments.
pub fn mom_far_fields(geom: &Geometry, freqs: &[f64], port: usize, opts: MomOptions) -> Result<Vec<FarField>> {
    let f_max = freqs.iter().copied().fold(0.0, f64::max);
    let problem = MomProblem::new(geom, f_max, opts)?;
    freqs
        .iter()
        .map(|f| {
            let mut pt = problem.solve(*f, true)?;
            if port >= pt.far.len() {
                anyhow::bail!("port {} doesn't exist (the model has {})", port + 1, pt.far.len());
            }
            Ok(pt.far.swap_remove(port))
        })
        .collect()
}

/// The S-parameters over `f_lo..f_hi` with the method of moments, solved at
/// `samples` frequencies and interpolated to `points`.
pub fn mom_network(geom: &Geometry, f_lo: f64, f_hi: f64, samples: usize, points: usize, opts: MomOptions) -> Result<Network> {
    let problem = MomProblem::new(geom, f_hi, opts)?;
    let fs = linspace(f_lo, f_hi, samples.max(3));
    let z: Vec<_> = fs.iter().map(|f| problem.solve(*f, false).map(|p| p.z)).collect::<Result<_>>()?;
    let z0: Vec<f64> = geom.ports.iter().map(|p| p.z0).collect();
    Ok(Network::from_z(fs, &z, z0).interpolate(&linspace(f_lo, f_hi, points.max(2))))
}

/// Unwraps a phase sequence (radians) so neighbours differ by less than π.
pub(crate) fn unwrap(phase: &mut [f64]) {
    use std::f64::consts::TAU;
    for i in 1..phase.len() {
        let d = phase[i] - phase[i - 1];
        phase[i] -= (d / TAU).round() * TAU;
    }
}
