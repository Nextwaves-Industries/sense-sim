//! Who made the solvers the simulations run, and on what terms.

use bevy_egui::egui::{self, RichText};


/// The separate programs the simulations run. None is linked into Sense Sim or sent with it: each is
/// fetched from its own project, with consent, and runs as a process of its own.
const SOLVERS: &str = "Gmsh (gmsh.info): GNU General Public License, version 2 or later, with exceptions.\n\
CalculiX (dhondt.de): GNU General Public License, version 2 or later.\n\
OpenFOAM (openfoam.com): GNU General Public License, version 3.\n\
Elmer (elmerfem.org): GNU General Public License, version 2 or later; its libraries LGPL.\n\n\
Each runs as a separate program. None is linked into Sense Sim or shipped with it: they're downloaded from their own projects when you ask, and their source is available there.\n\n\
This offering is not approved or endorsed by OpenCFD Limited, producer and distributor of the OpenFOAM software via www.openfoam.com, and owner of the OPENFOAM\u{ae} and OpenCFD\u{ae} trade marks.";

pub fn show(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("Open-source notices").open(open).default_width(560.0).default_height(460.0).resizable(true).show(ctx, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new("Solvers").strong());
            ui.label(SOLVERS);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notices_are_all_there() {

        for name in ["Gmsh", "CalculiX", "OpenFOAM", "Elmer", "not approved or endorsed"] {
            assert!(SOLVERS.contains(name), "{name}");
        }
    }
}
