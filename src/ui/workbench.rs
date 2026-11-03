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
    /// Put the positioning devices in it.
    Placement,
    /// Design an antenna, in Antenna Studio.
    Antenna,
    /// See how it performs: live tests, RF solvers, predictions.
    Simulation,
    /// Run connected equipment.
    Plant,
    /// See the real hardware found on the network.
    Factory,
}

impl Workbench {
    pub const ALL: [Workbench; 6] = [Workbench::Layout, Workbench::Placement, Workbench::Antenna, Workbench::Simulation, Workbench::Plant, Workbench::Factory];

    pub fn name(self) -> &'static str {
        match self {
            Workbench::Layout => "Layout",
            Workbench::Placement => "Placement",
            Workbench::Antenna => "Antenna Design",
            Workbench::Simulation => "Simulation",
            Workbench::Plant => "Plant",
            Workbench::Factory => "Factory",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Workbench::Layout => icon::CUBE,
            Workbench::Placement => icon::BROADCAST,
            Workbench::Antenna => icon::TARGET,
            Workbench::Simulation => icon::WAVE_SINE,
            Workbench::Plant => icon::DROP,
            Workbench::Factory => icon::FACTORY,
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Workbench::Layout => "Build the site: assets, walls and arranging",
            Workbench::Placement => "Place positioning devices and plan coverage",
            Workbench::Antenna => "Design an antenna: models, solvers, results, optimisation",
            Workbench::Simulation => "Test the system live, run RF solvers and check predictions",
            Workbench::Plant => "Run connected equipment: ports, flow and alarms",
            Workbench::Factory => "See the real hardware on the network: how many of what, and how it is doing",
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
            "placement" => Workbench::Placement,
            "antenna" => Workbench::Antenna,
            "simulation" => Workbench::Simulation,
            "plant" => Workbench::Plant,
            "factory" => Workbench::Factory,
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
    /// A workspace asked for this workbench, for `follow` to bring to the front.
    pub request: Option<Workbench>,
    /// Where leaving a full-canvas workbench goes back to.
    previous: Workbench,
    cards: [Option<Page>; Workbench::ALL.len()],
}

impl Default for Benches {
    fn default() -> Self {
        Self { current: Workbench::default(), request: None, previous: Workbench::default(), cards: Workbench::ALL.map(Workbench::first_card) }
    }
}

impl LeftPanelSlot {
    /// This workbench has a tab in this build.
    pub fn shows(&self, bench: Workbench) -> bool {
        match bench {
            Workbench::Antenna => cfg!(feature = "antenna-studio"),
            _ => true,
        }
    }

    /// A workspace fills the canvas: the scene and its cards step aside.
    pub fn canvas_taken(&self) -> bool {
        self.studio_open
    }
}

/// New, Open, Save and Close act on what is in front. A scene hidden behind a workspace
/// must not be saved, or replaced, by a command meant for the workspace.
pub fn route_file_commands(flags: &mut Flags, slot: &mut LeftPanelSlot) {
    if !slot.studio_open {
        return;
    }
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
    slot.bench.previous = from;
    slot.bench.current = to;
}

/// Something outside the ribbon opened a card or a full-canvas workspace, or closed
/// one: the workbench that owns it comes to the front.
pub fn follow(slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    if let Some(to) = slot.bench.request.take() {
        switch(slot, rtls, to);
        return;
    }
    let current = slot.bench.current;
    if slot.studio_open && current != Workbench::Antenna {
        switch(slot, rtls, Workbench::Antenna);
    } else if current == Workbench::Antenna && !slot.studio_open {
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
