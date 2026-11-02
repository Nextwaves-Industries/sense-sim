//! Starter materials, with typical handbook values at room temperature. They're starting points:
//! the study shows them and they can be edited, and real data sheets should replace them for
//! anything that matters.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Solid {
    pub name: String,
    /// Pa
    pub young: f64,
    pub poisson: f64,
    /// kg/m3
    pub density: f64,
    /// W/m K
    pub conductivity: f64,
    /// J/kg K
    pub specific_heat: f64,
    /// 1/K
    pub expansion: f64,
    /// Pa. Zero: stays elastic.
    pub yield_stress: f64,
    /// S/m, for the electromagnetic analyses.
    pub electrical_conductivity: f64,
    pub relative_permeability: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fluid {
    pub name: String,
    /// kg/m3
    pub density: f64,
    /// Pa s
    pub viscosity: f64,
    /// W/m K
    pub conductivity: f64,
    /// J/kg K
    pub specific_heat: f64,
    /// 1/K, for buoyancy.
    pub expansion: f64,
    /// m/s. Zero for a liquid treated as incompressible.
    pub speed_of_sound: f64,
}

pub fn solids() -> Vec<Solid> {
    let s = |name: &str, young: f64, poisson, density, conductivity, specific_heat, expansion, yield_stress, sigma, mu_r| Solid {
        name: name.into(),
        young,
        poisson,
        density,
        conductivity,
        specific_heat,
        expansion,
        yield_stress,
        electrical_conductivity: sigma,
        relative_permeability: mu_r,
    };
    vec![
        s("Structural steel", 210e9, 0.30, 7850.0, 50.0, 480.0, 12e-6, 250e6, 6.2e6, 1000.0),
        s("Stainless steel 304", 193e9, 0.29, 8000.0, 16.2, 500.0, 17.3e-6, 215e6, 1.4e6, 1.02),
        s("Aluminium 6061-T6", 68.9e9, 0.33, 2700.0, 167.0, 896.0, 23.6e-6, 276e6, 2.5e7, 1.0),
        s("Copper", 120e9, 0.34, 8960.0, 390.0, 385.0, 16.6e-6, 70e6, 5.8e7, 1.0),
        s("Titanium Ti-6Al-4V", 113.8e9, 0.342, 4430.0, 6.7, 526.0, 8.6e-6, 880e6, 5.6e5, 1.0),
        s("ABS plastic", 2.3e9, 0.35, 1050.0, 0.17, 1400.0, 90e-6, 40e6, 1e-14, 1.0),
        s("FR-4 circuit board", 22e9, 0.14, 1850.0, 0.3, 1100.0, 15e-6, 0.0, 1e-13, 1.0),
    ]
}

pub fn fluids() -> Vec<Fluid> {
    vec![
        Fluid { name: "Air at 20 C".into(), density: 1.204, viscosity: 1.825e-5, conductivity: 0.0257, specific_heat: 1005.0, expansion: 3.43e-3, speed_of_sound: 343.0 },
        Fluid { name: "Water at 20 C".into(), density: 998.0, viscosity: 1.002e-3, conductivity: 0.598, specific_heat: 4182.0, expansion: 2.07e-4, speed_of_sound: 0.0 },
    ]
}

/// Air, as the insulating space of the electromagnetic analyses.
pub fn vacuum_like() -> Solid {
    Solid { name: "Air".into(), young: 0.0, poisson: 0.0, density: 1.204, conductivity: 0.0257, specific_heat: 1005.0, expansion: 0.0, yield_stress: 0.0, electrical_conductivity: 0.0, relative_permeability: 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_physically_plausible() {
        for s in solids() {
            assert!(s.young > 1e8 && s.young < 1e12, "{}", s.name);
            assert!(s.poisson > 0.0 && s.poisson < 0.5, "{}", s.name);
            assert!(s.density > 100.0, "{}", s.name);
            assert!(s.conductivity > 0.0 && s.specific_heat > 0.0 && s.expansion > 0.0, "{}", s.name);
        }
        for f in fluids() {
            assert!(f.density > 0.0 && f.viscosity > 0.0, "{}", f.name);
        }
    }
}
