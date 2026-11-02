//! The app's workbenches, like the environments of a CAD package: each job has
//! its own ribbon tools and project tree, over the one scene.
//!
//! A workbench remembers the card it had open. Anything may still open a card or
//! the Studio directly (the assistant, scripted runs, a button on a device); the
//! workbench that owns it then comes to the front, see [`follow`].

use super::LeftPanelSlot;
use super::topbar::Flags;
use crate::rtls::ui::{Page, RtlsUiState, Tab};
use crate::theme::icon;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Workbench {
    /// Build the site: assets, walls, arranging.
    #[default]
    Layout,
    /// Model parts in 3D, in SolidCraft.
    Design,
    /// Put the positioning devices in it.
    Placement,
    /// Design an antenna, in Antenna Studio.
    Antenna,
    /// See how it performs: live tests, RF solvers, predictions.
    Simulation,
    /// Run connected equipment.
    Plant,
}

impl Workbench {
    pub const ALL: [Workbench; 6] = [Workbench::Layout, Workbench::Design, Workbench::Placement, Workbench::Antenna, Workbench::Simulation, Workbench::Plant];

    pub fn name(self) -> &'static str {
        match self {
            Workbench::Layout => "Layout",
            Workbench::Design => "3D Design",
            Workbench::Placement => "Placement",
            Workbench::Antenna => "Antenna Design",
            Workbench::Simulation => "Simulation",
            Workbench::Plant => "Plant",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Workbench::Layout => icon::CUBE,
            Workbench::Design => icon::CUBE_TRANSPARENT,
            Workbench::Placement => icon::BROADCAST,
            Workbench::Antenna => icon::TARGET,
            Workbench::Simulation => icon::WAVE_SINE,
            Workbench::Plant => icon::DROP,
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Workbench::Layout => "Build the site: assets, walls and arranging",
            Workbench::Design => "Model parts in 3D: sketches, solids and assemblies",
            Workbench::Placement => "Place positioning devices and plan coverage",
            Workbench::Antenna => "Design an antenna: models, solvers, results, optimisation",
            Workbench::Simulation => "Test the system live, run RF solvers and check predictions",
            Workbench::Plant => "Run connected equipment: ports, flow and alarms",
        }
    }

    pub fn available(self) -> bool {
        self != Workbench::Antenna || cfg!(feature = "antenna-studio")
    }

    /// This workbench's tree offers the asset library.
    pub fn has_library(self) -> bool {
        matches!(self, Workbench::Layout | Workbench::Placement | Workbench::Plant)
    }

    /// `--open bench-<name>`, for scripted runs.
    pub fn from_name(s: &str) -> Option<Workbench> {
        Some(match s {
            "layout" => Workbench::Layout,
            "design" => Workbench::Design,
            "placement" => Workbench::Placement,
            "antenna" => Workbench::Antenna,
            "simulation" => Workbench::Simulation,
            "plant" => Workbench::Plant,
            _ => return None,
        })
    }

    /// The workbench a card belongs to.
    pub fn of_page(page: Page) -> Workbench {
        match page {
            Page::Deploy | Page::Tool(Tab::Devices | Tab::Inspector | Tab::Planning | Tab::Settings) => Workbench::Placement,
            Page::Test | Page::OpenEms | Page::Fdtd | Page::Outcomes | Page::Tool(Tab::Live | Tab::Rf | Tab::Outputs) => Workbench::Simulation,
        }
    }

    /// The card that opens the first time this workbench does.
    fn first_card(self) -> Option<Page> {
        match self {
            Workbench::Placement => Some(Page::Deploy),
            Workbench::Simulation => Some(Page::Test),
            _ => None,
        }
    }
}

/// What the design strip asks of the 3D Design workspace.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DesignCommand {
    New,
    Save,
    Undo,
    Redo,
    Settings,
    /// Show this open design.
    Show(usize),
    /// Close this open design; it asks first if there are unsaved changes.
    Close(usize),
    /// The design's File menu, under a button at this x, in window points.
    FileMenu(f32),
}

/// How the design workspace stands, for the design strip. Empty until it has started.
#[derive(Clone, Default)]
pub struct DesignInfo {
    /// The open designs: name, and whether there are unsaved changes.
    pub designs: Vec<(String, bool)>,
    pub active: usize,
    pub can_save: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub file_menu_open: bool,
}

/// What the File menu asks of the open antenna project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StudioFile {
    Save,
    SaveAs,
    Open,
}

/// The open antenna project, for the top bar.
#[derive(Clone, Default)]
pub struct StudioDoc {
    pub name: String,
    /// Its file's name, once it has one.
    pub file: Option<String>,
    pub path: Option<String>,
    /// Changed since that file was written.
    pub unsaved: bool,
}

/// The workbench in front and what the others had open.
pub struct Benches {
    pub current: Workbench,
    /// The design code is in this build; until it says so the tab stays hidden.
    pub design_ready: bool,
    /// Where leaving a full-canvas workbench goes back to.
    previous: Workbench,
    cards: [Option<Page>; Workbench::ALL.len()],
}

impl Default for Benches {
    fn default() -> Self {
        Self { current: Workbench::default(), design_ready: false, previous: Workbench::default(), cards: Workbench::ALL.map(Workbench::first_card) }
    }
}

impl LeftPanelSlot {
    /// This workbench has a tab in this build.
    pub fn shows(&self, bench: Workbench) -> bool {
        match bench {
            Workbench::Antenna => cfg!(feature = "antenna-studio"),
            Workbench::Design => self.bench.design_ready,
            _ => true,
        }
    }

    /// A workspace fills the canvas: the scene and its cards step aside.
    pub fn canvas_taken(&self) -> bool {
        self.studio_open || self.design_open
    }
}

/// New, Open, Save and Close act on what is in front. A scene hidden behind a workspace
/// must not be saved, or replaced, by a command meant for the workspace.
pub fn route_file_commands(flags: &mut Flags, slot: &mut LeftPanelSlot) {
    if slot.studio_open {
        if flags.new {
            slot.studio_goto.push("gallery".to_string());
        }
        if flags.open {
            slot.studio_file.push(StudioFile::Open);
        }
        if flags.save_as {
            slot.studio_file.push(StudioFile::SaveAs);
        } else if flags.save {
            slot.studio_file.push(StudioFile::Save);
        }
    } else if slot.design_open {
        if flags.new {
            slot.design_commands.push(DesignCommand::New);
        }
        if flags.save {
            slot.design_commands.push(DesignCommand::Save);
        }
        // Save As and Open are in the design's own File menu.
        if flags.open || flags.save_as {
            let x = slot.design_file_button.map_or(0.0, |r| r.left());
            slot.design_commands.push(DesignCommand::FileMenu(x));
        }
        if flags.close {
            slot.design_commands.push(DesignCommand::Close(slot.design_info.active));
        }
    } else {
        return;
    }
    *flags = Flags { new: false, open: false, save: false, save_as: false, close: false };
}

/// Brings a workbench to the front, with the card it last had open.
pub fn switch(slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState, to: Workbench) {
    let from = slot.bench.current;
    if from == to {
        return;
    }
    let page = rtls.local.page();
    if Workbench::of_page(page) == from {
        slot.bench.cards[from as usize] = slot.rtls_open.then_some(page);
    }
    let card = slot.bench.cards[to as usize];
    if let Some(p) = card {
        rtls.local.set_page(p);
    }
    slot.rtls_open = card.is_some();
    slot.studio_open = to == Workbench::Antenna;
    slot.design_open = to == Workbench::Design;
    slot.bench.previous = from;
    slot.bench.current = to;
}

/// Something outside the ribbon opened a card or a full-canvas workspace, or closed
/// one: the workbench that owns it comes to the front.
pub fn follow(slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    let current = slot.bench.current;
    if slot.studio_open && current != Workbench::Antenna {
        switch(slot, rtls, Workbench::Antenna);
    } else if slot.design_open && current != Workbench::Design {
        switch(slot, rtls, Workbench::Design);
    } else if (current == Workbench::Antenna && !slot.studio_open) || (current == Workbench::Design && !slot.design_open) {
        let back = slot.bench.previous;
        switch(slot, rtls, back);
    } else if slot.rtls_open && Workbench::of_page(rtls.local.page()) != current {
        let page = rtls.local.page();
        switch(slot, rtls, Workbench::of_page(page));
        rtls.local.set_page(page);
        slot.rtls_open = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> (LeftPanelSlot, RtlsUiState) {
        (LeftPanelSlot::default(), RtlsUiState::default())
    }

    #[test]
    fn opens_its_first_card_then_remembers_the_last_one() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Placement);
        assert!(slot.rtls_open && rtls.local.page() == Page::Deploy);

        rtls.local.set_page(Page::Tool(Tab::Planning));
        switch(&mut slot, &mut rtls, Workbench::Layout);
        assert!(!slot.rtls_open);

        switch(&mut slot, &mut rtls, Workbench::Placement);
        assert!(slot.rtls_open && rtls.local.page() == Page::Tool(Tab::Planning));
    }

    #[test]
    fn a_closed_card_stays_closed() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Simulation);
        slot.rtls_open = false;
        switch(&mut slot, &mut rtls, Workbench::Plant);
        switch(&mut slot, &mut rtls, Workbench::Simulation);
        assert!(!slot.rtls_open);
    }

    #[test]
    fn a_card_opened_from_elsewhere_brings_its_workbench_and_keeps_that_card() {
        let (mut slot, mut rtls) = ui();
        rtls.local.set_page(Page::Fdtd);
        slot.rtls_open = true;
        follow(&mut slot, &mut rtls);
        assert!(slot.bench.current == Workbench::Simulation);
        assert!(slot.rtls_open && rtls.local.page() == Page::Fdtd);
    }

    #[test]
    fn the_studio_opens_its_workbench_and_closing_it_goes_back() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Simulation);

        slot.studio_open = true;
        follow(&mut slot, &mut rtls);
        assert!(slot.bench.current == Workbench::Antenna && !slot.rtls_open);

        slot.studio_open = false;
        follow(&mut slot, &mut rtls);
        assert!(slot.bench.current == Workbench::Simulation && slot.rtls_open);
    }

    #[test]
    fn the_design_workspace_opens_its_workbench_and_closing_it_goes_back() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Simulation);

        // A simulation asks for faces to be picked.
        slot.design_open = true;
        follow(&mut slot, &mut rtls);
        assert!(slot.bench.current == Workbench::Design && !slot.rtls_open);

        slot.design_open = false;
        follow(&mut slot, &mut rtls);
        assert!(slot.bench.current == Workbench::Simulation && slot.rtls_open);
    }

    #[test]
    fn the_full_canvas_workspaces_replace_each_other() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Design);
        slot.studio_open = true;
        follow(&mut slot, &mut rtls);
        assert!(slot.bench.current == Workbench::Antenna && slot.studio_open && !slot.design_open);
    }

    #[test]
    fn design_stays_hidden_until_its_code_is_there() {
        let (mut slot, _) = ui();
        assert!(!slot.shows(Workbench::Design));
        slot.bench.design_ready = true;
        assert!(slot.shows(Workbench::Design));
    }

    fn all_flags() -> Flags {
        Flags { new: true, open: true, save: true, save_as: false, close: true }
    }

    #[test]
    fn file_commands_in_the_studio_never_reach_the_hidden_scene() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Antenna);
        let mut flags = all_flags();
        route_file_commands(&mut flags, &mut slot);
        assert!(!flags.new && !flags.open && !flags.save && !flags.save_as && !flags.close);
        assert_eq!(slot.studio_file, [StudioFile::Open, StudioFile::Save]);
        assert_eq!(slot.studio_goto, ["gallery"]);
    }

    #[test]
    fn file_commands_in_the_design_workspace_go_to_the_design() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Design);
        let mut flags = all_flags();
        route_file_commands(&mut flags, &mut slot);
        assert!(!flags.save && !flags.close);
        assert!(slot.design_commands.contains(&DesignCommand::Save));
    }

    #[test]
    fn file_commands_stay_with_the_scene_elsewhere() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Placement);
        let mut flags = all_flags();
        route_file_commands(&mut flags, &mut slot);
        assert!(flags.save && flags.new);
        assert!(slot.studio_file.is_empty());
    }

    #[test]
    fn leaving_the_studio_for_another_workbench_closes_it() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Antenna);
        assert!(slot.studio_open);
        switch(&mut slot, &mut rtls, Workbench::Plant);
        assert!(!slot.studio_open);
    }
}
