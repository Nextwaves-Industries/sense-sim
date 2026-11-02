//! Who made what this is built on, and on what terms. SolidCraft's own Help > About > Licences covers
//! its dependencies too; this is the same text, reachable from the Sense Sim side, plus the solvers.

use bevy_egui::egui::{self, RichText};

use crate::theme;

const SOLIDCRAFT_NOTICE: &str = include_str!("../../third_party/solidcraft/NOTICE");
const SOLIDCRAFT_THIRD_PARTY: &str = include_str!("../../third_party/solidcraft/THIRD-PARTY-LICENSES.txt");

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
            ui.add_space(8.0);
            ui.label(RichText::new("3D design: SolidCraft").strong());
            ui.label(SOLIDCRAFT_NOTICE);
            ui.add_space(8.0);
            egui::CollapsingHeader::new("SolidCraft's dependencies, in full").show(ui, |ui| {
                ui.label(RichText::new(SOLIDCRAFT_THIRD_PARTY).monospace().size(10.5).color(theme::WEAK));
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notices_are_all_there() {
        assert!(SOLIDCRAFT_NOTICE.contains("SolidCraft"));
        assert!(SOLIDCRAFT_THIRD_PARTY.len() > 1000);
        for name in ["Gmsh", "CalculiX", "OpenFOAM", "Elmer", "not approved or endorsed"] {
            assert!(SOLVERS.contains(name), "{name}");
        }
    }
}
