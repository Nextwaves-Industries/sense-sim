//! Solid modelling for antennas: sketches with constraints, features in a tree, bodies as
//! named faces. Every dimension is an expression over the project's variables.

pub mod blend;
pub mod boolean;
pub mod mesh;
pub mod prims;
pub mod remesh;
pub mod spec;
pub mod sketch;
pub mod tree;
#[cfg(test)]
mod solve_tests;

pub use tree::{Anchor, AxisRef, Body, BodyKind, Built, CombineOp, CoordSys, Extent, Feature, Flat, Kind, Layout, Mode, Model, ObjectSteps, Output, PathRef, PlaneRef, Prim, SolvedSketch};

use anyhow::{Result, bail};

use crate::expr::{self, Vars};

/// A length as typed: a plain number is millimetres, the way a CAD field reads it. Anything
/// else is an expression, in SI, and may carry its own units.
pub fn length(text: &str, vars: &Vars) -> Result<f64> {
    let t = text.trim();
    if t.is_empty() {
        bail!("A dimension is empty.");
    }
    match t.parse::<f64>() {
        Ok(v) => Ok(v * 1e-3),
        Err(_) => expr::eval(t, vars),
    }
}

/// An angle as typed, in radians: a plain number is degrees. `45deg` and `pi/4` work too.
pub fn angle(text: &str, vars: &Vars) -> Result<f64> {
    let t = text.trim();
    if t.is_empty() {
        bail!("An angle is empty.");
    }
    match t.parse::<f64>() {
        Ok(v) => Ok(v.to_radians()),
        Err(_) => expr::eval(t, vars),
    }
}
