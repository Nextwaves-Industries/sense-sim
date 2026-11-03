//! The app's workspaces, like the environments of a CAD package: each job has its
//! own ribbon tabs and project tree. (The code calls them workbenches; on screen they
//! are workspaces.)
//!
//! Which ones are on offer depends on the project. A scene project has Layout, Plan,
//! Simulation, Plant and Factory over its one scene, and opens in the one it is for
//! ([`settle_loaded_project`]). An antenna project needs no scene and has its own:
//! Antenna, whose ribbon tabs follow the design from model to results.
//!
//! A workspace remembers the card it had open, and a scene project remembers its
//! workspace while another tab shows ([`ProjectView`]). Anything may still open a card
//! or the Studio directly (the assistant, scripted runs, a button on a device); the
//! workspace that owns it then comes to the front, see [`follow`].

use bevy_egui::egui::Color32;

use super::LeftPanelSlot;
use super::topbar::Flags;
use crate::rtls::ui::{Page, RtlsUiState, Tab};
use crate::theme::{self, icon};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Workbench {
    /// Build the site: assets, walls, arranging.
    #[default]
    Layout,
    /// Put the positioning devices in it and plan coverage.
    Plan,
    /// Design an antenna, in Antenna Studio.
    Antenna,
    /// See how it performs: live tests, RF solvers, predictions.
    Simulation,
    /// Run connected equipment.
    Plant,
    /// See the real hardware found on the network.
    Factory,
}

/// The Antenna workspace's tab with the 3D modeler's tools.
pub const ANTENNA_MODEL_TAB: usize = 1;

/// What a project is: it decides the workspaces on offer and which one it opens in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjectKind {
    Scene,
    Antenna,
}

impl Workbench {
    pub const ALL: [Workbench; 6] = [Workbench::Layout, Workbench::Plan, Workbench::Antenna, Workbench::Simulation, Workbench::Plant, Workbench::Factory];
    /// A scene project's workspaces, in the order the work goes.
    pub const SCENE: [Workbench; 5] = [Workbench::Layout, Workbench::Plan, Workbench::Simulation, Workbench::Plant, Workbench::Factory];

    pub fn project(self) -> ProjectKind {
        match self {
            Workbench::Antenna => ProjectKind::Antenna,
            _ => ProjectKind::Scene,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Workbench::Layout => "Layout",
            Workbench::Plan => "Plan",
            Workbench::Antenna => "Antenna",
            Workbench::Simulation => "Simulation",
            Workbench::Plant => "Plant",
            Workbench::Factory => "Factory",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Workbench::Layout => icon::CUBE,
            Workbench::Plan => icon::BROADCAST,
            Workbench::Antenna => icon::TARGET,
            Workbench::Simulation => icon::WAVE_SINE,
            Workbench::Plant => icon::DROP,
            Workbench::Factory => icon::FACTORY,
        }
    }

    /// Marks the workspace in the switcher. Amber is left out: it means attention.
    pub fn color(self) -> Color32 {
        match self {
            Workbench::Layout => theme::ACCENT,
            Workbench::Plan => theme::PURPLE,
            Workbench::Antenna => Color32::from_rgb(194, 54, 110),
            Workbench::Simulation => Color32::from_rgb(0, 137, 123),
            Workbench::Plant => Color32::from_rgb(0, 131, 176),
            Workbench::Factory => Color32::from_rgb(84, 96, 112),
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Workbench::Layout => "Build the site: assets, walls and arranging",
            Workbench::Plan => "Place positioning devices and plan coverage",
            Workbench::Antenna => "Design one antenna on its own: model, solve, read the results, optimise",
            Workbench::Simulation => "Test the system live, run RF solvers and check predictions",
            Workbench::Plant => "Run connected equipment: ports, flow and alarms",
            Workbench::Factory => "See the real hardware on the network and how it is doing",
        }
    }

    /// The ribbon tabs of this workspace's own tools, between File and View.
    pub fn tabs(self) -> &'static [&'static str] {
        match self {
            Workbench::Layout => &["Build", "Edit"],
            Workbench::Plan => &["Place", "Coverage"],
            Workbench::Antenna => &["Design", "Model", "Solve", "Results", "Analysis", "Arrays"],
            Workbench::Simulation => &["Test", "Solvers", "Connect"],
            Workbench::Plant => &["Control"],
            Workbench::Factory => &["Monitor", "Connect"],
        }
    }

    /// This workspace's tree offers the asset library.
    pub fn has_library(self) -> bool {
        matches!(self, Workbench::Layout | Workbench::Plan | Workbench::Plant)
    }

    /// `--open bench-<name>`, for scripted runs.
    pub fn from_name(s: &str) -> Option<Workbench> {
        Some(match s {
            "layout" => Workbench::Layout,
            "plan" | "placement" => Workbench::Plan,
            "antenna" => Workbench::Antenna,
            "simulation" => Workbench::Simulation,
            "plant" => Workbench::Plant,
            "factory" => Workbench::Factory,
            _ => return None,
        })
    }

    /// The workspace a card belongs to.
    pub fn of_page(page: Page) -> Workbench {
        match page {
            Page::Deploy | Page::Tool(Tab::Devices | Tab::Inspector | Tab::Planning | Tab::Settings) => Workbench::Plan,
            Page::Test | Page::OpenEms | Page::Fdtd | Page::Outcomes | Page::Tool(Tab::Live | Tab::Rf | Tab::Outputs) => Workbench::Simulation,
        }
    }

    /// The card that opens the first time this workspace does.
    fn first_card(self) -> Option<Page> {
        match self {
            Workbench::Plan => Some(Page::Deploy),
            Workbench::Simulation => Some(Page::Test),
            _ => None,
        }
    }
}

/// The workspace a scene project opens in: what it holds says what it is for.
fn scene_default(has_plant: bool, devices: usize, zones: usize) -> Workbench {
    if has_plant {
        Workbench::Plant
    } else if devices + zones > 0 {
        Workbench::Plan
    } else {
        Workbench::Layout
    }
}

/// An antenna project, which opens in Antenna whichever workspace is showing.
pub fn is_antenna_file(path: &std::path::Path) -> bool {
    crate::project_file::holds_antenna(path)
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

type Cards = [Option<Page>; Workbench::ALL.len()];

/// The workspace in front and what the others had open.
pub struct Benches {
    pub current: Workbench,
    /// A workspace asked for this one, for `follow` to bring to the front.
    pub request: Option<Workbench>,
    /// Where leaving a full-canvas workspace goes back to.
    previous: Workbench,
    cards: Cards,
    /// The user asked for the project being opened, so it goes to the workspace it is
    /// for. Scripted runs and the assistant stay where they asked to be.
    pub opening: bool,
    /// This project was opened that way and hasn't finished loading.
    pub settle_on_load: bool,
}

impl Default for Benches {
    fn default() -> Self {
        Self { current: Workbench::default(), request: None, previous: Workbench::default(), cards: Workbench::ALL.map(Workbench::first_card), opening: false, settle_on_load: false }
    }
}

/// A scene project's place in the workspaces, kept with its tab while another shows.
pub struct ProjectView {
    workspace: Workbench,
    cards: Cards,
    settle_on_load: bool,
}

impl Default for ProjectView {
    fn default() -> Self {
        Self::fresh(false)
    }
}

impl ProjectView {
    /// As a project starts. `settle_on_load`: it picks its workspace once it has arrived.
    pub fn fresh(settle_on_load: bool) -> Self {
        Self { workspace: Workbench::default(), cards: Workbench::ALL.map(Workbench::first_card), settle_on_load }
    }
}

impl LeftPanelSlot {
    /// This workspace can be chosen in this build.
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

    /// What goes on in a workspace while it is in the background.
    pub fn activity(&self, bench: Workbench) -> Option<(String, Color32)> {
        match bench {
            Workbench::Simulation if self.rtls_alerts > 0 => Some((format!("{} alarms", self.rtls_alerts), theme::DANGER)),
            Workbench::Plant if self.plant == Some(true) => Some(("Running".to_string(), theme::SUCCESS)),
            Workbench::Factory if self.twin.down > 0 => Some((format!("{} down", self.twin.down), theme::DANGER)),
            _ => None,
        }
    }

    /// The scene project is leaving the front: where it stood.
    pub fn take_view(&mut self, rtls: &RtlsUiState) -> ProjectView {
        let bench = &mut self.bench;
        let workspace = if bench.current == Workbench::Antenna {
            bench.previous
        } else {
            let page = rtls.local.page();
            if Workbench::of_page(page) == bench.current {
                bench.cards[bench.current as usize] = self.rtls_open.then_some(page);
            }
            bench.current
        };
        ProjectView { workspace, cards: bench.cards, settle_on_load: std::mem::take(&mut bench.settle_on_load) }
    }

    /// A scene project comes to the front, as it stood. An antenna project in front
    /// stays there: the scene waits behind it.
    pub fn show_view(&mut self, rtls: &mut RtlsUiState, view: ProjectView) {
        let bench = &mut self.bench;
        bench.cards = view.cards;
        bench.settle_on_load = view.settle_on_load;
        if bench.current == Workbench::Antenna {
            bench.previous = view.workspace;
            return;
        }
        bench.current = view.workspace;
        let card = bench.cards[view.workspace as usize];
        if let Some(p) = card {
            rtls.local.set_page(p);
        }
        self.rtls_open = card.is_some();
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

/// Brings a workspace to the front, with the card it last had open.
pub fn switch(slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState, to: Workbench) {
    // Someone chose: a project still loading doesn't get to overrule them.
    slot.bench.settle_on_load = false;
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

/// A project the user opened has finished loading: it goes to the workspace it is for.
pub fn settle_loaded_project(slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    if !std::mem::take(&mut slot.bench.settle_on_load) {
        return;
    }
    let to = scene_default(slot.plant.is_some(), slot.rtls_status.devices, slot.rtls_status.zones);
    if slot.bench.current == Workbench::Antenna {
        slot.bench.previous = to;
    } else {
        switch(slot, rtls, to);
    }
}

/// Something outside the ribbon opened a card or a full-canvas workspace, or closed
/// one: the workspace that owns it comes to the front.
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
        switch(&mut slot, &mut rtls, Workbench::Plan);
        assert!(slot.rtls_open && rtls.local.page() == Page::Deploy);

        rtls.local.set_page(Page::Tool(Tab::Planning));
        switch(&mut slot, &mut rtls, Workbench::Layout);
        assert!(!slot.rtls_open);

        switch(&mut slot, &mut rtls, Workbench::Plan);
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
        switch(&mut slot, &mut rtls, Workbench::Plan);
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

    #[test]
    fn antenna_is_its_own_kind_of_project() {
        assert!(Workbench::SCENE.iter().all(|b| b.project() == ProjectKind::Scene));
        assert_eq!(Workbench::Antenna.project(), ProjectKind::Antenna);
        let every: Vec<Workbench> = Workbench::SCENE.into_iter().chain([Workbench::Antenna]).collect();
        assert!(Workbench::ALL.iter().all(|b| every.contains(b)), "every workspace is in one of the two lists");
        assert!(Workbench::ALL.iter().all(|b| !b.tabs().is_empty()), "each has tools to show");
    }

    #[test]
    fn a_loaded_project_opens_in_the_workspace_it_is_for() {
        let settled = |plant: Option<bool>, devices: usize, zones: usize| {
            let (mut slot, mut rtls) = ui();
            slot.bench.settle_on_load = true;
            slot.plant = plant;
            slot.rtls_status.devices = devices;
            slot.rtls_status.zones = zones;
            settle_loaded_project(&mut slot, &mut rtls);
            assert!(!slot.bench.settle_on_load);
            slot.bench.current
        };
        assert_eq!(settled(None, 0, 0), Workbench::Layout);
        assert_eq!(settled(None, 9, 0), Workbench::Plan);
        assert_eq!(settled(None, 0, 2), Workbench::Plan);
        assert_eq!(settled(Some(false), 0, 0), Workbench::Plant);
    }

    #[test]
    fn only_a_project_the_user_opened_picks_its_workspace() {
        let (mut slot, mut rtls) = ui();
        slot.rtls_status.devices = 9;
        settle_loaded_project(&mut slot, &mut rtls);
        assert_eq!(slot.bench.current, Workbench::Layout, "a scripted load stays where it is");
    }

    #[test]
    fn choosing_a_workspace_while_a_project_loads_is_not_overruled() {
        let (mut slot, mut rtls) = ui();
        slot.bench.settle_on_load = true;
        slot.rtls_status.devices = 9;
        switch(&mut slot, &mut rtls, Workbench::Factory);
        settle_loaded_project(&mut slot, &mut rtls);
        assert_eq!(slot.bench.current, Workbench::Factory);
    }

    #[test]
    fn a_scene_loading_behind_the_studio_stays_behind_it() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Antenna);
        slot.bench.settle_on_load = true;
        slot.rtls_status.devices = 9;
        settle_loaded_project(&mut slot, &mut rtls);
        assert!(slot.studio_open && slot.bench.current == Workbench::Antenna);
        slot.studio_open = false;
        follow(&mut slot, &mut rtls);
        assert_eq!(slot.bench.current, Workbench::Plan, "and it is waiting there when the Studio closes");
    }

    #[test]
    fn each_scene_project_keeps_its_own_workspace_and_card() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Plan);
        rtls.local.set_page(Page::Tool(Tab::Planning));

        let first = slot.take_view(&rtls);
        // The second project has its own card state, as tabs swap them.
        let mut other = RtlsUiState::default();
        slot.show_view(&mut other, ProjectView::fresh(false));
        assert_eq!(slot.bench.current, Workbench::Layout);
        assert!(!slot.rtls_open);

        switch(&mut slot, &mut other, Workbench::Plant);
        let second = slot.take_view(&other);

        slot.show_view(&mut rtls, first);
        assert_eq!(slot.bench.current, Workbench::Plan);
        assert!(slot.rtls_open && rtls.local.page() == Page::Tool(Tab::Planning));

        slot.show_view(&mut other, second);
        assert_eq!(slot.bench.current, Workbench::Plant);
        assert!(!slot.rtls_open);
    }

    #[test]
    fn changing_scene_tabs_leaves_the_studio_in_front() {
        let (mut slot, mut rtls) = ui();
        switch(&mut slot, &mut rtls, Workbench::Plan);
        let plan = slot.take_view(&rtls);
        slot.show_view(&mut rtls, ProjectView::fresh(false));
        switch(&mut slot, &mut rtls, Workbench::Antenna);

        slot.show_view(&mut rtls, plan);
        assert!(slot.studio_open && slot.bench.current == Workbench::Antenna && !slot.rtls_open);

        slot.studio_open = false;
        follow(&mut slot, &mut rtls);
        assert_eq!(slot.bench.current, Workbench::Plan, "closing it shows the scene that came forward");
    }

    #[test]
    fn antenna_files_are_told_from_layouts_by_name() {
        assert!(is_antenna_file(std::path::Path::new(r"C:\work\Dipole.Antenna.JSON")));
        assert!(!is_antenna_file(std::path::Path::new(r"C:\work\antenna.json")));
        assert!(!is_antenna_file(std::path::Path::new(r"C:\work\warehouse.sesim")));
    }
}
